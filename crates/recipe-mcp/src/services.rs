//! Application services shared by the MCP tools and the REST API.

use recipe_core::{Recipe, RecipeInput};
use recipe_db::{Db, DbError};
use recipe_scraper::images::{ImageStore, MAX_PHOTOS, NormalizedPhoto, normalize_photo};
use recipe_scraper::importer::input_from_draft;
use recipe_scraper::mealie::read_mealie_backup;
use recipe_scraper::{RecipeImporter, ScraperError};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("{0}")]
    InvalidUrl(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error(transparent)]
    Scraper(#[from] ScraperError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Result of a Mealie backup import.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MealieImportReport {
    pub imported_count: usize,
    pub failed_count: usize,
    pub errors: Vec<String>,
}

/// Imports recipes through the scraper pipeline and persists them with covers.
#[derive(Clone)]
pub struct ImportService {
    db: Db,
    importer: RecipeImporter,
    images: ImageStore,
}

impl ImportService {
    pub fn new(db: Db, importer: RecipeImporter, images: ImageStore) -> Self {
        Self { db, importer, images }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Validate, fetch, import and persist a recipe from `url`, attaching its
    /// cover image when one can be downloaded and converted.
    pub async fn import_url(&self, url: &str, is_public: bool) -> Result<Recipe, ImportError> {
        let parsed = url::Url::parse(url.trim()).map_err(|e| ImportError::InvalidUrl(format!("invalid url: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ImportError::InvalidUrl("only http(s) urls can be imported".to_string()));
        }
        let result = self.importer.import_url(parsed.as_str()).await?;
        let mut input = result.recipe;
        input.is_public = is_public;
        let recipe = self.db.create_recipe(input).await?;
        let Some(image_url) = result.image_url else { return Ok(recipe) };
        match self.importer.fetch_bytes(&image_url).await {
            Ok(bytes) => Ok(self.attach_cover(recipe, bytes).await),
            Err(e) => {
                tracing::warn!(error = %e, url = %image_url, "cover image download failed");
                Ok(recipe)
            }
        }
    }

    /// Import unstructured text (captions, chat messages) and persist it.
    pub async fn import_text(&self, text: &str, origin_url: Option<&str>, is_public: bool) -> Result<Recipe, ImportError> {
        let result = self.importer.import_text(text, origin_url).await?;
        let mut input = result.recipe;
        input.is_public = is_public;
        Ok(self.db.create_recipe(input).await?)
    }

    /// Import a recipe from photos: normalize, one vision call, persist, and use
    /// the first dish photo as the cover.
    pub async fn import_photos(&self, photos: Vec<Vec<u8>>, is_public: bool) -> Result<Recipe, ImportError> {
        if photos.is_empty() {
            return Err(ImportError::InvalidInput("upload at least one photo".to_string()));
        }
        if photos.len() > MAX_PHOTOS {
            return Err(ImportError::InvalidInput(format!("at most {MAX_PHOTOS} photos per import")));
        }
        if !self.importer.supports_images() {
            return Err(ScraperError::ModelCannotSeeImages("the configured model".to_string()).into());
        }
        let normalized: Vec<NormalizedPhoto> = tokio::task::spawn_blocking(move || {
            photos.iter().map(|p| normalize_photo(p)).collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| ImportError::Internal(e.to_string()))??;

        let extraction = self.importer.extract_photos(&normalized).await?;
        let kinds: Vec<String> = extraction.photos.iter().map(|p| format!("{}:{:?}", p.index, p.kind)).collect();
        tracing::info!(
            title = %extraction.recipe.title,
            photos = normalized.len(),
            kinds = ?kinds,
            cover = ?extraction.cover_index(),
            unreadable = extraction.unreadable.len(),
            language = extraction.language.as_deref().unwrap_or("?"),
            "recipe read from photos"
        );
        let mut input = input_from_draft(extraction.recipe.clone(), None);
        input.is_public = is_public;
        if input.language.is_none() {
            input.language = extraction.language.as_deref().and_then(recipe_core::normalize_lang);
        }
        let recipe = self.db.create_recipe(input).await?;
        match extraction.cover_index().and_then(|i| normalized.get(i)) {
            Some(photo) => Ok(self.attach_cover(recipe, photo.bytes.clone()).await),
            None => Ok(recipe),
        }
    }

    /// Import every recipe from a Mealie backup archive.
    pub async fn import_mealie(&self, archive: Vec<u8>) -> Result<MealieImportReport, ImportError> {
        let recipes = tokio::task::spawn_blocking(move || read_mealie_backup(std::io::Cursor::new(archive)))
            .await
            .map_err(|e| ImportError::Internal(e.to_string()))??;

        let mut report = MealieImportReport { imported_count: 0, failed_count: 0, errors: Vec::new() };
        for entry in recipes {
            let label = entry.source_id.clone().unwrap_or_else(|| entry.recipe.title.clone());
            match self.db.create_recipe(entry.recipe).await {
                Ok(recipe) => {
                    if let Some(bytes) = entry.image {
                        self.attach_cover(recipe, bytes).await;
                    }
                    report.imported_count += 1;
                }
                Err(e) => {
                    report.failed_count += 1;
                    report.errors.push(format!("{label}: {e}"));
                }
            }
        }
        Ok(report)
    }

    /// Replace the cover of an existing recipe from uploaded image bytes.
    /// Unlike [`Self::attach_cover`], conversion failures are returned.
    /// `Ok(None)` when the recipe does not exist.
    pub async fn set_cover(&self, recipe_id: &str, bytes: Vec<u8>) -> Result<Option<Recipe>, ImportError> {
        let Some(recipe) = self.db.get_recipe(recipe_id).await? else { return Ok(None) };
        let store = self.images.clone();
        let id = recipe.id.clone();
        let web_path = tokio::task::spawn_blocking(move || store.save_cover(&id, &bytes))
            .await
            .map_err(|e| ImportError::Internal(e.to_string()))??;
        let mut input = RecipeInput::from(recipe.clone());
        input.image_path = Some(web_path);
        Ok(self.db.update_recipe(&recipe.id, input).await?)
    }

    /// Convert and store `bytes` as the recipe's cover. Failures are logged and
    /// leave the recipe without an image; the import itself never fails.
    pub async fn attach_cover(&self, recipe: Recipe, bytes: Vec<u8>) -> Recipe {
        let store = self.images.clone();
        let id = recipe.id.clone();
        let saved = tokio::task::spawn_blocking(move || store.save_cover(&id, &bytes)).await;
        let web_path = match saved {
            Ok(Ok(path)) => path,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, id = %recipe.id, "cover image conversion failed");
                return recipe;
            }
            Err(e) => {
                tracing::warn!(error = %e, id = %recipe.id, "cover image task failed");
                return recipe;
            }
        };
        let mut input = RecipeInput::from(recipe.clone());
        input.image_path = Some(web_path);
        match self.db.update_recipe(&recipe.id, input).await {
            Ok(Some(updated)) => updated,
            Ok(None) => recipe,
            Err(e) => {
                tracing::warn!(error = %e, id = %recipe.id, "could not record cover path");
                recipe
            }
        }
    }
}
