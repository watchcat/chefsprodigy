# Changelog

Stories moved out of the Kanban `Done` column, newest first. Ids are never reused.

## Archived 2026-10-26

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

## Archived 2026-10-23

### RN-20 · Frontend PWA with Cook Mode (L)
Verified against the live node 2026-10-21: library with search/tag/pantry filters and covers, detail with specialty badges, server-side serving scaling, Cook Mode with per-step timers and WakeLock, responsive at 375 px.
Original scope excluded manual CRUD, Mealie upload, and shopping lists; later stories RN-36 and RN-37 added deletion and Mealie backup upload. RN-42 subsequently completed the full-browser Lighthouse installability audit and install flow.

### RN-34 · Import from the UI
Import dialog covers URL and text imports with a federation toggle; server errors (e.g. no `LLM_MODEL`) are shown inline. Verified in-browser 2026-10-21.

### RN-33 · Frontend source and build pipeline
Resolved by design: `docs/SPEC-web-ui.md` defines a no-build, dependency-free UI, so `frontend/dist` is the source. Validate with `node --check frontend/dist/app.js`.

### RN-22 · Update CLAUDE.md and architecture doc for rig 0.45
Docs describe the catalog reference (`LLM_MODEL=vendor/model`), `LLM_API_KEY`, and the structured-output request; no `Extractor<T>` left.

### RN-32 · Toolchain alignment
Flake updated to nixpkgs 2026-10-20 (rustc 1.99); `rust-toolchain.toml` added for rustup users; rig-core unpinned to 0.45.

### RN-19 · MCP Streamable HTTP at `/mcp` with bearer auth
401 without token, 503 when `NODE_API_KEY` unset, loopback host allowlist by default.

### RN-18 · MCP stdio transport (`node-server mcp-stdio`)
Logs to stderr; verified handshake and `tools/list` against the release binary.

### RN-17 · MCP tools: search, details, import, shopping list
Readable `is_error` results; shopping list sums per unit and splits specialty items.

### RN-16 · Shared `ImportService` used by REST and MCP
Cover download, WebP conversion, and Mealie import in one place.

### RN-15 · Import endpoints (`/import/url`, `/import/text`, `/import/mealie-backup`)
Verified live against BBC Good Food; covers served at `/images/{id}.webp`.

### RN-14 · Mealie backup reader
Handles the 1.x `database.json` dump and per-recipe JSON exports.

### RN-13 · rig-core structured extraction behind `RecipeExtractor`
Heuristic fallback when `LLM_MODEL` is unset; free-text import returns a clear 503.

### RN-12 · Heuristic ingredient normalizer
Fractions, unicode fractions, kitchen and imperial units, stopword canonical ids.

### RN-11 · JSON-LD recipe parser and HTML sanitizer
`@graph`, arrays, HowToSection, ISO 8601 durations; noise tags stripped before Markdown.

### RN-10 · Embedded UI with SPA fallback
`rust-embed` of `frontend/dist`, correct content types, JSON 404 for unknown API routes.

### RN-9 · Federation manifest and public recipe endpoints

### RN-8 · REST CRUD and search (`/api/v1/recipes`)
Serving scaling, JSON errors, pagination caps.

### RN-7 · Repository layer with FTS5 search
Pantry ranking, tag filter, allergen exclusion, max time, safe FTS tokenization.

### RN-6 · SQLite pool with WAL, NORMAL sync, foreign keys, busy timeout, embedded migrations

### RN-5 · `RecipeInput` DTO, serving scaling, search and manifest types

### RN-4 · Deterministic `content_hash` with golden test

### RN-3 · Domain models with JSON Schema derivations

### RN-2 · Initial migration (recipes, ingredients, FTS5 triggers)

### RN-1 · Cargo workspace with five crates and shared dependencies

## Archived 2026-10-24

### RN-26 · Delete removes the stored cover (S)
`DELETE /api/v1/recipes/{id}` now removes `images/{id}.webp`; covered by a test in `crates/node-server/tests/import_api.rs`.

### RN-23 · Verify LLM extraction against a real provider (M)
Verified 2026-10-21 with `minimax/MiniMax-M3`: free-text import returned metric units, snake_case canonical ids and step timers in ~3 s. Model replies missing fields are now parsed leniently (`parse_recipe_reply`) and schema mismatches surface as 502 instead of 500.

## Archived 2026-10-24

### RN-42 · PWA audit and install flow (S)
The standalone manifest and static-only service worker pass installability requirements. The navigation now exposes an in-app Install control only after the browser supplies `beforeinstallprompt`, then delegates to the browser-native prompt and clears its deferred state.
- Verified 2026-10-22 with `scripts/devbuild.sh`: Lighthouse 11.7 PWA score was `1`, the installability audit passed, the app was service-worker controlled, and the browser console had no errors.

### RN-40 · UI: library pagination controls (S)
The library now uses previous/next controls, visible range/total metadata, and `#/?page=N` routes rather than appending an unbounded list. Searches and filter changes return to page one; out-of-range page hashes recover to the final available page.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: a 30-recipe fixture rendered 12/12/6 cards across pages 1–3, showed the correct ranges, and retained `#/?page=3` on the final page.

### RN-37 · UI: Mealie backup upload (S)
Import dialog now includes a labelled third tab that posts a `.zip` as multipart field `file` to `POST /api/v1/import/mealie-backup`. Its in-page report safely renders imported/failed counts and every returned error, while the library refreshes without closing the report.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: a Mealie-format ZIP imported Browser Mealie Soup with `1 imported` / `0 failed` and a refreshed card; a malformed ZIP displayed the server error inline.

### RN-36 · UI: delete recipe with confirmation (S)
Recipe detail now includes a destructive Delete action backed by a native in-page confirmation dialog—never `confirm()`—that calls `DELETE /api/v1/recipes/{id}` and returns to the library.
- Verified in-browser 2026-10-22 via `scripts/devbuild.sh`: deletion removed the seeded recipe from the rendered library without a document reload; a deliberately stale confirmation received 404 and recovered to the refreshed empty library with an explanatory toast.

### RN-38 · REST endpoint for shopping lists (S, backend)
`POST /api/v1/shopping-list` wraps the MCP aggregation; documented in `docs/MCP_AND_API_SPEC.md`.

### RN-29 · Image upload endpoint (S)
`PUT /api/v1/recipes/{id}/image` (multipart `file`) stores a WebP cover via `ImportService::set_cover`; 400 on non-images, 404 on unknown ids.

### RN-27 · Pagination metadata in search (S)
`GET /api/v1/recipes` returns `total`; `Db::count_recipes` shares the filter builder with search.
