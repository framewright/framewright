//! Rendering on the GPU, checked against the color maths in fw-core.
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
    /// The file's pixels as the decoder will see them (quantized to 8 bits).
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
            // since color matrices with large coefficients magnify it.
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
fn every_color_space_pair_matches_the_cpu_reference() {
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
fn the_frame_cache_shows_unchanged_frames_again_for_free() {
    let mut s = scene();
    let mut plain = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    plain.render_gpu(&plan(&s), &s.media).unwrap();
    plain.render_gpu(&plan(&s), &s.media).unwrap();
    assert_eq!(plain.stats().cache_hits, 0, "off unless asked for");

    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES).with_frame_cache(64 << 20);
    let first = renderer.render_gpu(&plan(&s), &s.media).unwrap();
    assert!(renderer.has_frame(&plan(&s)));
    let again = renderer.render_gpu(&plan(&s), &s.media).unwrap();
    assert_eq!(
        again.texture, first.texture,
        "the same texture, from the cache"
    );
    assert_eq!(
        (renderer.stats().cache_hits, renderer.stats().kernels),
        (1, 1)
    );

    // An edit changes the key: a new frame, while the old one stays cached.
    let before = plan(&s);
    s.editor
        .apply(
            "Exposure",
            vec![set(s.exposure, "exposure", ParamValue::Float(-2.0))],
        )
        .unwrap();
    assert!(!renderer.has_frame(&plan(&s)));
    let darker = renderer.render_gpu(&plan(&s), &s.media).unwrap();
    assert_ne!(darker.texture, first.texture);
    assert!(renderer.has_frame(&before));
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

/// One clip filling a timeline the clip's size, delivered in the clip's own
/// space, so the plan's output is exactly what the GPU unpacked.
fn unpacked_on_gpu(video: &Path, at: Time) -> (RgbaImage, Plan) {
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    let info = fw_media::probe_video(video).unwrap();
    let mut ids = SequentialIds::new(90);
    let root = ids.next_id();
    let (v1, clip) = (TrackId(ids.next_id()), ids.next_id());
    let settings = TimelineSettings {
        width: info.width,
        height: info.height,
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
                        path: video.display().to_string(),
                    }),
                ),
            ],
        )
        .unwrap();
    let media = ProjectMedia::new("/");
    let options = Options {
        delivery_space: info.space,
        ..Options::at(at)
    };
    let plan = compile(editor.doc(), editor.registry(), &media, &options).unwrap();
    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let image = renderer.render(&plan, &media).unwrap();
    (Arc::unwrap_or_clone(image), plan)
}

#[test]
fn gpu_unpacking_matches_the_cpu_reference() {
    // 4:2:0 YUV with co-sited chroma, packed ARGB (unpacked to 16 bits by
    // swscale), planar RGB and limited-range levels.
    for video in [
        fw_testmedia::h264(),
        fw_testmedia::count_alpha(),
        fw_testmedia::count_24fps(),
        fw_testmedia::levels_yuv(),
    ] {
        let at = Time::of_frame(5, Time::from_seconds(24)).unwrap();
        let (got, plan) = unpacked_on_gpu(&video, at);
        assert!(
            !plan.describe().contains("kernel"),
            "{}: the output should be the unpacked load\n{}",
            video.display(),
            plan.describe()
        );
        let info = fw_media::probe_video(&video).unwrap();
        let mut reader = fw_media::VideoReader::open(&video).unwrap();
        let mut want = reader
            .frame(info.frames.frame_at(at).unwrap())
            .unwrap()
            .to_rgba();
        want.premultiply();
        // Half-float output: within one f16 step of the reference.
        for (i, (g, w)) in got.pixels.iter().zip(&want.pixels).enumerate() {
            assert!(
                (g - w).abs() <= w.abs().max(1.0 / 1024.0) / 1024.0 + 1e-6,
                "{}: value {i} is {g}, reference {w}",
                video.display()
            );
        }
    }
}

