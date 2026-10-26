//! Data transfer objects shared by the REST API, MCP tools and importer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hash::calculate_content_hash;
use crate::models::{IngredientItem, InstructionStep, Recipe};

fn default_servings() -> u8 {
    4
}

/// Mutable recipe payload used to create or fully replace a recipe.
/// Identity, timestamps and `content_hash` are derived by the node.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RecipeInput {
    /// Recipe title
    pub title: String,
    /// Optional short description
    #[serde(default)]
    pub description: Option<String>,
    /// Number of servings the ingredient quantities are written for (defaults to 4)
    #[serde(default = "default_servings")]
    pub servings: u8,
    /// Active preparation time in minutes
    #[serde(default)]
    pub prep_time_minutes: Option<u16>,
    /// Cooking time in minutes
    #[serde(default)]
    pub cook_time_minutes: Option<u16>,
    /// Normalized ingredient list
    #[serde(default)]
    pub ingredients: Vec<IngredientItem>,
    /// Ordered cooking steps
    #[serde(default)]
    pub instructions: Vec<InstructionStep>,
    /// Free-form tags (cuisine, course, diet, ...)
    #[serde(default)]
    pub tags: Vec<String>,
    /// Relative path to the stored cover image
    #[serde(default)]
    pub image_path: Option<String>,
    /// URL the recipe was imported from, if any
    #[serde(default)]
    pub origin_url: Option<String>,
    /// Decentralized identifier of the author
    #[serde(default)]
    pub author_did: Option<String>,
    /// Whether the recipe is exposed via federation
    #[serde(default)]
    pub is_public: bool,
    /// Source language of the recipe text (BCP-47 primary subtag such as "nl")
    #[serde(default)]
    pub language: Option<String>,
}

impl From<Recipe> for RecipeInput {
    fn from(r: Recipe) -> Self {
        Self {
            title: r.title,
            description: r.description,
            servings: r.servings,
            prep_time_minutes: r.prep_time_minutes,
            cook_time_minutes: r.cook_time_minutes,
            ingredients: r.ingredients,
            instructions: r.instructions,
            tags: r.tags,
            image_path: r.image_path,
            origin_url: r.origin_url,
            author_did: r.author_did,
            is_public: r.is_public,
            language: r.language,
        }
    }
}

impl Recipe {
    /// Build a brand-new recipe from user input, computing its `content_hash`.
    pub fn from_input(id: impl Into<String>, input: RecipeInput, now: impl Into<String>) -> Self {
        let now = now.into();
        let mut recipe = Self {
            id: id.into(),
            title: input.title,
            description: input.description,
            servings: input.servings,
            prep_time_minutes: input.prep_time_minutes,
            cook_time_minutes: input.cook_time_minutes,
            ingredients: input.ingredients,
            instructions: input.instructions,
            tags: input.tags,
            image_path: input.image_path,
            origin_url: input.origin_url,
            author_did: input.author_did,
            content_hash: String::new(),
            is_public: input.is_public,
            created_at: now.clone(),
            updated_at: now,
            language: input.language.as_deref().and_then(normalize_lang),
        };
        recipe.content_hash = calculate_content_hash(&recipe);
        recipe
    }

    /// Replace all mutable fields from `input`, bump `updated_at` and recompute `content_hash`.
    /// `id` and `created_at` are preserved.
    pub fn apply_input(&mut self, input: RecipeInput, now: impl Into<String>) {
        self.title = input.title;
        self.description = input.description;
        self.servings = input.servings;
        self.prep_time_minutes = input.prep_time_minutes;
        self.cook_time_minutes = input.cook_time_minutes;
        self.ingredients = input.ingredients;
        self.instructions = input.instructions;
        self.tags = input.tags;
        self.image_path = input.image_path;
        self.origin_url = input.origin_url;
        self.author_did = input.author_did;
        self.is_public = input.is_public;
        self.language = input.language.as_deref().and_then(normalize_lang);
        self.updated_at = now.into();
        self.content_hash = calculate_content_hash(self);
    }

    /// Present this recipe in another language. Only wording changes: title,
    /// description, ingredient `raw_text`, step text and tags. Items the
    /// translation does not cover keep the source wording.
    pub fn localized(&self, t: &RecipeTranslation) -> LocalizedRecipe {
        let mut recipe = self.clone();
        recipe.title = t.title.clone();
        if t.description.is_some() {
            recipe.description = t.description.clone();
        }
        for (item, text) in recipe.ingredients.iter_mut().zip(t.ingredients.iter()) {
            if !text.trim().is_empty() {
                item.raw_text = text.clone();
            }
        }
        for (step, text) in recipe.instructions.iter_mut().zip(t.instructions.iter()) {
            if !text.trim().is_empty() {
                step.text = text.clone();
            }
        }
        if !t.tags.is_empty() {
            recipe.tags = t.tags.clone();
        }
        let stale = t.is_stale_for(self);
        LocalizedRecipe {
            recipe,
            translation: Some(TranslationInfo {
                lang: t.lang.clone(),
                origin: t.origin,
                stale,
                translated_at: t.updated_at.clone(),
            }),
        }
    }

    /// Return a copy scaled to `target_servings`. Metric amounts are multiplied by
    /// `target / servings`; raw text and the stored hash are untouched. A zero
    /// target or zero base servings yields an unchanged copy.
    pub fn scaled_to(&self, target_servings: u8) -> Self {
        if target_servings == 0 || self.servings == 0 || target_servings == self.servings {
            return self.clone();
        }
        let factor = f32::from(target_servings) / f32::from(self.servings);
        let mut scaled = self.clone();
        scaled.servings = target_servings;
        for item in &mut scaled.ingredients {
            item.metric_amount = item.metric_amount.map(|a| a * factor);
        }
        scaled
    }
}

