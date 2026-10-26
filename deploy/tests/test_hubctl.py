"""Tests for deploy/hubctl against a temporary root, a fake `docker` and a fake `rclone`.

Run: python3 -I -m unittest discover -s deploy/tests
"""

import contextlib
import hashlib
import hmac
import importlib.machinery
import importlib.util
import io
import os
import sqlite3
import stat
import tempfile
import time
import unittest
import zipfile
from pathlib import Path
from urllib.parse import parse_qs, urlparse

HERE = Path(__file__).resolve().parent
HUBCTL_PATH = HERE.parent / "hubctl"

FAKE_DOCKER = """#!/bin/sh
echo "$@" >> "$FAKE_LOG"
case "$*" in
  *inspect*) echo healthy ;;
esac
exit 0
"""

FAKE_RCLONE = """#!/bin/sh
echo "$@" >> "$FAKE_LOG"
if [ "$1" = "copyto" ]; then
  dest="$FAKE_REMOTE/${3#*:}"
  mkdir -p "$(dirname "$dest")"
  cp "$2" "$dest"
fi
exit 0
"""


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
        self.remote = base / "remote"
        self.log = base / "calls.log"
        self.log.write_text("")
        bin_dir = base / "bin"
        bin_dir.mkdir()
        self.fake_docker = bin_dir / "docker"
        self.fake_docker.write_text(FAKE_DOCKER)
        self.fake_docker.chmod(0o755)
        self.fake_rclone = bin_dir / "rclone"
        self.fake_rclone.write_text(FAKE_RCLONE)
        self.fake_rclone.chmod(0o755)
        self.env_patch = {
            "HUBCTL_DOCKER": str(self.fake_docker),
            "HUBCTL_RCLONE": str(self.fake_rclone),
            "FAKE_LOG": str(self.log),
            "FAKE_REMOTE": str(self.remote),
        }
        self.saved = {k: os.environ.get(k) for k in self.env_patch}
        os.environ.update(self.env_patch)
        self.write_env(TLS_MODE="origin")
        certs = self.root / "caddy" / "certs"
        certs.mkdir(parents=True)
        (certs / "origin.pem").write_text("cert")
        (certs / "origin.key").write_text("key")

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
            "TLS_MODE": "origin",
            "R2_REMOTE": "r2:bucket",
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

    def hub_env(self, name="demo"):
        return hubctl.read_env_file(self.root / "hubs" / name / ".env")


class NameValidation(unittest.TestCase):
    def test_accepts_valid_subdomains_and_rejects_the_rest(self):
        for ok in ["jacob", "maria-2", "a", "x" * 63, "chef42"]:
            hubctl.validate_name(ok)
        for bad in ["", "WWW", "-bad", "bad-", "a.b", "under_score", "x" * 64, "hub name"]:
            with self.assertRaises(hubctl.HubctlError, msg=bad):
                hubctl.validate_name(bad)

    def test_rejects_reserved_names(self):
        for reserved in ["www", "api", "mail", "admin", "gateway"]:
            with self.assertRaises(hubctl.HubctlError, msg=reserved):
                hubctl.validate_name(reserved)


