# Spec: Hosted platform on one k3s node (hubs, model gateway, aggregator)

Status: proposed, 2026-10-26; revised 2026-10-27 for k3s + Traefik + `kube-rs` (replacing the
Docker Compose / Caddy design, whose first implementation was rolled back on the same day).
Companion to `docs/ARCHITECTURE_AND_SCHEMAS.md` (the node), `docs/MCP_AND_API_SPEC.md` (its
API) and `docs/SPEC-i18n.md`. Work items are tracked on the board in `docs/KANBAN.md` as
RN-63 and up.

## 1. Objective

Run many recipe hubs for many people under `chefslegacy.com`, one hub per subdomain
(`jacob.chefslegacy.com`), with a central search over everything public on the apex, on a
single VPS running **k3s**. Each hub stays the small single binary that exists today, packed
in one container image. Everything that is shared, expensive or sensitive moves into two
platform services, plus one small privileged one:

- **llm-gateway**: the only process that talks to model vendors. Holds vendor keys, enforces
  per-hub quotas, caches deterministic replies, records usage.
- **platform**: the apex site. Hub registry, aggregator (federation poller and search index),
  signup, transactional mail outbox, admin.
- **provisioner** (phase 3): a `kube-rs` service that creates and removes hub workloads through
  the Kubernetes API with a narrowly scoped `ServiceAccount`; nothing in the system touches a
  container runtime socket.

Rate limiting lives in two places and nowhere else: model spend at the gateway, HTTP abuse
at the edge (Cloudflare rules and Traefik middlewares), so the per-node limiter (RN-28)
becomes optional.

## 2. Decisions taken as defaults

| Topic | Decision | Alternative kept open |
|---|---|---|
| Hosting | One VPS (4 GB, 2 vCPU class), Ubuntu LTS, single-node k3s (embedded containerd, Traefik, local-path storage, ServiceLB) | Fly.io machines with the same image; a second k3s node when one is full |
| Edge | Cloudflare proxied (orange cloud) for apex and `*.chefslegacy.com`; Origin CA wildcard cert as Traefik's default certificate; VPS firewall allows 443 only from Cloudflare IPs | DNS-only with cert-manager and ACME DNS-01 |
| Routing | k3s's bundled Traefik; one `Ingress` per hub and one for the apex; Traefik middlewares for body limits and headers | ingress-nginx |
| Isolation | Namespaces `chefs-system` (gateway, platform, provisioner, backups) and `chefs-hubs` (one `StatefulSet` per hub); default-deny `NetworkPolicy` in `chefs-hubs`, hubs reach only DNS, the public internet and the gateway | |
| Hub image | `ghcr.io/watchcat/chefsprodigy:<version>`, distroless, non-root, `/data` volume, no shell tools; health through kubelet `httpGet` probes plus `node-server healthcheck` for `docker run` and debugging | |
| Model access | Hubs call llm-gateway only; gateway is a Rust crate in this workspace | LiteLLM proxy, Cloudflare AI Gateway |
| Owner login | `NODE_API_KEY` stays the permanent master secret (API, MCP, direct login); short-lived signed magic links sign the owner in on any device without rotating it | |
| Email | Transactional provider (Resend first) behind an outbox in `platform` | Postmark, SES |
| Backups | Nightly `CronJob` in `chefs-system`: per-hub export to Cloudflare R2 via rclone, 30-day retention by bucket lifecycle rule | restic; Longhorn snapshots on a multi-node cluster |
| Provisioning | `hubctl` CLI (Python, `kubectl`-based) for phases 0–2; platform-driven signup through the `kube-rs` provisioner in phase 3 | |
| Model cost | Platform key with per-hub free-tier quotas (§5.2); owners may add their own key (BYOK in the gateway) | |

## 3. Architecture

```
                Internet
                   │
        Cloudflare: DNS, wildcard TLS, WAF, rate-limit rules, hides VPS IP
                   │  (firewall: only Cloudflare ranges may reach :443)
   ────────────────┼──────────────────────────── VPS: single-node k3s ──────────
                   ▼
   kube-system     Traefik (ServiceLB on :443; default cert = Origin CA wildcard Secret;
                   trusts Cloudflare ranges for the client IP; routes by Ingress host)
                     chefslegacy.com        → platform.chefs-system:8080
                     jacob.chefslegacy.com  → hub-jacob.chefs-hubs:8080
                     maria.chefslegacy.com  → hub-maria.chefs-hubs:8080

   chefs-system    ┌──────────────┬──────────────┬────────────────┬──────────────────┐
                   │ llm-gateway  │ platform     │ provisioner    │ backup CronJob   │
                   │ gateway.db   │ platform.db  │ (kube-rs, SA)  │ (rclone → R2)    │
                   │ PVC          │ PVC          │                │                  │
                   └──────┬───────┴──────┬───────┴───────┬────────┴────────┬─────────┘
                          │ vendor key   │ polls         │ creates/removes │ reads hub
                          │ MiniMax…     │ manifests     │ hub workloads   │ Secrets, calls
                          │              │ sends mail    │ via the K8s API │ /api/v1/export
   chefs-hubs      ┌──────┴───────┐ ┌────┴─────────┐     │                 │
                   │ hub-jacob    │ │ hub-maria    │ ◄───┘                 │
                   │ StatefulSet  │ │ StatefulSet  │                       │
                   │ Secret, Svc, │ │ Secret, Svc, │ ◄─────────────────────┘
                   │ Ingress, PVC │ │ Ingress, PVC │
                   └──────────────┘ └──────────────┘
                   NetworkPolicy: ingress only from Traefik and platform; egress only to
                   DNS, public addresses and llm-gateway. No route to platform, provisioner,
                   other hubs or the Kubernetes API.
```

