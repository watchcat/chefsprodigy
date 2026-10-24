//! `/api/v1/federation` endpoints consumed by the Central Aggregator.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use recipe_core::{ManifestEntry, Recipe};
use serde::Serialize;

use crate::{ApiError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/federation/manifest", get(manifest))
        .route("/federation/recipes/{id}", get(public_recipe))
}

/// Node metadata plus the public recipe index for delta sync.
#[derive(Debug, Serialize)]
pub struct Manifest {
    pub author_did: Option<String>,
    pub chef_name: String,
    pub recipes: Vec<ManifestEntry>,
}

async fn manifest(State(state): State<AppState>) -> Result<Json<Manifest>, ApiError> {
    let node = state.node();
    Ok(Json(Manifest {
        author_did: node.author_did.clone(),
        chef_name: node.chef_name.clone(),
        recipes: state.db().public_manifest().await?,
    }))
}

async fn public_recipe(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<Recipe>, ApiError> {
    state
        .db()
        .get_public_recipe(&id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("recipe not found"))
}
