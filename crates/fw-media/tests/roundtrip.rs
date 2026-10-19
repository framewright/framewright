//! Writing and reading back each format.

use fw_core::{ColorSpace, RgbaImage};
use fw_media::{MediaError, probe, read_image, write_exr, write_png};

fn gradient(w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w - 1) as f32;
            img.set_pixel(x, y, [fx, 1.0 - fx, 0.25, if y == 0 { 0.5 } else { 1.0 }]);
        }
    }
    img
}

#[test]
fn png_round_trips_at_8_and_16_bits() {
    let dir = tempfile::tempdir().unwrap();
    let src = gradient(16, 4);
    for (sixteen, step) in [(false, 1.0 / 255.0), (true, 1.0 / 65535.0)] {
        let path = dir.path().join(format!("g{sixteen}.png"));
        write_png(&path, &src, sixteen).unwrap();
        let (back, space) = read_image(&path).unwrap();
        assert_eq!(space, ColorSpace::SRGB);
        assert!(!back.premultiplied);
        for (a, b) in src.pixels.iter().zip(&back.pixels) {
            assert!((a - b).abs() <= step / 2.0 + 1e-7, "{a} vs {b}");
        }
        let p = probe(&path).unwrap();
        assert_eq!((p.width, p.height), (16, 4));
    }
}

#[test]
fn exr_round_trips_with_its_primaries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.exr");
    let mut src = gradient(16, 4);
    src.premultiply();
    write_exr(&path, &src, ColorSpace::ACESCG).unwrap();
    let (back, space) = read_image(&path).unwrap();
    assert_eq!(space, ColorSpace::ACESCG);
    let mut straight = src.clone();
    straight.unpremultiply();
    for (a, b) in straight.pixels.iter().zip(&back.pixels) {
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }
    assert_eq!(probe(&path).unwrap().space, ColorSpace::ACESCG);
    assert!(write_exr(&dir.path().join("bad.exr"), &src, ColorSpace::SRGB).is_err());
}

#[test]
fn exr_files_are_byte_reproducible() {
    let dir = tempfile::tempdir().unwrap();
    let mut img = gradient(300, 200);
    img.premultiply();
    let (a, b) = (dir.path().join("a.exr"), dir.path().join("b.exr"));
    write_exr(&a, &img, ColorSpace::LINEAR_REC709).unwrap();
    write_exr(&b, &img, ColorSpace::LINEAR_REC709).unwrap();
    assert!(std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap());
}

#[test]
fn fingerprints_follow_content_not_names() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    write_png(&a, &gradient(8, 2), false).unwrap();
    std::fs::copy(&a, &b).unwrap();
    assert_eq!(
        probe(&a).unwrap().fingerprint,
        probe(&b).unwrap().fingerprint
    );
    write_png(&b, &gradient(8, 3), false).unwrap();
    assert_ne!(
        probe(&a).unwrap().fingerprint,
        probe(&b).unwrap().fingerprint
    );
    // Writes leave no temporary files behind.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn unknown_types_are_refused() {
    assert!(matches!(
        probe(std::path::Path::new("notes.txt")),
        Err(MediaError::Unsupported { .. })
    ));
}
