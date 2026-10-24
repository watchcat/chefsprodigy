//! `search_recipes`

use recipe_core::SearchQuery;
use recipe_db::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpToolError;

/// Default number of hits when `limit` is omitted.
pub const DEFAULT_LIMIT: u32 = 5;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchRecipesParams {
    /// Full-text search query (dish name, cuisine style, or keyword)
    #[serde(default)]
    pub query: Option<String>,
    /// List of canonical_id tokens available in the user's fridge/pantry
    #[serde(default)]
    pub include_canonical_ingredients: Vec<String>,
    /// Allergens or disliked canonical_id tokens to exclude
    #[serde(default)]
    pub exclude_canonical_ingredients: Vec<String>,
    /// Maximum combined prep + cook time in minutes
    #[serde(default)]
    pub max_total_time_minutes: Option<u16>,
    /// Maximum number of results (default 5)
    #[serde(default)]
    pub limit: Option<u32>,
    /// Display language (BCP-47 code such as "ru"); titles use an existing translation when available
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHit {
    /// Recipe id to pass to get_recipe_details or build_smart_shopping_list
    pub id: String,
    pub title: String,
    pub servings: u8,
    /// prep + cook time in minutes, when known
    pub total_time_minutes: Option<u16>,
    /// Canonical ids of every ingredient in the recipe
    pub canonical_ingredients: Vec<String>,
    /// Ingredients not covered by include_canonical_ingredients (all of them when no pantry was given)
    pub missing_ingredients: Vec<String>,
    /// Source language of the recipe, when known
    pub language: Option<String>,
    /// Language of the translation applied to the title, when one was
    pub translation_lang: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchRecipesOutput {
    pub recipes: Vec<SearchHit>,
}

/// Lowercased canonical ids from `canonical` that are not in `pantry` (case-insensitive).
pub fn missing_ingredients(canonical: &[String], pantry: &[String]) -> Vec<String> {
    let pantry: Vec<String> = pantry.iter().map(|p| p.trim().to_lowercase()).collect();
    canonical
        .iter()
        .map(|c| c.trim().to_lowercase())
        .filter(|c| !pantry.contains(c))
        .collect()
}

pub async fn search(db: &Db, p: SearchRecipesParams) -> Result<SearchRecipesOutput, McpToolError> {
    let pantry = p.include_canonical_ingredients;
    let query = SearchQuery {
        query: p.query,
        tag: None,
        include_canonical_ingredients: pantry.clone(),
        exclude_canonical_ingredients: p.exclude_canonical_ingredients,
        max_total_time_minutes: p.max_total_time_minutes,
        limit: Some(p.limit.unwrap_or(DEFAULT_LIMIT)),
        offset: None,
        lang: p.language,
    };
    let recipes = db
        .search_recipes(&query)
        .await?
        .into_iter()
        .map(|s| SearchHit {
            missing_ingredients: missing_ingredients(&s.canonical_ingredients, &pantry),
            id: s.id,
            title: s.title,
            servings: s.servings,
            total_time_minutes: s.total_time_minutes,
            canonical_ingredients: s.canonical_ingredients,
            language: s.language,
            translation_lang: s.translation_lang,
        })
        .collect();
    Ok(SearchRecipesOutput { recipes })
}
