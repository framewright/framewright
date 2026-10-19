//! Rendering on the GPU, checked against the colour maths in fw-core.
//!
//! These need a GPU (or a software Vulkan driver such as llvmpipe). Pick one
//! with `FW_ADAPTER=<part of its name>`.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use fw_compile::{Options, Plan, compile};
use fw_core::color::{conversion_matrix, mul_vec};
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{ColorSpace, NodeId, RgbaImage, Time};
use fw_doc::{Command, Document, Edge, Editor, Param, PortRef};
use fw_media::ProjectMedia;
use fw_plugin::{MediaRef, ParamValue};
use fw_render::{DEFAULT_CACHE_BYTES, Gpu, Renderer};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let gpu = Gpu::new().expect("these tests need a GPU; set FW_ADAPTER to choose one");
        eprintln!("rendering on {}", gpu.describe());
        Arc::new(gpu)
    })
    .clone()
}

/// An awkward size, so row padding and partial workgroups are exercised.
const W: u32 = 61;
const H: u32 = 37;

/// Ramps in every channel, including alpha, and values near 0 and 1.
fn test_image() -> RgbaImage {
    let mut img = RgbaImage::new(W, H, false);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            img.set_pixel(x, y, [fx, fy, 1.0 - fx * fy, 0.25 + 0.75 * fy]);
        }
    }
    img
}

struct Scene {
    _dir: tempfile::TempDir,
    media: ProjectMedia,
    editor: Editor,
    read: NodeId,
    exposure: NodeId,
    output: NodeId,
    /// The file's pixels as the decoder will see them (quantised to 8 bits).
    source: RgbaImage,
}

fn set(node: NodeId, param: &str, value: ParamValue) -> Command {
    Command::SetParam {
        node,
        param: param.into(),
        value: Some(Param::Constant(value)),
    }
}

fn scene() -> Scene {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("gradient.png");
    fw_media::write_png(&file, &test_image(), false).unwrap();
    let (source, _) = fw_media::read_image(&file).unwrap();

    let mut ids = SequentialIds::new(1);
    let root = ids.next_id();
    let [read, exposure, output] = [(); 3].map(|_| ids.next_id());
    let mut editor = Editor::new(Document::new(root), Arc::new(fw_ops::builtin_registry()));
    let add = |id, type_id: &str| Command::AddNode {
        parent: root,
        id,
        type_id: type_id.into(),
        name: None,
        placement: None,
    };
    let connect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    editor
        .apply(
            "Build",
            vec![
                add(read, "fw.read_image"),
                add(exposure, "fw.exposure"),
                add(output, "fw.output"),
                set(
                    read,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: "gradient.png".into(),
                    }),
                ),
                set(exposure, "exposure", ParamValue::Float(1.0)),
                connect(read, exposure),
                connect(exposure, output),
            ],
        )
        .unwrap();
    Scene {
        media: ProjectMedia::new(dir.path()),
        _dir: dir,
        editor,
        read,
        exposure,
        output,
        source,
    }
}

fn plan(s: &Scene) -> Plan {
    compile(
        s.editor.doc(),
        s.editor.registry(),
        &s.media,
        &Options::at(Time::ZERO),
    )
    .unwrap()
}

/// What the plan should produce, computed in f64 on the CPU.
fn reference(source: &RgbaImage, input: ColorSpace, stops: f64, output: ColorSpace) -> RgbaImage {
    let working = ColorSpace::ACESCG;
    let to_working = conversion_matrix(input.primaries, working.primaries);
    let to_output = conversion_matrix(working.primaries, output.primaries);
    let mut out = RgbaImage::new(source.width, source.height, true);
    for y in 0..source.height {
        for x in 0..source.width {
            // The engine uploads premultiplied half floats; model that rounding,
            // since colour matrices with large coefficients magnify it.
            let [r, g, b, a] = source.pixel(x, y);
            let half = |v: f32| f64::from(half::f16::from_f32(v).to_f32());
            let a16 = half(a);
            let straight = |v: f32| if a16 > 0.0 { half(v * a) / a16 } else { 0.0 };
            let a = a16;
            let rgb = [straight(r), straight(g), straight(b)].map(|v| input.transfer.decode(v));
            let rgb = mul_vec(&to_working, rgb).map(|v| v * 2f64.powf(stops));
            let rgb = mul_vec(&to_output, rgb).map(|v| output.transfer.encode(v));
            out.set_pixel(
                x,
                y,
                [rgb[0] * a, rgb[1] * a, rgb[2] * a, a].map(|v| v as f32),
            );
        }
    }
    out
}

/// Half floats carry about 11 bits of precision.
fn assert_close(got: &RgbaImage, want: &RgbaImage, what: &str) {
    assert_eq!((got.width, got.height), (want.width, want.height));
    let mut worst = (0.0f32, 0usize);
    for (i, (g, w)) in got.pixels.iter().zip(&want.pixels).enumerate() {
        let err = (g - w).abs() / w.abs().max(1.0);
        if err > worst.0 || err.is_nan() {
            worst = (err, i);
        }
    }
    let (err, i) = worst;
    assert!(
        err <= 1e-3,
        "{what}: pixel {} channel {}: got {} want {} (relative error {err})",
        i / 4,
        i % 4,
        got.pixels[i],
        want.pixels[i]
    );
}

