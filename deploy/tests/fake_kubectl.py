#!/usr/bin/env python3
"""A stand-in `kubectl` for hubctl's tests: keeps applied objects in a JSON state
file (FAKE_KUBE_STATE), logs every invocation (FAKE_LOG) and answers the handful
of read commands hubctl uses. Pods are synthesized from stored StatefulSets.
"""

import json
import os
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
        if obj.get("kind") != "StatefulSet" or obj["metadata"].get("namespace") != ns:
            continue
        replicas = obj.get("spec", {}).get("replicas", 1)
        for i in range(replicas):
            pods.append({
                "kind": "Pod",
                "metadata": {"name": f"{obj['metadata']['name']}-{i}", "namespace": ns,
                             "labels": obj["spec"]["template"]["metadata"].get("labels", {})},
                "status": {"phase": "Running", "containerStatuses": [{"name": "node", "ready": True, "restartCount": 0}]},
            })
    return pods


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
        print("condition met")
        return 0
    if cmd == "logs":
        print("fake backup log: hub demo: exported")
        return 0
    if cmd in ("cp", "exec", "run", "top", "label", "annotate"):
        print(f"{cmd} ok")
        return 0
    print(f"fake kubectl: unsupported command {argv}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
