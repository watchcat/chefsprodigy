//! Video links: which URLs count as a YouTube or TikTok post, and what the
//! readers lift from a saved page (docs/SPEC-video-import.md).

use recipe_scraper::ScraperError;
use recipe_scraper::social::{self, Platform, SocialLink, SocialPost, VideoLink, tiktok, youtube};

fn video(url: &str) -> VideoLink {
    match SocialLink::parse(url) {
        Some(SocialLink::Video(v)) => v,
        other => panic!("{url} should be a video link, got {other:?}"),
    }
}

#[test]
fn youtube_link_forms_share_one_canonical_url() {
    let canonical = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    for url in [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
        "https://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ?si=AbCdEf",
        "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        "https://www.youtube.com/shorts/dQw4w9WgXcQ?feature=share",
        "https://www.youtube.com/live/dQw4w9WgXcQ",
        "https://www.youtube.com/embed/dQw4w9WgXcQ",
        "http://www.youtube.com/watch?v=dQw4w9WgXcQ#t=10",
        "  https://youtu.be/dQw4w9WgXcQ  ",
    ] {
        let v = video(url);
        assert_eq!(v.platform, Platform::YouTube, "{url}");
        assert_eq!(v.id, "dQw4w9WgXcQ", "{url}");
        assert_eq!(v.canonical_url, canonical, "{url}");
    }
}

#[test]
fn tiktok_video_links_drop_their_query() {
    for url in [
        "https://www.tiktok.com/@yashajj/video/7430634507935468843",
        "https://www.tiktok.com/@yashajj/video/7430634507935468843?lang=en&is_from_webapp=1",
        "https://tiktok.com/@yashajj/video/7430634507935468843/",
        "https://m.tiktok.com/@yashajj/video/7430634507935468843",
    ] {
        let v = video(url);
        assert_eq!(v.platform, Platform::TikTok, "{url}");
        assert_eq!(v.id, "7430634507935468843", "{url}");
        assert_eq!(
            v.canonical_url, "https://www.tiktok.com/@yashajj/video/7430634507935468843",
            "{url}"
        );
    }
}

#[test]
fn tiktok_share_links_are_resolved_later() {
    for (url, kept) in [
        (
            "https://vm.tiktok.com/ZMabc123/",
            "https://vm.tiktok.com/ZMabc123/",
        ),
        (
            "https://vt.tiktok.com/ZSxyz/?k=1",
            "https://vt.tiktok.com/ZSxyz/",
        ),
        (
            "https://www.tiktok.com/t/ZTabc123/",
            "https://www.tiktok.com/t/ZTabc123/",
        ),
    ] {
        assert_eq!(
            SocialLink::parse(url),
            Some(SocialLink::TikTokShare(kept.to_string())),
            "{url}"
        );
    }
}

#[test]
fn instagram_links_are_recognized_as_unsupported() {
    for url in [
        "https://www.instagram.com/reel/CyjUMFjP1BB/",
        "https://instagram.com/p/CyjUMFjP1BB/?igsh=abc",
        "https://instagr.am/p/CyjUMFjP1BB",
    ] {
        assert_eq!(
            SocialLink::parse(url),
            Some(SocialLink::Unsupported("Instagram")),
            "{url}"
        );
    }
}

#[test]
fn everything_else_takes_the_normal_link_import() {
    for url in [
        "https://example.com/watch?v=dQw4w9WgXcQ",
        "https://www.youtube.com/@somechannel",
        "https://www.youtube.com/playlist?list=PL123",
        "https://www.youtube.com/watch?v=short",
        "https://www.youtube.com/watch",
        "https://notyoutube.com/shorts/dQw4w9WgXcQ",
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://www.tiktok.com/@yashajj",
        "https://www.tiktok.com/@yashajj/video/notdigits",
        "https://www.tiktok.com/@yashajj/photo/7430634507935468843",
        "https://vm.tiktok.com/",
        "ftp://youtu.be/dQw4w9WgXcQ",
        "not a url",
        "",
    ] {
        assert_eq!(SocialLink::parse(url), None, "{url}");
    }
}

