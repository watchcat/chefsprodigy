//! Cover image conversion: downsize to a sane width and encode as WebP.
//!
//! The `image` crate ships a lossless WebP encoder only; lossy quality 80 would
//! require native libwebp bindings, which conflicts with the single static
//! binary goal. Lossless at 1200px keeps files small enough for a home node.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::ScraperError;

/// Maximum width of stored cover images in pixels.
pub const MAX_WIDTH: u32 = 1200;

/// Decode any supported image, cap its width at [`MAX_WIDTH`] preserving the
/// aspect ratio, and return WebP bytes.
pub fn to_webp_cover(bytes: &[u8]) -> Result<Vec<u8>, ScraperError> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > MAX_WIDTH {
        img.resize(MAX_WIDTH, u32::MAX, FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img.to_rgba8()).write_to(&mut out, ImageFormat::WebP)?;
    Ok(out.into_inner())
}

/// On-disk store for cover images: `<dir>/<recipe_id>.webp`, served by the
/// node at `images/<recipe_id>.webp`.
#[derive(Debug, Clone)]
pub struct ImageStore {
    dir: std::path::PathBuf,
}

impl ImageStore {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Convert `raw` to WebP, write it for `recipe_id`, and return the web path
    /// to store in `Recipe::image_path`.
    pub fn save_cover(&self, recipe_id: &str, raw: &[u8]) -> Result<String, ScraperError> {
        let webp = to_webp_cover(raw)?;
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(format!("{recipe_id}.webp")), webp)?;
        Ok(format!("images/{recipe_id}.webp"))
    }

    /// Remove the cover for `recipe_id` if present.
    pub fn delete_cover(&self, recipe_id: &str) -> Result<(), ScraperError> {
        match std::fs::remove_file(self.dir.join(format!("{recipe_id}.webp"))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Absolute path for a served file name (`<id>.webp`). Returns `None` for
    /// anything that is not a plain `.webp` basename, which blocks traversal.
    pub fn file_path(&self, name: &str) -> Option<std::path::PathBuf> {
        let stem = name.strip_suffix(".webp")?;
        let valid = !stem.is_empty()
            && stem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        valid.then(|| self.dir.join(name))
    }
}

/// Longest side of a photo after normalization, in pixels.
pub const MAX_PHOTO_SIDE: u32 = 1600;
/// Maximum photos per import.
pub const MAX_PHOTOS: usize = 6;
/// JPEG quality for normalized photos.
const PHOTO_JPEG_QUALITY: u8 = 85;

/// A photo ready to be sent to a vision model: oriented, bounded, JPEG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPhoto {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Always `image/jpeg`.
    pub media_type: &'static str,
}

/// Decode any supported image, apply its EXIF orientation, cap the longest
/// side at [`MAX_PHOTO_SIDE`] and re-encode as JPEG.
pub fn normalize_photo(bytes: &[u8]) -> Result<NormalizedPhoto, ScraperError> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    if img.width().max(img.height()) > MAX_PHOTO_SIDE {
        img = img.resize(MAX_PHOTO_SIDE, MAX_PHOTO_SIDE, FilterType::Triangle);
    }
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut out = Cursor::new(Vec::new());
    rgb.write_with_encoder(JpegEncoder::new_with_quality(&mut out, PHOTO_JPEG_QUALITY))?;
    Ok(NormalizedPhoto {
        bytes: out.into_inner(),
        width: rgb.width(),
        height: rgb.height(),
        media_type: "image/jpeg",
    })
}
