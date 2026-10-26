# Frontend brief for implementing agents

Read this before picking up any `UI:` story from `docs/KANBAN.md`.

## What you are working on
A no-build, dependency-free PWA in `frontend/dist/` (`index.html`, `app.js`, `styles.css`,
`manifest.webmanifest`, `sw.js`, `assets/`). There is no bundler and no `frontend/src`:
`frontend/dist` **is** the source. Product and scope rules are in `docs/SPEC-web-ui.md`.
Keep it that way: plain ES2022, no framework, no CDN, no npm.

The files are embedded into the Rust binary at build time by `rust-embed`
(`crates/node-server/src/ui.rs`), so every change needs a rebuild to show up.

## Run and check
```sh
scripts/devbuild.sh              # builds, starts http://127.0.0.1:8080, opens the browser
scripts/devbuild.sh --fresh      # same, but wipes ./data and seeds examples/sample-recipe.json
node --check frontend/dist/app.js
python3 scripts/i18n-check.py    # bundle parity and no hard-coded UI strings
cargo test --workspace           # API behaviour the UI relies on is covered here
cargo clippy --workspace --all-targets -- -D warnings
```
Settings and secrets come from `.env` (template `.env.example`). Without `LLM_MODEL`,
text import answers 503 with a message the UI must show; URL import still works.

## API you talk to (same origin, `/api/v1`)
Full spec: `docs/MCP_AND_API_SPEC.md`. Quick map:

| Need | Call |
|---|---|
| list / search | `GET /recipes?q=&tag=&ingredient=&limit=&offset=&lang=` → `{items, limit, offset, total}`; `q` also matches translations; with `lang` items carry translated `title`/`tags` and `translation_lang` when a translation exists |
| one recipe (scaled, translated) | `GET /recipes/{id}?servings=N&lang=ru` → recipe shape plus `language` (source) and optional `translation {lang, origin, stale, translated_at}`; `lang=source` for the original; `PUT/DELETE /recipes/{id}/translations/{lang}` for human corrections; with `lang` every ingredient has `canonical_label`; `GET /i18n/languages` lists UI languages and whether content translation is on |
| create / update / delete | `POST /recipes`, `PUT /recipes/{id}`, `DELETE /recipes/{id}` (204) |
| import | `POST /import/url {url,is_public}`, `POST /import/text {text,origin_url,is_public}`, `POST /import/mealie-backup` (multipart `file`), `POST /import/photo` (multipart `photos` ×1–6 + `is_public` text; 422 = no recipe found, 503 = model cannot see images) |
| covers | `image_path` is relative, e.g. `images/<id>.webp`, served at `/images/<id>.webp`; upload with `PUT /recipes/{id}/image` (multipart `file`) |
| shopping list | `POST /shopping-list?lang=ru {items:[{recipe_id,servings}], pantry_exclude_canonical_ids:[]}` → `{basic_groceries, specialty_items, missing_recipes}`; with `lang` each line has `canonical_label` |
| auth | `POST /auth/login {api_key}` sets the `node_session` cookie; `POST /auth/logout`; `GET /auth/me` → `{authenticated}`. Writes, imports and live translations need the owner; anonymous visitors see public recipes only and cached translations only. Without `NODE_API_KEY` everyone is the owner. Show a login affordance when `/auth/me` is false and hide write actions |
| errors | always JSON `{"error": "..."}` with 400/401/404/502/503; render the message |

Payload shape for create/update: `examples/sample-recipe.json`. Units are exactly
`g`, `ml`, `pcs`, `to_taste`; `metric_amount` is `null` for `to_taste`.

## Conventions in the existing code
- Routing is hash-based (`#/`, `#/recipe/{id}`, `#/recipe/{id}/cook`); views are built
  in `app.js` with `<template>` elements from `index.html` and rendered via `setView()`.
- All API calls go through the `request()` helper in `app.js`; reuse it.
- Design tokens live at the top of `styles.css`; keyboard focus uses `:focus-visible`.
  The `<main>` landmark receives focus on every route change (accessibility) and must
  keep `outline: none`.
- Dialogs are native `<dialog>` elements; never use `alert/confirm/prompt`.
- Mobile first: everything must work at 375 px with no horizontal scroll.
- The service worker caches only the static shell; bump `CACHE_NAME` in `sw.js` when
  you add or rename a static file, and add it to `APP_SHELL`.
- The UI is multilingual (`docs/SPEC-i18n.md`, RN-58/RN-59): every user-visible string
  goes through `t(key)` and lives in `frontend/dist/i18n/{en,ru,nl}.json`. Add new keys to
  all three bundles (English is the fallback) and run `python3 scripts/i18n-check.py`
  before finishing a story. The header switcher persists `localStorage["ui.lang"]` and
  sets `<html lang>`; send that language as `lang` on content requests so titles, tags and
  labels arrive translated. Recipes themselves can be any language, so never assume ASCII.

## Verifying a story
Start the node, exercise the feature in a real browser, and check the server log in the
terminal for 4xx/5xx. Then update the story's block in `docs/KANBAN.md` with what you
verified and move it to `Done` (or `Review` if you want a second pair of eyes). The live
board (`python3 scripts/kanban.py serve`) lets you drag it instead.
