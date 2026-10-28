# MCP Server & REST API Specification

The Standalone Node exposes two primary interfaces:

1. **MCP (Model Context Protocol)** — enables external and local AI agents (Claude Desktop, Cursor, custom assistants) to search, import, and plan meals.
2. **REST API** — powers the embedded Web UI (PWA) and exposes federation endpoints for the Central Aggregator.

---

## 1. MCP Server Specification (`crates/recipe-mcp`)

The MCP server must support two transport modes:
* **Local Transport:** `stdio` (invoked via `node-server mcp-stdio`).
* **Remote Transport:** `SSE` / `Streamable HTTP` mounted at `/mcp` (protected via `Authorization: Bearer <NODE_API_KEY>`).

MCP tools are implemented once in `recipe-mcp` (rmcp `#[tool]` handlers over a shared `ImportService`). A rig-based internal agent consumes them through the `rig-rmcp` crate, which exposes any MCP server's tools as rig tools, so no separate `rig::tool::Tool` adapters are needed.

### MCP Tool 1: `search_recipes`
Searches the local node database by keywords, available pantry ingredients, excluded allergens, and preparation time.

* **Input JSON Schema:**
  ```json
  {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Full-text search query (dish name, cuisine style, or keyword)"
      },
      "include_canonical_ingredients": {
        "type": "array",
        "items": { "type": "string" },
        "description": "List of canonical_id tokens available in the user's fridge/pantry"
      },
      "exclude_canonical_ingredients": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Allergens or disliked canonical_id tokens to exclude"
      },
      "max_total_time_minutes": {
        "type": "integer",
        "description": "Maximum combined prep + cook time in minutes"
      },
      "limit": {
        "type": "integer",
        "default": 5
      }
    }
  }
  ```
* **Output:** Array of recipe summaries (`id`, `title`, `servings`, `total_time_minutes`, `canonical_ingredients`, `missing_ingredients`, `language`, `translation_lang`).
* All three read tools (`search_recipes`, `get_recipe_details`, `build_smart_shopping_list`) accept an optional `language` (BCP-47 code) with the same semantics as the REST `lang` parameter: translated wording where available, localized `canonical_label` on ingredients and shopping lines.

### MCP Tool 2: `get_recipe_details`
Retrieves the complete recipe, dynamically scaling ingredient quantities if `target_servings` is specified.

* **Input Parameters:**
  * `recipe_id` (`string`, required)
  * `target_servings` (`integer`, optional — scales `metric_amount` of all ingredients by `target_servings / recipe.servings`).
  * `language` (`string`, optional — display language; translates wording on demand and adds `canonical_label`).

### MCP Tool 3: `import_recipe`
Imports and normalizes a recipe from a URL or raw unstructured text using the `recipe-scraper` pipeline (`Schema.org` JSON-LD first, rig structured-output extraction as fallback) and persists it to SQLite.

* **Input Parameters:**
  * `source_url` (`string`, optional)
  * `raw_text` (`string`, optional — used when importing from social media captions, chat messages, or OCR text)
  * `photo_paths` (`string[]`, optional — local image files read with the vision model; stdio clients only)
  * `is_public` (`boolean`, default `false`)

### MCP Tool 4: `build_smart_shopping_list`
Takes a list of recipes and desired servings, aggregates identical `canonical_id` items, sums their `metric_amount`, and splits the output into two distinct buckets:
1. `basic_groceries` (`is_specialty == false`) — everyday items for local supermarket client-side cart injection.
2. `specialty_items` (`is_specialty == true`) — rare/ethnic items ready for online delivery search.

* **Input JSON Schema:**
  ```json
  {
    "type": "object",
    "required": ["items"],
    "properties": {
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["recipe_id", "servings"],
          "properties": {
            "recipe_id": { "type": "string" },
            "servings": { "type": "integer" }
          }
        }
      },
      "pantry_exclude_canonical_ids": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Canonical IDs already in stock at home (e.g., salt, water, black_pepper, olive_oil)"
      }
    }
  }
  ```

