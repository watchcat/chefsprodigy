//! Orchestrates stage 1 (JSON-LD) and stage 2 (LLM over Markdown) imports, and
//! the video-link ladder that tries the cheapest source first.

use std::sync::Arc;
use std::time::Duration;

use recipe_core::{InstructionStep, RecipeInput};
use schemars::JsonSchema;
use serde::Serialize;

use crate::ScraperError;
use crate::images::NormalizedPhoto;
use crate::jsonld::{SchemaOrgRecipe, parse_schema_org};
use crate::langdetect::detect_language;
use crate::llm::{
    ExtractedRecipeDraft, PhotoExtraction, RecipeExtractor, TranslationReply, TranslationRequest,
};
use crate::normalize::heuristic_ingredient;
use crate::sanitize::html_to_markdown;
use crate::social::{
    self, FREE_PASS_LINKS, MissingPart, Platform, SocialLink, SocialPost, VideoLink,
    candidate_links, missing_parts,
};
use crate::ssrf::{self, UrlGuard};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/124.0 Safari/537.36 RecipeNode/0.1";

/// Default number of servings when a source does not state one.
pub const DEFAULT_SERVINGS: u8 = 4;

/// Largest video page read, in bytes (a YouTube watch page is about 1.5 MB).
const MAX_VIDEO_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Which rung of the video-link ladder produced the recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    /// A recipe page the caption links to
    LinkedPage,
    /// The caption or description itself
    Caption,
}

/// How a video link was read and what no source held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ImportReport {
    pub source: ImportSource,
    /// Parts of the recipe no source held; empty for a complete recipe
    pub missing: Vec<MissingPart>,
    /// One sentence for the cook when something is missing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ImportReport {
    pub fn new(source: ImportSource, missing: Vec<MissingPart>) -> Self {
        let note = match missing.as_slice() {
            [] => None,
            [MissingPart::Instructions] => Some(
                "The caption lists the ingredients but not the steps; they are probably only in the video.",
            ),
            [MissingPart::Ingredients] => Some(
                "The caption gives the steps but not the ingredients; they are probably only in the video.",
            ),
            _ => Some("Neither the ingredients nor the steps were found."),
        };
        Self {
            source,
            missing,
            note: note.map(str::to_string),
        }
    }
}

/// Outcome of an import: the recipe payload plus cover image URLs the caller
/// may download and store.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportResult {
    pub recipe: RecipeInput,
    /// Cover candidates, best first; the caller keeps the first that downloads and decodes.
    pub image_urls: Vec<String>,
    /// Set for a video link, `None` for every other import.
    pub report: Option<ImportReport>,
}

/// Where video pages are read from: the platforms themselves unless a test
/// points the importer at a stand-in site.
#[derive(Debug, Clone, Default)]
struct VideoOrigins {
    youtube: Option<String>,
    tiktok: Option<String>,
}

/// A page a caption links to, as fetched.
struct LinkedPage {
    /// The link as the caption wrote it.
    link: String,
    /// Where it landed after redirects.
    url: String,
    html: String,
}

/// Recipe import service. Every URL it fetches passes the SSRF guard
/// ([`crate::ssrf`]): public http(s) destinations only, re-checked on each
/// redirect hop.
#[derive(Clone)]
pub struct RecipeImporter {
    http: reqwest::Client,
    guard: UrlGuard,
    extractor: Arc<dyn RecipeExtractor>,
    origins: VideoOrigins,
}

impl RecipeImporter {
    pub fn new(extractor: Arc<dyn RecipeExtractor>) -> Self {
        let guard = UrlGuard::new();
        Self {
            http: build_client(guard),
            guard,
            extractor,
            origins: VideoOrigins::default(),
        }
    }

    /// Read YouTube and TikTok pages from these origins instead of the
    /// platforms. Meant for tests that run a stand-in site locally, like
    /// [`allow_private_hosts`](Self::allow_private_hosts); the links stored
    /// with a recipe stay the real ones.
    pub fn with_social_origins(mut self, youtube: &str, tiktok: &str) -> Self {
        self.origins = VideoOrigins {
            youtube: Some(youtube.trim_end_matches('/').to_string()),
            tiktok: Some(tiktok.trim_end_matches('/').to_string()),
        };
        self
    }

