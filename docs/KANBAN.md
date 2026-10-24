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
Export the SQLite file plus images as a zip and import it back.
- Acceptance: round trip preserves ids, hashes, and covers.

### RN-31 · Idle memory measurement in CI (S)
The < 20 MB RSS goal has never been measured (sandbox blocked `ps`).
- Acceptance: a script in `scripts/` prints RSS after startup and 50 requests; number recorded in README.

---

## In Progress

_(empty)_

---

## Review

_(empty)_

---

## Done

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

### RN-45 · MCP: photo_paths on import_recipe (S)
`import_recipe` accepts `photo_paths`; unreadable paths are reported in the tool error. End-to-end test over stdio with a fake vision extractor.

### RN-44 · Backend: photo import endpoint and vision extraction (M)
`POST /api/v1/import/photo` live: EXIF-oriented 1600 px JPEG normalization, one vision call (`PhotoExtraction`), lenient parsing, cover from the first dish photo, and 422/503 paths. The first MiniMax-M3 browser run is verified under RN-46.

### RN-41 · UI: cover image upload in the editor (S)
The editor accepts JPEG, PNG, and WebP images, then posts the selected file as multipart field `file` to `PUT /api/v1/recipes/{id}/image` after a recipe exists. It waits for the returned recipe and routes to its detail page, where the converted cover is rendered.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: a JPEG uploaded through the Beef Pho editor and the returned WebP cover rendered on both the recipe detail page and library card.

### RN-35 · UI: manual recipe editor (M)
The PWA now has `#/new` and `#/recipe/{id}/edit` views with accessible create/edit forms for title, description, servings, times, tags, provenance, visibility, normalized ingredients, specialty flags, and ordered timed steps. Ingredient and step rows are dynamically addable/removable; successful saves use the documented `POST` or `PUT` endpoint and API failures render inline.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: Browser Lemon Orzo was created from the form and opened in detail; the seeded Beef Pho recipe round-tripped all 5 ingredients, 5 specialty controls, 3 ordered instructions, and 3 timer controls through `PUT`; dynamic controls added a sixth ingredient and fourth step without persisting on Cancel. Browser console had no errors.

### RN-43 · Drop the unused legacy vector mark (S)
Removed the unreferenced 150 KB legacy vector app mark from the embedded bundle. The local PNG icon set remains the sole browser and manifest branding source, and the frontend scope now documents that arrangement.
- Verified 2026-10-22: the static bundle is 176 KB and a repository-source scan finds no retired asset filename.
