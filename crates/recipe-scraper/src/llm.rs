//! Stage 2: structured extraction through `rig-core`, behind a trait so the
//! importer can run without a model (heuristics) or with a fake in tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::{CompletionRequest, Message};
use rig_core::message::{DocumentSourceKind, Image, ImageMediaType, UserContent};
use rig_core::operation::Completion;
use rig_core::providers::registry::ConnectOptions;
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ScraperError;
use crate::images::NormalizedPhoto;
use crate::normalize::heuristic_ingredient;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Environment variable naming the model, as a rig catalog reference
/// (`vendor/model`, e.g. `minimax/MiniMax-M3`, `anthropic/claude-haiku-4-5`, `ollama/llama3.2`).
pub const LLM_MODEL_ENV: &str = "LLM_MODEL";

/// Optional explicit credential; otherwise the vendor's conventional variable
/// (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, ...) is used.
pub const LLM_API_KEY_ENV: &str = "LLM_API_KEY";

/// Connection options for `reference`. With `LLM_API_KEY` set, that key is
/// used and nothing else is read. Otherwise rig reads the vendor's own
/// variables (`MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, endpoint overrides such
/// as `MINIMAX_ANTHROPIC_API_BASE`, ...); local vendors need no key.
fn connect_options() -> ConnectOptions {
    match std::env::var(LLM_API_KEY_ENV).map(|k| k.trim().to_string()) {
        Ok(k) if !k.is_empty() => ConnectOptions::new().api_key(k),
        _ => ConnectOptions::new(),
    }
}

/// Recipe draft produced by full-text extraction.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ExtractedRecipeDraft {
    /// Recipe title
    pub title: String,
    /// One or two sentence description, if the source has one
    pub description: Option<String>,
    /// Number of servings, if stated
    pub servings: Option<u8>,
    /// Active preparation time in minutes, if stated
    pub prep_time_minutes: Option<u16>,
    /// Cooking time in minutes, if stated
    pub cook_time_minutes: Option<u16>,
    /// Every ingredient, normalized to metric units with a snake_case canonical_id
    pub ingredients: Vec<IngredientItem>,
    /// Ordered steps; extract any mentioned duration into timer_seconds
    pub instructions: Vec<InstructionStep>,
    /// Short lowercase tags: cuisine, course, diet
    pub tags: Vec<String>,
    /// BCP-47 language code of the source text (e.g. "nl", "ru")
    #[serde(default)]
    pub language: Option<String>,
}

/// What the translator is asked to translate: the presentation strings of one recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TranslationRequest {
    /// Target language code
    pub target_lang: String,
    /// Source language code when known
    pub source_lang: Option<String>,
    pub title: String,
    pub description: Option<String>,
    /// Ingredient lines, in order
    pub ingredients: Vec<String>,
    /// Step texts, in order
    pub instructions: Vec<String>,
    pub tags: Vec<String>,
}

/// The translator's reply: same shape and order as the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TranslationReply {
    pub title: String,
    pub description: Option<String>,
    /// One translated line per source ingredient, same order
    pub ingredients: Vec<String>,
    /// One translated text per source step, same order
    pub instructions: Vec<String>,
    pub tags: Vec<String>,
}

/// What a photo shows, as judged by the vision model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PhotoKind {
    /// Printed or handwritten recipe text
    RecipeText,
    /// The finished dish
    Dish,
    /// Both the dish and recipe text
    Both,
    /// Nothing recipe-related
    Unrelated,
}

/// Classification of one uploaded photo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PhotoInfo {
    /// Zero-based position of the photo in the request
    pub index: usize,
    pub kind: PhotoKind,
    /// Short note, e.g. "handwritten, second page"
    pub notes: Option<String>,
}

/// Result of reading recipe photos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhotoExtraction {
    /// The recipe transcribed from the photos
    pub recipe: ExtractedRecipeDraft,
    /// One entry per photo, in request order
    pub photos: Vec<PhotoInfo>,
    /// Parts of the source that could not be read
    pub unreadable: Vec<String>,
    /// BCP-47 language tag of the source text
    pub language: Option<String>,
}

impl PhotoExtraction {
    /// Index of the first photo that shows the dish, if any.
    pub fn cover_index(&self) -> Option<usize> {
        self.photos
            .iter()
            .find(|p| matches!(p.kind, PhotoKind::Dish | PhotoKind::Both))
            .map(|p| p.index)
    }
}

/// Wrapper so the ingredient list is a JSON object at the top level.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct NormalizedIngredients {
    /// One entry per input line, in the same order, raw_text copied verbatim
    ingredients: Vec<IngredientItem>,
}

// ---------------------------------------------------------------------------
// Lenient wire types. Models drop or rename fields; these accept the common
// variants and fill defaults so one sloppy ingredient does not sink an import.

fn snake_case(s: &str) -> String {
    let id = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();
    id.split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

// ---- lenient scalar deserializers: models send "3–4", "15 minutes", "0,5", "Grams" ...

fn first_number(s: &str) -> Option<f32> {
    let cleaned = s.replace(',', ".");
    let start = cleaned.find(|c: char| c.is_ascii_digit())?;
    let end = cleaned[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|e| start + e)
        .unwrap_or(cleaned.len());
    cleaned[start..end].trim_end_matches('.').parse().ok()
}

fn de_f32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|v| v as f32),
        Some(serde_json::Value::String(s)) => first_number(&s),
        _ => None,
    })
}

