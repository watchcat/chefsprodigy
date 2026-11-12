//! The caption call of a video import: what is sent to the model and how its
//! reply is read (docs/SPEC-video-import.md).

use recipe_scraper::ScraperError;
use recipe_scraper::llm::{parse_post_reply, post_prompt};
use recipe_scraper::social::{MissingPart, Platform, SocialPost};
use serde_json::json;

fn post(text: &str) -> SocialPost {
    SocialPost {
        platform: Platform::YouTube,
        canonical_url: "https://www.youtube.com/watch?v=AbCdEfGhIjK".to_string(),
        title: Some("One-Pan Lemon Chicken".to_string()),
        author: Some("Sample Kitchen".to_string()),
        text: text.to_string(),
        links: vec!["https://samplekitchen.example.com/lemon-chicken".to_string()],
        cover_urls: Vec::new(),
        duration_seconds: Some(612),
        language: None,
    }
}

fn ingredient(raw: &str, id: &str) -> serde_json::Value {
    json!({"raw_text": raw, "metric_amount": 1, "unit": "pcs", "canonical_id": id, "is_specialty": false})
}

#[test]
fn a_full_recipe_in_the_caption_is_read_with_its_link() {
    let reply = json!({
        "recipe": {
            "title": "Lemon Chicken",
            "ingredients": [ingredient("4 chicken thighs", "chicken_thigh")],
            "instructions": [{"step_number": 1, "text": "Brown the chicken.", "timer_seconds": 300}],
            "tags": ["Dinner"],
            "language": "en"
        },
        "missing": [],
        "recipe_link": " https://samplekitchen.example.com/lemon-chicken "
    })
    .to_string();
    let extraction = parse_post_reply(&reply, Some("One-Pan Lemon Chicken")).unwrap();
    let recipe = extraction.recipe.expect("a recipe");
    assert_eq!(recipe.title, "Lemon Chicken", "the model's title wins");
    assert_eq!(recipe.ingredients.len(), 1);
    assert_eq!(recipe.instructions[0].timer_seconds, Some(300));
    assert_eq!(recipe.language.as_deref(), Some("en"));
    assert!(extraction.missing.is_empty());
    assert_eq!(
        extraction.recipe_link.as_deref(),
        Some("https://samplekitchen.example.com/lemon-chicken")
    );
}

