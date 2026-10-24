//! Error type for the database layer.

use thiserror::Error;

/// Errors raised by `recipe-db`.
#[derive(Debug, Error)]
pub enum DbError {
    /// Underlying sqlx / SQLite failure.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Migration failure on startup.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A row could not be converted into a domain model.
    #[error("invalid data in database: {0}")]
    InvalidData(String),
    /// JSON column (instructions / tags) failed to (de)serialize.
    #[error("json column error: {0}")]
    Json(#[from] serde_json::Error),
    /// Core domain error (e.g. unknown unit string stored in DB).
    #[error(transparent)]
    Core(#[from] recipe_core::CoreError),
}
