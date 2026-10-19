//! Project media access: probes are remembered until a file changes.

use fw_compile::{Decoder, MediaResolver};
use fw_media::ProjectMedia;
use fw_plugin::MediaRef;

#[test]
fn probes_are_cached_until_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mkv");
    std::fs::copy(fw_testmedia::count_24fps(), &clip).unwrap();
    let media = ProjectMedia::new(dir.path());
    let reference = MediaRef {
        path: "clip.mkv".into(),
    };

    let first = media.probe(&reference).unwrap();
    for _ in 0..10 {
        assert_eq!(media.probe(&reference).unwrap(), first);
    }
    assert_eq!(
        media.probe_reads(),
        1,
        "compiling frames doesn't re-read the file"
    );

    // Replacing the file with different content is noticed.
    std::fs::copy(fw_testmedia::count_vfr(), &clip).unwrap();
    let changed = media.probe(&reference).unwrap();
    assert_eq!(media.probe_reads(), 2);
    assert_ne!(changed.fingerprint, first.fingerprint);
    assert!(changed.frames != first.frames);

    // Decoding through the cached probe still gives the right frame.
    let plan_frame = fw_compile::LoadStep {
        output: fw_plugin::ImageHandle(0),
        key: changed.fingerprint,
        path: changed.path.clone(),
        fingerprint: changed.fingerprint,
        space: changed.space,
        width: changed.width,
        height: changed.height,
        region: fw_core::Rect::from_size(64.0, 32.0),
        frame: Some("1/10".parse().unwrap()),
    };
    let image = media.decode(&plan_frame).unwrap();
    assert_eq!(
        (image.pixel(5, 5)[0] * 255.0).round() as u8,
        fw_testmedia::level(2)
    );
    assert_eq!(media.probe_reads(), 2);
}
