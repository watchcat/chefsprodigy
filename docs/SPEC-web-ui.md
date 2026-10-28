# Spec: Embedded Web UI

## Objective

Build the first complete browser interface for the standalone recipe node. The UI will be a responsive, dependency-free PWA shell embedded in the Rust binary from `frontend/dist`. It lets a household cook read the interface in English, Russian, or Dutch, discover locally stored recipes, filter the collection, import recipes from links, text, backups, or photographs, author recipes, refine every saved recipe field and its cover image, review a recipe at a chosen serving size, combine selected recipes into a grocery list, delete an unwanted recipe safely, and follow its steps in an uncluttered Cook Mode with optional timers.

The intended user is the owner of a self-hosted recipe node. The UI communicates only with the same-origin REST API documented in `docs/MCP_AND_API_SPEC.md`; it does not require a cloud account, telemetry, third-party font/CDN, or a new backend endpoint.

### Included capabilities

- **Recipe library:** fetch and paginate `GET /api/v1/recipes`; full-text search plus tag and canonical-ingredient filters; responsive cards; total/range metadata and previous/next page controls backed by `#/?page=N`; useful empty, loading, and error states.
- **Recipe import:** an accessible modal for URL imports (`POST /api/v1/import/url`), pasted/free-form-text imports (`POST /api/v1/import/text`), Mealie `.zip` backup uploads (`POST /api/v1/import/mealie-backup`), and camera-friendly multi-photo imports (`POST /api/v1/import/photo`). The photo picker accepts 1–6 images, previews its selection locally, explains the configured vision-model disclosure, and reports 422/503 responses inline before opening a successfully created recipe. Before photo upload, the browser bounds each selected image to a 1600 px long edge and recompresses when needed to at most 12 MB per image, reporting preparation progress and the aggregate prepared size so a six-photo batch remains below Cloudflare's 100 MB cap.
- **Manual recipe editor:** accessible create (`#/new`) and edit (`#/recipe/{id}/edit`) views for title, description, servings, timings, comma-separated tags, provenance, public visibility, normalized ingredients, and ordered timed steps. The editor saves through `POST`/`PUT /api/v1/recipes` and renders server errors inline.
- **Cover image upload:** the editor accepts JPEG, PNG, or WebP and sends selected files as multipart field `file` to `PUT /api/v1/recipes/{id}/image` after a recipe ID exists; the returned WebP cover is rendered immediately in recipe detail and cards.
- **Recipe detail:** fetch `GET /api/v1/recipes/{id}`; show description, tags, timing, cover image when present, normalized ingredients, and ordered instructions. A native-dialog confirmation deletes a recipe through `DELETE /api/v1/recipes/{id}` and recovers gracefully when it was already removed.
- **Serving scaling:** request a server-scaled recipe through `?servings=` when the user changes the serving count.
- **Cook Mode:** one-step-at-a-time layout with previous/next navigation, progress, optional per-step countdown timers, and a best-effort Screen Wake Lock while enabled.
- **Shopping list builder:** `#/shopping` fetches the local recipe library, lets a cook select recipes and requested servings, accepts optional comma-separated pantry exclusions, then posts `{items, pantry_exclude_canonical_ids}` to `POST /api/v1/shopping-list`. It renders aggregated basic groceries and specialty items as independent accessible checklists; checked state, selected recipes, and pantry text are local browser preferences.
- **Interface language:** header language control for English (`en`), Russian (`ru`), and Dutch (`nl`), persisted at `localStorage["ui.lang"]`. Complete static message bundles live in `frontend/dist/i18n`; the bundle loader sets `<html lang>`, falls back to English, and applies locale-aware `Intl` formatting for amounts, units, timer digits, and durations.
- **Localized recipe content:** the selected UI language is supplied as `lang` on library, detail, and shopping-list calls. Translated cards and detail views use the localized API fields and `canonical_label` when available. Recipe detail displays translated-source provenance, supports toggling to the original wording, shows a stale-source hint for retained human corrections, and provides a dedicated `#/recipe/{id}/translation` correction form that saves ordered presentation fields through `PUT /api/v1/recipes/{id}/translations/{lang}`.
- **Owner and visitor modes:** on startup, the PWA calls `GET /api/v1/auth/me`. Anonymous visitors browse public recipes and build shopping lists but never see write controls; they can open a native owner-key dialog that posts `{api_key}` to `POST /api/v1/auth/login`, uses the returned HttpOnly session cookie, and offers `POST /api/v1/auth/logout`. Write-only routes are guarded, real `401` responses reopen the login dialog, and visitors see a non-spend translation hint when a requested display language has not been cached by the owner.
- **PWA shell:** application metadata, a lightweight service worker that caches the static shell only, client-side hash routes, and a progressive in-app install control when the browser emits `beforeinstallprompt`.

### Excluded from this initial UI

