# Kanban Board

Lightweight story board for the Standalone Recipe Node. Move a story between
sections by cutting and pasting its block. Keep stories small enough to finish
in one sitting and always name the acceptance check.

**Conventions**
- ID: `RN-<n>`, never reused.
- Size: `S` (< 1 h), `M` (half a day), `L` (a day or more; consider splitting).
- Every story lists an acceptance check that can be run (`cargo test`, a curl, a screenshot).
- `Done` keeps the last ~20 stories; older ones move to `docs/CHANGELOG.md` (create when needed).
- Live board with drag-and-drop (writes moves back here): `python3 scripts/kanban.py serve`.
- Static read-only export to `docs/kanban.html`: `python3 scripts/kanban.py render --open`.

---

## Backlog

### RN-70 · platform search page on the apex (M)
Dependency-free UI reusing the hub's design tokens: search, hub directory, links to hubs.
- Acceptance: a search in `ru` finds a Dutch recipe through its translation when the hub exposes it; works at 375 px.

### RN-71 · Mail outbox and Resend adapter (S)
Outbox table, sender loop with backoff, suppression list, provider trait with a Resend implementation and a fake for tests.
- Acceptance: retries and suppression covered by tests with the fake provider.

### RN-72 · UI: client-side photo downscale before upload (S)
Resize photos to 1600 px in the browser before `POST /import/photo` so multi-photo imports stay under Cloudflare's 100 MB body cap.
- Acceptance: six 12 MP photos upload under 100 MB total; server result unchanged.

### RN-73 · Signup, magic-link owner login, kube-rs provisioner, BYOK (L)
Spec §4.6 and §5.4. Hub: `GET /api/v1/auth/magic?token=<expires>.<nonce>.<mac>` (HMAC-SHA256 over `NODE_API_KEY`, 10-minute validity, single use, sets `node_session`, redirects to `/#/`) and owner-only `POST /api/v1/auth/magic-link`; `hubctl login-link`. Platform: accounts, email verification via the outbox, subdomain choice, "email me a sign-in link" for existing owners, hub-owner model keys stored in the gateway. Provisioner: a `kube-rs` service in `chefs-system` whose `ServiceAccount` is bound to a `Role` limited to `StatefulSets`, `Secrets`, `Services`, `Ingresses` and PVCs in `chefs-hubs` (no Docker socket anywhere), exposing `create/remove/rotate-key/login-link` to the platform only. `NODE_API_KEY` stays the master secret and is never rotated for login.
- Acceptance: end to end on the node: email → subdomain → running hub → signed in through the emailed link with the key untouched; expired, tampered and reused links answer 401; MCP clients keep working throughout; the provisioner's `ServiceAccount` cannot read `chefs-system` secrets.

### RN-21 · Lossy WebP covers (S/M)
Covers are lossless today (about 1 MB for a 1200 px image). Decide between a
lower max width, a lossy encoder via `libwebp` bindings, or keeping lossless.
- Acceptance: a BBC Good Food import stores a cover under 200 KB, or the decision is recorded in `docs/ARCHITECTURE_AND_SCHEMAS.md`.

### RN-24 · Mealie backup against a real archive (S)
Field names in `recipe-scraper/src/mealie.rs` follow Mealie's table names from memory.
- Acceptance: a real Mealie 1.x backup imports with zero `errors`.

### RN-25 · MCP resources (M)
Expose `recipe://{id}` and `recipe://manifest` as MCP resources next to the tools.
- Acceptance: `resources/list` and `resources/read` work over stdio in `crates/recipe-mcp/tests/server.rs`.

### RN-28 · Federation auth and rate limiting (M)
Federation endpoints are public by design but unthrottled.
- Acceptance: tower-http rate limit on `/api/v1/federation/*`; documented in `docs/MCP_AND_API_SPEC.md`.

### RN-30 · Backup and restore of the node (M)
Export the SQLite file plus images as a zip and import it back. Required for the hosted platform (spec §4.7): adds `GET /api/v1/export` used by the nightly backup job.
- Acceptance: round trip preserves ids, hashes, and covers.

### RN-31 · Idle memory measurement in CI (S)
The < 20 MB RSS goal has never been measured (sandbox blocked `ps`). Required for the hosted platform (spec §7): sets hub density per VPS.
- Acceptance: a script in `scripts/` prints RSS after startup and 50 requests; number recorded in README.

