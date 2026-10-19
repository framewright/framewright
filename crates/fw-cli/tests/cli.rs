//! The `fw` binary end to end. Needs a GPU, like the fw-render tests.

use std::path::Path;
use std::process::Command;

fn fw(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_fw"))
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "fw {args:?} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn example(dir: &Path) -> String {
    fw(&["example", dir.to_str().unwrap()]);
    dir.join("project.json").to_str().unwrap().to_owned()
}

#[test]
fn renders_are_byte_identical_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let project = example(dir.path());
    let a = dir.path().join("a.exr");
    let b = dir.path().join("b.exr");
    fw(&[
        "render",
        &project,
        "--time",
        "1.5",
        "--out",
        a.to_str().unwrap(),
    ]);
    fw(&[
        "render",
        &project,
        "--time",
        "3/2",
        "--out",
        b.to_str().unwrap(),
    ]);
    let (a, b) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    assert!(
        a == b,
        "the two renders differ ({} and {} bytes)",
        a.len(),
        b.len()
    );
}

#[test]
fn renders_png_in_srgb() {
    let dir = tempfile::tempdir().unwrap();
    let project = example(dir.path());
    let out = dir.path().join("frame.png");
    let report = fw(&[
        "render",
        &project,
        "--out",
        out.to_str().unwrap(),
        "--sixteen-bit",
    ]);
    assert!(report.contains("1920×1080 sRGB"), "{report}");
    let (img, _) = fw_media_probe(&out);
    assert_eq!((img.0, img.1), (1920, 1080));
}

fn fw_media_probe(path: &Path) -> ((u32, u32), ()) {
    let bytes = std::fs::read(path).unwrap();
    // PNG IHDR: width and height are big-endian u32s at bytes 16..24.
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    ((w, h), ())
}

#[test]
fn inspect_shows_the_fused_conversions() {
    let dir = tempfile::tempdir().unwrap();
    let project = example(dir.path());
    let plan = fw(&["inspect", &project, "--wgsl"]);
    assert!(plan.contains("fused"), "{plan}");
    assert!(plan.contains("inserted: sRGB → ACEScg"), "{plan}");
    assert!(plan.contains("@compute"), "{plan}");
}

#[test]
fn bad_inputs_fail_cleanly() {
    let out = Command::new(env!("CARGO_BIN_EXE_fw"))
        .args(["render", "nope.json", "--out", "x.exr"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope.json"));
}
