# Data Architecture, Rust Models & Scraper Pipeline

This document defines the exact data structures and storage contracts for `recipe-core`, `recipe-db`, and `recipe-scraper`.

---

## 1. Domain Models (`crates/recipe-core`)

All core structs must derive `Serialize`, `Deserialize`, and `schemars::JsonSchema` (required for rig structured outputs and MCP tool schema generation). Use English doc-comments (`///`) as they are directly injected into the JSON Schema for the LLM.

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Standardized measurement unit for normalized ingredients
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementUnit {
    G,
    Ml,
    Pcs,
    ToTaste,
}

/// 3-Tier Ingredient Model
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IngredientItem {
    /// Tier 1: Original human-readable text from the recipe (e.g., "2 tbsp dark soy sauce (preferably Pearl River Bridge)")
    pub raw_text: String,
    /// Tier 2: Normalized quantity converted to base metric units (grams, milliliters, or pieces)
    pub metric_amount: Option<f32>,
    /// Tier 2: Standardized metric unit ("g", "ml", "pcs", "to_taste")
    pub unit: MeasurementUnit,
    /// Tier 3: Canonical English snake_case token for shopping & matching (e.g., "dark_soy_sauce", "garlic")
    pub canonical_id: String,
    /// Tier 3: True if this is an ethnic, rare, or specialty ingredient not found in basic convenience stores
    pub is_specialty: bool,
    /// Display label for `canonical_id` in the viewer's language; filled by the node from
    /// `canonical_terms`, never by the model (skipped in the LLM schema, omitted when None)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub canonical_label: Option<String>,
}

/// Single cooking instruction step
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstructionStep {
    pub step_number: u16,
    pub text: String,
    /// Optional extracted timer duration in seconds for interactive Cook Mode timers
    pub timer_seconds: Option<u32>,
}

/// Full Recipe Entity (used across API, DB, and Federation)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Recipe {
    pub id: String, // ULID
    pub title: String,
    pub description: Option<String>,
    pub servings: u8,
    pub prep_time_minutes: Option<u16>,
    pub cook_time_minutes: Option<u16>,
    pub ingredients: Vec<IngredientItem>,
    pub instructions: Vec<InstructionStep>,
    pub tags: Vec<String>,
    pub image_path: Option<String>,
    
    // Federation & provenance metadata
    pub origin_url: Option<String>,
    pub author_did: Option<String>,
    pub content_hash: String,
    pub is_public: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Source language of the recipe text (BCP-47 primary subtag such as "nl"); None when unknown.
    /// Set at import (JSON-LD `inLanguage`, `<html lang>`, the offline detector, or the model)
    /// and by `backfill-language`; never part of `content_hash`.
    #[serde(default)]
    pub language: Option<String>,
}
```

### Translations (`docs/SPEC-i18n.md`, shipped in RN-53..RN-57)

Translations are stored next to the recipe, one row per language, and never change the
recipe's `updated_at` or `content_hash`; a translation is **stale** once
`source_updated_at != recipe.updated_at`. Machine translations are regenerated when stale,
human corrections are kept and flagged. Derives abridged; see `crates/recipe-core/src/dto.rs`.

```rust
/// Who produced a translation
#[serde(rename_all = "snake_case")]
pub enum TranslationOrigin { Llm, Human }

