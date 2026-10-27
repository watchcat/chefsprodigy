//! Recipe repository: CRUD, FTS5 search and federation queries.
//!
//! Every write recomputes `content_hash` through `recipe-core` so the stored
//! value is always consistent with the persisted ingredients and steps.

use chrono::{SecondsFormat, Utc};
use recipe_core::{
    IngredientItem, InstructionStep, ManifestEntry, ManifestTranslation, Recipe, RecipeInput,
    RecipeSummary, SearchQuery, new_recipe_id,
};
use sqlx::sqlite::Sqlite;
use sqlx::{FromRow, QueryBuilder, SqliteConnection};

use crate::{Db, DbError};

/// Default page size for searches.
pub const DEFAULT_LIMIT: u32 = 20;
/// Hard cap on page size to protect the node's memory budget.
pub const MAX_LIMIT: u32 = 100;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}

#[derive(FromRow)]
struct RecipeRow {
    id: String,
    title: String,
    description: Option<String>,
    servings: i64,
    prep_time_minutes: Option<i64>,
    cook_time_minutes: Option<i64>,
    instructions: String,
    tags: String,
    image_path: Option<String>,
    created_at: String,
    updated_at: String,
    origin_url: Option<String>,
    author_did: Option<String>,
    content_hash: String,
    is_public: bool,
    language: Option<String>,
}

#[derive(FromRow)]
struct IngredientRow {
    raw_text: String,
    metric_amount: Option<f64>,
    unit: String,
    canonical_id: String,
    is_specialty: bool,
}

#[derive(FromRow)]
struct SummaryRow {
    id: String,
    title: String,
    servings: i64,
    prep_time_minutes: Option<i64>,
    cook_time_minutes: Option<i64>,
    tags: String,
    image_path: Option<String>,
    canonical_ingredients: Option<String>,
    language: Option<String>,
    translation_lang: Option<String>,
}

fn to_u8(field: &str, v: i64) -> Result<u8, DbError> {
    u8::try_from(v).map_err(|_| DbError::InvalidData(format!("{field} out of range: {v}")))
}

fn to_opt_u16(field: &str, v: Option<i64>) -> Result<Option<u16>, DbError> {
    v.map(|v| {
        u16::try_from(v).map_err(|_| DbError::InvalidData(format!("{field} out of range: {v}")))
    })
    .transpose()
}

impl TryFrom<IngredientRow> for IngredientItem {
    type Error = DbError;

    fn try_from(r: IngredientRow) -> Result<Self, DbError> {
        Ok(Self {
            raw_text: r.raw_text,
            metric_amount: r.metric_amount.map(|a| a as f32),
            unit: r.unit.parse()?,
            canonical_id: r.canonical_id,
            is_specialty: r.is_specialty,
            canonical_label: None,
        })
    }
}

impl RecipeRow {
    fn into_recipe(self, ingredients: Vec<IngredientItem>) -> Result<Recipe, DbError> {
        let instructions: Vec<InstructionStep> = serde_json::from_str(&self.instructions)?;
        let tags: Vec<String> = serde_json::from_str(&self.tags)?;
        Ok(Recipe {
            id: self.id,
            title: self.title,
            description: self.description,
            servings: to_u8("servings", self.servings)?,
            prep_time_minutes: to_opt_u16("prep_time_minutes", self.prep_time_minutes)?,
            cook_time_minutes: to_opt_u16("cook_time_minutes", self.cook_time_minutes)?,
            ingredients,
            instructions,
            tags,
            image_path: self.image_path,
            origin_url: self.origin_url,
            author_did: self.author_did,
            content_hash: self.content_hash,
            is_public: self.is_public,
            created_at: self.created_at,
            updated_at: self.updated_at,
            language: self.language,
        })
    }
}

impl TryFrom<SummaryRow> for RecipeSummary {
    type Error = DbError;