class Create(HubctlCase):
    def test_create_writes_secrets_compose_and_caddyfile_and_starts_the_hub(self):
        code, out = self.run_hubctl("create", "demo", "--chef", "Demo Chef")
        self.assertEqual(code, 0, out)

        env_path = self.root / "hubs" / "demo" / ".env"
        self.assertEqual(stat.S_IMODE(env_path.stat().st_mode), 0o600)
        env = self.hub_env()
        self.assertRegex(env["NODE_API_KEY"], r"^[0-9a-f]{64}$")
        self.assertEqual(env["NODE_CHEF_NAME"], "Demo Chef")
        self.assertEqual(env["NODE_PUBLIC_URL"], "https://demo.example.test")
        self.assertEqual(env["NODE_HOST"], "0.0.0.0")
        self.assertEqual(env["NODE_DATA_DIR"], "/data")
        self.assertEqual(env["LLM_MODEL"], "minimax/MiniMax-M3", "phase 0 copies the model settings")
        self.assertEqual(env["MINIMAX_API_KEY"], "vendor-key-for-tests")
        self.assertIn(env["NODE_API_KEY"], out, "the owner key is printed once")
        self.assertTrue((self.root / "hubs" / "demo" / "data").is_dir())

        compose = (self.root / "compose.hubs.yml").read_text()
        for needle in [
            "hub-demo:",
            "image: ghcr.io/example/hub:1.2.3",
            "env_file: hubs/demo/.env",
            "caddy: demo.example.test",
            'caddy.reverse_proxy: "{{upstreams 8080}}"',
            'caddy.reverse_proxy.flush_interval: "-1"',
            "networks: [chefs_net]",
            "mem_limit: 128m",
            "read_only: true",
            '"/usr/local/bin/node-server", "healthcheck"',
        ]:
            self.assertIn(needle, compose)

        caddyfile = (self.root / "caddy" / "Caddyfile").read_text()
        self.assertIn("trusted_proxies static", caddyfile)
        self.assertIn("173.245.48.0/20", caddyfile)
        self.assertIn("tls /certs/origin.pem /certs/origin.key", caddyfile)
        self.assertIn("delete token", caddyfile)
        self.assertIn("max_size 110MB", caddyfile)
        self.assertIn("\nexample.test {", caddyfile)
        self.assertTrue((self.root / "compose.yml").exists(), "base compose copied next to the generated one")

        calls = " | ".join(self.calls())
        self.assertIn("up -d caddy", calls)
        self.assertIn("up -d hub-demo", calls)
        self.assertIn("-f " + str(self.root / "compose.yml"), calls)
        self.assertIn("-f " + str(self.root / "compose.hubs.yml"), calls)

    def test_create_is_idempotent_and_keeps_the_key(self):
        self.run_hubctl("create", "demo")
        key = self.hub_env()["NODE_API_KEY"]
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.hub_env()["NODE_API_KEY"], key)
        self.assertNotIn(key, out, "an existing key is never printed again")
        self.assertEqual((self.root / "compose.hubs.yml").read_text().count("hub-demo:"), 1)

    def test_two_hubs_share_one_generated_file(self):
        self.run_hubctl("create", "jacob")
        self.run_hubctl("create", "maria")
        compose = (self.root / "compose.hubs.yml").read_text()
        self.assertIn("hub-jacob:", compose)
        self.assertIn("hub-maria:", compose)
        code, out = self.run_hubctl("list")
        self.assertEqual(code, 0)
        self.assertIn("jacob", out)
        self.assertIn("https://maria.example.test", out)

    def test_tls_off_mode_renders_plain_http_sites(self):
        self.write_env(TLS_MODE="off", DOMAIN="localhost")
        code, out = self.run_hubctl("create", "demo")
        self.assertEqual(code, 0, out)
        self.assertIn("caddy: http://demo.localhost", (self.root / "compose.hubs.yml").read_text())
        caddyfile = (self.root / "caddy" / "Caddyfile").read_text()
        self.assertNotIn("tls /certs", caddyfile)
        self.assertIn("auto_https off", caddyfile)
        self.assertIn("\nhttp://localhost {", caddyfile)
        self.assertEqual(self.hub_env()["NODE_PUBLIC_URL"], "http://demo.localhost")

    def test_origin_mode_requires_the_certificate_files(self):
        (self.root / "caddy" / "certs" / "origin.key").unlink()
        code, out = self.run_hubctl("create", "demo")
        self.assertNotEqual(code, 0)
        self.assertIn("origin.key", out)

    def test_missing_domain_is_a_clear_error(self):
        self.write_env(DOMAIN=None)
        code, out = self.run_hubctl("create", "demo")
        self.assertNotEqual(code, 0)
        self.assertIn("DOMAIN", out)