#[test]
fn links_are_lifted_from_caption_text_in_order_without_duplicates() {
    let text = "Full recipe: https://blog.example.com/pasta.\n\
                Shop (https://shop.example.com/pan), again https://blog.example.com/pasta\n\
                and http://second.example.org/a?b=1&c=2! not-a-link.example.com www.nolink.com";
    assert_eq!(
        social::links_in(text),
        vec![
            "https://blog.example.com/pasta",
            "https://shop.example.com/pan",
            "http://second.example.org/a?b=1&c=2",
        ]
    );
    assert!(social::links_in("no links here").is_empty());
}

#[test]
fn candidate_links_skip_social_networks_shops_and_music() {
    let links: Vec<String> = [
        "https://www.instagram.com/cook",
        "https://amzn.to/3abc",
        "https://www.amazon.co.uk/dp/B00",
        "https://blog.example.com/pasta",
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://linktr.ee/cook",
        "https://open.spotify.com/track/1",
        "https://bit.ly/pasta-recipe",
        "https://www.tiktok.com/@cook",
        "https://x.com/cook",
        "https://cook.example.org/recipes/1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        social::candidate_links(&links),
        vec![
            "https://blog.example.com/pasta",
            "https://bit.ly/pasta-recipe",
            "https://cook.example.org/recipes/1",
        ]
    );
}

const YOUTUBE_VIDEO: &str = include_str!("fixtures/social/youtube_video.html");
const YOUTUBE_SHORT: &str = include_str!("fixtures/social/youtube_short.html");
const YOUTUBE_GONE: &str = include_str!("fixtures/social/youtube_unavailable.html");
const YOUTUBE_BOT_CHECK: &str = include_str!("fixtures/social/youtube_bot_check.html");
const TIKTOK_RECIPE: &str = include_str!("fixtures/social/tiktok_recipe_caption.html");
const TIKTOK_LINK: &str = include_str!("fixtures/social/tiktok_link_caption.html");
const TIKTOK_GONE: &str = include_str!("fixtures/social/tiktok_unavailable.html");

fn read(html: &str, url: &str) -> Result<SocialPost, ScraperError> {
    let link = video(url);
    match link.platform {
        Platform::YouTube => youtube::read_post(html, &link),
        Platform::TikTok => tiktok::read_post(html, &link),
    }
}

#[test]
fn the_youtube_reader_lifts_title_description_length_and_covers() {
    let post = read(YOUTUBE_VIDEO, "https://youtu.be/AbCdEfGhIjK").unwrap();
    assert_eq!(post.platform, Platform::YouTube);
    assert_eq!(
        post.canonical_url,
        "https://www.youtube.com/watch?v=AbCdEfGhIjK"
    );
    assert_eq!(post.title.as_deref(), Some("One-Pan Lemon Chicken"));
    assert_eq!(post.author.as_deref(), Some("Sample Kitchen"));
    assert!(
        post.text.starts_with("One pan, forty minutes."),
        "{}",
        post.text
    );
    assert!(post.text.contains("4 chicken thighs"), "{}", post.text);
    assert_eq!(post.duration_seconds, Some(612));
    assert_eq!(
        post.links,
        vec![
            "https://samplekitchen.example.com/lemon-chicken",
            "https://www.instagram.com/samplekitchen",
        ]
    );
    assert_eq!(
        post.cover_urls,
        vec![
            "https://i.ytimg.com/vi/AbCdEfGhIjK/maxresdefault.jpg",
            "https://i.ytimg.com/vi/AbCdEfGhIjK/hqdefault.jpg",
        ]
    );
    assert_eq!(post.language, None);
}

#[test]
fn the_youtube_reader_reads_a_short_like_any_video() {
    let post = read(YOUTUBE_SHORT, "https://www.youtube.com/shorts/ZyXwVuTsRqP").unwrap();
    assert_eq!(
        post.canonical_url,
        "https://www.youtube.com/watch?v=ZyXwVuTsRqP"
    );
    assert_eq!(
        post.title.as_deref(),
        Some("3-ingredient flatbread #shorts")
    );
    assert_eq!(post.duration_seconds, Some(38));
    assert_eq!(post.text, "");
    assert!(post.links.is_empty());
}