fn de_u8<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    Ok(de_f32(d)?
        .filter(|v| *v > 0.0)
        .and_then(|v| u8::try_from(v.round() as i64).ok()))
}

fn de_u32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|v| v.round() as u32),
        Some(serde_json::Value::String(s)) => first_number(&s)
            .map(|v| v.round() as u32)
            .or_else(|| crate::importer::extract_timer_seconds(&s)),
        _ => None,
    })
}

/// Minutes from a number, a seconds field, or a phrase like "1 hour 10 min".
fn de_minutes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .and_then(|v| u16::try_from(v.round() as i64).ok()),
        Some(serde_json::Value::String(s)) => crate::mealie::human_duration_minutes(&s)
            .or_else(|| first_number(&s).and_then(|v| u16::try_from(v.round() as i64).ok())),
        _ => None,
    })
}

fn de_seconds_as_minutes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    Ok(de_u32(d)?
        .map(|secs| secs.div_ceil(60))
        .and_then(|m| u16::try_from(m).ok()))
}

fn de_unit<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<MeasurementUnit>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.and_then(|u| {
        let u = u.trim().to_lowercase().replace([' ', '-'], "_");
        match u.as_str() {
            "g" | "gram" | "grams" | "gr" => Some(MeasurementUnit::G),
            "ml" | "milliliter" | "milliliters" | "millilitre" | "millilitres" => {
                Some(MeasurementUnit::Ml)
            }
            "pcs" | "pc" | "piece" | "pieces" | "count" | "whole" | "unit" | "units" => {
                Some(MeasurementUnit::Pcs)
            }
            "to_taste" | "totaste" | "pinch" | "none" | "" => Some(MeasurementUnit::ToTaste),
            _ => None, // e.g. "tbsp": resolved from raw_text by the caller
        }
    }))
}

#[derive(Debug, Default, Deserialize)]
struct WireIngredient {
    #[serde(
        default,
        alias = "text",
        alias = "name",
        alias = "original",
        alias = "original_text",
        alias = "line"
    )]
    raw_text: Option<String>,
    #[serde(
        default,
        alias = "amount",
        alias = "quantity",
        alias = "metric_quantity",
        deserialize_with = "de_f32"
    )]
    metric_amount: Option<f32>,
    #[serde(default, alias = "metric_unit", deserialize_with = "de_unit")]
    unit: Option<MeasurementUnit>,
    /// Set when the model's unit was not one of ours (e.g. "tbsp"); resolved from raw_text.
    #[serde(skip)]
    _unused: (),
    #[serde(
        default,
        alias = "canonical",
        alias = "id",
        alias = "ingredient",
        alias = "ingredient_id"
    )]
    canonical_id: Option<String>,
    #[serde(default, alias = "specialty")]
    is_specialty: bool,
}

impl WireIngredient {
    fn into_item(self) -> Option<IngredientItem> {
        let canonical_id = self
            .canonical_id
            .as_deref()
            .map(snake_case)
            .filter(|c| !c.is_empty())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .map(|r| heuristic_ingredient(r).canonical_id)
            })?;
        let (metric_amount, unit) = match (self.unit, self.metric_amount, self.raw_text.as_deref())
        {
            (Some(u), amount, _) => (amount, u),
            // No usable unit from the model: let the heuristic parser read the original wording.
            (None, _, Some(raw)) if raw.trim().chars().any(|c| c.is_ascii_digit()) => {
                let h = heuristic_ingredient(raw);
                if h.metric_amount.is_some() {
                    (h.metric_amount, h.unit)
                } else {
                    (self.metric_amount, MeasurementUnit::Pcs)
                }
            }
            (None, Some(a), _) => (Some(a), MeasurementUnit::Pcs),
            (None, None, _) => (None, MeasurementUnit::ToTaste),
        };
        let metric_amount = if unit == MeasurementUnit::ToTaste {
            None
        } else {
            metric_amount
        };
        let raw_text = self
            .raw_text
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| {
                let name = canonical_id.replace('_', " ");
                match metric_amount {
                    Some(a) => format!("{a} {} {name}", unit.as_str()),
                    None => name,
                }
            });
        Some(IngredientItem {
            raw_text,
            metric_amount,
            unit,
            canonical_id,
            is_specialty: self.is_specialty,
            canonical_label: None,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireStep {
    Text(String),
    Step {
        #[serde(
            default,
            alias = "number",
            alias = "step",
            alias = "index",
            deserialize_with = "de_u32"
        )]
        step_number: Option<u32>,
        #[serde(default, alias = "instruction", alias = "description")]
        text: String,
        #[serde(
            default,
            alias = "timer",
            alias = "duration_seconds",
            deserialize_with = "de_u32"
        )]
        timer_seconds: Option<u32>,
    },
}

