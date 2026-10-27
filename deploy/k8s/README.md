# deploy/k8s: reference manifests

These files are **generated** by `deploy/hubctl render --reference deploy/k8s` with example
settings (`DOMAIN=chefslegacy.com`, `TLS_MODE=origin`, no credentials). They exist so the
cluster's shape can be reviewed in a pull request; the live cluster gets the same objects
from `hubctl init` (namespaces, network policies, Traefik config, backup CronJob) and
`hubctl create <name>` (one Secret, StatefulSet, Service and Ingress per hub).

| File | Objects |
|------|---------|
| `namespaces.yaml` | `chefs-system`, `chefs-hubs` |
| `network-policies.yaml` | default deny in `chefs-hubs`; hubs accept Traefik (`kube-system`) and `role=hub-client` pods in `chefs-system`; hubs reach DNS, public addresses and `llm-gateway` only |
| `traefik-config.yaml` | `HelmChartConfig traefik` (Cloudflare trusted IPs, http→https redirect, access logs off), `Middleware headers`, `TLSStore default` → `Secret chefs-origin-tls` |
| `backup-cronjob.yaml` | `ServiceAccount hub-backup`, `Role`/`RoleBinding` to read hub Secrets, the fetch/upload scripts, `CronJob hub-backup` (rclone → R2) |
| `hub.example.yaml` | what `hubctl render jacob` prints before the hub exists: placeholder key, `https://jacob.chefslegacy.com` |

Edit `deploy/hubctl`, not these files, then regenerate them; `deploy/tests` fails when the
checked-in copy drifts. `hubctl render <name>` and `hubctl render all` print the objects for a
real deployment (reading `.env` and, for an existing hub, its current Secret); `--json`
prints a `List` instead of YAML documents.

`kubectl apply -f deploy/k8s/` would create the example hub `jacob` with a placeholder owner
key; use `hubctl create` instead, which mints the key, waits for readiness and prints the
sign-in link.
