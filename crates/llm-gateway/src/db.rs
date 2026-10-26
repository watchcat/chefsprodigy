//! `gateway.db`: hubs (hashed tokens), per-day usage and model prices.
//! Same pragmas as the recipe node (WAL, NORMAL, foreign keys, busy timeout).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::auth::{hash_token, new_token};
use crate::error::GatewayError;
use crate::quota::UsageTotals;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const MAX_CONNECTIONS: u32 = 4;

#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// Per-hub quota columns (free tier by default, docs/SPEC-hosted-platform.md section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub onboarding_daily_requests: i64,
    pub onboarding_days: i64,
    pub daily_request_limit: i64,
    /// Secondary cap on input + output tokens per day; `None` means no cap
    pub daily_token_limit: Option<i64>,
    pub monthly_request_limit: i64,
    pub monthly_cost_cap_cents: i64,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            onboarding_daily_requests: 30,
            onboarding_days: 3,
            daily_request_limit: 10,
            daily_token_limit: None,
            monthly_request_limit: 150,
            monthly_cost_cap_cents: 100,
        }
    }
}

/// Partial quota update: absent fields keep their value; `daily_token_limit: 0`
/// removes that cap.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotaPatch {
    pub onboarding_daily_requests: Option<i64>,
    pub onboarding_days: Option<i64>,
    pub daily_request_limit: Option<i64>,
    pub daily_token_limit: Option<i64>,
    pub monthly_request_limit: Option<i64>,
    pub monthly_cost_cap_cents: Option<i64>,
}

impl QuotaPatch {
    pub fn apply(&self, quota: &mut Quota) {
        if let Some(v) = self.onboarding_daily_requests {
            quota.onboarding_daily_requests = v;
        }
        if let Some(v) = self.onboarding_days {
            quota.onboarding_days = v;
        }
        if let Some(v) = self.daily_request_limit {
            quota.daily_request_limit = v;
        }
        if let Some(v) = self.daily_token_limit {
            quota.daily_token_limit = (v > 0).then_some(v);
        }
        if let Some(v) = self.monthly_request_limit {
            quota.monthly_request_limit = v;
        }
        if let Some(v) = self.monthly_cost_cap_cents {
            quota.monthly_cost_cap_cents = v;
        }
    }
}

/// A registered hub (never the token or its hash; the BYOK blob stays encrypted).
#[derive(Debug, Clone, Serialize)]
pub struct Hub {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub quota: Quota,
    #[serde(skip)]
    pub byok_json_enc: Option<Vec<u8>>,
}

/// A freshly registered hub with its token, shown once.
#[derive(Debug, Clone, Serialize)]
pub struct NewHub {
    pub name: String,
    pub token: String,
    pub created_at: String,
    pub quota: Quota,
}

