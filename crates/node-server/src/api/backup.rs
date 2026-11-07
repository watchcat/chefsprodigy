//! `/api/v1/export` and `/api/v1/import/export`: backup and restore of a node.
//!
//! The export is a zip holding `recipes.db` (a consistent snapshot taken with
//! `VACUUM INTO`) and the `images/` directory, which is the layout
//! `hubctl restore` already unpacks into a hub's data directory. The import
//! takes the same zip back and restores it into an empty node.
//!
//! Both endpoints are owner-only: the archive holds the whole library, and
//! the nightly backup job authenticates with the hub's owner key from its
//! secret.
//!
//! The archive is built on disk and streamed back, never held in memory in
//! full: a hub's covers add up, and the node runs under a tight memory
//! budget.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::auth::RequireOwner;
use crate::{ApiError, AppState};

/// Name of the database file inside the archive.
const DB_ENTRY: &str = "recipes.db";
/// Directory of cover images inside the archive.
const IMAGES_DIR: &str = "images";
/// Upper bound for an uploaded backup archive.
const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;
/// Upper bound for a single file read out of the archive.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
/// Name of the form field the archive is uploaded in.
const UPLOAD_FIELD: &str = "file";

pub fn router() -> Router<AppState> {
    Router::new().route("/export", get(export)).route(
        "/import/export",
        post(import_export).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
    )
}

/// `GET /api/v1/export`: the whole node as a zip archive.
///
/// The archive holds `recipes.db` (a consistent snapshot) and `images/`, so
/// `hubctl restore` can unpack it straight into a hub's data directory.
async fn export(State(state): State<AppState>, _owner: RequireOwner) -> Result<Response, ApiError> {
    let scratch = ScratchDir::new("chefsprodigy-export")?;
    let archive = scratch.path().join("export.zip");
    state.backup().write_archive(&archive).await?;
    let file = tokio::fs::File::open(&archive).await?;
    let stream = ReaderStream::new(file);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"chefsprodigy-export.zip\"",
            ),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

/// `POST /api/v1/import/export`: restore an archive into this node.
///
/// The node must be empty: restoring over existing recipes would mix two
/// sets of rows and leave the ids ambiguous.
async fn import_export(
    State(state): State<AppState>,
    _owner: RequireOwner,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let scratch = ScratchDir::new("chefsprodigy-import")?;
    let upload = scratch.path().join("upload.zip");
    let mut received = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(e.body_text()))?
    {
        if field.file_name().is_none() && field.name() != Some(UPLOAD_FIELD) {
            continue;
        }
        // The upload goes to disk as it arrives, so a large archive never has
        // to fit in memory.
        let mut out = tokio::fs::File::create(&upload).await?;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::BadRequest(e.body_text()))?
        {
            tokio::io::AsyncWriteExt::write_all(&mut out, &chunk).await?;
        }
        out.flush().await?;
        received = true;
        break;
    }
    if !received {
        return Err(ApiError::BadRequest(format!(
            "multipart upload must contain a {UPLOAD_FIELD} field"
        )));
    }
    let report = state.backup().restore_from_archive(&upload).await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "recipes": report.recipes,
            "ingredients": report.ingredients,
            "translations": report.translations,
        })),
    )
        .into_response())
}

/// Reads and writes the node's backup archive.
pub struct BackupService {
    db: recipe_db::Db,
    images: recipe_scraper::images::ImageStore,
}

impl BackupService {
    pub fn new(db: recipe_db::Db, images: recipe_scraper::images::ImageStore) -> Self {
        Self { db, images }
    }

