//! `/api/v1/i18n`: what languages the node offers.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::AppState;

/// UI message bundles shipped with the frontend.
pub const UI_LANGUAGES: &[&str] = &["en", "ru", "nl"];

#[derive(Debug, Serialize)]
pub struct Languages {
    pub ui: Vec<&'static str>,
    /// Whether recipe content can be translated (a translating model is configured).
    pub content_translation: bool,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/i18n/languages", get(languages))
}

async fn languages(State(state): State<AppState>) -> Json<Languages> {
    Json(Languages { ui: UI_LANGUAGES.to_vec(), content_translation: state.translations().can_translate() })
}