---

## In Progress

_(empty)_

---

## Review

_(empty)_

---

## Done

### RN-69 · platform v1: registry, poller, index, search API (M)
New crate per spec §5.3 without mail and signup: hub registry (`internal_url` = `http://hub-<name>.chefs-hubs.svc:8080`), federation poller with backoff comparing `(updated_at, content_hash)` and each translation's `(lang, updated_at)`, `recipes_index` + `translations_index`, FTS with one row per `(hub, recipe, lang)` so translated titles and tags are searchable, fork grouping by content hash, `GET /api/v1/search?lang=` returning the matched language's title/tags and `matched_lang`.
- Acceptance: two hubs indexed from manifests; a translation added on a hub appears in `ru` search after the next poll with no recipe change; forks grouped; recipes and translations removed on the next poll after unpublishing.


- Done 2026-10-28 (k3s edition): new crate `crates/platform` (lib + `platform` binary; `platform.db` with WAL, NORMAL, foreign keys and busy_timeout 5000; migrations in the crate; FTS5 `recipes_fts` with one row per (hub, recipe, lang), `unicode61 remove_diacritics 2`), sharing the hub's new `FederationManifest` DTO from recipe-core. Poller: one pass callable directly, background loop every 600 s plus jitter, per-hub exponential backoff with `last_error`, refetch only on a changed `(updated_at, content_hash)` or translation `(lang, updated_at)`, off-host redirects and oversized manifests refused. Search: best rank per recipe, one hit per content hash with `also_at`, the requested language's title and tags with `matched_lang`, falling back to the source. Admin API behind `PLATFORM_ADMIN_TOKEN` (503 without it); CLI `serve | add-hub | remove-hub | poll | hubs | healthcheck`; SIGTERM shutdown. hubctl: `init` deploys `deploy/platform` in `chefs-system` (pod labels `app=platform`, `role=hub-client`; `platform-ingress` NetworkPolicy admitting only Traefik) with an apex Ingress routing only `/api/v1/search`, `/api/v1/hubs` and `/health`; `create`/`remove` register and unregister hubs through `kubectl exec deploy/platform -- platform add-hub|remove-hub`; `upgrade` rolls gateway, platform, then hubs; `deploy/k8s/platform.yaml` joins the reference set. Evidence per acceptance bullet: `two_hubs_are_indexed_from_their_manifests`; `a_translation_added_on_a_hub_appears_in_ru_search_after_the_next_poll` (recipe hash and `updated_at` unchanged); `forks_sharing_a_content_hash_are_grouped_with_also_at`; `unpublished_recipes_and_removed_translations_leave_the_index_on_the_next_poll` (FTS rows 5 → 2 → 0). Platform tests 24, hubctl `Platform` tests 9. k3d, fresh cluster, all four stories merged, 41 of 41 checks: two hubs polled through `role=hub-client`; apex search for "erwtensoep" returned jacob's recipe with maria's fork in `also_at`; the private recipe was not indexed; a `ru` translation PUT on the hub came back as "Гороховый суп" with `matched_lang: ru` after the next poll; the apex directory listed both hubs without internal URLs; apex `/admin/hubs` was a 404; a hub pod could not reach the platform; after unpublishing, both searches returned 0; `remove maria` cleared her index rows; the platform pod stopped in 1 s on SIGTERM. Open: `platform.db` is not in the nightly backup (the index refills from the hubs, the registry from `hubctl create`); RN-70 must keep `/admin` unrouted when it adds `/`.

### RN-68 · llm-gateway v2: quotas, cache, admin API (M)
Daily/monthly quotas, deterministic-reply cache, admin endpoints and BYOK per spec §5.2.
- Acceptance: over-quota request gets 429 before any upstream call; a repeated label request is a cache hit; admin API tested.