#[test]
fn a_caption_without_a_recipe_can_still_name_the_link() {
    let reply = r#"Here you go:
```json
{"recipe": null, "missing": ["ingredients", "instructions"], "recipe_link": "https://samplekitchen.example.com/lemon-chicken"}
```"#;
    let extraction = parse_post_reply(reply, None).unwrap();
    assert_eq!(extraction.recipe, None);
    assert_eq!(
        extraction.missing,
        vec![MissingPart::Ingredients, MissingPart::Instructions]
    );
    assert_eq!(
        extraction.recipe_link.as_deref(),
        Some("https://samplekitchen.example.com/lemon-chicken")
    );

    let nothing = parse_post_reply(
        r#"{"recipe": null, "missing": [], "recipe_link": null}"#,
        None,
    )
    .unwrap();
    assert_eq!(nothing.recipe, None);
    assert_eq!(nothing.recipe_link, None);
    let blank_link = parse_post_reply(r#"{"recipe": null, "recipe_link": "  "}"#, None).unwrap();
    assert_eq!(blank_link.recipe_link, None);
}

#[test]
fn an_ingredients_only_caption_gives_a_partial_draft() {
    let reply = json!({
        "recipe": {
            "title": "Tomato pasta",
            "ingredients": [ingredient("1 yellow onion", "yellow_onion"), ingredient("250 g pasta", "pasta")],
            "instructions": []
        },
        "missing": ["steps"]
    })
    .to_string();
    let extraction = parse_post_reply(&reply, None).unwrap();
    let recipe = extraction.recipe.expect("a partial recipe");
    assert_eq!(recipe.ingredients.len(), 2);
    assert!(recipe.instructions.is_empty());
    assert_eq!(extraction.missing, vec![MissingPart::Instructions]);
    assert_eq!(extraction.recipe_link, None);
}

#[test]
fn a_recipe_without_a_title_takes_the_video_title_then_the_ingredients() {
    let untitled = json!({
        "recipe": {
            "ingredients": [ingredient("4 chicken thighs", "chicken_thigh")],
            "instructions": ["Brown the chicken."]
        }
    })
    .to_string();
    let with_video_title = parse_post_reply(&untitled, Some("One-Pan Lemon Chicken")).unwrap();
    assert_eq!(
        with_video_title.recipe.unwrap().title,
        "One-Pan Lemon Chicken"
    );

    let synthesized = parse_post_reply(&untitled, None).unwrap().recipe.unwrap();
    assert!(
        synthesized.title.to_lowercase().contains("chicken"),
        "named by its ingredients: {}",
        synthesized.title
    );

    let steps_only =
        json!({"recipe": {"ingredients": [], "instructions": ["Mix.", "Bake."]}}).to_string();
    let recipe = parse_post_reply(&steps_only, None).unwrap().recipe.unwrap();
    assert_eq!(recipe.instructions.len(), 2);
    assert!(!recipe.title.trim().is_empty(), "a title is always present");
}

#[test]
fn a_recipe_at_the_top_level_is_accepted() {
    let reply = json!({
        "title": "Flatbread",
        "ingredients": [ingredient("200 g flour", "flour")],
        "instructions": ["Mix and fry."],
        "recipe_link": null
    })
    .to_string();
    let extraction = parse_post_reply(&reply, None).unwrap();
    assert_eq!(extraction.recipe.unwrap().title, "Flatbread");
}

#[test]
fn an_empty_recipe_object_counts_as_no_recipe() {
    for reply in [
        r#"{"recipe": {}, "missing": ["ingredients", "instructions"]}"#,
        r#"{"recipe": {"title": "Pasta night", "ingredients": [], "instructions": []}}"#,
        r#"{"recipe": false}"#,
    ] {
        let extraction = parse_post_reply(reply, Some("Video title")).unwrap();
        assert_eq!(extraction.recipe, None, "{reply}");
    }
}

#[test]
fn replies_that_are_not_the_expected_json_are_model_errors() {
    for reply in ["I could not find a recipe.", "[1, 2, 3]", ""] {
        let err = parse_post_reply(reply, None).unwrap_err();
        assert!(matches!(err, ScraperError::Llm(_)), "{reply:?}: {err:?}");
    }
}

#[test]
fn the_prompt_carries_title_creator_links_and_caption_only() {
    let prompt = post_prompt(&post(
        "Ingredients:\n4 chicken thighs\n<script>alert(1)</script>",
    ));
    assert!(prompt.contains("One-Pan Lemon Chicken"), "{prompt}");
    assert!(prompt.contains("Sample Kitchen"), "{prompt}");
    assert!(
        prompt.contains("https://samplekitchen.example.com/lemon-chicken"),
        "{prompt}"
    );
    assert!(prompt.contains("4 chicken thighs"), "{prompt}");
    assert!(
        !prompt.contains("i.ytimg.com") && !prompt.contains("watch?v="),
        "neither covers nor the video link are sent: {prompt}"
    );
}

#[test]
fn the_prompt_caps_the_caption_at_twenty_thousand_characters() {
    let long = "щ".repeat(25_000);
    let prompt = post_prompt(&post(&long));
    assert_eq!(prompt.chars().filter(|c| *c == 'щ').count(), 20_000);

    let untitled = SocialPost {
        title: None,
        author: None,
        links: Vec::new(),
        ..post("just a caption")
    };
    let prompt = post_prompt(&untitled);
    assert!(prompt.contains("just a caption"), "{prompt}");
    assert!(
        !prompt.contains("Title:") && !prompt.contains("Creator:"),
        "{prompt}"
    );
}
