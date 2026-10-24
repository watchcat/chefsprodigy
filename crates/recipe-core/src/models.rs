//! Core domain models used across API, DB, MCP and federation.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Standardized measurement unit for normalized ingredients
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementUnit {
    /// Grams (weight)
    G,
    /// Milliliters (volume)
    Ml,
    /// Pieces / whole items (e.g. eggs, cloves, limes)
    Pcs,
    /// No fixed quantity; season to taste
    ToTaste,
}

impl MeasurementUnit {
    /// Stable string form used in the database and the `content_hash`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::G => "g",
            Self::Ml => "ml",
            Self::Pcs => "pcs",
            Self::ToTaste => "to_taste",
        }
    }
}

impl fmt::Display for MeasurementUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MeasurementUnit {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "g" => Ok(Self::G),
            "ml" => Ok(Self::Ml),
            "pcs" => Ok(Self::Pcs),
            "to_taste" => Ok(Self::ToTaste),
            other => Err(CoreError::UnknownUnit(other.to_string())),
        }
    }
}

/// 3-Tier Ingredient Model
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
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
    /// Display label for `canonical_id` in the viewer's language; filled by the node, never by the model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub canonical_label: Option<String>,
}

/// Single cooking instruction step
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct InstructionStep {
    /// 1-based position of this step in the recipe
    pub step_number: u16,
    /// The instruction text for this step
    pub text: String,
    /// Optional extracted timer duration in seconds for interactive Cook Mode timers
    pub timer_seconds: Option<u32>,
}

/// Full Recipe Entity (used across API, DB, and Federation)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Recipe {
    /// ULID identifier of the recipe
    pub id: String,
    /// Recipe title
    pub title: String,
    /// Optional short description
    pub description: Option<String>,
    /// Number of servings the ingredient quantities are written for
    pub servings: u8,
    /// Active preparation time in minutes
    pub prep_time_minutes: Option<u16>,
    /// Cooking time in minutes
    pub cook_time_minutes: Option<u16>,
    /// Normalized ingredient list
    pub ingredients: Vec<IngredientItem>,
    /// Ordered cooking steps
    pub instructions: Vec<InstructionStep>,
    /// Free-form tags (cuisine, course, diet, ...)
    pub tags: Vec<String>,
    /// Relative path to the stored cover image (WebP)
    pub image_path: Option<String>,

    // Federation & provenance metadata
    /// URL the recipe was imported from, if any
    pub origin_url: Option<String>,
    /// Decentralized identifier of the author for federation
    pub author_did: Option<String>,
    /// Deterministic SHA-256 content hash (see `calculate_content_hash`)
    pub content_hash: String,
    /// Whether the recipe is exposed via the federation endpoints
    pub is_public: bool,
    /// RFC 3339 creation timestamp
    pub created_at: String,
    /// RFC 3339 last-update timestamp
    pub updated_at: String,
    /// Source language of the recipe text (BCP-47 primary subtag such as "nl"); None when unknown
    #[serde(default)]
    pub language: Option<String>,
}

impl Recipe {
    /// Combined prep + cook time, if either is known.
    pub fn total_time_minutes(&self) -> Option<u16> {
        match (self.prep_time_minutes, self.cook_time_minutes) {
            (None, None) => None,
            (p, c) => Some(p.unwrap_or(0).saturating_add(c.unwrap_or(0))),
        }
    }
}

/// Generate a new ULID-based recipe id.
pub fn new_recipe_id() -> String {
    ulid::Ulid::generate().to_string()
}
