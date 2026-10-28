//! Gateway-side failures, rendered in the Messages API error envelope
//! (`{"type":"error","error":{"type":…,"message":…}}`) so a hub's rig client
//! surfaces a readable message instead of a parse failure.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotImplemented(String),
    /// A dependency is missing from the configuration (vendor key, admin token).
    #[error("{0}")]
    Unavailable(String),
    /// The vendor could not be reached or answered garbage.
    #[error("upstream request failed: {0}")]
    Upstream(String),
    /// The hub's free-tier quota is used up; nothing was sent upstream.
    #[error("{0}")]
    QuotaExceeded(String),
    /// BYOK encryption or decryption failed.
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

impl GatewayError {
    fn status_and_kind(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "invalid_request_error"),
            Self::NotImplemented(_) => (StatusCode::NOT_IMPLEMENTED, "api_error"),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "api_error"),
            Self::Upstream(_) => (StatusCode::BAD_GATEWAY, "api_error"),
            Self::QuotaExceeded(_) => (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded"),
            Self::Db(_) | Self::Migrate(_) | Self::Crypto(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "api_error")
            }
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, kind) = self.status_and_kind();
        let message = match &self {
            Self::Db(_) | Self::Migrate(_) | Self::Crypto(_) => {
                tracing::error!(error = %self, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        (
            status,
            Json(json!({"type": "error", "error": {"type": kind, "message": message}})),
        )
            .into_response()
    }
}
