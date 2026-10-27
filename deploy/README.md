# Hosting hubs on one k3s node

`deploy/` runs many Chef's Prodigy hubs on a single VPS: k3s (Kubernetes with Traefik,
CoreDNS, local-path storage and network policies built in), Cloudflare in front for DNS,
TLS and the WAF, one `StatefulSet` per hub, nightly backups to R2 and a Python tool,
`hubctl`, that drives `kubectl`. Design and rationale: [docs/SPEC-hosted-platform.md](../docs/SPEC-hosted-platform.md).

```
hubctl init                 namespaces, network policies, Traefik config, backup CronJob
hubctl create jacob         Secret + StatefulSet + Service + Ingress + 2Gi volume; prints the owner key once
hubctl list | status        hubs, URLs, pod state
hubctl login-link jacob     10-minute magic sign-in link for the owner
hubctl rotate-key jacob     new owner key, hub restarted
hubctl backup jacob|all     run the nightly job now and show its log
hubctl restore jacob x.zip  replace the hub's data with an export archive
hubctl upgrade [jacob|all]  roll hubs to HUB_IMAGE from .env, one at a time
hubctl remove jacob [--purge]  delete the workload; --purge also deletes the volume
hubctl render [jacob|all] [--json]  print the objects hubctl would apply
```

`hubctl` needs Python 3.9+ and `kubectl` (k3s ships one as `k3s kubectl`; `hubctl` uses
`/etc/rancher/k3s/k3s.yaml` when `KUBECONFIG` is unset). Settings live in
`/srv/chefslegacy/.env` (`--root` or `HUBCTL_ROOT` change the directory); everything else is
cluster state. Tests: `python3 -I -m unittest discover -s deploy/tests`.

## 1. Node and k3s

A 2 vCPU / 4 GB VPS runs k3s plus a few dozen hubs (a hub idles below 20 MB). Debian 12 or
Ubuntu 24.04:

```sh
curl -sfL https://get.k3s.io | sh -
kubectl get nodes          # Ready; Traefik, CoreDNS, local-path, metrics-server in kube-system
```

Keep the defaults: Traefik (the ingress on :80/:443 through ServiceLB), CoreDNS, the
local-path provisioner (`/var/lib/rancher/k3s/storage/`) and the network-policy controller
are all used. Firewall at the **provider** (Hetzner Cloud Firewall, DigitalOcean firewall,
…): allow 22 from your addresses, 80 and 443 only from [Cloudflare's ranges](https://www.cloudflare.com/ips/),
nothing else (the k3s API on 6443 stays local; use SSH). Host firewalls such as ufw
interact badly with kube-proxy's NAT rules, so prefer the provider's.

Install the tool and its settings:

```sh
install -m 0755 deploy/hubctl /usr/local/bin/hubctl
mkdir -p /srv/chefslegacy && install -m 0600 deploy/env.example /srv/chefslegacy/.env
$EDITOR /srv/chefslegacy/.env          # DOMAIN, HUB_IMAGE, model access, later R2
```

## 2. Cloudflare

1. **DNS**: `A chefslegacy.com → <node IP>` and `A *.chefslegacy.com → <node IP>`, both
   proxied (orange cloud). The node's address never appears in DNS.
2. **SSL/TLS → Overview**: encryption mode *Full (strict)*. **Edge Certificates**: *Always
   Use HTTPS* on.
3. **SSL/TLS → Origin Server → Create Certificate**: hostnames `chefslegacy.com` and
   `*.chefslegacy.com`, 15 years. Save the certificate as `origin.pem` and the key as
   `origin.key`, then on the node:

   ```sh
   kubectl -n kube-system create secret tls chefs-origin-tls --cert=origin.pem --key=origin.key
   ```

   `hubctl init` refuses to run in `TLS_MODE=origin` until this Secret exists; Traefik serves
   it as the default certificate for every hostname.
4. Optional but recommended: a WAF rate-limiting rule for `/api/v1/auth/*` and `/mcp`, and a
   firewall rule that challenges countries you do not serve.

Cloudflare's proxy caps request bodies at 100 MB on the free plan and closes connections
that stay silent for 100 s; MCP clients reconnect using the `retry` hint the hub sends.

## 3. Bring the cluster up and create the first hub

```sh
hubctl init
kubectl -n kube-system rollout status deploy/traefik    # restarts once with the new values
hubctl create jacob --chef "Jacob"
```

`init` is idempotent and creates: the `chefs-system` and `chefs-hubs` namespaces; the
default-deny `NetworkPolicy` set in `chefs-hubs`; Traefik's `HelmChartConfig` (trusts
Cloudflare's ranges for the visitor IP, redirects http to https, access logs off because the
magic-link token travels in a query string), the `headers` middleware and the default
`TLSStore`; the backup `CronJob` with its `ServiceAccount` and `Role`. `create` prints the
owner key **once**, then a sign-in link; store the key in a password manager and send the
link to the owner. Check the hub from outside:

```sh
curl https://jacob.chefslegacy.com/api/v1/health
curl -sS -D - -o /dev/null https://jacob.chefslegacy.com/mcp \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

The second call should answer `200` with `Content-Type: text/event-stream` and an
`Mcp-Session-Id` header: MCP streams through Cloudflare and Traefik.

## 4. Day to day

- `hubctl list` and `hubctl status` show every hub, its URL and pod state;
  `kubectl -n chefs-hubs logs hub-jacob-0` shows a hub's log.
- `hubctl login-link jacob` mints a fresh 10-minute link when an owner is locked out;
  `hubctl rotate-key jacob` replaces the owner key (all sessions and MCP clients must sign in
  again) and prints the new one once.
- Upgrades: set `HUB_IMAGE` in `.env` to the new tag and run `hubctl upgrade all`; hubs roll
  one at a time and the command waits for each readiness probe. Newly created hubs always use
  the current `HUB_IMAGE`.
- `hubctl remove jacob` deletes the workload but keeps the volume `data-hub-jacob-0`
  (`hubctl create jacob` later picks it up again, with a new key); `--purge` deletes the data.
- `hubctl render jacob` prints exactly what `create` would apply; `hubctl render all` the
  cluster objects; `deploy/k8s/` holds a generated reference copy for review.

## 5. Backups and restore

Create an R2 bucket (for example `chefslegacy-backups`) and an R2 API token with *Object
Read & Write* on that bucket, fill `R2_BUCKET`, `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID` and
`R2_SECRET_ACCESS_KEY` in `.env` and run `hubctl init` again: it stores them in the Secret
`backup-r2` and the nightly `CronJob` (`BACKUP_SCHEDULE`, default 03:17 UTC) starts
uploading `hubs/<name>/<name>-<timestamp>.zip`. Add a lifecycle rule on the bucket that
deletes objects after 30 days. `hubctl backup all` (or one hub) runs the job immediately
and prints its log, which is also the way to check credentials.

The job reads each hub's owner key from its Secret and calls `GET /api/v1/export` on the
hub's cluster address; until the hub image includes that endpoint (story RN-30) the log says
`export endpoint not available yet (RN-30); skipped`. Without `R2_*` the job still runs and
reports `R2 not configured`.

Restore replaces a hub's data with an archive that contains `recipes.db` (and `images/`) at
its top level, which is what the export produces:

```sh
hubctl restore jacob jacob-20261027T031700Z.zip
```

It scales the hub to zero, unpacks the archive into the volume through a short-lived helper
pod, fixes ownership for uid 65532 and scales back up. The hub starts with the restored
data (SQLite migrations run if the archive is older than the image).

## 6. Local dry run with k3d

Everything above can be rehearsed on a laptop with Docker and [k3d](https://k3d.io):

```sh
docker build -t chefsprodigy:dev .
k3d cluster create chefs -p "8081:80@loadbalancer"
k3d image import chefsprodigy:dev -c chefs
export KUBECONFIG=$(k3d kubeconfig write chefs)
mkdir -p /tmp/chefs && printf 'DOMAIN=example.test\nHUB_IMAGE=chefsprodigy:dev\nTLS_MODE=off\n' > /tmp/chefs/.env
deploy/hubctl --root /tmp/chefs init
kubectl -n kube-system rollout status deploy/traefik
deploy/hubctl --root /tmp/chefs create demo
curl -H 'Host: demo.example.test' http://localhost:8081/api/v1/health
k3d cluster delete chefs
```

`TLS_MODE=off` routes hubs on Traefik's plain `web` entrypoint and gives them `http://`
public URLs; the Host header stands in for DNS. Import images one per `k3d image import`
call: a multi-image import can fail with `ctr: content digest … not found` while k3d still
reports success, and the hub then sits in `ImagePullBackOff`. The same setup verified, for RN-65, that a
hub pod cannot reach another hub, the `chefs-system` namespace or the Kubernetes API while
DNS and public addresses work, that `role=hub-client` pods in `chefs-system` can reach hubs,
that `/mcp` streams through Traefik, and that backup, restore, rotate-key, upgrade and
remove behave as described.

## 7. What a hub can and cannot do

- Runs as uid 65532 with a read-only root filesystem, no capabilities, the default seccomp
  profile, no service-account token, 50m/32Mi requested and 500m/128Mi limits,
  `TOKIO_WORKER_THREADS=2`, `/data` on its own volume and `/tmp` on an emptyDir.
- Accepts connections only from Traefik and from `role=hub-client` pods in `chefs-system`
  (the backup job, later the platform poller).
- Opens connections only to cluster DNS, to public addresses (private ranges, the node and
  the Kubernetes API are excluded) and to `llm-gateway` in `chefs-system`. The hub's own
  SSRF guard refuses private destinations as a second line.
- Its Secret holds the owner key and model settings; `hubctl` prints keys only when they are
  created and never logs them.

## 8. Troubleshooting

| Symptom | Look at |
|---------|---------|
| `create` reports *not ready yet* | `kubectl -n chefs-hubs describe pod hub-<name>-0`, then `logs`; a missing image tag or a volume that cannot be provisioned show up there |
| 404 from Traefik for a hostname | `kubectl -n chefs-hubs get ingress`; the host must be `<name>.<DOMAIN>` and DNS must point at the node |
| Certificate errors behind Cloudflare | Secret `chefs-origin-tls` in `kube-system`, Cloudflare mode *Full (strict)*, `kubectl -n kube-system get tlsstore default` |
| Traefik ignores the Cloudflare ranges | `kubectl -n kube-system get deploy traefik -o jsonpath='{.spec.template.spec.containers[0].args}'` should list them; `kubectl -n kube-system logs job/helm-install-traefik` shows chart errors |
| Backup job fails | `hubctl backup all` prints the log; `kubectl -n chefs-system get jobs` lists past runs |
| Hub cannot call the model | `kubectl -n chefs-hubs logs hub-<name>-0`; egress to public addresses is allowed, private ones are not |
