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

### RN-74 · SSRF guard on user-supplied URL fetches (S)
Harden `recipe-scraper` (`RecipeImporter::import_url`, `fetch_bytes` for covers and JSON-LD images, used by `ImportService::import_url`): resolve the hostname first and refuse loopback, link-local incl. cloud metadata (`169.254.0.0/16`, `fe80::/10`), RFC 1918, CGNAT `100.64/10`, `fc00::/7`, multicast/unspecified addresses and any scheme other than `http`/`https`; re-check every redirect hop and connect to the checked address. A builder flag `allow_private_hosts` exists only for tests with local servers. Spec: `docs/SPEC-hosted-platform.md` §6.
- Acceptance: tests show `http://169.254.169.254/`, `http://127.0.0.1:8080/`, `http://llm-gateway:8080/` and a public URL redirecting to a private address are refused with a clear error; public URLs still import; the model client is unaffected.

### RN-65 · deploy/: Compose, Caddy, hubctl, backup job (M)
Files described in the spec §3, §4.7, §5.5, §5.6 plus a deployment how-to for one VPS behind Cloudflare.
- Acceptance: `hubctl create demo` brings up a routed hub on a fresh box; `hubctl backup demo` lands an archive in R2.

### RN-66 · llm-gateway v1: passthrough, hub tokens, usage (M)
New crate per spec §5.2: Messages-format passthrough, token auth, usage rows, vendor key custody. No cache or quota yet.
- Acceptance: a hub works unchanged with `MINIMAX_ANTHROPIC_API_BASE` pointed at the gateway; usage recorded per hub.

### RN-67 · Hub: model quota-exceeded mapping (S)
Gateway 429/402 surfaces as a readable `model_quota_exceeded` error (HTTP 429) instead of a generic model failure.
- Acceptance: API test with a fake extractor returning the gateway's error.

### RN-68 · llm-gateway v2: quotas, cache, admin API (M)
Daily/monthly quotas, deterministic-reply cache, admin endpoints and BYOK per spec §5.2.
- Acceptance: over-quota request gets 429 before any upstream call; a repeated label request is a cache hit; admin API tested.

### RN-75 · Seeded `canonical_terms` dictionary (S)
SQLite migration inserting `en`, `ru`, `nl` labels for the ~200 most common everyday ingredients (`salt`, `garlic`, `onion`, `butter`, `egg`, `olive_oil`, …) with `origin = 'human'` via `INSERT OR IGNORE`, so hubs never spend model tokens on basic terms and the gateway cache is not relied on for them (spec §4.3).
- Acceptance: a fresh node labels `salt`/`garlic`/`onion` in all three languages with no model call (fake extractor records zero `translate_terms` calls); rows that already exist are untouched.

### RN-69 · platform v1: registry, poller, index, search API (M)
New crate per spec §5.3 without mail and signup: hub registry, federation poller with backoff comparing `(updated_at, content_hash)` and each translation's `(lang, updated_at)`, `recipes_index` + `translations_index`, FTS with one row per `(hub, recipe, lang)`, fork grouping by content hash, `GET /api/v1/search?lang=` returning the matched language's title/tags and `matched_lang`.
- Acceptance: two hubs indexed from manifests; a translation added on a hub appears in `ru` search after the next poll with no recipe change; forks grouped; recipes and translations removed on the next poll after unpublishing.

### RN-70 · platform search page on the apex (M)
Dependency-free UI reusing the hub's design tokens: search, hub directory, links to hubs.
- Acceptance: a search in `ru` finds a Dutch recipe through its translation when the hub exposes it; works at 375 px.

### RN-71 · Mail outbox and Resend adapter (S)
Outbox table, sender loop with backoff, suppression list, provider trait with a Resend implementation and a fake for tests.
- Acceptance: retries and suppression covered by tests with the fake provider.

### RN-72 · UI: client-side photo downscale before upload (S)
Resize photos to 1600 px in the browser before `POST /import/photo` so multi-photo imports stay under Cloudflare's 100 MB body cap.
- Acceptance: six 12 MP photos upload under 100 MB total; server result unchanged.

### RN-73 · Signup, magic-link owner login, provisioner, BYOK (L)
Spec §4.6 and §5.4. Hub: `GET /api/v1/auth/magic?token=<expires>.<nonce>.<mac>` (HMAC-SHA256 over `NODE_API_KEY`, 10-minute validity, single use, sets `node_session`, redirects to `/#/`) and owner-only `POST /api/v1/auth/magic-link`; `hubctl login-link`. Platform: accounts, email verification via the outbox, subdomain choice, provisioner on `control_net` with the Docker socket exposing `create/remove/rotate-key/login-link`, "email me a sign-in link" for existing owners, hub-owner model keys stored in the gateway. `NODE_API_KEY` stays the master secret and is never rotated for login.
- Acceptance: end to end on the VPS: email → subdomain → running hub → signed in through the emailed link with the key untouched; expired, tampered and reused links answer 401; MCP clients keep working throughout.

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

### RN-64 · `NODE_PUBLIC_URL`, manifest `node_url`, translations in federation, SSE headers (S)
`NODE_PUBLIC_URL` surfaced as `node_url` in `GET /api/v1/federation/manifest`, its host added to the MCP allowlist by default. Manifest entries gain `translations: [{lang, origin, stale, updated_at}]` and `GET /api/v1/federation/recipes/{id}` returns `translations: [RecipeTranslation]`, because saving a translation never changes `updated_at`/`content_hash` and the aggregator must still notice it (spec §4.4, `docs/MCP_AND_API_SPEC.md`). SSE responses on `/mcp` send `Cache-Control: no-cache` and `X-Accel-Buffering: no`.
- Acceptance: API tests: manifest lists a translation with its own `updated_at` after `PUT /recipes/{id}/translations/ru` without the recipe's `updated_at` changing; federated recipe carries it; public recipes only; allowlist derived from the URL.

