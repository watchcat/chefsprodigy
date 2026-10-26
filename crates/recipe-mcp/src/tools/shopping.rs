//! `build_smart_shopping_list`

use std::collections::BTreeMap;

use recipe_core::{MeasurementUnit, Recipe};
use recipe_db::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{McpToolError, TranslationService};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShoppingItemRequest {
    /// Recipe id
    pub recipe_id: String,
    /// Desired servings (0 keeps the recipe's own serving count)
    pub servings: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildShoppingListParams {
    /// Recipes to cook and how many servings of each
    pub items: Vec<ShoppingItemRequest>,
    /// Canonical IDs already in stock at home (e.g., salt, water, black_pepper, olive_oil)
    #[serde(default)]
    pub pantry_exclude_canonical_ids: Vec<String>,
    /// Display language for canonical_label on each line (BCP-47 code such as "ru")
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct ShoppingLine {
    pub canonical_id: String,
    /// Display name in the requested language (humanized id when none)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_label: Option<String>,
    /// Summed metric amount across recipes; null for to-taste items
    pub total_amount: Option<f32>,
    pub unit: MeasurementUnit,
    pub is_specialty: bool,
    /// How many of the requested recipes use this ingredient
    pub recipe_count: usize,
    /// Original ingredient lines, for context
    pub raw_examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct ShoppingList {
    /// Everyday items for a local supermarket
    pub basic_groceries: Vec<ShoppingLine>,
    /// Rare/ethnic items for online or specialty-store search
    pub specialty_items: Vec<ShoppingLine>,
    /// Requested recipe ids that do not exist
    pub missing_recipes: Vec<String>,
}

/// Aggregate ingredients across `(recipe, servings)` pairs: same canonical id
/// and unit are summed after scaling, pantry items are dropped, and lines are
/// split by specialty flag.
pub fn aggregate_shopping_list(recipes: &[(&Recipe, u8)], pantry: &[String]) -> ShoppingList {
    let pantry: Vec<String> = pantry.iter().map(|p| p.trim().to_lowercase()).collect();
    let mut lines: BTreeMap<(String, &'static str), ShoppingLine> = BTreeMap::new();

    for (recipe, servings) in recipes {
        let scaled = if *servings == 0 { (*recipe).clone() } else { recipe.scaled_to(*servings) };
        let mut seen_in_recipe: Vec<(String, &'static str)> = Vec::new();
        for item in &scaled.ingredients {
            let id = item.canonical_id.trim().to_lowercase();
            if id.is_empty() || pantry.contains(&id) {
                continue;
            }
            let key = (id.clone(), item.unit.as_str());
            let line = lines.entry(key.clone()).or_insert_with(|| ShoppingLine {
                canonical_id: id,
                canonical_label: None,
                total_amount: None,
                unit: item.unit,
                is_specialty: false,
                recipe_count: 0,
                raw_examples: Vec::new(),
            });
            if let Some(a) = item.metric_amount {
                line.total_amount = Some(line.total_amount.unwrap_or(0.0) + a);
            }
            line.is_specialty |= item.is_specialty;
            if line.raw_examples.len() < 3 && !line.raw_examples.contains(&item.raw_text) {
                line.raw_examples.push(item.raw_text.clone());
            }
            if !seen_in_recipe.contains(&key) {
                line.recipe_count += 1;
                seen_in_recipe.push(key);
            }
        }
    }

    let mut list = ShoppingList { basic_groceries: Vec::new(), specialty_items: Vec::new(), missing_recipes: Vec::new() };
    for (_, mut line) in lines {
        line.total_amount = line.total_amount.map(|a| (a * 100.0).round() / 100.0);
        if line.is_specialty { list.specialty_items.push(line) } else { list.basic_groceries.push(line) }
    }
    list
}

/// Attach localized labels to every line.
pub async fn label_lines(list: &mut ShoppingList, translations: &TranslationService, lang: &str) -> Result<(), McpToolError> {
    let ids: Vec<String> = list.basic_groceries.iter().chain(list.specialty_items.iter()).map(|l| l.canonical_id.clone()).collect();
    let labels = translations.labels(&ids, lang).await?;
    for line in list.basic_groceries.iter_mut().chain(list.specialty_items.iter_mut()) {
        line.canonical_label = labels.get(&line.canonical_id).cloned();
    }
    Ok(())
}

pub async fn build(db: &Db, translations: &TranslationService, p: BuildShoppingListParams) -> Result<ShoppingList, McpToolError> {
    build_with_visibility(db, translations, p, false, true).await
}

/// `build` for a caller who may only see public recipes (`public_only`) and
/// may or may not trigger model calls for labels (`allow_live`).
pub async fn build_with_visibility(
    db: &Db,
    translations: &TranslationService,
    p: BuildShoppingListParams,
    public_only: bool,
    allow_live: bool,
) -> Result<ShoppingList, McpToolError> {
    if p.items.is_empty() {
        return Err(McpToolError::InvalidInput("items must contain at least one recipe".to_string()));
    }
    let mut loaded: Vec<(Recipe, u8)> = Vec::new();
    let mut missing = Vec::new();
    for item in &p.items {
        match db.get_recipe(&item.recipe_id).await? {
            Some(r) if !public_only || r.is_public => loaded.push((r, item.servings)),
            _ => missing.push(item.recipe_id.clone()),
        }
    }
    let pairs: Vec<(&Recipe, u8)> = loaded.iter().map(|(r, s)| (r, *s)).collect();
    let mut list = aggregate_shopping_list(&pairs, &p.pantry_exclude_canonical_ids);
    list.missing_recipes = missing;
    if let Some(lang) = p.language.as_deref() {
        let ids: Vec<String> = list.basic_groceries.iter().chain(list.specialty_items.iter()).map(|l| l.canonical_id.clone()).collect();
        let labels = translations.labels_with(&ids, lang, allow_live).await?;
        for line in list.basic_groceries.iter_mut().chain(list.specialty_items.iter_mut()) {
            line.canonical_label = labels.get(&line.canonical_id).cloned();
        }
    }
    Ok(list)
}