/// A cached vendor reply.
#[derive(Debug, Clone)]
pub struct CachedReply {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CacheStats {
    pub entries: i64,
    pub bytes: i64,
    pub hits: i64,
}

const HUB_COLUMNS: &str = "id, name, status, created_at, onboarding_daily_requests, onboarding_days, \
    daily_request_limit, daily_token_limit, monthly_request_limit, monthly_cost_cap_cents, byok_json_enc";

fn hub_from_row(r: &SqliteRow) -> Hub {
    Hub {
        id: r.get("id"),
        name: r.get("name"),
        status: r.get("status"),
        created_at: r.get("created_at"),
        quota: Quota {
            onboarding_daily_requests: r.get("onboarding_daily_requests"),
            onboarding_days: r.get("onboarding_days"),
            daily_request_limit: r.get("daily_request_limit"),
            daily_token_limit: r.get("daily_token_limit"),
            monthly_request_limit: r.get("monthly_request_limit"),
            monthly_cost_cap_cents: r.get("monthly_cost_cap_cents"),
        },
        byok_json_enc: r.get("byok_json_enc"),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageRow {
    pub hub: String,
    pub day: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: f64,
}

/// Hub names are subdomain labels: lowercase letters, digits and hyphens.
pub fn valid_hub_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn rfc3339(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Db {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, GatewayError> {
        let options = SqliteConnectOptions::new()
            .filename(path.as_ref())
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        tracing::info!(path = %path.as_ref().display(), "gateway database ready");
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Register a hub with the free-tier quota; returns its token (stored only as a hash).
    pub async fn create_hub(&self, name: &str) -> Result<NewHub, GatewayError> {
        self.create_hub_with(name, Quota::default()).await
    }

    pub async fn create_hub_with(&self, name: &str, quota: Quota) -> Result<NewHub, GatewayError> {
        if !valid_hub_name(name) {
            return Err(GatewayError::BadRequest(format!(
                "{name:?} is not a valid hub name (lowercase letters, digits, hyphens; 1-63 characters)"
            )));
        }
        let token = new_token();
        let created_at = now();
        let result = sqlx::query(
            "INSERT INTO hubs (name, token_hash, status, created_at, onboarding_daily_requests, onboarding_days, \
             daily_request_limit, daily_token_limit, monthly_request_limit, monthly_cost_cap_cents) \
             VALUES (?, ?, 'active', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(name)
        .bind(hash_token(&token))
        .bind(&created_at)
        .bind(quota.onboarding_daily_requests)
        .bind(quota.onboarding_days)
        .bind(quota.daily_request_limit)
        .bind(quota.daily_token_limit)
        .bind(quota.monthly_request_limit)
        .bind(quota.monthly_cost_cap_cents)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(NewHub {
                name: name.to_string(),
                token,
                created_at,
                quota,
            }),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(GatewayError::Conflict(format!("hub {name} already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list_hubs(&self) -> Result<Vec<Hub>, GatewayError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs ORDER BY name"
        )))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(hub_from_row).collect())
    }

    pub async fn hub_by_name(&self, name: &str) -> Result<Option<Hub>, GatewayError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs WHERE name = ?"
        )))
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(hub_from_row))
    }

