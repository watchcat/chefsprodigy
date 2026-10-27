use recipe_core::{
    IngredientItem, InstructionStep, MeasurementUnit, Recipe, RecipeInput, calculate_content_hash,
};

fn sample_input() -> RecipeInput {
    RecipeInput {
        title: "Garlic Noodles".to_string(),
        description: Some("Quick".to_string()),
        servings: 2,
        prep_time_minutes: Some(5),
        cook_time_minutes: Some(10),
        ingredients: vec![
            IngredientItem {
                raw_text: "200g noodles".to_string(),
                metric_amount: Some(200.0),
                unit: MeasurementUnit::G,
                canonical_id: "noodles".to_string(),
                is_specialty: false,
                canonical_label: None,
            },
            IngredientItem {
                raw_text: "salt".to_string(),
                metric_amount: None,
                unit: MeasurementUnit::ToTaste,
                canonical_id: "salt".to_string(),
                is_specialty: false,
                canonical_label: None,
            },
        ],
        instructions: vec![InstructionStep {
            step_number: 1,
            text: "Boil.".to_string(),
            timer_seconds: Some(300),
        }],
        tags: vec!["quick".to_string()],
        image_path: None,
        origin_url: Some("https://example.com/noodles".to_string()),
        author_did: None,
        is_public: true,
        language: None,
    }
}

