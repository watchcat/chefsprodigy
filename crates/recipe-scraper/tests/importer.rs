use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit};
use recipe_scraper::llm::{ExtractedRecipeDraft, HeuristicExtractor, RecipeExtractor};
use recipe_scraper::{RecipeImporter, ScraperError};

const PAGE: &str = include_str!("fixtures/jsonld_page.html");

/// Records what the importer sends to the "LLM" so tests can assert on token economy.
#[derive(Default)]
struct SpyExtractor {
    normalize_calls: Mutex<Vec<Vec<String>>>,
    extract_calls: Mutex<Vec<String>>,
}

impl RecipeExtractor for SpyExtractor {
    fn normalize_ingredients<'a>(
        &'a self,
        raw: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            self.normalize_calls.lock().unwrap().push(raw.to_vec());
            Ok(raw
                .iter()
                .map(|r| IngredientItem {
                    raw_text: r.clone(),
                    metric_amount: Some(1.0),
                    unit: MeasurementUnit::Pcs,
                    canonical_id: format!("llm_{}", r.split_whitespace().last().unwrap_or("x")),
                    is_specialty: r.contains("anise"),
                    canonical_label: None,
                })
                .collect())
        })
    }

    fn extract_recipe<'a>(
        &'a self,
        markdown: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            self.extract_calls
                .lock()
                .unwrap()
                .push(markdown.to_string());
            Ok(ExtractedRecipeDraft {
                title: "LLM Title".to_string(),
                description: None,
                servings: Some(3),
                prep_time_minutes: None,
                cook_time_minutes: Some(20),
                ingredients: vec![IngredientItem {
                    raw_text: "2 eggs".to_string(),
                    metric_amount: Some(2.0),
                    unit: MeasurementUnit::Pcs,
                    canonical_id: "egg".to_string(),
                    is_specialty: false,
                    canonical_label: None,
                }],
                instructions: vec![InstructionStep {
                    step_number: 1,
                    text: "Fry.".to_string(),
                    timer_seconds: Some(120),
                }],
                tags: vec!["breakfast".to_string()],
                language: None,
            })
        })
    }
}

#[tokio::test]
async fn html_with_jsonld_uses_structured_data_and_only_normalizes_ingredients() {
    let spy = Arc::new(SpyExtractor::default());
    let importer = RecipeImporter::new(spy.clone());

    let result = importer
        .import_html(PAGE, Some("https://example.com/pho"))
        .await
        .unwrap();
    let r = &result.recipe;
    assert_eq!(r.title, "Beef Pho");
    assert_eq!(r.servings, 4);
    assert_eq!(r.prep_time_minutes, Some(30));
    assert_eq!(r.cook_time_minutes, Some(360));
    assert_eq!(r.origin_url.as_deref(), Some("https://example.com/pho"));
    assert_eq!(r.instructions.len(), 3);
    assert_eq!(r.instructions[0].step_number, 1);
    assert_eq!(
        r.instructions[0].timer_seconds,
        Some(1800),
        "30 minutes extracted from step text"
    );
    assert_eq!(
        r.instructions[1].timer_seconds,
        Some(21600),
        "6 hours extracted from step text"
    );
    assert_eq!(r.instructions[2].timer_seconds, None);
    assert_eq!(
        r.tags,
        vec!["vietnamese", "soup", "dinner"],
        "tags deduplicated, lowercase"
    );
    assert_eq!(
        result.image_url.as_deref(),
        Some("https://example.com/pho.jpg")
    );

    assert_eq!(r.ingredients.len(), 4);
    assert_eq!(r.ingredients[1].canonical_id, "llm_anise");
    assert!(r.ingredients[1].is_specialty);

    assert_eq!(
        spy.normalize_calls.lock().unwrap().len(),
        1,
        "exactly one normalization call"
    );
    assert!(
        spy.extract_calls.lock().unwrap().is_empty(),
        "full extraction must NOT run when JSON-LD exists"
    );
}

