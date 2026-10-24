//! `get_recipe_details`

use recipe_core::LocalizedRecipe;
use recipe_db::Db;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{McpToolError, TranslationService};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRecipeDetailsParams {
    /// Recipe id (from search_recipes)
    pub recipe_id: String,
    /// Scale all ingredient quantities to this many servings
    #[serde(default)]
    pub target_servings: Option<u8>,
    /// Display language (BCP-47 code such as "ru"): wording is translated on demand and ingredients get localized labels
    #[serde(default)]
    pub language: Option<String>,
}

pub async fn get_details(db: &Db, translations: &TranslationService, p: GetRecipeDetailsParams) -> Result<LocalizedRecipe, McpToolError> {
    let recipe = db
        .get_recipe(&p.recipe_id)
        .await?
        .ok_or_else(|| McpToolError::NotFound(format!("recipe {}", p.recipe_id)))?;
    let mut localized = translations.localize(recipe, p.language.as_deref()).await?;
    if let Some(target) = p.target_servings {
        localized.recipe = localized.recipe.scaled_to(target);
    }
    Ok(localized)
}