    /// Write the archive to `dest`: a consistent database snapshot plus the
    /// covers.
    pub async fn write_archive(&self, dest: &Path) -> Result<(), ApiError> {
        let scratch = ScratchDir::new("chefsprodigy-build")?;
        let snapshot = scratch.path().join(DB_ENTRY);
        self.db.snapshot_to(&snapshot).await?;
        let images = self.images.dir().to_path_buf();
        // The zip is written by blocking code, off the async runtime's
        // threads, because it is CPU-bound work on files.
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            let file = std::fs::File::create(&dest)?;
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file(DB_ENTRY, options)
                .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
            // The database is copied through, not read into memory: a hub's
            // database is the largest thing in the archive.
            let mut db_file = std::fs::File::open(&snapshot)?;
            std::io::copy(&mut db_file, &mut zip)?;

            // Covers, under the same names the hub serves them.
            let covers = cover_files(&images)?;
            if !covers.is_empty() {
                zip.add_directory(IMAGES_DIR, options)
                    .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
            }
            for cover in covers {
                let path = format!("{IMAGES_DIR}/{}", file_name(&cover));
                zip.start_file(path, options)
                    .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
                let mut cover_file = std::fs::File::open(&cover)?;
                std::io::copy(&mut cover_file, &mut zip)?;
            }
            zip.finish()
                .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
            Ok(())
        })
        .await
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?
    }

    /// Restore an archive into this node.
    pub async fn restore_from_archive(
        &self,
        archive: &Path,
    ) -> Result<recipe_db::RestoreReport, ApiError> {
        let scratch = ScratchDir::new("chefsprodigy-extract")?;
        let extracted = scratch.path().join("extracted");
        std::fs::create_dir_all(&extracted)?;

        // The zip crate is synchronous and needs a seekable reader, so the
        // extraction runs on a blocking thread.
        let archive = archive.to_path_buf();
        let target = extracted.clone();
        let db_path = tokio::task::spawn_blocking(move || extract_archive(&archive, &target))
            .await
            .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))??;

        let report = self.db.restore_from(&db_path).await?;
        self.restore_covers(&extracted).await?;
        Ok(report)
    }

    /// Put the covers from an extracted archive back into the image store.
    async fn restore_covers(&self, extracted: &Path) -> Result<(), ApiError> {
        let dir = extracted.join(IMAGES_DIR);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(ApiError::Internal(e.into())),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".webp") {
                continue;
            }
            let bytes = tokio::fs::read(entry.path())
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            self.images
                .save_cover_bytes(&name, &bytes)
                .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
        }
        Ok(())
    }
}

/// Unpack the archive into `extracted`, and return the path of the database
/// file inside it.
///
/// Only two kinds of entry are taken: the database file, and cover images
/// under `images/` whose names are plain `.webp` file names. Anything else in
/// the archive is ignored, so an archive cannot write outside the extracted
/// directory.
fn extract_archive(archive: &Path, extracted: &Path) -> Result<PathBuf, ApiError> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| ApiError::BadRequest(format!("not a readable zip archive: {e}")))?;
    let mut db_path: Option<PathBuf> = None;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| ApiError::BadRequest(format!("unreadable archive entry: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|e| ApiError::BadRequest(format!("unreadable archive name: {e}")))?
            .into_owned();
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(ApiError::BadRequest(format!(
                "archive entry {name} is too large"
            )));
        }
        let path = if name == DB_ENTRY {
            let path = extracted.join(DB_ENTRY);
            db_path = Some(path.clone());
            path
        } else if let Some(file) = cover_name(&name) {
            extracted.join(IMAGES_DIR).join(file)
        } else {
            continue;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&path)?;
        // Copied through rather than read into memory: the database is the
        // largest entry in the archive.
        std::io::copy(&mut entry, &mut out)?;
    }
    db_path.ok_or_else(|| {
        ApiError::BadRequest(format!("archive has no {DB_ENTRY}; is it a node backup?"))
    })
}

/// The cover files in `dir`, sorted by name so an archive is reproducible.
fn cover_files(dir: &Path) -> Result<Vec<PathBuf>, ApiError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ApiError::Internal(e.into())),
    };
    let mut covers = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".webp") {
            continue;
        }
        covers.push(entry.path());
    }
    covers.sort();
    Ok(covers)
}

/// The file name of a path.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The cover file name of an archive entry, if it is one.
///
/// The name must be a plain `.webp` file name: no directory parts, so an
/// archive cannot place a file outside the image directory. The rule is the
/// image store's own, so an archive that carries a name the store would
/// refuse to serve is skipped rather than half-restored.
fn cover_name(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix(IMAGES_DIR)?;
    let rest = rest.strip_prefix('/')?;
    if recipe_scraper::images::is_cover_file_name(rest) {
        Some(rest)
    } else {
        None
    }
}

/// A directory that is removed when it goes out of scope.
struct ScratchDir {
    path: PathBuf,
}

/// Counts scratch directories so two requests never share one.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl ScratchDir {
    fn new(tag: &str) -> Result<Self, ApiError> {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
