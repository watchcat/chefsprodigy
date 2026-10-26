//! Mealie backup importer.
//!
//! Two archive layouts are understood:
//! * Mealie 1.x backups: a `database.json` dump with `recipes`,
//!   `recipes_ingredients`, `recipe_instructions`, `ingredient_units`,
//!   `ingredient_foods`, `tags`, `recipes_to_tags` tables and images under
//!   `data/recipes/<id>/images/original.*`.
//! * Per-recipe JSON exports (`recipes/<slug>/<slug>.json` with camelCase
//!   fields like `recipeIngredient`) and an `images/original.*` sibling.

use std::collections::HashMap;
use std::io::{Read, Seek};

use recipe_core::{IngredientItem, RecipeInput};
use serde_json::Value;
use zip::ZipArchive;

use crate::ScraperError;
use crate::importer::{DEFAULT_SERVINGS, build_steps, dedup_tags, fill_language};
use crate::jsonld::iso8601_duration_minutes;
use crate::normalize::heuristic_ingredient;

/// One recipe lifted from a Mealie archive.
#[derive(Debug, Clone)]
pub struct MealieRecipe {
    pub recipe: RecipeInput,
    /// Original cover image bytes, if the archive contained one.
    pub image: Option<Vec<u8>>,
    /// Mealie's own id or slug, for logging and dedup.
    pub source_id: Option<String>,
}

/// Read every recipe from a Mealie backup zip.
pub fn read_mealie_backup<R: Read + Seek>(reader: R) -> Result<Vec<MealieRecipe>, ScraperError> {
    let mut archive = ZipArchive::new(reader)?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.into_owned();
        if name.ends_with(".json") || name.contains("/images/") {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            files.insert(name, buf);
        }
    }

    let mut out = Vec::new();
    if let Some((_, dump)) = files
        .iter()
        .find(|(n, _)| *n == "database.json" || n.ends_with("/database.json"))
    {
        let value: Value = serde_json::from_slice(dump)?;
        out.extend(parse_database_dump(&value, &files));
    }
    for (name, bytes) in &files {
        if !name.ends_with(".json") || name.ends_with("database.json") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            continue;
        };
        if value.get("name").and_then(Value::as_str).is_some()
            && (value.get("recipeIngredient").is_some() || value.get("recipe_ingredient").is_some())
        {
            let dir = name.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            out.push(parse_recipe_export(&value, dir, &files));
        }
    }
    if out.is_empty() {
        return Err(ScraperError::InvalidBackup(
            "no recipes found in archive".to_string(),
        ));
    }
    Ok(out)
}

fn rows<'a>(db: &'a Value, table: &str) -> impl Iterator<Item = &'a Value> {
    db.get(table)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn find_image(files: &HashMap<String, Vec<u8>>, needle: &str) -> Option<Vec<u8>> {
    files
        .iter()
        .find(|(n, _)| n.contains(needle))
        .map(|(_, b)| b.clone())
}

fn parse_database_dump(db: &Value, files: &HashMap<String, Vec<u8>>) -> Vec<MealieRecipe> {
    let units: HashMap<&str, String> = rows(db, "ingredient_units")
        .filter_map(|u| {
            let id = u.get("id").and_then(Value::as_str)?;
            let label = str_of(u, "abbreviation").or_else(|| str_of(u, "name"))?;
            Some((id, label))
        })
        .collect();
    let foods: HashMap<&str, String> = rows(db, "ingredient_foods")
        .filter_map(|f| Some((f.get("id").and_then(Value::as_str)?, str_of(f, "name")?)))
        .collect();
    let tags: HashMap<&str, String> = rows(db, "tags")
        .filter_map(|t| Some((t.get("id").and_then(Value::as_str)?, str_of(t, "name")?)))
        .collect();

    rows(db, "recipes")
        .filter_map(|r| {
            let id = r.get("id").and_then(Value::as_str)?;
            let title = str_of(r, "name")?;

            let mut ingredient_rows: Vec<&Value> = rows(db, "recipes_ingredients")
                .filter(|i| i.get("recipe_id").and_then(Value::as_str) == Some(id))
                .collect();
            ingredient_rows.sort_by_key(|i| i.get("position").and_then(Value::as_i64).unwrap_or(0));
            let ingredients = ingredient_rows
                .iter()
                .map(|i| {
                    let unit = i
                        .get("unit_id")
                        .and_then(Value::as_str)
                        .and_then(|u| units.get(u))
                        .cloned();
                    let food = i
                        .get("food_id")
                        .and_then(Value::as_str)
                        .and_then(|f| foods.get(f))
                        .cloned();
                    ingredient_from_parts(
                        str_of(i, "original_text"),
                        None,
                        i.get("quantity"),
                        unit,
                        food,
                        str_of(i, "note"),
                    )
                })
                .collect();

            let mut step_rows: Vec<&Value> = rows(db, "recipe_instructions")
                .filter(|s| s.get("recipe_id").and_then(Value::as_str) == Some(id))
                .collect();
            step_rows.sort_by_key(|s| s.get("position").and_then(Value::as_i64).unwrap_or(0));
            let instructions = build_steps(
                step_rows
                    .iter()
                    .filter_map(|s| s.get("text").and_then(Value::as_str))
                    .map(|t| (t, None)),
            );

            let recipe_tags: Vec<String> = rows(db, "recipes_to_tags")
                .filter(|rt| rt.get("recipe_id").and_then(Value::as_str) == Some(id))
                .filter_map(|rt| rt.get("tag_id").and_then(Value::as_str))
                .filter_map(|tid| tags.get(tid).cloned())
                .collect();

            let image = find_image(files, &format!("/{id}/images/original."));
            let mut recipe = RecipeInput {
                title,
                description: str_of(r, "description"),
                servings: str_of(r, "recipe_yield")
                    .and_then(|y| leading_u8(&y))
                    .unwrap_or(DEFAULT_SERVINGS),
                prep_time_minutes: str_of(r, "prep_time").and_then(|d| human_duration_minutes(&d)),
                cook_time_minutes: str_of(r, "cook_time")
                    .or_else(|| str_of(r, "perform_time"))
                    .and_then(|d| human_duration_minutes(&d)),
                ingredients,
                instructions,
                tags: dedup_tags(recipe_tags),
                image_path: None,
                origin_url: str_of(r, "org_url").or_else(|| str_of(r, "orgURL")),
                author_did: None,
                is_public: false,
                language: None,
            };
            fill_language(&mut recipe);
            Some(MealieRecipe {
                recipe,
                image,
                source_id: Some(id.to_string()),
            })
        })
        .collect()
}

