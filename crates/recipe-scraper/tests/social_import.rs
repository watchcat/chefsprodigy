//! The video-link ladder end to end against a local stand-in for YouTube, TikTok
//! and the linked recipe pages, with a fake model that counts its calls
//! (docs/SPEC-video-import.md).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit};
use recipe_scraper::importer::{ImportReport, ImportSource};
use recipe_scraper::llm::{
    ExtractedRecipeDraft, HeuristicExtractor, PostExtraction, RecipeExtractor,
};
use recipe_scraper::social::{MissingPart, SocialPost};
use recipe_scraper::{RecipeImporter, ScraperError};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const YOUTUBE: &str = "https://www.youtube.com/watch?v=AbCdEfGhIjK";
const TIKTOK: &str = "https://www.tiktok.com/@samplecook/video/7430634507935468843";

type Boxed<'a, T> = Pin<Box<dyn Future<Output = Result<T, ScraperError>> + Send + 'a>>;

/// A fake model: answers the caption call and the page extraction with preset
/// values and records what it was asked.
#[derive(Default)]
struct Spy {
    caption_reply: Option<PostExtraction>,
    page_draft: Option<ExtractedRecipeDraft>,
    caption_calls: Mutex<Vec<SocialPost>>,
    page_calls: Mutex<Vec<String>>,
    normalize_calls: Mutex<usize>,
}

impl RecipeExtractor for Spy {
    fn normalize_ingredients<'a>(&'a self, raw: &'a [String]) -> Boxed<'a, Vec<IngredientItem>> {
        Box::pin(async move {
            *self.normalize_calls.lock().unwrap() += 1;
            Ok(raw.iter().map(|r| ingredient(r)).collect())
        })
    }

    fn extract_recipe<'a>(&'a self, markdown: &'a str) -> Boxed<'a, ExtractedRecipeDraft> {
        Box::pin(async move {
            self.page_calls.lock().unwrap().push(markdown.to_string());
            self.page_draft
                .clone()
                .ok_or_else(|| ScraperError::Llm("page extraction failed".to_string()))
        })
    }

    fn extract_from_post<'a>(&'a self, post: &'a SocialPost) -> Boxed<'a, PostExtraction> {
        Box::pin(async move {
            self.caption_calls.lock().unwrap().push(post.clone());
            self.caption_reply
                .clone()
                .ok_or_else(|| ScraperError::Llm("caption call failed".to_string()))
        })
    }
}

fn ingredient(raw: &str) -> IngredientItem {
    IngredientItem {
        raw_text: raw.to_string(),
        metric_amount: Some(1.0),
        unit: MeasurementUnit::Pcs,
        canonical_id: raw.replace(' ', "_"),
        is_specialty: false,
        canonical_label: None,
    }
}

fn draft(title: &str, ingredients: usize, steps: usize) -> ExtractedRecipeDraft {
    ExtractedRecipeDraft {
        title: title.to_string(),
        description: None,
        servings: Some(2),
        prep_time_minutes: None,
        cook_time_minutes: None,
        ingredients: (0..ingredients)
            .map(|i| ingredient(&format!("item {i}")))
            .collect(),
        instructions: (0..steps)
            .map(|i| InstructionStep {
                step_number: (i + 1) as u16,
                text: format!("Do thing {i}."),
                timer_seconds: None,
            })
            .collect(),
        tags: Vec::new(),
        language: None,
    }
}

fn caption(recipe: Option<ExtractedRecipeDraft>, recipe_link: Option<&str>) -> PostExtraction {
    PostExtraction {
        recipe,
        missing: Vec::new(),
        recipe_link: recipe_link.map(str::to_string),
    }
}

