//! Compiling Timeline documents.

use std::path::PathBuf;
use std::sync::Arc;

use fw_compile::{
    CompileError, FrameTimes, MediaInfo, MediaResolver, Options, Plan, Step, compile,
};
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{ColorSpace, ContentHash, NodeId, Time};
use fw_doc::{
    Command, Document, Editor, Param, Placement, TimelineSettings, Track, TrackId, TrackKind,
};
use fw_plugin::{MediaRef, ParamValue};

fn t(s: &str) -> Time {
    s.parse().unwrap()
}

/// Stills are 64×32 sRGB. `.mov` files are 24 fps video, 240 frames.
/// `big.png` is the wrong size for the canvas. Names containing "alpha" have
/// an alpha channel; everything else is opaque.
struct FakeMedia;

impl MediaResolver for FakeMedia {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String> {
        let (width, height) = if media.path == "big.png" {
            (128, 64)
        } else {
            (64, 32)
        };
        let frames = media.path.ends_with(".mov").then(|| FrameTimes::Constant {
            start: Time::ZERO,
            rate: Time::from_seconds(24),
            count: 240,
        });
        Ok(MediaInfo {
            path: PathBuf::from(&media.path),
            width,
            height,
            space: ColorSpace::SRGB,
            fingerprint: ContentHash::of_bytes(media.path.as_bytes()),
            frames,
            opaque: !media.path.contains("alpha"),
            has_audio: false,
            origin: None,
        })
    }
}

struct Sample {
    editor: Editor,
    root: NodeId,
    v1: TrackId,
    v2: TrackId,
    ids: SequentialIds,
}

fn placement(track: TrackId, start: &str, duration: &str) -> Placement {
    Placement {
        track,
        start: t(start),
        duration: t(duration),
        source_in: Time::ZERO,
        speed: Time::from_seconds(1),
    }
}

impl Sample {
    fn new() -> Sample {
        let mut ids = SequentialIds::new(3);
        let root = ids.next_id();
        let (v1, v2) = (TrackId(ids.next_id()), TrackId(ids.next_id()));
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
        let track = |id, name: &str, index| Command::AddTrack {
            timeline: root,
            track: Track {
                id,
                kind: TrackKind::Video,
                name: name.into(),
            },
            index,
        };
        // V1 at the bottom, V2 above it.
        editor
            .apply("Tracks", vec![track(v1, "V1", 0), track(v2, "V2", 1)])
            .unwrap();
        Sample {
            editor,
            root,
            v1,
            v2,
            ids,
        }
    }

    fn clip(&mut self, file: &str, at: Placement) -> NodeId {
        let id = self.ids.next_id();
        let commands = vec![
            Command::AddNode {
                parent: self.root,
                id,
                type_id: "fw.clip".into(),
                name: Some(file.into()),
                placement: Some(at),
            },
            Command::SetParam {
                node: id,
                param: "file".into(),
                value: Some(Param::Constant(ParamValue::Media(MediaRef {
                    path: file.into(),
                }))),
            },
        ];
        self.editor.apply("Add clip", commands).unwrap();
        id
    }

    fn plan(&self, time: &str) -> Plan {
        compile(
            self.editor.doc(),
            self.editor.registry(),
            &FakeMedia,
            &Options::at(t(time)),
        )
        .unwrap()
    }
}

fn shown(plan: &Plan) -> String {
    match plan.steps.first() {
        Some(Step::Load(load)) => {
            let frame = load.frame.map(|f| format!("@{f}")).unwrap_or_default();
            format!("{}{frame}", load.path.display())
        }
        Some(Step::Fill(_)) => "gap".into(),
        other => panic!("unexpected first step {other:?}"),
    }
}

#[test]
fn the_top_clip_wins_and_gaps_are_transparent() {
    let mut s = Sample::new();
    let (v1, v2) = (s.v1, s.v2);
    s.clip("a.png", placement(v1, "0", "4"));
    s.clip("b.png", placement(v2, "2", "1"));
    assert_eq!(shown(&s.plan("1")), "a.png");
    assert_eq!(shown(&s.plan("2.5")), "b.png");
    assert_eq!(shown(&s.plan("3")), "a.png", "clip ends are exclusive");
    let gap = s.plan("5");
    assert_eq!(shown(&gap), "gap");
    assert_eq!((gap.width, gap.height), (64, 32));
    println!("{}", s.plan("2.5").describe());
}

#[test]
fn a_still_keys_the_same_for_its_whole_length() {
    let mut s = Sample::new();
    let v1 = s.v1;
    s.clip("a.png", placement(v1, "0", "4"));
    assert_eq!(s.plan("0").output_key(), s.plan("3.9").output_key());
    assert_eq!(
        s.plan("5").output_key(),
        s.plan("9").output_key(),
        "gaps too"
    );
}

#[test]
fn video_frames_follow_the_source_clock() {
    let mut s = Sample::new();
    let v1 = s.v1;
    // Starts at 10 s, shows the source from 1 s, at double speed.
    let mut at = placement(v1, "10", "4");
    at.source_in = t("1");
    at.speed = t("2");
    s.clip("clip.mov", at);
    // Parent 11 s → local 1 s → source 1 + 2 = 3 s → frame 72.
    assert_eq!(shown(&s.plan("11")), "clip.mov@3");
    // Within one 24 fps source frame the key holds; across frames it changes.
    assert_eq!(s.plan("11").output_key(), s.plan("11.02").output_key());
    assert_ne!(s.plan("11").output_key(), s.plan("11.03").output_key());
    assert_eq!(shown(&s.plan("11.03")), "clip.mov@73/24");
}