#[derive(Debug, Deserialize)]
struct WireRecipe {
    #[serde(
        default,
        alias = "name",
        alias = "recipe_name",
        alias = "recipe_title",
        alias = "dish",
        alias = "heading"
    )]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(
        default,
        alias = "yield",
        alias = "serves",
        alias = "portions",
        deserialize_with = "de_u8"
    )]
    servings: Option<u8>,
    #[serde(
        default,
        alias = "prep_time",
        alias = "prep_minutes",
        deserialize_with = "de_minutes"
    )]
    prep_time_minutes: Option<u16>,
    #[serde(
        default,
        alias = "cook_time",
        alias = "cook_minutes",
        deserialize_with = "de_minutes"
    )]
    cook_time_minutes: Option<u16>,
    #[serde(default, alias = "total_time", deserialize_with = "de_minutes")]
    total_time_minutes: Option<u16>,
    #[serde(default, deserialize_with = "de_seconds_as_minutes")]
    total_time_seconds: Option<u16>,
    #[serde(default, deserialize_with = "de_seconds_as_minutes")]
    prep_time_seconds: Option<u16>,
    #[serde(default, deserialize_with = "de_seconds_as_minutes")]
    cook_time_seconds: Option<u16>,
    #[serde(default)]
    ingredients: Vec<WireIngredient>,
    #[serde(default, alias = "steps", alias = "method")]
    instructions: Vec<WireStep>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(
        default,
        alias = "lang",
        alias = "source_language",
        alias = "recipe_language",
        alias = "original_language"
    )]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireIngredients {
    Wrapped { ingredients: Vec<WireIngredient> },
    Bare(Vec<WireIngredient>),
}

/// Parse a full-recipe reply into a draft, tolerating fences, prose, missing
/// and renamed fields. Fails only when no usable title is present.
pub fn parse_recipe_reply(text: &str) -> Result<ExtractedRecipeDraft, ScraperError> {
    let value: serde_json::Value = parse_json_lenient(text)?;
    recipe_from_value(value)
}

/// Lenient conversion of an already-parsed JSON value into a draft.
pub fn recipe_from_value(
    mut value: serde_json::Value,
) -> Result<ExtractedRecipeDraft, ScraperError> {
    // Unwrap {"recipe": {...}}, {"data": {...}} style envelopes.
    for _ in 0..3 {
        let inner = ["recipe", "data", "result", "output", "extracted"]
            .iter()
            .find_map(|k| value.get(*k).filter(|v| v.is_object()).cloned());
        match inner {
            Some(v) => value = v,
            None => break,
        }
    }
    if value.get("properties").is_some()
        && value.get("type").and_then(|t| t.as_str()) == Some("object")
    {
        return Err(ScraperError::Llm(
            "model echoed the JSON schema instead of filling it".to_string(),
        ));
    }
    hoist_times(&mut value);
    let wire: WireRecipe = serde_json::from_value(value)
        .map_err(|e| ScraperError::Llm(format!("model output did not match the schema ({e})")))?;
    let ingredients: Vec<IngredientItem> = wire
        .ingredients
        .into_iter()
        .filter_map(WireIngredient::into_item)
        .filter(|i| !is_equipment(&i.canonical_id))
        .collect();
    let title = match wire
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        Some(t) => t,
        // Recipe cards often carry the title on the other side: name the dish by its ingredients.
        None if !ingredients.is_empty() => synthesized_title(&ingredients),
        None => {
            return Err(ScraperError::Llm(
                "model reply has no recipe title".to_string(),
            ));
        }
    };
    let instructions = wire
        .instructions
        .into_iter()
        .map(|s| match s {
            WireStep::Text(text) => (text, None, None),
            WireStep::Step {
                step_number,
                text,
                timer_seconds,
            } => (text, step_number, timer_seconds),
        })
        .filter(|(text, _, _)| !text.trim().is_empty())
        .enumerate()
        .map(|(i, (text, number, timer))| InstructionStep {
            step_number: number
                .and_then(|n| u16::try_from(n).ok())
                .unwrap_or((i + 1) as u16),
            text: text.trim().to_string(),
            timer_seconds: timer,
        })
        .collect();
    let prep = wire.prep_time_minutes.or(wire.prep_time_seconds);
    let mut cook = wire.cook_time_minutes.or(wire.cook_time_seconds);
    if prep.is_none() && cook.is_none() {
        cook = wire.total_time_minutes.or(wire.total_time_seconds);
    }
    Ok(ExtractedRecipeDraft {
        title,
        description: wire.description,
        servings: wire.servings,
        prep_time_minutes: prep,
        cook_time_minutes: cook,
        ingredients,
        instructions,
        tags: wire.tags,
        language: wire
            .language
            .as_deref()
            .and_then(recipe_core::normalize_lang),
    })
}

/// Move `{"times": {"prep_seconds": ..}}` / `{"time": {..}}` entries to the top level
/// where the lenient field aliases can see them.
fn hoist_times(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let nested = ["times", "time", "timing", "durations"]
        .iter()
        .find_map(|k| obj.remove(*k))
        .and_then(|v| v.as_object().cloned());
    let Some(nested) = nested else { return };
    for (k, v) in nested {
        let key = match k.as_str() {
            "prep_seconds" | "prep_time_seconds" => "prep_time_seconds",
            "cook_seconds" | "cook_time_seconds" => "cook_time_seconds",
            "total_seconds" | "total_time_seconds" => "total_time_seconds",
            "prep" | "prep_minutes" | "prep_time" | "prep_time_minutes" => "prep_time_minutes",
            "cook" | "cook_minutes" | "cook_time" | "cook_time_minutes" => "cook_time_minutes",
            "total" | "total_minutes" | "total_time" | "total_time_minutes" => "total_time_minutes",
            _ => continue,
        };
        obj.entry(key).or_insert(v);
    }
}