- Done 2026-10-28 (k3s edition): ported from `09a0348` onto the RN-66 gateway. Free-tier quotas are per-hub columns (30 requests a day for the first 3 days, then 10; 150 requests or 100 cents a month; optional daily token cap) checked before any upstream call, answering 429 `{"type":"error","error":{"type":"quota_exceeded",…}}`, which RN-67 turns into the hub's readable error. Requests in flight count against the limit, so concurrent calls cannot overshoot; token and cost caps include prompt-cache tokens. Canonical-JSON reply cache (keys sorted, integral floats as integers, temperature 0 only, 2xx up to 1 MB, 30-day TTL, LRU above 10000 entries) is consulted before the quota, so a hit is free even over quota; replies carry `x-gateway-cache: hit|miss|bypass`. BYOK keys are encrypted with XChaCha20-Poly1305 under `GATEWAY_MASTER_KEY`; a BYOK hub skips the platform quota and still has usage recorded. Admin API: POST with quota overrides, PATCH (quota, status), DELETE (hub and usage), BYOK add/remove, `GET/PUT /admin/prices` (new: the distroless image has no sqlite3 to insert prices), `GET /admin/cache/stats`. The CLI output hubctl parses is unchanged; hubctl passes the master key (masked in `render`) and the cache settings into the gateway Secret; deploy/README §3 shows the admin API through `kubectl port-forward`. Evidence: gateway crate 40 tests (30 integration, 7 CLI, 3 unit), among them over-quota 429 after exactly N vendor calls, onboarding tier ending after `onboarding_days`, monthly request and cost caps, two concurrent calls at limit 1 giving one 200 and one 429, a reordered-keys request with `0.0` vs `0` hitting the cache from another hub with no extra usage, no caching at temperature 0.7 or of errors, BYOK bypass with the key never listed, the PATCH/disable/re-activate/DELETE flow; deploy tests 42. Merged with RN-67 and RN-75: 262 workspace tests pass, clippy and rustfmt clean. Open: no model prices are seeded, so the cost cap stays inactive until the operator PUTs prices; identical concurrent cache misses both reach the vendor; `gateway.db` is not in the nightly backup. k3d, fresh cluster, all four stories merged (41 of 41 checks): an identical temperature-0 request from a hub pod answered `x-gateway-cache: miss` then `hit`, usage stayed at 1 request, `/admin/cache/stats` showed 1 entry and 1 hit; `/admin/hubs` through `kubectl port-forward` listed the quotas and 401 without the token; `PATCH {"onboarding_days":0,"daily_request_limit":0}` blocked maria with 429 and no upstream call; a real text import through gateway v2 still answered 201; `render all` masked the master key. The cache is shared across hubs and consulted before the quota, so a hub over its limit still gets replies already cached for the same request.

### RN-75 · Seeded `canonical_terms` dictionary (S)
SQLite migration inserting `en`, `ru`, `nl` labels for the ~200 most common everyday ingredients (`salt`, `garlic`, `onion`, `butter`, `egg`, `olive_oil`, …) with `origin = 'human'` via `INSERT OR IGNORE`, so hubs never spend model tokens on basic terms and the gateway cache is not relied on for them (spec §4.3).
- Acceptance: a fresh node labels `salt`/`garlic`/`onion` in all three languages with no model call (fake extractor records zero `translate_terms` calls); rows that already exist are untouched.


- Done 2026-10-28: ported from `a6894ea`. `migrations/20261027000001_seed_canonical_terms.sql` seeds 270 everyday canonical ids with en/ru/nl labels (810 rows, `origin = 'human'`, `INSERT OR IGNORE`), including the plural and compound ids the heuristic normalizer produces (eggs, garlic_cloves, bay_leaves, salt_pepper, extra_virgin_olive_oil); 25 ids and five label corrections (for example nl cottage_cheese "kwark" → "cottagecheese") are new against the old commit. The recipe-db upsert lets a model label replace only `llm` rows, so seeded and hand-corrected labels are never overwritten by the model while a human label replaces anything. Evidence: 6 new tests. On a fresh node the service and the API label salt/garlic/onion as соль/чеснок/лук, zout/knoflook/ui and salt/garlic/onion with the counting fake at 0 `translate_terms` calls; 38 typical recipe lines normalized by `heuristic_ingredient` all hit the seed with 0 model calls; a database migrated up to the seed with an existing model row and an existing human row keeps both unchanged after the seed runs; a model upsert cannot overwrite a seeded label. Merged with RN-67 and RN-68: 262 workspace tests pass, clippy and rustfmt clean. Noticed, not fixed: "1 egg" normalizes to the id `unknown` because `egg` is in the heuristic's unit-word list ("2 eggs" and "1 large egg" work). k3d, fresh cluster: an anonymous `?lang=ru` and `?lang=nl` view of a new recipe showed соль/чеснок/лук and zout/knoflook/ui while the gateway recorded no usage for that hub.

