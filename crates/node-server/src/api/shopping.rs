//! `/api/v1/shopping-list`: aggregate ingredients for several recipes.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use recipe_mcp::tools::shopping::{BuildShoppingListParams, ShoppingList, build_with_visibility};
use serde::Deserialize;

use crate::api::extract::{Json as JsonBody, Query};
use crate::api::recipes::display_language;
use crate::auth::OptionalOwner;
use crate::{ApiError, AppState};

#[derive(Debug, Default, Deserialize)]
pub struct ShoppingParams {
    /// Display language for `canonical_label`; falls back to `Accept-Language`
    pub lang: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/shopping-list", post(shopping_list))
}

async fn shopping_list(
    State(state): State<AppState>,
    OptionalOwner(owner): OptionalOwner,
    Query(q): Query<ShoppingParams>,
    headers: HeaderMap,
    JsonBody(mut params): JsonBody<BuildShoppingListParams>,
) -> Result<Json<ShoppingList>, ApiError> {
    if params.language.is_none() {
        params.language = display_language(q.lang.as_deref(), &headers)?;
    }
    Ok(Json(
        build_with_visibility(state.db(), &state.translations(), params, !owner, owner).await?,
    ))
}