#[test]
fn several_video_layers_each_show_their_own_frame() {
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    // Frame 0 (black) below frame 10 at 50%: two uploads of the same size
    // in one plan, which must not share textures.
    let video = fw_testmedia::count_24fps();
    let mut ids = SequentialIds::new(110);
    let root = ids.next_id();
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
    let mut commands = vec![];
    for (index, (frame, opacity)) in [(0, 100.0), (10, 50.0)].into_iter().enumerate() {
        let (track, clip) = (TrackId(ids.next_id()), ids.next_id());
        commands.extend([
            Command::AddTrack {
                timeline: root,
                track: Track {
                    id: track,
                    kind: TrackKind::Video,
                    name: format!("V{}", index + 1),
                },
                index,
            },
            Command::AddNode {
                parent: root,
                id: clip,
                type_id: "fw.clip".into(),
                name: None,
                placement: Some(Placement {
                    track,
                    start: Time::ZERO,
                    duration: Time::from_seconds(1),
                    source_in: Time::of_frame(frame, Time::from_seconds(24)).unwrap(),
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
            set(clip, "opacity", ParamValue::Float(opacity)),
        ]);
    }
    editor.apply("Build", commands).unwrap();
    let media = ProjectMedia::new("/");
    let plan = compile(
        editor.doc(),
        editor.registry(),
        &media,
        &Options::at(Time::ZERO),
    )
    .unwrap();
    let image = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan, &media)
        .unwrap();
    // Untagged video is Rec.709 on a 2.4 display; the output is linear Rec.709.
    let top = (fw_testmedia::level(10) as f32 / 255.0).powf(2.4);
    let got = image.pixel(10, 10)[0];
    assert!(
        (got - 0.5 * top).abs() < 2e-3,
        "got {got}, want {} (half of frame 10 over black)",
        0.5 * top
    );
}

#[test]
fn draft_frames_come_from_editing_copies() {
    use fw_compile::Step;
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    use fw_media::CopyProfile;
    let video = fw_testmedia::count_24fps();
    let mut ids = SequentialIds::new(130);
    let (root, track, clip) = (ids.next_id(), TrackId(ids.next_id()), ids.next_id());
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
                        id: track,
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
                        track,
                        start: Time::ZERO,
                        duration: Time::from_seconds(2),
                        source_in: Time::ZERO,
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
    // A copy at half size, in chunks of 10 frames.
    let profile = CopyProfile {
        name: "test-half",
        max_width: 32,
        max_height: 16,
        chunk_frames: 10,
        prores_profile: 0,
    };
    let cache = tempfile::tempdir().unwrap();
    let media = ProjectMedia::with_copies("/", cache.path(), profile);
    let at = Time::of_frame(15, Time::from_seconds(24)).unwrap();
    let plan_at = |quality| {
        let mut options = Options::at(at);
        options.quality = quality;
        compile(editor.doc(), editor.registry(), &media, &options).unwrap()
    };
    let load = |plan: &Plan| {
        plan.steps
            .iter()
            .find_map(|s| match s {
                Step::Load(l) => Some(l.clone()),
                _ => None,
            })
            .unwrap()
    };
    // Compiling asked for the copy; once it's made, drafts read it.
    plan_at(fw_plugin::Quality::Final);
    media.copies().unwrap().wait();
    let (draft, last) = (
        plan_at(fw_plugin::Quality::Draft),
        plan_at(fw_plugin::Quality::Final),
    );
    let (draft_load, final_load) = (load(&draft), load(&last));
    assert_eq!(draft_load.copy, Some("test-half"));
    assert_eq!(final_load.copy, None);
    assert_ne!(
        draft_load.key, final_load.key,
        "copy and original never share a cache entry"
    );

    let mut renderer = Renderer::new(gpu(), DEFAULT_CACHE_BYTES);
    let want = (fw_testmedia::level(15) as f32 / 255.0).powf(2.4);
    for (plan, what) in [(&draft, "draft, from the half-size copy"), (&last, "final")] {
        let image = renderer.render(plan, &media).unwrap();
        assert_eq!((image.width, image.height), (64, 32), "{what}");
        let got = image.pixel(10, 10)[0];
        assert!((got - want).abs() < 5e-3, "{what}: got {got}, want {want}");
    }
    assert_eq!(renderer.stats().decodes, 2);
}

/// A timeline the size of `video` showing it from its start on each of
/// `layers` tracks, the upper ones at 50%.
fn video_timeline(video: &Path, layers: usize) -> Editor {
    use fw_doc::{Placement, TimelineSettings, Track, TrackId, TrackKind};
    let info = fw_media::probe_video(video).unwrap();
    let mut ids = SequentialIds::new(150);
    let root = ids.next_id();
    let settings = TimelineSettings {
        width: info.width,
        height: info.height,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    let mut commands = vec![];
    for index in 0..layers {
        let (track, clip) = (TrackId(ids.next_id()), ids.next_id());
        commands.extend([
            Command::AddTrack {
                timeline: root,
                track: Track {
                    id: track,
                    kind: TrackKind::Video,
                    name: format!("V{}", index + 1),
                },
                index,
            },
            Command::AddNode {
                parent: root,
                id: clip,
                type_id: "fw.clip".into(),
                name: None,
                placement: Some(Placement {
                    track,
                    start: Time::ZERO,
                    duration: Time::from_seconds(60),
                    source_in: Time::ZERO,
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
            set(
                clip,
                "opacity",
                ParamValue::Float(if index == 0 { 100.0 } else { 50.0 }),
            ),
        ]);
    }
    editor.apply("Build", commands).unwrap();
    editor
}

/// Renders `frames` of `video` with hardware and with software decoding,
/// requiring identical pixels. Returns how many pictures were decoded in
/// hardware and how many decoded at all; `None` when the GPU can't decode
/// video.
fn compare_hardware_decoding(video: &Path, layers: usize, frames: &[i64]) -> Option<(usize, u64)> {
    let gpu = gpu();
    let Some(device) = gpu.video_device() else {
        eprintln!("{} has no hardware decoding; skipped", gpu.describe());
        return None;
    };
    let info = fw_media::probe_video(video).unwrap();
    let fw_compile::FrameTimes::Constant { rate, .. } = info.frames else {
        panic!("expected a constant rate");
    };
    let editor = video_timeline(video, layers);
    let software = ProjectMedia::new("/");
    let mut hardware = ProjectMedia::new("/");
    hardware
        .use_hardware(device, fw_media::HardwarePolicy::Always)
        .unwrap();
    let mut on_cpu = Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES);
    let mut on_gpu = Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES);
    for &n in frames {
        // Final quality, so nothing reads an editing copy.
        let options = Options::at(Time::of_frame(n, rate).unwrap());
        let plan = compile(editor.doc(), editor.registry(), &software, &options).unwrap();
        let want = on_cpu.render(&plan, &software).unwrap();
        let got = on_gpu.render(&plan, &hardware).unwrap();
        assert!(
            got.pixels == want.pixels,
            "{}: frame {n} differs between hardware and software decoding",
            video.display()
        );
    }
    Some((hardware.hardware_frames(), on_gpu.stats().decodes))
}

#[test]
fn hardware_decoding_matches_software() {
    // In order, then seeking backwards past keyframes and far forwards.
    let mut frames: Vec<i64> = (0..30).collect();
    frames.extend([100, 5, 70, 49, 48, 47, 0, 119]);
    if let Some((in_hardware, decoded)) =
        compare_hardware_decoding(&fw_testmedia::h264(), 1, &frames)
    {
        assert_eq!(
            in_hardware as u64, decoded,
            "every frame was decoded in hardware"
        );
        assert!(decoded > 30);
    }
}

#[test]
fn one_hardware_picture_read_twice_in_a_plan() {
    // Both layers load the same frame, so the plan reads one decoded
    // picture twice; it must be acquired once, or its waits deadlock.
    compare_hardware_decoding(&fw_testmedia::h264(), 2, &[0, 1, 2, 30]);
}

#[test]
fn ten_bit_hardware_decoding_matches_software() {
    // HEVC Main 10: P010 from hardware, 16-bit planes from software.
    let frames = [0, 1, 2, 25, 3, 47];
    if let Some((in_hardware, decoded)) =
        compare_hardware_decoding(&fw_testmedia::hevc_10bit(), 1, &frames)
    {
        assert_eq!(
            in_hardware as u64, decoded,
            "every frame was decoded in hardware"
        );
    }
}

#[test]
fn streams_the_hardware_cant_decode_fall_back_to_software() {
    // 4:2:2 H.264: FFmpeg's Vulkan decoder declines it on today's GPUs.
    let frames = [0, 1, 30, 2];
    if let Some((in_hardware, decoded)) =
        compare_hardware_decoding(&fw_testmedia::h264_422(), 1, &frames)
    {
        assert!(decoded > 0);
        eprintln!("4:2:2: {in_hardware} of {decoded} pictures decoded in hardware");
    }
}

#[test]
fn the_faster_decoder_is_chosen_for_each_kind_of_video() {
    let gpu = gpu();
    let Some(device) = gpu.video_device() else {
        eprintln!("{} has no hardware decoding; skipped", gpu.describe());
        return;
    };
    let video = fw_testmedia::h264();
    let editor = video_timeline(&video, 1);
    let mut media = ProjectMedia::new("/");
    media
        .use_hardware(device, fw_media::HardwarePolicy::Fastest)
        .unwrap();
    let mut renderer = Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES);
    for n in 0..5 {
        let options = Options::at(Time::of_frame(n, Time::from_seconds(24)).unwrap());
        let plan = compile(editor.doc(), editor.registry(), &media, &options).unwrap();
        renderer.render(&plan, &media).unwrap();
    }
    // Measured once, on first use; the frames went the way it chose.
    let choices = media.decoder_choices();
    assert_eq!(choices.len(), 1, "{choices:?}");
    let choice = &choices[0];
    assert!(
        choice.format.starts_with("h264 High yuv420p 320×180"),
        "{choice:?}"
    );
    assert_eq!(
        choice.hardware,
        fw_media::prefers_hardware(choice.hardware_time, choice.software_time)
    );
    let in_hardware = media.hardware_frames() as u64;
    assert_eq!(
        in_hardware,
        if choice.hardware {
            renderer.stats().decodes
        } else {
            0
        }
    );
    eprintln!("{choice:?}");
}

/// Writes each pixel's `pos` relative to `frame`, so the mapping from pixels
/// to canvas units can be read back.
struct Position;

impl fw_plugin::ImageBehavior for Position {
    fn input_space(&self, _: &fw_plugin::ResolvedParams, _: usize) -> fw_plugin::SpaceNeed {
        // The delivery space, so nothing converts what it writes.
        fw_plugin::SpaceNeed::Exact(ColorSpace::LINEAR_REC709)
    }

    fn emit(
        &self,
        _params: &fw_plugin::ResolvedParams,
        _request: &fw_plugin::Request,
        inputs: &[fw_plugin::ImageHandle],
        plan: &mut dyn fw_plugin::PlanBuilder,
    ) -> Result<fw_plugin::ImageHandle, fw_plugin::NodeError> {
        let op = fw_plugin::PixelOp {
            label: "position".into(),
            wgsl: "return vec4<f32>((pos - frame.xy) / (frame.zw - frame.xy), frame.x, 1.0);"
                .into(),
            params: vec![],
        };
        Ok(plan.per_pixel(inputs[0], op))
    }
}

#[test]
fn operations_see_each_pixels_center_in_canvas_units() {
    let mut s = scene();
    let mut registry = fw_ops::builtin_registry();
    let desc = s.editor.registry().desc("fw.exposure").unwrap().clone();
    let desc = fw_plugin::NodeTypeDesc {
        id: "test.position".into(),
        name: "Position".into(),
        params: vec![],
        ..desc
    };
    registry
        .register(fw_plugin::NodeType::new(desc).with_image(Position, "position"))
        .unwrap();
    let mut editor = Editor::new(s.editor.doc().clone(), Arc::new(registry));
    let position = SequentialIds::new(50).next_id();
    let root = editor.doc().root();
    editor
        .apply(
            "Position",
            vec![
                Command::AddNode {
                    parent: root,
                    id: position,
                    type_id: "test.position".into(),
                    name: None,
                    placement: None,
                },
                Command::Connect {
                    edge: Edge {
                        from: PortRef::new(s.read, "image"),
                        to: PortRef::new(position, "image"),
                    },
                },
                Command::Connect {
                    edge: Edge {
                        from: PortRef::new(position, "image"),
                        to: PortRef::new(s.output, "image"),
                    },
                },
            ],
        )
        .unwrap();
    s.editor = editor;
    let got = Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
        .render(&plan(&s), &s.media)
        .unwrap();
    for (x, y) in [
        (0, 0),
        (W - 1, 0),
        (0, H - 1),
        (W / 2, H / 2),
        (W - 1, H - 1),
    ] {
        let [u, v, left, _] = got.pixel(x, y);
        let want = [(x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32];
        assert!(
            (u - want[0]).abs() < 1e-3 && (v - want[1]).abs() < 1e-3,
            "pixel ({x}, {y}) saw {:?}, not {want:?}",
            [u, v]
        );
        assert_eq!(left, 0.0, "the frame starts at the canvas origin");
    }
}

#[test]
fn invert_flips_each_channel_in_linear_light() {
    let mut s = scene();
    let renderer = |s: &Scene| {
        Renderer::new(gpu(), DEFAULT_CACHE_BYTES)
            .render(&plan(s), &s.media)
            .unwrap()
    };
    s.editor
        .apply(
            "No change",
            vec![set(s.exposure, "exposure", ParamValue::Float(0.0))],
        )
        .unwrap();
    let plain = renderer(&s);
    let invert = SequentialIds::new(50).next_id();
    let root = s.editor.doc().root();
    let connect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    s.editor
        .apply(
            "Invert",
            vec![
                Command::AddNode {
                    parent: root,
                    id: invert,
                    type_id: "fw.invert".into(),
                    name: None,
                    placement: None,
                },
                connect(s.exposure, invert),
                connect(invert, s.output),
            ],
        )
        .unwrap();
    let inverted = renderer(&s);
    // In the working space each premultiplied channel becomes alpha minus
    // itself; the output space sees white carried across by its matrix.
    let white = mul_vec(
        &conversion_matrix(
            ColorSpace::ACESCG.primaries,
            ColorSpace::LINEAR_REC709.primaries,
        ),
        [1.0; 3],
    );
    let mut want = RgbaImage::new(W, H, true);
    for y in 0..H {
        for x in 0..W {
            let [r, g, b, a] = plain.pixel(x, y);
            let flip = |v: f32, w: f64| (f64::from(a) * w) as f32 - v;
            want.set_pixel(
                x,
                y,
                [flip(r, white[0]), flip(g, white[1]), flip(b, white[2]), a],
            );
        }
    }
    assert_close(&inverted, &want, "invert");
}
