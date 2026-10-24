//! Orchestrates stage 1 (JSON-LD) and stage 2 (LLM over Markdown) imports.

use std::sync::Arc;
use std::time::Duration;

use recipe_core::{InstructionStep, RecipeInput};

use crate::ScraperError;
use crate::images::NormalizedPhoto;
use crate::jsonld::{SchemaOrgRecipe, parse_schema_org};
use crate::langdetect::detect_language;
use crate::llm::{ExtractedRecipeDraft, PhotoExtraction, RecipeExtractor, TranslationReply, TranslationRequest};
use crate::sanitize::html_to_markdown;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/124.0 Safari/537.36 RecipeNode/0.1";

/// Default number of servings when a source does not state one.
pub const DEFAULT_SERVINGS: u8 = 4;

/// Outcome of an import: the recipe payload plus a cover image URL the caller
/// may download and store.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportResult {
    pub recipe: RecipeInput,
    pub image_url: Option<String>,
}

/// Recipe import service.
#[derive(Clone)]
pub struct RecipeImporter {
    http: reqwest::Client,
    extractor: Arc<dyn RecipeExtractor>,
}

impl RecipeImporter {
    pub fn new(extractor: Arc<dyn RecipeExtractor>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self { http, extractor }
    }

    /// Use a custom HTTP client (proxies, test servers).
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Fetch `url` and import it.
    pub async fn import_url(&self, url: &str) -> Result<ImportResult, ScraperError> {
        let html = self.http.get(url).send().await?.error_for_status()?.text().await?;
        self.import_html(&html, Some(url)).await
    }

    /// Import already-fetched HTML: JSON-LD first, Markdown + LLM otherwise.
    pub async fn import_html(&self, html: &str, origin_url: Option<&str>) -> Result<ImportResult, ScraperError> {
        if let Some(schema) = parse_schema_org(html).filter(|s| !s.name.is_empty()) {
            tracing::debug!(name = %schema.name, "recipe found via JSON-LD");
            return self.import_schema_org(schema, origin_url).await;
        }
        tracing::debug!("no JSON-LD recipe; falling back to LLM extraction");
        let markdown = html_to_markdown(html);
        self.import_markdown(&markdown, origin_url).await
    }

    /// Import unstructured text (captions, chat messages, OCR output).
    pub async fn import_text(&self, text: &str, origin_url: Option<&str>) -> Result<ImportResult, ScraperError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ScraperError::EmptyInput);
        }
        self.import_markdown(text, origin_url).await
    }

    /// Whether the configured extractor can read photos.
    pub fn supports_images(&self) -> bool {
        self.extractor.supports_images()
    }

    /// Read a recipe from normalized photos (vision model).
    pub async fn extract_photos(&self, photos: &[NormalizedPhoto]) -> Result<PhotoExtraction, ScraperError> {
        if !self.extractor.supports_images() {
            return Err(ScraperError::ModelCannotSeeImages("the configured extractor".to_string()));
        }
        self.extractor.extract_from_photos(photos).await
    }

    /// Whether the configured extractor can translate recipe wording.
    pub fn supports_translation(&self) -> bool {
        self.extractor.supports_translation()
    }

    /// Translate recipe wording with the configured model.
    pub async fn translate(&self, req: &TranslationRequest) -> Result<TranslationReply, ScraperError> {
        self.extractor.translate_recipe(req).await
    }

    /// Ask the model which language `sample` is in.
    pub async fn detect_language(&self, sample: &str) -> Result<Option<String>, ScraperError> {
        self.extractor.detect_language(sample).await
    }

    /// Localized labels for canonical ingredient ids.
    pub async fn translate_terms(&self, ids: &[String], lang: &str) -> Result<std::collections::HashMap<String, String>, ScraperError> {
        self.extractor.translate_terms(ids, lang).await
    }

    /// Download bytes (used for cover images).
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ScraperError> {
        Ok(self.http.get(url).send().await?.error_for_status()?.bytes().await?.to_vec())
    }

    async fn import_schema_org(&self, s: SchemaOrgRecipe, origin_url: Option<&str>) -> Result<ImportResult, ScraperError> {
        let ingredients = self.extractor.normalize_ingredients(&s.ingredients).await?;
        let (prep, cook) = match (s.prep_minutes, s.cook_minutes, s.total_minutes) {
            (None, None, Some(total)) => (None, Some(total)),
            (p, c, _) => (p, c),
        };
        let mut recipe = RecipeInput {
                title: s.name,
                description: s.description,
                servings: s.servings.unwrap_or(DEFAULT_SERVINGS),
                prep_time_minutes: prep,
                cook_time_minutes: cook,
                ingredients,
                instructions: build_steps(s.instructions.iter().map(|t| (t.as_str(), None))),
                tags: dedup_tags(s.tags),
                image_path: None,
                origin_url: origin_url.map(str::to_string),
                author_did: None,
                is_public: false,
                language: s.language.clone(),
            };
        fill_language(&mut recipe);
        Ok(ImportResult { recipe, image_url: s.image_url.as_deref().and_then(|img| resolve_url(img, origin_url)) })
    }

    async fn import_markdown(&self, markdown: &str, origin_url: Option<&str>) -> Result<ImportResult, ScraperError> {
        if markdown.trim().is_empty() {
            return Err(ScraperError::EmptyInput);
        }
        let draft: ExtractedRecipeDraft = self.extractor.extract_recipe(markdown).await?;
        Ok(ImportResult { recipe: input_from_draft(draft, origin_url), image_url: None })
    }
}