/// Normalize a language tag or English language name to a lowercase BCP-47
/// primary subtag: "NL-be" -> "nl", "ru_RU" -> "ru", "Dutch" -> "nl".
/// Returns None for anything that is not a 2–3 letter code or a known name.
pub fn normalize_lang(input: &str) -> Option<String> {
    let s = input.trim().to_lowercase();
    let well_formed_tag = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let primary = s.split(['-', '_']).next().unwrap_or("");
    if well_formed_tag
        && (2..=3).contains(&primary.len())
        && primary.chars().all(|c| c.is_ascii_lowercase())
    {
        return Some(primary.to_string());
    }
    let named = match s.as_str() {
        "english" => "en",
        "dutch" | "nederlands" | "flemish" => "nl",
        "russian" | "русский" => "ru",
        "german" | "deutsch" => "de",
        "french" | "français" | "francais" => "fr",
        "spanish" | "español" | "espanol" => "es",
        "italian" | "italiano" => "it",
        "portuguese" | "português" => "pt",
        "polish" | "polski" => "pl",
        "ukrainian" | "українська" => "uk",
        "turkish" => "tr",
        "chinese" | "mandarin" => "zh",
        "japanese" => "ja",
        "korean" => "ko",
        "vietnamese" => "vi",
        "thai" => "th",
        "arabic" => "ar",
        "hebrew" => "he",
        "greek" => "el",
        "swedish" => "sv",
        "danish" => "da",
        "norwegian" => "no",
        "finnish" => "fi",
        "czech" => "cs",
        "hungarian" => "hu",
        "indonesian" => "id",
        "hindi" => "hi",
        _ => return None,
    };
    Some(named.to_string())
}

/// Who produced a translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranslationOrigin {
    /// Generated by the configured model
    Llm,
    /// Written or corrected by a person; never overwritten automatically
    Human,
}

impl TranslationOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Human => "human",
        }
    }
}

impl std::str::FromStr for TranslationOrigin {
    type Err = crate::CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "llm" => Ok(Self::Llm),
            "human" => Ok(Self::Human),
            other => Err(crate::CoreError::UnknownTranslationOrigin(
                other.to_string(),
            )),
        }
    }
}

/// The presentation fields of one recipe in one language (stored row).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

impl RecipeTranslation {
    /// A translation is stale once the recipe changed after it was made.
    pub fn is_stale_for(&self, recipe: &Recipe) -> bool {
        self.source_updated_at != recipe.updated_at
    }
}

/// Payload for saving a human translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TranslationInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ingredients: Vec<String>,
    #[serde(default)]
    pub instructions: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Metadata about the translation applied to a returned recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TranslationInfo {
    pub lang: String,
    pub origin: TranslationOrigin,
    /// The source recipe changed after this translation was made
    pub stale: bool,
    pub translated_at: String,
}

/// A recipe as shown to a viewer: the recipe (possibly with translated
/// wording) plus what translation, if any, was applied. Serializes with the
/// same shape as `Recipe` plus an optional `translation` member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LocalizedRecipe {
    #[serde(flatten)]
    pub recipe: Recipe,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<TranslationInfo>,
}

impl LocalizedRecipe {
    /// The recipe in its source language.
    pub fn source(recipe: Recipe) -> Self {
        Self {
            recipe,
            translation: None,
        }
    }
}

/// Compact recipe representation returned by searches and listings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RecipeSummary {
    /// ULID identifier
    pub id: String,
    /// Recipe title
    pub title: String,
    /// Servings the recipe is written for
    pub servings: u8,
    /// prep + cook time in minutes, when known
    pub total_time_minutes: Option<u16>,
    /// Free-form tags
    pub tags: Vec<String>,
    /// Relative path to the cover image
    pub image_path: Option<String>,
    /// Canonical ids of all ingredients, in recipe order
    pub canonical_ingredients: Vec<String>,
    /// Source language of the recipe text, when known
    #[serde(default)]
    pub language: Option<String>,
    /// Language of the translation applied to title/tags, when one was
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_lang: Option<String>,
}

/// Search parameters shared by the REST API and the `search_recipes` MCP tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SearchQuery {
    /// Full-text query over title, description, ingredients and tags
    #[serde(default)]
    pub query: Option<String>,
    /// Only recipes carrying this exact tag
    #[serde(default)]
    pub tag: Option<String>,
    /// Pantry canonical ids: recipes must use at least one, ranked by overlap
    #[serde(default)]
    pub include_canonical_ingredients: Vec<String>,
    /// Canonical ids that must not appear (allergens, dislikes)
    #[serde(default)]
    pub exclude_canonical_ingredients: Vec<String>,
    /// Maximum prep + cook time in minutes
    #[serde(default)]
    pub max_total_time_minutes: Option<u16>,
    /// Page size (defaults to 20, capped by the server)
    #[serde(default)]
    pub limit: Option<u32>,
    /// Page offset
    #[serde(default)]
    pub offset: Option<u32>,
    /// Display language: summaries use an existing translation into it when available
    #[serde(default)]
    pub lang: Option<String>,
    /// Restrict to recipes with `is_public = true` (anonymous readers)
    #[serde(default)]
    pub public_only: bool,
}

/// Entry in the federation manifest used by the Central Aggregator for delta sync.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ManifestEntry {
    /// ULID identifier
    pub id: String,
    /// RFC 3339 last-update timestamp
    pub updated_at: String,
    /// Deterministic content hash
    pub content_hash: String,
}
