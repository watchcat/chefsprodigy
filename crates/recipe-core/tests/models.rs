use recipe_core::{IngredientItem, MeasurementUnit, Recipe};
use schemars::schema_for;

#[test]
fn measurement_unit_serializes_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&MeasurementUnit::ToTaste).unwrap(),
        "\"to_taste\""
    );
    assert_eq!(serde_json::to_string(&MeasurementUnit::G).unwrap(), "\"g\"");
    assert_eq!(
        serde_json::to_string(&MeasurementUnit::Ml).unwrap(),
        "\"ml\""
    );
    assert_eq!(
        serde_json::to_string(&MeasurementUnit::Pcs).unwrap(),
        "\"pcs\""
    );
}

#[test]
fn measurement_unit_round_trips_through_db_string() {
    for unit in [
        MeasurementUnit::G,
        MeasurementUnit::Ml,
        MeasurementUnit::Pcs,
        MeasurementUnit::ToTaste,
    ] {
        let s = unit.as_str();
        assert_eq!(s.parse::<MeasurementUnit>().unwrap(), unit);
    }
    assert!("cups".parse::<MeasurementUnit>().is_err());
}

#[test]
fn ingredient_item_deserializes_from_llm_json() {
    let json = r#"{"raw_text":"2 tbsp dark soy sauce","metric_amount":30.0,"unit":"ml","canonical_id":"dark_soy_sauce","is_specialty":true}"#;
    let item: IngredientItem = serde_json::from_str(json).unwrap();
    assert_eq!(item.unit, MeasurementUnit::Ml);
    assert_eq!(item.canonical_id, "dark_soy_sauce");
    assert!(item.is_specialty);
}

#[test]
fn recipe_json_schema_exposes_ingredient_doc_comments() {
    let schema = schema_for!(Recipe);
    let text = serde_json::to_string(&schema).unwrap();
    assert!(text.contains("canonical_id"));
    assert!(
        text.contains("Tier 3"),
        "doc comments must be injected into schema for the LLM"
    );
}
