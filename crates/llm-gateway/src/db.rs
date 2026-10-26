//! `gateway.db`: hubs (hashed tokens), per-day usage and model prices.
//! Same pragmas as the recipe node (WAL, NORMAL, foreign keys, busy timeout).

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use sqlx::Row;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::auth::{hash_token, new_token};
use crate::error::GatewayError;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const MAX_CONNECTIONS: u32 = 4;

#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// A registered hub as the admin API lists it (never the token or its hash).
#[derive(Debug, Clone, Serialize)]
pub struct Hub {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub status: String,
    pub created_at: String,
}

/// A freshly registered hub with its token, shown once.
#[derive(Debug, Clone, Serialize)]
pub struct NewHub {
    pub name: String,
    pub token: String,
    pub created_at: String,
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

    /// Register a hub and return its token (stored only as a hash).
    pub async fn create_hub(&self, name: &str) -> Result<NewHub, GatewayError> {
        if !valid_hub_name(name) {
            return Err(GatewayError::BadRequest(format!(
                "{name:?} is not a valid hub name (lowercase letters, digits, hyphens; 1-63 characters)"
            )));
        }
        let token = new_token();
        let created_at = now();
        let result = sqlx::query(
            "INSERT INTO hubs (name, token_hash, status, created_at) VALUES (?, ?, 'active', ?)",
        )
        .bind(name)
        .bind(hash_token(&token))
        .bind(&created_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(NewHub {
                name: name.to_string(),
                token,
                created_at,
            }),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(GatewayError::Conflict(format!("hub {name} already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list_hubs(&self) -> Result<Vec<Hub>, GatewayError> {
        let rows = sqlx::query("SELECT id, name, status, created_at FROM hubs ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| Hub {
                id: r.get("id"),
                name: r.get("name"),
                status: r.get("status"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    /// The active hub owning `token`, if any.
    pub async fn find_hub_by_token(&self, token: &str) -> Result<Option<Hub>, GatewayError> {
        let row = sqlx::query(
            "SELECT id, name, status, created_at FROM hubs WHERE token_hash = ? AND status = 'active'",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Hub {
            id: r.get("id"),
            name: r.get("name"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        }))
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
