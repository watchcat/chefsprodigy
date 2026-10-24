use image::{ImageFormat, RgbImage};
use recipe_scraper::images::{MAX_WIDTH, to_webp_cover};
use std::io::Cursor;

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

#[test]
fn converts_to_webp_and_downsizes_wide_images() {
    let out = to_webp_cover(&png(2400, 1200)).unwrap();
    assert_eq!(&out[0..4], b"RIFF");
    assert_eq!(&out[8..12], b"WEBP");
    let decoded = image::load_from_memory(&out).unwrap();
    assert_eq!(decoded.width(), MAX_WIDTH);
    assert_eq!(decoded.height(), 600, "aspect ratio preserved");
}

#[test]
fn small_images_are_not_upscaled() {
    let out = to_webp_cover(&png(300, 200)).unwrap();
    let decoded = image::load_from_memory(&out).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (300, 200));
}

#[test]
fn garbage_bytes_are_an_error() {
    assert!(to_webp_cover(b"not an image").is_err());
}

mod store {
    use recipe_scraper::images::ImageStore;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn store() -> (ImageStore, std::path::PathBuf) {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("recipe-images-test-{}-{n}", std::process::id()));
        (ImageStore::new(&dir), dir)
    }

    #[test]
    fn save_cover_converts_writes_and_returns_web_path() {
        let (store, dir) = store();
        let web_path = store.save_cover("01ABC", &super::png(50, 20)).unwrap();
        assert_eq!(web_path, "images/01ABC.webp");
        let bytes = std::fs::read(dir.join("01ABC.webp")).unwrap();
        assert_eq!(&bytes[8..12], b"WEBP");
        assert_eq!(store.file_path("01ABC.webp").unwrap(), dir.join("01ABC.webp"));
    }

    #[test]
    fn file_path_rejects_traversal_and_odd_names() {
        let (store, _) = store();
        for bad in ["../x.webp", "a/b.webp", "", "x.png", "x.webp\0"] {
            assert!(store.file_path(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn delete_cover_is_idempotent() {
        let (store, dir) = store();
        store.save_cover("gone", &super::png(10, 10)).unwrap();
        store.delete_cover("gone").unwrap();
        store.delete_cover("gone").unwrap();
        assert!(!dir.join("gone.webp").exists());
    }
}

mod photos {
    use recipe_scraper::images::{MAX_PHOTO_SIDE, normalize_photo};

    #[test]
    fn large_photo_is_downscaled_and_reencoded_as_jpeg() {
        let p = normalize_photo(&super::png(3000, 2000)).unwrap();
        assert_eq!((p.width, p.height), (MAX_PHOTO_SIDE, 1067));
        assert_eq!(&p.bytes[0..3], &[0xFF, 0xD8, 0xFF], "JPEG magic");
        assert_eq!(p.media_type, "image/jpeg");
        assert!(p.bytes.len() < 3000 * 2000, "far smaller than raw pixels");
    }

    #[test]
    fn small_photo_keeps_its_size() {
        let p = normalize_photo(&super::png(640, 480)).unwrap();
        assert_eq!((p.width, p.height), (640, 480));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(normalize_photo(b"not a photo").is_err());
    }
}
