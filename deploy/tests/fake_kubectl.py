#!/usr/bin/env python3
"""A stand-in `kubectl` for hubctl's tests: keeps applied objects in a JSON state
file (FAKE_KUBE_STATE), logs every invocation (FAKE_LOG) and answers the handful
of read commands hubctl uses. Pods are synthesized from stored StatefulSets and
Deployments; `exec deploy/llm-gateway -- llm-gateway …` emulates the gateway CLI
with hub tokens and usage rows kept under the state's "_gateway" key.
"""

import json
import os
import secrets
import sys
from pathlib import Path

STATE = Path(os.environ["FAKE_KUBE_STATE"])
LOG = Path(os.environ["FAKE_LOG"])


def load():
    return json.loads(STATE.read_text()) if STATE.exists() else {}


def save(state):
    STATE.write_text(json.dumps(state, indent=1, sort_keys=True))


def key(kind, ns, name):
    return f"{kind.lower()}/{ns}/{name}"


def flag(args, name, default=None):
    for i, a in enumerate(args):
        if a == name and i + 1 < len(args):
            return args[i + 1]
        if a.startswith(name + "="):
            return a.split("=", 1)[1]
    return default


KINDS = {
    "secret": "Secret", "secrets": "Secret", "statefulset": "StatefulSet", "statefulsets": "StatefulSet",
    "sts": "StatefulSet", "service": "Service", "svc": "Service", "ingress": "Ingress", "ingresses": "Ingress",
    "cronjob": "CronJob", "cronjobs": "CronJob", "job": "Job", "jobs": "Job", "pod": "Pod", "pods": "Pod",
    "pvc": "PersistentVolumeClaim", "persistentvolumeclaim": "PersistentVolumeClaim",
    "persistentvolumeclaims": "PersistentVolumeClaim", "namespace": "Namespace", "namespaces": "Namespace",
    "networkpolicy": "NetworkPolicy", "networkpolicies": "NetworkPolicy", "configmap": "ConfigMap",
    "serviceaccount": "ServiceAccount", "role": "Role", "rolebinding": "RoleBinding",
    "deployment": "Deployment", "deployments": "Deployment", "deploy": "Deployment",
    "middleware": "Middleware", "tlsstore": "TLSStore", "helmchartconfig": "HelmChartConfig",
}


def norm_kind(word):
    return KINDS.get(word.lower(), word)


def matches(obj, selector):
    if not selector:
        return True
    labels = obj.get("metadata", {}).get("labels", {})
    for pair in selector.split(","):
        k, _, v = pair.partition("=")
        if labels.get(k) != v:
            return False
    return True


def synthesized_pods(state, ns):
    pods = []
    for k, obj in state.items():
        if obj.get("kind") not in ("StatefulSet", "Deployment") or obj["metadata"].get("namespace") != ns:
            continue
        replicas = obj.get("spec", {}).get("replicas", 1)
        for i in range(replicas):
            suffix = str(i) if obj["kind"] == "StatefulSet" else f"7f9c-{i}"
            pods.append({
                "kind": "Pod",
                "metadata": {"name": f"{obj['metadata']['name']}-{suffix}", "namespace": ns,
                             "labels": obj["spec"]["template"]["metadata"].get("labels", {})},
                "status": {"phase": "Running", "containerStatuses": [{"name": "node", "ready": True, "restartCount": 0}]},
            })
    return pods


def gateway_cli(state, args):
    """The llm-gateway subcommands hubctl runs through `kubectl exec`."""
    gw = state.setdefault("_gateway", {"hubs": {}, "usage": []})
    sub, rest = args[0], args[1:]
    name = next((a for a in rest if not a.startswith("-")), None)
    if sub == "add-hub":
        hub = gw["hubs"].get(name)
        if hub and "--rotate" not in rest:
            print(f"Error: hub {name} already exists", file=sys.stderr)
            return 1
        token = secrets.token_hex(32)
        created = hub["created_at"] if hub else "2026-10-27T00:00:00Z"
        gw["hubs"][name] = {"token": token, "status": "active", "created_at": created}
        save(state)
        print(json.dumps({"name": name, "token": token, "created_at": created}))
        return 0
    if sub == "disable-hub":
        if name in gw["hubs"]:
            gw["hubs"][name]["status"] = "disabled"
            save(state)
            print(f"hub {name} disabled")
        else:
            print(f"hub {name} is not registered")
        return 0
    if sub == "usage":
        hub, start, end = flag(rest, "--hub"), flag(rest, "--from"), flag(rest, "--to")
        rows = [r for r in gw["usage"] if (not hub or r["hub"] == hub)
                and (not start or r["day"] >= start) and (not end or r["day"] <= end)]
        print(json.dumps(sorted(rows, key=lambda r: (r["day"], r["hub"]), reverse=True), indent=2))
        return 0
    print(f"fake llm-gateway: unsupported {args}", file=sys.stderr)
    return 2


