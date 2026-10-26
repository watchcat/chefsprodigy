//! Stage 1: free, local extraction of `Schema.org/Recipe` JSON-LD.

use scraper::{Html, Selector};
use serde_json::Value;

/// Structured recipe data lifted from a page's JSON-LD, before normalization.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemaOrgRecipe {
    pub name: String,
    pub description: Option<String>,
    /// Raw ingredient lines exactly as published.
    pub ingredients: Vec<String>,
    /// Plain-text instruction steps in order (sections flattened).
    pub instructions: Vec<String>,
    pub servings: Option<u8>,
    pub prep_minutes: Option<u16>,
    pub cook_minutes: Option<u16>,
    pub total_minutes: Option<u16>,
    pub image_url: Option<String>,
    /// Lowercased keywords, cuisine and category (may contain duplicates).
    pub tags: Vec<String>,
    /// Source language: JSON-LD `inLanguage`, else `<html lang>`.
    pub language: Option<String>,
}

/// Find the first `Recipe` node in any `application/ld+json` block of `html`.
pub fn parse_schema_org(html: &str) -> Option<SchemaOrgRecipe> {
    let doc = Html::parse_document(html);
    let html_lang = Selector::parse("html")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .and_then(|el| el.value().attr("lang"))
        .and_then(recipe_core::normalize_lang);
    let selector = Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;
    doc.select(&selector).find_map(|script| {
        let text: String = script.text().collect();
        let value: Value = serde_json::from_str(text.trim()).ok()?;
        find_recipe_node(&value).map(|node| {
            let mut recipe = from_node(node);
            recipe.language = node
                .get("inLanguage")
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(a) => a.iter().find_map(|x| x.as_str().map(str::to_string)),
                    Value::Object(o) => o
                        .get("name")
                        .or_else(|| o.get("alternateName"))
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    _ => None,
                })
                .and_then(|s| recipe_core::normalize_lang(&s))
                .or_else(|| html_lang.clone());
            recipe
        })
    })
}

fn is_recipe(v: &Value) -> bool {
    match v.get("@type") {
        Some(Value::String(t)) => t == "Recipe",
        Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str() == Some("Recipe")),
        _ => false,
    }
}

fn find_recipe_node(v: &Value) -> Option<&Value> {
    match v {
        Value::Array(items) => items.iter().find_map(find_recipe_node),
        Value::Object(_) if is_recipe(v) => Some(v),
        Value::Object(_) => v.get("@graph").and_then(find_recipe_node),
        _ => None,
    }
}

fn from_node(node: &Value) -> SchemaOrgRecipe {
    let str_field = |k: &str| {
        node.get(k)
            .and_then(Value::as_str)
            .map(|s| plain_text(s).join(" "))
    };
    let mut instructions = Vec::new();
    if let Some(v) = node.get("recipeInstructions") {
        collect_instructions(v, &mut instructions);
    }
    let mut tags: Vec<String> = Vec::new();
    for key in ["keywords", "recipeCuisine", "recipeCategory"] {
        if let Some(v) = node.get(key) {
            collect_strings(v, &mut tags);
        }
    }
    let tags = tags
        .iter()
        .flat_map(|t| t.split(','))
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    SchemaOrgRecipe {
        name: str_field("name").unwrap_or_default(),
        description: str_field("description").filter(|d| !d.is_empty()),
        ingredients: node
            .get("recipeIngredient")
            .map(|v| {
                let mut out = Vec::new();
                collect_strings(v, &mut out);
                out.into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        instructions,
        servings: node.get("recipeYield").and_then(parse_yield),
        prep_minutes: node
            .get("prepTime")
            .and_then(Value::as_str)
            .and_then(iso8601_duration_minutes),
        cook_minutes: node
            .get("cookTime")
            .and_then(Value::as_str)
            .and_then(iso8601_duration_minutes),
        total_minutes: node
            .get("totalTime")
            .and_then(Value::as_str)
            .and_then(iso8601_duration_minutes),
        image_url: node.get("image").and_then(first_image_url),
        tags,
        language: None,
    }
}

fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| collect_strings(i, out)),
        Value::Object(_) => {
            if let Some(s) = v.get("name").and_then(Value::as_str) {
                out.push(s.to_string());
            }
        }
        _ => {}
    }
}

fn collect_instructions(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.extend(plain_text(s)),
        Value::Array(items) => items.iter().for_each(|i| collect_instructions(i, out)),
        Value::Object(obj) => {
            if let Some(items) = obj.get("itemListElement") {
                collect_instructions(items, out);
            } else if let Some(text) = obj
                .get("text")
                .or_else(|| obj.get("name"))
                .and_then(Value::as_str)
            {
                out.extend(plain_text(text));
            }
        }
        _ => {}
    }
}

/// Decode HTML entities and tags in a string, splitting block elements and
/// newlines into separate non-empty lines.
fn plain_text(s: &str) -> Vec<String> {
    let fragment = Html::parse_fragment(s);
    let blocks = Selector::parse("p, li, div, h1, h2, h3, h4").ok();
    let mut lines: Vec<String> = Vec::new();
    if let Some(sel) = &blocks {
        for el in fragment.select(sel) {
            // Only take leaf blocks so nested containers don't duplicate text.
            if el.select(sel).next().is_none() {
                lines.push(el.text().collect::<String>());
            }
        }
    }
    if lines.is_empty() {
        lines.push(fragment.root_element().text().collect::<String>());
    }
    lines
        .iter()
        .flat_map(|l| l.split('\n'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect()
}

fn first_image_url(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(first_image_url),
        Value::Object(obj) => obj
            .get("url")
            .or_else(|| obj.get("contentUrl"))
            .and_then(first_image_url),
        _ => None,
    }
}

fn parse_yield(v: &Value) -> Option<u8> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u8::try_from(n).ok()),
        Value::String(s) => leading_number(s)
            .and_then(|n| u8::try_from(n.round() as i64).ok())
            .filter(|n| *n > 0),
        Value::Array(items) => items.iter().find_map(parse_yield),
        _ => None,
    }
}

fn leading_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let end = s[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|e| start + e)
        .unwrap_or(s.len());
    s[start..end].parse().ok()
}

/// Parse an ISO 8601 duration (`PT1H30M`, `P1DT2H`, `PT90S`) into whole minutes
/// (seconds rounded). Returns `None` for anything that is not ISO 8601.
pub fn iso8601_duration_minutes(s: &str) -> Option<u16> {
    let s = s.trim();
    let rest = s.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, t),
        None => (rest, ""),
    };
    let mut seconds = 0f64;
    for (part, is_time) in [(date_part, false), (time_part, true)] {
        let mut num = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
                continue;
            }
            let n: f64 = num.parse().ok()?;
            num.clear();
            let mult = match (c, is_time) {
                ('W', false) => 7.0 * 86_400.0,
                ('D', false) => 86_400.0,
                ('H', true) => 3_600.0,
                ('M', true) => 60.0,
                ('S', true) => 1.0,
                _ => return None,
            };
            seconds += n * mult;
        }
        if !num.is_empty() {
            return None;
        }
    }
    if rest.is_empty() {
        return None;
    }
    u16::try_from((seconds / 60.0).round() as i64).ok()
}