### RN-67 · Hub: model quota-exceeded mapping (S)
Gateway 429/402 surfaces as a readable `model_quota_exceeded` error (HTTP 429) instead of a generic model failure.
- Acceptance: API test with a fake extractor returning the gateway's error.


- Done 2026-10-28 (k3s edition): ported from `76ea681`. A 429 or 402 from the model gateway or vendor now surfaces as HTTP 429 `{"error": "model quota exceeded: …", "code": "model_quota_exceeded"}` instead of a 502 "llm failure"; the message names the cause (node quota used up for the gateway's `quota_exceeded`, vendor rate limit, payment needed) and carries the endpoint's own text. MCP tools return the same sentence. New against the old commit: JSON-LD imports keep working while the model refuses (spec §4.2), because the importer normalizes the JSON-LD ingredient lines locally; `RigExtractor::connect(reference, base_url, api_key)` connects without reading the environment. Evidence: 9 new tests. Six drive a real RigExtractor (MiniMax and Anthropic references) against a fake vendor answering the gateway's exact `quota_exceeded` envelope, `rate_limit_error`, 402, a non-JSON 429 and a 500 (stays generic). `import_text_reports_model_quota_exhaustion_as_429_with_a_code` checks the 429 and the code with a fake extractor. `import_url_keeps_jsonld_imports_working_when_the_model_quota_is_used_up` gets 201 with `star_anise` while a page without JSON-LD gets 429. `import_recipe_reports_model_quota_exhaustion_readably` checks the MCP error and that nothing was saved. The web UI already shows the API's `error` text, so it shows the quota sentence with no UI change. Merged with RN-68 and RN-75: 262 workspace tests pass, clippy and rustfmt clean. Open: refused translations and language detection still fall back to the source wording silently (only a warning is logged). k3d, fresh cluster, all four stories merged: with maria's gateway limits patched to 0, her text import answered 429 `{"error":"model quota exceeded: the model quota for this node is used up (daily limit of 0 model requests reached for this hub; it resets at 00:00 UTC)","code":"model_quota_exceeded"}` and the gateway recorded no usage for her.

### RN-66 · llm-gateway v1: passthrough, hub tokens, usage (M)
New crate per spec §5.2: Messages-format passthrough, token auth, usage rows, vendor key custody. No cache or quota yet.
- Acceptance: a hub works unchanged with `MINIMAX_ANTHROPIC_API_BASE` pointed at the gateway; usage recorded per hub.


- Done 2026-10-28 (k3s edition): new crate `crates/llm-gateway` (from the Compose-era `6f76639`): `/anthropic` and `/minimax` Messages passthrough, hub tokens stored as SHA-256, `usage_daily` rows, one retry on 5xx/429, two in-flight calls per hub, `stream: true` → 501, admin `/admin/hubs` and `/admin/usage`. New for k3s: CLI `add-hub [--rotate] [--json]`, `disable-hub`, `usage` (JSON) so hubctl drives the gateway through `kubectl exec` without an admin port; SIGTERM shutdown (PID 1 in a pod); input tokens now include `cache_creation_input_tokens` and `cache_read_input_tokens`, which MiniMax reports outside `input_tokens` (seen live: 697 + 128). Same image as the hubs. hubctl: `init` deploys `llm-gateway` in `chefs-system` (Secret with the vendor keys, 1Gi PVC, one-replica `Recreate` Deployment, 64Mi limit, hub hardening, env checksum on the pod template, NetworkPolicy admitting only `app=hub` pods from `chefs-hubs`) when `.env` has a vendor key (`LLM_GATEWAY=off` opts out); `create` registers the hub and writes the token plus `MINIMAX_ANTHROPIC_API_BASE`/`ANTHROPIC_BASE_URL` = `http://llm-gateway.chefs-system.svc:8080/anthropic` into its Secret instead of the vendor key, and moves direct-access hubs onto the gateway; `remove` disables the token; new `gateway-token <name>` and `usage [name] [--from --to]`; `upgrade` rolls the gateway first; `status` and `render` show it; `deploy/k8s/llm-gateway.yaml` added to the reference set. Also fixed: `init` on a fresh k3s node failed while Traefik's CRDs were still installing; it now waits for `middlewares.traefik.io`. Tests: workspace 226 passed (gateway 12 proxy + 7 CLI), clippy and rustfmt clean; deploy tests 41 passed (2 PyYAML-only skipped under `-I`, both pass with PyYAML). k3d (k3s v1.36.4), fresh cluster, 33 of 33 checks: hub Secret holds a gateway token and no vendor key; a text import through Traefik → hub → gateway → MiniMax returned 201 with canonical ids spaghetti, garlic, lemon, olive_oil, parmesan_cheese, and `hubctl usage demo` showed 1 request, 402 input and 392 output tokens; pods in `default` and unlabelled `chefs-system` pods cannot reach the gateway; bad token 401, valid token 400 on an empty body; the gateway pod stopped in 1 s on SIGTERM and kept its usage across the restart; `gateway-token` refused the old token; `upgrade` moved gateway and hub to a new tag; `remove` revoked the token and a new `create` got an active one; `kubectl apply --dry-run=server -f deploy/k8s/` accepted. Not covered: `gateway.db` is not in the nightly backup yet (re-issue with `hubctl gateway-token`); node-server still shuts down only on Ctrl-C, so hub pods wait out the 30 s grace period on restart.

### RN-65 · deploy/k8s: namespaces, policies, Traefik config, hub template, hubctl, backup CronJob (M)
k3s + Traefik + `kubectl` per `docs/SPEC-hosted-platform.md` §3, §4.7, §5.5, §5.6: `deploy/k8s/` with the `chefs-system` and `chefs-hubs` namespaces, default-deny `NetworkPolicy` in `chefs-hubs` (ingress from Traefik and the platform only; egress to DNS, public addresses and `llm-gateway`), the Traefik `HelmChartConfig` (Cloudflare trusted IPs, Origin CA default `TLSStore`, `headers` middleware, access logs off), the per-hub template (`StatefulSet` + `Secret` + `Service` + `Ingress` + PVC, non-root, read-only, probes on `/api/v1/health`), the nightly backup `CronJob` to R2 via rclone, and `hubctl` (Python, standard library, drives `kubectl`: init, create, remove, list, status, rotate-key, login-link, backup, restore, upgrade) plus a how-to for a fresh k3s node behind Cloudflare.
- Acceptance: `hubctl create demo` brings up a routed hub on a fresh k3s node; the backup `CronJob` lands an archive in R2; MCP streams through Cloudflare and Traefik; a hub pod cannot reach the platform, the provisioner or another hub.


- Done 2026-10-27 (k3s edition): `deploy/hubctl` (Python 3.9+, standard library, drives `kubectl` with JSON manifests on stdin) implements init, create, remove [--purge], list, status, rotate-key, login-link, backup <name|all>, restore, upgrade and render [name|all] [--json] / render --reference; `deploy/k8s/` holds the generated reference set (namespaces, network policies, Traefik `HelmChartConfig` + `headers` Middleware + `TLSStore`, backup CronJob with ServiceAccount/Role/scripts, `hub.example.yaml`), kept in sync by a test; `deploy/env.example`, `deploy/README.md` (k3s install, provider firewall, Cloudflare DNS and Origin CA Secret, first hub, MCP check, R2 backups, restore, upgrades, k3d dry run, troubleshooting) and `.github/workflows/deploy.yml`. 25 unit tests against a fake kubectl (`deploy/tests`). Verified on a fresh k3d cluster (k3s v1.36.4; Traefik restarted from the HelmChartConfig with the Cloudflare ranges in its args and the access log off): `hubctl create demo` came up and was routed through Traefik (health and UI 200 with the security headers, unknown host 404); MCP `initialize` and `tools/list` answered 200 and the standalone SSE stream delivered its headers at once (chunked, no `Content-Length`), but only after dropping the spec's `buffering` middleware, which captured whole responses and stalled `/mcp` (the GET stream never produced headers; `initialize` came back with a `Content-Length`); spec §5.5/§5.6 and this card now say `headers` only. NetworkPolicy probes: a hub-labelled pod cannot reach another hub, a `chefs-system` stand-in for the platform or the Kubernetes API while DNS and public addresses work; a `role=hub-client` pod in `chefs-system` reaches the hub and an unlabelled one cannot. `hubctl backup demo` ran the CronJob's Job to completion (RBAC read of the hub Secret, fetch reached the hub where `/api/v1/export` is still 404 until RN-30, rclone stage skipped without R2 settings), so "an archive lands in R2" stays open until RN-30 ships and R2 credentials exist; `hubctl restore` put an export fixture holding the recipe "Smoke Noodles" back into the hub; rotate-key (old key 401, new key 200), login-link, list, status, upgrade, remove (PVC kept) and `--purge` (PVC deleted) behaved. 54 of 55 smoke checks passed; the single failure was the throwaway script expecting JSON from `render`, which now prints YAML by default (`--json` is unit-tested).

### RN-74 · SSRF guard on user-supplied URL fetches (S)
Harden `recipe-scraper` (`RecipeImporter::import_url`, `fetch_bytes` for covers and JSON-LD images, used by `ImportService::import_url`): resolve the hostname first and refuse loopback, link-local incl. cloud metadata (`169.254.0.0/16`, `fe80::/10`), RFC 1918, CGNAT `100.64/10`, `fc00::/7`, multicast/unspecified addresses and any scheme other than `http`/`https`; re-check every redirect hop and connect to the checked address. A builder flag `allow_private_hosts` exists only for tests with local servers. Spec: `docs/SPEC-hosted-platform.md` §6.
- Acceptance: tests show `http://169.254.169.254/`, `http://127.0.0.1:8080/`, `http://llm-gateway:8080/` and a public URL redirecting to a private address are refused with a clear error; public URLs still import; the model client is unaffected.

- Done 2026-10-27 (re-applied from the rolled-back 33c7a53 onto the k3s baseline): `recipe-scraper/src/ssrf.rs` with `UrlGuard` (scheme `http`/`https` only, literal IP hosts and every DNS answer for a hostname checked), `blocked_reason` classifying loopback, RFC 1918, CGNAT `100.64/10`, link-local `169.254/16` + `fe80::/10`, ULA `fc00::/7`, multicast, unspecified and reserved ranges, IPv4 embedded in IPv6 (mapped, NAT64, 6to4) judged as IPv4; the importer's reqwest client uses `GuardedResolver` (system lookup, every answer checked, connection pinned to the checked address) and `redirect_policy` (scheme + literal IP re-checked per hop, 10-hop limit); redirects reqwest will not follow (`file:`, `ftp:`) are refused explicitly. `ScraperError::BlockedUrl` → HTTP 400 (`refusing to fetch …`) and MCP invalid-input. `RecipeImporter::allow_private_hosts()` unlocks loopback/private/CGNAT/ULA for tests only, never link-local or multicast. Cluster service addresses fall in the blocked ranges, so the guard also stops a hub pointing an import at `llm-gateway.chefs-system.svc`, the platform or another hub even where a NetworkPolicy allows the path (spec §6); the hostname test uses that service name. 8 tests (207 total), clippy and rustfmt clean; CLAUDE.md guardrail 6, architecture doc, API spec and README describe the guard.

### RN-64 · `NODE_PUBLIC_URL`, manifest `node_url`, translations in federation, SSE headers (S)
`NODE_PUBLIC_URL` surfaced as `node_url` in `GET /api/v1/federation/manifest`, its host added to the MCP allowlist by default. Manifest entries gain `translations: [{lang, origin, stale, updated_at}]` and `GET /api/v1/federation/recipes/{id}` returns `translations: [RecipeTranslation]`, because saving a translation never changes `updated_at`/`content_hash` and the aggregator must still notice it (spec §4.4, `docs/MCP_AND_API_SPEC.md`). SSE responses on `/mcp` send `Cache-Control: no-cache` and `X-Accel-Buffering: no`.
- Acceptance: API tests: manifest lists a translation with its own `updated_at` after `PUT /recipes/{id}/translations/ru` without the recipe's `updated_at` changing; federated recipe carries it; public recipes only; allowlist derived from the URL.

- Done 2026-10-27 (re-applied from the rolled-back a02de56 onto the k3s baseline): `NODE_PUBLIC_URL` serve option (validated absolute http(s) URL, trailing slash dropped) published as manifest `node_url` (null when unset); the default `/mcp` allowlist is loopback plus that URL's `host[:port]` (`default_mcp_allowed_hosts_with`), an explicit `NODE_MCP_ALLOWED_HOSTS` still replaces the list. `ManifestEntry.translations: [{lang, origin, stale, updated_at}]` from one join over public recipes; `GET /federation/recipes/{id}` returns `FederatedRecipe` (recipe flat + `translations: [RecipeTranslation]`); private recipes and their translations stay hidden. SSE headers: rmcp 3.6 already sends `Cache-Control: no-cache` and `X-Accel-Buffering: no` on every event stream; a test pins it. 11 tests (DTO shapes, repository queries, manifest keeps the recipe's `updated_at` after a translation and flips `stale` after an edit, federated translations, private exclusion, node_url, allowlist derivation, SSE headers); workspace 199, clippy and rustfmt clean. API spec, architecture doc, README and `.env.example` document the two variables. In the k3s design the hub's `NODE_PUBLIC_URL` comes from its `Secret` and the platform polls `http://hub-<name>.chefs-hubs.svc:8080`.

