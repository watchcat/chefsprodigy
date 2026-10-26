//! The model's JSON is rarely perfect: fields go missing or get renamed.
//! Parsing must fill sensible defaults instead of failing the whole import.

use recipe_core::MeasurementUnit;
use recipe_scraper::ScraperError;
use recipe_scraper::llm::{parse_ingredients_reply, parse_recipe_reply};

#[test]
fn recipe_reply_fills_missing_raw_text_and_unit() {
    let reply = r#"{
      "title": "Garlic butter shrimp",
      "servings": 2,
      "ingredients": [
        {"metric_amount": 300, "unit": "g", "canonical_id": "shrimp", "is_specialty": false},
        {"canonical_id": "salt"},
        {"raw_text": "3 cloves garlic", "metric_amount": 3, "canonical_id": "garlic"}
      ],
      "instructions": [
        {"text": "Melt butter, cook 1 minute."},
        {"step_number": 2, "text": "Fry shrimp 3 minutes.", "timer_seconds": 180}
      ],
      "tags": ["Seafood"]
    }"#;
    let draft = parse_recipe_reply(reply).unwrap();
    assert_eq!(draft.title, "Garlic butter shrimp");
    assert_eq!(draft.ingredients.len(), 3);
    assert_eq!(
        draft.ingredients[0].raw_text, "300 g shrimp",
        "raw_text composed from the parts"
    );
    assert_eq!(
        (
            draft.ingredients[0].metric_amount,
            draft.ingredients[0].unit
        ),
        (Some(300.0), MeasurementUnit::G)
    );
    assert_eq!(draft.ingredients[1].raw_text, "salt");
    assert_eq!(
        (
            draft.ingredients[1].metric_amount,
            draft.ingredients[1].unit
        ),
        (None, MeasurementUnit::ToTaste)
    );
    assert_eq!(
        draft.ingredients[2].unit,
        MeasurementUnit::Pcs,
        "amount without unit means pieces"
    );
    assert_eq!(draft.instructions[0].step_number, 1);
    assert_eq!(draft.instructions[0].timer_seconds, None);
    assert_eq!(draft.instructions[1].timer_seconds, Some(180));
}

#[test]
fn recipe_reply_accepts_common_field_aliases_and_fences() {
    let reply = "Here you go:\n```json\n{\"name\": \"Toast\", \"ingredients\": [{\"ingredient\": \"bread\", \"amount\": 2, \"text\": \"2 slices bread\"}], \"steps\": [\"Toast it.\"]}\n```";
    let draft = parse_recipe_reply(reply).unwrap();
    assert_eq!(draft.title, "Toast");
    assert_eq!(draft.ingredients[0].canonical_id, "bread");
    assert_eq!(draft.ingredients[0].raw_text, "2 slices bread");
    assert_eq!(draft.ingredients[0].metric_amount, Some(2.0));
    assert_eq!(draft.instructions[0].text, "Toast it.");
}

#[test]
fn ingredient_reply_accepts_bare_array_or_wrapped_object() {
    let wrapped = r#"{"ingredients":[{"raw_text":"1 egg","metric_amount":1,"unit":"pcs","canonical_id":"egg","is_specialty":false}]}"#;
    assert_eq!(
        parse_ingredients_reply(wrapped).unwrap()[0].canonical_id,
        "egg"
    );
    let bare = r#"[{"raw_text":"1 egg","canonical_id":"egg"}]"#;
    let items = parse_ingredients_reply(bare).unwrap();
    assert_eq!(items[0].canonical_id, "egg");
    assert_eq!(
        (items[0].metric_amount, items[0].unit),
        (Some(1.0), MeasurementUnit::Pcs),
        "quantity read from raw_text"
    );
}

