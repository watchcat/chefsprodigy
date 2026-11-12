//! Video links (YouTube, TikTok): which URLs count as a post, the record the
//! page readers produce, and the link helpers of the import ladder
//! (`docs/SPEC-video-import.md`). The readers are pure functions over fetched
//! HTML; fetching stays in [`crate::RecipeImporter`], behind the SSRF guard.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

pub mod tiktok;
pub mod youtube;

/// Most linked pages checked for structured recipe data before the caption call.
pub const FREE_PASS_LINKS: usize = 3;

/// A platform whose video pages the importer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    YouTube,
    TikTok,
}

/// A video with a known id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoLink {
    pub platform: Platform,
    pub id: String,
    /// The one spelling that is fetched, stored as `origin_url` and handed to a video model.
    pub canonical_url: String,
}

/// A link `import_url` treats as a video post instead of a recipe page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocialLink {
    Video(VideoLink),
    /// A TikTok share link (`vm.tiktok.com/CODE`); the video is known after its redirects.
    TikTokShare(String),
    /// A platform that gives a server nothing to import, by name.
    Unsupported(&'static str),
}

impl SocialLink {
    /// Recognize a YouTube, TikTok or Instagram link; `None` leaves the URL to
    /// the normal page import. Query strings and fragments are dropped.
    pub fn parse(url: &str) -> Option<Self> {
        let url = Url::parse(url.trim()).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        match host.as_str() {
            "youtube.com" | "www.youtube.com" | "m.youtube.com" => {
                let id = match segments.as_slice() {
                    ["watch"] => url
                        .query_pairs()
                        .find(|(k, _)| k == "v")
                        .map(|(_, v)| v.into_owned())?,
                    ["shorts" | "live" | "embed", id] => (*id).to_string(),
                    _ => return None,
                };
                youtube_video(&id)
            }
            "youtu.be" => match segments.as_slice() {
                [id] => youtube_video(id),
                _ => None,
            },
            "tiktok.com" | "www.tiktok.com" | "m.tiktok.com" => match segments.as_slice() {
                [user, "video", id] if user.starts_with('@') && user.len() > 1 => {
                    tiktok_video(user, id)
                }
                ["t", code] => Some(Self::TikTokShare(format!("https://{host}/t/{code}/"))),
                _ => None,
            },
            "vm.tiktok.com" | "vt.tiktok.com" => match segments.as_slice() {
                [code] => Some(Self::TikTokShare(format!("https://{host}/{code}/"))),
                _ => None,
            },
            "instagram.com" | "www.instagram.com" | "instagr.am" => {
                Some(Self::Unsupported("Instagram"))
            }
            _ => None,
        }
    }
}

fn youtube_video(id: &str) -> Option<SocialLink> {
    let valid = id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    valid.then(|| {
        SocialLink::Video(VideoLink {
            platform: Platform::YouTube,
            id: id.to_string(),
            canonical_url: format!("https://www.youtube.com/watch?v={id}"),
        })
    })
}

fn tiktok_video(user: &str, id: &str) -> Option<SocialLink> {
    let valid = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    valid.then(|| {
        SocialLink::Video(VideoLink {
            platform: Platform::TikTok,
            id: id.to_string(),
            canonical_url: format!("https://www.tiktok.com/{user}/video/{id}"),
        })
    })
}

/// What a platform reader lifts from a video page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialPost {
    pub platform: Platform,
    pub canonical_url: String,
    /// YouTube's video title; TikTok posts have none.
    pub title: Option<String>,
    pub author: Option<String>,
    /// The description or caption, as written.
    pub text: String,
    /// The `http(s)` links in `text`, in order.
    pub links: Vec<String>,
    /// Cover candidates, best first.
    pub cover_urls: Vec<String>,
    pub duration_seconds: Option<u32>,
    /// Language of the caption when the platform states it.
    pub language: Option<String>,
}

/// A part of a recipe a source did not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissingPart {
    Ingredients,
    Instructions,
}

/// The parts missing from a recipe with this many ingredients and steps.
pub fn missing_parts(ingredients: usize, steps: usize) -> Vec<MissingPart> {
    let mut missing = Vec::new();
    if ingredients == 0 {
        missing.push(MissingPart::Ingredients);
    }
    if steps == 0 {
        missing.push(MissingPart::Instructions);
    }
    missing
}

/// The `http(s)` URLs written in `text`, in order and without duplicates.
/// Punctuation that ends a sentence or closes a bracket is not part of a link.
pub fn links_in(text: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let Some(start) = word.find("https://").or_else(|| word.find("http://")) else {
            continue;
        };
        let candidate = word[start..].trim_end_matches(|c: char| {
            matches!(
                c,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '»' | '”' | '’'
            )
        });
        let Ok(url) = Url::parse(candidate) else {
            continue;
        };
        if url.host_str().is_none() {
            continue;
        }
        if !links.iter().any(|l| l == candidate) {
            links.push(candidate.to_string());
        }
    }
    links
}

/// Hosts a recipe is never read from: the video platforms themselves, other
/// social networks, link-in-bio services, shops and music services.
const SKIPPED_HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "tiktok.com",
    "instagram.com",
    "instagr.am",
    "facebook.com",
    "fb.com",
    "fb.me",
    "twitter.com",
    "x.com",
    "t.co",
    "threads.net",
    "threads.com",
    "pinterest.com",
    "pin.it",
    "snapchat.com",
    "discord.gg",
    "discord.com",
    "t.me",
    "wa.me",
    "whatsapp.com",
    "twitch.tv",
    "patreon.com",
    "linktr.ee",
    "beacons.ai",
    "bio.link",
    "stan.store",
    "amzn.to",
    "amzn.eu",
    "a.co",
    "etsy.com",
    "ebay.com",
    "spotify.com",
    "music.apple.com",
    "soundcloud.com",
    "epidemicsound.com",
];

fn is_skipped_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    SKIPPED_HOSTS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        // amazon.com, www.amazon.co.uk, smile.amazon.de, ...
        || host.split('.').any(|label| label == "amazon")
}

/// The links of a caption that may lead to the written recipe: `links` without
/// the hosts on the skip list.
pub fn candidate_links(links: &[String]) -> Vec<String> {
    links
        .iter()
        .filter(|link| {
            Url::parse(link)
                .ok()
                .and_then(|u| u.host_str().map(|h| !is_skipped_host(h)))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}