def main(argv):
    with LOG.open("a") as log:
        log.write(" ".join(argv) + "\n")
    state = load()
    cmd = argv[0] if argv else ""
    ns = flag(argv, "-n") or flag(argv, "--namespace") or "default"

    if cmd == "apply":
        src = flag(argv, "-f")
        if src == "-":
            doc = json.load(sys.stdin)
            items = doc.get("items", []) if doc.get("kind") == "List" else [doc]
            for item in items:
                meta = item.setdefault("metadata", {})
                item_ns = meta.get("namespace") or ns
                if item["kind"] not in ("Namespace", "TLSStore", "HelmChartConfig") or meta.get("namespace"):
                    meta["namespace"] = item_ns
                state[key(item["kind"], meta.get("namespace", ""), meta["name"])] = item
                print(f"{item['kind'].lower()}/{meta['name']} configured")
            save(state)
        else:
            print(f"applied {src}")
        return 0

    if cmd == "get":
        words = [a for a in argv[1:] if not a.startswith("-") and a not in (ns, flag(argv, "-l") or "", flag(argv, "-o") or "")]
        kind_word, name = (words + [None, None])[:2]
        if kind_word and "/" in kind_word:
            kind_word, name = kind_word.split("/", 1)
        kind = norm_kind(kind_word or "")
        selector = flag(argv, "-l") or flag(argv, "--selector")
        if kind == "Pod":
            items = [p for p in synthesized_pods(state, ns) if matches(p, selector)]
        else:
            items = [o for k, o in state.items() if o.get("kind") == kind
                     and (o["metadata"].get("namespace", "") == ns or kind in ("Namespace", "TLSStore", "HelmChartConfig"))
                     and matches(o, selector)]
        if name:
            found = [o for o in items if o["metadata"]["name"] == name]
            if not found:
                print(f'Error from server (NotFound): {kind_word} "{name}" not found', file=sys.stderr)
                return 1
            print(json.dumps(found[0]))
        else:
            print(json.dumps({"kind": "List", "items": items}))
        return 0

    if cmd == "delete":
        words = [a for a in argv[1:] if not a.startswith("-") and a != ns]
        kind_word, name = (words + [None, None])[:2]
        if kind_word and "/" in kind_word:
            kind_word, name = kind_word.split("/", 1)
        k = key(norm_kind(kind_word), ns, name)
        if k in state:
            del state[k]
            save(state)
            print(f"{kind_word} \"{name}\" deleted")
        elif "--ignore-not-found" not in argv:
            print(f'Error from server (NotFound): {kind_word} "{name}" not found', file=sys.stderr)
            return 1
        return 0

    if cmd == "scale":
        target = next(a for a in argv[1:] if "/" in a)
        kind_word, name = target.split("/", 1)
        replicas = int(flag(argv, "--replicas"))
        obj = state.get(key(norm_kind(kind_word), ns, name))
        if obj is None:
            print("not found", file=sys.stderr)
            return 1
        obj.setdefault("spec", {})["replicas"] = replicas
        save(state)
        print(f"{target} scaled")
        return 0

    if cmd == "set" and len(argv) > 1 and argv[1] == "image":
        target = next(a for a in argv[2:] if "/" in a and "=" not in a)
        kind_word, name = target.split("/", 1)
        assignment = next(a for a in argv[2:] if "=" in a and not a.startswith("-"))
        container, image = assignment.split("=", 1)
        obj = state.get(key(norm_kind(kind_word), ns, name))
        if obj is None:
            print("not found", file=sys.stderr)
            return 1
        for c in obj["spec"]["template"]["spec"]["containers"]:
            if c["name"] == container:
                c["image"] = image
        save(state)
        print(f"{target} image updated")
        return 0

    if cmd == "rollout":
        print("rollout ok")
        return 0
    if cmd == "wait":
        if state.get("_fail_wait"):
            print("error: timed out waiting for the condition", file=sys.stderr)
            return 1
        print("condition met")
        return 0
    if cmd == "logs":
        print("fake backup log: hub demo: exported")
        return 0
    if cmd == "exec" and "--" in argv:
        target = next(a for a in argv[1:argv.index("--")] if not a.startswith("-") and a != ns)
        command = argv[argv.index("--") + 1:]
        if command and command[0].endswith("llm-gateway"):
            name = target.split("/", 1)[-1]
            if key("Deployment", ns, name) not in state:
                print(f'Error from server (NotFound): deployments.apps "{name}" not found', file=sys.stderr)
                return 1
            return gateway_cli(state, command[1:])
        print("exec ok")
        return 0
    if cmd in ("cp", "exec", "run", "top", "label", "annotate"):
        print(f"{cmd} ok")
        return 0
    print(f"fake kubectl: unsupported command {argv}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