#[test]
fn every_colour_space_pair_matches_the_cpu_reference() {
    let mut s = scene();
    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let spaces = [
        ("srgb", ColorSpace::SRGB),
        ("linear_rec709", ColorSpace::LINEAR_REC709),
        ("acescg", ColorSpace::ACESCG),
        ("acescct", ColorSpace::ACESCCT),
        ("rec709_display", ColorSpace::REC709_DISPLAY),
    ];
    for (in_id, in_space) in spaces {
        for (out_id, out_space) in spaces {
            s.editor
                .apply(
                    "Spaces",
                    vec![
                        set(s.read, "space", ParamValue::Text(in_id.into())),
                        set(s.output, "space", ParamValue::Text(out_id.into())),
                    ],
                )
                .unwrap();
            let got = renderer.render(&plan(&s), &s.media).unwrap();
            let want = reference(&s.source, in_space, 1.0, out_space);
            assert_close(&got, &want, &format!("{in_id} → {out_id}"));
        }
    }
}

#[test]
fn caches_skip_repeated_work() {
    let mut s = scene();
    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let first = renderer.render(&plan(&s), &s.media).unwrap();
    let again = renderer.render(&plan(&s), &s.media).unwrap();
    assert!(
        Arc::ptr_eq(&first, &again),
        "an unchanged plan comes from the cache"
    );
    assert_eq!(renderer.stats().cache_hits, 1);
    assert_eq!(renderer.stats().kernels, 1);

    // A new exposure re-runs the kernel but reuses the decoded file and the shader.
    s.editor
        .apply(
            "Exposure",
            vec![set(s.exposure, "exposure", ParamValue::Float(-2.0))],
        )
        .unwrap();
    let darker = renderer.render(&plan(&s), &s.media).unwrap();
    assert_ne!(*darker, *first);
    let stats = renderer.stats();
    assert_eq!(
        (stats.decodes, stats.kernels, stats.pipelines_built),
        (1, 2, 1)
    );
}

#[test]
fn rendering_is_bit_identical_with_a_cold_cache() {
    let s = scene();
    let a = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan(&s), &s.media)
        .unwrap();
    let b = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan(&s), &s.media)
        .unwrap();
    let bits = |img: &RgbaImage| img.pixels.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b));
}

/// The rendered output, stored as an EXR. Rewrite it with `FW_BLESS=1` after a
/// deliberate change to rendering.
#[test]
fn output_matches_golden_image() {
    let s = scene();
    let got = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan(&s), &s.media)
        .unwrap();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/exposure.exr");
    if std::env::var_os("FW_BLESS").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        fw_media::write_exr(&golden, &got, ColorSpace::LINEAR_REC709).unwrap();
    }
    let (mut want, space) =
        fw_media::read_image(&golden).expect("run with FW_BLESS=1 to create the golden image");
    want.premultiply();
    assert_eq!(space, ColorSpace::LINEAR_REC709);
    assert_close(&got, &want, "golden");
}

#[test]
fn timelines_render_clips_and_transparent_gaps() {
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    let s = scene();
    let mut ids = SequentialIds::new(50);
    let root = ids.next_id();
    let (v1, clip) = (TrackId(ids.next_id()), ids.next_id());
    let settings = TimelineSettings {
        width: W,
        height: H,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    editor
        .apply(
            "Build",
            vec![
                Command::AddTrack {
                    timeline: root,
                    track: Track {
                        id: v1,
                        kind: TrackKind::Video,
                        name: "V1".into(),
                    },
                    index: 0,
                },
                Command::AddNode {
                    parent: root,
                    id: clip,
                    type_id: "fw.clip".into(),
                    name: None,
                    placement: Some(Placement {
                        track: v1,
                        start: Time::ZERO,
                        duration: Time::from_seconds(1),
                        source_in: Time::ZERO,
                        speed: Time::from_seconds(1),
                    }),
                },
                set(
                    clip,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: "gradient.png".into(),
                    }),
                ),
            ],
        )
        .unwrap();
    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let mut at = |t: &str| {
        let plan = compile(
            editor.doc(),
            editor.registry(),
            &s.media,
            &Options::at(t.parse().unwrap()),
        )
        .unwrap();
        renderer.render(&plan, &s.media).unwrap()
    };
    let shown = at("1/2");
    let want = reference(&s.source, ColorSpace::SRGB, 0.0, ColorSpace::LINEAR_REC709);
    assert_close(&shown, &want, "clip");
    let gap = at("2");
    assert!(
        gap.pixels.iter().all(|v| *v == 0.0),
        "a gap is transparent black"
    );
}