/// Kitchen tools that models sometimes list among ingredients.
const EQUIPMENT_WORDS: &[&str] = &[
    "grater",
    "kettle",
    "colander",
    "skillet",
    "saucepan",
    "frying_pan",
    "pan",
    "pot",
    "lid",
    "whisk",
    "blender",
    "mixer",
    "sieve",
    "strainer",
    "peeler",
    "tongs",
    "spatula",
    "ladle",
    "oven",
    "microwave",
    "knife",
    "board",
    "jug",
    "scale",
    "thermometer",
    "foil",
    "parchment",
    "skewer",
    "skewers",
];

fn is_equipment(canonical_id: &str) -> bool {
    let tokens: Vec<&str> = canonical_id.split('_').collect();
    // "frying_pan" / "saucepan" as a whole, or a bare tool word as the last token
    // ("large_skillet", "medium_saucepan_with_lid"); food names like "panko" or
    // "nori_sheet" do not end in a tool word.
    let last = tokens.last().copied().unwrap_or("");
    let has_tool_phrase =
        canonical_id.contains("frying_pan") || canonical_id.contains("measuring_");
    has_tool_phrase
        || EQUIPMENT_WORDS.contains(&last)
        || tokens.windows(2).any(|w| w == ["with", "lid"])
}

/// "Torchiette, parma ham slices & cherry tomato" from the first three ingredients.
fn synthesized_title(ingredients: &[IngredientItem]) -> String {
    let names: Vec<String> = ingredients
        .iter()
        .filter(|i| i.unit != MeasurementUnit::ToTaste)
        .chain(
            ingredients
                .iter()
                .filter(|i| i.unit == MeasurementUnit::ToTaste),
        )
        .map(|i| i.canonical_id.replace('_', " "))
        .take(3)
        .collect();
    let mut title = match names.len() {
        0 => "Untitled recipe".to_string(),
        1 => names[0].clone(),
        n => format!("{} & {}", names[..n - 1].join(", "), names[n - 1]),
    };
    if let Some(first) = title.get(..1) {
        title = first.to_uppercase() + &title[1..];
    }
    title
}

/// Whether a model note describes a photograph of the finished dish.
fn mentions_dish_photo(notes: &str) -> bool {
    let n = notes.to_lowercase();
    [
        "dish photo",
        "photo of the dish",
        "photograph of the dish",
        "dish photograph",
        "plated dish",
        "finished dish",
        "food photo",
        "hero image",
        "hero photo",
        "photo of the finished",
        "picture of the dish",
    ]
    .iter()
    .any(|k| n.contains(k))
}

fn photo_kind_from_str(s: &str) -> PhotoKind {
    match s.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
        "recipe_text" | "text" | "recipe" | "page" | "card" | "handwritten" => {
            PhotoKind::RecipeText
        }
        "dish" | "food" | "plate" | "meal" | "dish_photo" | "finished_dish" => PhotoKind::Dish,
        "both" | "mixed" | "dish_and_text" | "text_and_dish" | "cover" | "front" | "card_front" => {
            PhotoKind::Both
        }
        _ => PhotoKind::Unrelated,
    }
}

/// Parse a photo-reading reply. The recipe may sit under `recipe` or at the top
/// level; photo classifications are optional and read leniently.
pub fn parse_photo_reply(text: &str) -> Result<PhotoExtraction, ScraperError> {
    let value: serde_json::Value = parse_json_lenient(text)?;
    let photos: Vec<PhotoInfo> = value
        .get("photos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, p)| {
                    let notes = p
                        .get("notes")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .filter(|n| !n.is_empty());
                    let kind = [
                        "kind",
                        "classification",
                        "type",
                        "category",
                        "label",
                        "content",
                    ]
                    .iter()
                    .find_map(|k| p.get(*k).and_then(|v| v.as_str()))
                    .map(photo_kind_from_str)
                    .unwrap_or(PhotoKind::Unrelated);
                    // Models often file a card front as "text" while describing its hero photo.
                    let kind = match kind {
                        PhotoKind::RecipeText | PhotoKind::Unrelated
                            if notes.as_deref().is_some_and(mentions_dish_photo) =>
                        {
                            PhotoKind::Both
                        }
                        k => k,
                    };
                    PhotoInfo {
                        index: p
                            .get("index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(i),
                        kind,
                        notes,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let unreadable: Vec<String> = value
        .get("unreadable")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let language = value
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|l| !l.is_empty());

    let recipe_value = match value.get("recipe") {
        Some(r) if r.is_object() => r.clone(),
        Some(_) => serde_json::Value::Null,
        None => value.clone(),
    };
    let explanation = || {
        let notes: Vec<String> = photos.iter().filter_map(|p| p.notes.clone()).collect();
        if notes.is_empty() {
            "the photos do not contain a readable recipe".to_string()
        } else {
            notes.join("; ")
        }
    };
    if recipe_value.is_null() {
        return Err(ScraperError::NoRecipeInPhotos(explanation()));
    }
    let recipe = match recipe_from_value(recipe_value) {
        Ok(r) => r,
        Err(ScraperError::Llm(m)) if m.contains("no recipe title") => {
            return Err(ScraperError::NoRecipeInPhotos(explanation()));
        }
        Err(e) => return Err(e),
    };
    Ok(PhotoExtraction {
        recipe,
        photos,
        unreadable,
        language,
    })
}

const TRANSLATE_PREAMBLE: &str = "You translate recipes for home cooks. Translate every string in the request into the \
target language, naturally and idiomatically. Keep quantities, units, product and brand names exactly as written. Keep the \
same number and order of ingredients and steps; never merge, split, add or drop items. Return JSON only with the same \
shape as the request: title, description, ingredients (array of strings), instructions (array of strings), tags.";

/// Reply schema for language detection.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DetectedLanguage {
    /// BCP-47 language code of the text, e.g. "nl"; null when unsure
    language: Option<String>,
}

const DETECT_PREAMBLE: &str = "Identify the language the recipe text is written in. Reply with JSON only: \
{\"language\": \"<BCP-47 code such as nl, en, ru>\"} or {\"language\": null} if unsure.";

/// Wrapper for the term-label reply schema.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct TermLabels {
    /// canonical_id -> label in the target language
    labels: HashMap<String, String>,
}

