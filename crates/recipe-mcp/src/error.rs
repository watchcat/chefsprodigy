//! Tool-level errors, rendered as MCP tool results with `is_error = true` so
//! the calling agent sees a readable message instead of a protocol failure.

use recipe_db::DbError;
use recipe_scraper::ScraperError;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock};
use thiserror::Error;

use crate::services::ImportError;

#[derive(Debug, Error)]
pub enum McpToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("could not fetch source: {0}")]
    Fetch(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("import failed: {0}")]
    Import(String),
    #[error("database error: {0}")]
    Db(#[from] DbError),
}

impl From<ScraperError> for McpToolError {
    fn from(e: ScraperError) -> Self {
        match e {
            ScraperError::Http(inner) => Self::Fetch(inner.to_string()),
            ScraperError::LlmUnavailable
            | ScraperError::ModelCannotSeeImages(_)
            | ScraperError::ModelQuotaExceeded(_) => Self::Unavailable(e.to_string()),
            ScraperError::EmptyInput | ScraperError::BlockedUrl { .. } => {
                Self::InvalidInput(e.to_string())
            }
            ScraperError::NoRecipeInPhotos(_) => Self::Import(e.to_string()),
            other => Self::Import(other.to_string()),
        }
    }
}

impl From<ImportError> for McpToolError {
    fn from(e: ImportError) -> Self {
        match e {
            ImportError::InvalidUrl(m) | ImportError::InvalidInput(m) => Self::InvalidInput(m),
            ImportError::Scraper(s) => s.into(),
            ImportError::Db(d) => Self::Db(d),
            ImportError::Internal(m) => Self::Import(m),
        }
    }
}

impl IntoCallToolResult for McpToolError {
    fn into_call_tool_result(self) -> Result<CallToolResponse, rmcp::ErrorData> {
        Ok(CallToolResult::error(vec![ContentBlock::text(self.to_string())]).into())
    }
}