/// A local site answering by path; unknown paths get a 404. Records every path asked.
struct Site {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Site {
    async fn start(build: impl FnOnce(&str) -> Vec<(&'static str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<HashMap<String, String>> = Arc::new(
            build(&base)
                .into_iter()
                .map(|(path, response)| (path.to_string(), response))
                .collect(),
        );
        let hits = Arc::new(Mutex::new(Vec::new()));
        let seen = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let routes = routes.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    seen.lock().unwrap().push(path.clone());
                    let response = routes.get(&path).cloned().unwrap_or_else(|| {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    });
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { base, hits }
    }

    fn hits(&self, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == path)
            .count()
    }

    fn importer(&self, extractor: Arc<dyn RecipeExtractor>) -> RecipeImporter {
        RecipeImporter::new(extractor)
            .allow_private_hosts()
            .with_social_origins(&self.base, &self.base)
    }
}

fn html(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn status(line: &str) -> String {
    format!("HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn redirect_to(location: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn youtube_page(description: &str) -> String {
    html(&youtube_body(description))
}

fn youtube_body(description: &str) -> String {
    let player = json!({
        "playabilityStatus": {"status": "OK"},
        "videoDetails": {
            "videoId": "AbCdEfGhIjK",
            "title": "One-Pan Lemon Chicken",
            "lengthSeconds": "612",
            "shortDescription": description,
            "author": "Sample Kitchen"
        }
    });
    format!(
        "<html><body><script>var ytInitialPlayerResponse = {player};var meta = 1;</script></body></html>"
    )
}

fn tiktok_page(desc: &str) -> String {
    let data = json!({"__DEFAULT_SCOPE__": {"webapp.video-detail": {
        "statusCode": 0,
        "itemInfo": {"itemStruct": {
            "desc": desc,
            "textLanguage": "nl",
            "author": {"nickname": "Sample Cook", "uniqueId": "samplecook"},
            "video": {"duration": 150, "cover": "https://cdn.example.com/cover.jpg", "originCover": "https://cdn.example.com/origin.jpg"}
        }}
    }}});
    html(&format!(
        r#"<html><body><script id="__UNIVERSAL_DATA_FOR_REHYDRATION__" type="application/json">{data}</script></body></html>"#
    ))
}

const JSONLD_PAGE: &str = r#"<html lang="en"><head><script type="application/ld+json">
{"@type":"Recipe","name":"Lemon Chicken (blog)","image":"/img/chicken.jpg",
 "recipeIngredient":["4 chicken thighs","2 lemons"],"recipeInstructions":["Brown the chicken.","Add lemon."]}
</script></head><body><h1>Lemon Chicken</h1></body></html>"#;

const PLAIN_PAGE: &str = "<html><body><h1>Lemon chicken, written out</h1><p>Brown four thighs, add lemon.</p></body></html>";

const YT_PATH: &str = "/watch?v=AbCdEfGhIjK";
const TT_PATH: &str = "/@samplecook/video/7430634507935468843";

#[tokio::test]
async fn a_linked_page_with_structured_data_is_imported_without_a_caption_call() {
    let site = Site::start(|base| {
        vec![
            (
                YT_PATH,
                youtube_page(&format!(
                    "Follow me https://www.instagram.com/samplekitchen\nRecipe: {base}/blog/lemon-chicken"
                )),
            ),
            ("/blog/lemon-chicken", html(JSONLD_PAGE)),
        ]
    })
    .await;
    let spy = Arc::new(Spy::default());
    let result = site
        .importer(spy.clone())
        .import_url("https://youtu.be/AbCdEfGhIjK?si=abc")
        .await
        .unwrap();

    assert_eq!(result.recipe.title, "Lemon Chicken (blog)");
    assert_eq!(result.recipe.ingredients.len(), 2);
    assert_eq!(result.recipe.instructions.len(), 2);
    assert_eq!(
        result.recipe.origin_url.as_deref(),
        Some(YOUTUBE),
        "the video stays the source"
    );
    assert_eq!(
        result.report,
        Some(ImportReport::new(ImportSource::LinkedPage, Vec::new()))
    );
    assert_eq!(
        result.image_urls,
        vec![
            "https://i.ytimg.com/vi/AbCdEfGhIjK/maxresdefault.jpg".to_string(),
            "https://i.ytimg.com/vi/AbCdEfGhIjK/hqdefault.jpg".to_string(),
            format!("{}/img/chicken.jpg", site.base),
        ],
        "the video's covers first, then the linked page's picture"
    );
    assert!(
        spy.caption_calls.lock().unwrap().is_empty(),
        "no caption call"
    );
    assert!(
        spy.page_calls.lock().unwrap().is_empty(),
        "no page extraction"
    );
    assert_eq!(*spy.normalize_calls.lock().unwrap(), 1);
    assert_eq!(site.hits(YT_PATH), 1);
    assert_eq!(site.hits("/blog/lemon-chicken"), 1);
}

#[tokio::test]
async fn a_complete_caption_is_one_model_call_and_no_linked_fetch() {
    let site =
        Site::start(|_| vec![(TT_PATH, tiktok_page("ingredients: ... instructions: ..."))]).await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Tomato pasta", 4, 4)), None)),
        ..Spy::default()
    });
    let result = site
        .importer(spy.clone())
        .import_url(&format!("{TIKTOK}?lang=en"))
        .await
        .unwrap();

    assert_eq!(result.recipe.title, "Tomato pasta");
    assert_eq!(result.recipe.origin_url.as_deref(), Some(TIKTOK));
    assert_eq!(
        result.recipe.language.as_deref(),
        Some("nl"),
        "TikTok's caption language fills in when the model names none"
    );
    assert_eq!(
        result.report,
        Some(ImportReport::new(ImportSource::Caption, Vec::new()))
    );
    assert_eq!(
        result.image_urls,
        vec![
            "https://cdn.example.com/cover.jpg",
            "https://cdn.example.com/origin.jpg"
        ]
    );
    let calls = spy.caption_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one caption call");
    assert_eq!(calls[0].text, "ingredients: ... instructions: ...");
    assert_eq!(calls[0].author.as_deref(), Some("Sample Cook"));
    assert!(spy.page_calls.lock().unwrap().is_empty());
    assert_eq!(
        site.hits.lock().unwrap().len(),
        1,
        "only the video page was fetched"
    );
}

