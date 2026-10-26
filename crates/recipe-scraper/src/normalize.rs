//! Zero-LLM heuristic ingredient normalization. Used when no model is
//! configured, and as a deterministic baseline the LLM path can improve on.

use recipe_core::{IngredientItem, MeasurementUnit};

/// Unit words mapped to (target unit, multiplier to that unit).
fn unit_factor(word: &str) -> Option<(MeasurementUnit, f32)> {
    use MeasurementUnit::*;
    let w = word.trim_end_matches('.').to_lowercase();
    let w = w.as_str();
    Some(match w {
        "g" | "gram" | "grams" | "gr" => (G, 1.0),
        "kg" | "kilogram" | "kilograms" => (G, 1000.0),
        "mg" => (G, 0.001),
        "oz" | "ounce" | "ounces" => (G, 28.35),
        "lb" | "lbs" | "pound" | "pounds" => (G, 453.6),
        "ml" | "milliliter" | "milliliters" | "millilitre" | "millilitres" => (Ml, 1.0),
        "cl" => (Ml, 10.0),
        "dl" => (Ml, 100.0),
        "l" | "liter" | "liters" | "litre" | "litres" => (Ml, 1000.0),
        "cup" | "cups" | "c" => (Ml, 240.0),
        "tbsp" | "tbs" | "tablespoon" | "tablespoons" => (Ml, 15.0),
        "tsp" | "teaspoon" | "teaspoons" => (Ml, 5.0),
        "fl" => (Ml, 29.57),
        "clove" | "cloves" | "piece" | "pieces" | "pcs" | "pc" | "slice" | "slices" | "can"
        | "cans" | "stick" | "sticks" | "bunch" | "bunches" | "sprig" | "sprigs" | "stalk"
        | "stalks" | "head" | "heads" | "sheet" | "sheets" | "leaf" | "leaves" | "fillet"
        | "fillets" | "egg" | "whole" | "large" | "medium" | "small" => (Pcs, 1.0),
        "pinch" | "pinches" | "dash" | "dashes" | "handful" | "splash" => (ToTaste, 0.0),
        _ => return None,
    })
}

/// Words that describe preparation or size rather than the ingredient itself.
const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "of",
    "to",
    "taste",
    "fresh",
    "freshly",
    "chopped",
    "minced",
    "diced",
    "sliced",
    "finely",
    "roughly",
    "coarsely",
    "thinly",
    "peeled",
    "grated",
    "crushed",
    "optional",
    "plus",
    "extra",
    "more",
    "for",
    "serving",
    "garnish",
    "large",
    "medium",
    "small",
    "about",
    "approximately",
    "or",
    "and",
    "as",
    "needed",
    "divided",
    "room",
    "temperature",
    "softened",
    "melted",
    "cold",
    "warm",
    "hot",
    "ripe",
    "raw",
    "cooked",
    "dried",
    "pinch",
    "pinches",
    "dash",
    "handful",
    "splash",
    "piece",
    "pieces",
    "pcs",
    "ground",
];

fn unicode_fraction(c: char) -> Option<f32> {
    Some(match c {
        '½' => 0.5,
        '⅓' => 1.0 / 3.0,
        '⅔' => 2.0 / 3.0,
        '¼' => 0.25,
        '¾' => 0.75,
        '⅛' => 0.125,
        '⅜' => 0.375,
        '⅝' => 0.625,
        '⅞' => 0.875,
        _ => return None,
    })
}

/// Parse "1", "1.5", "1/2", "½", "2-3" (lower bound) into a number.
fn parse_number(tok: &str) -> Option<f32> {
    let tok = tok.split(['-', '–']).next().unwrap_or(tok);
    if tok.is_empty() {
        return None;
    }
    if let Some((a, b)) = tok.split_once('/') {
        let (a, b): (f32, f32) = (a.parse().ok()?, b.parse().ok()?);
        return (b != 0.0).then(|| a / b);
    }
    if let Some(f) = tok.chars().next().and_then(unicode_fraction) {
        return (tok.chars().count() == 1).then_some(f);
    }
    // "1½"
    let mut chars = tok.chars();
    let last = chars.next_back()?;
    if let Some(f) = unicode_fraction(last) {
        let whole: f32 = chars.as_str().parse().ok()?;
        return Some(whole + f);
    }
    tok.replace(',', ".").parse().ok()
}

