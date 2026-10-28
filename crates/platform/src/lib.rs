//! platform: the apex service for hosted hubs (docs/SPEC-hosted-platform.md
//! section 5.3, v1 without mail and signup).
//!
//! - **Registry**: hubs with their cluster address (`http://hub-<name>.chefs-hubs.svc:8080`),
//!   public URL and poll status, managed by hubctl through the CLI or by the admin API.
//! - **Poller**: fetches each hub's federation manifest every 10 minutes (with jitter
//!   and per-hub backoff) and only the recipes or translations that changed.
//! - **Index**: `recipes_index` + `translations_index` and an FTS5 table with one row
//!   per (hub, recipe, language), so translated titles and tags are searchable.
//! - **Search**: `GET /api/v1/search` groups forks by content hash and answers in the
//!   requested language when the hub exposes that translation.

pub mod admin;
pub mod api;
pub mod config;
pub mod db;
pub mod error;
pub mod health;
pub mod index;
pub mod poller;
pub mod search;
pub mod state;

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;

pub use config::Config;
pub use db::{Db, DirectoryEntry, Hub, HubPatch, HubRegistration};
pub use error::PlatformError;
pub use poller::{PollMode, PollReport, Poller};
pub use search::{AlsoAt, SearchHit, SearchPage, SearchParams};
pub use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(api::router())
        .merge(admin::router())
        .fallback(not_found)
        // Paths only: query strings (search terms) stay out of the request log.
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &Request<Body>| {
                tracing::info_span!("request", method = %req.method(), path = %req.uri().path())
            }),
        )
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

async fn not_found() -> PlatformError {
    PlatformError::NotFound("no such endpoint".to_string())
}