#[tokio::test]
async fn the_caption_tells_the_language_when_nothing_else_does() {
    let dutch = "Stamppot met boerenkool voor de hele week. Kook de aardappelen met de boerenkool \
                 en stamp alles fijn met de melk en de boter. Ingrediënten: aardappelen, boerenkool, een scheutje melk.";
    let site = Site::start(|_| vec![(YT_PATH, youtube_page(dutch))]).await;
    // The draft's own wording ("item 0", "Do thing 0.") gives the detector nothing.
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Stamppot", 2, 2)), None)),
        ..Spy::default()
    });
    let result = site.importer(spy).import_url(YOUTUBE).await.unwrap();
    assert_eq!(result.recipe.language.as_deref(), Some("nl"));
}

#[tokio::test]
async fn the_page_the_caption_call_names_is_imported_from_its_fetched_copy() {
    let site = Site::start(|base| {
        vec![
            (
                TT_PATH,
                tiktok_page(&format!(
                    "grab the recipe right here: {base}/blog/plain  #pasta"
                )),
            ),
            ("/blog/plain", html(PLAIN_PAGE)),
        ]
    })
    .await;
    let named = format!("{}/blog/plain", site.base);
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, Some(&named))),
        page_draft: Some(draft("Lemon chicken", 2, 3)),
        ..Spy::default()
    });
    let result = site.importer(spy.clone()).import_url(TIKTOK).await.unwrap();

    assert_eq!(result.recipe.title, "Lemon chicken");
    assert_eq!(result.recipe.origin_url.as_deref(), Some(TIKTOK));
    assert_eq!(
        result.report,
        Some(ImportReport::new(ImportSource::LinkedPage, Vec::new()))
    );
    assert_eq!(spy.caption_calls.lock().unwrap().len(), 1);
    let pages = spy.page_calls.lock().unwrap();
    assert_eq!(pages.len(), 1);
    assert!(
        pages[0].contains("Lemon chicken, written out"),
        "{}",
        pages[0]
    );
    assert_eq!(
        site.hits("/blog/plain"),
        1,
        "the free pass's copy is reused"
    );
}

