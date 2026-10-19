//! Decoding video, checked against synthetic clips whose frames say which
//! frame they are.

use fw_compile::FrameTimes;
use fw_core::color::Primaries;
use fw_core::{ColorSpace, RgbaImage, Time, Transfer};
use fw_media::{VideoReader, probe_video};
use fw_testmedia::level;

fn t(s: &str) -> Time {
    s.parse().unwrap()
}

/// Which counting frame an image is, read from one pixel.
fn frame_number(img: &RgbaImage) -> u8 {
    let [r, g, b, _] = img.pixel(5, 5);
    assert_eq!((r, g), (b, b), "counting frames are grey");
    (r * 255.0).round() as u8
}

#[test]
fn constant_rate_frames_are_exact() {
    let path = fw_testmedia::count_24fps();
    let info = probe_video(&path).unwrap();
    assert_eq!((info.width, info.height, info.has_alpha), (64, 32, false));
    assert_eq!(
        info.frames,
        FrameTimes::Constant {
            start: Time::ZERO,
            rate: Time::from_seconds(24),
            count: 48
        }
    );
    // Forwards, backwards, repeated and jumping: each answer is the right frame.
    let mut reader = VideoReader::open(&path).unwrap();
    for n in [0u64, 1, 2, 10, 3, 40, 47, 47, 11, 0] {
        let at = info
            .frames
            .frame_at(Time::of_frame(n as i64, t("24")).unwrap())
            .unwrap();
        assert_eq!(
            frame_number(&reader.frame(at).unwrap().to_rgba()),
            level(n),
            "frame {n}"
        );
    }
}

#[test]
fn variable_rate_frames_keep_their_own_times() {
    let path = fw_testmedia::count_vfr();
    let info = probe_video(&path).unwrap();
    let FrameTimes::Variable(times) = &info.frames else {
        panic!("expected variable frame times, got {:?}", info.frames);
    };
    assert_eq!(times.len(), 60);
    // Alternately 1/30 s and 2/30 s apart, at the container's millisecond resolution.
    assert_eq!(
        &times[..4],
        &[t("0"), t("33/1000"), t("1/10"), t("133/1000")]
    );
    let mut reader = VideoReader::open(&path).unwrap();
    for n in [0usize, 5, 2, 59, 30] {
        assert_eq!(
            frame_number(&reader.frame(times[n]).unwrap().to_rgba()),
            level(n as u64),
            "frame {n}"
        );
    }
    // A moment between two frames shows the earlier one.
    let between = info.frames.frame_at(t("0.05")).unwrap();
    assert_eq!(
        frame_number(&reader.frame(between).unwrap().to_rgba()),
        level(1)
    );
}

#[test]
fn seeking_gives_the_same_pixels_as_decoding_straight_through() {
    let path = fw_testmedia::h264();
    let info = probe_video(&path).unwrap();
    assert_eq!(
        info.space,
        ColorSpace::new(Primaries::Rec709, Transfer::Gamma24)
    );
    let FrameTimes::Constant { rate, count, .. } = info.frames else {
        panic!("expected a constant rate");
    };
    assert_eq!(rate, t("24000/1001"));
    // Seeking lands on keyframes, which the probe finds.
    assert_eq!(info.keyframes.first(), Some(&info.first_pts));
    assert!(info.keyframes.len() > 1, "keyframes {:?}", info.keyframes);
    let at = |n: u64| Time::of_frame(n as i64, rate).unwrap();

    let mut straight = VideoReader::open(&path).unwrap();
    let in_order: Vec<RgbaImage> = (0..count)
        .map(|n| straight.frame(at(n)).unwrap().to_rgba())
        .collect();

    // Jumping around (backwards past keyframes, and far forwards) must agree exactly.
    let mut jumping = VideoReader::open(&path).unwrap();
    for n in [count - 1, 5, 70, 1, 49, 48, 47, 0] {
        let frame = jumping.frame(at(n)).unwrap().to_rgba();
        assert!(
            frame == in_order[n as usize],
            "frame {n} differs after seeking"
        );
    }
    assert!(in_order[10] != in_order[11], "the test pattern moves");
}

#[test]
fn alpha_survives_decoding() {
    let path = fw_testmedia::count_alpha();
    let info = probe_video(&path).unwrap();
    assert!(info.has_alpha);
    let mut reader = VideoReader::open(&path).unwrap();
    let frame = reader
        .frame(Time::of_frame(5, t("24")).unwrap())
        .unwrap()
        .to_rgba();
    let alpha = frame.pixel(3, 3)[3];
    assert!((alpha - 128.0 / 255.0).abs() < 1e-6, "alpha {alpha}");
    // Colour stays straight (not premultiplied) and exact.
    assert_eq!(frame_number(&frame), level(5));
}

#[test]
fn video_black_and_white_land_on_zero_and_one() {
    let path = fw_testmedia::levels_yuv();
    let mut reader = VideoReader::open(&path).unwrap();
    let frame = reader.frame(Time::ZERO).unwrap().to_rgba();
    let black = frame.pixel(8, 16);
    let white = frame.pixel(56, 16);
    for c in 0..3 {
        assert!(black[c].abs() < 1e-4, "black {black:?}");
        assert!((white[c] - 1.0).abs() < 1e-4, "white {white:?}");
    }
    assert_eq!(white[3], 1.0);
}

#[test]
fn planar_formats_arrive_as_decoded() {
    use fw_core::planar::{PlanarModel, Siting, YuvMatrix, YuvRange};
    // H.264 4:2:0 is handed over untouched: three planes, chroma halved.
    let mut reader = VideoReader::open(&fw_testmedia::h264()).unwrap();
    let frame = reader.frame(Time::ZERO).unwrap();
    let e = frame.encoding;
    assert_eq!(
        e.model,
        PlanarModel::Yuv {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited
        }
    );
    assert_eq!(
        (e.bits, e.shift, e.chroma_shift, e.alpha),
        (8, 0, (1, 1), false)
    );
    assert_eq!(e.siting, (Siting::Cosited, Siting::Centred));
    let sizes: Vec<_> = frame.planes.iter().map(|p| (p.width, p.height)).collect();
    assert_eq!(sizes, [(320, 180), (160, 90), (160, 90)]);
    assert_eq!(frame.byte_size(), 320 * 180 * 3 / 2);

    // Packed ARGB has no planar form, so it's unpacked to 16-bit planes.
    let mut reader = VideoReader::open(&fw_testmedia::count_alpha()).unwrap();
    let e = reader.frame(Time::ZERO).unwrap().encoding;
    assert_eq!(
        (e.model, e.bits, e.shift, e.alpha),
        (PlanarModel::Rgb, 8, 8, true)
    );
}