#[test]
fn unusable_reply_is_an_llm_error_naming_the_problem() {
    let err = parse_recipe_reply("Sorry, I cannot help with that.").unwrap_err();
    assert!(matches!(err, ScraperError::Llm(_)), "{err:?}");
    let err = parse_recipe_reply(r#"{"ingredients": []}"#).unwrap_err();
    assert!(
        matches!(&err, ScraperError::Llm(m) if m.contains("title")),
        "{err:?}"
    );
}

#[test]
fn canonical_id_is_normalized_to_snake_case() {
    let draft = parse_recipe_reply(
        r#"{"title":"T","ingredients":[{"canonical_id":"Dark Soy-Sauce "}],"instructions":[]}"#,
    )
    .unwrap();
    assert_eq!(draft.ingredients[0].canonical_id, "dark_soy_sauce");
}

#[test]
fn recipe_reply_unwraps_outer_objects_and_extra_title_aliases() {
    let wrapped = r#"{"recipe": {"recipe_name": "Pho", "ingredients": [], "instructions": []}}"#;
    assert_eq!(parse_recipe_reply(wrapped).unwrap().title, "Pho");
    let data = r#"{"data": {"result": {"dish": "Toast", "ingredients": [], "instructions": []}}}"#;
    assert_eq!(parse_recipe_reply(data).unwrap().title, "Toast");
}

#[test]
fn schema_echo_is_reported_as_such() {
    let echo =
        r#"{"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}"#;
    let err = parse_recipe_reply(echo).unwrap_err();
    assert!(
        matches!(&err, ScraperError::Llm(m) if m.contains("schema") && m.contains("echo")),
        "{err:?}"
    );
}

#[test]
fn recipe_reply_accepts_loose_numbers_times_and_step_numbers() {
    let reply = r#"{
      "title": "Zucchini stir-fry",
      "servings": "3–4",
      "total_time_seconds": 600,
      "equipment": ["wok"],
      "ingredients": [
        {"raw_text": "Сахар: 0,5 ч. л.", "metric_amount": "2,5", "unit": "Grams", "canonical_id": "sugar", "is_specialty": false},
        {"raw_text": "2 tbsp butter", "metric_amount": 2, "unit": "tbsp", "canonical_id": "butter"},
        {"raw_text": "1 egg", "metric_amount": "1", "unit": "piece", "canonical_id": "egg"}
      ],
      "steps": [
        {"number": 1, "text": "Toast the pepper.", "timer_seconds": "60"},
        {"number": 2, "text": "Slice.", "timer_seconds": null}
      ]
    }"#;
    let d = parse_recipe_reply(reply).unwrap();
    assert_eq!(d.servings, Some(3), "first number of a range");
    assert_eq!(
        d.cook_time_minutes,
        Some(10),
        "total seconds become cook minutes when nothing else is given"
    );
    assert_eq!(
        (d.ingredients[0].metric_amount, d.ingredients[0].unit),
        (Some(2.5), MeasurementUnit::G)
    );
    assert_eq!(
        (d.ingredients[1].metric_amount, d.ingredients[1].unit),
        (Some(30.0), MeasurementUnit::Ml),
        "unknown unit falls back to parsing raw_text"
    );
    assert_eq!(
        (d.ingredients[2].metric_amount, d.ingredients[2].unit),
        (Some(1.0), MeasurementUnit::Pcs)
    );
    assert_eq!(d.instructions[0].step_number, 1);
    assert_eq!(d.instructions[0].timer_seconds, Some(60));
    assert_eq!(d.instructions[1].step_number, 2);
}

#[test]
fn recipe_reply_accepts_time_strings_and_serving_phrases() {
    let reply = r#"{"title":"T","servings":"4 servings","prep_time":"15 minutes","cook_time_minutes":"1 hour 10 min","ingredients":[],"instructions":[]}"#;
    let d = parse_recipe_reply(reply).unwrap();
    assert_eq!(d.servings, Some(4));
    assert_eq!(d.prep_time_minutes, Some(15));
    assert_eq!(d.cook_time_minutes, Some(70));
}

mod photo_reply {
    use recipe_scraper::ScraperError;
    use recipe_scraper::llm::{PhotoKind, parse_photo_reply};

    #[test]
    fn nested_recipe_and_photo_kinds_are_read_leniently() {
        let reply = r#"{
          "recipe": {"title": "Grandma's borscht", "servings": "6", "ingredients": [{"raw_text": "2 beets", "canonical_id": "beetroot"}], "steps": ["Boil."]},
          "photos": [{"index": 0, "kind": "recipe-text", "notes": "handwritten"}, {"kind": "Dish"}, {"index": 2, "kind": "something else"}],
          "unreadable": ["quantity of dill"],
          "language": "ru"
        }"#;
        let x = parse_photo_reply(reply).unwrap();
        assert_eq!(x.recipe.title, "Grandma's borscht");
        assert_eq!(x.recipe.servings, Some(6));
        assert_eq!(x.recipe.ingredients[0].canonical_id, "beetroot");
        assert_eq!(x.photos.len(), 3);
        assert_eq!(
            (x.photos[0].index, x.photos[0].kind),
            (0, PhotoKind::RecipeText)
        );
        assert_eq!(
            (x.photos[1].index, x.photos[1].kind),
            (1, PhotoKind::Dish),
            "missing index falls back to position"
        );
        assert_eq!(
            x.photos[2].kind,
            PhotoKind::Unrelated,
            "unknown kind is treated as unrelated"
        );
        assert_eq!(x.unreadable, vec!["quantity of dill"]);
        assert_eq!(x.language.as_deref(), Some("ru"));
        assert_eq!(x.cover_index(), Some(1));
    }

    #[test]
    fn flat_reply_without_recipe_wrapper_still_works() {
        let reply = r#"{"title": "Toast", "ingredients": [], "instructions": [], "photos": [{"index": 0, "kind": "both"}]}"#;
        let x = parse_photo_reply(reply).unwrap();
        assert_eq!(x.recipe.title, "Toast");
        assert_eq!(x.cover_index(), Some(0), "'both' counts as a dish photo");
    }

    #[test]
    fn text_only_photos_give_no_cover() {
        let reply = r#"{"recipe": {"title": "T", "ingredients": [], "instructions": []}, "photos": [{"index": 0, "kind": "recipe_text"}]}"#;
        assert_eq!(parse_photo_reply(reply).unwrap().cover_index(), None);
    }

    #[test]
    fn no_recipe_in_photos_is_a_distinct_error() {
        let reply = r#"{"recipe": null, "photos": [{"index": 0, "kind": "unrelated", "notes": "a cat"}], "unreadable": []}"#;
        let err = parse_photo_reply(reply).unwrap_err();
        assert!(matches!(err, ScraperError::NoRecipeInPhotos(_)), "{err:?}");
    }
}

