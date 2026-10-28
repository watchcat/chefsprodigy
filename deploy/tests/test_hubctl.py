"""Tests for deploy/hubctl (k3s edition) against a fake kubectl with JSON state.

Run: python3 -I -m unittest discover -s deploy/tests
"""

import contextlib
import hashlib
import hmac
import importlib.machinery
import importlib.util
import io
import json
import os
import tempfile
import time
import unittest
import zipfile
from pathlib import Path
from urllib.parse import parse_qs, urlparse

HERE = Path(__file__).resolve().parent
HUBCTL_PATH = HERE.parent / "hubctl"
FAKE_KUBECTL = HERE / "fake_kubectl.py"


def load_hubctl():
    loader = importlib.machinery.SourceFileLoader("hubctl", str(HUBCTL_PATH))
    spec = importlib.util.spec_from_loader("hubctl", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


hubctl = load_hubctl()


class HubctlCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.root = base / "root"
        self.root.mkdir()
        self.state = base / "kube-state.json"
        self.log = base / "calls.log"
        self.log.write_text("")
        self.env_patch = {
            "HUBCTL_KUBECTL": str(FAKE_KUBECTL),
            "FAKE_KUBE_STATE": str(self.state),
            "FAKE_LOG": str(self.log),
        }
        self.saved = {k: os.environ.get(k) for k in self.env_patch}
        os.environ.update(self.env_patch)
        self.write_env()

    def tearDown(self):
        for k, v in self.saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self.tmp.cleanup()

    def write_env(self, **overrides):
        values = {
            "DOMAIN": "example.test",
            "HUB_IMAGE": "ghcr.io/example/hub:1.2.3",
            "TLS_MODE": "off",
            "LLM_MODEL": "minimax/MiniMax-M3",
            "MINIMAX_API_KEY": "vendor-key-for-tests",
        }
        values.update(overrides)
        (self.root / ".env").write_text(
            "".join(f"{k}={v}\n" for k, v in values.items() if v is not None)
        )

    def run_hubctl(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = hubctl.main(["--root", str(self.root), *args])
        return code, out.getvalue()

    def calls(self):
        return [l for l in self.log.read_text().splitlines() if l]

    def raw_state(self):
        return json.loads(self.state.read_text()) if self.state.exists() else {}

    def objects(self):
        """Cluster objects only; the fake gateway's bookkeeping lives under "_gateway"."""
        return {k: v for k, v in self.raw_state().items() if not k.startswith("_")}

    def obj(self, kind, ns, name):
        return self.objects()[f"{kind.lower()}/{ns}/{name}"]

    def seed_origin_secret(self):
        self.state.write_text(json.dumps({
            "secret/kube-system/chefs-origin-tls": {"kind": "Secret", "metadata": {"name": "chefs-origin-tls", "namespace": "kube-system"}}
        }))


class NameValidation(unittest.TestCase):
    def test_accepts_valid_subdomains_and_rejects_the_rest(self):
        for ok in ["jacob", "maria-2", "a", "x" * 63]:
            hubctl.validate_name(ok)
        for bad in ["", "WWW", "-bad", "bad-", "a.b", "under_score", "x" * 64, "hub name", "www", "api", "gateway", "platform"]:
            with self.assertRaises(hubctl.HubctlError, msg=bad):
                hubctl.validate_name(bad)


class Init(HubctlCase):
    def test_init_applies_namespaces_policies_traefik_config_and_the_backup_cronjob(self):
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        objs = self.objects()
        self.assertIn("namespace//chefs-system", objs)
        self.assertIn("namespace//chefs-hubs", objs)
        deny = self.obj("NetworkPolicy", "chefs-hubs", "default-deny")
        self.assertEqual(deny["spec"]["podSelector"], {})
        self.assertEqual(sorted(deny["spec"]["policyTypes"]), ["Egress", "Ingress"])
        egress = self.obj("NetworkPolicy", "chefs-hubs", "hub-egress")
        blocks = [r["ipBlock"] for rule in egress["spec"]["egress"] for r in rule.get("to", []) if "ipBlock" in r]
        self.assertTrue(any(b["cidr"] == "0.0.0.0/0" and "10.0.0.0/8" in b["except"] and "169.254.0.0/16" in b["except"] for b in blocks), blocks)
        ingress = self.obj("NetworkPolicy", "chefs-hubs", "hub-ingress")
        froms = json.dumps(ingress["spec"]["ingress"])
        self.assertIn("kube-system", froms, "Traefik may reach hubs")
        self.assertIn("chefs-system", froms, "the platform poller and the backup job may reach hubs")
        traefik = self.obj("HelmChartConfig", "kube-system", "traefik")
        values = traefik["spec"]["valuesContent"]
        self.assertIn("173.245.48.0/20", values)
        self.assertIn("trustedIPs", values)
        self.assertRegex(values, r"access:\s*\n\s*enabled: false")
        self.assertNotIn("middleware/chefs-system/buffering", objs,
                         "Traefik's buffering middleware captures whole responses and stalls /mcp event streams")
        self.assertIn("middleware/chefs-system/headers", objs)
        cron = self.obj("CronJob", "chefs-system", "hub-backup")
        self.assertEqual(cron["spec"]["schedule"], "17 3 * * *")
        self.assertEqual(cron["spec"]["concurrencyPolicy"], "Forbid")
        pod = cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]
        self.assertEqual(pod["serviceAccountName"], "hub-backup")
        images = [c["image"] for c in pod.get("initContainers", []) + pod["containers"]]
        self.assertTrue(any("rclone" in i for i in images), images)
        self.assertIn("rolebinding/chefs-hubs/hub-backup-reads-secrets", objs)
        self.assertFalse(any(k.startswith("tlsstore/") for k in objs), "no TLS store in TLS_MODE=off")

    def test_init_waits_for_traefiks_crds_before_applying_middlewares(self):
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        calls = self.calls()
        wait = next(i for i, c in enumerate(calls) if "wait --for=condition=established crd/middlewares.traefik.io" in c)
        applies = [i for i, c in enumerate(calls) if c.startswith("apply")]
        self.assertLess(applies[0], wait, "namespaces first")
        self.assertLess(wait, applies[1], "the Middleware is applied after the CRD exists")

    def test_init_explains_a_traefik_that_is_still_installing(self):
        state = self.raw_state()
        state["_fail_wait"] = True
        self.state.write_text(json.dumps(state))
        saved, hubctl.CRD_WAIT_SECONDS = hubctl.CRD_WAIT_SECONDS, 0
        try:
            code, out = self.run_hubctl("init")
        finally:
            hubctl.CRD_WAIT_SECONDS = saved
        self.assertNotEqual(code, 0)
        self.assertIn("Traefik", out)
        self.assertIn("helm-install-traefik", out)

    def test_origin_mode_needs_the_origin_ca_secret_and_installs_the_default_tls_store(self):
        self.write_env(TLS_MODE="origin")
        code, out = self.run_hubctl("init")
        self.assertNotEqual(code, 0)
        self.assertIn("chefs-origin-tls", out)
        self.seed_origin_secret()
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        store = self.obj("TLSStore", "kube-system", "default")
        self.assertEqual(store["spec"]["defaultCertificate"]["secretName"], "chefs-origin-tls")
        values = self.obj("HelmChartConfig", "kube-system", "traefik")["spec"]["valuesContent"]
        self.assertIn("redirectTo", values.replace("redirections", "redirectTo"), "web redirects to websecure")

    def test_r2_settings_become_the_backup_secret(self):
        self.write_env(R2_BUCKET="chefs-backups", R2_ACCOUNT_ID="acc123", R2_ACCESS_KEY_ID="id", R2_SECRET_ACCESS_KEY="sec")
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        secret = self.obj("Secret", "chefs-system", "backup-r2")
        data = secret["stringData"]
        self.assertEqual(data["RCLONE_CONFIG_R2_TYPE"], "s3")
        self.assertEqual(data["RCLONE_CONFIG_R2_PROVIDER"], "Cloudflare")
        self.assertEqual(data["RCLONE_CONFIG_R2_ENDPOINT"], "https://acc123.r2.cloudflarestorage.com")
        self.assertEqual(data["R2_BUCKET"], "chefs-backups")
        self.assertNotIn("sec", out, "secrets are never printed")


class Create(HubctlCase):
    def test_create_renders_secret_statefulset_service_and_ingress(self):
        code, out = self.run_hubctl("create", "demo", "--chef", "Demo Chef")
        self.assertEqual(code, 0, out)
        secret = self.obj("Secret", "chefs-hubs", "hub-demo-env")
        self.assertEqual(secret["metadata"]["labels"], {"app": "hub", "hub": "demo"})
        data = secret["stringData"]
        self.assertRegex(data["NODE_API_KEY"], r"^[0-9a-f]{64}$")
        self.assertEqual(data["NODE_CHEF_NAME"], "Demo Chef")
        self.assertEqual(data["NODE_PUBLIC_URL"], "http://demo.example.test")
        self.assertEqual(data["LLM_MODEL"], "minimax/MiniMax-M3", "the model setting is copied")
        self.assertNotIn("vendor-key-for-tests", json.dumps(data), "the vendor key stays in the gateway")
        self.assertIn(data["NODE_API_KEY"], out, "the owner key is printed once")

        sts = self.obj("StatefulSet", "chefs-hubs", "hub-demo")
        pod = sts["spec"]["template"]["spec"]
        node = pod["containers"][0]
        self.assertEqual(node["image"], "ghcr.io/example/hub:1.2.3")
        self.assertEqual(node["args"], ["serve"])
        self.assertIn({"name": "TOKIO_WORKER_THREADS", "value": "2"}, node["env"])
        self.assertEqual(node["envFrom"], [{"secretRef": {"name": "hub-demo-env"}}])
        self.assertEqual(node["readinessProbe"]["httpGet"]["path"], "/api/v1/health")
        self.assertEqual(node["livenessProbe"]["httpGet"]["path"], "/api/v1/health")
        self.assertEqual(node["resources"]["limits"], {"cpu": "500m", "memory": "128Mi"})
        self.assertTrue(node["securityContext"]["readOnlyRootFilesystem"])
        self.assertEqual(node["securityContext"]["capabilities"], {"drop": ["ALL"]})
        self.assertEqual(pod["securityContext"]["runAsUser"], 65532)
        self.assertFalse(pod["automountServiceAccountToken"])
        claim = sts["spec"]["volumeClaimTemplates"][0]
        self.assertEqual(claim["metadata"]["name"], "data")
        self.assertEqual(claim["spec"]["storageClassName"], "local-path")
        self.assertEqual(claim["spec"]["resources"]["requests"]["storage"], "2Gi")

        svc = self.obj("Service", "chefs-hubs", "hub-demo")
        self.assertEqual(svc["spec"]["ports"][0]["port"], 8080)
        ing = self.obj("Ingress", "chefs-hubs", "hub-demo")
        self.assertEqual(ing["spec"]["rules"][0]["host"], "demo.example.test")
        self.assertEqual(ing["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.entrypoints"], "web")
        self.assertEqual(ing["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.middlewares"],
                         "chefs-system-headers@kubernetescrd", "headers only: buffering would break MCP streaming")
        self.assertNotIn("tls", ing["spec"], "TLS_MODE=off: plain http")
        self.assertTrue(any("rollout status statefulset/hub-demo" in c for c in self.calls()), self.calls())

    def test_origin_mode_ingress_uses_tls_and_websecure(self):
        self.write_env(TLS_MODE="origin")
        self.seed_origin_secret()
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        ing = self.obj("Ingress", "chefs-hubs", "hub-demo")
        self.assertEqual(ing["spec"]["tls"], [{"hosts": ["demo.example.test"]}])
        self.assertEqual(ing["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.entrypoints"], "websecure")
        self.assertEqual(self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_PUBLIC_URL"], "https://demo.example.test")

    def test_create_is_idempotent_and_keeps_the_key(self):
        self.run_hubctl("create", "demo")
        key = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_API_KEY"]
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_API_KEY"], key)
        self.assertNotIn(key, out, "an existing key is never printed again")
        code, out = self.run_hubctl("list")
        self.assertEqual(code, 0, out)
        self.assertIn("http://demo.example.test", out)

    def test_missing_domain_is_a_clear_error(self):
        self.write_env(DOMAIN=None)
        code, out = self.run_hubctl("create", "demo")
        self.assertNotEqual(code, 0)
        self.assertIn("DOMAIN", out)


class Keys(HubctlCase):
    def test_login_link_token_verifies_with_the_hub_key(self):
        self.run_hubctl("create", "demo")
        key = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_API_KEY"]
        code, out = self.run_hubctl("login-link", "demo")
        self.assertEqual(code, 0, out)
        url = next(w for w in out.split() if w.startswith("http://demo"))
        parsed = urlparse(url)
        self.assertEqual(parsed.path, "/api/v1/auth/magic")
        token = parse_qs(parsed.query)["token"][0]
        expires, nonce, mac = token.split(".")
        self.assertRegex(nonce, r"^[0-9a-f]{16}$")
        self.assertTrue(0 < int(expires) - time.time() <= 600)
        expected = hmac.new(key.encode(), f"chefsprodigy-magic:{expires}:{nonce}".encode(), hashlib.sha256).hexdigest()
        self.assertEqual(mac, expected)

    def test_rotate_key_writes_a_new_key_and_restarts_the_hub(self):
        self.run_hubctl("create", "demo")
        old = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_API_KEY"]
        self.log.write_text("")
        code, out = self.run_hubctl("rotate-key", "demo")
        self.assertEqual(code, 0, out)
        new = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_API_KEY"]
        self.assertNotEqual(new, old)
        self.assertIn(new, out)
        self.assertEqual(self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["NODE_CHEF_NAME"], "Demo")
        self.assertTrue(any("rollout restart statefulset/hub-demo" in c for c in self.calls()), self.calls())


class BackupRestore(HubctlCase):
    def test_backup_runs_a_job_from_the_cronjob_for_one_hub_or_all(self):
        self.run_hubctl("init")
        self.run_hubctl("create", "demo")
        self.log.write_text("")
        code, out = self.run_hubctl("backup", "demo")
        self.assertEqual(code, 0, out)
        jobs = [o for k, o in self.objects().items() if o["kind"] == "Job"]
        self.assertEqual(len(jobs), 1, jobs)
        job = jobs[0]
        self.assertEqual(job["metadata"]["namespace"], "chefs-system")
        self.assertEqual(job["metadata"]["annotations"]["cronjob.kubernetes.io/instantiate"], "manual")
        containers = job["spec"]["template"]["spec"].get("initContainers", []) + job["spec"]["template"]["spec"]["containers"]
        hubs_env = [e for c in containers for e in c.get("env", []) if e["name"] == "HUBS"]
        self.assertTrue(hubs_env and all(e["value"] == "demo" for e in hubs_env), containers)
        self.assertTrue(any(c.startswith("wait --for=condition=complete") for c in self.calls()), self.calls())
        self.assertIn("fake backup log", out)

        code, out = self.run_hubctl("backup", "all")
        self.assertEqual(code, 0, out)
        jobs = [o for k, o in self.objects().items() if o["kind"] == "Job"]
        self.assertEqual(len(jobs), 2)

    def test_restore_unpacks_an_archive_into_the_hub_volume(self):
        self.run_hubctl("create", "demo")
        archive = self.root / "demo.zip"
        with zipfile.ZipFile(archive, "w") as zf:
            zf.writestr("recipes.db", "db")
            zf.writestr("images/01A.webp", "img")
        self.log.write_text("")
        code, out = self.run_hubctl("restore", "demo", str(archive))
        self.assertEqual(code, 0, out)
        calls = self.calls()
        order = [next((i for i, c in enumerate(calls) if needle in c), -1) for needle in (
            "scale statefulset/hub-demo", "apply -f -", "cp ", "exec ", "delete pod hub-demo-restore", "scale statefulset/hub-demo")]
        self.assertTrue(all(i >= 0 for i in order), calls)
        first_scale = calls[order[0]]
        self.assertIn("--replicas=0", first_scale)
        last_scale = [c for c in calls if "scale statefulset/hub-demo" in c][-1]
        self.assertIn("--replicas=1", last_scale)
        exec_call = calls[order[3]]
        self.assertIn("unzip", exec_call)
        self.assertIn("65532", exec_call)

    def test_restore_rejects_archives_without_a_database(self):
        self.run_hubctl("create", "demo")
        bad = self.root / "bad.zip"
        with zipfile.ZipFile(bad, "w") as zf:
            zf.writestr("notes.txt", "x")
        code, out = self.run_hubctl("restore", "demo", str(bad))
        self.assertNotEqual(code, 0)
        self.assertIn("recipes.db", out)


class RemoveUpgradeStatus(HubctlCase):
    def test_remove_keeps_the_volume_until_purge(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("remove", "demo")
        self.assertEqual(code, 0, out)
        objs = self.objects()
        self.assertNotIn("statefulset/chefs-hubs/hub-demo", objs)
        self.assertNotIn("secret/chefs-hubs/hub-demo-env", objs)
        self.assertNotIn("ingress/chefs-hubs/hub-demo", objs)
        self.assertFalse(any("delete pvc data-hub-demo-0" in c for c in self.calls()), "data kept")
        code, out = self.run_hubctl("remove", "demo", "--purge")
        self.assertEqual(code, 0, out)
        self.assertTrue(any("delete pvc data-hub-demo-0" in c for c in self.calls()), self.calls())

    def test_upgrade_sets_the_image_on_each_hub_and_waits(self):
        self.run_hubctl("create", "jacob")
        self.run_hubctl("create", "maria")
        self.write_env(HUB_IMAGE="ghcr.io/example/hub:2.0.0")
        self.log.write_text("")
        code, out = self.run_hubctl("upgrade", "all")
        self.assertEqual(code, 0, out)
        for name in ("jacob", "maria"):
            sts = self.obj("StatefulSet", "chefs-hubs", f"hub-{name}")
            self.assertEqual(sts["spec"]["template"]["spec"]["containers"][0]["image"], "ghcr.io/example/hub:2.0.0")
        self.assertEqual(len([c for c in self.calls() if "rollout status statefulset" in c]), 2)

    def test_status_reports_pod_readiness(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("status")
        self.assertEqual(code, 0, out)
        self.assertIn("demo", out)
        self.assertIn("Running", out)



GATEWAY_BASE = "http://llm-gateway.chefs-system.svc:8080/anthropic"


class Gateway(HubctlCase):
    def gateway_state(self):
        return self.raw_state().get("_gateway", {"hubs": {}, "usage": []})

    def test_init_deploys_the_gateway_with_the_vendor_keys(self):
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        self.assertNotIn("vendor-key-for-tests", out, "vendor keys are never printed")
        secret = self.obj("Secret", "chefs-system", "llm-gateway-env")
        self.assertEqual(secret["stringData"]["MINIMAX_API_KEY"], "vendor-key-for-tests")
        self.assertNotIn("LLM_MODEL", secret["stringData"], "only gateway settings go in the gateway Secret")
        pvc = self.obj("PersistentVolumeClaim", "chefs-system", "llm-gateway-data")
        self.assertEqual(pvc["spec"]["storageClassName"], "local-path")
        dep = self.obj("Deployment", "chefs-system", "llm-gateway")
        self.assertEqual(dep["spec"]["strategy"], {"type": "Recreate"}, "one SQLite writer at a time")
        pod = dep["spec"]["template"]["spec"]
        self.assertEqual(dep["spec"]["template"]["metadata"]["labels"]["app"], "llm-gateway",
                         "the hub egress policy selects this label")
        c = pod["containers"][0]
        self.assertEqual(c["image"], "ghcr.io/example/hub:1.2.3", "same image as the hubs")
        self.assertEqual(c["command"], ["/usr/local/bin/llm-gateway"])
        self.assertEqual(c["args"], ["serve"])
        self.assertEqual(c["envFrom"], [{"secretRef": {"name": "llm-gateway-env"}}])
        self.assertEqual(c["readinessProbe"]["httpGet"]["path"], "/health")
        self.assertEqual(c["resources"]["limits"]["memory"], "64Mi")
        self.assertTrue(c["securityContext"]["readOnlyRootFilesystem"])
        self.assertFalse(pod["automountServiceAccountToken"])
        svc = self.obj("Service", "chefs-system", "llm-gateway")
        self.assertEqual(svc["spec"]["ports"][0]["port"], 8080)
        policy = self.obj("NetworkPolicy", "chefs-system", "llm-gateway-ingress")
        self.assertEqual(policy["spec"]["podSelector"], {"matchLabels": {"app": "llm-gateway"}})
        froms = json.dumps(policy["spec"]["ingress"])
        self.assertIn("chefs-hubs", froms)
        self.assertIn('"app": "hub"', froms, "only hub pods may call the gateway")

    def test_admin_token_and_upstream_overrides_reach_the_gateway(self):
        self.write_env(GATEWAY_ADMIN_TOKEN="admin-secret", MINIMAX_ANTHROPIC_API_BASE="https://api.minimaxi.com/anthropic",
                       ANTHROPIC_API_KEY="anthropic-vendor-key")
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        data = self.obj("Secret", "chefs-system", "llm-gateway-env")["stringData"]
        self.assertEqual(data["GATEWAY_ADMIN_TOKEN"], "admin-secret")
        self.assertEqual(data["MINIMAX_ANTHROPIC_API_BASE"], "https://api.minimaxi.com/anthropic")
        self.assertEqual(data["ANTHROPIC_API_KEY"], "anthropic-vendor-key")

    def test_master_key_and_cache_settings_reach_the_gateway_and_render_masks_the_master_key(self):
        self.write_env(GATEWAY_MASTER_KEY="master-key-for-tests", GATEWAY_CACHE_TTL_DAYS="7",
                       GATEWAY_CACHE_MAX_ENTRIES="500")
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        self.assertNotIn("master-key-for-tests", out, "the master key is never printed")
        data = self.obj("Secret", "chefs-system", "llm-gateway-env")["stringData"]
        self.assertEqual(data["GATEWAY_MASTER_KEY"], "master-key-for-tests")
        self.assertEqual(data["GATEWAY_CACHE_TTL_DAYS"], "7")
        self.assertEqual(data["GATEWAY_CACHE_MAX_ENTRIES"], "500")

        code, out = self.run_hubctl("render", "--json", "all")
        self.assertEqual(code, 0, out)
        self.assertNotIn("master-key-for-tests", out, "render masks the master key like the vendor keys")
        secret = next(i for i in json.loads(out)["items"]
                      if i["kind"] == "Secret" and i["metadata"]["name"] == "llm-gateway-env")
        self.assertEqual(secret["stringData"]["GATEWAY_MASTER_KEY"], hubctl.GATEWAY_PLACEHOLDER)
        self.assertEqual(secret["stringData"]["GATEWAY_CACHE_TTL_DAYS"], "7", "plain settings stay readable")

    def test_create_registers_the_hub_and_gives_it_a_gateway_token(self):
        self.write_env(LLM_API_KEY="generic-vendor-key")
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        data = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]
        token = self.gateway_state()["hubs"]["demo"]["token"]
        self.assertEqual(data["MINIMAX_API_KEY"], token)
        self.assertEqual(data["ANTHROPIC_API_KEY"], token, "Anthropic models go through the gateway too")
        self.assertEqual(data["MINIMAX_ANTHROPIC_API_BASE"], GATEWAY_BASE)
        self.assertEqual(data["ANTHROPIC_BASE_URL"], GATEWAY_BASE)
        self.assertNotIn("LLM_API_KEY", data, "a generic key would override the gateway token")
        self.assertNotIn("vendor-key", json.dumps(data))
        self.assertNotIn(token, out, "the hub token is not printed")
        calls = self.calls()
        rollout = next(i for i, c in enumerate(calls) if "rollout status deployment/llm-gateway" in c)
        register = next(i for i, c in enumerate(calls) if "add-hub demo --rotate --json" in c)
        self.assertLess(rollout, register, "the gateway is ready before hubctl registers the hub")

    def test_create_again_keeps_the_gateway_token(self):
        self.run_hubctl("create", "demo")
        token = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["MINIMAX_API_KEY"]
        self.log.write_text("")
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["MINIMAX_API_KEY"], token)
        self.assertFalse(any("llm-gateway add-hub" in c for c in self.calls()), self.calls())

    def test_create_moves_a_direct_hub_onto_the_gateway(self):
        self.write_env(LLM_GATEWAY="off", LLM_API_KEY="generic-vendor-key")
        self.run_hubctl("create", "demo")
        data = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]
        self.assertEqual(data["MINIMAX_API_KEY"], "vendor-key-for-tests", "LLM_GATEWAY=off: direct vendor access")
        self.write_env()
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        data = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]
        self.assertEqual(data["MINIMAX_API_KEY"], self.gateway_state()["hubs"]["demo"]["token"])
        self.assertEqual(data["MINIMAX_ANTHROPIC_API_BASE"], GATEWAY_BASE)
        self.assertNotIn("LLM_API_KEY", data)
        self.assertNotIn("vendor-key", json.dumps(data))

    def test_gateway_off_keeps_direct_vendor_access_and_deploys_no_gateway(self):
        self.write_env(LLM_GATEWAY="off")
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertNotIn("deployment/chefs-system/llm-gateway", self.objects())
        data = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]
        self.assertEqual(data["MINIMAX_API_KEY"], "vendor-key-for-tests")
        self.assertNotIn("MINIMAX_ANTHROPIC_API_BASE", data)

    def test_without_a_vendor_key_there_is_no_gateway(self):
        self.write_env(MINIMAX_API_KEY=None)
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertNotIn("deployment/chefs-system/llm-gateway", self.objects())
        self.assertNotIn("MINIMAX_API_KEY", self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"])

    def test_remove_disables_the_hub_at_the_gateway(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("remove", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.gateway_state()["hubs"]["demo"]["status"], "disabled")
        self.assertIn("gateway token revoked", out)

    def test_gateway_token_issues_a_new_token_and_restarts_the_hub(self):
        self.run_hubctl("create", "demo")
        old = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]["MINIMAX_API_KEY"]
        self.log.write_text("")
        code, out = self.run_hubctl("gateway-token", "demo")
        self.assertEqual(code, 0, out)
        data = self.obj("Secret", "chefs-hubs", "hub-demo-env")["stringData"]
        self.assertNotEqual(data["MINIMAX_API_KEY"], old)
        self.assertEqual(data["MINIMAX_API_KEY"], self.gateway_state()["hubs"]["demo"]["token"])
        self.assertEqual(data["ANTHROPIC_API_KEY"], data["MINIMAX_API_KEY"])
        self.assertTrue(any("rollout restart statefulset/hub-demo" in c for c in self.calls()), self.calls())
        code, out = self.run_hubctl("gateway-token", "ghost")
        self.assertNotEqual(code, 0)
        self.assertIn("does not exist", out)

    def test_usage_prints_the_gateway_rows(self):
        self.run_hubctl("create", "jacob")
        state = self.raw_state()
        state["_gateway"]["usage"] = [
            {"hub": "jacob", "day": "2026-10-27", "requests": 3, "input_tokens": 690, "output_tokens": 879, "cost_cents": 0.0},
            {"hub": "maria", "day": "2026-10-27", "requests": 1, "input_tokens": 10, "output_tokens": 20, "cost_cents": 0.0},
            {"hub": "jacob", "day": "2026-10-20", "requests": 9, "input_tokens": 1, "output_tokens": 1, "cost_cents": 0.0},
        ]
        self.state.write_text(json.dumps(state))
        code, out = self.run_hubctl("usage", "jacob", "--from", "2026-10-21")
        self.assertEqual(code, 0, out)
        self.assertIn("2026-10-27", out)
        self.assertIn("690", out)
        self.assertNotIn("maria", out)
        self.assertNotIn("2026-10-20", out)
        self.assertTrue(any("usage --hub jacob --from 2026-10-21" in c for c in self.calls()), self.calls())
        code, out = self.run_hubctl("usage")
        self.assertEqual(code, 0, out)
        self.assertIn("maria", out)
        self.assertRegex(out, r"total\s+13\s", "a totals line sums the requests")

    def test_usage_needs_the_gateway(self):
        self.write_env(LLM_GATEWAY="off")
        code, out = self.run_hubctl("usage")
        self.assertNotEqual(code, 0)
        self.assertIn("gateway", out)

    def test_upgrade_moves_the_gateway_to_the_new_image(self):
        self.run_hubctl("create", "demo")
        self.write_env(HUB_IMAGE="ghcr.io/example/hub:2.0.0")
        code, out = self.run_hubctl("upgrade")
        self.assertEqual(code, 0, out)
        dep = self.obj("Deployment", "chefs-system", "llm-gateway")
        self.assertEqual(dep["spec"]["template"]["spec"]["containers"][0]["image"], "ghcr.io/example/hub:2.0.0")
        self.assertIn("llm-gateway", out)

    def test_status_shows_the_gateway(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("status")
        self.assertEqual(code, 0, out)
        self.assertRegex(out, r"llm-gateway\s+.*Running ready")

    def test_render_all_shows_the_gateway_without_vendor_keys(self):
        code, out = self.run_hubctl("render", "--json", "all")
        self.assertEqual(code, 0, out)
        names = {(i["kind"], i["metadata"]["name"]) for i in json.loads(out)["items"]}
        self.assertIn(("Deployment", "llm-gateway"), names)
        self.assertNotIn("vendor-key-for-tests", out, "render masks the gateway's vendor keys")


PLATFORM_EXEC = "exec deploy/platform -n chefs-system -- /usr/local/bin/platform"


class Platform(HubctlCase):
    def platform_state(self):
        return self.raw_state().get("_platform", {"hubs": {}})

    def test_init_deploys_the_platform_with_an_apex_ingress_for_the_search_page_and_public_api(self):
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        self.assertIn("platform", out)
        secret = self.obj("Secret", "chefs-system", "platform-env")
        self.assertEqual(secret["stringData"], {}, "no admin token in .env: admin API off")
        pvc = self.obj("PersistentVolumeClaim", "chefs-system", "platform-data")
        self.assertEqual(pvc["spec"]["storageClassName"], "local-path")
        dep = self.obj("Deployment", "chefs-system", "platform")
        self.assertEqual(dep["spec"]["replicas"], 1)
        self.assertEqual(dep["spec"]["strategy"], {"type": "Recreate"}, "one SQLite writer at a time")
        labels = dep["spec"]["template"]["metadata"]["labels"]
        self.assertEqual(labels, {"app": "platform", "role": "hub-client"})
        pod = dep["spec"]["template"]["spec"]
        c = pod["containers"][0]
        self.assertEqual(c["image"], "ghcr.io/example/hub:1.2.3", "same image as the hubs")
        self.assertEqual(c["command"], ["/usr/local/bin/platform"])
        self.assertEqual(c["args"], ["serve"])
        self.assertEqual(c["envFrom"], [{"secretRef": {"name": "platform-env"}}])
        env = {e["name"]: e["value"] for e in c["env"]}
        self.assertEqual(env, {"PLATFORM_PORT": "8080", "PLATFORM_DATA_DIR": "/data", "TOKIO_WORKER_THREADS": "2"})
        self.assertEqual(c["readinessProbe"]["httpGet"]["path"], "/health")
        self.assertEqual(c["livenessProbe"]["httpGet"]["path"], "/health")
        self.assertEqual(c["resources"]["limits"]["memory"], "128Mi")
        self.assertTrue(c["securityContext"]["readOnlyRootFilesystem"])
        self.assertEqual(c["securityContext"]["capabilities"], {"drop": ["ALL"]})
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertEqual(pod["securityContext"]["runAsUser"], 65532)
        svc = self.obj("Service", "chefs-system", "platform")
        self.assertEqual(svc["spec"]["selector"], {"app": "platform"})
        self.assertEqual(svc["spec"]["ports"][0]["port"], 8080)

        ingress = self.obj("Ingress", "chefs-system", "platform")
        rule = ingress["spec"]["rules"][0]
        self.assertEqual(rule["host"], "example.test", "the apex")
        paths = {p["path"]: p["pathType"] for p in rule["http"]["paths"]}
        self.assertEqual(paths, {
            "/": "Exact",
            "/assets": "Prefix",
            "/api/v1/search": "Prefix",
            "/api/v1/hubs": "Prefix",
            "/health": "Exact",
        })
        self.assertEqual(paths["/"], "Exact", "the search page must not make every platform path public")
        self.assertFalse(any(p.startswith("/admin") for p in paths), "/admin never reaches the internet")
        annotations = ingress["metadata"]["annotations"]
        self.assertEqual(annotations["traefik.ingress.kubernetes.io/router.entrypoints"], "web", "TLS_MODE=off")
        self.assertEqual(annotations["traefik.ingress.kubernetes.io/router.middlewares"], "chefs-system-headers@kubernetescrd")
        self.assertNotIn("tls", ingress["spec"])
        policy = self.obj("NetworkPolicy", "chefs-system", "platform-ingress")
        self.assertEqual(policy["spec"]["podSelector"], {"matchLabels": {"app": "platform"}})
        self.assertIn("kube-system", json.dumps(policy["spec"]["ingress"]), "Traefik may reach the platform")

    def test_the_hub_ingress_policy_admits_the_platform_poller(self):
        self.run_hubctl("init")
        labels = self.obj("Deployment", "chefs-system", "platform")["spec"]["template"]["metadata"]["labels"]
        rule = self.obj("NetworkPolicy", "chefs-hubs", "hub-ingress")["spec"]["ingress"][0]
        system = [f for f in rule["from"] if f.get("namespaceSelector", {}).get("matchLabels", {})
                  .get("kubernetes.io/metadata.name") == "chefs-system"]
        self.assertTrue(system, rule)
        for key, value in system[0]["podSelector"]["matchLabels"].items():
            self.assertEqual(labels.get(key), value, "platform pods match the hub-ingress podSelector")

    def test_origin_mode_serves_the_apex_over_websecure_with_tls(self):
        self.write_env(TLS_MODE="origin")
        self.seed_origin_secret()
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        ingress = self.obj("Ingress", "chefs-system", "platform")
        self.assertEqual(ingress["spec"]["tls"], [{"hosts": ["example.test"]}])
        self.assertEqual(ingress["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.entrypoints"], "websecure")

    def test_admin_token_and_poll_settings_reach_the_platform_and_render_masks_the_token(self):
        self.write_env(PLATFORM_ADMIN_TOKEN="platform-admin-secret", PLATFORM_POLL_INTERVAL_SECS="300",
                       PLATFORM_MEM_LIMIT="96Mi", PLATFORM_STORAGE="2Gi")
        code, out = self.run_hubctl("init")
        self.assertEqual(code, 0, out)
        self.assertNotIn("platform-admin-secret", out)
        data = self.obj("Secret", "chefs-system", "platform-env")["stringData"]
        self.assertEqual(data, {"PLATFORM_ADMIN_TOKEN": "platform-admin-secret", "PLATFORM_POLL_INTERVAL_SECS": "300"})
        dep = self.obj("Deployment", "chefs-system", "platform")
        self.assertEqual(dep["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"], "96Mi")
        self.assertEqual(self.obj("PersistentVolumeClaim", "chefs-system", "platform-data")["spec"]["resources"]["requests"]["storage"], "2Gi")
        before = dep["spec"]["template"]["metadata"]["annotations"]["chefsprodigy/env-checksum"]
        self.write_env(PLATFORM_ADMIN_TOKEN="rotated-admin-secret")
        self.run_hubctl("init")
        after = self.obj("Deployment", "chefs-system", "platform")["spec"]["template"]["metadata"]["annotations"]
        self.assertNotEqual(after["chefsprodigy/env-checksum"], before, "a changed Secret restarts the platform")

        code, out = self.run_hubctl("render", "--json", "all")
        self.assertEqual(code, 0, out)
        items = json.loads(out)["items"]
        self.assertIn(("Ingress", "platform"), {(i["kind"], i["metadata"]["name"]) for i in items})
        self.assertNotIn("rotated-admin-secret", out, "render masks the admin token")
        secret = next(i for i in items if i["kind"] == "Secret" and i["metadata"]["name"] == "platform-env")
        self.assertEqual(secret["stringData"]["PLATFORM_ADMIN_TOKEN"], hubctl.PLATFORM_PLACEHOLDER)

    def test_create_registers_the_hub_with_its_cluster_and_public_urls(self):
        code, out = self.run_hubctl("create", "demo", "--chef", "Demo Chef")
        self.assertEqual(code, 0, out)
        hub = self.platform_state()["hubs"]["demo"]
        self.assertEqual(hub["internal_url"], "http://hub-demo.chefs-hubs.svc:8080")
        self.assertEqual(hub["public_url"], "http://demo.example.test")
        self.assertEqual(hub["chef_name"], "Demo Chef")
        calls = self.calls()
        register = next(i for i, c in enumerate(calls) if f"{PLATFORM_EXEC} add-hub demo" in c)
        self.assertIn("--internal-url http://hub-demo.chefs-hubs.svc:8080 --public-url http://demo.example.test",
                      calls[register])
        hub_ready = next(i for i, c in enumerate(calls) if "rollout status statefulset/hub-demo" in c)
        platform_ready = next(i for i, c in enumerate(calls) if "rollout status deployment/platform" in c)
        self.assertLess(hub_ready, register, "the hub is up before it is registered")
        self.assertLess(platform_ready, register, "the platform is ready before hubctl registers the hub")
        self.assertIn("registered at platform", out)

        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.platform_state()["hubs"]["demo"]["chef_name"], "Demo Chef",
                         "a repeated create re-registers with the stored chef name")

    def test_create_reports_a_failed_registration(self):
        self.run_hubctl("init")
        state = self.raw_state()
        del state["deployment/chefs-system/platform"]
        self.state.write_text(json.dumps(state))
        hubctl_platform_manifests = hubctl.platform_manifests
        hubctl.platform_manifests = lambda cfg, values: []  # the platform Deployment does not come up
        try:
            code, out = self.run_hubctl("create", "demo")
        finally:
            hubctl.platform_manifests = hubctl_platform_manifests
        self.assertEqual(code, 1, out)
        self.assertIn("could not register demo", out)
        self.assertIn("statefulset/chefs-hubs/hub-demo", self.objects(), "the hub itself is still created")

    def test_remove_unregisters_the_hub_from_the_platform(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("remove", "demo")
        self.assertEqual(code, 0, out)
        self.assertNotIn("demo", self.platform_state()["hubs"])
        self.assertTrue(any(f"{PLATFORM_EXEC} remove-hub demo" in c for c in self.calls()), self.calls())
        self.assertIn("search index cleared", out)

    def test_upgrade_rolls_the_platform_after_the_gateway_and_before_the_hubs(self):
        self.run_hubctl("create", "demo")
        self.write_env(HUB_IMAGE="ghcr.io/example/hub:2.0.0")
        self.log.write_text("")
        code, out = self.run_hubctl("upgrade")
        self.assertEqual(code, 0, out)
        dep = self.obj("Deployment", "chefs-system", "platform")
        self.assertEqual(dep["spec"]["template"]["spec"]["containers"][0]["image"], "ghcr.io/example/hub:2.0.0")
        images = [c for c in self.calls() if c.startswith("set image")]
        order = [c.split()[2] for c in images]
        self.assertEqual(order, ["deployment/llm-gateway", "deployment/platform", "statefulset/hub-demo"])
        self.assertIn("platform: ready", out)

    def test_status_shows_the_platform(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("status")
        self.assertEqual(code, 0, out)
        self.assertRegex(out, r"platform\s+http://example\.test\s+Running ready")


try:  # PyYAML is optional: when present the emitted YAML is parsed back and compared.
    import yaml  # type: ignore
except ImportError:  # pragma: no cover
    yaml = None

K8S_DIR = HERE.parent / "k8s"
REFERENCE_FILES = ["namespaces.yaml", "network-policies.yaml", "traefik-config.yaml", "backup-cronjob.yaml",
                   "llm-gateway.yaml", "platform.yaml", "hub.example.yaml"]


class Render(HubctlCase):
    def test_render_json_lists_the_hub_manifests(self):
        code, out = self.run_hubctl("render", "--json", "demo")
        self.assertEqual(code, 0, out)
        doc = json.loads(out)
        kinds = sorted(item["kind"] for item in doc["items"])
        self.assertEqual(kinds, ["Ingress", "Secret", "Service", "StatefulSet"])
        self.assertEqual(self.calls(), [c for c in self.calls() if c.startswith("get ")], "render only reads")

    def test_render_defaults_to_yaml_documents(self):
        code, out = self.run_hubctl("render", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(out.count("\n---\n"), 3, out)
        for kind in ("Secret", "StatefulSet", "Service", "Ingress"):
            self.assertIn(f"kind: {kind}\n", out)
        self.assertIn('value: "2"', out, "numeric-looking strings stay strings")
        self.assertIn("name: hub-demo-env", out)
        if yaml:
            docs = list(yaml.safe_load_all(out))
            _, as_json = self.run_hubctl("render", "--json", "demo")
            self.assertEqual(docs, json.loads(as_json)["items"])

    def test_render_all_covers_the_cluster_objects(self):
        code, out = self.run_hubctl("render", "--json", "all")
        self.assertEqual(code, 0, out)
        kinds = {item["kind"] for item in json.loads(out)["items"]}
        self.assertTrue({"Namespace", "NetworkPolicy", "HelmChartConfig", "Middleware", "CronJob", "Role", "RoleBinding"} <= kinds, kinds)

    def test_reference_manifests_match_the_checked_in_files(self):
        (self.root / ".env").unlink()  # the reference set does not depend on a deployment
        out_dir = Path(self.tmp.name) / "ref"
        code, out = self.run_hubctl("render", "--reference", str(out_dir))
        self.assertEqual(code, 0, out)
        self.assertEqual(self.calls(), [], "the reference render never talks to kubectl")
        self.assertEqual(sorted(p.name for p in out_dir.iterdir()), sorted(REFERENCE_FILES))
        for name in REFERENCE_FILES:
            checked_in = K8S_DIR / name
            self.assertTrue(checked_in.exists(), f"{checked_in} missing: run deploy/hubctl render --reference deploy/k8s")
            self.assertEqual((out_dir / name).read_text(), checked_in.read_text(),
                             f"deploy/k8s/{name} is stale: run deploy/hubctl render --reference deploy/k8s")
        traefik = (out_dir / "traefik-config.yaml").read_text()
        self.assertIn("kind: TLSStore", traefik, "the reference set is the production (origin TLS) shape")
        self.assertIn("redirections", traefik)
        self.assertNotIn("buffering", traefik)
        hub = (out_dir / "hub.example.yaml").read_text()
        self.assertIn("jacob.chefslegacy.com", hub)
        self.assertNotRegex(hub, r"[0-9a-f]{64}", "no real key in the example")
        backup = (out_dir / "backup-cronjob.yaml").read_text()
        self.assertNotIn("RCLONE_CONFIG_R2_SECRET_ACCESS_KEY", backup, "no credentials in the reference set")
        gateway = (out_dir / "llm-gateway.yaml").read_text()
        for kind in ("Secret", "PersistentVolumeClaim", "Deployment", "Service", "NetworkPolicy"):
            self.assertIn(f"kind: {kind}\n", gateway)
        self.assertNotRegex(gateway, r"[0-9a-f]{64}", "no real key in the gateway example")
        self.assertIn('MINIMAX_ANTHROPIC_API_BASE: "http://llm-gateway.chefs-system.svc:8080/anthropic"', hub,
                      "the example hub talks to the gateway")
        platform = (out_dir / "platform.yaml").read_text()
        for kind in ("Secret", "PersistentVolumeClaim", "Deployment", "Service", "Ingress", "NetworkPolicy"):
            self.assertIn(f"kind: {kind}\n", platform)
        self.assertIn("- host: chefslegacy.com\n", platform, "the apex Ingress")
        self.assertIn("websecure", platform)
        self.assertNotIn("/admin", platform)
        self.assertIn(f'PLATFORM_ADMIN_TOKEN: "{hubctl.PLATFORM_PLACEHOLDER}"', platform)
        self.assertNotRegex(platform, r"[0-9a-f]{64}", "no real token in the platform example")

    @unittest.skipUnless(yaml, "PyYAML not installed")
    def test_reference_files_parse_as_yaml(self):
        for name in REFERENCE_FILES:
            docs = [d for d in yaml.safe_load_all((K8S_DIR / name).read_text()) if d]
            self.assertTrue(docs, name)
            for d in docs:
                self.assertIn("kind", d, name)


class YamlEmitter(unittest.TestCase):
    def test_scalars_are_quoted_only_when_needed(self):
        out = hubctl.to_yaml({
            "schedule": "17 3 * * *", "threads": "2", "flag": True, "count": 1, "word": "yes", "empty": "",
            "path": "/api/v1/health", "image": "ghcr.io/x/y:1.2", "colon": "has: colon", "none": None, "dash": "-x",
        })
        self.assertEqual(out, (
            'schedule: "17 3 * * *"\n'
            'threads: "2"\n'
            "flag: true\n"
            "count: 1\n"
            'word: "yes"\n'
            'empty: ""\n'
            "path: /api/v1/health\n"
            'image: "ghcr.io/x/y:1.2"\n'
            'colon: "has: colon"\n'
            "none: null\n"
            'dash: "-x"\n'
        ))

    def test_nested_mappings_lists_and_empties(self):
        out = hubctl.to_yaml({"spec": {"items": [{"name": "x", "ports": [8080, "http"]}, "s", ["a", "b"]], "empty": {}, "none": []}})
        self.assertEqual(out, (
            "spec:\n"
            "  items:\n"
            "    - name: x\n"
            "      ports:\n"
            "        - 8080\n"
            "        - http\n"
            "    - s\n"
            "    - - a\n"
            "      - b\n"
            "  empty: {}\n"
            "  none: []\n"
        ))

    def test_multiline_strings_become_block_scalars(self):
        out = hubctl.to_yaml({"script": "#!/bin/sh\necho hi\n", "nonl": "a\n\nb", "items": ["x\ny\n"]})
        self.assertEqual(out, (
            "script: |\n"
            "  #!/bin/sh\n"
            "  echo hi\n"
            "nonl: |-\n"
            "  a\n"
            "\n"
            "  b\n"
            "items:\n"
            "  - |\n"
            "    x\n"
            "    y\n"
        ))

    @unittest.skipUnless(yaml, "PyYAML not installed")
    def test_round_trips_through_pyyaml(self):
        doc = {
            "a": "17 3 * * *", "b": "2", "c": True, "d": 1, "e": "yes", "f": "", "g": None, "h": "x: y", "i": "#c", "j": " lead",
            "k": "trail ", "l": "0.0.0.0/0", "m": ["1", 1, 1.5, "1.5", "null", "~", "0x1f", "1e3", "2026-10-27", "1:30"],
            "n": {"kubernetes.io/metadata.name": "chefs-hubs"}, "o": "line1\nline2\n", "p": "line1\n\n", "q": "  indented first\nsecond",
            "r": "unicode é ü 🍜", "s": "tab\there", "t": "quote\"inside", "u": "back\\slash", "v": "- not a list", "w": "? not a key",
        }
        self.assertEqual(yaml.safe_load(hubctl.to_yaml(doc)), doc)


if __name__ == "__main__":
    unittest.main()