Hubs live in `chefs-hubs`, one `StatefulSet` each with a `Secret` (owner key, gateway token,
settings), a `Service`, an `Ingress` and a `PersistentVolumeClaim` for `/data`. The gateway,
the platform, the provisioner and the backup `CronJob` live in `chefs-system`. The
`NetworkPolicy` set in `chefs-hubs` is default-deny: a hub accepts traffic from Traefik and
from the platform's poller, and may open connections to DNS, to public addresses and to
`llm-gateway.chefs-system`. The SSRF guard in the hub (§6) is the second line: even an
allowed egress path refuses private destinations named by a user.

Cluster layout:

```
deploy/k8s/                      reference manifests generated by `hubctl render --reference`
  namespaces.yaml                chefs-system, chefs-hubs
  traefik-config.yaml            HelmChartConfig for k3s's Traefik (§5.5), headers Middleware, default TLSStore
  network-policies.yaml          default deny in chefs-hubs + the allowed paths above
  backup-cronjob.yaml            nightly export → R2 (§4.7): ServiceAccount, Role, scripts, CronJob
  hub.example.yaml               StatefulSet + Secret + Service + Ingress for one hub (`hubctl render <name>`)
  llm-gateway.yaml               Deployment, Service, PVC, Secret refs (phase 1)
  platform.yaml                  Deployment, Service, PVC, apex Ingress (phase 2)
  provisioner.yaml               Deployment, Service, ServiceAccount, Role, RoleBinding (phase 3)
deploy/hubctl                    Python, standard library, drives kubectl (§5.6); renders the
                                 manifests above, and deploy/tests fails when the copies drift
deploy/README.md                 how-to: k3s install, Cloudflare, Origin CA Secret, first hub, k3d dry run
```

On the node: `/var/lib/rancher/k3s/storage/` holds the local-path PVCs
(`pvc-…_chefs-hubs_data-hub-jacob-0`, …), `/etc/rancher/k3s/` the kubeconfig.

## 4. Data flows

### 4.1 Visitor reads a recipe
Browser → Cloudflare → Traefik (Host = `jacob.chefslegacy.com`) → `hub-jacob` → SQLite → JSON.
The hub applies the owner/visitor rules from RN-61: visitors see public recipes only and
cached translations only. No platform service is involved.

### 4.2 Owner imports a recipe (URL, text, photos)
1. Browser (session cookie) → hub. JSON-LD pages are parsed locally; no model call. Every
   user-supplied URL (page and images) passes the SSRF guard (§6) before it is fetched.
2. For text, photos or ingredient normalization the hub sends a Messages-format request to
   `http://llm-gateway.chefs-system.svc:8080/anthropic/v1/messages` with its **hub token** as
   `x-api-key`.
3. Gateway: authenticate token → check daily/monthly quota → cache lookup → forward to the
   vendor with the real key → record usage (tokens, estimated cost) → return the vendor's
   reply unchanged. 429 with `{"error":{"type":"quota_exceeded"}}` when over quota.
4. Hub stores the recipe and cover in `/data` and answers the browser. A 429 surfaces as
   "model quota exhausted" (new error mapping, RN-67); JSON-LD imports keep working.

### 4.3 Translation and labels
Same path as 4.2. The hub caches per recipe and language in its own SQLite (RN-54/56), so
a hub asks the gateway once per recipe and language.

Cross-hub reuse at the gateway is limited: label requests go out in batches of up to 100
canonical ids and two hubs rarely send the same batch, so a byte-identical request is the
exception. Two measures make the cache useful anyway:

- the gateway hashes a **canonical** form of the request (sorted object keys, no
  insignificant whitespace, hub token excluded; §5.2), so identical content always hashes
  the same regardless of serializer or key order;
- the node ships a **seeded dictionary** of everyday ingredients (`canonical_terms` for
  `en`, `ru`, `nl`, the ~200 most common ids such as `salt`, `garlic`, `onion`, `butter`,
  `egg`, `olive_oil`) through a SQLite migration, so basic terms never cost a model call
  on any hub (RN-75). Seeded rows carry `origin = 'human'` and are never overwritten by
  the model; later additions come as further `INSERT OR IGNORE` migrations.

### 4.4 Federation and search on the apex
1. `platform` holds the registry: `name`, `internal_url`
   (`http://hub-jacob.chefs-hubs.svc:8080`), `public_url` (`https://jacob.chefslegacy.com`),
   owner, status.
2. Every 10 minutes (with jitter) the poller fetches each hub's
   `GET /api/v1/federation/manifest` inside the cluster. Each manifest entry carries
   `id`, `updated_at`, `content_hash` and `translations: [{lang, origin, stale, updated_at}]`
   (RN-64). Translations need their own timestamps because saving one never changes the
   recipe's `updated_at` or `content_hash`. The poller compares `(id, updated_at,
   content_hash)` and every `(lang, updated_at)` pair with the index and fetches changed
   recipes from `GET /api/v1/federation/recipes/{id}`, which returns the recipe together
   with `translations: [RecipeTranslation]` (every stored translation, LLM or human).
   Hubs only list public recipes there.
3. The index stores one row per `(hub, recipe_id)` with title, tags, canonical ids, source
   language, `content_hash`, `updated_at`, cover URL built from `public_url + image_path`,
   plus one row per `(hub, recipe_id, lang)` for each federated translation (title, tags,
   origin, stale flag, `updated_at`). The FTS5 table holds one row per language of each
   recipe, the source language included (§5.3). Rows sharing a `content_hash` are shown as
   one dish with several hubs ("also at …").
4. `GET chefslegacy.com/api/v1/search?q=&ingredient=&lang=` and the search page return hits
   linking to `https://jacob.chefslegacy.com/#/recipe/{id}`. With `lang` the hit carries the
   title and tags of that language when the hub exposes such a translation, otherwise the
   source wording, and says which language matched (`matched_lang`).