#[tokio::test]
async fn html_without_jsonld_falls_back_to_markdown_extraction() {
    let spy = Arc::new(SpyExtractor::default());
    let importer = RecipeImporter::new(spy.clone());
    let html = "<html><head><script>var secret=1</script></head><body><nav>Menu</nav><h1>Eggs</h1><p>Fry two eggs.</p></body></html>";

    let result = importer.import_html(html, None).await.unwrap();
    assert_eq!(result.recipe.title, "LLM Title");
    assert_eq!(result.recipe.servings, 3);
    assert_eq!(result.recipe.ingredients[0].canonical_id, "egg");
    assert_eq!(result.recipe.instructions[0].timer_seconds, Some(120));

    let calls = spy.extract_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains("# Eggs"),
        "markdown sent to LLM: {}",
        calls[0]
    );
    assert!(
        !calls[0].contains("secret"),
        "script content must never reach the LLM"
    );
    assert!(!calls[0].contains("Menu"), "nav must be stripped");
    assert!(spy.normalize_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn raw_text_import_goes_straight_to_extraction() {
    let spy = Arc::new(SpyExtractor::default());
    let importer = RecipeImporter::new(spy.clone());
    let result = importer
        .import_text("Fry two eggs in butter.", Some("https://instagram.com/p/x"))
        .await
        .unwrap();
    assert_eq!(result.recipe.title, "LLM Title");
    assert_eq!(
        result.recipe.origin_url.as_deref(),
        Some("https://instagram.com/p/x")
    );
    assert_eq!(
        spy.extract_calls.lock().unwrap()[0],
        "Fry two eggs in butter."
    );
}

#[tokio::test]
async fn heuristic_extractor_handles_jsonld_without_any_llm() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let result = importer.import_html(PAGE, None).await.unwrap();
    let ing = &result.recipe.ingredients;
    assert_eq!(ing[0].canonical_id, "beef_bones");
    assert_eq!(
        (ing[0].metric_amount, ing[0].unit),
        (Some(1000.0), MeasurementUnit::G)
    );
    assert_eq!(ing[3].unit, MeasurementUnit::ToTaste);
}

#[tokio::test]
async fn heuristic_extractor_refuses_unstructured_text() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let err = importer
        .import_text("some caption", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::LlmUnavailable), "{err:?}");
    let err = importer
        .import_html("<p>no structured data</p>", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ScraperError::LlmUnavailable), "{err:?}");
}

#[tokio::test]
async fn empty_text_is_rejected_before_calling_llm() {
    let spy = Arc::new(SpyExtractor::default());
    let importer = RecipeImporter::new(spy.clone());
    let err = importer.import_text("   \n ", None).await.unwrap_err();
    assert!(matches!(err, ScraperError::EmptyInput), "{err:?}");
    assert!(spy.extract_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn relative_image_urls_are_resolved_against_origin() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let html = r#"<script type="application/ld+json">{"@type":"Recipe","name":"R","image":"/img/cover.jpg","recipeIngredient":["1 egg"],"recipeInstructions":"Fry."}</script>"#;
    let result = importer
        .import_html(html, Some("https://blog.example.com/recipes/r?x=1"))
        .await
        .unwrap();
    assert_eq!(
        result.image_url.as_deref(),
        Some("https://blog.example.com/img/cover.jpg")
    );

    let result = importer.import_html(html, None).await.unwrap();
    assert_eq!(
        result.image_url, None,
        "unresolvable relative URL is dropped"
    );
}

#[tokio::test]
async fn imports_carry_the_source_language() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let html = r#"<html lang="nl"><script type="application/ld+json">{"@type":"Recipe","name":"A","recipeIngredient":["1 ei"],"recipeInstructions":"Bak."}</script></html>"#;
    assert_eq!(
        importer
            .import_html(html, None)
            .await
            .unwrap()
            .recipe
            .language
            .as_deref(),
        Some("nl")
    );
    let mut draft_json =
        serde_json::json!({"title":"T","language":"RU","ingredients":[],"instructions":[]})
            .to_string();
    let draft = recipe_scraper::llm::parse_recipe_reply(&draft_json).unwrap();
    assert_eq!(draft.language.as_deref(), Some("ru"));
    draft_json = serde_json::json!({"title":"T","source_language":"Dutch","ingredients":[],"instructions":[]}).to_string();
    assert_eq!(
        recipe_scraper::llm::parse_recipe_reply(&draft_json)
            .unwrap()
            .language
            .as_deref(),
        Some("nl"),
        "language names map to codes"
    );
    assert_eq!(
        recipe_scraper::importer::input_from_draft(draft, None)
            .language
            .as_deref(),
        Some("ru")
    );
}

#[tokio::test]
async fn language_is_guessed_from_the_text_when_the_source_does_not_say() {
    let importer = RecipeImporter::new(Arc::new(HeuristicExtractor));
    let html = r#"<script type="application/ld+json">{"@type":"Recipe","name":"Stamppot met boerenkool","recipeIngredient":["1 kg aardappelen","300 g boerenkool","een scheutje melk"],"recipeInstructions":"Kook de aardappelen met de boerenkool en stamp alles fijn met de melk en de boter."}</script>"#;
    assert_eq!(
        importer
            .import_html(html, None)
            .await
            .unwrap()
            .recipe
            .language
            .as_deref(),
        Some("nl")
    );
}