    /// Accept loopback and private-network destinations as well. Meant for
    /// tests that run a stand-in site locally; link-local (cloud metadata),
    /// multicast and unspecified addresses stay refused. Call it before
    /// [`with_http_client`](Self::with_http_client), which replaces the client.
    pub fn allow_private_hosts(mut self) -> Self {
        self.guard = UrlGuard::allowing_private_hosts();
        self.http = build_client(self.guard);
        self
    }

    /// Use a custom HTTP client (proxies, test servers). URLs are still checked
    /// before each request, but the client must bring its own redirect policy
    /// and DNS guard: see [`crate::ssrf::redirect_policy`] and
    /// [`crate::ssrf::GuardedResolver`].
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Fetch `url` and import it. A YouTube or TikTok link takes the
    /// video-link ladder ([`import_post`](Self::import_post)); every other URL
    /// is imported as a recipe page.
    pub async fn import_url(&self, url: &str) -> Result<ImportResult, ScraperError> {
        match SocialLink::parse(url) {
            Some(link) => {
                let post = self.read_post(link).await?;
                self.import_post(post).await
            }
            None => self.import_page(url).await,
        }
    }

    async fn import_page(&self, url: &str) -> Result<ImportResult, ScraperError> {
        let target = self.guard.check(url).await?;
        let response = self
            .http
            .get(target.clone())
            .send()
            .await
            .map_err(|e| ssrf::fetch_error(url, e))?;
        ssrf::check_response(url, &response)?;
        let html = response.error_for_status()?.text().await?;
        self.import_html(&html, Some(target.as_str())).await
    }

    /// Fetch the page of a video link and read it into a post: the first step
    /// of [`import_url`](Self::import_url) for such a link.
    pub async fn read_post(&self, link: SocialLink) -> Result<SocialPost, ScraperError> {
        match link {
            SocialLink::Unsupported(platform) => {
                Err(ScraperError::UnsupportedSource(platform.to_string()))
            }
            SocialLink::Video(video) => {
                let (_, html) = self.fetch_video_page(&self.video_page_url(&video)).await?;
                read_video_page(&html, &video)
            }
            SocialLink::TikTokShare(share) => {
                let fetch = match &self.origins.tiktok {
                    Some(origin) => {
                        let path = url::Url::parse(&share)
                            .map(|u| u.path().to_string())
                            .unwrap_or_default();
                        format!("{origin}{path}")
                    }
                    None => share,
                };
                let (landed, html) = self.fetch_video_page(&fetch).await?;
                // A stand-in origin serves TikTok's paths: read where it landed as TikTok's own.
                let landed = match &self.origins.tiktok {
                    Some(_) => format!("https://www.tiktok.com{}", landed.path()),
                    None => landed.to_string(),
                };
                match SocialLink::parse(&landed) {
                    Some(SocialLink::Video(video)) if video.platform == Platform::TikTok => {
                        read_video_page(&html, &video)
                    }
                    // A dead code lands on TikTok's home page.
                    _ => Err(ScraperError::PostUnavailable(
                        "the share link leads to no video".to_string(),
                    )),
                }
            }
        }
    }

    fn video_page_url(&self, video: &VideoLink) -> String {
        let (origin, real) = match video.platform {
            Platform::YouTube => (&self.origins.youtube, "https://www.youtube.com"),
            Platform::TikTok => (&self.origins.tiktok, "https://www.tiktok.com"),
        };
        match origin {
            Some(origin) => video.canonical_url.replacen(real, origin, 1),
            None => video.canonical_url.clone(),
        }
    }

