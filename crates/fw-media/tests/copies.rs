//! Editing copies: every frame of a copy is the original's frame at that
//! time, chunk boundaries and variable frame rates included.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use fw_core::planar::{PlanarModel, YuvMatrix, YuvRange};
use fw_media::{CopyProfile, VideoInfo, VideoReader, probe_video, write_copy};
use fw_testmedia::level;

/// Small chunks, so short test clips cross several boundaries.
const PROFILE: CopyProfile = CopyProfile {
    name: "test",
    max_width: 1920,
    max_height: 1080,
    chunk_frames: 10,
    prores_profile: 0,
};

fn make(source: &Path, profile: &CopyProfile) -> (tempfile::TempDir, VideoInfo, Vec<u64>) {
    let dir = tempfile::tempdir().unwrap();
    let info = probe_video(source).unwrap();
    let mut landed = vec![];
    write_copy(
        source,
        &info,
        profile,
        dir.path(),
        2,
        &AtomicBool::new(false),
        |k| landed.push(k),
    )
    .unwrap();
    (dir, info, landed)
}

/// Frame `index` of the original, read from the copy in `dir`.
fn copy_frame(dir: &Path, profile: &CopyProfile, index: u64) -> fw_core::PlanarImage {
    let chunk = CopyProfile::chunk_path(dir, index / profile.chunk_frames);
    let info = probe_video(&chunk).unwrap();
    let at = info.frames.time_of(index % profile.chunk_frames).unwrap();
    VideoReader::open_intra(&chunk, &info)
        .unwrap()
        .frame(at)
        .unwrap()
}

fn gray(image: &fw_core::RgbaImage) -> u8 {
    (image.pixel(5, 5)[0] * 255.0).round() as u8
}

#[test]
fn every_frame_of_a_copy_is_the_originals() {
    for source in [fw_testmedia::count_24fps(), fw_testmedia::count_vfr()] {
        let (dir, info, landed) = make(&source, &PROFILE);
        let count = info.frames.count();
        let chunks = PROFILE.chunk_count(&info);
        assert_eq!(
            landed,
            (0..chunks).collect::<Vec<_>>(),
            "{}",
            source.display()
        );
        for i in 0..count {
            let frame = copy_frame(dir.path(), &PROFILE, i);
            assert_eq!(
                gray(&frame.to_rgba()),
                level(i),
                "{} frame {i}",
                source.display()
            );
        }
        // Each chunk holds its share of frames, the last one the remainder.
        let last = probe_video(&CopyProfile::chunk_path(dir.path(), chunks - 1)).unwrap();
        assert_eq!(
            last.frames.count(),
            count - (chunks - 1) * PROFILE.chunk_frames
        );
    }
}

#[test]
fn copies_are_prores_proxy_with_the_sources_color() {
    let source = fw_testmedia::h264();
    let (dir, info, _) = make(&source, &PROFILE);
    let original = VideoReader::open(&source)
        .unwrap()
        .frame(fw_core::Time::ZERO)
        .unwrap();
    let copy = copy_frame(dir.path(), &PROFILE, 0);
    let e = copy.encoding;
    assert_eq!(
        e.model,
        PlanarModel::Yuv {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited
        }
    );
    assert_eq!((e.bits, e.chroma_shift), (10, (1, 0)));
    assert_eq!(e.siting.0, original.encoding.siting.0);
    let chunk = CopyProfile::chunk_path(dir.path(), 0);
    assert_eq!(probe_video(&chunk).unwrap().space, info.space);
    // Close to the original. ProRes HQ shows the conversion itself is right;
    // Proxy loses more, most at saturated color edges.
    let differences = |copy: &fw_core::PlanarImage| {
        let (a, b) = (original.to_rgba(), copy.to_rgba());
        let mut d: Vec<f32> = a
            .pixels
            .iter()
            .zip(&b.pixels)
            .map(|(x, y)| (x - y).abs())
            .collect();
        d.sort_by(f32::total_cmp);
        let mean = d.iter().sum::<f32>() / d.len() as f32;
        (mean, d[d.len() * 999 / 1000], d[d.len() - 1])
    };
    let (mean, p999, _) = differences(&copy);
    assert!(
        mean < 0.01 && p999 < 0.25,
        "Proxy: mean {mean}, 99.9% {p999}"
    );
    let hq = CopyProfile {
        prores_profile: 3,
        ..PROFILE
    };
    let (dir, _, _) = make(&source, &hq);
    let (mean, _, worst) = differences(&copy_frame(dir.path(), &hq, 0));
    assert!(
        mean < 0.002 && worst < 0.1,
        "HQ: mean {mean}, worst {worst}"
    );
}

#[test]
fn black_and_white_survive_the_copy() {
    let (dir, _, _) = make(&fw_testmedia::levels_yuv(), &PROFILE);
    let frame = copy_frame(dir.path(), &PROFILE, 0).to_rgba();
    let (black, white) = (frame.pixel(8, 16), frame.pixel(56, 16));
    for c in 0..3 {
        assert!(black[c].abs() < 2e-3, "black {black:?}");
        assert!((white[c] - 1.0).abs() < 2e-3, "white {white:?}");
    }
}

