use recipe_scraper::jsonld::{iso8601_duration_minutes, parse_schema_org};

const PAGE: &str = include_str!("fixtures/jsonld_page.html");

#[test]
fn finds_recipe_inside_graph_wrapper() {
    let r = parse_schema_org(PAGE).expect("recipe found");
    assert_eq!(r.name, "Beef Pho");
    assert_eq!(r.description.as_deref(), Some("A classic."));
    assert_eq!(
        r.ingredients,
        vec![
            "1 kg beef bones",
            "3 star anise",
            "2 tbsp fish sauce",
            "salt to taste"
        ]
    );
    assert_eq!(
        r.instructions,
        vec![
            "Roast the bones for 30 minutes.",
            "Simmer for 6 hours.",
            "Serve & enjoy."
        ]
    );
    assert_eq!(r.servings, Some(4));
    assert_eq!(r.prep_minutes, Some(30));
    assert_eq!(r.cook_minutes, Some(360));
    assert_eq!(r.image_url.as_deref(), Some("https://example.com/pho.jpg"));
    assert_eq!(r.tags, vec!["vietnamese", "soup", "vietnamese", "dinner"]);
}

#[test]
fn finds_top_level_recipe_and_array_forms() {
    let top = r#"<script type="application/ld+json">{"@type":"Recipe","name":"A","recipeIngredient":["x"],"recipeInstructions":"Do it."}</script>"#;
    let r = parse_schema_org(top).unwrap();
    assert_eq!(r.name, "A");
    assert_eq!(r.instructions, vec!["Do it."]);

    let arr = r#"<script type="application/ld+json">[{"@type":"Thing"},{"@type":["Recipe","Thing"],"name":"B","recipeIngredient":["y"],"recipeInstructions":["Step 1","Step 2"],"image":["https://i/1.jpg","https://i/2.jpg"],"recipeYield":"6"}]</script>"#;
    let r = parse_schema_org(arr).unwrap();
    assert_eq!(r.name, "B");
    assert_eq!(r.instructions, vec!["Step 1", "Step 2"]);
    assert_eq!(r.image_url.as_deref(), Some("https://i/1.jpg"));
    assert_eq!(r.servings, Some(6));
}

#[test]
fn ignores_pages_without_recipe_or_with_broken_json() {
    assert!(parse_schema_org("<html><body>nothing</body></html>").is_none());
    assert!(parse_schema_org(r#"<script type="application/ld+json">{not json</script>"#).is_none());
    assert!(
        parse_schema_org(r#"<script type="application/ld+json">{"@type":"Article"}</script>"#)
            .is_none()
    );
}

#[test]
fn instructions_strip_html_tags() {
    let html = r#"<script type="application/ld+json">{"@type":"Recipe","name":"C","recipeInstructions":"<p>Mix <b>well</b>.</p><p>Bake.</p>"}</script>"#;
    let r = parse_schema_org(html).unwrap();
    assert_eq!(r.instructions, vec!["Mix well.", "Bake."]);
}

#[test]
fn parses_iso8601_durations() {
    assert_eq!(iso8601_duration_minutes("PT30M"), Some(30));
    assert_eq!(iso8601_duration_minutes("PT1H30M"), Some(90));
    assert_eq!(iso8601_duration_minutes("PT2H"), Some(120));
    assert_eq!(iso8601_duration_minutes("P1DT1H"), Some(1500));
    assert_eq!(iso8601_duration_minutes("PT90S"), Some(2));
    assert_eq!(iso8601_duration_minutes("PT0M"), Some(0));
    assert_eq!(iso8601_duration_minutes("45 minutes"), None);
    assert_eq!(iso8601_duration_minutes(""), None);
}

#[test]
fn language_comes_from_in_language_or_html_lang() {
    let html = r#"<html lang="nl-NL"><script type="application/ld+json">{"@type":"Recipe","name":"A","recipeIngredient":["x"],"recipeInstructions":"Do."}</script></html>"#;
    assert_eq!(
        parse_schema_org(html).unwrap().language.as_deref(),
        Some("nl")
    );
    let html = r#"<html lang="nl"><script type="application/ld+json">{"@type":"Recipe","name":"A","inLanguage":"ru-RU","recipeIngredient":["x"],"recipeInstructions":"Do."}</script></html>"#;
    assert_eq!(
        parse_schema_org(html).unwrap().language.as_deref(),
        Some("ru"),
        "inLanguage wins"
    );
    let html = r#"<script type="application/ld+json">{"@type":"Recipe","name":"A","recipeIngredient":["x"],"recipeInstructions":"Do."}</script>"#;
    assert_eq!(parse_schema_org(html).unwrap().language, None);
}