const TERMS_PREAMBLE: &str = "You name grocery ingredients for shoppers. For each English snake_case canonical ingredient id, \
give the natural short name a shopper in the target language would use (e.g. garlic -> чеснок for ru). Return JSON only: \
{\"labels\": {\"<canonical_id>\": \"<label>\", ...}} covering every id.";

/// Parse a term-label reply: `{"labels": {id: label}}`, a bare map, or an
/// array of `{canonical_id|id, label}` objects. Unknown ids are dropped.
pub fn parse_term_labels(
    text: &str,
    ids: &[String],
) -> Result<HashMap<String, String>, ScraperError> {
    let value: serde_json::Value = parse_json_lenient(text)?;
    let mut out = HashMap::new();
    let mut take = |id: &str, label: &str| {
        let label = label.trim();
        if !label.is_empty() && ids.iter().any(|x| x == id) {
            out.insert(id.to_string(), label.to_string());
        }
    };
    match value.get("labels").unwrap_or(&value) {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if let Some(l) = v.as_str() {
                    take(k, l);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let id = ["canonical_id", "id", "term"]
                    .iter()
                    .find_map(|k| item.get(*k).and_then(|v| v.as_str()));
                let label = ["label", "name", "translation", "text"]
                    .iter()
                    .find_map(|k| item.get(*k).and_then(|v| v.as_str()));
                if let (Some(id), Some(label)) = (id, label) {
                    take(id, label);
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

/// Read a list that may come as strings or as `{index, text}` objects.
fn strings_from(v: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let arr = v?.as_array()?;
    let mut indexed: Vec<(usize, String)> = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let (idx, text) = match item {
            serde_json::Value::String(s) => (i, s.clone()),
            serde_json::Value::Object(o) => {
                let text = ["text", "translation", "value", "raw_text", "line"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(|t| t.as_str()))
                    .unwrap_or("")
                    .to_string();
                (
                    o.get("index")
                        .and_then(|n| n.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(i),
                    text,
                )
            }
            _ => (i, String::new()),
        };
        indexed.push((idx, text));
    }
    indexed.sort_by_key(|(i, _)| *i);
    Some(indexed.into_iter().map(|(_, t)| t).collect())
}

/// Parse a translation reply against its request: lists must match in length,
/// optional parts fall back to the source.
pub fn parse_translation_reply(
    text: &str,
    req: &TranslationRequest,
) -> Result<TranslationReply, ScraperError> {
    let mut value: serde_json::Value = parse_json_lenient(text)?;
    for key in ["translation", "result", "data"] {
        if let Some(inner) = value.get(key).filter(|v| v.is_object()).cloned() {
            value = inner;
        }
    }
    let title = value
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ScraperError::TranslationIncomplete("no title in the reply".to_string()))?
        .to_string();
    let ingredients = strings_from(value.get("ingredients")).unwrap_or_default();
    if ingredients.len() != req.ingredients.len() {
        return Err(ScraperError::TranslationIncomplete(format!(
            "expected {} ingredient lines, got {}",
            req.ingredients.len(),
            ingredients.len()
        )));
    }
    let instructions =
        strings_from(value.get("instructions").or_else(|| value.get("steps"))).unwrap_or_default();
    if instructions.len() != req.instructions.len() {
        return Err(ScraperError::TranslationIncomplete(format!(
            "expected {} steps, got {}",
            req.instructions.len(),
            instructions.len()
        )));
    }
    let description = value
        .get("description")
        .and_then(|d| d.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| req.description.clone());
    let tags = strings_from(value.get("tags"))
        .filter(|t| t.len() == req.tags.len())
        .unwrap_or_else(|| req.tags.clone());
    Ok(TranslationReply {
        title,
        description,
        ingredients,
        instructions,
        tags,
    })
}

/// Parse an ingredient-normalization reply (wrapped object or bare array).
pub fn parse_ingredients_reply(text: &str) -> Result<Vec<IngredientItem>, ScraperError> {
    let wire: WireIngredients = parse_json_lenient(text)?;
    let items = match wire {
        WireIngredients::Wrapped { ingredients } | WireIngredients::Bare(ingredients) => {
            ingredients
        }
    };
    Ok(items
        .into_iter()
        .filter_map(WireIngredient::into_item)
        .collect())
}

/// Structured-extraction backend used by the importer.
pub trait RecipeExtractor: Send + Sync {
    /// Normalize raw ingredient lines into the 3-tier model (stage 1 micro-normalization).
    fn normalize_ingredients<'a>(
        &'a self,
        raw: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<IngredientItem>, ScraperError>>;
    /// Extract a whole recipe from sanitized Markdown or plain text (stage 2).
    fn extract_recipe<'a>(
        &'a self,
        markdown: &'a str,
    ) -> BoxFuture<'a, Result<ExtractedRecipeDraft, ScraperError>>;
    /// Whether the backend can read images at all.
    fn supports_images(&self) -> bool {
        false
    }
    /// Transcribe a recipe from photos (cookbook page, card, dish).
    fn extract_from_photos<'a>(
        &'a self,
        _photos: &'a [NormalizedPhoto],
    ) -> BoxFuture<'a, Result<PhotoExtraction, ScraperError>> {
        Box::pin(async {
            Err(ScraperError::ModelCannotSeeImages(
                "no LLM is configured, so the node".to_string(),
            ))
        })
    }
    /// Whether this backend can translate recipe wording.
    fn supports_translation(&self) -> bool {
        true
    }
    /// Translate the presentation strings of a recipe.
    fn translate_recipe<'a>(
        &'a self,
        _req: &'a TranslationRequest,
    ) -> BoxFuture<'a, Result<TranslationReply, ScraperError>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
    /// Language of `sample` as a BCP-47 code when the model can tell; the
    /// offline heuristic (`langdetect`) is tried by callers first.
    fn detect_language<'a>(
        &'a self,
        _sample: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ScraperError>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
    /// Localized display labels for canonical ingredient ids (`garlic` -> "чеснок").
    fn translate_terms<'a>(
        &'a self,
        _ids: &'a [String],
        _lang: &'a str,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, ScraperError>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
}