5. Optional push (phase 2): a hub notifies `POST platform/api/v1/hubs/{name}/changed` with its
   hub token after a public recipe or translation changes; the platform polls that hub
   immediately. The `NetworkPolicy` for that path is added only if the push is built.

### 4.5 Provisioning a hub (phase 0–2, CLI)
`hubctl create jacob --chef "Jacob"`:
1. Generate `NODE_API_KEY` (32 random bytes, hex).
2. Ask the gateway admin API for a hub token with the default quota.
3. Render the hub manifests (`hubctl create`; `deploy/k8s/hub.example.yaml` shows the shape) for the hub: `Secret hub-jacob-env` (the hub
   settings of §5.1), `StatefulSet hub-jacob` (1 replica, PVC `data`), `Service hub-jacob`,
   `Ingress hub-jacob` for `jacob.chefslegacy.com`; `kubectl apply -n chefs-hubs`; wait for
   the pod to be `Ready`.
4. Register the hub in `platform` (admin API) so the poller picks it up.
5. Print the owner key once and a magic login link (`hubctl login-link jacob`, §4.6); the
   key lives only in the hub's `Secret`.

### 4.6 Owner login and signup

**Magic links (RN-73).** `NODE_API_KEY` remains the permanent master secret for the API,
MCP clients and direct login. For people, the hub additionally accepts a short-lived signed
link so nobody has to type or keep a 64-character key:

- Token: `<expires>.<nonce>.<mac>` where `expires` is a Unix timestamp at most 10 minutes
  ahead, `nonce` is 8 random bytes (hex) and
  `mac = HMAC-SHA256(key = NODE_API_KEY, msg = "chefsprodigy-magic:" + expires + ":" + nonce)`;
  the key is the `NODE_API_KEY` string as configured.
- `GET /api/v1/auth/magic?token=…` on the hub verifies the MAC in constant time, rejects
  expired or already-used tokens (an in-memory set of consumed nonces, pruned at expiry),
  sets the `node_session` cookie exactly as `POST /auth/login` does and redirects to `/#/`
  so the token leaves the address bar.
- Who can mint a link: anything that holds the key. `hubctl login-link <name>` reads the
  hub's `Secret` through `kubectl`; the provisioner offers `POST /hubs/{name}/login-link` to
  the platform (phase 3); a logged-in owner can call `POST /api/v1/auth/magic-link` on their
  hub to sign in on another device. The platform never stores `NODE_API_KEY`.
- A link is sent only to the verified account email, is single use and expires in 10
  minutes, so a leaked mail is a small window. `rotate-key` stays for compromise only;
  rotating still invalidates sessions and MCP clients, which is now the exception rather
  than the recovery path.

**Signup (phase 3).** Visitor on the apex enters email → platform writes a mail to the
outbox → provider sends a verification link → visitor picks a free subdomain → platform calls
the **provisioner** (`kube-rs` service in `chefs-system` whose `ServiceAccount` may manage
`StatefulSets`, `Secrets`, `Services` and `Ingresses` in `chefs-hubs` and nothing else;
exposes only `create/remove/rotate-key/login-link`) → hub pod starts → platform asks the
provisioner for a magic link and emails "your hub is ready, sign in" with it; the owner key
is shown once in the browser for API and MCP use. "Email me a sign-in link" on the apex
repeats the last step for existing owners on any device.

### 4.7 Backup and restore
A `CronJob` in `chefs-system` runs nightly with a `ServiceAccount` that may read `Secrets` in
`chefs-hubs`: for each hub it calls `GET /api/v1/export` (owner key from the hub's `Secret`;
a zip of a consistent SQLite copy via `VACUUM INTO` plus `images/`) and uploads it with
rclone to `r2://chefslegacy-backups/hubs/<name>/<date>.zip`; `gateway.db` and `platform.db`
are exported the same way through their services' export endpoints. R2 credentials come from
a `Secret` in `chefs-system`; an R2 bucket lifecycle rule deletes objects after 30 days.
Restore = `hubctl restore <name> <archive>`: scale the `StatefulSet` to 0, unpack into the
PVC through a short-lived helper pod (`kubectl cp`), scale back to 1. Owners can download
the same export from their hub (RN-30).

### 4.8 Updates
GitHub Actions builds and pushes the image on tags. `hubctl upgrade all` sets the new image
tag on each hub `StatefulSet` one by one and waits for the pod to be `Ready` (migrations run
at startup); the gateway and platform `Deployments` are updated the same way. A rollback is
`kubectl rollout undo`.

## 5. Service contracts

### 5.1 Hub (existing `node-server`), changes needed
- `NODE_PUBLIC_URL` (e.g. `https://jacob.chefslegacy.com`): included in the federation
  manifest as `node_url`; its host is added to the MCP allowlist automatically (RN-64).
- Federation exposes translations (RN-64): manifest entries gain
  `translations: [{lang, origin, stale, updated_at}]`; `GET /api/v1/federation/recipes/{id}`
  returns the recipe plus `translations: [RecipeTranslation]`. Public recipes only, as today.
  This is what lets `platform` index translated titles and tags in `recipes_fts` (§5.3).
