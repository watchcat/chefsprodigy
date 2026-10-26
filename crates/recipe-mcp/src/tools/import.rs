//! `import_recipe`

use recipe_core::Recipe;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{ImportService, McpToolError};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ImportRecipeParams {
    /// Web page to import (Schema.org JSON-LD is used when present, otherwise the page text is extracted)
    #[serde(default)]
    pub source_url: Option<String>,
    /// Raw unstructured recipe text (social media caption, chat message, OCR output)
    #[serde(default)]
    pub raw_text: Option<String>,
    /// Expose the imported recipe through federation
    #[serde(default)]
    pub is_public: bool,
    /// Local image files (cookbook page, recipe card, finished dish) to read with the vision model; stdio clients only
    #[serde(default)]
    pub photo_paths: Option<Vec<String>>,
}

pub async fn import(
    service: &ImportService,
    p: ImportRecipeParams,
) -> Result<Recipe, McpToolError> {
    let url = p
        .source_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let text = p
        .raw_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let paths: Vec<&str> = p
        .photo_paths
        .iter()
        .flatten()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !paths.is_empty() {
        let mut photos = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = tokio::fs::read(path).await.map_err(|e| {
                McpToolError::InvalidInput(format!("cannot read photo {path}: {e}"))
            })?;
            photos.push(bytes);
        }
        return Ok(service.import_photos(photos, p.is_public).await?);
    }
    match (url, text) {
        (Some(url), _) => Ok(service.import_url(url, p.is_public).await?),
        (None, Some(text)) => Ok(service.import_text(text, None, p.is_public).await?),
        (None, None) => Err(McpToolError::InvalidInput(
            "provide source_url, raw_text, or photo_paths".to_string(),
        )),
    }
}