### RN-63 · Dockerfile, `healthcheck` subcommand and image publishing (S)
Per `docs/SPEC-hosted-platform.md` §5.1: multi-stage build, distroless runtime with no shell tools, non-root, `/data` volume; new `node-server healthcheck [--port]` subcommand that does one loopback GET of `/api/v1/health` (2 s timeout, exit 0/1) and is used by `HEALTHCHECK`, so the image needs no `curl`; `TOKIO_WORKER_THREADS=2` set in the image and repeated in the k3s pod template (§5.6), where the kubelet probes `/api/v1/health` over `httpGet`; GitHub Actions publishing `ghcr.io/watchcat/chefsprodigy` on tags.
- Acceptance: `docker run` reports healthy with no `curl`/`wget` in the image; `node-server healthcheck` exits 1 when nothing listens; image under 30 MB compressed.

- Done 2026-10-27 (re-applied from the rolled-back 9c38a83 onto the k3s baseline, board and docs redone): `Dockerfile` (rust:1.97-slim-bookworm build stage with BuildKit cache mounts → `gcr.io/distroless/cc-debian12:nonroot`, `/data` owned by uid 65532, `TOKIO_WORKER_THREADS=2`, `HEALTHCHECK` through the binary), `.dockerignore` (excludes docs, scripts, deploy/, secrets, `rust-toolchain.toml`), `node-server healthcheck [--port] [--host]` (hand-written loopback GET of `/api/v1/health`, 2 s timeout, exit 0/1; `NODE_PORT` default like `serve`), `.github/workflows/docker.yml` (build + smoke test on pushes and PRs, publish `ghcr.io/watchcat/chefsprodigy` as version/major.minor/latest on `v*` tags). 8 tests in `tests/healthcheck.rs` (188 total), clippy and rustfmt clean. Container check 2026-10-27 (arm64): healthy after 15 s via its own probe, no `/bin/sh`, uid 65532, recipe POST to `/data` 201, probe exit 1 on a dead port and 0 on the live one, `TOKIO_WORKER_THREADS=2` in the image env, 14.4 MB uncompressed / 14.1 MB gzip (limit 30 MB), 2.8 MiB RSS idle. On k3s the kubelet probes `/api/v1/health` over `httpGet` (RN-65 pod template); the subcommand serves `docker run` and `kubectl exec`. First GHCR publish happens on the first `v*` tag.