    fn try_from(r: SummaryRow) -> Result<Self, DbError> {
        let prep = to_opt_u16("prep_time_minutes", r.prep_time_minutes)?;
        let cook = to_opt_u16("cook_time_minutes", r.cook_time_minutes)?;
        let total_time_minutes = match (prep, cook) {
            (None, None) => None,
            (p, c) => Some(p.unwrap_or(0).saturating_add(c.unwrap_or(0))),
        };
        Ok(Self {
            id: r.id,
            title: r.title,
            servings: to_u8("servings", r.servings)?,
            total_time_minutes,
            tags: serde_json::from_str(&r.tags)?,
            image_path: r.image_path,
            canonical_ingredients: r
                .canonical_ingredients
                .map(|s| s.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            language: r.language,
            translation_lang: r.translation_lang,
        })
    }
}

/// Turn free text into a safe FTS5 expression: each alphanumeric token is
/// quoted and prefix-matched, tokens are implicitly AND-ed. Returns `None`
/// when the input has no searchable tokens.
pub fn fts_expression(raw: &str) -> Option<String> {
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

async fn insert_ingredients(
    conn: &mut SqliteConnection,
    recipe_id: &str,
    ingredients: &[IngredientItem],
) -> Result<(), DbError> {
    for (position, item) in ingredients.iter().enumerate() {
        sqlx::query(
            "INSERT INTO recipe_ingredients (recipe_id, raw_text, metric_amount, unit, canonical_id, is_specialty, position) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(recipe_id)
        .bind(&item.raw_text)
        .bind(item.metric_amount.map(f64::from))
        .bind(item.unit.as_str())
        .bind(&item.canonical_id)
        .bind(item.is_specialty)
        .bind(position as i64)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn load_ingredients(
    conn: &mut SqliteConnection,
    recipe_id: &str,
) -> Result<Vec<IngredientItem>, DbError> {
    let rows: Vec<IngredientRow> = sqlx::query_as(
        "SELECT raw_text, metric_amount, unit, canonical_id, is_specialty \
         FROM recipe_ingredients WHERE recipe_id = ? ORDER BY position, id",
    )
    .bind(recipe_id)
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(IngredientItem::try_from).collect()
}

async fn load_recipe(conn: &mut SqliteConnection, id: &str) -> Result<Option<Recipe>, DbError> {
    let row: Option<RecipeRow> = sqlx::query_as(
        "SELECT id, title, description, servings, prep_time_minutes, cook_time_minutes, instructions, tags, image_path, created_at, updated_at, origin_url, author_did, content_hash, is_public, language FROM recipes WHERE id = ?",
    )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let ingredients = load_ingredients(conn, &row.id).await?;
            row.into_recipe(ingredients).map(Some)
        }
    }
}

/// Append ` FROM recipes r [JOIN recipes_fts] WHERE <filters>` for `query`.
/// Shared by search and count so the two can never disagree.
fn push_from_and_filters(qb: &mut QueryBuilder<Sqlite>, query: &SearchQuery, fts: Option<&str>) {
    qb.push(" FROM recipes r");
    if let Some(expr) = fts {
        // Match the source text and any translation; one row per recipe with its best rank.
        qb.push(
            " JOIN (SELECT recipe_id, MIN(rank) AS rank FROM (\
                SELECT recipe_id, rank FROM recipes_fts WHERE recipes_fts MATCH ",
        )
        .push_bind(expr.to_string())
        .push(
            " UNION ALL SELECT recipe_id, rank FROM translations_fts WHERE translations_fts MATCH ",
        )
        .push_bind(expr.to_string())
        .push(") GROUP BY recipe_id) hits ON hits.recipe_id = r.id");
    }
    if let Some(lang) = query.lang.as_deref().and_then(recipe_core::normalize_lang) {
        qb.push(" LEFT JOIN recipe_translations tr ON tr.recipe_id = r.id AND tr.lang = ")
            .push_bind(lang);
    }
    qb.push(" WHERE 1 = 1");
    if query.public_only {
        qb.push(" AND r.is_public = 1");
    }
    if let Some(tag) = &query.tag {
        qb.push(" AND EXISTS (SELECT 1 FROM json_each(r.tags) WHERE json_each.value = ")
            .push_bind(tag.clone())
            .push(")");
    }
    if !query.exclude_canonical_ingredients.is_empty() {
        qb.push(" AND NOT EXISTS (SELECT 1 FROM recipe_ingredients ri WHERE ri.recipe_id = r.id AND ri.canonical_id IN (");
        let mut sep = qb.separated(", ");
        for c in &query.exclude_canonical_ingredients {
            sep.push_bind(c.clone());
        }
        qb.push("))");
    }
    if !query.include_canonical_ingredients.is_empty() {
        qb.push(" AND EXISTS (SELECT 1 FROM recipe_ingredients ri WHERE ri.recipe_id = r.id AND ri.canonical_id IN (");
        let mut sep = qb.separated(", ");
        for c in &query.include_canonical_ingredients {
            sep.push_bind(c.clone());
        }
        qb.push("))");
    }
    if let Some(max) = query.max_total_time_minutes {
        qb.push(" AND (COALESCE(r.prep_time_minutes, 0) + COALESCE(r.cook_time_minutes, 0)) <= ")
            .push_bind(i64::from(max));
    }
}

impl Db {
    /// Persist a new recipe, assigning a ULID and computing `content_hash`.
    pub async fn create_recipe(&self, input: RecipeInput) -> Result<Recipe, DbError> {
        let recipe = Recipe::from_input(new_recipe_id(), input, now_rfc3339());
        let instructions = serde_json::to_string(&recipe.instructions)?;
        let tags = serde_json::to_string(&recipe.tags)?;

        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO recipes (id, title, description, servings, prep_time_minutes, cook_time_minutes, instructions, tags, image_path, created_at, updated_at, origin_url, author_did, content_hash, is_public, language) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&recipe.id)
        .bind(&recipe.title)
        .bind(&recipe.description)
        .bind(i64::from(recipe.servings))
        .bind(recipe.prep_time_minutes.map(i64::from))
        .bind(recipe.cook_time_minutes.map(i64::from))
        .bind(&instructions)
        .bind(&tags)
        .bind(&recipe.image_path)
        .bind(&recipe.created_at)
        .bind(&recipe.updated_at)
        .bind(&recipe.origin_url)
        .bind(&recipe.author_did)
        .bind(&recipe.content_hash)
        .bind(recipe.is_public)
        .bind(&recipe.language)
        .execute(&mut *tx)
        .await?;
        insert_ingredients(&mut tx, &recipe.id, &recipe.ingredients).await?;
        tx.commit().await?;
        Ok(recipe)
    }

    /// Fetch a recipe by id.
    pub async fn get_recipe(&self, id: &str) -> Result<Option<Recipe>, DbError> {
        let mut conn = self.pool().acquire().await?;
        load_recipe(&mut conn, id).await
    }

    /// Fetch a recipe only if it is public (federation endpoint).
    pub async fn get_public_recipe(&self, id: &str) -> Result<Option<Recipe>, DbError> {
        Ok(self.get_recipe(id).await?.filter(|r| r.is_public))
    }

    /// Replace all mutable fields of an existing recipe. Returns `None` if it does not exist.
    pub async fn update_recipe(
        &self,
        id: &str,
        input: RecipeInput,
    ) -> Result<Option<Recipe>, DbError> {
        let mut tx = self.pool().begin().await?;
        let Some(mut recipe) = load_recipe(&mut tx, id).await? else {
            return Ok(None);
        };
        recipe.apply_input(input, now_rfc3339());
        let instructions = serde_json::to_string(&recipe.instructions)?;
        let tags = serde_json::to_string(&recipe.tags)?;

        sqlx::query(
            "UPDATE recipes SET title = ?, description = ?, servings = ?, prep_time_minutes = ?, cook_time_minutes = ?, \
             instructions = ?, tags = ?, image_path = ?, updated_at = ?, origin_url = ?, author_did = ?, \
             content_hash = ?, is_public = ?, language = ? WHERE id = ?",
        )
        .bind(&recipe.title)
        .bind(&recipe.description)
        .bind(i64::from(recipe.servings))
        .bind(recipe.prep_time_minutes.map(i64::from))
        .bind(recipe.cook_time_minutes.map(i64::from))
        .bind(&instructions)
        .bind(&tags)
        .bind(&recipe.image_path)
        .bind(&recipe.updated_at)
        .bind(&recipe.origin_url)
        .bind(&recipe.author_did)
        .bind(&recipe.content_hash)
        .bind(recipe.is_public)
        .bind(&recipe.language)
        .bind(&recipe.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM recipe_ingredients WHERE recipe_id = ?")
            .bind(&recipe.id)
            .execute(&mut *tx)
            .await?;
        insert_ingredients(&mut tx, &recipe.id, &recipe.ingredients).await?;
        tx.commit().await?;
        Ok(Some(recipe))
    }

    /// Delete a recipe (ingredients cascade). Returns whether a row was removed.
    pub async fn delete_recipe(&self, id: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM recipes WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Search recipes using FTS5 plus structured filters. Results are ordered by
    /// pantry overlap (when given), then FTS rank (when a query is given), then
    /// most recently updated.
    pub async fn search_recipes(&self, query: &SearchQuery) -> Result<Vec<RecipeSummary>, DbError> {
        let fts = query.query.as_deref().and_then(fts_expression);
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = query.offset.unwrap_or(0);

        let lang = query.lang.as_deref().and_then(recipe_core::normalize_lang);
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT r.id, ");
        if lang.is_some() {
            qb.push("COALESCE(tr.title, r.title) AS title, ");
        } else {
            qb.push("r.title, ");
        }
        qb.push("r.servings, r.prep_time_minutes, r.cook_time_minutes, ");
        if lang.is_some() {
            qb.push("COALESCE(tr.tags, r.tags) AS tags, ");
        } else {
            qb.push("r.tags, ");
        }
        qb.push(
            "r.image_path, r.language, \
             (SELECT GROUP_CONCAT(canonical_id, ',') FROM (SELECT canonical_id FROM recipe_ingredients \
              WHERE recipe_id = r.id ORDER BY position, id)) AS canonical_ingredients",
        );
        if lang.is_some() {
            qb.push(", tr.lang AS translation_lang");
        } else {
            qb.push(", NULL AS translation_lang");
        }
        if query.include_canonical_ingredients.is_empty() {
            qb.push(", 0 AS pantry_matches");
        } else {
            qb.push(
                ", (SELECT COUNT(DISTINCT canonical_id) FROM recipe_ingredients ri \
                 WHERE ri.recipe_id = r.id AND ri.canonical_id IN (",
            );
            let mut sep = qb.separated(", ");
            for c in &query.include_canonical_ingredients {
                sep.push_bind(c);
            }
            qb.push(")) AS pantry_matches");
        }
        push_from_and_filters(&mut qb, query, fts.as_deref());

        qb.push(" ORDER BY pantry_matches DESC");
        if fts.is_some() {
            qb.push(", hits.rank");
        }
        qb.push(", r.updated_at DESC, r.rowid DESC LIMIT ")
            .push_bind(i64::from(limit))
            .push(" OFFSET ")
            .push_bind(i64::from(offset));

        let rows: Vec<SummaryRow> = qb.build_query_as().fetch_all(self.pool()).await?;
        rows.into_iter().map(RecipeSummary::try_from).collect()
    }

    /// Number of recipes matching `query`'s filters, ignoring `limit`/`offset`.
    pub async fn count_recipes(&self, query: &SearchQuery) -> Result<u64, DbError> {
        let fts = query.query.as_deref().and_then(fts_expression);
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*)");
        push_from_and_filters(&mut qb, query, fts.as_deref());
        let count: i64 = qb.build_query_scalar().fetch_one(self.pool()).await?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Public recipes with their hashes and translation stamps, for Central
    /// Aggregator delta sync. A translation is stale when its `source_updated_at`
    /// no longer matches the recipe's `updated_at`.
    pub async fn public_manifest(&self) -> Result<Vec<ManifestEntry>, DbError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, updated_at, content_hash FROM recipes WHERE is_public = 1 ORDER BY updated_at DESC, rowid DESC",
        )
        .fetch_all(self.pool())
        .await?;
        let stamps: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT t.recipe_id, t.lang, t.origin, t.source_updated_at, t.updated_at \
             FROM recipe_translations t JOIN recipes r ON r.id = t.recipe_id \
             WHERE r.is_public = 1 ORDER BY t.recipe_id, t.lang",
        )
        .fetch_all(self.pool())
        .await?;
        let mut by_recipe: std::collections::HashMap<
            String,
            Vec<(String, String, String, String)>,
        > = std::collections::HashMap::new();
        for (recipe_id, lang, origin, source_updated_at, updated_at) in stamps {
            by_recipe.entry(recipe_id).or_default().push((
                lang,
                origin,
                source_updated_at,
                updated_at,
            ));
        }
        rows.into_iter()
            .map(|(id, updated_at, content_hash)| {
                let translations = by_recipe
                    .remove(&id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(lang, origin, source_updated_at, translated_at)| {
                        Ok(ManifestTranslation {
                            lang,
                            origin: origin.parse()?,
                            stale: source_updated_at != updated_at,
                            updated_at: translated_at,
                        })
                    })
                    .collect::<Result<Vec<_>, DbError>>()?;
                Ok(ManifestEntry {
                    id,
                    updated_at,
                    content_hash,
                    translations,
                })
            })
            .collect()
    }
}
