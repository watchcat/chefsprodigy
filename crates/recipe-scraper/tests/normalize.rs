use recipe_core::MeasurementUnit;
use recipe_scraper::normalize::heuristic_ingredient;

#[test]
fn parses_metric_weight_and_volume() {
    let i = heuristic_ingredient("1 kg beef bones");
    assert_eq!((i.metric_amount, i.unit), (Some(1000.0), MeasurementUnit::G));
    assert_eq!(i.canonical_id, "beef_bones");
    assert_eq!(i.raw_text, "1 kg beef bones");

    let i = heuristic_ingredient("250ml whole milk");
    assert_eq!((i.metric_amount, i.unit), (Some(250.0), MeasurementUnit::Ml));
    assert_eq!(i.canonical_id, "whole_milk");

    let i = heuristic_ingredient("1.5 l water");
    assert_eq!((i.metric_amount, i.unit), (Some(1500.0), MeasurementUnit::Ml));
}

#[test]
fn converts_imperial_and_kitchen_units() {
    let i = heuristic_ingredient("2 tbsp fish sauce");
    assert_eq!((i.metric_amount, i.unit), (Some(30.0), MeasurementUnit::Ml));
    assert_eq!(i.canonical_id, "fish_sauce");

    let i = heuristic_ingredient("1/2 cup sugar");
    assert_eq!((i.metric_amount, i.unit), (Some(120.0), MeasurementUnit::Ml));

    let i = heuristic_ingredient("½ tsp salt");
    assert_eq!((i.metric_amount, i.unit), (Some(2.5), MeasurementUnit::Ml));
    assert_eq!(i.canonical_id, "salt");

    let i = heuristic_ingredient("1 ½ lb ground beef");
    assert_eq!(i.unit, MeasurementUnit::G);
    assert!((i.metric_amount.unwrap() - 680.0).abs() < 1.0, "{:?}", i.metric_amount);

    let i = heuristic_ingredient("4 oz cream cheese");
    assert_eq!((i.metric_amount, i.unit), (Some(113.0), MeasurementUnit::G));
}

#[test]
fn counts_pieces_and_cloves() {
    let i = heuristic_ingredient("3 star anise");
    assert_eq!((i.metric_amount, i.unit), (Some(3.0), MeasurementUnit::Pcs));
    assert_eq!(i.canonical_id, "star_anise");

    let i = heuristic_ingredient("2 cloves garlic, minced");
    assert_eq!((i.metric_amount, i.unit), (Some(2.0), MeasurementUnit::Pcs));
    assert_eq!(i.canonical_id, "garlic");

    let i = heuristic_ingredient("2 large eggs");
    assert_eq!((i.metric_amount, i.unit), (Some(2.0), MeasurementUnit::Pcs));
    assert_eq!(i.canonical_id, "eggs");
}

#[test]
fn unquantified_items_are_to_taste() {
    for raw in ["salt to taste", "Salt", "freshly ground black pepper", "a pinch of salt"] {
        let i = heuristic_ingredient(raw);
        assert_eq!(i.unit, MeasurementUnit::ToTaste, "{raw}");
        assert_eq!(i.metric_amount, None, "{raw}");
    }
    assert_eq!(heuristic_ingredient("freshly ground black pepper").canonical_id, "black_pepper");
}

#[test]
fn canonical_id_drops_parentheticals_and_preparation_notes() {
    assert_eq!(heuristic_ingredient("2 tbsp dark soy sauce (preferably Pearl River Bridge)").canonical_id, "dark_soy_sauce");
    assert_eq!(heuristic_ingredient("1 onion, finely chopped").canonical_id, "onion");
    assert_eq!(heuristic_ingredient("200g fresh spinach leaves, washed").canonical_id, "spinach_leaves");
    assert_eq!(heuristic_ingredient("  ").canonical_id, "unknown");
}