mod untitled_and_equipment {
    use recipe_core::MeasurementUnit;
    use recipe_scraper::llm::{parse_photo_reply, parse_recipe_reply};

    #[test]
    fn missing_title_with_real_content_gets_a_synthesized_title() {
        let reply = r#"{"title": null, "servings": 2, "ingredients": [
            {"raw_text": "250g torchiette", "metric_amount": 250, "unit": "g", "canonical_id": "torchiette"},
            {"raw_text": "1x plakjes Parmaham", "metric_amount": 1, "unit": "pcs", "canonical_id": "parma_ham_slices"},
            {"raw_text": "1x cherrytomaten", "metric_amount": 1, "unit": "pcs", "canonical_id": "cherry_tomato"},
            {"raw_text": "1 citroen", "metric_amount": 1, "unit": "pcs", "canonical_id": "lemon"}
          ], "steps": [{"text": "Kook de pasta."}]}"#;
        let d = parse_recipe_reply(reply).unwrap();
        assert_eq!(d.title, "Torchiette, parma ham slices & cherry tomato");
        let x = parse_photo_reply(&format!(
            r#"{{"recipe": {}, "photos": [{{"index": 0, "kind": "recipe_text"}}]}}"#,
            reply
        ))
        .unwrap();
        assert_eq!(
            x.recipe.title,
            "Torchiette, parma ham slices & cherry tomato"
        );
    }

    #[test]
    fn empty_reply_without_title_is_still_an_error() {
        assert!(parse_recipe_reply(r#"{"title": null, "ingredients": [], "steps": []}"#).is_err());
    }

    #[test]
    fn nested_times_object_is_hoisted() {
        let reply = r#"{"title": "T", "times": {"total_seconds": null, "prep_seconds": 600, "cook_seconds": 1200}, "ingredients": [], "steps": []}"#;
        let d = parse_recipe_reply(reply).unwrap();
        assert_eq!(
            (d.prep_time_minutes, d.cook_time_minutes),
            (Some(10), Some(20))
        );
        let reply =
            r#"{"title": "T", "time": {"total_minutes": 25}, "ingredients": [], "steps": []}"#;
        let d = parse_recipe_reply(reply).unwrap();
        assert_eq!(d.cook_time_minutes, Some(25));
    }

    #[test]
    fn equipment_is_dropped_from_ingredients() {
        let reply = r#"{"title": "T", "ingredients": [
            {"raw_text": "olijfolie", "unit": "to_taste", "canonical_id": "olive_oil"},
            {"raw_text": "fijne keukenrasp", "unit": "to_taste", "canonical_id": "fine_grater"},
            {"raw_text": "waterkoker", "unit": "to_taste", "canonical_id": "kettle"},
            {"raw_text": "grote koekenpan", "unit": "to_taste", "canonical_id": "large_skillet"},
            {"raw_text": "middelgrote kookpan met deksel", "unit": "to_taste", "canonical_id": "medium_saucepan_with_lid"},
            {"raw_text": "2 nori sheets", "metric_amount": 2, "unit": "pcs", "canonical_id": "nori_sheet"},
            {"raw_text": "panko", "metric_amount": 50, "unit": "g", "canonical_id": "panko"}
          ], "steps": []}"#;
        let d = parse_recipe_reply(reply).unwrap();
        let ids: Vec<&str> = d
            .ingredients
            .iter()
            .map(|i| i.canonical_id.as_str())
            .collect();
        assert_eq!(ids, vec!["olive_oil", "nori_sheet", "panko"]);
        assert_eq!(d.ingredients[0].unit, MeasurementUnit::ToTaste);
    }
}