### RN-62 · UI: owner login and read-only mode for visitors (S)
The PWA now checks `GET /api/v1/auth/me` before rendering a route, presents a native owner-key login dialog backed by `POST /api/v1/auth/login`, and offers `POST /api/v1/auth/logout`. Visitors see only public recipes and no create/edit/delete/import/translation-correction controls; direct write routes render an owner-access prompt, real `401` responses open the same login dialog, and the recipe detail explains that uncached display-language translations appear after the owner has viewed the recipe. Owner sessions use the existing HttpOnly cookie and survive reloads.
- Verified in-browser 2026-10-24 via `scripts/devbuild.sh` against an isolated key-protected node: anonymous view contained only a public Dutch recipe, no write controls, a Russian cached-translation hint, and the protected `#/new` login prompt. After native-dialog sign-in, the session survived a reload, the owner saw the private recipe plus import/edit/delete controls, and logout restored public-only visitor mode.

### RN-61 · Owner vs. public reader authentication on the REST API (M)
`NODE_API_KEY` is the owner secret: `RequireOwner`/`OptionalOwner` extractors (bearer header or HttpOnly `node_session` cookie, constant-time compare), `POST /auth/login|logout`, `GET /auth/me`. Writes, cover upload, translation edits and all imports are owner-only (401); anonymous callers see public recipes only (listings, counts, shopping lists, 404 on private ids) and get cached translations/labels without live model calls. Without a key the node is single-user and warns at startup. 6 new tests.

