//! The public API on the apex: search and the hub directory. Only these paths (and
//! `/health`) are routed by the apex Ingress.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::db::DirectoryEntry;
use crate::error::PlatformError;
use crate::search::{SearchPage, SearchParams};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/search", get(search))
        .route("/api/v1/hubs", get(hubs))
}

async fn search(
    State(state): State<AppState>,
    params: Result<Query<SearchParams>, QueryRejection>,
) -> Result<Json<SearchPage>, PlatformError> {
    let Query(params) = params.map_err(|e| PlatformError::BadRequest(e.body_text()))?;
    Ok(Json(state.db().search(&params).await?))
}

#[derive(Debug, Serialize)]
struct Directory {
    hubs: Vec<DirectoryEntry>,
}

async fn hubs(State(state): State<AppState>) -> Result<Json<Directory>, PlatformError> {
    Ok(Json(Directory {
        hubs: state.db().directory().await?,
    }))
}
