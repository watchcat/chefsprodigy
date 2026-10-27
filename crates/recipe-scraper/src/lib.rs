//! `recipe-scraper`: two-stage import pipeline.
//!
//! 1. Free, local `Schema.org/Recipe` JSON-LD parsing ([`jsonld`]).
//! 2. LLM extraction via `rig-core` only when structured data is missing
//!    ([`llm`]), always over sanitized Markdown ([`sanitize`]), never raw HTML.
//! 3. Mealie `.zip` backup migrator ([`mealie`]) and cover-image conversion
//!    ([`images`]).
//!
//! [`RecipeImporter`] orchestrates stages 1 and 2; every URL it fetches passes the
//! SSRF guard in [`ssrf`].

pub mod error;
pub mod images;
pub mod importer;
pub mod jsonld;
pub mod langdetect;
pub mod llm;
pub mod mealie;
pub mod normalize;
pub mod sanitize;
pub mod ssrf;

pub use error::ScraperError;
pub use importer::{ImportResult, RecipeImporter};
pub use llm::{HeuristicExtractor, RecipeExtractor, RigExtractor, extractor_from_env};