#[tokio::test]
async fn a_link_the_model_names_that_is_not_in_the_caption_is_never_fetched() {
    let site = Site::start(|base| {
        vec![
            (TT_PATH, tiktok_page(&format!("see {base}/blog/plain"))),
            ("/blog/plain", html(PLAIN_PAGE)),
            ("/elsewhere", html(JSONLD_PAGE)),
        ]
    })
    .await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, Some(&format!("{}/elsewhere", site.base)))),
        page_draft: Some(draft("Should not be used", 2, 2)),
        ..Spy::default()
    });
    let err = site
        .importer(spy.clone())
        .import_url(TIKTOK)
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::NoRecipeInPost(_)), "{err:?}");
    assert_eq!(site.hits("/elsewhere"), 0);
    assert!(spy.page_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_named_link_on_the_skip_list_is_not_followed() {
    let site = Site::start(|_| {
        vec![(
            TT_PATH,
            tiktok_page("recipe on https://www.instagram.com/p/abc/"),
        )]
    })
    .await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, Some("https://www.instagram.com/p/abc/"))),
        page_draft: Some(draft("Should not be used", 2, 2)),
        ..Spy::default()
    });
    let err = site
        .importer(spy.clone())
        .import_url(TIKTOK)
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::NoRecipeInPost(_)), "{err:?}");
    assert!(spy.page_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_ingredients_only_caption_is_saved_as_a_partial_recipe() {
    let site = Site::start(|_| vec![(TT_PATH, tiktok_page("ingredients: onion, garlic"))]).await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Tomato pasta", 3, 0)), None)),
        ..Spy::default()
    });
    let result = site.importer(spy).import_url(TIKTOK).await.unwrap();
    assert_eq!(result.recipe.ingredients.len(), 3);
    assert!(result.recipe.instructions.is_empty());
    let report = result.report.unwrap();
    assert_eq!(report.source, ImportSource::Caption);
    assert_eq!(report.missing, vec![MissingPart::Instructions]);
    assert!(report.note.is_some(), "a partial import explains itself");

    let site = Site::start(|_| vec![(TT_PATH, tiktok_page("method: mix, bake"))]).await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Cake", 0, 2)), None)),
        ..Spy::default()
    });
    let report = site
        .importer(spy)
        .import_url(TIKTOK)
        .await
        .unwrap()
        .report
        .unwrap();
    assert_eq!(report.missing, vec![MissingPart::Ingredients]);
}

#[tokio::test]
async fn a_named_page_without_a_full_recipe_leaves_the_caption_draft() {
    let site = Site::start(|base| {
        vec![
            (
                TT_PATH,
                tiktok_page(&format!("ingredients: onion. method: {base}/blog/plain")),
            ),
            ("/blog/plain", html(PLAIN_PAGE)),
        ]
    })
    .await;
    let named = format!("{}/blog/plain", site.base);
    // The page extraction fails outright; the caption's partial draft is still saved.
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Tomato pasta", 2, 0)), Some(&named))),
        page_draft: None,
        ..Spy::default()
    });
    let result = site.importer(spy).import_url(TIKTOK).await.unwrap();
    assert_eq!(result.recipe.title, "Tomato pasta");
    assert_eq!(
        result.report.unwrap().missing,
        vec![MissingPart::Instructions]
    );

    // With nothing in the caption either, the page's failure is what the cook hears about.
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, Some(&named))),
        page_draft: None,
        ..Spy::default()
    });
    let err = site.importer(spy).import_url(TIKTOK).await.unwrap_err();
    assert!(matches!(err, ScraperError::Llm(_)), "{err:?}");
}