#[test]
fn layers_composite_over_each_other_in_linear_light() {
    use fw_core::color::conversion_matrix;
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    let s = scene();
    // A second, different image for the bottom track.
    let mut bottom_src = test_image();
    for px in bottom_src.pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 1.0;
    }
    fw_media::write_png(&s._dir.path().join("bottom.png"), &bottom_src, false).unwrap();
    let (bottom_src, _) = fw_media::read_image(&s._dir.path().join("bottom.png")).unwrap();

    let mut ids = SequentialIds::new(60);
    let root = ids.next_id();
    let (v1, v2) = (TrackId(ids.next_id()), TrackId(ids.next_id()));
    let (under, over) = (ids.next_id(), ids.next_id());
    let settings = TimelineSettings {
        width: W,
        height: H,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    let track = |id, index| Command::AddTrack {
        timeline: root,
        track: Track {
            id,
            kind: TrackKind::Video,
            name: format!("V{}", index + 1),
        },
        index,
    };
    let clip = |id, track| Command::AddNode {
        parent: root,
        id,
        type_id: "fw.clip".into(),
        name: None,
        placement: Some(Placement {
            track,
            start: Time::ZERO,
            duration: Time::from_seconds(1),
            source_in: Time::ZERO,
            speed: Time::from_seconds(1),
        }),
    };
    editor
        .apply(
            "Build",
            vec![
                track(v1, 0),
                track(v2, 1),
                clip(under, v1),
                clip(over, v2),
                set(
                    under,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: "bottom.png".into(),
                    }),
                ),
                set(
                    over,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: "gradient.png".into(),
                    }),
                ),
                set(over, "opacity", ParamValue::Float(50.0)),
            ],
        )
        .unwrap();
    let plan = compile(
        editor.doc(),
        editor.registry(),
        &s.media,
        &Options::at(Time::ZERO),
    )
    .unwrap();
    let got = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan, &s.media)
        .unwrap();

    // Both layers in ACEScg, the top one faded and stored as half floats
    // between passes, then "over", then to linear Rec.709.
    let half = |v: f32| half::f16::from_f32(v).to_f32();
    let top = reference(&s.source, ColorSpace::SRGB, 0.0, ColorSpace::ACESCG);
    let below = reference(&bottom_src, ColorSpace::SRGB, 0.0, ColorSpace::ACESCG);
    let to_rec709 = conversion_matrix(
        ColorSpace::ACESCG.primaries,
        ColorSpace::LINEAR_REC709.primaries,
    );
    let mut want = RgbaImage::new(W, H, true);
    for y in 0..H {
        for x in 0..W {
            let fg = top.pixel(x, y).map(|v| half(v * 0.5));
            let bg = below.pixel(x, y);
            let over: [f32; 4] = std::array::from_fn(|i| fg[i] + bg[i] * (1.0 - fg[3]));
            let rgb = mul_vec(&to_rec709, [over[0], over[1], over[2]].map(f64::from));
            want.set_pixel(x, y, [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, over[3]]);
        }
    }
    assert_close(&got, &want, "50% layer over another");
}

#[test]
fn video_clips_show_the_frame_for_each_moment() {
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    let video = fw_testmedia::count_24fps();
    let mut ids = SequentialIds::new(70);
    let root = ids.next_id();
    let (v1, clip) = (TrackId(ids.next_id()), ids.next_id());
    let settings = TimelineSettings {
        width: 64,
        height: 32,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    editor
        .apply(
            "Build",
            vec![
                Command::AddTrack {
                    timeline: root,
                    track: Track {
                        id: v1,
                        kind: TrackKind::Video,
                        name: "V1".into(),
                    },
                    index: 0,
                },
                // On the timeline from 1 s, showing the source from 0.5 s.
                Command::AddNode {
                    parent: root,
                    id: clip,
                    type_id: "fw.clip".into(),
                    name: None,
                    placement: Some(Placement {
                        track: v1,
                        start: Time::from_seconds(1),
                        duration: Time::from_seconds(1),
                        source_in: "1/2".parse().unwrap(),
                        speed: Time::from_seconds(1),
                    }),
                },
                set(
                    clip,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: video.display().to_string(),
                    }),
                ),
            ],
        )
        .unwrap();
    let media = ProjectMedia::new("/");
    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let level_at = |renderer: &mut Renderer, t: &str| {
        let plan = compile(
            editor.doc(),
            editor.registry(),
            &media,
            &Options::at(t.parse().unwrap()),
        )
        .unwrap();
        let img = renderer.render(&plan, &media).unwrap();
        // Untagged video is treated as Rec.709 on a 2.4 display; output is linear Rec.709.
        let linear = img.pixel(10, 10)[0] as f64;
        (linear.powf(1.0 / 2.4) * 255.0).round() as u8
    };
    // Timeline 1.25 s → source 0.75 s → frame 18.
    assert_eq!(level_at(&mut renderer, "1.25"), fw_testmedia::level(18));
    assert_eq!(level_at(&mut renderer, "1.5"), fw_testmedia::level(24));
    assert_eq!(level_at(&mut renderer, "1"), fw_testmedia::level(12));
    let hits = renderer.stats().cache_hits;
    assert_eq!(level_at(&mut renderer, "1.25"), fw_testmedia::level(18));
    assert_eq!(
        renderer.stats().cache_hits,
        hits + 1,
        "a frame already shown comes from the cache"
    );
}
