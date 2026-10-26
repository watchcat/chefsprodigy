//! Standalone Recipe Node HTTP server library.
//!
//! The binary in `main.rs` is a thin CLI around [`build_router`]; keeping the
//! router here lets integration tests drive it in-process.

pub mod api;
pub mod auth;
pub mod error;
pub mod state;
pub mod images;
pub mod mcp;
pub mod ui;

use axum::Router;
use tower_http::trace::TraceLayer;

pub use error::ApiError;
pub use state::{AppState, NodeInfo};

/// Assemble the full application router: REST API under `/api/v1` and the
/// embedded web UI for everything else.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .nest("/api/v1", api::router())
        .merge(mcp::router(&state))
        .route("/images/{name}", axum::routing::get(images::serve_image))
        .fallback(ui::serve_embedded)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