#[tokio::test]
async fn no_recipe_anywhere_is_its_own_error_and_an_empty_caption_skips_the_model() {
    let site = Site::start(|_| {
        vec![
            (TT_PATH, tiktok_page("look at this #fyp")),
            (YT_PATH, youtube_page("")),
        ]
    })
    .await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, None)),
        ..Spy::default()
    });
    let importer = site.importer(spy.clone());
    let err = importer.import_url(TIKTOK).await.unwrap_err();
    assert!(matches!(err, ScraperError::NoRecipeInPost(_)), "{err:?}");
    assert_eq!(spy.caption_calls.lock().unwrap().len(), 1);

    let err = importer.import_url(YOUTUBE).await.unwrap_err();
    assert!(matches!(err, ScraperError::NoRecipeInPost(_)), "{err:?}");
    assert_eq!(
        spy.caption_calls.lock().unwrap().len(),
        1,
        "an empty description is not sent to the model"
    );
}

#[tokio::test]
async fn at_most_three_linked_pages_plus_the_named_one_are_fetched() {
    let site = Site::start(|base| {
        let description = (1..=5)
            .map(|i| format!("{base}/p{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut routes = vec![(YT_PATH, youtube_page(&description))];
        for path in ["/p1", "/p2", "/p3", "/p4", "/p5"] {
            routes.push((path, html(PLAIN_PAGE)));
        }
        routes
    })
    .await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(None, Some(&format!("{}/p5", site.base)))),
        page_draft: Some(draft("From page five", 1, 1)),
        ..Spy::default()
    });
    let result = site.importer(spy).import_url(YOUTUBE).await.unwrap();
    assert_eq!(result.recipe.title, "From page five");
    for (path, expected) in [("/p1", 1), ("/p2", 1), ("/p3", 1), ("/p4", 0), ("/p5", 1)] {
        assert_eq!(site.hits(path), expected, "{path}");
    }
}

#[tokio::test]
async fn a_linked_page_that_fails_is_skipped() {
    let site = Site::start(|base| {
        vec![
            (
                TT_PATH,
                tiktok_page(&format!("{base}/broken and {base}/blog/lemon-chicken")),
            ),
            ("/broken", status("500 Internal Server Error")),
            ("/blog/lemon-chicken", html(JSONLD_PAGE)),
        ]
    })
    .await;
    let result = site
        .importer(Arc::new(Spy::default()))
        .import_url(TIKTOK)
        .await
        .unwrap();
    assert_eq!(result.recipe.title, "Lemon Chicken (blog)");
    assert_eq!(site.hits("/broken"), 1);
}

#[tokio::test]
async fn unavailable_and_unreadable_video_pages_are_told_apart() {
    let gone_youtube = include_str!("fixtures/social/youtube_unavailable.html");
    let gone_tiktok = include_str!("fixtures/social/tiktok_unavailable.html");
    let site = Site::start(|_| {
        vec![
            (YT_PATH, html(gone_youtube)),
            (TT_PATH, html(gone_tiktok)),
            (
                "/watch?v=bbbbbbbbbbb",
                html("<html><body>Before you continue</body></html>"),
            ),
            ("/watch?v=ccccccccccc", status("404 Not Found")),
            ("/watch?v=ddddddddddd", status("403 Forbidden")),
            // A page any reader would accept, padded past the 4 MB cap.
            (
                "/watch?v=eeeeeeeeeee",
                html(&format!(
                    "{}<!--{}-->",
                    youtube_body("Ingredients: 1 egg. Method: fry it."),
                    "a".repeat(4 * 1024 * 1024)
                )),
            ),
        ]
    })
    .await;
    let importer = site.importer(Arc::new(Spy::default()));
    for url in [YOUTUBE, TIKTOK, "https://youtu.be/ccccccccccc"] {
        let err = importer.import_url(url).await.unwrap_err();
        assert!(
            matches!(err, ScraperError::PostUnavailable(_)),
            "{url}: {err:?}"
        );
    }
    for url in [
        "https://youtu.be/bbbbbbbbbbb",
        "https://youtu.be/ddddddddddd",
        "https://youtu.be/eeeeeeeeeee",
    ] {
        let err = importer.import_url(url).await.unwrap_err();
        assert!(
            matches!(err, ScraperError::PostUnreadable(_)),
            "{url}: {err:?}"
        );
    }
    let oversized = importer
        .import_url("https://youtu.be/eeeeeeeeeee")
        .await
        .unwrap_err();
    assert!(oversized.to_string().contains("4 MB"), "{oversized}");
}

