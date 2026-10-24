//! `recipe-db`: SQLite connection pool (WAL + FTS5), repositories and search.
//!
//! Migrations live in the workspace-level `migrations/` directory and are
//! embedded at compile time via `sqlx::migrate!`, then applied automatically
//! by [`Db::open`]. Recipe persistence lives in [`recipes`].

pub mod error;
pub mod pool;
pub mod recipes;
pub mod translations;

pub use error::DbError;
pub use pool::{Db, MIGRATOR};
