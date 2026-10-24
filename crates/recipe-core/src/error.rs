//! Domain error type for `recipe-core`.

use thiserror::Error;

/// Errors raised by core domain logic.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A unit string that is not one of `g`, `ml`, `pcs`, `to_taste`.
    #[error("unknown measurement unit: {0}")]
    UnknownUnit(String),
    /// A translation origin string that is not `llm` or `human`.
    #[error("unknown translation origin: {0}")]
    UnknownTranslationOrigin(String),
}
