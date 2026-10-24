use std::io::{Cursor, Write};

use recipe_core::MeasurementUnit;
use recipe_scraper::mealie::read_mealie_backup;
use serde_json::json;
use zip::write::SimpleFileOptions;

fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        w.start_file(*name, SimpleFileOptions::default()).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn tiny_png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

#[test]
fn reads_mealie_v1_database_dump_with_joined_tables() {
    let db = json!({
        "recipes": [
            {"id": "r-1", "slug": "beef-pho", "name": "Beef Pho", "description": "Classic", "recipe_yield": "4 servings",
             "prep_time": "30 minutes", "cook_time": "6 hours", "total_time": null},
            {"id": "r-2", "slug": "toast", "name": "Toast", "description": null, "recipe_yield": null}
        ],
        "recipes_ingredients": [
            {"id": "i-1", "recipe_id": "r-1", "position": 1, "quantity": 3.0, "unit_id": null, "food_id": "f-1", "note": "whole", "original_text": "3 star anise"},
            {"id": "i-2", "recipe_id": "r-1", "position": 0, "quantity": 1.0, "unit_id": "u-1", "food_id": "f-2", "note": "", "original_text": null},
            {"id": "i-3", "recipe_id": "r-2", "position": 0, "quantity": 2.0, "unit_id": null, "food_id": "f-3", "note": "", "original_text": null}
        ],
        "ingredient_units": [{"id": "u-1", "name": "kilogram", "abbreviation": "kg"}],
        "ingredient_foods": [{"id": "f-1", "name": "star anise"}, {"id": "f-2", "name": "beef bones"}, {"id": "f-3", "name": "bread"}],
        "recipe_instructions": [
            {"id": "s-2", "recipe_id": "r-1", "position": 1, "text": "Simmer for 6 hours."},
            {"id": "s-1", "recipe_id": "r-1", "position": 0, "text": "Roast bones."},
            {"id": "s-3", "recipe_id": "r-2", "position": 0, "text": "Toast it."}
        ],
        "tags": [{"id": "t-1", "name": "Soup"}, {"id": "t-2", "name": "Vietnamese"}],
        "recipes_to_tags": [{"recipe_id": "r-1", "tag_id": "t-1"}, {"recipe_id": "r-1", "tag_id": "t-2"}]
    });
    let zip = build_zip(&[
        ("database.json", db.to_string().as_bytes()),
        ("data/recipes/r-1/images/original.png", &tiny_png()),
    ]);

    let recipes = read_mealie_backup(Cursor::new(zip)).unwrap();
    assert_eq!(recipes.len(), 2);

    let pho = recipes.iter().find(|r| r.recipe.title == "Beef Pho").unwrap();
    assert_eq!(pho.recipe.servings, 4);
    assert_eq!(pho.recipe.prep_time_minutes, Some(30));
    assert_eq!(pho.recipe.cook_time_minutes, Some(360));
    assert_eq!(pho.recipe.tags, vec!["soup", "vietnamese"]);
    assert_eq!(pho.recipe.instructions.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["Roast bones.", "Simmer for 6 hours."]);
    assert_eq!(pho.recipe.instructions[1].timer_seconds, Some(21600));
    assert_eq!(pho.recipe.ingredients.len(), 2);
    assert_eq!(pho.recipe.ingredients[0].raw_text, "1 kg beef bones", "position ordering, composed from quantity+unit+food");
    assert_eq!((pho.recipe.ingredients[0].metric_amount, pho.recipe.ingredients[0].unit), (Some(1000.0), MeasurementUnit::G));
    assert_eq!(pho.recipe.ingredients[0].canonical_id, "beef_bones");
    assert_eq!(pho.recipe.ingredients[1].raw_text, "3 star anise", "original_text preferred when present");
    assert!(pho.image.is_some(), "image bytes attached");
    assert_eq!(pho.source_id.as_deref(), Some("r-1"));

    let toast = recipes.iter().find(|r| r.recipe.title == "Toast").unwrap();
    assert_eq!(toast.recipe.servings, 4, "missing yield defaults to 4");
    assert_eq!(toast.recipe.ingredients[0].raw_text, "2 bread");
    assert!(toast.image.is_none());
}

#[test]
fn reads_per_recipe_json_exports() {
    let recipe = json!({
        "name": "Cheese Omelette", "description": "Fast", "recipeYield": "1 serving",
        "prepTime": "PT5M", "performTime": "PT5M",
        "recipeIngredient": [
            {"quantity": 3, "unit": null, "food": {"name": "egg"}, "note": "large", "originalText": null, "display": "3 large egg"},
            {"quantity": 50, "unit": {"name": "gram", "abbreviation": "g"}, "food": {"name": "cheddar"}, "note": "", "originalText": "50g cheddar"}
        ],
        "recipeInstructions": [{"text": "Whisk."}, {"text": "Cook 3 minutes."}],
        "tags": [{"name": "Breakfast"}],
        "image": "original.webp"
    });
    let zip = build_zip(&[
        ("recipes/cheese-omelette/cheese-omelette.json", recipe.to_string().as_bytes()),
        ("recipes/cheese-omelette/images/original.png", &tiny_png()),
        ("README.txt", b"not a recipe"),
    ]);

    let recipes = read_mealie_backup(Cursor::new(zip)).unwrap();
    assert_eq!(recipes.len(), 1);
    let r = &recipes[0];
    assert_eq!(r.recipe.title, "Cheese Omelette");
    assert_eq!(r.recipe.servings, 1);
    assert_eq!(r.recipe.prep_time_minutes, Some(5));
    assert_eq!(r.recipe.cook_time_minutes, Some(5));
    assert_eq!(r.recipe.tags, vec!["breakfast"]);
    assert_eq!(r.recipe.ingredients[0].raw_text, "3 large egg");
    assert_eq!((r.recipe.ingredients[0].metric_amount, r.recipe.ingredients[0].unit), (Some(3.0), MeasurementUnit::Pcs));
    assert_eq!(r.recipe.ingredients[1].raw_text, "50g cheddar");
    assert_eq!((r.recipe.ingredients[1].metric_amount, r.recipe.ingredients[1].unit), (Some(50.0), MeasurementUnit::G));
    assert_eq!(r.recipe.instructions[1].timer_seconds, Some(180));
    assert!(r.image.is_some());
}

#[test]
fn archive_without_recipes_is_an_error_and_corrupt_zip_too() {
    let zip = build_zip(&[("README.txt", b"hello")]);
    assert!(read_mealie_backup(Cursor::new(zip)).is_err());
    assert!(read_mealie_backup(Cursor::new(b"definitely not a zip".to_vec())).is_err());
}
