# Spec: Multiple languages (UI and recipe content)

## Objective
A household may read the interface in English, Russian or Dutch and keep recipes that
arrived in any language. The UI is fully translated; recipe content is translated on
demand by the configured LLM, cached, searchable, and correctable by hand.

Decisions taken (2026-10-24): ship `en`, `ru`, `nl` first; translate recipe content
lazily on first view and cache it; index translations for search; human corrections are
stored and never overwritten by regeneration.

## Vocabulary
- **Source language**: the language a recipe was written in (`recipes.language`,
  BCP-47 primary subtag such as `ru`; `NULL` when unknown).
- **Display language**: what the viewer asked for (`?lang=` or `Accept-Language`,
  overridden by the UI setting).
- **Translation**: the presentation fields of one recipe in one language. Never the
  normalization layer: `canonical_id`, `metric_amount`, `unit`, `content_hash` and ids
  are language-independent and are not translated.

## Data model (`migrations/<timestamp>_i18n.sql`)
```sql
ALTER TABLE recipes ADD COLUMN language TEXT;            -- e.g. 'ru', NULL = unknown

CREATE TABLE recipe_translations (
    recipe_id   TEXT NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    lang        TEXT NOT NULL,
    title       TEXT NOT NULL,
    description TEXT,
    ingredients TEXT NOT NULL,   -- JSON array of raw_text, same length/order as recipe_ingredients
    instructions TEXT NOT NULL,  -- JSON array of step text, same length/order as recipe.instructions
    tags        TEXT NOT NULL,   -- JSON array
    origin      TEXT NOT NULL CHECK (origin IN ('llm','human')),
    source_updated_at TEXT NOT NULL,   -- recipes.updated_at when this translation was made
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (recipe_id, lang)
);

CREATE TABLE canonical_terms (            -- localized labels for canonical ingredient ids
    canonical_id TEXT NOT NULL,
    lang         TEXT NOT NULL,
    label        TEXT NOT NULL,
    origin       TEXT NOT NULL CHECK (origin IN ('llm','human')),
    PRIMARY KEY (canonical_id, lang)
);

CREATE VIRTUAL TABLE translations_fts USING fts5(
    recipe_id UNINDEXED, lang UNINDEXED, title, description, ingredients_raw, tags
);
-- triggers keep translations_fts in sync with recipe_translations (insert/update/delete)
```
A translation is **stale** when `recipes.updated_at > source_updated_at`. Stale `llm`
rows are regenerated on next request; stale `human` rows are kept and reported as stale.

## Source language detection
- Photo and free-text imports: `ExtractedRecipeDraft` carries `language` (the LLM reports
  it). URL imports: JSON-LD `inLanguage`, else `<html lang>`, else the LLM draft. Every
  import then runs the offline detector (`recipe-scraper/src/langdetect.rs`: script signals
  for Cyrillic, stopword scoring for en/nl/de/fr/es/it) when the language is still unknown;
  thin evidence leaves `NULL` rather than guessing.
- Backfill: `node-server backfill-language` and a background pass at `serve` startup detect
  the language of recipes that lack one (heuristic first, model only when unsure) and store
  it without touching `updated_at` or the hash.
- The editor (RN-35) gets a language selector so the user can set or fix it.

## Translation service (`recipe-mcp::TranslationService`, LLM calls in `recipe-scraper::llm`)
`translate(recipe, target_lang) -> RecipeTranslation`

1. Return the cached row if present and not stale (or stale but `human`).
2. Otherwise build one structured request: the source strings as indexed JSON
   (`title`, `description`, `ingredients[i].raw_text`, `instructions[i].text`, `tags`),
   target language, and a preamble: translate for a home cook, keep quantities, product
   names and brand names as written, keep the same number and order of items, do not add
   or merge steps. Reply schema mirrors the input with indices; parsing is lenient
   (reuse the reply-reader style of `parse_recipe_reply`) and validates lengths. A length
   mismatch is a `TranslationError::Incomplete` and falls back to the source language.
