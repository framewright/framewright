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
/// `big.png` is the wrong size for the canvas.
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