/// No-LLM backend: heuristic ingredient parsing, no free-text extraction.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicExtractor;

impl RecipeExtractor for HeuristicExtractor {
    fn supports_translation(&self) -> bool {
        false
    }

    fn normalize_ingredients<'a>(
        &'a self,
        raw: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<IngredientItem>, ScraperError>> {
        Box::pin(async move { Ok(raw.iter().map(|r| heuristic_ingredient(r)).collect()) })
    }

    fn extract_recipe<'a>(
        &'a self,
        _markdown: &'a str,
    ) -> BoxFuture<'a, Result<ExtractedRecipeDraft, ScraperError>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
}

const NORMALIZE_PREAMBLE: &str = "You are a culinary technologist. For every raw ingredient line you receive, output one \
normalized entry in the same order. Copy raw_text verbatim. Convert all volume and imperial measurements (cups, tbsp, \
tsp, oz, lb, cloves, pinches) into metric grams (g), milliliters (ml), or pieces (pcs); use unit to_taste with a null \
metric_amount when no quantity is given. Generate a standardized English snake_case canonical_id naming only the \
ingredient itself (e.g. dark_soy_sauce, garlic). Set is_specialty to true for ethnic, gourmet, or hard-to-find \
ingredients not stocked by a basic convenience store. Every entry must contain raw_text, metric_amount (number or null), \
unit (one of g, ml, pcs, to_taste), canonical_id and is_specialty. Respond with a JSON object {\"ingredients\": [...]} only.";

const EXTRACT_PREAMBLE: &str = "You are a culinary technologist. Extract the recipe into the target schema. Convert all \
volume/imperial measurements (cups, tbsp, tsp, cloves, pinches) into metric grams (g), milliliters (ml), or pieces \
(pcs). Generate a standardized English canonical_id in snake_case for every ingredient. Set is_specialty = true for \
ethnic, gourmet, or hard-to-find ingredients. Extract any time durations mentioned in steps into timer_seconds. \
Number steps from 1. Every ingredient must contain raw_text (the original wording), metric_amount (number or null), unit \
(one of g, ml, pcs, to_taste), canonical_id and is_specialty. Respond with JSON only, no prose.";

const PHOTO_PREAMBLE: &str = "You are a culinary technologist reading photographs. The photos may show a cookbook page, a \
recipe card, a handwritten note, a screenshot, or the finished dish. Transcribe the recipe faithfully in its original \
language into the target schema: title, servings, times, every ingredient with raw_text (the original wording), \
metric_amount (number or null), unit (one of g, ml, pcs, to_taste), a snake_case English canonical_id and is_specialty, \
and numbered steps with timer_seconds for any mentioned durations. Never invent steps or quantities for parts you cannot \
read; list those in `unreadable`. Classify every photo by index with `kind`: recipe_text (only text), dish (only a photograph of the finished dish), both \
(text together with a photograph of the finished dish, e.g. the front of a recipe card), or unrelated. If no photo \
contains a recipe, set recipe to null and explain in the photo notes. Kitchen equipment (pans, graters, kettles, \
colanders) is never an ingredient; leave it out. If the page shows no title, compose a short descriptive one from the \
main ingredients in the page's language. Respond with JSON only, no prose.";

const PHOTO_PROMPT: &str = "Read these photos and return the recipe as JSON matching the schema.";

