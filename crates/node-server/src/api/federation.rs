//! `/api/v1/federation` endpoints consumed by the Central Aggregator.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use recipe_core::{FederatedRecipe, FederationManifest};

use crate::{ApiError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/federation/manifest", get(manifest))
        .route("/federation/recipes/{id}", get(public_recipe))
}

/// Node metadata plus the public recipe index for delta sync; the platform's
/// poller deserializes the same `recipe_core::FederationManifest`.
async fn manifest(State(state): State<AppState>) -> Result<Json<FederationManifest>, ApiError> {
    let node = state.node();
    Ok(Json(FederationManifest {
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