/// The presentation fields of one recipe in one language (stored row)
pub struct RecipeTranslation {
    pub lang: String,
    pub title: String,
    pub description: Option<String>,
    /// Translated `raw_text` per ingredient, in recipe order
    pub ingredients: Vec<String>,
    /// Translated step text, in recipe order
    pub instructions: Vec<String>,
    pub tags: Vec<String>,
    pub origin: TranslationOrigin,
    /// `recipe.updated_at` at the time this translation was made
    pub source_updated_at: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Provenance attached to a localized recipe
pub struct TranslationInfo { pub lang: String, pub origin: TranslationOrigin, pub stale: bool, pub translated_at: String }

/// What `GET /recipes/{id}?lang=` returns: the `Recipe` shape (flattened, translated wording,
/// `canonical_label` on every ingredient) plus an optional `translation` member
pub struct LocalizedRecipe { #[serde(flatten)] pub recipe: Recipe, pub translation: Option<TranslationInfo> }

/// One line of the federation manifest (`GET /api/v1/federation/manifest`)
pub struct ManifestEntry { pub id: String, pub updated_at: String, pub content_hash: String }
// Planned (RN-64): `translations: Vec<{ lang, origin, stale, updated_at }>` per entry and
// `translations: Vec<RecipeTranslation>` on `GET /api/v1/federation/recipes/{id}`, so an
// aggregator notices translations without a recipe change (docs/SPEC-hosted-platform.md §4.4).
```

Localized ingredient labels come from `canonical_terms` (`TranslationService::labels`):
cached per `(canonical_id, lang)`, fetched from the model in batches of up to 100 with a
humanized id as fallback; a seeded dictionary of everyday ingredients is planned (RN-75).

### `content_hash` Calculation Algorithm
To protect authorship and deduplicate recipes in the Central Aggregator, implement `calculate_content_hash(recipe)`:
1. Collect all `canonical_id` strings from `recipe.ingredients`, convert to lowercase, and sort alphabetically.
2. Concatenate them with the joined `instructions` text (lowercased, stripped of whitespace and punctuation).
3. Compute and return the hex-encoded `SHA-256` digest.

---

## 2. SQLite Database Schema

### 2.1 Base schema (`migrations/20260330000001_init.sql`)

Full-text search is powered by an `FTS5` virtual table kept in sync automatically via SQLite
triggers (the ingredient sync is three triggers because SQLite has no `INSERT OR UPDATE OR
DELETE` event list in one trigger):

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE recipes (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    servings INTEGER NOT NULL DEFAULT 4,
    prep_time_minutes INTEGER,
    cook_time_minutes INTEGER,
    instructions TEXT NOT NULL, -- JSON array of InstructionStep
    tags TEXT NOT NULL DEFAULT '[]', -- JSON array of strings
    image_path TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    origin_url TEXT,
    author_did TEXT,
    content_hash TEXT NOT NULL,
    is_public BOOLEAN NOT NULL DEFAULT 0
);

CREATE INDEX idx_recipes_public ON recipes(is_public) WHERE is_public = 1;
CREATE INDEX idx_recipes_hash ON recipes(content_hash);
CREATE INDEX idx_recipes_author ON recipes(author_did);

CREATE TABLE recipe_ingredients (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipe_id TEXT NOT NULL,
    raw_text TEXT NOT NULL,
    metric_amount REAL,
    unit TEXT NOT NULL,
    canonical_id TEXT NOT NULL,
    is_specialty BOOLEAN NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (recipe_id) REFERENCES recipes(id) ON DELETE CASCADE
);

CREATE INDEX idx_recipe_ingredients_recipe_id ON recipe_ingredients(recipe_id);
CREATE INDEX idx_recipe_ingredients_canonical ON recipe_ingredients(canonical_id);

CREATE VIRTUAL TABLE recipes_fts USING fts5(
    recipe_id UNINDEXED,
    title,
    description,
    ingredients_raw,
    tags
);

CREATE TRIGGER recipes_ai AFTER INSERT ON recipes BEGIN
    INSERT INTO recipes_fts(recipe_id, title, description, ingredients_raw, tags)
    VALUES (NEW.id, NEW.title, COALESCE(NEW.description, ''), '', NEW.tags);
END;

CREATE TRIGGER recipes_au AFTER UPDATE ON recipes BEGIN
    UPDATE recipes_fts 
    SET title = NEW.title, 
        description = COALESCE(NEW.description, ''),
        tags = NEW.tags
    WHERE recipe_id = NEW.id;
END;

CREATE TRIGGER recipes_ad AFTER DELETE ON recipes BEGIN
    DELETE FROM recipes_fts WHERE recipe_id = OLD.id;
END;

CREATE TRIGGER ingredients_fts_sync_ai AFTER INSERT ON recipe_ingredients BEGIN
    UPDATE recipes_fts
    SET ingredients_raw = (
        SELECT COALESCE(GROUP_CONCAT(raw_text || ' ' || canonical_id, ' '), '')
        FROM recipe_ingredients
        WHERE recipe_id = NEW.recipe_id
    )
    WHERE recipe_id = NEW.recipe_id;
END;

CREATE TRIGGER ingredients_fts_sync_au AFTER UPDATE ON recipe_ingredients BEGIN
    -- same body as _ai
END;

CREATE TRIGGER ingredients_fts_sync_ad AFTER DELETE ON recipe_ingredients BEGIN
    -- same body with OLD.recipe_id
END;
```

### 2.2 Languages and translations (`migrations/20261024000001_i18n.sql`)

Shipped with RN-53/RN-54 (`docs/SPEC-i18n.md`): the source language on `recipes`, cached
translations with their own timestamps, localized canonical labels, and a second FTS index
over translated text that search unions with `recipes_fts` (best rank per recipe).

```sql
ALTER TABLE recipes ADD COLUMN language TEXT;           -- BCP-47 primary subtag, NULL = unknown

CREATE TABLE recipe_translations (
    recipe_id TEXT NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    lang TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    ingredients TEXT NOT NULL,   -- JSON array of raw_text, same order as recipe_ingredients
    instructions TEXT NOT NULL,  -- JSON array of step text, same order as recipe instructions
    tags TEXT NOT NULL DEFAULT '[]',
    origin TEXT NOT NULL CHECK (origin IN ('llm', 'human')),
    source_updated_at TEXT NOT NULL,  -- recipes.updated_at when translated; differs => stale
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (recipe_id, lang)
);

CREATE TABLE canonical_terms (          -- localized ingredient labels, e.g. (garlic, ru, чеснок)
    canonical_id TEXT NOT NULL,
    lang TEXT NOT NULL,
    label TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('llm', 'human')),
    PRIMARY KEY (canonical_id, lang)
);

CREATE VIRTUAL TABLE translations_fts USING fts5(
    recipe_id UNINDEXED,
    lang UNINDEXED,
    title,
    description,
    ingredients_raw,
    tags
);
-- recipe_translations_ai / _au / _ad keep translations_fts in sync, mirroring the recipes triggers.
```

---

## 3. Scraper Pipeline (`crates/recipe-scraper`)

`RecipeImporter` runs the following stages; the cheapest one that yields a recipe wins. Every
user-supplied URL (page, cover, JSON-LD image) will pass an SSRF guard before it is fetched
(RN-74, `docs/SPEC-hosted-platform.md` §6).

### Stage 1: Fast `Schema.org/Recipe` Parsing (Zero-LLM or Micro-Normalization)
1. Fetch the target URL HTML via `reqwest` using a browser-like `User-Agent`.
2. Use `scraper` to locate all `<script type="application/ld+json">` blocks.
3. Find the JSON node matching `"@type": "Recipe"` (handling both top-level objects, arrays, and `@graph` wrappers).
4. Extract `name`, `recipeYield`, `recipeIngredient` (array of strings), `recipeInstructions`, and the image URL.
5. Pass **only the raw ingredient strings** to a lightweight structured-output completion (`RecipeExtractor::normalize_ingredients`) to populate `metric_amount`, `unit`, `canonical_id`, and `is_specialty`. Without a configured model the heuristic normalizer in `recipe-scraper/src/normalize.rs` is used instead.

### Stage 2: Full AI Extraction Fallback (For Instagram, YouTube, Telegram, unstructured blogs)
If `JSON-LD` is missing or the user submits raw unstructured text:
1. Strip noise tags from the DOM (`sanitize.rs`: `script`, `style`, `nav`, `header`, `footer`, `svg`, `aside`, `noscript`, `iframe`, `form`, `button`, `head`).
2. Convert the cleaned HTML into compact Markdown using `htmd`.
3. Call `RecipeExtractor::extract_recipe` (a rig `CompletionRequest` whose `output_schema` is the JSON Schema of `ExtractedRecipeDraft`, parsed leniently from the reply) with a strict system preamble:
   - *"You are a culinary technologist. Extract the recipe into the target schema. Convert all volume/imperial measurements (cups, tbsp, tsp, cloves, pinches) into metric grams (`g`), milliliters (`ml`), or pieces (`pcs`). Generate a standardized English `canonical_id` in `snake_case` for every ingredient. Set `is_specialty = true` for ethnic, gourmet, or hard-to-find ingredients. Extract any time durations mentioned in steps into `timer_seconds`."*

### Source language (`langdetect.rs`, `docs/SPEC-i18n.md`)
After any stage `fill_language` sets `recipe.language`: JSON-LD `inLanguage` or `<html lang>`
when present, otherwise the offline detector (Cyrillic letter signals for `ru`/`uk`, stopword
scoring for `en`/`nl`/`de`/`fr`/`es`/`it`; at least two hits and a clear winner), and only when
that is unsure the model (`RecipeExtractor::detect_language`). `backfill-language` applies
the same to recipes imported before the column existed, without touching `updated_at` or
`content_hash`.

### Stage 3: Photo import (`POST /api/v1/import/photo`, `docs/SPEC-photo-import.md`)
1. Accept 1–6 images (`images::MAX_PHOTOS`). Each is decoded, oriented, downscaled so the
   longer side is at most 1600 px (`MAX_PHOTO_SIDE`) and re-encoded as JPEG quality 85
   (`normalize_photo`); the originals are discarded.
2. All photos go to the vision model in one call (`RecipeExtractor::extract_from_photos`,
   the `ExtractedRecipeDraft` schema plus a per-photo classification: recipe text, the
   finished dish, or unrelated). `PhotoExtraction::cover_index` picks the first dish photo.
3. The recipe is saved immediately (`ImportService::import_photos`) and the chosen photo
   becomes the cover through the same WebP path as web covers.
4. Errors: `422` when no recipe is found in the photos, `503` when the configured model
   cannot see images or no model is configured, `502` for an unusable model reply.

### LLM configuration (`crates/recipe-scraper/src/llm.rs`)

The extractor is behind the `RecipeExtractor` trait (`normalize_ingredients`,
`extract_recipe`, `extract_from_photos`, `detect_language`, `translate_recipe`,
`translate_terms`, plus `supports_images` / `supports_translation` capability flags) so the
importer runs with a fake in tests and with heuristics when no model is configured.

| Variable | Meaning |
|---|---|
| `LLM_MODEL` | rig catalog reference `vendor/model`, e.g. `minimax/MiniMax-M3`, `anthropic/claude-haiku-4-5`, `openai/gpt-5-mini`, `ollama/llama3.2`. Unset: heuristics only, `/import/text` answers 503. |
| `<VENDOR>_API_KEY` | The vendor's own credential (`MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, ...), read by rig together with its endpoint overrides (e.g. `MINIMAX_ANTHROPIC_API_BASE`). Local vendors need none. |
| `LLM_API_KEY` | Optional generic override. When set, only this key is used and no other vendor variable is read. |

`RigExtractor::new` resolves the reference with `Catalog::builtin().connect_with(reference, options)` where `options` carries `LLM_API_KEY` if set and is otherwise empty so rig reads the vendor's environment, and sends `CompletionRequest::new(Message::user(prompt)).preamble(..).temperature(0.0).output_schema(schema_for!(T))`.

### Stage 4: Mealie Backup Migrator (`read_mealie_backup`, `POST /api/v1/import/mealie-backup`)
1. Accept a `.zip` archive generated by Mealie's backup export.
2. Stream and parse the recipe JSON files and associated media files.
3. Convert cover images to WebP with `images::to_webp_cover` (width capped at 1200 px, Lanczos; the `image` crate encodes lossless WebP only, lossy quality 80 is RN-21) and store them as `data/images/{recipe_id}.webp` via `ImageStore::save_cover`.