### RN-60 · Backfill source language for existing recipes (S)
Offline detector (`langdetect.rs`: Cyrillic letter signals for ru/uk, stopword scoring for en/nl/de/fr/es/it) applied at import time and by `backfill-language` (CLI + background pass at `serve` startup); the model is asked only when the heuristic is unsure; backfill never touches `updated_at`/`content_hash`. 7 new tests.

### RN-59 · UI: translated recipe content and corrections (M)
Library, recipe detail, and shopping-list requests now send the selected UI language as `lang`. Localized titles, tags, ingredient labels, and shopping labels render from the server response. Recipe detail shows translation provenance, permits a source-wording toggle, displays a translation loading state, and offers `#/recipe/{id}/translation` to save ordered human wording corrections through the documented translation endpoint; retained human corrections show a stale-source hint after source edits.
- Verified in-browser 2026-10-24 via `scripts/devbuild.sh`: a Dutch `Hollandse stamppot` card rendered in Russian, opened with its human-translation provenance, toggled to its Dutch source and back, and saved a corrected Russian ingredient. The correction survived a reload; after a source-recipe `PUT`, it remained visible with the Russian stale-source hint. Browser console had no errors.
- Follow-up verified in-browser 2026-10-24 after updating the static PWA shell: the saved Torchiette recipe rendered its Russian title, ingredient wording, and method after selecting Russian, and remained Russian after reload with `html[lang="ru"]` and `localStorage["ui.lang"] = "ru"`.

