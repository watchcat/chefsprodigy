//! `GET /api/v1/search`: FTS over every language row, best rank per recipe, one hit
//! per content hash (forks under `also_at`), wording in the requested language.

use recipe_core::normalize_lang;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::db::Db;
use crate::error::PlatformError;
use crate::index::recipe_url;

pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 50;
pub const MAX_OFFSET: u32 = 10_000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchParams {
    /// Free text over titles, tags and canonical ingredient ids in every language
    #[serde(default)]
    pub q: Option<String>,
    /// Canonical ingredient ids that must all be present (comma separated)
    #[serde(default)]
    pub ingredient: Option<String>,
    /// Preferred language for the returned title and tags
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchPage {
    /// Number of dishes (content-hash groups) matching
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
    /// The normalized `lang` parameter, if any
    pub lang: Option<String>,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchHit {
    pub hub: String,
    pub public_url: String,
    pub recipe_id: String,
    /// The recipe in the hub's web UI
    pub url: String,
    pub title: String,
    pub tags: Vec<String>,
    pub source_language: Option<String>,
    /// Language of `title` and `tags`
    pub matched_lang: Option<String>,
    pub image_url: Option<String>,
    /// Other copies of the same dish (same content hash)
    pub also_at: Vec<AlsoAt>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AlsoAt {
    pub hub: String,
    pub public_url: String,
    pub recipe_id: String,
    pub url: String,
}

/// Turn free text into a safe FTS5 expression: each alphanumeric token is quoted
/// and prefix-matched, tokens are implicitly AND-ed (as the hub's own search does).
/// `None` when the input has no searchable tokens.
pub fn fts_expression(raw: &str) -> Option<String> {
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(16)
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

/// `ingredient=garlic, Smoked Sausage` → `["garlic", "smoked_sausage"]`.
pub fn ingredient_ids(raw: &str) -> Vec<String> {
    let mut ids: Vec<String> = raw
        .split(',')
        .map(|part| {
            part.split(|c: char| c.is_whitespace() || c == '-' || c == '_')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join("_")
                .to_lowercase()
        })
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    ids.truncate(10);
    ids
}

/// The CTEs shared by the page and the count: `grouped` holds one candidate per
/// (hub, recipe) with its best rank and the language of its best-matching row, and
/// `leader = 1` marks the best candidate of each content-hash group.
fn push_grouped(qb: &mut QueryBuilder<Sqlite>, fts: Option<&str>, ingredients: &[String]) {
    qb.push("WITH ");
    match fts {
        Some(expr) => {
            qb.push(
                "m AS (SELECT hub_id, recipe_id, lang, rank FROM recipes_fts WHERE recipes_fts MATCH ",
            )
            .push_bind(expr.to_string())
            .push(
                "), best AS (SELECT hub_id, recipe_id, lang AS hit_lang, rank FROM (\
                 SELECT hub_id, recipe_id, lang, rank, \
                 ROW_NUMBER() OVER (PARTITION BY hub_id, recipe_id ORDER BY rank, lang) AS n FROM m) \
                 WHERE n = 1), ",
            );
        }
        None => {
            qb.push(
                "best AS (SELECT hub_id, recipe_id, NULL AS hit_lang, 0.0 AS rank FROM recipes_index), ",
            );
        }
    }
    qb.push(
        "cand AS (SELECT r.hub_id, r.recipe_id, r.content_hash, r.updated_at, b.rank, b.hit_lang, \
         h.name AS hub_name FROM best b \
         JOIN recipes_index r ON r.hub_id = b.hub_id AND r.recipe_id = b.recipe_id \
         JOIN hubs h ON h.id = r.hub_id WHERE h.status = 'active'",
    );
    for id in ingredients {
        qb.push(" AND EXISTS (SELECT 1 FROM json_each(r.canonical_json) WHERE json_each.value = ")
            .push_bind(id.clone())
            .push(")");
    }
    qb.push(
        "), grouped AS (SELECT *, ROW_NUMBER() OVER (PARTITION BY content_hash \
         ORDER BY rank, updated_at DESC, hub_name, recipe_id) AS leader FROM cand) ",
    );
}

fn tags(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

impl Db {
    pub async fn search(&self, params: &SearchParams) -> Result<SearchPage, PlatformError> {
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = params.offset.unwrap_or(0).min(MAX_OFFSET);
        let lang = params.lang.as_deref().and_then(normalize_lang);
        let fts = params.q.as_deref().and_then(fts_expression);
        let ingredients = params
            .ingredient
            .as_deref()
            .map(ingredient_ids)
            .unwrap_or_default();

        let mut count = QueryBuilder::<Sqlite>::new("");
        push_grouped(&mut count, fts.as_deref(), &ingredients);
        count.push("SELECT COUNT(*) FROM grouped WHERE leader = 1");
        let total: i64 = count.build_query_scalar().fetch_one(self.pool()).await?;

        let mut page = QueryBuilder::<Sqlite>::new("");
        push_grouped(&mut page, fts.as_deref(), &ingredients);
        page.push(
            "SELECT g.recipe_id, g.hub_name, h.public_url, r.title, r.tags_json, r.language, \
             r.image_url, r.content_hash, t.lang AS t_lang, t.title AS t_title, t.tags_json AS t_tags \
             FROM grouped g JOIN hubs h ON h.id = g.hub_id \
             JOIN recipes_index r ON r.hub_id = g.hub_id AND r.recipe_id = g.recipe_id \
             LEFT JOIN translations_index t ON t.hub_id = g.hub_id AND t.recipe_id = g.recipe_id \
             AND t.lang = COALESCE(",
        )
        .push_bind(lang.clone())
        .push(
            ", g.hit_lang) WHERE g.leader = 1 \
             ORDER BY g.rank, g.updated_at DESC, g.hub_name, g.recipe_id LIMIT ",
        )
        .push_bind(i64::from(limit))
        .push(" OFFSET ")
        .push_bind(i64::from(offset));
        let rows = page.build().fetch_all(self.pool()).await?;

        let mut hits: Vec<(String, SearchHit)> = rows
            .iter()
            .map(|row| {
                let public_url: String = row.get("public_url");
                let recipe_id: String = row.get("recipe_id");
                let translated: Option<String> = row.get("t_lang");
                let source_language: Option<String> = row.get("language");
                let (title, tag_json, matched_lang) = match translated {
                    Some(t_lang) => (
                        row.get::<String, _>("t_title"),
                        row.get::<String, _>("t_tags"),
                        Some(t_lang),
                    ),
                    None => (
                        row.get::<String, _>("title"),
                        row.get::<String, _>("tags_json"),
                        source_language.clone(),
                    ),
                };
                let hit = SearchHit {
                    hub: row.get("hub_name"),
                    url: recipe_url(&public_url, &recipe_id),
                    public_url,
                    recipe_id,
                    title,
                    tags: tags(&tag_json),
                    source_language,
                    matched_lang,
                    image_url: row.get("image_url"),
                    also_at: Vec::new(),
                };
                (row.get::<String, _>("content_hash"), hit)
            })
            .collect();
        self.attach_forks(&mut hits).await?;

        Ok(SearchPage {
            total,
            limit,
            offset,
            lang,
            hits: hits.into_iter().map(|(_, hit)| hit).collect(),
        })
    }

    /// Fill `also_at` with the other active copies of each hit's dish.
    async fn attach_forks(&self, hits: &mut [(String, SearchHit)]) -> Result<(), PlatformError> {
        if hits.is_empty() {
            return Ok(());
        }
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT r.content_hash, r.recipe_id, h.name, h.public_url FROM recipes_index r \
             JOIN hubs h ON h.id = r.hub_id WHERE h.status = 'active' AND r.content_hash IN (",
        );
        let mut hashes = qb.separated(", ");
        for (hash, _) in hits.iter() {
            hashes.push_bind(hash.clone());
        }
        qb.push(") ORDER BY h.name, r.recipe_id");
        let rows = qb.build().fetch_all(self.pool()).await?;
        for (hash, hit) in hits.iter_mut() {
            for row in &rows {
                let row_hash: String = row.get("content_hash");
                let hub: String = row.get("name");
                let recipe_id: String = row.get("recipe_id");
                if &row_hash != hash || (hub == hit.hub && recipe_id == hit.recipe_id) {
                    continue;
                }
                let public_url: String = row.get("public_url");
                hit.also_at.push(AlsoAt {
                    url: recipe_url(&public_url, &recipe_id),
                    hub,
                    public_url,
                    recipe_id,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_expressions_quote_every_token() {
        assert_eq!(
            fts_expression("pea soup").as_deref(),
            Some("\"pea\"* \"soup\"*")
        );
        assert_eq!(
            fts_expression("гороховый-суп").as_deref(),
            Some("\"гороховый\"* \"суп\"*")
        );
        assert_eq!(fts_expression("\"*)( OR"), Some("\"OR\"*".to_string()));
        assert_eq!(fts_expression("  ,; "), None);
    }

    #[test]
    fn ingredient_ids_look_like_canonical_ids() {
        assert_eq!(
            ingredient_ids("garlic, Smoked Sausage,,olive-oil"),
            ["garlic", "olive_oil", "smoked_sausage"]
        );
        assert!(ingredient_ids(" , ").is_empty());
    }
}
