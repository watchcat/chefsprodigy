use recipe_core::{IngredientItem, MeasurementUnit, Recipe, RecipeInput};
use recipe_mcp::tools::search::missing_ingredients;
use recipe_mcp::tools::shopping::{ShoppingLine, aggregate_shopping_list};

fn ing(id: &str, amount: Option<f32>, unit: MeasurementUnit, specialty: bool) -> IngredientItem {
    IngredientItem { raw_text: format!("raw {id}"), metric_amount: amount, unit, canonical_id: id.into(), is_specialty: specialty, canonical_label: None }
}

fn recipe(title: &str, servings: u8, ingredients: Vec<IngredientItem>) -> Recipe {
    let input = RecipeInput {
        title: title.into(),
        description: None,
        servings,
        prep_time_minutes: None,
        cook_time_minutes: None,
        ingredients,
        instructions: vec![],
        tags: vec![],
        image_path: None,
        origin_url: None,
        author_did: None,
        is_public: false,
        language: None,
    };
    Recipe::from_input(title.to_lowercase(), input, "t")
}

#[test]
fn missing_ingredients_is_recipe_minus_pantry_case_insensitive() {
    let canonical = vec!["egg".to_string(), "Cheddar".to_string(), "salt".to_string()];
    let pantry = vec!["EGG".to_string(), "salt".to_string()];
    assert_eq!(missing_ingredients(&canonical, &pantry), vec!["cheddar"]);
    assert_eq!(missing_ingredients(&canonical, &[]), vec!["egg", "cheddar", "salt"]);
}

#[test]
fn shopping_list_scales_sums_and_splits_buckets() {
    let pho = recipe("Pho", 2, vec![
        ing("beef_bones", Some(1000.0), MeasurementUnit::G, false),
        ing("star_anise", Some(3.0), MeasurementUnit::Pcs, true),
        ing("salt", None, MeasurementUnit::ToTaste, false),
    ]);
    let omelette = recipe("Omelette", 1, vec![
        ing("egg", Some(3.0), MeasurementUnit::Pcs, false),
        ing("star_anise", Some(1.0), MeasurementUnit::Pcs, false),
        ing("salt", None, MeasurementUnit::ToTaste, false),
    ]);

    // Pho for 4 (x2), omelette for 2 (x2). Pantry already has salt.
    let list = aggregate_shopping_list(&[(&pho, 4), (&omelette, 2)], &["salt".to_string()]);

    let basic: Vec<(&str, Option<f32>, MeasurementUnit)> =
        list.basic_groceries.iter().map(|l| (l.canonical_id.as_str(), l.total_amount, l.unit)).collect();
    assert_eq!(basic, vec![("beef_bones", Some(2000.0), MeasurementUnit::G), ("egg", Some(6.0), MeasurementUnit::Pcs)]);

    assert_eq!(list.specialty_items.len(), 1);
    let anise: &ShoppingLine = &list.specialty_items[0];
    assert_eq!(anise.canonical_id, "star_anise");
    assert_eq!(anise.total_amount, Some(8.0), "3*2 from pho + 1*2 from omelette");
    assert!(anise.is_specialty, "specialty if any recipe marks it specialty");
    assert_eq!(anise.recipe_count, 2);
    assert!(list.basic_groceries.iter().all(|l| l.canonical_id != "salt"), "pantry items excluded");
}

#[test]
fn shopping_list_keeps_different_units_apart_and_unquantified_items() {
    let a = recipe("A", 1, vec![ing("milk", Some(200.0), MeasurementUnit::Ml, false), ing("pepper", None, MeasurementUnit::ToTaste, false)]);
    let b = recipe("B", 1, vec![ing("milk", Some(1.0), MeasurementUnit::Pcs, false), ing("pepper", None, MeasurementUnit::ToTaste, false)]);
    let list = aggregate_shopping_list(&[(&a, 1), (&b, 1)], &[]);
    let lines: Vec<(&str, Option<f32>, MeasurementUnit)> =
        list.basic_groceries.iter().map(|l| (l.canonical_id.as_str(), l.total_amount, l.unit)).collect();
    assert_eq!(lines, vec![
        ("milk", Some(200.0), MeasurementUnit::Ml),
        ("milk", Some(1.0), MeasurementUnit::Pcs),
        ("pepper", None, MeasurementUnit::ToTaste),
    ]);
    assert_eq!(list.basic_groceries[2].recipe_count, 2);
}

#[test]
fn shopping_list_zero_servings_request_uses_recipe_default() {
    let a = recipe("A", 2, vec![ing("rice", Some(300.0), MeasurementUnit::G, false)]);
    let list = aggregate_shopping_list(&[(&a, 0)], &[]);
    assert_eq!(list.basic_groceries[0].total_amount, Some(300.0));
}