class Keys(HubctlCase):
    def test_login_link_token_verifies_with_the_hub_key(self):
        self.run_hubctl("create", "demo")
        key = self.hub_env()["NODE_API_KEY"]
        code, out = self.run_hubctl("login-link", "demo")
        self.assertEqual(code, 0, out)
        url = next(w for w in out.split() if w.startswith("https://"))
        parsed = urlparse(url)
        self.assertEqual(parsed.netloc, "demo.example.test")
        self.assertEqual(parsed.path, "/api/v1/auth/magic")
        token = parse_qs(parsed.query)["token"][0]
        expires, nonce, mac = token.split(".")
        self.assertRegex(nonce, r"^[0-9a-f]{16}$")
        self.assertTrue(0 < int(expires) - time.time() <= 600)
        expected = hmac.new(
            key.encode(), f"chefsprodigy-magic:{expires}:{nonce}".encode(), hashlib.sha256
        ).hexdigest()
        self.assertEqual(mac, expected)

    def test_rotate_key_writes_a_new_key_and_recreates_the_hub(self):
        self.run_hubctl("create", "demo")
        old = self.hub_env()["NODE_API_KEY"]
        self.log.write_text("")
        code, out = self.run_hubctl("rotate-key", "demo")
        self.assertEqual(code, 0, out)
        new = self.hub_env()["NODE_API_KEY"]
        self.assertNotEqual(new, old)
        self.assertIn(new, out)
        self.assertEqual(self.hub_env()["NODE_CHEF_NAME"], "Demo", "other settings survive")
        self.assertTrue(any("--force-recreate hub-demo" in c for c in self.calls()), self.calls())


class BackupRestore(HubctlCase):
    def seed_data(self, name="demo", title="Pho"):
        data = self.root / "hubs" / name / "data"
        (data / "images").mkdir(parents=True, exist_ok=True)
        con = sqlite3.connect(data / "recipes.db")
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("CREATE TABLE IF NOT EXISTS recipes (id TEXT PRIMARY KEY, title TEXT)")
        con.execute("DELETE FROM recipes")
        con.execute("INSERT INTO recipes VALUES ('01A', ?)", (title,))
        con.commit()
        # keep the connection open so a -wal file exists while we snapshot
        (data / "images" / "01A.webp").write_bytes(b"RIFF....WEBP")
        return con

    def test_backup_uploads_a_consistent_archive(self):
        self.run_hubctl("create", "demo")
        con = self.seed_data()
        (self.root / "gateway" / "data").mkdir(parents=True)
        g = sqlite3.connect(self.root / "gateway" / "data" / "gateway.db")
        g.execute("CREATE TABLE t (x)")
        g.commit()
        g.close()

        code, out = self.run_hubctl("backup", "all")
        self.assertEqual(code, 0, out)
        con.close()

        uploaded = sorted(p.relative_to(self.remote).as_posix() for p in self.remote.rglob("*") if p.is_file())
        self.assertEqual(len(uploaded), 2, uploaded)
        hub_zip = next(p for p in uploaded if p.startswith("bucket/hubs/demo/demo-"))
        self.assertTrue(hub_zip.endswith(".zip"), hub_zip)
        self.assertTrue(any(p.startswith("bucket/gateway/gateway-") and p.endswith(".db") for p in uploaded), uploaded)

        with zipfile.ZipFile(self.remote / hub_zip) as zf:
            names = zf.namelist()
            self.assertIn("recipes.db", names)
            self.assertIn("images/01A.webp", names)
            with tempfile.TemporaryDirectory() as d:
                zf.extract("recipes.db", d)
                rows = sqlite3.connect(Path(d) / "recipes.db").execute("SELECT title FROM recipes").fetchall()
        self.assertEqual(rows, [("Pho",)])
        self.assertEqual(list((self.root / "backups").rglob("*.zip")), [], "local copy removed after upload")

    def test_backup_without_remote_needs_the_local_flag(self):
        self.write_env(R2_REMOTE=None)
        self.run_hubctl("create", "demo")
        self.seed_data().close()
        code, out = self.run_hubctl("backup", "demo")
        self.assertNotEqual(code, 0)
        self.assertIn("R2_REMOTE", out)
        code, out = self.run_hubctl("backup", "demo", "--local")
        self.assertEqual(code, 0, out)
        archives = list((self.root / "backups" / "demo").glob("demo-*.zip"))
        self.assertEqual(len(archives), 1)
        self.assertIn(str(archives[0]), out)

    def test_restore_replaces_data_and_restarts_the_hub(self):
        self.run_hubctl("create", "demo")
        self.seed_data(title="Original").close()
        code, out = self.run_hubctl("backup", "demo", "--local")
        self.assertEqual(code, 0, out)
        archive = next((self.root / "backups" / "demo").glob("demo-*.zip"))

        self.seed_data(title="Changed later").close()
        (self.root / "hubs" / "demo" / "data" / "images" / "junk.webp").write_bytes(b"x")
        self.log.write_text("")
        code, out = self.run_hubctl("restore", "demo", str(archive))
        self.assertEqual(code, 0, out)

        data = self.root / "hubs" / "demo" / "data"
        rows = sqlite3.connect(data / "recipes.db").execute("SELECT title FROM recipes").fetchall()
        self.assertEqual(rows, [("Original",)])
        self.assertFalse((data / "images" / "junk.webp").exists(), "restore replaces the data directory")
        self.assertTrue((data / "images" / "01A.webp").exists())
        self.assertTrue(any(p.name.startswith("data.pre-restore-") for p in (self.root / "hubs" / "demo").iterdir()))
        calls = self.calls()
        self.assertTrue(any(c.endswith("stop hub-demo") for c in calls), calls)
        self.assertTrue(any(c.endswith("up -d hub-demo") for c in calls), calls)

    def test_restore_rejects_archives_without_a_database_or_with_bad_paths(self):
        self.run_hubctl("create", "demo")
        bad = self.root / "bad.zip"
        with zipfile.ZipFile(bad, "w") as zf:
            zf.writestr("notes.txt", "x")
        code, out = self.run_hubctl("restore", "demo", str(bad))
        self.assertNotEqual(code, 0)
        self.assertIn("recipes.db", out)
        evil = self.root / "evil.zip"
        with zipfile.ZipFile(evil, "w") as zf:
            zf.writestr("recipes.db", "x")
            zf.writestr("../escape", "x")
        code, out = self.run_hubctl("restore", "demo", str(evil))
        self.assertNotEqual(code, 0)


