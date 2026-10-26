# Deploying hubs on one VPS behind Cloudflare

This directory is the operational half of [`docs/SPEC-hosted-platform.md`](../docs/SPEC-hosted-platform.md):
one VPS runs Caddy and one container per hub, Cloudflare fronts every hostname, and
`hubctl` creates, backs up, restores and upgrades hubs. Phase 0 of the rollout (section 9):
hubs talk to the model vendor directly; the llm-gateway, platform and signup come later.

```
deploy/
  hubctl                 Python 3 (standard library) CLI, run as root on the VPS
  compose.yml            Caddy (caddy-docker-proxy) and the two networks
  templates/Caddyfile.tmpl   rendered into <root>/caddy/Caddyfile by hubctl
  env.example            the platform-level .env
  backup.sh              cron wrapper around `hubctl backup all`
  tests/                 python3 -I -m unittest discover -s deploy/tests
```

Everything hubctl manages lives under `/srv/chefslegacy` (override with `--root` or
`HUBCTL_ROOT`); see the layout at the top of `hubctl`.

## 1. Prerequisites

- A VPS with Debian 12 or Ubuntu 24.04, 2 GB RAM or more (a 4 GB box carries 20 to 30
  hubs), a public IPv4 address, and root access over SSH.
- Docker Engine with the Compose plugin: follow https://docs.docker.com/engine/install/.
  Check with `docker compose version` (v2.20 or newer).
- A domain on Cloudflare (free plan is enough) and, for backups, a Cloudflare R2 bucket.
- `rclone` on the VPS for backups: `apt install rclone`.
- A published hub image. Tags `v*` of this repository publish
  `ghcr.io/watchcat/chefsprodigy:<version>` through `.github/workflows/docker.yml`.

## 2. Cloudflare

1. **DNS**: two proxied (orange cloud) A records pointing at the VPS: the apex
   (`chefslegacy.com`) and a wildcard (`*`). Every hub becomes `<name>.chefslegacy.com`
   without further DNS work.
2. **SSL/TLS**: encryption mode **Full (strict)**; turn on *Always Use HTTPS*.
3. **Origin CA certificate**: SSL/TLS → Origin Server → *Create Certificate*, hostnames
   `chefslegacy.com` and `*.chefslegacy.com`, 15 years. Save the certificate as
   `/srv/chefslegacy/caddy/certs/origin.pem` and the private key as `origin.key`
   (`chmod 600 origin.key`). Caddy serves this certificate; browsers see Cloudflare's.
4. **Rate limiting** (Security → WAF → Rate limiting rules): one rule for
   `(http.request.uri.path contains "/api/v1/")` on anonymous traffic, for example 60
   requests per 10 seconds per IP; a bot-fight rule on the apex later for the search page.
5. Note the upload cap: the free plan accepts request bodies up to 100 MB. Large Mealie
   archives can go through `hubctl restore` instead (section 8).

## 3. Firewall

Only Cloudflare may reach the web ports; SSH only from your own addresses.

```sh
apt install -y ufw fail2ban
ufw default deny incoming
ufw default allow outgoing
ufw allow from <your.ip.address> to any port 22 proto tcp
for net in $(curl -s https://www.cloudflare.com/ips-v4) $(curl -s https://www.cloudflare.com/ips-v6); do
  ufw allow from "$net" to any port 443 proto tcp
  ufw allow from "$net" to any port 443 proto udp
done
ufw enable
```

Port 80 stays closed: *Always Use HTTPS* redirects at the edge. Docker publishes 80 and
443 on the host, but ufw's rules apply before Docker's forwarding for inbound traffic only
when `DOCKER-USER` rules are in place; the simplest safe setup is to also bind Caddy's
ports to the VPS's public address only (`HTTP_PORT=203.0.113.10:80` works, Compose accepts
`ip:port` on the left side) and to keep the Cloudflare rules above.

## 4. Install

```sh
mkdir -p /srv/chefslegacy && cd /srv/chefslegacy
git clone https://github.com/watchcat/chefsprodigy.git src
ln -s src/deploy deploy
cp deploy/env.example .env && chmod 600 .env
# edit .env: DOMAIN, HUB_IMAGE tag, R2_REMOTE, LLM_MODEL and the vendor key
mkdir -p caddy/certs            # put origin.pem and origin.key here (section 2)
deploy/hubctl init              # directories, Caddyfile, `docker compose up -d caddy`
```

`hubctl init` is idempotent; run it again after changing `.env`. It copies `compose.yml`
next to the generated `compose.hubs.yml`, renders `caddy/Caddyfile` (Cloudflare's ranges
as trusted proxies, `CF-Connecting-IP` as the client IP, 110 MB request bodies, access logs
with the magic-link `token` parameter removed) and starts Caddy.