---

## 2. Internal REST API (`crates/node-server`)

All endpoints return `application/json`.

### Authentication: owner vs. public reader
The node has one owner, identified by `NODE_API_KEY`. A request proves ownership with
`Authorization: Bearer <key>` or with the `node_session` cookie (HttpOnly, SameSite=Lax,
Path=/, holds a digest of the key, never the key itself) set by:

* `POST /api/v1/auth/login` — body `{ "api_key": "..." }`; `200 {"authenticated": true}` + cookie, or `401`.
* `POST /api/v1/auth/logout` — clears the cookie.
* `GET /api/v1/auth/me` — `{ "authenticated": bool }`, never fails.
* Planned (RN-73, `docs/SPEC-hosted-platform.md` §4.6): `GET /api/v1/auth/magic?token=<expires>.<nonce>.<mac>` — signed, single-use login link valid for 10 minutes (`mac = HMAC-SHA256(NODE_API_KEY, "chefsprodigy-magic:" + expires + ":" + nonce)`); sets the `node_session` cookie exactly like `login` and redirects to `/#/`; `401` JSON when expired, reused or invalid. Owner-only `POST /api/v1/auth/magic-link` → `{ "url", "expires_at" }` mints one for another device; `hubctl login-link` and the platform provisioner mint them from the hub's `Secret` in `chefs-hubs`. `NODE_API_KEY` stays the master secret; magic links never rotate it.

Rules:
* **Owner only** (`401` otherwise): creating, updating and deleting recipes, cover upload, translation `PUT`/`DELETE`, and every `/api/v1/import/*` endpoint.
* **Everyone** can read, but anonymous callers only see recipes with `is_public = true`: private ones are excluded from listings, counts and shopping lists and answer `404` on `GET /recipes/{id}`.
* **Model spend is the owner's**: anonymous readers asking for `?lang=` get cached translations and labels only; a live translation is never triggered for them.
* **Single-user mode**: without `NODE_API_KEY` every caller is the owner (the server logs a warning). `/mcp` keeps its own bearer check with the same key.

### Recipes & Search
* `GET /api/v1/recipes?q=&tag=&ingredient=&limit=20&offset=0&lang=ru` — Search via FTS5 and filters. `q` matches the source text **and** stored translations (one row per recipe, best rank). With `lang` (or `Accept-Language`), summaries use an existing translation for `title`/`tags` and carry `translation_lang`; listing never triggers a translation. Returns `{ "items": [RecipeSummary], "limit", "offset", "total" }` where `total` counts all matches across pages.
* `GET /api/v1/recipes/{id}?servings=4&lang=ru` — Fetch full recipe (with dynamic serving scaling). `lang` (or `Accept-Language` when absent) selects the display language: wording is translated on first request by the configured model and cached; the response adds `language` (source) and `translation: {lang, origin: llm|human, stale, translated_at}` when a translation was applied. `lang=source` disables translation. Without a model the source wording is returned and `translation` is absent. Design: `docs/SPEC-i18n.md`.
* `PUT /api/v1/recipes/{id}/translations/{lang}` — Store a human translation (`{title, description, ingredients[], instructions[], tags[]}`, arrays in recipe order). Human translations are kept when the source changes (flagged `stale`). `DELETE` drops a translation so it is regenerated.
* `GET /api/v1/i18n/languages` — `{ "ui": ["en","ru","nl"], "content_translation": bool }`.
* `POST /api/v1/recipes` — Create a recipe manually. Recipes carry an optional `language` (BCP-47 primary subtag, e.g. `nl`), detected on import or set here.
* `PUT /api/v1/recipes/{id}` — Update a recipe (automatically recalculates `content_hash`).
* `DELETE /api/v1/recipes/{id}` — Delete a recipe (and its stored cover image).
* `PUT /api/v1/recipes/{id}/image` — `multipart/form-data` with an image in field `file`; converted to WebP (max 1200 px) and stored as the cover. Returns the updated recipe. `400` for non-image data, `404` for an unknown recipe.