/// `rig-core` backed extractor using structured outputs.
#[derive(Clone)]
pub struct RigExtractor {
    model: DynModel<Completion>,
    reference: String,
}

impl std::fmt::Debug for RigExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RigExtractor")
            .field("reference", &self.reference)
            .finish()
    }
}

impl RigExtractor {
    /// Connect to `reference` (`vendor/model`); credentials come from the
    /// vendor's usual environment variable (e.g. `ANTHROPIC_API_KEY`).
    pub fn new(reference: &str) -> Result<Self, ScraperError> {
        Self::with_options(reference, connect_options())
    }

    /// Connect to `reference` through `base_url` (an LLM gateway, a test server)
    /// with `api_key`, reading nothing from the environment.
    pub fn connect(reference: &str, base_url: &str, api_key: &str) -> Result<Self, ScraperError> {
        Self::with_options(
            reference,
            ConnectOptions::new()
                .api_key(api_key.to_string())
                .base_url(base_url.to_string()),
        )
    }

    fn with_options(reference: &str, options: ConnectOptions) -> Result<Self, ScraperError> {
        let model = Catalog::builtin()
            .connect_with(reference, options)
            .map_err(|e| ScraperError::Llm(format!("cannot connect to {reference}: {e}")))?;
        Ok(Self {
            model,
            reference: reference.to_string(),
        })
    }