## 5. Create a hub

```sh
deploy/hubctl create jacob --chef "Jacob"
```

This writes `hubs/jacob/.env` (mode 0600: the owner key `NODE_API_KEY`, `NODE_CHEF_NAME`,
`NODE_PUBLIC_URL=https://jacob.chefslegacy.com`, and the model settings copied from
`.env`), creates `hubs/jacob/data/` owned by uid 65532 (the image's user), regenerates
`compose.hubs.yml` with one service per hub, starts `hub-jacob` and waits for its health
check. Caddy picks up the new route from the container labels within a few seconds.

The command prints the **owner key once**; it is stored only in `hubs/jacob/.env`. It also
prints a sign-in link (hubs accept it once RN-73 ships; until then the owner signs in with
the key at `https://jacob.chefslegacy.com`).

Check:

```sh
deploy/hubctl status
curl -s https://jacob.chefslegacy.com/api/v1/health
```

MCP clients use `https://jacob.chefslegacy.com/mcp` with `Authorization: Bearer <owner key>`.
The hub answers Streamable HTTP with `Cache-Control: no-cache` and `X-Accel-Buffering: no`,
Caddy proxies hub routes with `flush_interval -1`, so the SSE stream passes Cloudflare
unbuffered. Verify once on the first hosted hub:

```sh
curl -sN -D - -o /dev/null https://jacob.chefslegacy.com/mcp \
  -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}'
```

Expect `content-type: text/event-stream` within a second. If Cloudflare ever buffers,
add a DNS-only `mcp.<name>` hostname (spec section 11).

Hub names are lowercase letters, digits and hyphens, 1 to 63 characters; `www`, `api`,
`mail`, `admin`, `gateway`, `platform`, `provisioner` and `caddy` are reserved.

## 6. Day-to-day

| Task | Command |
|---|---|
| Health of Caddy and every hub | `hubctl status` |
| Hubs and their URLs | `hubctl list` |
| New owner key (invalidates sessions and MCP clients) | `hubctl rotate-key jacob` |
| Sign-in link for the owner (10 minutes, single use) | `hubctl login-link jacob` |
| Roll hubs to the tag in `HUB_IMAGE` | edit `.env`, then `hubctl upgrade all` |
| Stop a hub, keep its data under `hubs/.removed/` | `hubctl remove jacob` |
| Delete the kept data | `hubctl remove jacob --purge` |
| Logs | `docker logs hub-jacob`, `docker logs caddy` |

`upgrade` pulls the image once, then restarts hubs one by one, waiting for each health
check; migrations run at startup.

## 7. Backups to R2

Create an R2 bucket (for example `chefslegacy-backups`) with a lifecycle rule that deletes
objects after 30 days, and an R2 API token with object read/write. Configure rclone on the
VPS:

```sh
rclone config create r2 s3 provider=Cloudflare access_key_id=<id> secret_access_key=<secret> \
  endpoint=https://<account id>.r2.cloudflarestorage.com acl=private
rclone lsd r2:chefslegacy-backups
```

`R2_REMOTE=r2:chefslegacy-backups` in `.env` matches that remote. Then:

```sh
deploy/hubctl backup jacob          # one hub
deploy/hubctl backup all            # every hub, plus gateway.db and platform.db when present
deploy/hubctl backup jacob --local  # keep the archive under backups/ instead of uploading
```

A hub backup is a zip with `recipes.db` (a consistent copy taken with `VACUUM INTO` while
the hub keeps running) and `images/`, uploaded to `hubs/<name>/<name>-<UTC stamp>.zip`.
Nightly job as root:

```
17 3 * * * /srv/chefslegacy/deploy/backup.sh >> /var/log/chefs-backup.log 2>&1
```

## 8. Restore

```sh
rclone copy r2:chefslegacy-backups/hubs/jacob/jacob-20261026T031700Z.zip /tmp/
deploy/hubctl restore jacob /tmp/jacob-20261026T031700Z.zip
```

The hub is stopped, its current `data/` is renamed to `data.pre-restore-<stamp>` (delete it
once you are happy), the archive's `recipes.db` and `images/` are unpacked, ownership is set
to uid 65532 and the hub starts again. The same path moves a hub to a new box and loads a
Mealie import that is too large for Cloudflare's upload cap: import locally, back up, restore.

## 9. Local dry run

The whole stack runs on a laptop with Docker Desktop, without Cloudflare or certificates:

```sh
mkdir -p /tmp/chefs && printf 'DOMAIN=localhost\nTLS_MODE=off\nHTTP_PORT=8081\nHTTPS_PORT=8443\nHUB_IMAGE=chefsprodigy:latest\n' > /tmp/chefs/.env
docker build -t chefsprodigy .            # or set HUB_IMAGE to a published tag
deploy/hubctl --root /tmp/chefs create demo --chef Demo
curl -s -H 'Host: demo.localhost' http://127.0.0.1:8081/api/v1/health
deploy/hubctl --root /tmp/chefs remove demo --purge
docker compose --project-directory /tmp/chefs -f /tmp/chefs/compose.yml down
```

`TLS_MODE=off` renders `http://` site addresses and `auto_https off`; never use it on a
public host.

## 10. LLM gateway (phase 1)

The gateway (`crates/llm-gateway`, same image, `entrypoint: llm-gateway`) keeps the vendor
keys out of the hubs: each hub gets its own token, every call is recorded per hub and day,
and quotas and a reply cache follow in RN-68. Enable it by setting `GATEWAY_ADMIN_TOKEN` in
`.env` (`openssl rand -hex 32`) and running `hubctl init`. From then on `hubctl create`
registers each new hub at the gateway and writes the hub token and
`MINIMAX_ANTHROPIC_API_BASE=http://llm-gateway:8080/anthropic` into the hub's `.env`; the
hub's model client needs no change. Hubs created before that keep calling the vendor
directly until you switch them:

```sh
TOKEN=$(curl -s -X POST http://127.0.0.1:8090/admin/hubs \
  -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN" -H 'content-type: application/json' \
  -d '{"name":"jacob"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')
# in hubs/jacob/.env: MINIMAX_API_KEY=$TOKEN and MINIMAX_ANTHROPIC_API_BASE=http://llm-gateway:8080/anthropic
docker compose -f compose.yml -f compose.hubs.yml up -d --force-recreate hub-jacob
```

Usage per hub and day (requests, tokens, estimated cost once `model_prices` has rows):

```sh
curl -s "http://127.0.0.1:8090/admin/usage?hub=jacob&from=2026-10-01" -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN"
```

Without an admin token the gateway still runs (passthrough only) and `docker exec
llm-gateway /usr/local/bin/llm-gateway add-hub <name>` issues tokens on the command line.
`gateway/data/gateway.db` is included in `hubctl backup all`.

**Quotas.** Every hub starts on the free tier (spec section 11): 30 model requests per UTC
day during its first 3 days, then 10 per day, and at most 150 requests or 100 cents of
estimated cost per calendar month. Over quota the gateway answers `429 quota_exceeded`
before calling the vendor, and the hub shows "model quota exceeded" (RN-67). Raise a hub's
limits or pause it:

```sh
curl -s -X PATCH http://127.0.0.1:8090/admin/hubs/jacob -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'content-type: application/json' -d '{"daily_request_limit": 50, "monthly_request_limit": 1000}'
curl -s -X PATCH http://127.0.0.1:8090/admin/hubs/jacob -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'content-type: application/json' -d '{"status": "disabled"}'      # or "active"
curl -s -X DELETE http://127.0.0.1:8090/admin/hubs/jacob -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN"
```

Cost estimates need prices: insert rows into `model_prices` (cents per million tokens),
for example `INSERT INTO model_prices VALUES ('minimax', 'MiniMax-M3', 30, 120);` with
`sqlite3 gateway/data/gateway.db`; without prices the cost cap never triggers.

**Bring your own key.** With `GATEWAY_MASTER_KEY` set, a hub owner's own vendor key can be
stored encrypted; that hub then bypasses the platform quota (usage is still recorded):

```sh
curl -s -X POST http://127.0.0.1:8090/admin/hubs/jacob/byok -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN" \
  -H 'content-type: application/json' -d '{"vendor": "minimax", "key": "<the owner's key>"}'
curl -s -X DELETE http://127.0.0.1:8090/admin/hubs/jacob/byok/minimax -H "authorization: Bearer $GATEWAY_ADMIN_TOKEN"
```

**Cache.** Deterministic requests (`temperature` 0, no streaming) are cached for 30 days by
a hash of their canonical JSON, so two hubs normalizing the same ingredients share one
vendor call. `GET /admin/cache/stats` reports entries, bytes and hits;
`GATEWAY_CACHE_MAX_ENTRIES` (default 10000) bounds the table.

## 11. Troubleshooting

- `hubctl status` shows `unhealthy`: `docker logs hub-<name>`; the health check is the
  binary's own `node-server healthcheck`.
- 502 from Caddy for a hub: the container is starting or unhealthy; `docker logs caddy`
  shows the generated config and reload errors.
- Writes fail inside a hub: `hubs/<name>/data` must be owned by uid 65532 (`hubctl create`
  does this when run as root).
- Uploads over 100 MB fail at Cloudflare: use `hubctl restore` or a temporary DNS-only
  hostname.
- The hub cannot reach a URL you can open yourself: the SSRF guard refuses private, loopback
  and link-local destinations by design.
