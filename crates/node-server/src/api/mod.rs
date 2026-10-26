//! REST API (`/api/v1`).

pub mod extract;
pub mod federation;
pub mod i18n;
pub mod import;
pub mod recipes;
pub mod shopping;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::{ApiError, AppState};

/// All `/api/v1` routes. Unknown API paths return a JSON 404 instead of the UI.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .merge(recipes::router())
        .merge(federation::router())
        .merge(import::router())
        .merge(shopping::router())
        .merge(i18n::router())
        .merge(crate::auth::router())
        .fallback(not_found)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn not_found() -> ApiError {
    ApiError::not_found("no such endpoint")
}
