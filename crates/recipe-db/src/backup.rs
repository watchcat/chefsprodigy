//! Consistent database snapshots for backup and restore.
//!
//! A backup needs two things the live database cannot give directly: a copy
//! that is consistent even while the node is serving, and a way to put that
//! copy back. `VACUUM INTO` produces the first (it writes a fresh, compacted
//! database from a read transaction), and attaching the snapshot to the target
//! and copying rows produces the second.
//!
//! Both operations work on SQLite's file format rather than through the
//! recipe repository, so a restored node keeps the ids and content hashes it
//! was backed up with, which is what a backup is for.

use std::path::Path;

use sqlx::{Acquire, AssertSqlSafe, Sqlite, Transaction};

use crate::{Db, DbError};

/// Name the snapshot database is attached under while restoring.
const ATTACH_NAME: &str = "restore_source";

/// Tables copied on restore, in an order that satisfies the foreign keys
/// (`recipe_ingredients.recipe_id` and `recipe_translations.recipe_id` both
/// reference `recipes.id`).
///
/// The FTS tables are deliberately absent: their rows are rebuilt by the
/// insert triggers on `recipes` and `recipe_ingredients`, so copying the
/// base tables keeps the search index in step without copying it.
const RESTORED_TABLES: &[&str] = &["recipes", "recipe_ingredients", "recipe_translations"];

/// What a restore did, for the API to report back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Recipes now in the database.
    pub recipes: u64,
    /// Ingredient rows now in the database.
    pub ingredients: u64,
    /// Translation rows now in the database.
    pub translations: u64,
}

impl Db {
    /// Write a consistent snapshot of this database to `dest`.
    ///
    /// `VACUUM INTO` runs in its own read transaction, so the copy is
    /// consistent even while other connections are writing. It refuses to
    /// overwrite an existing file, which keeps a stale snapshot from being
    /// mistaken for a fresh one, so the destination must not exist.
    pub async fn snapshot_to(&self, dest: &Path) -> Result<(), DbError> {
        if dest.exists() {
            return Err(DbError::InvalidData(format!(
                "snapshot target {} already exists",
                dest.display()
            )));
        }
        let sql = format!(
            "VACUUM INTO '{}'",
            dest.to_string_lossy().replace('\'', "''")
        );
        // VACUUM cannot run inside a transaction, so it must not borrow one.
        let mut conn = self.pool().acquire().await?;
        sqlx::query(AssertSqlSafe(sql)).execute(&mut *conn).await?;
        Ok(())
    }

    /// Restore the database in `source` into this one.
    ///
    /// `source` is a database file produced by [`Db::snapshot_to`]. It is
    /// attached to this connection and its rows are copied into the
    /// corresponding tables, keeping the ids and content hashes the snapshot
    /// was taken with.
    ///
    /// The target must be empty: restoring over existing recipes would mix
    /// two sets of rows and leave the ids ambiguous. An empty target also
    /// means no row can collide with an existing one, so the copy cannot
    /// half-fail on a duplicate key.
    pub async fn restore_from(&self, source: &Path) -> Result<RestoreReport, DbError> {
        if !source.exists() {
            return Err(DbError::InvalidData(format!(
                "restore source {} does not exist",
                source.display()
            )));
        }
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recipes")
            .fetch_one(self.pool())
            .await?;
        if existing > 0 {
            return Err(DbError::Refused(
                "restore needs an empty node: this one already holds recipes".to_string(),
            ));
        }

        // ATTACH is per connection, so the attach, the copy and the detach all
        // have to happen on the same one.
        let mut conn = self.pool().acquire().await?;
        let attach = format!(
            "ATTACH DATABASE '{}' AS {ATTACH_NAME}",
            source.to_string_lossy().replace('\'', "''")
        );
        sqlx::query(AssertSqlSafe(attach))
            .execute(&mut *conn)
            .await?;

        let result = copy_rows(&mut conn).await;

        // DETACH must run even after a failed copy, or the connection keeps the
        // snapshot attached and the next ATTACH of the same name fails.
        let detach = sqlx::query(AssertSqlSafe(format!("DETACH DATABASE {ATTACH_NAME}")))
            .execute(&mut *conn)
            .await;
        match (result, detach) {
            (Ok(report), Ok(_)) => Ok(report),
            (Err(e), _) => Err(e),
            (_, Err(e)) => Err(DbError::Sqlx(e)),
        }
    }
}

/// Copy every restored table from the attached snapshot into the live
/// database, in one transaction.
async fn copy_rows(conn: &mut sqlx::SqliteConnection) -> Result<RestoreReport, DbError> {
    let mut tx: Transaction<'_, Sqlite> = conn.begin().await?;
    let mut report = RestoreReport::default();
    for table in RESTORED_TABLES {
        let columns = shared_columns(&mut tx, table).await?;
        if columns.is_empty() {
            // The snapshot and this node have no column in common for this
            // table, so there is nothing that can be copied.
            continue;
        }
        let list = columns.join(", ");
        let sql =
            format!("INSERT INTO main.{table} ({list}) SELECT {list} FROM {ATTACH_NAME}.{table}");
        let copied = sqlx::query(AssertSqlSafe(sql)).execute(&mut *tx).await?;
        match *table {
            "recipes" => report.recipes = copied.rows_affected(),
            "recipe_ingredients" => report.ingredients = copied.rows_affected(),
            "recipe_translations" => report.translations = copied.rows_affected(),
            _ => {}
        }
    }
    tx.commit().await?;
    Ok(report)
}

/// Columns that both this node's `table` and the attached snapshot have.
///
/// The snapshot may come from a node of a different version, so the list is
/// read from both sides and intersected. Columns only the live table has are
/// left to their defaults; columns only the snapshot has are dropped rather
/// than failing the restore.
async fn shared_columns(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
) -> Result<Vec<String>, DbError> {
    let live = table_columns(tx, table, None).await?;
    let snapshot = table_columns(tx, table, Some(ATTACH_NAME)).await?;
    Ok(live
        .into_iter()
        .filter(|name| snapshot.contains(name))
        .collect())
}

/// Column names of `table`, in the live database or in `schema` of the
/// attached snapshot.
async fn table_columns(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    schema: Option<&str>,
) -> Result<Vec<String>, DbError> {
    let sql = match schema {
        Some(schema) => format!("SELECT name FROM pragma_table_info('{table}', '{schema}')"),
        None => format!("SELECT name FROM pragma_table_info('{table}')"),
    };
    let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(sql))
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}
