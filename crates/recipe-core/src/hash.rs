//! Deterministic `content_hash` used by the Central Aggregator to track
//! forks and duplicates across nodes.
//!
//! Algorithm:
//! 1. For every ingredient build the token `canonical_id:amount:unit` where
//!    `canonical_id` is lowercased and `amount` is the metric quantity
//!    (empty when unknown). Sort the tokens alphabetically.
//! 2. Normalize every instruction step: lowercase, then drop all whitespace
//!    and punctuation (keep alphanumerics only). Concatenate in step order.
//! 3. Join `sorted_tokens` with `;`, append `|` and the normalized
//!    instruction text, then return the lowercase hex SHA-256 digest.
//!
//! Title, description, tags, images, provenance and timestamps are
//! deliberately excluded so cosmetic edits do not break deduplication.

use sha2::{Digest, Sha256};

use crate::models::{IngredientItem, Recipe};

/// Compute the deterministic content hash for a recipe.
pub fn calculate_content_hash(recipe: &Recipe) -> String {
    let mut tokens: Vec<String> = recipe.ingredients.iter().map(ingredient_token).collect();
    tokens.sort_unstable();

    let instructions: String = recipe
        .instructions
        .iter()
        .flat_map(|step| step.text.chars())
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    let mut hasher = Sha256::new();
    hasher.update(tokens.join(";").as_bytes());
    hasher.update(b"|");
    hasher.update(instructions.as_bytes());
    hex_lower(&hasher.finalize())
}

fn ingredient_token(item: &IngredientItem) -> String {
    let amount = item
        .metric_amount
        .map(|a| format!("{a}"))
        .unwrap_or_default();
    format!(
        "{}:{}:{}",
        item.canonical_id.trim().to_lowercase(),
        amount,
        item.unit.as_str()
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            // Writing to a String cannot fail.
            let _ = write!(s, "{b:02x}");
            s
        })
}
