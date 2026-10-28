# Hosting hubs on one k3s node

`deploy/` runs many Chef's Prodigy hubs on a single VPS: k3s (Kubernetes with Traefik,
CoreDNS, local-path storage and network policies built in), Cloudflare in front for DNS,
TLS and the WAF, one `StatefulSet` per hub, the `llm-gateway` that holds the model vendor
keys and gives each hub its own token, the `platform` that indexes every hub's public
recipes for one search on the apex, nightly backups to R2 and a Python tool,
`hubctl`, that drives `kubectl`. Design and rationale: [docs/SPEC-hosted-platform.md](../docs/SPEC-hosted-platform.md).

```
hubctl init                 namespaces, network policies, Traefik config, backup CronJob, llm-gateway, platform
hubctl create jacob         Secret + StatefulSet + Service + Ingress + 2Gi volume; prints the owner key once
hubctl list | status        hubs, URLs, pod state
hubctl login-link jacob     10-minute magic sign-in link for the owner
hubctl rotate-key jacob     new owner key, hub restarted
hubctl gateway-token jacob  new llm-gateway token for the hub, hub restarted
hubctl usage [jacob] [--from D --to D]  model requests and tokens per hub and day
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
`TLSStore`; the backup `CronJob` with its `ServiceAccount` and `Role`; and, when `.env` has a
vendor key, the `llm-gateway` (next paragraph). `create` prints the
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

**Search on the apex.** `init` also deploys the `platform` (`/usr/local/bin/platform serve`
from the hub image, 128Mi limit, `platform.db` on a 1Gi volume) with an `Ingress` for the
bare `DOMAIN` that routes only `/api/v1/search`, `/api/v1/hubs` and `/health`; `/admin` never
leaves the cluster, and stays off entirely unless `.env` sets `PLATFORM_ADMIN_TOKEN`. `create`
registers the hub through `kubectl exec deploy/platform -- platform add-hub <name>
--internal-url http://hub-<name>.chefs-hubs.svc:8080 --public-url <public URL> --chef <chef>`
(idempotent; running `create` again updates the entry) and `remove` runs `platform remove-hub`,
which drops the hub's index rows. The platform pods carry `role=hub-client`, so the hub
`NetworkPolicy` lets the poller in. Every 10 minutes (plus jitter) it reads each hub's
federation manifest and fetches only recipes whose `updated_at`/`content_hash` or translation
timestamps changed; unpublished recipes and removed translations leave the index on the next
poll, and a failing hub is retried with exponential backoff. To index right away and check:

```sh
kubectl -n chefs-system exec deploy/platform -- platform poll          # one pass now, one line per hub
kubectl -n chefs-system exec deploy/platform -- platform hubs          # last poll, last error, recipe count
curl 'https://chefslegacy.com/api/v1/search?q=soup&lang=ru'            # hits with matched_lang and also_at
```

`platform.db` is rebuilt from the hubs and is not in the nightly backup; after losing it,
`hubctl create <name>` for each hub registers it again and the next poll refills the index.
`upgrade all` moves the platform to the new `HUB_IMAGE` after the gateway, before the hubs.

**Model access.** The vendor keys from `.env` (`MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`) go only
into the gateway's Secret `llm-gateway-env` in `chefs-system`. The gateway is a `Deployment`
of the hub image (`/usr/local/bin/llm-gateway serve`, 64Mi limit) with `gateway.db` on its own
1Gi volume; only hub pods may connect to it. `hubctl create` registers each hub through
`kubectl exec deploy/llm-gateway -- llm-gateway add-hub <name>` and writes the returned token
into the hub's Secret as `MINIMAX_API_KEY` and `ANTHROPIC_API_KEY`, with
`MINIMAX_ANTHROPIC_API_BASE` and `ANTHROPIC_BASE_URL` set to
`http://llm-gateway.chefs-system.svc:8080/anthropic`. The hub binary is unchanged: it simply
talks to a different base URL with a different key. The gateway checks the token, swaps in
the vendor key and records requests and tokens per hub and day:

```sh
hubctl usage                       # every hub, newest day first, with a total line
hubctl usage jacob --from 2026-11-01
```

Running `hubctl create` again on a hub that was created with direct vendor access moves it
onto the gateway. `LLM_GATEWAY=off` (or no vendor key) skips the gateway and copies the
model settings into each hub as before. Quotas, prices, BYOK and the reply cache are
covered next.

### Quotas, prices, BYOK and the reply cache

Every hub starts on the free tier (spec section 11): 30 model requests per UTC day during
its first 3 days at the gateway, then 10 per day, and at most 150 requests or 100 cents of
estimated cost per calendar month. Over quota the gateway answers `429` with
`{"type":"error","error":{"type":"quota_exceeded","message":…}}` before it calls the vendor,
and the hub shows that the model quota is used up (RN-67). Daily limits reset at 00:00 UTC,
monthly ones on the 1st; only vendor calls count (`hubctl usage`), cache hits are free.

The limits are per-hub settings in the gateway's admin API. Put `GATEWAY_ADMIN_TOKEN` (and,
for BYOK, `GATEWAY_MASTER_KEY`) in `.env` and run `hubctl init`, which restarts the gateway
with the new Secret. The admin API has no Ingress; reach it through a port-forward, which
goes through the API server and the kubelet, so the policy that admits only hub pods does not
apply:

```sh
kubectl -n chefs-system port-forward deploy/llm-gateway 8090:8080 &
TOKEN=$(sed -n 's/^GATEWAY_ADMIN_TOKEN=//p' /srv/chefslegacy/.env)
gw() { curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; echo; }
gw http://127.0.0.1:8090/admin/hubs                    # quota, status and byok_vendors per hub
gw -X PATCH http://127.0.0.1:8090/admin/hubs/jacob -d '{"daily_request_limit": 50, "monthly_request_limit": 1000}'
gw -X PATCH http://127.0.0.1:8090/admin/hubs/jacob -d '{"onboarding_days": 0}'     # end the onboarding tier
gw -X PATCH http://127.0.0.1:8090/admin/hubs/jacob -d '{"status": "disabled"}'     # "active" restores the same token
kill %1                                                # close the port-forward
```

`PATCH` takes any of `onboarding_daily_requests`, `onboarding_days`, `daily_request_limit`,
`daily_token_limit` (input plus output tokens a day, the vendor's prompt-cache tokens
included; `0` removes this cap), `monthly_request_limit`, `monthly_cost_cap_cents` and
`status`; unknown fields and negative numbers are refused, and the answer shows the hub as
stored. During the onboarding days `onboarding_daily_requests` applies instead of
`daily_request_limit`. Any other limit set to `0` blocks the hub's model calls. `hubctl remove` and
`hubctl gateway-token` keep the limits and the onboarding start; `DELETE
/admin/hubs/<name>` forgets the hub and its usage.

**Prices.** The monthly cost cap needs a price, in cents per million tokens, for each model
name the hubs send (`MiniMax-M3` for `LLM_MODEL=minimax/MiniMax-M3`). Without one a call is
estimated at 0 cents and only the request limits apply. The figures below are placeholders;
take them from the vendor's price list. Prompt-cache reads are priced at the full input rate,
so the estimate is an upper bound.

```sh
gw -X PUT http://127.0.0.1:8090/admin/prices \
  -d '{"vendor": "minimax", "model": "MiniMax-M3", "input_cents_per_mtok": 30, "output_cents_per_mtok": 120}'
gw http://127.0.0.1:8090/admin/prices
```

**Bring your own key.** With `GATEWAY_MASTER_KEY` set, a hub owner's own vendor key can be
stored for that hub; the hub then calls the vendor with it, skips the platform limits and
still has its usage recorded. The key is encrypted (XChaCha20-Poly1305) in `gateway.db` and
never shown again; the listing names only the vendors. Read it without echoing it so it stays
out of the shell history:

