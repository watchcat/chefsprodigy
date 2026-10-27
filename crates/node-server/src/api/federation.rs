//! `/api/v1/federation` endpoints consumed by the Central Aggregator.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use recipe_core::{FederatedRecipe, ManifestEntry};
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
    /// Public base URL of this node (`NODE_PUBLIC_URL`), if configured
    pub node_url: Option<String>,
    pub recipes: Vec<ManifestEntry>,
}

async fn manifest(State(state): State<AppState>) -> Result<Json<Manifest>, ApiError> {
    let node = state.node();
    Ok(Json(Manifest {
        author_did: node.author_did.clone(),
        chef_name: node.chef_name.clone(),
        node_url: state.public_url().map(str::to_string),
        recipes: state.db().public_manifest().await?,
    }))
}

/// A public recipe with every stored translation; private recipes are not found.
async fn public_recipe(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<FederatedRecipe>, ApiError> {
    let Some(recipe) = state.db().get_public_recipe(&id).await? else {
        return Err(ApiError::not_found("recipe not found"));
    };
    let translations = state.db().list_translations(&id).await?;
    Ok(Json(FederatedRecipe {
        recipe,
        translations,
    }))
}