/// Turn a model draft into a recipe payload: default servings, renumbered steps,
/// timers filled from text, tags deduplicated.
pub fn input_from_draft(draft: ExtractedRecipeDraft, origin_url: Option<&str>) -> RecipeInput {
    let mut input = RecipeInput {
        title: draft.title,
        description: draft.description.filter(|d| !d.trim().is_empty()),
        servings: draft.servings.filter(|s| *s > 0).unwrap_or(DEFAULT_SERVINGS),
        prep_time_minutes: draft.prep_time_minutes,
        cook_time_minutes: draft.cook_time_minutes,
        ingredients: draft.ingredients,
        instructions: build_steps(draft.instructions.iter().map(|s| (s.text.as_str(), s.timer_seconds))),
        tags: dedup_tags(draft.tags),
        image_path: None,
        origin_url: origin_url.map(str::to_string),
        author_did: None,
        is_public: false,
        language: draft.language.as_deref().and_then(recipe_core::normalize_lang),
    };
    fill_language(&mut input);
    input
}

/// Text sample used for language detection: title, ingredient lines and steps.
pub fn language_sample(title: &str, ingredients: impl Iterator<Item = String>, steps: impl Iterator<Item = String>) -> String {
    let mut parts = vec![title.to_string()];
    parts.extend(ingredients.take(12));
    parts.extend(steps.take(6));
    parts.join("\n")
}

/// Guess `language` from the recipe text when the source did not say.
pub fn fill_language(input: &mut RecipeInput) {
    if input.language.is_some() {
        return;
    }
    let sample = language_sample(
        &input.title,
        input.ingredients.iter().map(|i| i.raw_text.clone()),
        input.instructions.iter().map(|s| s.text.clone()),
    );
    input.language = detect_language(&sample);
}

/// Make `candidate` absolute, joining it onto `base` when relative. Returns
/// `None` when it cannot be made absolute.
pub fn resolve_url(candidate: &str, base: Option<&str>) -> Option<String> {
    if let Ok(abs) = url::Url::parse(candidate) {
        return Some(abs.to_string());
    }
    let base = url::Url::parse(base?).ok()?;
    base.join(candidate).ok().map(|u| u.to_string())
}

/// Renumber steps from 1 and fill missing timers from the step text.
pub fn build_steps<'a>(steps: impl Iterator<Item = (&'a str, Option<u32>)>) -> Vec<InstructionStep> {
    steps
        .filter(|(t, _)| !t.trim().is_empty())
        .enumerate()
        .map(|(i, (text, timer))| InstructionStep {
            step_number: (i + 1) as u16,
            text: text.trim().to_string(),
            timer_seconds: timer.or_else(|| extract_timer_seconds(text)),
        })
        .collect()
}

/// Lowercase, trim, drop empties and duplicates (order preserved).
pub fn dedup_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// First duration mentioned in a step ("30 minutes", "6 hours", "1-2 min") in seconds.
pub fn extract_timer_seconds(text: &str) -> Option<u32> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let num_tok = w.split(['-', '–', '/']).next_back().unwrap_or(w);
        let Ok(n) = num_tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').parse::<f32>() else {
            continue;
        };
        if n <= 0.0 {
            continue;
        }
        let unit = words.get(i + 1)?.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
        let secs = match unit.as_str() {
            "second" | "seconds" | "sec" | "secs" | "s" => 1.0,
            "minute" | "minutes" | "min" | "mins" | "m" => 60.0,
            "hour" | "hours" | "hr" | "hrs" | "h" => 3600.0,
            _ => continue,
        };
        return Some((n * secs).round() as u32);
    }
    None
}
