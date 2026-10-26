//! `/api/v1/recipes` CRUD and search.

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use recipe_core::{LocalizedRecipe, Recipe, RecipeInput, RecipeSummary, SearchQuery, TranslationInput, normalize_lang};
use recipe_db::recipes::{DEFAULT_LIMIT, MAX_LIMIT};
use serde::{Deserialize, Serialize};

use crate::api::extract::{Json as JsonBody, Query};
use crate::auth::{OptionalOwner, RequireOwner};
use crate::{ApiError, AppState};

/// Upper bound for cover image uploads.
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/recipes", get(list).post(create))
        .route("/recipes/{id}", get(get_one).put(update).delete(delete))
        .route("/recipes/{id}/image", put(upload_image).layer(DefaultBodyLimit::max(MAX_IMAGE_BYTES)))
        .route("/recipes/{id}/translations/{lang}", put(put_translation).delete(delete_translation))
}

/// Query-string parameters for `GET /recipes`.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub q: Option<String>,
    pub tag: Option<String>,
    pub ingredient: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// Display language for titles/tags when a translation exists; `source` for the original
    pub lang: Option<String>,
}

/// Paged list response.
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub items: Vec<RecipeSummary>,
    pub limit: u32,
    pub offset: u32,
    /// Number of recipes matching the filters, across all pages.
    pub total: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct GetParams {
    pub servings: Option<u8>,
    /// Display language (BCP-47 code). `source` disables translation. Absent: `Accept-Language`.
    pub lang: Option<String>,
}

/// Resolve the display language from `?lang=` or the `Accept-Language` header.
/// `Ok(None)` means "show the source language".
pub fn display_language(param: Option<&str>, headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    if let Some(p) = param.map(str::trim).filter(|p| !p.is_empty()) {
        if p.eq_ignore_ascii_case("source") {
            return Ok(None);
        }
        return normalize_lang(p)
            .map(Some)
            .ok_or_else(|| ApiError::BadRequest(format!("invalid language code: {p}")));
    }
    let accept = headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).unwrap_or("");
    Ok(accept
        .split(',')
        .map(|part| part.split(';').next().unwrap_or("").trim())
        .filter(|tag| !tag.is_empty() && *tag != "*")
        .find_map(normalize_lang))
}

async fn list(
    State(state): State<AppState>,
    OptionalOwner(owner): OptionalOwner,
    Query(params): Query<ListParams>,
    headers: HeaderMap,
) -> Result<Json<ListResponse>, ApiError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let lang = display_language(params.lang.as_deref(), &headers)?;
    let query = SearchQuery {
        query: params.q,
        tag: params.tag,
        include_canonical_ingredients: params.ingredient.into_iter().collect(),
        limit: Some(limit),
        offset: Some(offset),
        lang,
        public_only: !owner,
        ..Default::default()
    };
    let (items, total) = tokio::try_join!(state.db().search_recipes(&query), state.db().count_recipes(&query))?;
    Ok(Json(ListResponse { items, limit, offset, total }))
}

async fn create(State(state): State<AppState>, _owner: RequireOwner, JsonBody(input): JsonBody<RecipeInput>) -> Result<Response, ApiError> {
    let recipe = state.db().create_recipe(input).await?;
    Ok((StatusCode::CREATED, Json(recipe)).into_response())
}

async fn get_one(
    State(state): State<AppState>,
    OptionalOwner(owner): OptionalOwner,
    Path(id): Path<String>,
    Query(params): Query<GetParams>,
    headers: HeaderMap,
) -> Result<Json<LocalizedRecipe>, ApiError> {
    let recipe = state
        .db()
        .get_recipe(&id)
        .await?
        .filter(|r| owner || r.is_public)
        .ok_or_else(|| ApiError::not_found("recipe not found"))?;
    let lang = display_language(params.lang.as_deref(), &headers)?;
    // Anonymous readers get cached translations only; live model calls are for the owner.
    let mut localized = state.translations().localize_with(recipe, lang.as_deref(), owner).await?;
    if let Some(target) = params.servings {
        localized.recipe = localized.recipe.scaled_to(target);
    }
    Ok(Json(localized))
}

/// `PUT /recipes/{id}/translations/{lang}`: store a human translation.
async fn put_translation(
    State(state): State<AppState>,
    _owner: RequireOwner,
    Path((id, lang)): Path<(String, String)>,
    JsonBody(input): JsonBody<TranslationInput>,
) -> Result<Json<LocalizedRecipe>, ApiError> {
    state
        .translations()
        .set_human_translation(&id, &lang, input)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("recipe not found"))
}

/// `DELETE /recipes/{id}/translations/{lang}`: drop a translation so it regenerates.
async fn delete_translation(
    State(state): State<AppState>,
    _owner: RequireOwner,
    Path((id, lang)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if state.translations().delete_translation(&id, &lang).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("translation not found"))
    }
}

async fn update(
    State(state): State<AppState>,
    _owner: RequireOwner,
    Path(id): Path<String>,
    JsonBody(input): JsonBody<RecipeInput>,
) -> Result<Json<Recipe>, ApiError> {
    let recipe = state.db().update_recipe(&id, input).await?.ok_or_else(|| ApiError::not_found("recipe not found"))?;
    Ok(Json(recipe))
}

async fn delete(State(state): State<AppState>, _owner: RequireOwner, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    if state.db().delete_recipe(&id).await? {
        if let Err(e) = state.images().delete_cover(&id) {
            tracing::warn!(error = %e, %id, "recipe deleted but its cover file could not be removed");
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("recipe not found"))
    }
}

/// `PUT /recipes/{id}/image`: multipart upload (field `file`) replacing the cover.
async fn upload_image(
    State(state): State<AppState>,
    _owner: RequireOwner,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Recipe>, ApiError> {
    let mut image: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::BadRequest(e.body_text()))? {
        if field.file_name().is_some() || matches!(field.name(), Some("file" | "image")) {
            image = Some(field.bytes().await.map_err(|e| ApiError::BadRequest(e.body_text()))?.to_vec());
            break;
        }
    }
    let image = image.ok_or_else(|| ApiError::BadRequest("multipart upload must contain an image file field".to_string()))?;
    state
        .imports()
        .set_cover(&id, image)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("recipe not found"))
}