#[tokio::test]
async fn instagram_links_are_refused_without_a_request() {
    let site = Site::start(|_| Vec::new()).await;
    let err = site
        .importer(Arc::new(Spy::default()))
        .import_url("https://www.instagram.com/reel/CyjUMFjP1BB/")
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::UnsupportedSource(_)), "{err:?}");
    assert!(err.to_string().contains("Instagram"), "{err}");
    assert!(site.hits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tiktok_share_link_resolves_to_its_video() {
    let site = Site::start(|base| {
        vec![
            (
                "/ZMabc123/",
                redirect_to(&format!("{base}{TT_PATH}?_r=1&share=x")),
            ),
            (
                "/@samplecook/video/7430634507935468843?_r=1&share=x",
                tiktok_page("ingredients: ... instructions: ..."),
            ),
            ("/t/ZTdead/", redirect_to(&format!("{base}/?_r=1"))),
            (
                "/?_r=1",
                html("<html><body>TikTok - Make Your Day</body></html>"),
            ),
        ]
    })
    .await;
    let spy = Arc::new(Spy {
        caption_reply: Some(caption(Some(draft("Tomato pasta", 2, 2)), None)),
        ..Spy::default()
    });
    let importer = site.importer(spy);
    let result = importer
        .import_url("https://vm.tiktok.com/ZMabc123/")
        .await
        .unwrap();
    assert_eq!(result.recipe.origin_url.as_deref(), Some(TIKTOK));

    let err = importer
        .import_url("https://www.tiktok.com/t/ZTdead/")
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::PostUnavailable(_)), "{err:?}");
}

#[tokio::test]
async fn without_a_model_only_structured_linked_pages_import() {
    let site = Site::start(|base| {
        vec![
            (
                YT_PATH,
                youtube_page(&format!("Recipe: {base}/blog/lemon-chicken")),
            ),
            ("/blog/lemon-chicken", html(JSONLD_PAGE)),
            (TT_PATH, tiktok_page("ingredients: onion, garlic")),
        ]
    })
    .await;
    let importer = site.importer(Arc::new(HeuristicExtractor));
    let result = importer.import_url(YOUTUBE).await.unwrap();
    assert_eq!(result.recipe.title, "Lemon Chicken (blog)");
    let err = importer.import_url(TIKTOK).await.unwrap_err();
    assert!(matches!(err, ScraperError::LlmUnavailable), "{err:?}");
}

#[tokio::test]
async fn other_links_keep_the_normal_import_and_carry_no_report() {
    let site = Site::start(|_| vec![("/blog/lemon-chicken", html(JSONLD_PAGE))]).await;
    let result = site
        .importer(Arc::new(Spy::default()))
        .import_url(&format!("{}/blog/lemon-chicken", site.base))
        .await
        .unwrap();
    assert_eq!(result.report, None);
    assert_eq!(
        result.image_urls,
        vec![format!("{}/img/chicken.jpg", site.base)]
    );
}