    /// The active hub owning `token`, if any.
    pub async fn find_hub_by_token(&self, token: &str) -> Result<Option<Hub>, GatewayError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs WHERE token_hash = ? AND status = 'active'"
        )))
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(hub_from_row))
    }

    /// Apply a quota patch and/or a status change; `None` when the hub does not exist.
    pub async fn update_hub(
        &self,
        name: &str,
        patch: &QuotaPatch,
        status: Option<&str>,
    ) -> Result<Option<Hub>, GatewayError> {
        let Some(mut hub) = self.hub_by_name(name).await? else {
            return Ok(None);
        };
        patch.apply(&mut hub.quota);
        if let Some(status) = status {
            hub.status = status.to_string();
        }
        sqlx::query(
            "UPDATE hubs SET status = ?, onboarding_daily_requests = ?, onboarding_days = ?, \
             daily_request_limit = ?, daily_token_limit = ?, monthly_request_limit = ?, \
             monthly_cost_cap_cents = ? WHERE id = ?",
        )
        .bind(&hub.status)
        .bind(hub.quota.onboarding_daily_requests)
        .bind(hub.quota.onboarding_days)
        .bind(hub.quota.daily_request_limit)
        .bind(hub.quota.daily_token_limit)
        .bind(hub.quota.monthly_request_limit)
        .bind(hub.quota.monthly_cost_cap_cents)
        .bind(hub.id)
        .execute(&self.pool)
        .await?;
        Ok(Some(hub))
    }

    /// Remove a hub and, through the foreign key, its usage rows.
    pub async fn delete_hub(&self, name: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM hubs WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Store (or clear, with `None`) a hub's encrypted BYOK map.
    pub async fn set_byok_blob(
        &self,
        hub_id: i64,
        blob: Option<&[u8]>,
    ) -> Result<(), GatewayError> {
        sqlx::query("UPDATE hubs SET byok_json_enc = ? WHERE id = ?")
            .bind(blob)
            .bind(hub_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Usage summed over the days matching `day_pattern` (`YYYY-MM-DD` or `YYYY-MM-%`).
    pub async fn usage_sum(
        &self,
        hub_id: i64,
        day_pattern: &str,
    ) -> Result<UsageTotals, GatewayError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(requests), 0) AS requests, COALESCE(SUM(input_tokens), 0) AS input_tokens, \
             COALESCE(SUM(output_tokens), 0) AS output_tokens, COALESCE(SUM(cost_cents), 0.0) AS cost_cents \
             FROM usage_daily WHERE hub_id = ? AND day LIKE ?",
        )
        .bind(hub_id)
        .bind(day_pattern)
        .fetch_one(&self.pool)
        .await?;
        Ok(UsageTotals {
            requests: row.get("requests"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cost_cents: row.get("cost_cents"),
        })
    }

    /// A cached reply younger than `ttl`; expired entries are dropped on the way.
    pub async fn cache_get(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<CachedReply>, GatewayError> {
        let cutoff = rfc3339(
            chrono::Utc::now()
                - chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::days(30)),
        );
        sqlx::query("DELETE FROM cache WHERE key_hash = ? AND created_at <= ?")
            .bind(key)
            .bind(&cutoff)
            .execute(&self.pool)
            .await?;
        let row = sqlx::query("SELECT status, body FROM cache WHERE key_hash = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| CachedReply {
            status: r.get::<i64, _>("status") as u16,
            body: r.get("body"),
        }))
    }

    pub async fn cache_touch(&self, key: &str) -> Result<(), GatewayError> {
        sqlx::query("UPDATE cache SET hits = hits + 1, last_hit = ? WHERE key_hash = ?")
            .bind(now())
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn cache_put(
        &self,
        key: &str,
        vendor: &str,
        model: &str,
        status: u16,
        body: &[u8],
    ) -> Result<(), GatewayError> {
        let stamp = now();
        sqlx::query(
            "INSERT OR REPLACE INTO cache (key_hash, vendor, model, status, body, created_at, last_hit, hits) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(key)
        .bind(vendor)
        .bind(model)
        .bind(i64::from(status))
        .bind(body)
        .bind(&stamp)
        .bind(&stamp)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Keep the `max_entries` most recently hit entries.
    pub async fn cache_evict(&self, max_entries: usize) -> Result<(), GatewayError> {
        sqlx::query(
            "DELETE FROM cache WHERE key_hash IN (\
                 SELECT key_hash FROM cache ORDER BY last_hit DESC, created_at DESC LIMIT -1 OFFSET ?)",
        )
        .bind(max_entries as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cache_stats(&self) -> Result<CacheStats, GatewayError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS entries, COALESCE(SUM(LENGTH(body)), 0) AS bytes, COALESCE(SUM(hits), 0) AS hits FROM cache",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(CacheStats {
            entries: row.get("entries"),
            bytes: row.get("bytes"),
            hits: row.get("hits"),
        })
    }

    /// Add one successful request to the hub's row for `day` (UTC, `YYYY-MM-DD`).
    pub async fn record_usage(
        &self,
        hub_id: i64,
        day: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost_cents: f64,
    ) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO usage_daily (hub_id, day, requests, input_tokens, output_tokens, cost_cents) \
             VALUES (?, ?, 1, ?, ?, ?) \
             ON CONFLICT(hub_id, day) DO UPDATE SET requests = requests + 1, \
             input_tokens = input_tokens + excluded.input_tokens, \
             output_tokens = output_tokens + excluded.output_tokens, \
             cost_cents = cost_cents + excluded.cost_cents",
        )
        .bind(hub_id)
        .bind(day)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(cost_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Usage rows, newest day first, optionally for one hub and a day range (inclusive).
    pub async fn usage(
        &self,
        hub: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<UsageRow>, GatewayError> {
        let rows = sqlx::query(
            "SELECT h.name, u.day, u.requests, u.input_tokens, u.output_tokens, u.cost_cents \
             FROM usage_daily u JOIN hubs h ON h.id = u.hub_id \
             WHERE (?1 IS NULL OR h.name = ?1) AND (?2 IS NULL OR u.day >= ?2) AND (?3 IS NULL OR u.day <= ?3) \
             ORDER BY u.day DESC, h.name",
        )
        .bind(hub)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| UsageRow {
                hub: r.get("name"),
                day: r.get("day"),
                requests: r.get("requests"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                cost_cents: r.get("cost_cents"),
            })
            .collect())
    }

    /// `(input, output)` cents per million tokens, when a price is configured.
    pub async fn price_for(
        &self,
        vendor: &str,
        model: &str,
    ) -> Result<Option<(f64, f64)>, GatewayError> {
        let row = sqlx::query(
            "SELECT input_cents_per_mtok, output_cents_per_mtok FROM model_prices WHERE vendor = ? AND model = ?",
        )
        .bind(vendor)
        .bind(model)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get(0), r.get(1))))
    }
}