- Federation configuration, a persistent pantry inventory, and remote/cloud sync. The documented APIs remain unchanged and available for a later UI increment.
- Full offline recipe data access. The static app shell may load while offline, but recipe data is always read from the local node API.
- Any visual asset not present in the repository. The embedded local PNG icon set is used for the app mark and manifest without external asset requests.

## Tech Stack

- **Runtime UI:** standards-based HTML, CSS, and modern browser JavaScript (ES2022), with no framework or package dependency.
- **Distribution:** `frontend/dist/index.html`, `frontend/dist/app.js`, `frontend/dist/styles.css`, `frontend/dist/i18n/{en,ru,nl}.json`, `frontend/dist/manifest.webmanifest`, and `frontend/dist/sw.js`.
- **Hosting:** Axum plus `rust-embed` as implemented in `crates/node-server/src/ui.rs`. The browser app is same-origin and uses relative `/api/v1/...` URLs.
- **Browser features:** Fetch API, `FormData`, File/Blob object URLs for local previews, History/hash routing, Web Storage for non-sensitive UI preferences including shopping-list checklist state, Service Worker API, `beforeinstallprompt`, and Screen Wake Lock API (all progressively enhanced).

The no-build design deliberately keeps this embedded binary small and preserves the project goal of an ultra-lightweight standalone node.

## Commands

```sh
# Start the node and serve the embedded frontend
cargo run -p node-server -- serve --port 8080

# Compile every Rust target (also validates rust-embed can include frontend/dist)
cargo check --workspace --all-targets

# Run project tests
cargo test --workspace

# Run the strict Rust linter
cargo clippy --workspace --all-targets -- -D warnings

# Validate JavaScript syntax
node --check frontend/dist/app.js

# Manually inspect the UI
open http://127.0.0.1:8080/
```

## Project Structure

```text
frontend/
└── dist/                         # Static assets embedded by rust-embed
    ├── index.html                # Semantic shell, dialogs, templates, metadata
    ├── styles.css                # Responsive visual system and component states
    ├── app.js                    # API client, state/rendering, import/cook interactions
    ├── manifest.webmanifest      # PWA application metadata
    └── sw.js                     # Cache-first static-shell service worker

docs/
└── SPEC-web-ui.md                # This approved UI scope

crates/node-server/src/ui.rs       # Existing embedded-asset handler; no route changes expected
```

## Code Style

Use the smallest native primitives that preserve clarity: semantic HTML, CSS custom properties, focused JavaScript functions, and predictable DOM `data-*` hooks. API data must always be rendered using DOM node creation or `textContent`, never concatenated into HTML, so imported recipe content cannot introduce browser markup.

```js
async function requestRecipe(id, servings) {
  const params = new URLSearchParams();
  if (servings) params.set("servings", String(servings));

  const response = await fetch(`/api/v1/recipes/${encodeURIComponent(id)}?${params}`);
  if (!response.ok) throw await apiError(response);
  return response.json();
}

function textElement(tagName, value, className) {
  const element = document.createElement(tagName);
  element.textContent = value;
  if (className) element.className = className;
  return element;
}
```

- Use `camelCase` for JavaScript variables/functions, `PascalCase` only for constructor-like data classes when needed, and kebab-case for CSS classes/data attributes.
- Keep application state in one explicit `state` object. Route parsing and API calls must be separate from renderer functions.
- Use `async`/`await`, test `response.ok`, and show useful server-provided error messages without exposing stack traces.
- Never interpolate imported titles, descriptions, tags, ingredients, or instructions using `innerHTML`.
- Prefer CSS Grid/Flexbox and logical properties; support a small screen first and enhance at tablet/desktop widths.
- Respect `prefers-reduced-motion`, sufficient contrast, visible keyboard focus, labelled controls, and semantic dialogs.

## Testing Strategy

