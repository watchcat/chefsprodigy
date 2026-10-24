use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit, Recipe, calculate_content_hash};

fn ingredient(canonical_id: &str, amount: Option<f32>, unit: MeasurementUnit) -> IngredientItem {
    IngredientItem {
        raw_text: format!("some {canonical_id}"),
        metric_amount: amount,
        unit,
        canonical_id: canonical_id.to_string(),
        is_specialty: false,
        canonical_label: None,
    }
}

fn step(n: u16, text: &str) -> InstructionStep {
    InstructionStep {
        step_number: n,
        text: text.to_string(),
        timer_seconds: None,
    }
}

fn base_recipe() -> Recipe {
    Recipe {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        title: "Garlic Noodles".to_string(),
        description: None,
        servings: 2,
        prep_time_minutes: Some(5),
        cook_time_minutes: Some(10),
        ingredients: vec![
            ingredient("Noodles", Some(200.0), MeasurementUnit::G),
            ingredient("garlic", Some(3.0), MeasurementUnit::Pcs),
            ingredient("salt", None, MeasurementUnit::ToTaste),
        ],
        instructions: vec![
            step(1, "Boil the noodles."),
            step(2, "Fry garlic, toss together!"),
        ],
        tags: vec!["quick".to_string()],
        image_path: None,
        origin_url: None,
        author_did: None,
        content_hash: String::new(),
        is_public: false,
        language: None,
        created_at: "2026-10-21T00:00:00Z".to_string(),
        updated_at: "2026-10-21T00:00:00Z".to_string(),
    }
}

#[test]
fn hash_is_64_hex_chars() {
    let hash = calculate_content_hash(&base_recipe());
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn hash_is_deterministic() {
    assert_eq!(calculate_content_hash(&base_recipe()), calculate_content_hash(&base_recipe()));
}

#[test]
fn hash_ignores_ingredient_order() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.ingredients.reverse();
    assert_eq!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_ignores_canonical_id_case() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.ingredients[0].canonical_id = "noodles".to_string();
    assert_eq!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_ignores_instruction_whitespace_punctuation_and_case() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.instructions[0].text = "  BOIL   the\nnoodles ".to_string();
    b.instructions[1].text = "fry garlic toss together".to_string();
    assert_eq!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_ignores_non_content_metadata() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.id = "other".to_string();
    b.title = "Different Title".to_string();
    b.description = Some("desc".to_string());
    b.tags = vec![];
    b.image_path = Some("x.webp".to_string());
    b.origin_url = Some("https://example.com".to_string());
    b.author_did = Some("did:key:z6Mk".to_string());
    b.is_public = true;
    b.updated_at = "2027-01-01T00:00:00Z".to_string();
    b.ingredients[0].raw_text = "200g of the best noodles".to_string();
    b.ingredients[0].is_specialty = true;
    assert_eq!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_changes_when_canonical_ingredient_changes() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.ingredients[1].canonical_id = "shallot".to_string();
    assert_ne!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_changes_when_quantity_changes() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.ingredients[0].metric_amount = Some(250.0);
    assert_ne!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_changes_when_unit_changes() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.ingredients[0].unit = MeasurementUnit::Ml;
    assert_ne!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_changes_when_instruction_text_changes() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.instructions[1].text = "Fry shallots, toss together!".to_string();
    assert_ne!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_does_not_depend_on_stored_content_hash_field() {
    let a = base_recipe();
    let mut b = base_recipe();
    b.content_hash = "deadbeef".to_string();
    assert_eq!(calculate_content_hash(&a), calculate_content_hash(&b));
}

#[test]
fn hash_is_stable_across_versions() {
    // Golden value: changing the algorithm breaks federation dedup, so this must be updated deliberately.
    let hash = calculate_content_hash(&base_recipe());
    assert_eq!(hash, "a4b8a602a18fd7736998c18dab4a6c5c93f9364d6ba883000db869cf0eddec44");
}