### RN-57 · MCP: language parameter on tools (S)
`search_recipes`, `get_recipe_details`, `build_smart_shopping_list` accept `language`; end-to-end stdio test with a fake translator.

### RN-56 · Backend: localized canonical ingredient labels (S)
`canonical_terms` cache + `TranslationService::labels` (batches of 100, humanized fallback, no-LLM safe); `canonical_label` on ingredients of localized recipes and on shopping-list lines (`?lang=`/`Accept-Language`).

### RN-55 · Backend: search across translations (S)
`q` matches `recipes_fts` and `translations_fts` (union, best rank, one row per recipe) in both search and count; `lang` on `GET /recipes` makes summaries use an existing translation and adds `translation_lang`.

### RN-58 · UI: message bundles and language switcher (M)
The embedded PWA now ships parity-checked `en`, `ru`, and `nl` message bundles, with bundle-backed static UI text, an English fallback, and a header switcher persisted in `localStorage["ui.lang"]`. The active language updates `<html lang>` and drives `Intl` number, unit, duration, and timer formatting; the static PWA shell caches all three bundles.
- Verified in-browser 2026-10-24 via `scripts/devbuild.sh`: the switcher changed the live library from English to Russian and Dutch, set `html[lang]` to `ru`/`nl`, persisted through a reload, and returned to unchanged English. `scripts/i18n-check.py` passed bundle parity and renderer-string checks.

### RN-54 · Backend: translation store and service (M)
`recipe_translations` + `translations_fts` (triggers) + `canonical_terms` tables; `TranslationService` translates lazily on `GET /recipes/{id}?lang=` (or `Accept-Language`), caches per language, coalesces concurrent requests, regenerates stale machine translations, keeps human ones (`PUT/DELETE /recipes/{id}/translations/{lang}`), validates reply lengths and falls back to the source on failure. `GET /i18n/languages`. 20 new tests.

### RN-53 · Backend: source language on recipes (S)
`recipes.language` (migration `20261024000001_i18n.sql`), normalized BCP-47 primary subtag; detected from JSON-LD `inLanguage` / `<html lang>`, LLM drafts (`language` field), photo extraction; exposed on recipes and summaries; settable via create/update.

### RN-52 · Fix editor heading collapsing to one letter per line (S)
Bug: on `#/recipe/{id}/edit` the title and eyebrow rendered one character per line. The heading grid's cover column was `auto`, so the cover image's 1200 px natural width took the whole row and left the title track at 0 px. Fixed 2026-10-24 with `fit-content(14rem)` for that column and a capped preview width; verified with and without a cover and at 375 px.