3. Store as `origin = 'llm'`. One request per recipe and language; concurrent viewers
   share an in-flight future per `(recipe_id, lang)` so a recipe is never translated twice.

`translate_terms(canonical_ids, lang)` batches up to 100 ids per call into
`canonical_terms`; missing labels fall back to the id with underscores replaced by spaces.
Everyday ids come pre-seeded in `en`, `ru` and `nl` as `origin = 'human'` rows (RN-75), which a
model label never overwrites.

Without `LLM_MODEL`, translation requests answer with the source language and
`"translation": null`; nothing errors.

## API
- `GET /api/v1/recipes/{id}?lang=ru&servings=4`: `Accept-Language` is used when `lang`
  is absent; `lang=source` disables translation. Response adds
  ```json
  "language": "nl",
  "translation": { "lang": "ru", "origin": "llm", "stale": false, "translated_at": "..." }
  ```
  and the presentation fields carry the translated text. `raw_text` on each ingredient is
  translated; `canonical_label` is added with the localized term.
- `GET /api/v1/recipes?lang=ru&q=...`: summaries carry translated titles/tags when a
  translation already exists (no translation is triggered by listing); `q` is matched
  against `recipes_fts` **and** `translations_fts`, results de-duplicated by recipe id
  with the best rank.
- `PUT /api/v1/recipes/{id}/translations/{lang}`: body = translation fields; stored as
  `origin='human'`. `DELETE` drops it so the next view regenerates.
- `POST /api/v1/shopping-list?lang=ru`: lines carry `canonical_label` in that language.
- `GET /api/v1/i18n/languages`: `["en","ru","nl"]` for UI bundles plus whether content
  translation is available (`llm` configured).

## MCP
`search_recipes`, `get_recipe_details`, `build_smart_shopping_list` gain an optional
`language` argument with the same semantics as `?lang=`.

## Frontend
- Message bundles `frontend/dist/i18n/{en,ru,nl}.json`, flat keys (`library.title`),
  ICU-free: plurals via `Intl.PluralRules`, numbers and durations via `Intl.NumberFormat`
  / `Intl.RelativeTimeFormat`. `t(key, vars)` helper; missing key falls back to `en` and
  logs once. `<html lang>` follows the UI language.
- Language choice: `localStorage["ui.lang"]`, default from `navigator.languages`
  filtered to the shipped set. A switcher in the header; the same setting drives the
  `lang` query for content.
- Recipe page: "Translated from Dutch · machine translation" badge with **Show
  original** toggle; a loading state during the first translation (a few seconds);
  **Fix translation** opens the editor fields for the translated text and saves via the
  `PUT` endpoint; stale human translations show a "source changed" hint.
- Units display localized labels (`g/ml/pcs` → `г/мл/шт`), ingredient names use
  `canonical_label` in the shopping list.
- All UI strings must come from the bundle; a check script
  (`scripts/i18n-check.py`) fails when `en.json` and another bundle diverge in keys or
  when `app.js` contains a quoted English sentence outside `t()`.

## Limits and privacy
Translation text goes to the LLM vendor like imports do. Target languages for content
are not limited to the UI set: any two-letter code is accepted. Cache size: one row per
recipe per language; no expiry besides staleness.

## Testing
- Migration and repository tests: cascade on recipe delete, staleness, human rows
  survive regeneration, FTS sync for translations.
- Service tests with a fake extractor: one call per `(recipe, lang)`, length mismatch
  falls back, concurrent requests coalesce.
- API tests: `lang` and `Accept-Language` precedence, `lang=source`, search across
  translations, human `PUT` kept after source edit (stale flag).
- Frontend: bundle key parity script; manual pass in `ru` and `nl` at 375 px.

## Out of scope for the first version
Right-to-left layouts, translating free text in MCP tool outputs, per-user language
preferences on the server (the UI setting is per device), and translating recipes that
have no configured LLM.