fn parse_recipe_export(v: &Value, dir: &str, files: &HashMap<String, Vec<u8>>) -> MealieRecipe {
    let ingredients = v
        .get("recipeIngredient")
        .or_else(|| v.get("recipe_ingredient"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|i| match i {
            Value::String(s) => heuristic_ingredient(s),
            _ => ingredient_from_parts(
                str_of(i, "originalText").or_else(|| str_of(i, "original_text")),
                str_of(i, "display"),
                i.get("quantity"),
                i.get("unit")
                    .and_then(|u| str_of(u, "abbreviation").or_else(|| str_of(u, "name"))),
                i.get("food").and_then(|f| str_of(f, "name")),
                str_of(i, "note"),
            ),
        })
        .collect();

    let instructions = build_steps(
        v.get("recipeInstructions")
            .or_else(|| v.get("recipe_instructions"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().or_else(|| s.get("text").and_then(Value::as_str)))
            .map(|t| (t, None)),
    );

    let tags = v
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().map(str::to_string).or_else(|| str_of(t, "name")))
        .collect();

    let image = find_image(files, &format!("{dir}/images/original."))
        .or_else(|| find_image(files, &format!("{dir}/original.")));

    let mut recipe = RecipeInput {
        title: str_of(v, "name").unwrap_or_else(|| "Untitled".to_string()),
        description: str_of(v, "description"),
        servings: str_of(v, "recipeYield")
            .or_else(|| str_of(v, "recipe_yield"))
            .and_then(|y| leading_u8(&y))
            .unwrap_or(DEFAULT_SERVINGS),
        prep_time_minutes: str_of(v, "prepTime")
            .or_else(|| str_of(v, "prep_time"))
            .and_then(|d| human_duration_minutes(&d)),
        cook_time_minutes: str_of(v, "performTime")
            .or_else(|| str_of(v, "cookTime"))
            .or_else(|| str_of(v, "perform_time"))
            .or_else(|| str_of(v, "cook_time"))
            .and_then(|d| human_duration_minutes(&d)),
        ingredients,
        instructions,
        tags: dedup_tags(tags),
        image_path: None,
        origin_url: str_of(v, "orgURL").or_else(|| str_of(v, "org_url")),
        author_did: None,
        is_public: false,
        language: None,
    };
    fill_language(&mut recipe);
    MealieRecipe {
        recipe,
        image,
        source_id: str_of(v, "id").or_else(|| str_of(v, "slug")),
    }
}

/// Prefer Mealie's original text, then its display string, else compose
/// "<quantity> <unit> <food>, <note>" and run the heuristic normalizer.
fn ingredient_from_parts(
    original: Option<String>,
    display: Option<String>,
    quantity: Option<&Value>,
    unit: Option<String>,
    food: Option<String>,
    note: Option<String>,
) -> IngredientItem {
    let raw = original.or(display).unwrap_or_else(|| {
        let mut parts: Vec<String> = Vec::new();
        if let Some(q) = quantity.and_then(Value::as_f64).filter(|q| *q > 0.0) {
            parts.push(if q.fract() == 0.0 {
                format!("{}", q as i64)
            } else {
                format!("{q}")
            });
        }
        parts.extend(unit);
        parts.extend(food);
        let mut raw = parts.join(" ");
        if let Some(n) = note {
            if raw.is_empty() {
                raw = n
            } else {
                raw = format!("{raw}, {n}")
            }
        }
        raw
    });
    heuristic_ingredient(&raw)
}

fn leading_u8(s: &str) -> Option<u8> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u8>().ok().filter(|n| *n > 0)
}

/// "30 minutes", "1 hour 30 minutes", "45 min", "PT5M" -> minutes.
pub fn human_duration_minutes(s: &str) -> Option<u16> {
    if let Some(m) = iso8601_duration_minutes(s) {
        return Some(m);
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut total = 0f32;
    let mut found = false;
    for (i, w) in words.iter().enumerate() {
        let Ok(n) = w.parse::<f32>() else { continue };
        let unit = words
            .get(i + 1)
            .map(|u| u.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase());
        let mult = match unit.as_deref() {
            Some("hour" | "hours" | "hr" | "hrs" | "h") => 60.0,
            Some("minute" | "minutes" | "min" | "mins" | "m") => 1.0,
            Some("day" | "days" | "d") => 1440.0,
            _ => continue,
        };
        total += n * mult;
        found = true;
    }
    found.then(|| total.round() as u16)
}
