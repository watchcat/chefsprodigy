//! Live check of the video page readers (docs/SPEC-video-import.md). The pages
//! of YouTube and TikTok change without notice, so this is not part of CI:
//!
//! ```sh
//! cargo test -p recipe-scraper --test social_live -- --ignored --nocapture
//! ```
//!
//! It reads one real YouTube video, one Short and one TikTok through the guarded
//! client and downloads a cover for each. `SOCIAL_LIVE_YOUTUBE`, `SOCIAL_LIVE_SHORT`
//! and `SOCIAL_LIVE_TIKTOK` replace the links. With `SOCIAL_LIVE_PAGES=<dir>` it
//! reads `youtube.html`, `short.html` and `tiktok.html` from that directory instead
//! of fetching: pages saved on another machine (the hosting node) with the
//! importer's `USER_AGENT`.

use std::sync::Arc;

use recipe_scraper::RecipeImporter;
use recipe_scraper::llm::HeuristicExtractor;
use recipe_scraper::social::{Platform, SocialLink, SocialPost, tiktok, youtube};

const YOUTUBE: &str = "https://www.youtube.com/watch?v=tIH6dyEsKyg";
const SHORT: &str = "https://www.youtube.com/shorts/9X2VkhayBxc";
const TIKTOK: &str = "https://www.tiktok.com/@yashajj/video/7430634507935468843";

fn link(variable: &str, default: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

async fn read(importer: &RecipeImporter, url: &str, saved: Option<String>) -> SocialPost {
    let link = SocialLink::parse(url).unwrap_or_else(|| panic!("{url} is not a video link"));
    match (saved, link) {
        (Some(html), SocialLink::Video(video)) => match video.platform {
            Platform::YouTube => youtube::read_post(&html, &video),
            Platform::TikTok => tiktok::read_post(&html, &video),
        },
        (Some(_), other) => panic!("a saved page needs a full video link, got {other:?}"),
        (None, link) => importer.read_post(link).await,
    }
    .unwrap_or_else(|e| panic!("{url}: {e}"))
}

#[tokio::test]
#[ignore = "reads live YouTube and TikTok pages"]
async fn the_readers_still_understand_live_pages() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let pages = std::env::var("SOCIAL_LIVE_PAGES").ok();
    for (name, file, url) in [
        (
            "YouTube video",
            "youtube.html",
            link("SOCIAL_LIVE_YOUTUBE", YOUTUBE),
        ),
        (
            "YouTube Short",
            "short.html",
            link("SOCIAL_LIVE_SHORT", SHORT),
        ),
        ("TikTok", "tiktok.html", link("SOCIAL_LIVE_TIKTOK", TIKTOK)),
    ] {
        let saved = pages.as_ref().map(|dir| {
            let path = std::path::Path::new(dir).join(file);
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        });
        let from_saved = saved.is_some();
        let post = read(&importer, &url, saved).await;
        assert!(
            !post.text.trim().is_empty(),
            "{name}: no caption read from {url}"
        );
        assert!(
            post.duration_seconds.is_some_and(|s| s > 0),
            "{name}: no length"
        );
        assert!(!post.cover_urls.is_empty(), "{name}: no cover candidates");
        let mut cover = "not downloaded (saved page)".to_string();
        if !from_saved {
            let mut stored = None;
            for candidate in &post.cover_urls {
                if let Ok(bytes) = importer.fetch_bytes(candidate).await
                    && let Ok(image) = image::load_from_memory(&bytes)
                {
                    stored = Some(format!("{}x{}", image.width(), image.height()));
                    break;
                }
            }
            cover = stored
                .unwrap_or_else(|| panic!("{name}: no cover candidate downloads as an image"));
        }
        println!(
            "{name}: {} | title {:?} | by {:?} | {} caption chars | {} links | {} s | cover {cover}",
            post.canonical_url,
            post.title,
            post.author,
            post.text.chars().count(),
            post.links.len(),
            post.duration_seconds.unwrap_or_default(),
        );
    }
}
