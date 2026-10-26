//! SQLite pool initialization with the pragmas required by the project
//! guardrails, plus embedded migrations.

use std::path::Path;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::DbError;

/// All SQL migrations from the workspace `migrations/` directory, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Busy timeout applied to every connection (`PRAGMA busy_timeout = 5000`).
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Maximum pool size. SQLite allows many readers but a single writer; a
/// small pool keeps the idle memory footprint low.
const MAX_CONNECTIONS: u32 = 4;

/// Handle to the SQLite database. Cheap to clone (wraps an `Arc`'d pool).
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Open (creating if necessary) the SQLite database at `path`, enforce the
    /// required pragmas on every connection, and run pending migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
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

        let db = Self { pool };
        db.run_migrations().await?;
        tracing::info!(path = %path.as_ref().display(), "database ready");
        Ok(db)
    }

    /// Apply any migrations not yet recorded in `_sqlx_migrations`.
    pub async fn run_migrations(&self) -> Result<(), DbError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Access the underlying pool for queries.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Gracefully close all connections (checkpoints the WAL).
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