- `node-server healthcheck [--port 8080]` subcommand (RN-63): one loopback GET of
  `/api/v1/health` with a 2 s timeout, exit code 0 or 1. The image then needs no `curl`
  or `wget`: `docker run` users get a `HEALTHCHECK` through the binary, and on k3s the
  kubelet probes `/api/v1/health` over HTTP itself (`readinessProbe`/`livenessProbe`
  `httpGet`), with the subcommand available for `kubectl exec` debugging.
- Container spec defaults: `NODE_HOST=0.0.0.0`, `NODE_PORT=8080`, `NODE_DATA_DIR=/data`,
  `TOKIO_WORKER_THREADS=2` (set in the image and repeated in the pod template; Tokio
  otherwise starts one worker per host vCPU in every container).
- Model via gateway, environment only:
  `LLM_MODEL=minimax/MiniMax-M3`,
  `MINIMAX_ANTHROPIC_API_BASE=http://llm-gateway.chefs-system.svc:8080/anthropic`,
  `MINIMAX_API_KEY=<hub token issued by the gateway>`.
  rig already honours these variables; no client code changes.
- Map gateway 429/402 to a clear `model_quota_exceeded` error (HTTP 429 from the hub) so
  the UI can show "quota exhausted" instead of "model failure" (RN-67).
- SSRF guard on every user-supplied URL fetch in `recipe-scraper` (RN-74, §6).
- Magic-link login: `GET /api/v1/auth/magic?token=` and owner-only
  `POST /api/v1/auth/magic-link` (RN-73, §4.6).
- Streaming responses (`/mcp` SSE) carry `Cache-Control: no-cache` and
  `X-Accel-Buffering: no` so Cloudflare does not buffer them; Traefik flushes
  `text/event-stream` responses as they arrive (RN-64).
- `GET /api/v1/export` (owner): zip with `recipes.db` (VACUUM INTO) and `images/`; and
  `POST /api/v1/import/export` to restore into an empty node (RN-30).
- Dockerfile (RN-63): multi-stage build, distroless (`gcr.io/distroless/cc` or `static`)
  runtime, non-root user, `/data` volume, `HEALTHCHECK` through the binary's own
  subcommand, image under 30 MB compressed.

Per-hub `Secret` (`hub-<name>-env` in `chefs-hubs`, consumed with `envFrom`):

```
NODE_API_KEY=…                 owner secret (generated)
NODE_CHEF_NAME=Jacob
NODE_AUTHOR_DID=did:key:…      optional
NODE_PUBLIC_URL=https://jacob.chefslegacy.com
NODE_MCP_ALLOWED_HOSTS=jacob.chefslegacy.com
LLM_MODEL=minimax/MiniMax-M3
MINIMAX_ANTHROPIC_API_BASE=http://llm-gateway.chefs-system.svc:8080/anthropic
MINIMAX_API_KEY=…              hub token (gateway-issued), not the vendor key
```

### 5.2 llm-gateway (new crate `crates/llm-gateway`)
Purpose: vendor key custody, quotas, cache, usage. Memory target under 30 MB. Runs as a
`Deployment` in `chefs-system` with a PVC for `gateway.db`; vendor keys, the admin token and
the master key come from a `Secret`.

Endpoints (data plane, hub token in `x-api-key` or `Authorization: Bearer`):
- `POST /anthropic/v1/messages`: Messages-format passthrough (MiniMax, Anthropic). `stream: true`
  is forwarded without caching in v1 (or rejected with 501 if simpler).
- `POST /openai/v1/chat/completions`: same for OpenAI-format vendors (phase 2).
- `GET /health`.

Admin API (`/admin/*`, `GATEWAY_ADMIN_TOKEN`):
- `POST /admin/hubs {name, quota?}` → `{token}` (shown once); quota fields default to the
  free tier below.
- `PATCH /admin/hubs/{name}` (quota, status), `DELETE`, `POST /admin/hubs/{name}/byok {vendor, key}`.
- `GET /admin/usage?hub=&from=&to=` → requests, tokens, cost per day.
- `GET /admin/cache/stats`.