### Shopping list
* `POST /api/v1/shopping-list?lang=ru` — Same contract as the `build_smart_shopping_list` MCP tool. With `lang` (or `Accept-Language`) every line carries `canonical_label`, the ingredient name in that language (cached in `canonical_terms`, fetched from the model in batches of 100, humanized id as fallback). Body: `{ "items": [{ "recipe_id", "servings" }], "pantry_exclude_canonical_ids": [] }`. Response: `{ "basic_groceries": [line], "specialty_items": [line], "missing_recipes": [id] }` with `line = { canonical_id, total_amount, unit, is_specialty, recipe_count, raw_examples }`. `400` when `items` is empty.

### Import & Migration
* `POST /api/v1/import/url` — Body: `{ "url": "https://...", "is_public": true }`. Runs `recipe-scraper`. URLs that are not `http`/`https` or that point (directly, through DNS or through a redirect) at loopback, private, link-local/cloud-metadata, multicast or unspecified addresses are refused with `400` and an error starting `refusing to fetch` (SSRF guard, RN-74); unreachable public sites answer `502`.
* `POST /api/v1/import/text` — Body: `{ "text": "...", "origin_url": null }`. Runs the rig structured-output extractor; returns `503` with a hint when `LLM_MODEL` is unset.
* `POST /api/v1/import/mealie-backup` — `multipart/form-data` accepting a Mealie `.zip` archive. Returns `{ "imported_count": 42, "failed_count": 0, "errors": [] }`.
* `POST /api/v1/import/photo` — `multipart/form-data` with 1–6 image files in field `photos` (repeatable) and an optional `is_public` text field. Photos are oriented, downscaled to 1600 px and sent to the vision model in one call; the first photo showing the dish becomes the cover. Returns `201` with the recipe. Errors: `400` (no/too many photos, not an image), `422` (no recipe found in the photos), `502` (model failure), `503` (model cannot read images or no `LLM_MODEL`). Design: `docs/SPEC-photo-import.md`.
* Model limits (RN-67): when the model gateway or the vendor refuses a call with `429` or `402` (hub quota used up, vendor rate limit, billing), the imports that need the model (`/import/text`, `/import/url` for a page without JSON-LD, `/import/photo`) answer `429` with `{ "error": "model quota exceeded: …", "code": "model_quota_exceeded" }`; the message names the limit and carries the endpoint's own explanation. JSON-LD imports keep working: their ingredients are normalized locally (heuristic `canonical_id`s) while the model refuses. Recipe reads keep working too: a live translation that is refused serves the source wording, and cached translations are unaffected. MCP tools report the same sentence as a tool error.

### Federation Endpoints (For Central Aggregator Sync)
* `GET /api/v1/federation/manifest` — Public endpoint exposing node metadata (`author_did`, `chef_name`, `node_url` taken from `NODE_PUBLIC_URL`, `null` when unset) and the public recipes (`is_public = 1`) as `{ id, updated_at, content_hash, translations }` for delta-syncing.
  `translations` lists every stored translation of the recipe as `{ lang, origin, stale, updated_at }`. Translations carry their own `updated_at` because saving one never changes the recipe's `updated_at` or `content_hash`, so an aggregator compares `(id, updated_at, content_hash)` and every `(lang, updated_at)` pair (`docs/SPEC-hosted-platform.md` §4.4); `stale` is true once the recipe changed after the translation was made. Private recipes and their translations never appear.
* `GET /api/v1/federation/recipes/{id}` — Returns the full canonical JSON of a public recipe (source language, flattened) plus `translations: [RecipeTranslation]` with every stored translation, LLM or human, each `{ lang, title, description, ingredients: [raw_text…], instructions: [text…], tags, origin, source_updated_at, created_at, updated_at }`. Private recipes answer `404`.