- Done 2026-10-26: `NODE_PUBLIC_URL` (validated absolute http(s) URL, trailing slash dropped) is published as manifest `node_url`; the default `/mcp` allowlist is loopback plus the URL's `host[:port]` (`default_mcp_allowed_hosts_with`), an explicit `NODE_MCP_ALLOWED_HOSTS` still wins. `ManifestEntry.translations: [{lang, origin, stale, updated_at}]` comes from one join over public recipes; `GET /federation/recipes/{id}` returns `FederatedRecipe` (recipe flat + `translations: [RecipeTranslation]`). SSE headers: rmcp 3.6 already sends `Cache-Control: no-cache` and `X-Accel-Buffering: no` on every event stream, so a regression test pins it instead of new code. 11 new tests (199 total), clippy and rustfmt clean.

### RN-63 · Dockerfile, `healthcheck` subcommand and image publishing (S)
Multi-stage `Dockerfile` (rust:1.97-slim-bookworm → distroless `cc-debian12:nonroot`, BuildKit cache mounts, `.dockerignore`), new `node-server healthcheck [--port] [--host]` subcommand (`crates/node-server/src/health.rs`: raw loopback HTTP GET of `/api/v1/health`, 2 s timeout, exit 0/1, port from `NODE_PORT`), image env `NODE_HOST=0.0.0.0 NODE_PORT=8080 NODE_DATA_DIR=/data TOKIO_WORKER_THREADS=2`, `/data` owned by uid 65532, `.github/workflows/docker.yml` building and smoke-testing on main/PRs and publishing `v*` tags to `ghcr.io/watchcat/chefsprodigy` (`<version>`, `<major.minor>`, `latest`). README and CLAUDE.md document it.
- 8 new tests in `crates/node-server/tests/healthcheck.rs` (healthy node, nothing listening, error status, wrong body, silent server timeout, CLI exit codes 0/1, `NODE_PORT` default); 188 tests total, clippy clean.
- Verified 2026-10-26 with a local `docker build` (arm64, 2m10s compile): container reported `healthy` after 9 s through its own probe; no `/bin/sh` in the image; process uid 65532; a recipe POST wrote to `/data` (201); `healthcheck --port 9` exited 1; image 13 MB compressed; 3.5 MiB RSS after a few requests. The first real GHCR publish happens on the first `v*` tag.

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

### RN-51 · Verify photo import with real photos (S)
Verified 2026-10-23 with MiniMax-M3: a Marley Spoon card (front with hero photo + back with Dutch text) imports with the full title, 12 ingredients, 6 steps with timers, servings 2, cook 30 min, and the front photo as cover; an English card ("Willow Chicken in Black Bean Sauce") also imported. Fixes found on the way: `classification`/`type` keys for photo kinds, notes mentioning a dish photo promote a text page to cover, equipment filtered from ingredients, nested `times` objects, synthesized title when the card side has no title.
Known limits: covers are lossless WebP (~1.2 MB for a card front, see RN-21); pasta name sometimes misread ("torchiatelle").

### RN-47 · Dev tooling: devbuild.sh, .env, per-toolchain target dirs (S)
`scripts/devbuild.sh` builds, runs, seeds and opens the browser; loads `.env` (template `.env.example`, git-ignored); uses `target/rustc-<version>/` per toolchain and refuses a stale `DEVELOPER_DIR`.

### RN-48 · MiniMax-M3 default and lenient model-reply parsing (M)
`LLM_MODEL=minimax/MiniMax-M3` via `.env`; rig reads vendor variables unless `LLM_API_KEY` overrides. Replies with missing/renamed fields, string ranges, time phrases, decimal commas and unit spellings now parse; gateway errors and rejected replies are logged.

### RN-49 · Navbar logo and PWA icons (S)
PNG mark at 128/192/512 px plus Apple touch icon; favicon, manifest icons and service-worker shell updated.

### RN-50 · Kanban tooling (S)
`scripts/kanban.py serve` (drag-and-drop board writing back to this file, replaces stale instances) and `render` (static `docs/kanban.html`).

### RN-39 · UI: shopping list builder (M)
The PWA now has a `#/shopping` builder, linked from the library and primary navigation. It loads local recipes into an accessible multi-select basket with per-recipe serving inputs and optional comma-separated pantry exclusions. It calls `POST /api/v1/shopping-list`, separates basic groceries from specialty items, and persists the basket, pantry text, and independently checked grocery rows in `localStorage`.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: Beef Pho and Quick Noodle Bowl produced an aggregated `600 g` Rice Noodles line for two recipes, Fish Sauce appeared under Specialty items, and Salt was excluded through the pantry field. A checked Rice Noodles row remained checked after a page reload and rebuilt list; browser console had no errors.

### RN-46 · UI: import from photos (M)
The import dialog now includes a responsive From photo tab with a camera-capable 1–6 image picker, local thumbnails, size/count feedback, a privacy disclosure, and visible analysis progress. It posts repeated multipart `photos` plus `is_public` to `POST /api/v1/import/photo`, renders errors inline, and opens a created recipe on success.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: a readable recipe-card JPEG imported through MiniMax-M3 as Lemon Herb Orzo and opened in detail; a logo image returned an inline 422 no-recipe message; an isolated no-model node returned the inline 503 vision-capability message. Browser console had no errors.