#[test]
fn mismatched_media_sizes_are_refused_for_now() {
    let mut s = Sample::new();
    let v1 = s.v1;
    let clip = s.clip("big.png", placement(v1, "0", "1"));
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &FakeMedia,
        &Options::at(Time::ZERO),
    )
    .unwrap_err();
    assert!(
        matches!(&err, CompileError::Node { node, .. } if *node == clip),
        "{err}"
    );
    assert!(err.to_string().contains("128×64"), "{err}");
}

fn loads(plan: &Plan) -> Vec<String> {
    plan.steps
        .iter()
        .filter_map(|s| match s {
            Step::Load(l) => Some(l.path.display().to_string()),
            _ => None,
        })
        .collect()
}

fn set(s: &mut Sample, node: NodeId, param: &str, value: ParamValue) {
    s.editor
        .apply(
            "Set",
            vec![Command::SetParam {
                node,
                param: param.into(),
                value: Some(Param::Constant(value)),
            }],
        )
        .unwrap();
}

#[test]
fn layers_with_alpha_composite_over_the_tracks_below() {
    let mut s = Sample::new();
    let (v1, v2) = (s.v1, s.v2);
    s.clip("a.png", placement(v1, "0", "4"));
    s.clip("title-alpha.png", placement(v2, "1", "2"));
    let plan = s.plan("2");
    println!("{}", plan.describe());
    assert_eq!(loads(&plan), ["title-alpha.png", "a.png"]);
    let composite = plan
        .steps
        .iter()
        .find_map(|st| match st {
            Step::Kernel(k) if k.ops.iter().any(|op| op.label == "composite normal") => Some(k),
            _ => None,
        })
        .expect("a composite kernel");
    assert_eq!(composite.extra_inputs.len(), 1);
    validate(&composite.wgsl);

    // What's underneath now matters to the result.
    let opaque_top = s.plan("0.5");
    assert_eq!(loads(&opaque_top), ["a.png"]);
    let mut moved = Sample::new();
    let (v1, v2) = (moved.v1, moved.v2);
    moved.clip("b.png", placement(v1, "0", "4"));
    moved.clip("title-alpha.png", placement(v2, "1", "2"));
    assert_ne!(moved.plan("2").output_key(), plan.output_key());
}

#[test]
fn an_opaque_full_frame_layer_hides_everything_below() {
    let mut s = Sample::new();
    let (v1, v2) = (s.v1, s.v2);
    s.clip("under.png", placement(v1, "0", "4"));
    let top = s.clip("over.png", placement(v2, "0", "4"));
    assert_eq!(loads(&s.plan("1")), ["over.png"]);

    // Fading the top layer makes the one below show through.
    set(&mut s, top, "opacity", ParamValue::Float(50.0));
    assert_eq!(loads(&s.plan("1")), ["over.png", "under.png"]);

    // So does a mode other than Normal.
    set(&mut s, top, "opacity", ParamValue::Float(100.0));
    set(&mut s, top, "composite", ParamValue::Text("add".into()));
    let plan = s.plan("1");
    assert_eq!(loads(&plan), ["over.png", "under.png"]);
    assert!(plan.describe().contains("composite add"));
}

fn validate(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl)
        .unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(wgsl)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
}

#[test]
fn a_clip_with_a_graph_shows_its_media_through_the_graph() {
    let mut s = Sample::new();
    let v1 = s.v1;
    let clip = s.clip("clip.mov", placement(v1, "0", "4"));
    let plain = s.plan("1");
    let [input, exposure, output] = [(); 3].map(|_| s.ids.next_id());
    let add = |id, type_id: &str| Command::AddNode {
        parent: clip,
        id,
        type_id: type_id.into(),
        name: None,
        placement: None,
    };
    let connect = |from, to| Command::Connect {
        edge: fw_doc::Edge {
            from: fw_doc::PortRef::new(from, "image"),
            to: fw_doc::PortRef::new(to, "image"),
        },
    };
    s.editor
        .apply(
            "Create Clip Graph",
            vec![
                Command::MakeGraph { node: clip },
                add(input, "fw.input"),
                add(output, "fw.output"),
                connect(input, output),
            ],
        )
        .unwrap();
    assert_eq!(
        shown(&s.plan("1")),
        "clip.mov@1",
        "Input is the clip's media"
    );
    s.editor
        .apply(
            "Add Exposure",
            vec![
                add(exposure, "fw.exposure"),
                connect(input, exposure),
                connect(exposure, output),
            ],
        )
        .unwrap();
    set(&mut s, exposure, "exposure", ParamValue::Float(1.0));
    let plan = s.plan("1");
    assert_eq!(shown(&plan), "clip.mov@1");
    assert!(plan.describe().contains("exposure"), "{}", plan.describe());
    assert_ne!(plan.output_key(), plain.output_key());
    assert_ne!(
        plan.output_key(),
        s.plan("1.5").output_key(),
        "frames still follow the source clock"
    );
    for step in &plan.steps {
        if let Step::Kernel(k) = step {
            validate(&k.wgsl);
        }
    }

    s.editor
        .apply("Delete", vec![Command::RemoveNode { id: output }])
        .unwrap();
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &FakeMedia,
        &Options::at(t("1")),
    )
    .unwrap_err();
    assert!(
        matches!(&err, CompileError::Node { node, .. } if *node == clip),
        "{err}"
    );
    assert!(err.to_string().contains("no Output"), "{err}");
}
