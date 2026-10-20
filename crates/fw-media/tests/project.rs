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
        node: None,
        copy: None,
    };
    let image = media.decode(&plan_frame).unwrap().to_rgba();
    assert_eq!(
        (image.pixel(5, 5)[0] * 255.0).round() as u8,
        fw_testmedia::level(2)
    );
    assert_eq!(media.probe_reads(), 2);
}

#[test]
fn two_clips_of_one_file_each_get_their_frames() {
    use fw_core::Time;
    use fw_core::id::{IdGen, SequentialIds};
    let mut ids = SequentialIds::new(1);
    let nodes = [ids.next_id(), ids.next_id()];
    let path = fw_testmedia::count_24fps();
    let media = ProjectMedia::new("/");
    let info = media
        .probe(&MediaRef {
            path: path.display().to_string(),
        })
        .unwrap();
    let load = |node: usize, n: i64| fw_compile::LoadStep {
        output: fw_plugin::ImageHandle(0),
        key: info.fingerprint,
        path: path.clone(),
        fingerprint: info.fingerprint,
        space: info.space,
        width: info.width,
        height: info.height,
        region: fw_core::Rect::from_size(64.0, 32.0),
        frame: Some(Time::of_frame(n, Time::from_seconds(24)).unwrap()),
        node: Some(nodes[node]),
        copy: None,
    };
    // Two clips playing side by side from different places in the file.
    for n in 0..12 {
        for (node, start) in [(0, 0), (1, 30)] {
            let image = media.decode(&load(node, start + n)).unwrap().to_rgba();
            assert_eq!(
                (image.pixel(5, 5)[0] * 255.0).round() as u8,
                fw_testmedia::level((start + n) as u64),
                "clip {node}, frame {}",
                start + n
            );
        }
    }
}