#[test]
fn a_video_that_is_not_public_is_unavailable_not_unreadable() {
    let err = read(YOUTUBE_GONE, "https://youtu.be/aaaaaaaaaaa").unwrap_err();
    assert!(matches!(err, ScraperError::PostUnavailable(_)), "{err:?}");
    let err = read(
        TIKTOK_GONE,
        "https://www.tiktok.com/@tiktok/video/1111111111111111111",
    )
    .unwrap_err();
    assert!(matches!(err, ScraperError::PostUnavailable(_)), "{err:?}");
}

#[test]
fn a_sign_in_wall_is_unreadable_because_it_may_be_a_bot_check() {
    // YouTube answers LOGIN_REQUIRED for private and age-restricted videos and also,
    // from a hosting provider's address, for public ones; the reason text is localized.
    let err = read(YOUTUBE_BOT_CHECK, "https://youtu.be/tIH6dyEsKyg").unwrap_err();
    assert!(matches!(err, ScraperError::PostUnreadable(_)), "{err:?}");
    assert!(err.to_string().contains("sign-in"), "{err}");
    assert!(err.to_string().contains("From text"), "{err}");
}

#[test]
fn a_page_without_the_expected_data_is_unreadable() {
    for html in [
        "",
        "<html><body>Before you continue to YouTube</body></html>",
        "<script>var ytInitialPlayerResponse = {broken</script>",
        "<script>var ytInitialPlayerResponse = {\"playabilityStatus\":{\"status\":\"OK\"}};</script>",
    ] {
        let err = read(html, "https://youtu.be/AbCdEfGhIjK").unwrap_err();
        assert!(
            matches!(err, ScraperError::PostUnreadable(_)),
            "{html}: {err:?}"
        );
    }
    for html in [
        "",
        "<html><body>Verify to continue</body></html>",
        r#"<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__" type="application/json">{"__DEFAULT_SCOPE__":{}}</script>"#,
        r#"<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__" type="application/json">not json</script>"#,
        r#"<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__" type="application/json">{"__DEFAULT_SCOPE__":{"webapp.video-detail":{"statusCode":0}}}</script>"#,
    ] {
        let err = read(
            html,
            "https://www.tiktok.com/@cook/video/7430634507935468843",
        )
        .unwrap_err();
        assert!(
            matches!(err, ScraperError::PostUnreadable(_)),
            "{html}: {err:?}"
        );
    }
}

#[test]
fn the_tiktok_reader_lifts_caption_creator_language_length_and_covers() {
    let post = read(
        TIKTOK_RECIPE,
        "https://www.tiktok.com/@samplecook/video/7430634507935468843?lang=en",
    )
    .unwrap();
    assert_eq!(post.platform, Platform::TikTok);
    assert_eq!(
        post.canonical_url,
        "https://www.tiktok.com/@samplecook/video/7430634507935468843"
    );
    assert_eq!(post.title, None, "TikTok posts have no title");
    assert_eq!(post.author.as_deref(), Some("Sample Cook"));
    assert!(post.text.contains("ingredients:"), "{}", post.text);
    assert!(post.text.contains("1 yellow onion"), "{}", post.text);
    assert_eq!(post.language.as_deref(), Some("en"));
    assert_eq!(post.duration_seconds, Some(150));
    assert_eq!(post.cover_urls.len(), 2);
    assert!(post.cover_urls[0].starts_with("https://p16-common-sign.tiktokcdn-eu.com/"));
    assert_ne!(post.cover_urls[0], post.cover_urls[1]);
    assert!(post.links.is_empty());
}

#[test]
fn the_tiktok_reader_finds_the_recipe_link_in_a_caption() {
    let post = read(
        TIKTOK_LINK,
        "https://www.tiktok.com/@samplehome/video/7449895843156528414",
    )
    .unwrap();
    assert_eq!(post.author.as_deref(), Some("Sample Home"));
    assert_eq!(
        post.links,
        vec!["https://samplehome.example.com/2024/10/17/that-viral-pasta/"]
    );
    assert_eq!(post.duration_seconds, Some(25));
}