    /// Build from `LLM_MODEL`; `Ok(None)` when it is unset.
    pub fn from_env() -> Result<Option<Self>, ScraperError> {
        match std::env::var(LLM_MODEL_ENV) {
            Ok(m) if !m.trim().is_empty() => Self::new(m.trim()).map(Some),
            _ => Ok(None),
        }
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    async fn complete<T: JsonSchema>(
        &self,
        preamble: &str,
        prompt: String,
    ) -> Result<String, ScraperError> {
        self.complete_message::<T>(preamble, Message::user(prompt))
            .await
    }

    async fn complete_message<T: JsonSchema>(
        &self,
        preamble: &str,
        message: Message,
    ) -> Result<String, ScraperError> {
        let request = CompletionRequest::new(message)
            .preamble(preamble)
            .temperature(0.0)
            .output_schema(schema_for!(T));
        let response = self.model.call(request).await.map_err(|e| {
            classify_model_error(
                e.provider_response_status().map(|s| s.as_u16()),
                e.provider_response_json().ok().flatten(),
                e.provider_response_body().map(str::to_string),
                e.to_string(),
            )
        })?;
        let text = response.text();
        tracing::debug!(model = %self.reference, chars = text.len(), "llm reply received");
        tracing::trace!(reply = %text, "llm reply body");
        Ok(text)
    }
}

/// Turn a failed model call into a `ScraperError`. A 429 or 402 from the
/// endpoint (gateway quota, vendor rate limit, billing) becomes
/// [`ScraperError::ModelQuotaExceeded`] with a sentence naming the limit and the
/// endpoint's own message; anything else stays a generic [`ScraperError::Llm`].
pub fn classify_model_error(
    status: Option<u16>,
    body: Option<serde_json::Value>,
    raw_body: Option<String>,
    display: String,
) -> ScraperError {
    let code = match status {
        Some(code @ (429 | 402)) => code,
        _ => return ScraperError::Llm(display),
    };
    let field = |name: &str| {
        body.as_ref()
            .and_then(|b| b.pointer(&format!("/error/{name}")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let kind = field("type").unwrap_or_default();
    let detail = field("message")
        .or_else(|| raw_body.map(|b| b.trim().to_string()))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("HTTP {code}"));
    let what = match (code, kind.as_str()) {
        (402, _) => "the model account needs payment",
        (_, "quota_exceeded") => "the model quota for this node is used up",
        (_, "rate_limit_error") => "the model rate limit was reached, try again shortly",
        _ => "the model refused the request because of a usage limit",
    };
    ScraperError::ModelQuotaExceeded(format!("{what} ({detail})"))
}

/// Parse JSON from model text, tolerating code fences and surrounding prose.
pub fn parse_json_lenient<T: DeserializeOwned>(text: &str) -> Result<T, ScraperError> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<T>(trimmed) {
        return Ok(v);
    }
    let (open, close) = match (trimmed.find('{'), trimmed.find('[')) {
        (Some(o), Some(a)) if a < o => ('[', ']'),
        (None, Some(_)) => ('[', ']'),
        _ => ('{', '}'),
    };
    let snippet: String = trimmed.chars().take(200).collect();
    let start = trimmed
        .find(open)
        .ok_or_else(|| ScraperError::Llm(format!("no JSON in model output: {snippet}")))?;
    let end = trimmed
        .rfind(close)
        .ok_or_else(|| ScraperError::Llm("unterminated JSON in model output".to_string()))?;
    if end <= start {
        return Err(ScraperError::Llm(
            "malformed JSON in model output".to_string(),
        ));
    }
    serde_json::from_str::<T>(&trimmed[start..=end]).map_err(|e| {
        let snippet: String = trimmed.chars().take(400).collect();
        ScraperError::Llm(format!(
            "model output did not match the schema ({e}); output starts: {snippet}"
        ))
    })
}

impl RecipeExtractor for RigExtractor {
    fn detect_language<'a>(
        &'a self,
        sample: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ScraperError>> {
        Box::pin(async move {
            let sample: String = sample.chars().take(1200).collect();
            let reply = self
                .complete::<DetectedLanguage>(DETECT_PREAMBLE, sample)
                .await?;
            let value: serde_json::Value = parse_json_lenient(&reply)?;
            Ok(value
                .get("language")
                .and_then(|v| v.as_str())
                .and_then(recipe_core::normalize_lang))
        })
    }

    fn translate_terms<'a>(
        &'a self,
        ids: &'a [String],
        lang: &'a str,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, ScraperError>> {
        Box::pin(async move {
            let prompt = format!(
                "Target language: {lang}\nIds: {}",
                serde_json::to_string(ids)?
            );
            let reply = self.complete::<TermLabels>(TERMS_PREAMBLE, prompt).await?;
            parse_term_labels(&reply, ids)
        })
    }

    fn translate_recipe<'a>(
        &'a self,
        req: &'a TranslationRequest,
    ) -> BoxFuture<'a, Result<TranslationReply, ScraperError>> {
        Box::pin(async move {
            let prompt = serde_json::to_string_pretty(req)?;
            let reply = self
                .complete::<TranslationReply>(TRANSLATE_PREAMBLE, prompt)
                .await?;
            parse_translation_reply(&reply, req).inspect_err(|e| {
                let head: String = reply.chars().take(2000).collect();
                tracing::debug!(error = %e, reply_head = %head, "translation reply rejected");
            })
        })
    }

    fn supports_images(&self) -> bool {
        // Unknown to the catalog: let the provider decide.
        self.model.spec().map(|s| s.input.image).unwrap_or(true)
    }

    fn extract_from_photos<'a>(
        &'a self,
        photos: &'a [NormalizedPhoto],
    ) -> BoxFuture<'a, Result<PhotoExtraction, ScraperError>> {
        Box::pin(async move {
            if !self.supports_images() {
                return Err(ScraperError::ModelCannotSeeImages(format!(
                    "model `{}`",
                    self.reference
                )));
            }
            let mut content: Vec<UserContent> = photos
                .iter()
                .map(|p| {
                    UserContent::Image(Image {
                        data: DocumentSourceKind::Raw(p.bytes.clone()),
                        media_type: Some(ImageMediaType::JPEG),
                        ..Default::default()
                    })
                })
                .collect();
            content.push(UserContent::text(PHOTO_PROMPT));
            let reply = self
                .complete_message::<PhotoExtraction>(PHOTO_PREAMBLE, Message::User { content })
                .await?;
            let parsed = parse_photo_reply(&reply).inspect_err(|e| {
                let head: String = reply.chars().take(3000).collect();
                tracing::debug!(error = %e, reply_head = %head, "photo reply rejected");
            })?;
            if parsed.photos.len() != photos.len() || parsed.cover_index().is_none() {
                let raw_photos = parse_json_lenient::<serde_json::Value>(&reply)
                    .ok()
                    .and_then(|v| v.get("photos").cloned())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "(no photos array)".to_string());
                tracing::debug!(expected = photos.len(), got = parsed.photos.len(), raw_photos = %raw_photos, "photo classification yielded no cover");
            }
            Ok(parsed)
        })
    }

    fn normalize_ingredients<'a>(
        &'a self,
        raw: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<IngredientItem>, ScraperError>> {
        Box::pin(async move {
            if raw.is_empty() {
                return Ok(Vec::new());
            }
            let prompt = raw
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {r}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            let reply = self
                .complete::<NormalizedIngredients>(NORMALIZE_PREAMBLE, prompt)
                .await?;
            let items = parse_ingredients_reply(&reply).inspect_err(|e| {
                let head: String = reply.chars().take(3000).collect();
                tracing::debug!(error = %e, reply_head = %head, "ingredient reply rejected");
            })?;
            if items.len() != raw.len() {
                tracing::warn!(
                    expected = raw.len(),
                    got = items.len(),
                    "normalization returned a different number of ingredients"
                );
            }
            Ok(items)
        })
    }

    fn extract_recipe<'a>(
        &'a self,
        markdown: &'a str,
    ) -> BoxFuture<'a, Result<ExtractedRecipeDraft, ScraperError>> {
        Box::pin(async move {
            let reply = self
                .complete::<ExtractedRecipeDraft>(EXTRACT_PREAMBLE, markdown.to_string())
                .await?;
            parse_recipe_reply(&reply).inspect_err(|e| {
                let head: String = reply.chars().take(3000).collect();
                tracing::debug!(error = %e, reply_head = %head, "recipe reply rejected");
            })
        })
    }
}

/// The extractor to use for this process: rig when `LLM_MODEL` is set and
/// connects, otherwise the heuristic fallback (logged as a warning).
pub fn extractor_from_env() -> Arc<dyn RecipeExtractor> {
    match RigExtractor::from_env() {
        Ok(Some(rig)) => {
            tracing::info!(model = rig.reference(), "LLM extractor enabled");
            Arc::new(rig)
        }
        Ok(None) => {
            tracing::warn!(
                "{LLM_MODEL_ENV} not set; using heuristic ingredient normalization, free-text import disabled"
            );
            Arc::new(HeuristicExtractor)
        }
        Err(e) => {
            tracing::error!(error = %e, "LLM extractor failed to initialize; falling back to heuristics");
            Arc::new(HeuristicExtractor)
        }
    }
}