#[test]
fn large_sources_are_scaled_down_to_the_profile() {
    let small = CopyProfile {
        max_width: 32,
        max_height: 32,
        ..PROFILE
    };
    let (dir, _, _) = make(&fw_testmedia::count_24fps(), &small);
    let frame = copy_frame(dir.path(), &small, 3);
    assert_eq!((frame.width, frame.height), (32, 16));
    assert_eq!(gray(&frame.to_rgba()), level(3));
}

#[test]
fn which_sources_get_copies() {
    let info = |path: &Path| probe_video(path).unwrap();
    // Long-GOP H.264 does; alpha never does.
    assert!(PROFILE.helps(&info(&fw_testmedia::h264())));
    assert!(!PROFILE.helps(&info(&fw_testmedia::count_alpha())));
    let tiny = CopyProfile {
        max_width: 16,
        max_height: 16,
        ..PROFILE
    };
    // A copy is intra-frame and small enough: it needs no copy of its own.
    let (dir, _, _) = make(&fw_testmedia::h264(), &PROFILE);
    let chunk = info(&CopyProfile::chunk_path(dir.path(), 0));
    assert!(!PROFILE.helps(&chunk));
    assert!(tiny.helps(&chunk), "larger than the profile");
}

#[test]
fn copies_from_an_earlier_session_are_reused() {
    use fw_media::EditingCopies;
    let root = tempfile::tempdir().unwrap();
    let source = fw_testmedia::h264();
    let info = probe_video(&source).unwrap();
    let fingerprint = fw_media::fingerprint(&source).unwrap();
    let chunks = PROFILE.chunk_count(&info) as usize;
    let chunk_dir = {
        let copies = EditingCopies::new(root.path(), PROFILE);
        assert_eq!(
            copies.locate(&fingerprint, 0),
            None,
            "nothing asked for yet"
        );
        copies.request(&source, fingerprint, &info);
        copies.wait();
        assert_eq!(copies.progress(&fingerprint), Some((chunks, chunks)));
        copies
            .locate(&fingerprint, 0)
            .unwrap()
            .0
            .parent()
            .unwrap()
            .to_owned()
    };
    // A later session finds the copy complete, with nothing to do.
    let copies = EditingCopies::new(root.path(), PROFILE);
    copies.request(&source, fingerprint, &info);
    assert_eq!(copies.progress(&fingerprint), Some((chunks, chunks)));
    drop(copies);
    // A missing chunk is made again; the others are kept.
    let lost = CopyProfile::chunk_path(&chunk_dir, 1);
    std::fs::remove_file(&lost).unwrap();
    let kept = std::fs::metadata(CopyProfile::chunk_path(&chunk_dir, 0))
        .unwrap()
        .modified()
        .unwrap();
    let copies = EditingCopies::new(root.path(), PROFILE);
    copies.request(&source, fingerprint, &info);
    copies.wait();
    assert_eq!(copies.progress(&fingerprint), Some((chunks, chunks)));
    assert!(lost.exists());
    let after = std::fs::metadata(CopyProfile::chunk_path(&chunk_dir, 0))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(kept, after, "chunk 0 was not rewritten");
}

#[test]
fn a_lost_chunk_falls_back_to_the_original_and_is_made_again() {
    use fw_compile::{Decoder, MediaResolver};
    use fw_media::ProjectMedia;
    // Half size, so even this intra-frame clip gets a copy.
    let half = CopyProfile {
        max_width: 32,
        max_height: 16,
        ..PROFILE
    };
    let root = tempfile::tempdir().unwrap();
    let media = ProjectMedia::with_copies("/", root.path(), half);
    let reference = fw_plugin::MediaRef {
        path: fw_testmedia::count_24fps().display().to_string(),
    };
    let info = media.probe(&reference).unwrap();
    let copies = media.copies().unwrap();
    copies.wait();
    let frame = info.frames.as_ref().unwrap().time_of(15).unwrap();
    let load = |copy| fw_compile::LoadStep {
        output: fw_plugin::ImageHandle(0),
        key: info.fingerprint,
        path: info.path.clone(),
        fingerprint: info.fingerprint,
        space: info.space,
        width: info.width,
        height: info.height,
        region: fw_core::Rect::from_size(64.0, 32.0),
        frame: Some(frame),
        node: None,
        copy,
    };
    assert_eq!(media.editing_copy(&info, frame), Some("test"));

    // The cache is cleared under a running session, before the chunk is read.
    let (chunk, _) = copies.locate(&info.fingerprint, 15).unwrap();
    std::fs::remove_file(&chunk).unwrap();
    assert!(media.decode(&load(Some("test"))).is_err());
    // Compiles now read the original, which decodes as usual...
    assert_eq!(media.editing_copy(&info, frame), None);
    assert_eq!(
        gray(&media.decode(&load(None)).unwrap().to_rgba().unwrap()),
        level(15)
    );
    // ...until the chunk is made again.
    copies.wait();
    assert_eq!(copies.failure(&info.fingerprint), None);
    assert_eq!(media.editing_copy(&info, frame), Some("test"));
    let decoded = media.decode(&load(Some("test"))).unwrap();
    assert_eq!((decoded.size()), (32, 16));
    assert_eq!(gray(&decoded.to_rgba().unwrap()), level(15));
}