    /// Fetch a video page: the URL it landed on and its body, capped at
    /// [`MAX_VIDEO_PAGE_BYTES`].
    async fn fetch_video_page(&self, url: &str) -> Result<(url::Url, String), ScraperError> {
        let target = self.guard.check(url).await?;
        let mut response = self
            .http
            .get(target)
            .send()
            .await
            .map_err(|e| ssrf::fetch_error(url, e))?;
        ssrf::check_response(url, &response)?;
        let status = response.status();
        if matches!(status.as_u16(), 404 | 410) {
            return Err(ScraperError::PostUnavailable(format!(
                "the platform answered {status}"
            )));
        }
        if !status.is_success() {
            return Err(ScraperError::PostUnreadable(format!(
                "the platform answered {status}"
            )));
        }
        let landed = response.url().clone();
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_VIDEO_PAGE_BYTES {
                return Err(ScraperError::PostUnreadable(
                    "the page is larger than 4 MB".to_string(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((landed, String::from_utf8_lossy(&body).into_owned()))
    }

    /// Fetch a page a caption links to.
    async fn fetch_linked(&self, link: &str) -> Result<LinkedPage, ScraperError> {
        let target = self.guard.check(link).await?;
        let response = self
            .http
            .get(target)
            .send()
            .await
            .map_err(|e| ssrf::fetch_error(link, e))?;
        ssrf::check_response(link, &response)?;
        let response = response.error_for_status()?;
        let url = response.url().to_string();
        Ok(LinkedPage {
            link: link.to_string(),
            url,
            html: response.text().await?,
        })
    }

    /// Import a video post, cheapest source first (`docs/SPEC-video-import.md`):
    /// a linked page with structured recipe data, then the caption, then the
    /// page the caption call names. A recipe that is still missing its
    /// ingredients or its steps is returned as it is, with the report saying so.
    pub async fn import_post(&self, post: SocialPost) -> Result<ImportResult, ScraperError> {
        let candidates = candidate_links(&post.links);
        let mut pages: Vec<LinkedPage> = Vec::new();
        for link in candidates.iter().take(FREE_PASS_LINKS) {
            let page = match self.fetch_linked(link).await {
                Ok(page) => page,
                Err(e) => {
                    tracing::debug!(error = %e, url = %link, "linked page skipped");
                    continue;
                }
            };
            let structured = parse_schema_org(&page.html).filter(|s| {
                !s.name.trim().is_empty() && !s.ingredients.is_empty() && !s.instructions.is_empty()
            });
            if let Some(schema) = structured {
                tracing::debug!(name = %schema.name, url = %page.url, "recipe found on a linked page");
                let found = self.import_schema_org(schema, Some(&page.url)).await?;
                return Ok(video_result(&post, found, ImportSource::LinkedPage));
            }
            pages.push(page);
        }

        let mut partial: Option<RecipeInput> = None;
        let mut page_error: Option<ScraperError> = None;
        if !post.text.trim().is_empty() {
            let extraction = self.extractor.extract_from_post(&post).await?;
            if let Some(mut draft) = extraction.recipe {
                if draft.language.is_none() {
                    draft.language = post.language.clone();
                }
                let mut recipe = input_from_draft(draft, Some(&post.canonical_url));
                // A short ingredient list may not tell the language; the caption it came from does.
                if recipe.language.is_none() {
                    recipe.language = detect_language(&post.text);
                }
                if is_complete(&recipe) {
                    return Ok(video_result(
                        &post,
                        recipe_only(recipe),
                        ImportSource::Caption,
                    ));
                }
                partial = Some(recipe);
            }
            let named = extraction
                .recipe_link
                .as_deref()
                .and_then(|link| named_candidate(link, &candidates));
            if let Some(named) = named {
                let page = match pages.iter().position(|p| p.link == named) {
                    Some(i) => Ok(pages.swap_remove(i)),
                    None => self.fetch_linked(&named).await,
                };
                let found = match page {
                    Ok(page) => self.import_html(&page.html, Some(&page.url)).await,
                    Err(e) => Err(e),
                };
                match found {
                    Ok(found) if is_complete(&found.recipe) => {
                        return Ok(video_result(&post, found, ImportSource::LinkedPage));
                    }
                    Ok(_) => tracing::debug!(url = %named, "named page holds no complete recipe"),
                    Err(e) => {
                        tracing::warn!(error = %e, url = %named, "named page could not be imported");
                        page_error = Some(e);
                    }
                }
            }
        }
        match partial {
            Some(recipe) => Ok(video_result(
                &post,
                recipe_only(recipe),
                ImportSource::Caption,
            )),
            None => Err(page_error.unwrap_or_else(|| {
                ScraperError::NoRecipeInPost(
                    "neither the caption nor a linked page holds ingredients or steps".to_string(),
                )
            })),
        }
    }

    /// Import already-fetched HTML: JSON-LD first, Markdown + LLM otherwise.
    pub async fn import_html(
        &self,
        html: &str,
        origin_url: Option<&str>,
    ) -> Result<ImportResult, ScraperError> {
        if let Some(schema) = parse_schema_org(html).filter(|s| !s.name.is_empty()) {
            tracing::debug!(name = %schema.name, "recipe found via JSON-LD");
            return self.import_schema_org(schema, origin_url).await;
        }
        tracing::debug!("no JSON-LD recipe; falling back to LLM extraction");
        let markdown = html_to_markdown(html);
        self.import_markdown(&markdown, origin_url).await
    }

    /// Import unstructured text (captions, chat messages, OCR output).
    pub async fn import_text(
        &self,
        text: &str,
        origin_url: Option<&str>,
    ) -> Result<ImportResult, ScraperError> {
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
    pub async fn extract_photos(
        &self,
        photos: &[NormalizedPhoto],
    ) -> Result<PhotoExtraction, ScraperError> {
        if !self.extractor.supports_images() {
            return Err(ScraperError::ModelCannotSeeImages(
                "the configured extractor".to_string(),
            ));
        }
        self.extractor.extract_from_photos(photos).await
    }

    /// Whether the configured extractor can translate recipe wording.
    pub fn supports_translation(&self) -> bool {
        self.extractor.supports_translation()
    }

    /// Translate recipe wording with the configured model.
    pub async fn translate(
        &self,
        req: &TranslationRequest,
    ) -> Result<TranslationReply, ScraperError> {
        self.extractor.translate_recipe(req).await
    }

    /// Ask the model which language `sample` is in.
    pub async fn detect_language(&self, sample: &str) -> Result<Option<String>, ScraperError> {
        self.extractor.detect_language(sample).await
    }

    /// Localized labels for canonical ingredient ids.
    pub async fn translate_terms(
        &self,
        ids: &[String],
        lang: &str,
    ) -> Result<std::collections::HashMap<String, String>, ScraperError> {
        self.extractor.translate_terms(ids, lang).await
    }

    /// Download bytes (used for cover images); guarded like `import_url`.
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ScraperError> {
        let target = self.guard.check(url).await?;
        let response = self
            .http
            .get(target)
            .send()
            .await
            .map_err(|e| ssrf::fetch_error(url, e))?;
        ssrf::check_response(url, &response)?;
        Ok(response.error_for_status()?.bytes().await?.to_vec())
    }

    async fn import_schema_org(
        &self,
        s: SchemaOrgRecipe,
        origin_url: Option<&str>,
    ) -> Result<ImportResult, ScraperError> {
        let ingredients = match self.extractor.normalize_ingredients(&s.ingredients).await {
            Ok(items) => items,
            // JSON-LD needs no model; keep importing it when the model is over its limit.
            Err(ScraperError::ModelQuotaExceeded(reason)) => {
                tracing::warn!(%reason, "model refused; normalizing JSON-LD ingredients locally");
                s.ingredients
                    .iter()
                    .map(|r| heuristic_ingredient(r))
                    .collect()
            }
            Err(e) => return Err(e),
        };
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
        Ok(ImportResult {
            recipe,
            image_urls: s
                .image_url
                .as_deref()
                .and_then(|img| resolve_url(img, origin_url))
                .into_iter()
                .collect(),
            report: None,
        })
    }

    async fn import_markdown(
        &self,
        markdown: &str,
        origin_url: Option<&str>,
    ) -> Result<ImportResult, ScraperError> {
        if markdown.trim().is_empty() {
            return Err(ScraperError::EmptyInput);
        }
        let draft: ExtractedRecipeDraft = self.extractor.extract_recipe(markdown).await?;
        Ok(recipe_only(input_from_draft(draft, origin_url)))
    }
}

fn read_video_page(html: &str, video: &VideoLink) -> Result<SocialPost, ScraperError> {
    match video.platform {
        Platform::YouTube => social::youtube::read_post(html, video),
        Platform::TikTok => social::tiktok::read_post(html, video),
    }
}

/// A recipe with at least one ingredient and one step.
fn is_complete(recipe: &RecipeInput) -> bool {
    missing_parts(recipe.ingredients.len(), recipe.instructions.len()).is_empty()
}

/// A result that is the recipe alone: no cover candidates, no report.
fn recipe_only(recipe: RecipeInput) -> ImportResult {
    ImportResult {
        recipe,
        image_urls: Vec::new(),
        report: None,
    }
}

/// The result of a video import: the video is the source, its covers come
/// before a linked page's picture, and the report says what is missing.
fn video_result(post: &SocialPost, found: ImportResult, source: ImportSource) -> ImportResult {
    let mut recipe = found.recipe;
    recipe.origin_url = Some(post.canonical_url.clone());
    let missing = missing_parts(recipe.ingredients.len(), recipe.instructions.len());
    let mut image_urls = post.cover_urls.clone();
    image_urls.extend(found.image_urls);
    ImportResult {
        recipe,
        image_urls,
        report: Some(ImportReport::new(source, missing)),
    }
}

/// The caption link the model named, as the caption wrote it: only a link that
/// is among `candidates` is ever followed. A trailing slash does not matter.
fn named_candidate(named: &str, candidates: &[String]) -> Option<String> {
    let bare = |link: &str| link.trim().trim_end_matches('/').to_string();
    let named = bare(named);
    candidates.iter().find(|c| bare(c) == named).cloned()
}

/// HTTP client whose DNS resolver and redirect policy enforce `guard`. Should
/// the client fail to build, fall back to one that follows no redirects at all,
/// so no hop can escape the pre-flight check.
fn build_client(guard: UrlGuard) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .redirect(ssrf::redirect_policy(guard))
        .dns_resolver(ssrf::GuardedResolver::new(guard))
        .build()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "guarded HTTP client failed to build; redirects disabled");
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default()
        })
}

/// Turn a model draft into a recipe payload: default servings, renumbered steps,
/// timers filled from text, tags deduplicated.
pub fn input_from_draft(draft: ExtractedRecipeDraft, origin_url: Option<&str>) -> RecipeInput {
    let mut input = RecipeInput {
        title: draft.title,
        description: draft.description.filter(|d| !d.trim().is_empty()),
        servings: draft
            .servings
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_SERVINGS),
        prep_time_minutes: draft.prep_time_minutes,
        cook_time_minutes: draft.cook_time_minutes,
        ingredients: draft.ingredients,
        instructions: build_steps(
            draft
                .instructions
                .iter()
                .map(|s| (s.text.as_str(), s.timer_seconds)),
        ),
        tags: dedup_tags(draft.tags),
        image_path: None,
        origin_url: origin_url.map(str::to_string),
        author_did: None,
        is_public: false,
        language: draft
            .language
            .as_deref()
            .and_then(recipe_core::normalize_lang),
    };
    fill_language(&mut input);
    input
}

/// Text sample used for language detection: title, ingredient lines and steps.
pub fn language_sample(
    title: &str,
    ingredients: impl Iterator<Item = String>,
    steps: impl Iterator<Item = String>,
) -> String {
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
pub fn build_steps<'a>(
    steps: impl Iterator<Item = (&'a str, Option<u32>)>,
) -> Vec<InstructionStep> {
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
        let Ok(n) = num_tok
            .trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
            .parse::<f32>()
        else {
            continue;
        };
        if n <= 0.0 {
            continue;
        }
        let unit = words
            .get(i + 1)?
            .trim_matches(|c: char| !c.is_alphabetic())
            .to_lowercase();
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
