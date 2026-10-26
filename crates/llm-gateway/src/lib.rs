//! llm-gateway: vendor key custody, per-hub tokens and usage accounting for
//! Chef's Prodigy hubs (docs/SPEC-hosted-platform.md section 5.2).
//!
//! A hub keeps its normal Messages-format client and points
//! `MINIMAX_ANTHROPIC_API_BASE` at `http://llm-gateway:8080/anthropic` with its
//! hub token as the key; the gateway swaps the token for the real vendor key,
//! forwards the request unchanged and records usage per hub and day. v2 adds the
//! free-tier quotas, the canonical-JSON reply cache and BYOK keys.

pub mod admin;
pub mod auth;
pub mod byok;
pub mod cache;
pub mod config;
pub mod db;
pub mod error;
pub mod health;
pub mod proxy;
pub mod quota;
pub mod state;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;

pub use config::{Config, Vendor};
pub use db::{CacheStats, Db, Hub, NewHub, Quota, QuotaPatch, UsageRow};
pub use error::GatewayError;
pub use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(admin::router())
        .merge(proxy::router())
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

async fn not_found() -> GatewayError {
    GatewayError::NotFound("no such endpoint".to_string())
}
