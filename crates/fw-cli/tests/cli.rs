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

#[test]
fn renders_timelines_at_any_time() {
    let dir = tempfile::tempdir().unwrap();
    example(dir.path());
    let timeline = dir.path().join("timeline.json");
    let timeline = timeline.to_str().unwrap();
    let plan = fw(&["inspect", timeline, "--time", "1"]);
    assert!(plan.contains("load"), "{plan}");
    let gap = fw(&["inspect", timeline, "--time", "2.5"]);
    assert!(gap.contains("fill"), "{gap}");
    let out = dir.path().join("t.exr");
    fw(&[
        "render",
        timeline,
        "--time",
        "4",
        "--out",
        out.to_str().unwrap(),
    ]);
}

/// Saves a one-clip timeline of the test H.264 file and returns its path.
fn video_project(dir: &Path) -> String {
    let video = fw_testmedia::h264();
    let json = format!(
        r#"{{
  "format": "framewright.project",
  "format_version": 1,
  "root": "00000000000000000000000001",
  "nodes": {{
    "00000000000000000000000001": {{
      "type": "fw.timeline",
      "version": 1,
      "name": "Main Timeline",
      "timeline": {{
        "width": 320,
        "height": 180,
        "rate": "24000/1001",
        "sample_rate": 48000,
        "tracks": [{{ "id": "00000000000000000000000002", "kind": "video", "name": "V1" }}],
        "clips": {{
          "00000000000000000000000003": {{
            "track": "00000000000000000000000002",
            "start": "0",
            "duration": "4",
            "source_in": "0",
            "node": {{
              "type": "fw.clip",
              "version": 1,
              "name": "testsrc",
              "params": {{ "file": {{ "path": "{}" }} }}
            }}
          }}
        }}
      }}
    }}
  }}
}}
"#,
        video.display()
    );
    let path = dir.join("video.json");
    std::fs::write(&path, json).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn video_frames_render_identically_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let project = video_project(dir.path());
    let out = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    fw(&["render", &project, "--time", "2.5", "--out", &out("a.exr")]);
    fw(&["render", &project, "--time", "2.5", "--out", &out("b.exr")]);
    fw(&["render", &project, "--time", "1", "--out", &out("c.exr")]);
    let read = |name: &str| std::fs::read(dir.path().join(name)).unwrap();
    assert!(
        read("a.exr") == read("b.exr"),
        "the same frame renders the same bytes"
    );
    assert!(
        read("a.exr") != read("c.exr"),
        "different moments show different frames"
    );
    let plan = fw(&["inspect", &project, "--time", "2.5"]);
    assert!(plan.contains("frame at"), "{plan}");
}

#[test]
fn plays_a_timeline_against_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let project = video_project(dir.path());
    let report = fw(&[
        "play",
        &project,
        "--from",
        "1",
        "--seconds",
        "1.5",
        "--no-audio",
    ]);
    let summary = report.lines().last().unwrap();
    assert!(summary.contains("0 failed"), "{report}");
    // "played 1.5 s: 37 frames shown, 0 dropped, 0 failed"
    let shown: u64 = summary
        .split(": ")
        .nth(1)
        .and_then(|rest| rest.split(" frames shown").next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{report}"));
    assert!(shown >= 30, "{report}");
}

#[test]
fn makes_editing_copies_in_the_project_cache() {
    let dir = tempfile::tempdir().unwrap();
    let project = video_project(dir.path());
    let report = fw(&["copies", &project]);
    assert!(
        report.lines().last().unwrap().starts_with("done in"),
        "{report}"
    );
    let copies = dir
        .path()
        .join(".framewright/cache/copies/prores-proxy-1080-v1");
    let chunks: Vec<_> = std::fs::read_dir(&copies)
        .unwrap()
        .flat_map(|copy| std::fs::read_dir(copy.unwrap().path()).unwrap())
        .map(|chunk| chunk.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(chunks, ["chunk-000000.mov"], "{report}");
    // Playing uses them; with --no-copies it doesn't need them.
    for extra in [None, Some("--no-copies")] {
        let mut args = vec!["play", &project, "--seconds", "1", "--no-audio"];
        args.extend(extra);
        let report = fw(&args);
        assert!(
            report.lines().last().unwrap().contains("0 failed"),
            "{report}"
        );
    }
}