Behaviour:
- Vendor selection by path prefix; upstream base URL and key from gateway env
  (`MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, …). A hub with a BYOK key for that vendor uses
  it instead and bypasses the platform budget (usage still recorded).
- Quota, free tier (decided, §11): 30 requests per UTC day during the first 3 days after
  the hub token was created (onboarding), then 10 per day, and at most 150 requests or
  100 cents of estimated cost per calendar month. The effective daily limit is derived from
  `hubs.created_at`; all four numbers are per-hub columns so an operator can raise them.
  Token counters stay as a secondary cap (`daily_token_limit`). Over quota → 429 before any
  upstream call. BYOK hubs skip the platform limits entirely.
- Cache: key = SHA-256 of vendor, model and the **canonical JSON** of the request body:
  object keys sorted recursively, no insignificant whitespace, integral floats written as
  integers (`0.0` and `0` are the same temperature), strings as serde_json emits them, and
  the hub's own credential excluded. Only when the body has `temperature` 0 and no
  streaming; never cache non-2xx; TTL 30 days; response size cap 1 MB; LRU by `last_hit`
  when the table exceeds a configured size. Two hubs that send the same content therefore
  hit the same entry even if their serializers differ (§4.3).
- Concurrency: at most 2 in-flight upstream calls per hub; 120 s upstream timeout; one retry
  with backoff on 5xx and on vendor 429.
- Logging: hub, model, cache hit, tokens, latency, status. Prompt content only with
  `GATEWAY_LOG_CONTENT=1`.

Storage (`gateway.db`, SQLite):
```
hubs(id, name UNIQUE, token_hash, status, created_at,
     onboarding_daily_requests DEFAULT 30, onboarding_days DEFAULT 3,
     daily_request_limit DEFAULT 10, daily_token_limit,
     monthly_request_limit DEFAULT 150, monthly_cost_cap_cents DEFAULT 100,
     byok_json_enc)
usage_daily(hub_id, day, requests, input_tokens, output_tokens, cost_cents, PRIMARY KEY(hub_id, day))
cache(key_hash PRIMARY KEY, vendor, model, status, body BLOB, created_at, last_hit, hits)
model_prices(vendor, model, input_cents_per_mtok, output_cents_per_mtok)
```
Tokens are stored hashed; BYOK keys encrypted with `GATEWAY_MASTER_KEY`.

### 5.3 platform (new crate `crates/platform`)
Purpose: apex site, registry, aggregator, signup, mail. Memory target under 128 MB. Runs as
a `Deployment` in `chefs-system` with a PVC for `platform.db`; the apex `Ingress` points at it.

Public API and pages:
- `GET /` landing and search page (dependency-free UI like the hub's, same design tokens
  copied from a file in this repo, §11).
- `GET /api/v1/search?q=&ingredient=&lang=&limit=&offset=` → hits with `hub`, `public_url`,
  `recipe_id`, `title`, `tags`, `source_language`, `matched_lang`, `image_url`, `also_at[]`.
  With `lang`, `title` and `tags` come from that language's federated translation when the
  hub exposes one (`matched_lang = lang`), otherwise from the source row.
- `GET /api/v1/hubs` → public directory (name, chef name, public recipe count).
- `POST /api/v1/hubs/{name}/changed` (hub token) → poll that hub now.
- `POST /api/v1/login-link {email}` (phase 3) → queues a "sign in to your hub" mail with a
  magic link minted by the provisioner (§4.6); always answers 202.

Admin API (`PLATFORM_ADMIN_TOKEN`): `POST/PATCH/DELETE /admin/hubs`, `POST /admin/poll`,
`GET /admin/outbox`.

Aggregator:
- Poller every 10 minutes with jitter; per-hub backoff on failures; stores
  `last_polled_at`, `last_ok_at`, `last_error`. Change detection uses the recipe's
  `(updated_at, content_hash)` and each translation's `(lang, updated_at)` from the manifest.
- Index tables:
  ```
  hubs(id, name UNIQUE, chef_name, internal_url, public_url, status, last_polled_at, last_ok_at, last_error)
  recipes_index(hub_id, recipe_id, title, tags_json, canonical_json, language, content_hash,
                updated_at, image_url, PRIMARY KEY(hub_id, recipe_id))
  translations_index(hub_id, recipe_id, lang, title, tags_json, origin, stale, updated_at,
                     PRIMARY KEY(hub_id, recipe_id, lang))
  recipes_fts(hub_id UNINDEXED, recipe_id UNINDEXED, lang UNINDEXED, title, tags, canonical)
      -- one row per (hub, recipe, lang): the source row plus one per federated translation
  forks: view grouping recipes_index by content_hash
  ```
  Search runs the FTS query over all rows, keeps the best rank per `(hub, recipe)`, and
  with `lang` prefers the row of that language for the returned title and tags.
- Removal: a recipe missing from a manifest is deleted from the index on the next poll,
  translations with it; a translation missing from an entry's list is deleted likewise.

Mail outbox:
- `mail_outbox(id, to_addr, template, vars_json, status, attempts, next_attempt_at,
  provider_message_id, created_at)`, `suppressions(addr, reason)`.
- Sender loop with exponential backoff (max 5 attempts); provider adapter trait with a
  Resend implementation; per-recipient limit (5 per hour); templates: `verify_email`,
  `hub_ready`, `login_link`, `key_rotated`.

Phase 3 additions: `accounts(email, created_at)`, `magic_links(token_hash, email, expires_at)`
(platform-side email verification; distinct from hub magic links), subdomain validation
(`^[a-z0-9][a-z0-9-]{1,30}$`, reserved names), provisioner client inside `chefs-system`.

### 5.4 provisioner (phase 3, `kube-rs` service with scoped RBAC)
A small Rust service (`crates/provisioner`, `kube-rs`) in `chefs-system`. Its
`ServiceAccount` is bound to a `Role` in `chefs-hubs` that allows `get/list/watch/create/
update/patch/delete` on `statefulsets`, `secrets`, `services`, `ingresses` and
`persistentvolumeclaims` in that namespace only; no cluster-wide rights, no access to
`chefs-system` secrets, no container runtime socket anywhere in the system. A
`NetworkPolicy` admits connections only from the platform pod. Admin token required.
`POST /hubs {name, chef}` / `DELETE /hubs/{name}` / `POST /hubs/{name}/rotate-key` /
`POST /hubs/{name}/login-link → {url, expires_at}` render the same manifests as `hubctl`
(shared template) and apply them through the API server; `login-link` reads the hub's
`Secret` for the key. Keeping key access in this one narrowly-privileged pod limits the blast
radius; hubs cannot reach it at all.

### 5.5 Traefik
k3s installs Traefik; it is configured through a `HelmChartConfig` in `kube-system`
(`deploy/k8s/traefik-config.yaml`), never edited by hand on the node:
- `ports.websecure.forwardedHeaders.trustedIPs` = Cloudflare ranges, so `X-Forwarded-For`
  and `CF-Connecting-IP` are trusted and hubs see the visitor's address; `ports.web`
  redirects to `websecure`.
- Default certificate: a `TLSStore default` referencing `Secret chefs-origin-tls` in
  `kube-system`, the Cloudflare Origin CA certificate for `chefslegacy.com` and
  `*.chefslegacy.com` (15 years); hub `Ingresses` only enable TLS and inherit it. Cloudflare
  SSL mode "Full (strict)".
- One Middleware in `chefs-system`, referenced by annotation from every hub `Ingress` and the
  apex: `headers` (frame deny, nosniff, referrer policy). No `buffering` middleware: Traefik's
  buffering captures the whole response before forwarding it and held back the `/mcp` event
  stream in the k3d test (no headers reached the client while the stream was open; the
  `initialize` reply even came back with a `Content-Length`). Request size limits live in
  the hub (`DefaultBodyLimit` per upload route) and at Cloudflare (100 MB on the free plan).
- Streaming: without that middleware Traefik flushes `text/event-stream` responses as they
  arrive, so `/mcp` SSE passes without configuration (verified through Traefik in k3d); the
  hub's `Cache-Control: no-cache` and `X-Accel-Buffering: no` headers take care of Cloudflare.
- Access logs stay off at Traefik (they would record the magic-link `token` query string);
  the hub and the platform log their own requests without query strings. Error-level Traefik
  logs stay on.

### 5.6 hubctl (`deploy/hubctl`, Python, standard library, `kubectl`-based)
Commands: `init` (namespaces, policies, Traefik config, gateway, platform, backup `CronJob`),
`create`, `remove [--purge]`, `list`, `rotate-key`, `login-link`, `backup`, `restore`,
`upgrade [name|all]`, `status`, `render [name|all] [--json]` (the objects it would apply) and
`render --reference deploy/k8s` (regenerates the checked-in reference set). Idempotent (`kubectl apply`); refuses names that are not
valid subdomains or collide with reserved ones (`www`, `api`, `mail`, `admin`, `gateway`,
`platform`). Runs on the node as root (kubeconfig in `/etc/rancher/k3s/k3s.yaml`) or from
any machine with a kubeconfig.

Hub manifests (rendered per hub by `hubctl`; the checked-in example is `deploy/k8s/hub.example.yaml`):

```yaml
apiVersion: v1
kind: Secret
metadata: {name: hub-{{name}}-env, namespace: chefs-hubs}
stringData: {NODE_API_KEY: "{{key}}", NODE_CHEF_NAME: "{{chef}}", NODE_PUBLIC_URL: "https://{{name}}.chefslegacy.com",
             LLM_MODEL: "minimax/MiniMax-M3", MINIMAX_ANTHROPIC_API_BASE: "http://llm-gateway.chefs-system.svc:8080/anthropic",
             MINIMAX_API_KEY: "{{gateway_token}}"}
---
apiVersion: apps/v1
kind: StatefulSet
metadata: {name: hub-{{name}}, namespace: chefs-hubs, labels: {app: hub, hub: {{name}}}}
spec:
  serviceName: hub-{{name}}
  replicas: 1
  selector: {matchLabels: {app: hub, hub: {{name}}}}
  template:
    metadata: {labels: {app: hub, hub: {{name}}}}
    spec:
      securityContext: {runAsNonRoot: true, runAsUser: 65532, fsGroup: 65532, seccompProfile: {type: RuntimeDefault}}
      containers:
        - name: node
          image: ghcr.io/watchcat/chefsprodigy:{{version}}
          args: ["serve"]
          env: [{name: TOKIO_WORKER_THREADS, value: "2"}]
          envFrom: [{secretRef: {name: hub-{{name}}-env}}]
          ports: [{containerPort: 8080, name: http}]
          volumeMounts: [{name: data, mountPath: /data}, {name: tmp, mountPath: /tmp}]
          resources: {requests: {cpu: 50m, memory: 32Mi}, limits: {cpu: 500m, memory: 128Mi}}
          securityContext: {readOnlyRootFilesystem: true, allowPrivilegeEscalation: false, capabilities: {drop: [ALL]}}
          readinessProbe: {httpGet: {path: /api/v1/health, port: http}, periodSeconds: 10}
          livenessProbe: {httpGet: {path: /api/v1/health, port: http}, periodSeconds: 30, failureThreshold: 3}
      volumes: [{name: tmp, emptyDir: {}}]
  volumeClaimTemplates:
    - metadata: {name: data}
      spec: {accessModes: [ReadWriteOnce], storageClassName: local-path, resources: {requests: {storage: 2Gi}}}
---
apiVersion: v1
kind: Service
metadata: {name: hub-{{name}}, namespace: chefs-hubs}
spec: {selector: {app: hub, hub: {{name}}}, ports: [{port: 8080, targetPort: http}]}
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: hub-{{name}}
  namespace: chefs-hubs
  annotations: {traefik.ingress.kubernetes.io/router.entrypoints: websecure,
                traefik.ingress.kubernetes.io/router.middlewares: chefs-system-headers@kubernetescrd}
spec:
  tls: [{hosts: ["{{name}}.chefslegacy.com"]}]
  rules:
    - host: {{name}}.chefslegacy.com
      http: {paths: [{path: /, pathType: Prefix, backend: {service: {name: hub-{{name}}, port: {number: 8080}}}}]}
```

## 6. Security

- Only Traefik is published (ServiceLB on 443; 80 redirects); the VPS firewall admits 443
  from Cloudflare ranges, 22 from your addresses and nothing else; the k3s API (6443) is
  bound to localhost and reached through SSH. Fail2ban on SSH.
- Namespace isolation: `chefs-hubs` is default-deny. A hub accepts ingress from Traefik
  (`kube-system`) and from the platform (the poller) and may egress to DNS, to public
  addresses and to `llm-gateway.chefs-system`. It cannot reach other hubs, the platform, the
  provisioner or the Kubernetes API (`automountServiceAccountToken: false`, no RBAC).
- SSRF guard (RN-74): everything in `recipe-scraper` that fetches a user-supplied URL
  (`RecipeImporter::import_url`, `fetch_bytes` for covers and JSON-LD images) resolves the
  hostname first and refuses loopback (`127.0.0.0/8`, `::1`), link-local including cloud
  metadata (`169.254.0.0/16`, `fe80::/10`), RFC 1918 (`10/8`, `172.16/12`, `192.168/16`),
  carrier-grade NAT (`100.64/10`), unique-local IPv6 (`fc00::/7`), multicast and
  unspecified addresses, and any scheme other than `http`/`https`. Redirects are followed
  manually (or through a redirect policy) so every hop is checked again, and the connection
  is made to the resolved, checked address so DNS rebinding cannot swap it. A builder flag
  (`allow_private_hosts`) exists only for tests with local servers. Cluster service
  addresses fall in these ranges, so the guard also blocks a hub owner from pointing an
  import at the gateway, the platform or another hub even where a `NetworkPolicy` allows the
  path. The model client is not affected: it talks to the gateway address from
  configuration, not to user input.
- Secrets: vendor keys, the gateway master key and the admin tokens live in `Secrets` in
  `chefs-system`; each hub's `Secret` holds only its owner key and its gateway token; tokens
  hashed at rest in the gateway and platform; BYOK keys encrypted; backups uploaded with an
  rclone `crypt` remote. The platform never holds `NODE_API_KEY`; magic links are minted
  where the key lives (§4.6). `kubectl get secret` access is root on the node and the
  provisioner's and backup job's `ServiceAccounts` only.
- Magic links: HMAC over the owner key, 10-minute expiry, single use, delivered only to the
  verified account email, token removed from the URL by the redirect, no edge access log.
- Pods: non-root (uid 65532), read-only root filesystem, all capabilities dropped,
  `RuntimeDefault` seccomp, memory and CPU limits, `/tmp` on `emptyDir`.
- Abuse: Cloudflare rate-limit rule on `/api/v1/*` for anonymous traffic and a bot rule for
  the apex search; a Traefik `rateLimit` middleware as a second layer; the gateway's quotas
  bound model spend even if a hub owner's key leaks.
- Privacy: gateway logs metadata only by default; the aggregator indexes only what hubs mark
  public (recipes and their translations alike); export archives belong to the owner.

## 7. Capacity and limits

- k3s itself (API server, etcd-in-SQLite, Traefik, CoreDNS, local-path, metrics-server) takes
  roughly 700 MB to 1 GB of RAM on an idle node. Hub idle memory is still unmeasured
  (RN-31); planning assumption 20–40 MB idle, 128 MB limit, 32 MB request.
  `TOKIO_WORKER_THREADS=2` keeps thread count and stack memory independent of the host's
  core count. A 4 GB VPS therefore carries 15–25 hubs with headroom for the gateway and
  platform; an 8 GB box roughly doubles that; beyond that, add a node to the same cluster
  (the manifests do not change).
- CPU: photo normalization and WebP encoding are the heavy paths; the 500m limit keeps a
  burst from one hub off the others.
- Storage: local-path PVCs are directories on the node's disk; 2 Gi per hub is a request,
  not a quota. Watch disk use; the backup `CronJob` is the recovery path.
- Cloudflare free plan: 100 MB request body. Multi-photo imports and Mealie archives can
  exceed it; the UI should downscale photos client-side before upload (RN-72) and the docs
  should point large Mealie imports at `hubctl restore` or a temporary DNS-only hostname.
- MCP over Streamable HTTP goes through Cloudflare on the normal hostname (§11); the hub's
  no-buffering headers and Traefik's native SSE flushing make the stream pass unbuffered.
  Verify once on the first hosted hub.

## 8. Observability

`kubectl logs` and `kubectl get pods -n chefs-hubs` (readiness from the probes);
`kubectl top` through metrics-server for the memory numbers RN-31 needs; gateway usage and
cache stats via its admin API; platform `/admin/hubs` shows last poll per hub; an external
uptime monitor on the apex and one hub. No metrics stack in v1.

## 9. Rollout

- **Phase 0**: Dockerfile with the `healthcheck` subcommand and image publishing;
  `NODE_PUBLIC_URL`, manifest `node_url` and translations in federation; SSRF guard;
  `deploy/k8s/` with namespaces, network policies, Traefik config and the hub template,
  `hubctl` over `kubectl`; run `watchcat.chefslegacy.com` as the first hub behind
  Cloudflare on a fresh k3s node. Model calls still go straight to MiniMax.
- **Phase 1**: llm-gateway v1 (passthrough, tokens, usage) and v2 (quotas, canonical cache,
  admin API, BYOK); hubs switch to it; seeded canonical terms; export endpoint and the
  nightly backup `CronJob`; memory measurement.
- **Phase 2**: platform with registry, poller, index (recipes and translations) and search
  page on the apex; `hubctl` registers hubs; optional change push from hubs.
- **Phase 3**: signup with email verification, magic-link owner login, mail outbox, the
  `kube-rs` provisioner with scoped RBAC, BYOK, client-side photo downscaling.

## 10. Work breakdown

| Story | Size | Scope | Acceptance |
|---|---|---|---|
| RN-63 Dockerfile, `healthcheck` subcommand, image publishing | S | multi-stage build, distroless, non-root, `/data`, `node-server healthcheck`, `HEALTHCHECK` through it, `TOKIO_WORKER_THREADS=2` in the image and the pod template, GitHub Actions to GHCR on tags | `docker run` reports healthy with no `curl` in the image; image < 30 MB compressed |
| RN-64 `NODE_PUBLIC_URL`, manifest `node_url`, translations in federation, SSE headers | S | config, manifest `node_url` and per-entry `translations`, `translations` on the federated recipe, MCP allowlist default, no-buffering headers on SSE | API tests: manifest lists a translation with its own `updated_at`; federated recipe carries it; allowlist derived from the URL |
| RN-74 SSRF guard in `recipe-scraper` | S | §6: resolve and block private, loopback, link-local and metadata ranges, non-HTTP schemes, re-check every redirect hop; test-only `allow_private_hosts` | tests: `http://169.254.169.254/`, `http://127.0.0.1:8080/`, `http://llm-gateway.chefs-system.svc:8080/` and a redirect to a private address are all refused; public URLs still import |
| RN-65 `deploy/k8s/`: namespaces, policies, Traefik config, hub template, `hubctl`, backup `CronJob` | M | files in §3/§5.5/§5.6/§4.7 with the two namespaces and `login-link`, how-to for a fresh k3s node | `hubctl create demo` brings up a routed hub on a fresh node; the `CronJob` lands an archive in R2; MCP streams through Cloudflare and Traefik |
| RN-30 Backup and restore of the node | M | `GET /api/v1/export`, restore import | round trip preserves ids, hashes, covers and translations |
| RN-66 llm-gateway v1: passthrough, tokens, usage | M | §5.2 without cache/quota | hub works unchanged against the gateway; usage rows recorded |
| RN-67 Hub: quota-exceeded mapping | S | 429 passthrough with a readable message | API test |
| RN-68 llm-gateway v2: quotas, canonical cache, admin API | M | §5.2 remainder incl. free-tier tiers and canonical JSON hashing | over-quota 429; the same request with reordered JSON keys is a cache hit; admin endpoints tested |
| RN-75 Seeded `canonical_terms` dictionary | S | migration with ~200 everyday ingredients in `en`, `ru`, `nl`, `origin = 'human'` | fresh node labels `salt`/`garlic`/`onion` in all three languages with no model call; existing rows untouched |
| RN-69 platform v1: registry, poller, index, search API | M | §5.3 without mail/signup, incl. `translations_index` and per-language FTS rows | two hubs indexed; a new translation on a hub appears after the next poll without a recipe change; fork grouping by hash; removal on next poll |
| RN-70 platform search page | M | apex UI reusing hub design tokens | search in `ru` finds a Dutch recipe via its translation when the hub exposes it |
| RN-71 mail outbox and Resend adapter | S | §5.3 outbox | retries and suppression tested with a fake provider |
| RN-72 UI: client-side photo downscale | S | resize to 1600 px before upload | 6-photo import stays under 100 MB |
| RN-73 signup, magic-link login, `kube-rs` provisioner, BYOK | L | §4.6, §5.4: hub `auth/magic` + `auth/magic-link`, `hubctl login-link`, provisioner with scoped RBAC in `chefs-hubs`, platform login-link mail | end-to-end: email → subdomain → running hub → signed in through the emailed link, key never rotated; expired and reused links are refused; the provisioner's `ServiceAccount` cannot read `chefs-system` secrets |
| RN-31 Idle memory measurement | S | existing story, now required; `kubectl top` on the first hosted hub | number recorded; informs §7 |

## 11. Decisions on former open questions

1. **Design tokens**: the apex UI copies the hub's tokens from a file checked into this repo
   (`frontend/dist/styles.css` tokens block extracted to `frontend/tokens.css` and included
   by both). No npm, no CDN.
2. **MCP through Cloudflare**: Streamable HTTP on `/mcp` goes through the proxied hostname.
   The hub sends `Cache-Control: no-cache` and `X-Accel-Buffering: no` on SSE responses and
   Traefik flushes event streams as they arrive (verified in k3d; §5.5 explains why hub routes
   carry no buffering middleware). A separate DNS-only `mcp.<name>` hostname is added only if
   buffering is actually observed at Cloudflare.
3. **Default free-tier quotas**: 30 model requests per day for the first 3 days after the hub
   is created, then 10 per day, capped at 150 requests or 1.00 USD per month, unless the
   owner configures BYOK in the gateway.
4. **Retention**: backups live 30 days in R2 through a bucket lifecycle rule. A removed hub's
   PVC stays until `hubctl remove --purge`, after which only the R2 copies remain and
   expire the same way.
5. **Why k3s instead of Docker Compose**: declarative manifests applied by `kubectl` (and by
   the provisioner through the API) instead of generated Compose files; provisioning through
   a `ServiceAccount` with a namespace-scoped `Role` instead of a container that owns the
   Docker socket; `NetworkPolicy` isolation between hubs and the control services; probes,
   rolling updates and `rollout undo` for free; and a path to a second node without changing
   the manifests. The cost is the cluster's own memory (§7), accepted on a 4 GB box.