#[test]
fn recipe_from_input_sets_identity_timestamps_and_hash() {
    let recipe = Recipe::from_input(
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        sample_input(),
        "2026-10-21T00:00:00Z",
    );
    assert_eq!(recipe.id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(recipe.title, "Garlic Noodles");
    assert_eq!(recipe.created_at, "2026-10-21T00:00:00Z");
    assert_eq!(recipe.updated_at, "2026-10-21T00:00:00Z");
    assert!(recipe.is_public);
    assert_eq!(recipe.content_hash, calculate_content_hash(&recipe));
    assert_eq!(recipe.content_hash.len(), 64);
}

#[test]
fn recipe_apply_input_keeps_id_and_created_at_but_rehashes() {
    let mut recipe = Recipe::from_input("id1", sample_input(), "2026-01-01T00:00:00Z");
    let original_hash = recipe.content_hash.clone();

    let mut edited = sample_input();
    edited.ingredients[0].canonical_id = "rice_noodles".to_string();
    recipe.apply_input(edited, "2026-02-02T00:00:00Z");

    assert_eq!(recipe.id, "id1");
    assert_eq!(recipe.created_at, "2026-01-01T00:00:00Z");
    assert_eq!(recipe.updated_at, "2026-02-02T00:00:00Z");
    assert_ne!(recipe.content_hash, original_hash);
    assert_eq!(recipe.content_hash, calculate_content_hash(&recipe));
}

#[test]
fn scaled_to_multiplies_metric_amounts_and_updates_servings() {
    let recipe = Recipe::from_input("id", sample_input(), "t");
    let scaled = recipe.scaled_to(4);
    assert_eq!(scaled.servings, 4);
    assert_eq!(scaled.ingredients[0].metric_amount, Some(400.0));
    assert_eq!(
        scaled.ingredients[1].metric_amount, None,
        "to-taste items stay unquantified"
    );
    assert_eq!(
        scaled.ingredients[0].raw_text, "200g noodles",
        "raw text is never rewritten"
    );
}

#[test]
fn scaled_to_same_servings_is_identity() {
    let recipe = Recipe::from_input("id", sample_input(), "t");
    assert_eq!(recipe.scaled_to(2), recipe);
}

#[test]
fn scaled_to_zero_target_or_zero_base_is_identity() {
    let recipe = Recipe::from_input("id", sample_input(), "t");
    assert_eq!(recipe.scaled_to(0), recipe);

    let mut zero_base = sample_input();
    zero_base.servings = 0;
    let recipe = Recipe::from_input("id", zero_base, "t");
    assert_eq!(recipe.scaled_to(4), recipe);
}

#[test]
fn scaling_does_not_change_content_hash_field() {
    let recipe = Recipe::from_input("id", sample_input(), "t");
    assert_eq!(recipe.scaled_to(6).content_hash, recipe.content_hash);
}

#[test]
fn recipe_input_round_trips_from_recipe() {
    let recipe = Recipe::from_input("id", sample_input(), "t");
    let back = RecipeInput::from(recipe.clone());
    assert_eq!(back.title, recipe.title);
    assert_eq!(back.ingredients, recipe.ingredients);
    assert_eq!(back.is_public, recipe.is_public);
}

#[test]
fn recipe_input_deserializes_with_defaults_for_optional_fields() {
    let json = r#"{"title":"Toast","ingredients":[],"instructions":[]}"#;
    let input: RecipeInput = serde_json::from_str(json).unwrap();
    assert_eq!(
        input.servings, 4,
        "servings defaults to 4 like the DB schema"
    );
    assert!(input.tags.is_empty());
    assert!(!input.is_public);
}

mod i18n {
    use recipe_core::{Recipe, RecipeInput, RecipeTranslation, TranslationOrigin, normalize_lang};

    fn input() -> RecipeInput {
        serde_json::from_str(r#"{"title":"Pho","language":"NL-be","servings":2,
          "ingredients":[{"raw_text":"1 kg botten","metric_amount":1000,"unit":"g","canonical_id":"beef_bones","is_specialty":false},
                         {"raw_text":"zout","metric_amount":null,"unit":"to_taste","canonical_id":"salt","is_specialty":false}],
          "instructions":[{"step_number":1,"text":"Kook.","timer_seconds":null},{"step_number":2,"text":"Serveer.","timer_seconds":null}],
          "tags":["soep"]}"#).unwrap()
    }

    #[test]
    fn language_is_optional_and_normalized() {
        assert_eq!(normalize_lang("NL-be"), Some("nl".to_string()));
        assert_eq!(normalize_lang(" ru_RU "), Some("ru".to_string()));
        assert_eq!(normalize_lang("eng"), Some("eng".to_string()));
        assert_eq!(normalize_lang(""), None);
        assert_eq!(normalize_lang("x"), None);
        assert_eq!(normalize_lang("12"), None);
        let r = Recipe::from_input("id", input(), "t");
        assert_eq!(
            r.language.as_deref(),
            Some("nl"),
            "normalized on the way in"
        );
        let no_lang: RecipeInput = serde_json::from_str(r#"{"title":"T"}"#).unwrap();
        assert_eq!(Recipe::from_input("id", no_lang, "t").language, None);
    }

    #[test]
    fn language_does_not_affect_content_hash() {
        let a = Recipe::from_input("id", input(), "t");
        let mut b_in = input();
        b_in.language = Some("en".into());
        let b = Recipe::from_input("id", b_in, "t");
        assert_eq!(a.content_hash, b.content_hash);
    }

    #[test]
    fn applying_a_translation_replaces_presentation_fields_only() {
        let r = Recipe::from_input("id", input(), "2026-10-24T00:00:00Z");
        let t = RecipeTranslation {
            lang: "ru".into(),
            title: "Фо".into(),
            description: Some("Суп".into()),
            ingredients: vec!["1 кг костей".into(), "соль".into()],
            instructions: vec!["Варить.".into(), "Подавать.".into()],
            tags: vec!["суп".into()],
            origin: TranslationOrigin::Llm,
            source_updated_at: "2026-10-24T00:00:00Z".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        assert!(!t.is_stale_for(&r));
        let loc = r.localized(&t);
        assert_eq!(loc.recipe.title, "Фо");
        assert_eq!(loc.recipe.ingredients[0].raw_text, "1 кг костей");
        assert_eq!(
            loc.recipe.ingredients[0].canonical_id, "beef_bones",
            "normalization layer untouched"
        );
        assert_eq!(loc.recipe.ingredients[0].metric_amount, Some(1000.0));
        assert_eq!(loc.recipe.instructions[1].text, "Подавать.");
        assert_eq!(loc.recipe.instructions[1].step_number, 2);
        assert_eq!(loc.recipe.tags, vec!["суп"]);
        assert_eq!(loc.recipe.content_hash, r.content_hash);
        assert_eq!(
            loc.recipe.language.as_deref(),
            Some("nl"),
            "source language stays"
        );
        let info = loc.translation.unwrap();
        assert_eq!(
            (info.lang.as_str(), info.origin, info.stale),
            ("ru", TranslationOrigin::Llm, false)
        );

        let json = serde_json::to_value(r.localized(&t)).unwrap();
        assert_eq!(json["title"], "Фо", "flattened: same shape as a recipe");
        assert_eq!(json["translation"]["lang"], "ru");
        let plain = serde_json::to_value(recipe_core::LocalizedRecipe::source(r.clone())).unwrap();
        assert!(plain["translation"].is_null());
    }

    #[test]
    fn translation_with_wrong_lengths_is_padded_from_source() {
        let r = Recipe::from_input("id", input(), "t");
        let t = RecipeTranslation {
            lang: "ru".into(),
            title: "Фо".into(),
            description: None,
            ingredients: vec!["1 кг костей".into()],
            instructions: vec![],
            tags: vec![],
            origin: TranslationOrigin::Human,
            source_updated_at: "old".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let loc = r.localized(&t);
        assert_eq!(
            loc.recipe.ingredients[1].raw_text, "zout",
            "missing items fall back to the source wording"
        );
        assert_eq!(loc.recipe.instructions[0].text, "Kook.");
        assert!(
            loc.translation.unwrap().stale,
            "source_updated_at differs from recipe.updated_at"
        );
    }
}