class RemoveUpgradeStatus(HubctlCase):
    def test_remove_keeps_data_until_purge(self):
        self.run_hubctl("create", "demo")
        (self.root / "hubs" / "demo" / "data" / "recipes.db").write_bytes(b"db")
        code, out = self.run_hubctl("remove", "demo")
        self.assertEqual(code, 0, out)
        self.assertNotIn("hub-demo:", (self.root / "compose.hubs.yml").read_text())
        self.assertTrue(any("rm -s -f hub-demo" in c for c in self.calls()), self.calls())
        self.assertFalse((self.root / "hubs" / "demo").exists())
        kept = list((self.root / "hubs" / ".removed").glob("demo*"))
        self.assertEqual(len(kept), 1, "data parked under hubs/.removed")
        code, out = self.run_hubctl("remove", "demo", "--purge")
        self.assertEqual(code, 0, out)
        self.assertEqual(list((self.root / "hubs" / ".removed").glob("demo*")), [])

    def test_upgrade_pulls_and_restarts_each_hub(self):
        self.run_hubctl("create", "jacob")
        self.run_hubctl("create", "maria")
        self.log.write_text("")
        code, out = self.run_hubctl("upgrade", "all")
        self.assertEqual(code, 0, out)
        calls = self.calls()
        self.assertTrue(any(c.endswith("pull hub-jacob hub-maria") for c in calls), calls)
        self.assertTrue(any(c.endswith("up -d hub-jacob") for c in calls), calls)
        self.assertTrue(any(c.endswith("up -d hub-maria") for c in calls), calls)

    def test_status_reports_health_per_hub(self):
        self.run_hubctl("create", "demo")
        code, out = self.run_hubctl("status")
        self.assertEqual(code, 0, out)
        self.assertIn("demo", out)
        self.assertIn("healthy", out)


if __name__ == "__main__":
    unittest.main()
