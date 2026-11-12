//! TikTok video page reader. The page carries its data as JSON in
//! `<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">`.

use scraper::{Html, Selector};
use serde_json::Value;

use super::{Platform, SocialPost, VideoLink, links_in};
use crate::ScraperError;

fn unreadable() -> ScraperError {
    ScraperError::PostUnreadable("TikTok's page carried no video data".to_string())
}

/// Read the video page of `link`. A `statusCode` other than 0 (10204 for a
/// removed video) means the video is not public.
pub fn read_post(html: &str, link: &VideoLink) -> Result<SocialPost, ScraperError> {
    let data = page_data(html).ok_or_else(unreadable)?;
    let detail = data
        .pointer("/__DEFAULT_SCOPE__/webapp.video-detail")
        .filter(|d| d.is_object())
        .ok_or_else(unreadable)?;
    if let Some(code) = detail.get("statusCode").and_then(Value::as_i64)
        && code != 0
    {
        let reason = detail
            .get("statusMsg")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("no reason given");
        return Err(ScraperError::PostUnavailable(format!(
            "TikTok answered {code} ({reason})"
        )));
    }
    let item = detail
        .pointer("/itemInfo/itemStruct")
        .filter(|i| i.is_object())
        .ok_or_else(unreadable)?;
    let string = |pointer: &str| {
        item.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let text = string("/desc").unwrap_or_default();
    let mut cover_urls: Vec<String> = Vec::new();
    for pointer in ["/video/cover", "/video/originCover"] {
        if let Some(url) = string(pointer)
            && !cover_urls.contains(&url)
        {
            cover_urls.push(url);
        }
    }
    Ok(SocialPost {
        platform: Platform::TikTok,
        canonical_url: link.canonical_url.clone(),
        title: None,
        author: string("/author/nickname").or_else(|| string("/author/uniqueId")),
        links: links_in(&text),
        text,
        cover_urls,
        duration_seconds: item
            .pointer("/video/duration")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        language: string("/textLanguage")
            .as_deref()
            .and_then(recipe_core::normalize_lang),
    })
}

fn page_data(html: &str) -> Option<Value> {
    let selector = Selector::parse("script#__UNIVERSAL_DATA_FOR_REHYDRATION__").ok()?;
    let doc = Html::parse_document(html);
    let script = doc.select(&selector).next()?;
    let json: String = script.text().collect();
    serde_json::from_str(json.trim()).ok()
}