1. **Static validation:** run `node --check frontend/dist/app.js` after every JavaScript change.
2. **Embedded build validation:** run `cargo check --workspace --all-targets` to confirm the Rust server can package all frontend assets.
3. **Existing backend regression:** run `cargo test --workspace`; UI work must not alter REST contracts or API test outcomes.
4. **Manual browser acceptance:** with the server running, exercise the states below against an empty database and after importing a sample recipe:
   - Library starts with a friendly empty state.
   - Search and filters produce a new request and cards link to details.
   - A 30-recipe collection pages in 12-item slices; the visible range/total and `#/?page=N` stay synchronized as the user moves between pages.
   - URL/text import submits once, disables duplicate submission, reports a result, and navigates to the new recipe.
   - A Mealie backup uploads as multipart field `file`, renders imported/failed counts plus any returned error list, and refreshes the library.
   - The photo tab offers a camera-capable 1–6 file picker, local thumbnails, its privacy disclosure, and a visible extraction-progress state; it posts repeated multipart `photos` plus `is_public`, then opens the resulting recipe. A no-recipe response (422) and unsupported/no-model response (503) remain inline and actionable.
   - A manual recipe creates with all documented JSON fields, dynamic ingredient/step rows, and inline validation feedback; editing the seeded full recipe round-trips its normalized ingredients, specialty flags, and timed/untimed steps.
   - A selected JPEG, PNG, or WebP cover uploads after save and reappears as the server-converted WebP cover in the detail view and library card.
   - The shopping builder selects two recipes, scales their servings, excludes a comma-separated pantry item, and renders server-aggregated everyday and specialty buckets. Checking an item remains checked after rebuilding the list and reloading the browser.
   - The header language control switches between English, Russian, and Dutch; each switch updates `<html lang>`, survives a reload, and leaves the English interface unchanged when selected again.
   - A Dutch recipe with a stored Russian translation appears as a Russian library card and detail view; the detail view can show the original wording, save a human correction, retain it through reload, and show the stale-source hint after the source recipe changes.
   - With `NODE_API_KEY` configured, an anonymous browser sees public recipes only, no create/edit/delete/import/correction controls, and the visitor cached-translation hint. The native login dialog accepts the owner key, shows all controls and private recipes after login, keeps the cookie through reload, and returns to public read-only mode after logout; a direct protected hash route and a real `401` both offer the login prompt.
   - Recipe detail shows the response accurately; serving changes produce scaled ingredient values.
   - Recipe deletion uses a native in-page confirmation, returns to the refreshed library without a document reload, and treats an already-deleted recipe as a normal recovery path.
   - Cook Mode advances steps, runs/pause/resets a timer when supplied, and never prevents navigation if Wake Lock is unavailable.
   - Layout remains usable at 320px, 768px, and desktop width using browser responsive emulation.
5. **PWA regression:** reload after the static shell has been cached; the application shell should load and show a normal connection/data error rather than a blank page when the node API is unavailable. Confirm Lighthouse installability and that an in-app install control appears only when `beforeinstallprompt` is available.

## Boundaries

- **Always:** use only documented same-origin REST endpoints; preserve the small embedded UI approach; provide keyboard-accessible controls and state feedback; disclose that selected recipe photos are sent to the configured vision model and are not retained; run the listed JavaScript/Rust validations; preserve the existing Axum fallback route.
- **Ask first:** adding JavaScript frameworks or npm dependencies, adding/changing API endpoints or Rust models, altering database schema, adding external services/CDNs/fonts, implementing other migration UI, or changing asset-embedding behaviour.
- **Never:** send recipe/pantry data to third parties; add analytics; commit secrets; use unsanitized `innerHTML` with API content; require a network connection for static asset rendering; modify generated `target/` files.

## Success Criteria

1. `GET /` renders a polished Chef's Prodigy library UI instead of the current placeholder and has no console errors.
2. The UI is fully embedded in `node-server` and loads without a Node/npm process or a CDN at runtime.
3. A user can search/filter the local library, move through server-backed recipe pages with a stable `#/?page=N` hash, open a recipe, alter servings, and see its ingredients and instructions.
4. A user can successfully import a recipe from a valid URL, raw text, Mealie backup, or 1–6 photos through the documented REST endpoints and receive actionable feedback on failure; backup reports show imported/failed counts and returned errors, while photo import offers previews, progress feedback, the vision-model privacy disclosure, and visible 422/503 messages.
5. A user can create a recipe or edit any saved recipe with its title, description, servings, times, tags, normalized ingredients, specialty flags, ordered steps, and optional timers; API failures are visible inline and no untrusted content is rendered as HTML.
6. A user can select a valid cover image in the editor and see the server-converted WebP cover in the returned recipe detail and library card.
7. A user can delete a recipe only after a native in-page confirmation; success and stale 404 responses both return to a refreshed library without a full-page reload.
8. Cook Mode displays one instruction at a time, tracks progress, supports any extracted timer, and uses Wake Lock only when supported and permitted.
9. The interface is usable by keyboard and on small mobile screens, with appropriate empty/loading/error feedback; when supported, it offers the browser's native PWA install prompt without blocking normal use.
10. A user can select recipes and servings in `#/shopping`, exclude optional pantry items, and receive correctly aggregated basic and specialty grocery buckets; independently checked grocery rows persist locally across a reload and rebuilt list.
11. A user can switch the static interface between English, Russian, and Dutch without a backend dependency; the chosen language persists locally, the document language follows it, and locale-aware units/numbers/durations are used.
12. A selected display language is supplied to library, detail, and shopping requests. Translated content identifies its source and origin, can reveal the source wording, and allows a human correction to persist in the displayed language after reload and a source-recipe update.
13. With owner authentication configured, visitors can read only public recipes and cannot access write controls, while a successful native-dialog login persists through reload, restores all owner controls and private recipes, and can be cleanly logged out back to visitor mode.
14. `scripts/i18n-check.py`, `node --check frontend/dist/app.js`, `cargo check --workspace --all-targets`, and `cargo test --workspace` pass.
