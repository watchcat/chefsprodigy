//! Lazy, cached translation of recipe wording (docs/SPEC-i18n.md).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use recipe_core::{LocalizedRecipe, Recipe, RecipeTranslation, TranslationInput, TranslationOrigin, normalize_lang};
use recipe_db::Db;
use recipe_core::TranslationOrigin as Origin;
use recipe_scraper::importer::language_sample;
use recipe_scraper::langdetect::detect_language;
use recipe_scraper::llm::TranslationRequest;
use recipe_scraper::{RecipeImporter, ScraperError};
use serde::Serialize;

/// Outcome of a language backfill pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BackfillReport {
    pub scanned: usize,
    pub detected: usize,
    pub undetected: usize,
}
use tokio::sync::Mutex;

use crate::ImportError;

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Per-(recipe, lang) locks so concurrent viewers trigger a single model call.
type InFlight = Arc<Mutex<HashMap<(String, String), Arc<Mutex<()>>>>>;

/// Translates recipes on first view and caches the result per language.
#[derive(Clone)]
pub struct TranslationService {
    db: Db,
    importer: RecipeImporter,
    in_flight: InFlight,
}

impl TranslationService {
    pub fn new(db: Db, importer: RecipeImporter) -> Self {
        Self { db, importer, in_flight: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Whether content translation is available (a translating model is configured).
    pub fn can_translate(&self) -> bool {
        self.importer.supports_translation()
    }

    /// Detect and store the source language of every recipe that lacks one.
    /// Offline heuristics first; the model only for recipes they cannot place.
    pub async fn backfill_languages(&self) -> Result<BackfillReport, ImportError> {
        let mut report = BackfillReport::default();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            let batch: Vec<Recipe> = self
                .db
                .recipes_without_language(200)
                .await?
                .into_iter()
                .filter(|r| !seen.contains(&r.id))
                .collect();
            if batch.is_empty() {
                break;
            }
            for recipe in batch {
                seen.insert(recipe.id.clone());
                report.scanned += 1;
                let sample = language_sample(
                    &recipe.title,
                    recipe.ingredients.iter().map(|i| i.raw_text.clone()),
                    recipe.instructions.iter().map(|s| s.text.clone()),
                );
                let mut lang = detect_language(&sample);
                if lang.is_none() && self.can_translate() {
                    match self.importer.detect_language(&sample).await {
                        Ok(l) => lang = l,
                        Err(e) => tracing::warn!(error = %e, recipe = %recipe.id, "language detection failed"),
                    }
                }
                match lang {
                    Some(l) => {
                        self.db.set_recipe_language(&recipe.id, &l).await?;
                        report.detected += 1;
                        tracing::info!(recipe = %recipe.id, lang = %l, "source language backfilled");
                    }
                    None => report.undetected += 1,
                }
            }
        }
        Ok(report)
    }

    /// Humanized fallback label for a canonical id.
    pub fn humanize(canonical_id: &str) -> String {
        canonical_id.replace('_', " ")
    }

    /// Localized labels for canonical ids: cached in `canonical_terms`, missing
    /// ones fetched from the model in batches of 100, else the humanized id.
    pub async fn labels(&self, canonical_ids: &[String], lang: &str) -> Result<HashMap<String, String>, ImportError> {
        self.labels_with(canonical_ids, lang, true).await
    }

    /// Like [`Self::labels`]; with `allow_live = false` only cached labels are
    /// used and nothing is asked of the model (anonymous readers).
    pub async fn labels_with(&self, canonical_ids: &[String], lang: &str, allow_live: bool) -> Result<HashMap<String, String>, ImportError> {
        let lang = normalize_lang(lang).ok_or_else(|| ImportError::InvalidInput(format!("invalid language code: {lang}")))?;
        let mut ids: Vec<String> = canonical_ids.iter().map(|i| i.trim().to_lowercase()).filter(|i| !i.is_empty()).collect();
        ids.sort();
        ids.dedup();
        let mut found = self.db.get_terms(&ids, &lang).await?;
        let missing: Vec<String> = ids.iter().filter(|i| !found.contains_key(*i)).cloned().collect();
        if !missing.is_empty() && allow_live && self.can_translate() {
            for chunk in missing.chunks(100) {
                match self.importer.translate_terms(chunk, &lang).await {
                    Ok(labels) => {
                        let rows: Vec<(String, String)> = labels.into_iter().collect();
                        self.db.upsert_terms(&lang, &rows, Origin::Llm).await?;
                        found.extend(rows);
                    }
                    Err(e) => tracing::warn!(error = %e, %lang, "term labelling failed; using ids"),
                }
            }
        }
        for id in &ids {
            found.entry(id.clone()).or_insert_with(|| Self::humanize(id));
        }
        Ok(found)
    }

    /// Fill `canonical_label` on every ingredient for `lang`.
    pub async fn label_ingredients(&self, recipe: &mut Recipe, lang: &str) -> Result<(), ImportError> {
        self.label_ingredients_with(recipe, lang, true).await
    }

    async fn label_ingredients_with(&self, recipe: &mut Recipe, lang: &str, allow_live: bool) -> Result<(), ImportError> {
        let ids: Vec<String> = recipe.ingredients.iter().map(|i| i.canonical_id.clone()).collect();
        let labels = self.labels_with(&ids, lang, allow_live).await?;
        for item in &mut recipe.ingredients {
            let key = item.canonical_id.trim().to_lowercase();
            // Without live lookups, only real cached labels count; the humanized
            // fallback would otherwise masquerade as a translation.
            item.canonical_label = labels.get(&key).filter(|l| allow_live || **l != Self::humanize(&key)).cloned();
        }
        Ok(())
    }

    /// Present `recipe` in `lang`. `None`, the recipe's own language, or an
    /// unavailable model all yield the source wording with no translation info.
    /// Whenever a language is given, ingredients carry localized labels.
    pub async fn localize(&self, recipe: Recipe, lang: Option<&str>) -> Result<LocalizedRecipe, ImportError> {
        self.localize_with(recipe, lang, true).await
    }

    /// Like [`Self::localize`]. With `allow_live = false` only cached
    /// translations and labels are served; the model is never called.
    pub async fn localize_with(&self, recipe: Recipe, lang: Option<&str>, allow_live: bool) -> Result<LocalizedRecipe, ImportError> {
        let Some(lang) = lang.and_then(normalize_lang) else { return Ok(LocalizedRecipe::source(recipe)) };
        let mut localized = self.localize_wording(recipe, &lang, allow_live).await?;
        self.label_ingredients_with(&mut localized.recipe, &lang, allow_live).await?;
        Ok(localized)
    }

    async fn localize_wording(&self, recipe: Recipe, lang: &str, allow_live: bool) -> Result<LocalizedRecipe, ImportError> {
        let lang = lang.to_string();
        if recipe.language.as_deref() == Some(lang.as_str()) {
            return Ok(LocalizedRecipe::source(recipe));
        }
        if let Some(t) = self.db.get_translation(&recipe.id, &lang).await? {
            if !t.is_stale_for(&recipe) || t.origin == TranslationOrigin::Human || !allow_live {
                return Ok(recipe.localized(&t));
            }
        }
        if !allow_live || !self.can_translate() {
            return Ok(LocalizedRecipe::source(recipe));
        }

        let key = (recipe.id.clone(), lang.clone());
        let lock = self.in_flight.lock().await.entry(key.clone()).or_default().clone();
        let _guard = lock.lock().await;
        // Another request may have finished while we waited.
        if let Some(t) = self.db.get_translation(&recipe.id, &lang).await? {
            if !t.is_stale_for(&recipe) || t.origin == TranslationOrigin::Human {
                return Ok(recipe.localized(&t));
            }
        }
        let result = self.translate_now(&recipe, &lang).await;
        drop(_guard);
        self.in_flight.lock().await.remove(&key);
        match result {
            Ok(t) => Ok(recipe.localized(&t)),
            Err(e) => {
                tracing::warn!(error = %e, recipe = %recipe.id, %lang, "translation failed; serving source language");
                Ok(LocalizedRecipe::source(recipe))
            }
        }
    }

    async fn translate_now(&self, recipe: &Recipe, lang: &str) -> Result<RecipeTranslation, ImportError> {
        let req = TranslationRequest {
            target_lang: lang.to_string(),
            source_lang: recipe.language.clone(),
            title: recipe.title.clone(),
            description: recipe.description.clone(),
            ingredients: recipe.ingredients.iter().map(|i| i.raw_text.clone()).collect(),
            instructions: recipe.instructions.iter().map(|s| s.text.clone()).collect(),
            tags: recipe.tags.clone(),
        };
        let reply = self.importer.translate(&req).await?;
        if reply.ingredients.len() != recipe.ingredients.len() || reply.instructions.len() != recipe.instructions.len() {
            return Err(ScraperError::TranslationIncomplete(format!(
                "got {} ingredient lines and {} steps for {} and {}",
                reply.ingredients.len(),
                reply.instructions.len(),
                recipe.ingredients.len(),
                recipe.instructions.len()
            ))
            .into());
        }
        let stamp = now();
        let t = RecipeTranslation {
            lang: lang.to_string(),
            title: reply.title,
            description: reply.description,
            ingredients: reply.ingredients,
            instructions: reply.instructions,
            tags: reply.tags,
            origin: TranslationOrigin::Llm,
            source_updated_at: recipe.updated_at.clone(),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        self.db.upsert_translation(&recipe.id, &t).await?;
        tracing::info!(recipe = %recipe.id, %lang, "recipe translated");
        Ok(t)
    }

    /// Store a person's translation; it is never overwritten by regeneration.
    /// `Ok(None)` when the recipe does not exist.
    pub async fn set_human_translation(
        &self,
        recipe_id: &str,
        lang: &str,
        input: TranslationInput,
    ) -> Result<Option<LocalizedRecipe>, ImportError> {
        let lang = normalize_lang(lang).ok_or_else(|| ImportError::InvalidInput(format!("invalid language code: {lang}")))?;
        let Some(recipe) = self.db.get_recipe(recipe_id).await? else { return Ok(None) };
        if input.title.trim().is_empty() {
            return Err(ImportError::InvalidInput("translation title must not be empty".to_string()));
        }
        let stamp = now();
        let created_at = self
            .db
            .get_translation(recipe_id, &lang)
            .await?
            .map(|t| t.created_at)
            .unwrap_or_else(|| stamp.clone());
        let t = RecipeTranslation {
            lang,
            title: input.title.trim().to_string(),
            description: input.description.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
            ingredients: input.ingredients,
            instructions: input.instructions,
            tags: input.tags,
            origin: TranslationOrigin::Human,
            source_updated_at: recipe.updated_at.clone(),
            created_at,
            updated_at: stamp,
        };
        self.db.upsert_translation(recipe_id, &t).await?;
        Ok(Some(recipe.localized(&t)))
    }

    /// Drop a stored translation so the next view regenerates it.
    pub async fn delete_translation(&self, recipe_id: &str, lang: &str) -> Result<bool, ImportError> {
        let lang = normalize_lang(lang).ok_or_else(|| ImportError::InvalidInput(format!("invalid language code: {lang}")))?;
        Ok(self.db.delete_translation(recipe_id, &lang).await?)
    }
}
