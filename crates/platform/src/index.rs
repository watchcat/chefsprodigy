//! What the index keeps from a federated recipe, and change detection against a
//! manifest entry. Hub content is bounded here (title and tag lengths, counts) so
//! one hub cannot bloat the shared index.

use std::collections::BTreeMap;

use recipe_core::{FederatedRecipe, ManifestEntry, normalize_lang};

use crate::db::IndexedRecipe;

const MAX_TITLE_CHARS: usize = 300;
const MAX_TAGS: usize = 30;
const MAX_TAG_CHARS: usize = 64;
const MAX_CANONICAL: usize = 200;
const MAX_TRANSLATIONS: usize = 40;

/// One recipe ready to be written to `recipes_index`, `translations_index` and `recipes_fts`.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexDoc {
    pub recipe_id: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Sorted, distinct canonical ingredient ids
    pub canonical: Vec<String>,
    pub language: Option<String>,
    pub content_hash: String,
    pub updated_at: String,
    pub image_url: Option<String>,
    pub translations: Vec<IndexTranslation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexTranslation {
    pub lang: String,
    pub title: String,
    pub tags: Vec<String>,
    pub origin: String,
    pub stale: bool,
    pub updated_at: String,
}

/// Recipe ids are ULIDs on the hub; anything else is skipped rather than put into
/// a URL path or a link.
pub fn valid_recipe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The language key a translation is indexed under (`None`: not indexed). Used for
/// both the manifest and the fetched recipe so change detection settles.
pub fn translation_lang(lang: &str) -> Option<String> {
    normalize_lang(lang)
}

/// `public_url` + the hub's relative image path (`images/<id>.webp`), when the path
/// is a plain relative path.
pub fn cover_url(public_url: &str, image_path: Option<&str>) -> Option<String> {
    let path = image_path?.trim().trim_start_matches('/');
    let plain = !path.is_empty()
        && path.len() <= 200
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    plain.then(|| format!("{}/{path}", public_url.trim_end_matches('/')))
}

/// Link to the recipe in the hub's web UI.
pub fn recipe_url(public_url: &str, recipe_id: &str) -> String {
    format!("{}/#/recipe/{recipe_id}", public_url.trim_end_matches('/'))
}

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag = clip(tag, MAX_TAG_CHARS);
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
        if out.len() == MAX_TAGS {
            break;
        }
    }
    out
}

impl IndexDoc {
    pub fn new(public_url: &str, fed: &FederatedRecipe) -> Self {
        let recipe = &fed.recipe;
        let mut canonical: Vec<String> = recipe
            .ingredients
            .iter()
            .map(|i| i.canonical_id.trim().to_lowercase())
            .filter(|c| !c.is_empty())
            .collect();
        canonical.sort();
        canonical.dedup();
        canonical.truncate(MAX_CANONICAL);
        let title = clip(&recipe.title, MAX_TITLE_CHARS);

        let mut by_lang: BTreeMap<String, IndexTranslation> = BTreeMap::new();
        for t in &fed.translations {
            let Some(lang) = translation_lang(&t.lang) else {
                continue;
            };
            if !by_lang.contains_key(&lang) && by_lang.len() == MAX_TRANSLATIONS {
                continue;
            }
            let translated = clip(&t.title, MAX_TITLE_CHARS);
            by_lang.insert(
                lang.clone(),
                IndexTranslation {
                    lang,
                    title: if translated.is_empty() {
                        title.clone()
                    } else {
                        translated
                    },
                    tags: clean_tags(&t.tags),
                    origin: t.origin.as_str().to_string(),
                    stale: t.is_stale_for(recipe),
                    updated_at: t.updated_at.clone(),
                },
            );
        }

        Self {
            recipe_id: recipe.id.clone(),
            title,
            tags: clean_tags(&recipe.tags),
            canonical,
            language: recipe.language.as_deref().and_then(normalize_lang),
            content_hash: recipe.content_hash.clone(),
            updated_at: recipe.updated_at.clone(),
            image_url: cover_url(public_url, recipe.image_path.as_deref()),
            translations: by_lang.into_values().collect(),
        }
    }
}

impl IndexedRecipe {
    /// True when the manifest entry describes exactly what is indexed: same
    /// `(updated_at, content_hash)` and the same `(lang, updated_at)` pairs.
    pub fn matches(&self, entry: &ManifestEntry) -> bool {
        if self.updated_at != entry.updated_at || self.content_hash != entry.content_hash {
            return false;
        }
        let listed: BTreeMap<String, String> = entry
            .translations
            .iter()
            .filter_map(|t| translation_lang(&t.lang).map(|lang| (lang, t.updated_at.clone())))
            .take(MAX_TRANSLATIONS)
            .collect();
        listed == self.translations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_urls_only_for_plain_relative_paths() {
        let base = "https://jacob.chefslegacy.com";
        assert_eq!(
            cover_url(base, Some("images/01ABC.webp")).as_deref(),
            Some("https://jacob.chefslegacy.com/images/01ABC.webp")
        );
        assert_eq!(
            cover_url(base, Some("/images/x.webp")).as_deref(),
            Some("https://jacob.chefslegacy.com/images/x.webp")
        );
        for bad in [
            "",
            "../secret",
            "images/../x",
            "https://evil.example/x.webp",
            "images//x",
            "a b.webp",
        ] {
            assert_eq!(cover_url(base, Some(bad)), None, "{bad}");
        }
        assert_eq!(cover_url(base, None), None);
    }

    #[test]
    fn recipe_ids_are_url_safe() {
        assert!(valid_recipe_id("01JACOB0000000000000000SOUP"));
        assert!(!valid_recipe_id(""));
        assert!(!valid_recipe_id("../manifest"));
        assert!(!valid_recipe_id("a/b"));
        assert!(!valid_recipe_id(&"x".repeat(65)));
    }
}
