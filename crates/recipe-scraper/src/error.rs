//! Error type for the import pipeline.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScraperError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("refusing to fetch {url}: {reason}")]
    BlockedUrl { url: String, reason: String },
    #[error("input is empty")]
    EmptyInput,
    #[error(
        "no LLM configured: set LLM_MODEL (e.g. anthropic/claude-haiku-4-5) to import unstructured content"
    )]
    LlmUnavailable,
    #[error("llm error: {0}")]
    Llm(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("invalid backup: {0}")]
    InvalidBackup(String),
    #[error("translation incomplete: {0}")]
    TranslationIncomplete(String),
    #[error("no recipe found in the photos: {0}")]
    NoRecipeInPhotos(String),
    #[error("{0} cannot read images; set LLM_MODEL to a vision-capable model")]
    ModelCannotSeeImages(String),
}