/// Split "250ml" into ("250", "ml"); returns None if no digit prefix + alpha suffix.
fn split_attached_unit(tok: &str) -> Option<(&str, &str)> {
    let idx = tok.find(|c: char| c.is_alphabetic())?;
    let (num, unit) = tok.split_at(idx);
    (!num.is_empty()
        && num
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || c == '/'))
    .then_some((num, unit))
}

fn round_amount(a: f32) -> f32 {
    if a >= 10.0 {
        a.round()
    } else {
        (a * 100.0).round() / 100.0
    }
}

/// Build a snake_case canonical id from the ingredient name words.
fn canonical_from_words(words: &[String]) -> String {
    let id = words
        .iter()
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .map(|w| {
            w.chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect::<String>()
        })
        .filter(|w| !w.trim_matches('_').is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let id = id
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if id.is_empty() {
        "unknown".to_string()
    } else {
        id
    }
}

/// Normalize a single raw ingredient line without any LLM.
pub fn heuristic_ingredient(raw: &str) -> IngredientItem {
    let raw_text = raw.trim().to_string();

    // Remove parentheticals and anything after the first comma (preparation notes).
    let mut name_part = String::new();
    let mut depth = 0;
    for c in raw_text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => name_part.push(c),
            _ => {}
        }
    }
    let name_part = name_part.split([',', ';']).next().unwrap_or("").to_string();
    let lowered = name_part
        .to_lowercase()
        .replace("freshly ground", "")
        .replace("to taste", "");

    let mut tokens = lowered.split_whitespace().peekable();
    let mut quantity: Option<f32> = None;
    let mut pending_unit: Option<String> = None;

    // Leading quantity tokens (possibly several: "1 1/2", "1 ½").
    while let Some(tok) = tokens.peek() {
        if let Some(n) = parse_number(tok) {
            quantity = Some(quantity.unwrap_or(0.0) + n);
            tokens.next();
        } else if let Some((num, unit)) =
            split_attached_unit(tok).filter(|(_, u)| unit_factor(u).is_some())
        {
            if let Some(n) = parse_number(num) {
                quantity = Some(quantity.unwrap_or(0.0) + n);
                pending_unit = Some(unit.to_string());
                tokens.next();
            }
            break;
        } else {
            break;
        }
    }

    let mut rest: Vec<String> = tokens.map(str::to_string).collect();
    let mut unit = MeasurementUnit::ToTaste;
    let mut amount = None;

    let unit_word = pending_unit.or_else(|| {
        rest.first()
            .filter(|w| unit_factor(w).is_some())
            .cloned()
            .inspect(|_| {
                rest.remove(0);
            })
    });
    // "fl oz" -> drop the trailing "oz"
    if unit_word.as_deref() == Some("fl") && rest.first().map(|w| w == "oz").unwrap_or(false) {
        rest.remove(0);
    }
    if rest.first().map(|w| w == "of").unwrap_or(false) {
        rest.remove(0);
    }

    match (quantity, unit_word.as_deref().and_then(unit_factor)) {
        (Some(q), Some((u, factor))) if u != MeasurementUnit::ToTaste => {
            unit = u;
            amount = Some(round_amount(q * factor));
        }
        (Some(q), None) => {
            unit = MeasurementUnit::Pcs;
            amount = Some(round_amount(q));
        }
        _ => {}
    }

    IngredientItem {
        raw_text,
        metric_amount: amount,
        unit,
        canonical_id: canonical_from_words(&rest),
        is_specialty: false,
        canonical_label: None,
    }
}
