//! `recipe-core`: domain models, DTOs, JSON Schemas and the deterministic
//! `content_hash` algorithm shared by every other crate in the workspace.
//!
//! All structs derive `Serialize`, `Deserialize` and `schemars::JsonSchema`
//! so they can be used directly by `rig` extractors and MCP tool schemas.
//! Doc comments are injected into the generated JSON Schema and are read by
//! the LLM, so keep them precise and in English.

pub mod dto;
pub mod error;
pub mod hash;
pub mod models;

pub use dto::{
    LocalizedRecipe, ManifestEntry, RecipeInput, RecipeSummary, RecipeTranslation, SearchQuery, TranslationInfo,
    TranslationInput, TranslationOrigin, normalize_lang,
};
pub use error::CoreError;
pub use hash::calculate_content_hash;
pub use models::{IngredientItem, InstructionStep, MeasurementUnit, Recipe, new_recipe_id};