```sh
read -rs OWNER_KEY
gw -X POST http://127.0.0.1:8090/admin/hubs/jacob/byok -d "{\"vendor\": \"minimax\", \"key\": \"$OWNER_KEY\"}"
gw -X DELETE http://127.0.0.1:8090/admin/hubs/jacob/byok/minimax       # back to the platform key and quota
```

Keys stored under an earlier `GATEWAY_MASTER_KEY` no longer decrypt: those hubs fall back to
the platform key and quota (the gateway log says so), and storing a new key answers `409`
until the old ones are removed with the `DELETE` above.

**Reply cache.** Requests with `temperature` 0 (the hubs send every import, translation and
label request that way) are answered from the cache when vendor, model and request content
match, across hubs and regardless of JSON key order. Only successful replies up to 1 MB are
kept, for `GATEWAY_CACHE_TTL_DAYS` (30), at most `GATEWAY_CACHE_MAX_ENTRIES` (10000) of them,
the least recently hit evicted first. Every reply carries `x-gateway-cache: hit`, `miss` or
`bypass`; `gw http://127.0.0.1:8090/admin/cache/stats` shows entries, bytes and hits. The
cache lives in `gateway.db` on the gateway's volume.

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
  The hub's gateway token is disabled either way; a later `create` issues a new one.
- `hubctl gateway-token jacob` replaces a hub's gateway token (a leaked token, or a gateway
  whose `gateway.db` was lost) and restarts the hub. `upgrade all` also moves the gateway to
  the new `HUB_IMAGE`, before the hubs.
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

The gateway's `gateway.db` (hub token hashes and usage counters) is not part of the nightly
backup. Losing it costs the usage history; `hubctl gateway-token <name>` for each hub
restores model access.

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
public URLs; the Host header stands in for DNS. Add a vendor key and `LLM_MODEL` to the
`.env` to rehearse the gateway as well. Import images one per `k3d image import`
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
  (the backup job and the platform poller).
- Opens connections only to cluster DNS, to public addresses (private ranges, the node and
  the Kubernetes API are excluded) and to `llm-gateway` in `chefs-system`. The hub's own
  SSRF guard refuses private destinations as a second line.
- Its Secret holds the owner key, the model name and its own gateway token, never a vendor
  key while the gateway runs; `hubctl` prints owner keys only when they are created and never
  prints gateway tokens or vendor keys.

## 8. Troubleshooting

| Symptom | Look at |
|---------|---------|
| `create` reports *not ready yet* | `kubectl -n chefs-hubs describe pod hub-<name>-0`, then `logs`; a missing image tag or a volume that cannot be provisioned show up there |
| 404 from Traefik for a hostname | `kubectl -n chefs-hubs get ingress`; the host must be `<name>.<DOMAIN>` and DNS must point at the node |
| Certificate errors behind Cloudflare | Secret `chefs-origin-tls` in `kube-system`, Cloudflare mode *Full (strict)*, `kubectl -n kube-system get tlsstore default` |
| Traefik ignores the Cloudflare ranges | `kubectl -n kube-system get deploy traefik -o jsonpath='{.spec.template.spec.containers[0].args}'` should list them; `kubectl -n kube-system logs job/helm-install-traefik` shows chart errors |
| Backup job fails | `hubctl backup all` prints the log; `kubectl -n chefs-system get jobs` lists past runs |
| Hub cannot call the model | `kubectl -n chefs-hubs logs hub-<name>-0`; then `kubectl -n chefs-system logs deploy/llm-gateway`. A 401 there means the hub's token is unknown: `hubctl gateway-token <name>` |
| `create` stops at *llm-gateway is not ready* | `kubectl -n chefs-system describe pod -l app=llm-gateway`; the gateway needs its volume and the hub image |
| A hub reports that its model quota is used up | `hubctl usage <name>` shows today's and this month's requests; raise the hub's limits or store its own key (section 3, *Quotas, prices, BYOK and the reply cache*) |
