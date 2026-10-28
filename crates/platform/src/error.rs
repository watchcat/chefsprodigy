//! Platform failures, rendered as `{"error": "..."}` JSON like the hub's REST API.

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::WWW_AUTHENTICATE;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    NotFound(String),
    /// A dependency is missing from the configuration (the admin token).
    #[error("{0}")]
    Unavailable(String),
    /// The outbound HTTP client could not be built.
    #[error("http client: {0}")]
    Client(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl PlatformError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Client(_) | Self::Db(_) | Self::Migrate(_) | Self::Json(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for PlatformError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = match &self {
            Self::Client(_) | Self::Db(_) | Self::Migrate(_) | Self::Json(_) => {
                tracing::error!(error = %self, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        let body = Json(json!({ "error": message }));
        if matches!(self, Self::Unauthorized(_)) {
            return (status, [(WWW_AUTHENTICATE, "Bearer")], body).into_response();
        }
        (status, body).into_response()
    }
}