mod photo_classification_aliases {
    use recipe_scraper::llm::{PhotoKind, parse_photo_reply};

    #[test]
    fn classification_key_aliases_are_read() {
        let reply = r#"{"recipe": {"title": "T", "ingredients": [], "steps": []}, "photos": [
            {"index": 0, "classification": "dish"},
            {"index": 1, "type": "recipe_text"},
            {"index": 2, "category": "both"},
            {"index": 3, "label": "unrelated"}
          ]}"#;
        let x = parse_photo_reply(reply).unwrap();
        let kinds: Vec<PhotoKind> = x.photos.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PhotoKind::Dish,
                PhotoKind::RecipeText,
                PhotoKind::Both,
                PhotoKind::Unrelated
            ]
        );
        assert_eq!(x.cover_index(), Some(0));
    }

    #[test]
    fn notes_describing_a_dish_photo_promote_text_pages_to_cover_candidates() {
        let reply = r#"{"recipe": {"title": "T", "ingredients": [], "steps": []}, "photos": [
            {"index": 0, "kind": "recipe_text", "notes": "Recipe cover with title, servings, total time, dish photograph, and introductory description."},
            {"index": 1, "kind": "recipe_text", "notes": "Back page with ingredient list and six numbered preparation steps."}
          ]}"#;
        let x = parse_photo_reply(reply).unwrap();
        assert_eq!(x.photos[0].kind, PhotoKind::Both);
        assert_eq!(x.photos[1].kind, PhotoKind::RecipeText);
        assert_eq!(x.cover_index(), Some(0));
    }
}

mod translation_reply {
    use recipe_scraper::ScraperError;
    use recipe_scraper::llm::{TranslationRequest, parse_translation_reply};

    fn req() -> TranslationRequest {
        TranslationRequest {
            target_lang: "ru".into(),
            source_lang: Some("nl".into()),
            title: "Pho".into(),
            description: Some("Soep".into()),
            ingredients: vec!["1 kg botten".into(), "zout".into()],
            instructions: vec!["Kook.".into(), "Serveer.".into()],
            tags: vec!["soep".into()],
        }
    }

    #[test]
    fn accepts_plain_arrays_and_indexed_objects() {
        let reply = r#"{"title":"Фо","description":"Суп","ingredients":["1 кг костей","соль"],"instructions":[{"index":1,"text":"Подавать."},{"index":0,"text":"Варить."}],"tags":["суп"]}"#;
        let t = parse_translation_reply(reply, &req()).unwrap();
        assert_eq!(t.title, "Фо");
        assert_eq!(t.ingredients, vec!["1 кг костей", "соль"]);
        assert_eq!(
            t.instructions,
            vec!["Варить.", "Подавать."],
            "indexed objects are re-ordered by index"
        );
        assert_eq!(t.tags, vec!["суп"]);
    }

    #[test]
    fn missing_optional_parts_fall_back_but_wrong_counts_are_an_error() {
        let reply = r#"{"title":"Фо","ingredients":["1 кг костей","соль"],"instructions":["Варить.","Подавать."]}"#;
        let t = parse_translation_reply(reply, &req()).unwrap();
        assert_eq!(
            t.description.as_deref(),
            Some("Soep"),
            "untranslated description keeps the source"
        );
        assert_eq!(t.tags, vec!["soep"]);
        let short = r#"{"title":"Фо","ingredients":["1 кг костей"],"instructions":["Варить.","Подавать."]}"#;
        assert!(matches!(
            parse_translation_reply(short, &req()).unwrap_err(),
            ScraperError::TranslationIncomplete(_)
        ));
    }
}

mod term_labels {
    use recipe_scraper::llm::parse_term_labels;

    #[test]
    fn accepts_object_map_and_array_forms() {
        let ids = vec!["garlic".to_string(), "fish_sauce".to_string()];
        let m = parse_term_labels(
            r#"{"labels": {"garlic": "чеснок", "fish_sauce": "рыбный соус"}}"#,
            &ids,
        )
        .unwrap();
        assert_eq!(m.get("garlic").map(String::as_str), Some("чеснок"));
        let m = parse_term_labels(r#"[{"canonical_id": "garlic", "label": "чеснок"}, {"id": "fish_sauce", "label": "рыбный соус"}, {"canonical_id": "unknown_id", "label": "x"}]"#, &ids).unwrap();
        assert_eq!(m.len(), 2, "unknown ids are ignored");
        assert_eq!(m["fish_sauce"], "рыбный соус");
        let m = parse_term_labels(r#"{"garlic": "чеснок"}"#, &ids).unwrap();
        assert_eq!(
            m.len(),
            1,
            "partial replies are fine; missing ids fall back later"
        );
    }
}
