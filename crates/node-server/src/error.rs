//! Uniform JSON error responses for the REST API.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use recipe_db::DbError;
use recipe_scraper::ScraperError;
use serde_json::json;

/// API-level error. Every variant renders as `{"error": "..."}` JSON.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Unauthorized(String),
    /// Input was well-formed but could not be processed (e.g. no recipe in the photos).
    Unprocessable(String),
    /// Upstream site could not be fetched.
    BadGateway(String),
    /// A required capability (e.g. the LLM) is not configured.
    ServiceUnavailable(String),
    Internal(anyhow::Error),
}

impl ApiError {
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound(what.into())
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        if matches!(self, Self::Unauthorized(_)) {
            return (status, [(axum::http::header::WWW_AUTHENTICATE, "Bearer")], Json(json!({ "error": "missing or invalid bearer token" })))
                .into_response();
        }
        let message = match &self {
            Self::BadGateway(m) | Self::ServiceUnavailable(m) => {
                tracing::warn!(status = %status, error = %m, "upstream failure");
                m.clone()
            }
            Self::NotFound(m) | Self::BadRequest(m) | Self::Unauthorized(m) | Self::Unprocessable(m) => m.clone(),
            Self::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                "internal server error".to_string()
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        Self::Internal(e.into())
    }
}

impl From<ScraperError> for ApiError {
    fn from(e: ScraperError) -> Self {
        match e {
            ScraperError::Http(inner) => Self::BadGateway(format!("could not fetch source: {inner}")),
            ScraperError::LlmUnavailable | ScraperError::ModelCannotSeeImages(_) => Self::ServiceUnavailable(e.to_string()),
            ScraperError::NoRecipeInPhotos(_) => Self::Unprocessable(e.to_string()),
            ScraperError::Llm(m) => Self::BadGateway(format!("llm failure: {m}")),
            ScraperError::TranslationIncomplete(m) => Self::BadGateway(format!("translation failed: {m}")),
            ScraperError::EmptyInput | ScraperError::InvalidBackup(_) | ScraperError::Zip(_) | ScraperError::Image(_) => {
                Self::BadRequest(e.to_string())
            }
            ScraperError::Json(_) | ScraperError::Io(_) => Self::Internal(e.into()),
        }
    }
}

impl From<recipe_mcp::ImportError> for ApiError {
    fn from(e: recipe_mcp::ImportError) -> Self {
        use recipe_mcp::ImportError;
        match e {
            ImportError::InvalidUrl(m) | ImportError::InvalidInput(m) => Self::BadRequest(m),
            ImportError::Scraper(s) => s.into(),
            ImportError::Db(d) => d.into(),
            ImportError::Internal(m) => Self::Internal(anyhow::anyhow!(m)),
        }
    }
}

impl From<recipe_mcp::McpToolError> for ApiError {
    fn from(e: recipe_mcp::McpToolError) -> Self {
        use recipe_mcp::McpToolError;
        match e {
            McpToolError::InvalidInput(m) => Self::BadRequest(m),
            McpToolError::NotFound(m) => Self::NotFound(format!("{m} not found")),
            McpToolError::Fetch(m) => Self::BadGateway(format!("could not fetch source: {m}")),
            McpToolError::Unavailable(m) => Self::ServiceUnavailable(m),
            McpToolError::Import(m) => Self::BadGateway(m),
            McpToolError::Db(d) => d.into(),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(r: PathRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}
