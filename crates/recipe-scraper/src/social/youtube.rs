//! YouTube watch page reader. The page carries the player response in a script,
//! as `var ytInitialPlayerResponse = {...};`.

use serde_json::Value;

use super::{Platform, SocialPost, VideoLink, links_in};
use crate::ScraperError;

const MARKER: &str = "ytInitialPlayerResponse";

fn unreadable() -> ScraperError {
    ScraperError::PostUnreadable("YouTube's page carried no video data".to_string())
}

/// Read the watch page of `link`. A `playabilityStatus` of `LOGIN_REQUIRED` means
/// the page could not be read without signing in; any other status than `OK`
/// means the video is not public. Its `reason` is localized and is not looked at.
pub fn read_post(html: &str, link: &VideoLink) -> Result<SocialPost, ScraperError> {
    let player = player_response(html).ok_or_else(unreadable)?;
    match player
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
    {
        Some("OK") => {}
        // Private and age-restricted videos, and the bot check YouTube shows a
        // hosting provider's address for a public one: the page does not say which.
        Some("LOGIN_REQUIRED") => {
            return Err(ScraperError::PostUnreadable(
                "YouTube asked for a sign-in, as it does for private or age-restricted videos and sometimes for servers"
                    .to_string(),
            ));
        }
        Some(_) => {
            return Err(ScraperError::PostUnavailable(
                "YouTube reports it as removed or private".to_string(),
            ));
        }
        None => return Err(unreadable()),
    }
    let details = player
        .get("videoDetails")
        .filter(|d| d.is_object())
        .ok_or_else(unreadable)?;
    let string = |key: &str| {
        details
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let text = details
        .get("shortDescription")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let duration_seconds = details.get("lengthSeconds").and_then(|v| match v {
        Value::String(s) => s.parse::<u32>().ok(),
        other => other.as_u64().and_then(|n| u32::try_from(n).ok()),
    });
    Ok(SocialPost {
        platform: Platform::YouTube,
        canonical_url: link.canonical_url.clone(),
        title: string("title"),
        author: string("author"),
        links: links_in(&text),
        text,
        cover_urls: vec![
            format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", link.id),
            format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", link.id),
        ],
        duration_seconds,
        language: None,
    })
}

/// The object assigned to `ytInitialPlayerResponse`. The name also appears where
/// nothing is assigned, so every mention is tried; the streaming deserializer
/// stops at the end of the object, whatever script follows it.
fn player_response(html: &str) -> Option<Value> {
    let mut rest = html;
    while let Some(at) = rest.find(MARKER) {
        rest = &rest[at + MARKER.len()..];
        let Some(after) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        if !after.starts_with('{') {
            continue;
        }
        let mut values = serde_json::Deserializer::from_str(after).into_iter::<Value>();
        if let Some(Ok(value)) = values.next() {
            return Some(value);
        }
    }
    None
}
