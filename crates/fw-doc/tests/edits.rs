//! Media pool, copying nodes, and the timeline edits built from commands.

mod common;

use std::sync::Arc;

use common::*;
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::edits::{ripple_delete, split_clip};
use fw_doc::format::{from_json, to_json};
use fw_doc::*;
use fw_plugin::ParamValue;

fn t(s: &str) -> Time {
    s.parse().unwrap()
}

struct Sample {
    editor: Editor,
    root: NodeId,
    v1: TrackId,
    ids: SequentialIds,
}

fn placement(track: TrackId, start: &str, duration: &str, source_in: &str) -> Placement {
    Placement {
        track,
        start: t(start),
        duration: t(duration),
        source_in: t(source_in),
        speed: Time::from_seconds(1),
    }
}

impl Sample {
    fn new() -> Sample {
        let mut ids = SequentialIds::new(3);
        let root = ids.next_id();
        let v1 = TrackId(ids.next_id());
        let settings = TimelineSettings {
            width: 64,
            height: 32,
            rate: Time::from_seconds(24),
            sample_rate: 48000,
        };
        let mut editor = Editor::new(
            Document::new_timeline(root, settings),
            Arc::new(full_registry()),
        );
        let track = Track {
            id: v1,
            kind: TrackKind::Video,
            name: "V1".into(),
        };
        editor
            .apply(
                "Track",
                vec![Command::AddTrack {
                    timeline: root,
                    track,
                    index: 0,
                }],
            )
            .unwrap();
        Sample {
            editor,
            root,
            v1,
            ids,
        }
    }

    fn clip(&mut self, start: &str, duration: &str) -> NodeId {
        let id = self.ids.next_id();
        let add = Command::AddNode {
            parent: self.root,
            id,
            type_id: fw_plugin::CLIP_TYPE.into(),
            name: None,
            placement: Some(placement(self.v1, start, duration, "10")),
        };
        self.editor.apply("Add Clip", vec![add]).unwrap();
        id
    }

    fn placement(&self, clip: NodeId) -> Placement {
        self.editor.doc().timeline(self.root).unwrap().clips[&clip].clone()
    }

    fn round_trip(&self) {
        let doc = self.editor.doc();
        doc.check_invariants().unwrap();
        let text = to_json(doc);
        assert_eq!(&from_json(&text, self.editor.registry()).unwrap(), doc);
    }
}

#[test]
fn imported_media_is_saved_and_undone() {
    let mut s = Sample::new();
    let add = |path: &str| Command::AddMedia { path: path.into() };
    s.editor
        .apply("Import", vec![add("clips/b.mp4"), add("a.png")])
        .unwrap();
    let pool: Vec<&str> = s.editor.doc().media_pool().collect();
    assert_eq!(pool, ["a.png", "clips/b.mp4"]);
    assert!(to_json(s.editor.doc()).contains("\"media\": ["));
    s.round_trip();
    assert!(matches!(
        s.editor.apply("Again", vec![add("a.png")]).unwrap_err(),
        CommandError::MediaListed(_)
    ));
    s.editor.undo().unwrap();
    assert_eq!(s.editor.doc().media_pool().count(), 0);
    assert!(!to_json(s.editor.doc()).contains("\"media\""));
}

#[test]
fn a_copied_clip_brings_its_graph_with_new_ids() {
    let mut s = Sample::new();
    let a = s.clip("0", "2");
    let (input, output) = clip_graph_ids(a);
    let gain = s.ids.next_id();
    let connect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    s.editor
        .apply(
            "Gain",
            vec![
                Command::AddNode {
                    parent: a,
                    id: gain,
                    type_id: "test.gain".into(),
                    name: None,
                    placement: None,
                },
                connect(input, gain),
                connect(gain, output),
            ],
        )
        .unwrap();
    let copy = s.ids.next_id();
    let duplicate = Command::DuplicateNode {
        id: a,
        new_id: copy,
        placement: Some(placement(s.v1, "2", "2", "10")),
    };
    s.editor.apply("Duplicate", vec![duplicate]).unwrap();
    let doc = s.editor.doc();
    let graph = doc.graph(copy).unwrap();
    assert_eq!(graph.nodes.len(), 3);
    assert!(!graph.nodes.contains(&gain), "the copy has its own nodes");
    let (copy_input, copy_output) = clip_graph_ids(copy);
    let copy_gain = doc.source_of(&PortRef::new(copy_output, "image")).unwrap();
    assert_eq!(
        doc.source_of(&PortRef::new(copy_gain.node, "image")),
        Some(&PortRef::new(copy_input, "image")),
        "connections follow the copies"
    );
    s.round_trip();

    // A clip that only passes its media through stays saved without its graph.
    let b = s.clip("4", "1");
    let copy_of_b = s.ids.next_id();
    let duplicate = Command::DuplicateNode {
        id: b,
        new_id: copy_of_b,
        placement: Some(placement(s.v1, "5", "1", "10")),
    };
    s.editor.apply("Duplicate", vec![duplicate]).unwrap();
    assert!(!to_json(s.editor.doc()).contains(&clip_graph_ids(copy_of_b).0.to_string()));

    // Undo the second copy, the second clip, then the first copy.
    for _ in 0..3 {
        s.editor.undo().unwrap();
    }
    assert!(!s.editor.doc().contains(copy));
    assert!(s.editor.doc().contains(a));
}

#[test]
fn splitting_a_clip_keeps_every_frame_the_same() {
    let mut s = Sample::new();
    let a = s.clip("1", "4");
    // A fade-out in the clip's last second, anchored to its end.
    let fade = Param::Keyframed(Keyframes {
        keyframes: vec![
            Keyframe {
                t: t("-1"),
                v: ParamValue::Float(100.0),
                interp: Interp::Linear,
                anchor: Anchor::End,
            },
            Keyframe {
                t: Time::ZERO,
                v: ParamValue::Float(0.0),
                interp: Interp::Linear,
                anchor: Anchor::End,
            },
        ],
    });
    s.editor
        .apply(
            "Fade",
            vec![Command::SetParam {
                node: a,
                param: "opacity".into(),
                value: Some(fade),
            }],
        )
        .unwrap();
    let opacity_at = |doc: &Document, clip: NodeId, p: &Placement, when: &str| {
        let local = t(when).checked_sub(p.start).unwrap();
        let param = &doc.node(clip).unwrap().as_known().unwrap().params["opacity"];
        param
            .value_at(KeyTime {
                local,
                length: Some(p.duration),
            })
            .unwrap()
    };
    let before = s.editor.doc().clone();
    let whole = s.placement(a);
    let times = ["1", "2", "3", "4", "9/2", "19/4"];
    let expected: Vec<ParamValue> = times
        .iter()
        .map(|when| opacity_at(&before, a, &whole, when))
        .collect();

    let b = s.ids.next_id();
    let commands = split_clip(s.editor.doc(), a, t("3"), b).unwrap();
    s.editor.apply("Split", commands).unwrap();
    let (first, second) = (s.placement(a), s.placement(b));
    assert_eq!((first.start, first.duration), (t("1"), t("2")));
    assert_eq!((second.start, second.duration), (t("3"), t("2")));
    assert_eq!(
        second.source_in,
        t("12"),
        "the second half plays on from there"
    );
    let doc = s.editor.doc();
    for (when, want) in times.iter().zip(&expected) {
        let (clip, p) = if t(when) < t("3") {
            (a, &first)
        } else {
            (b, &second)
        };
        assert_eq!(&opacity_at(doc, clip, p, when), want, "at {when}");
    }
    s.round_trip();
    assert!(split_clip(doc, a, t("1"), s.ids.next_id()).is_err());
    s.editor.undo().unwrap();
    assert_eq!(s.editor.doc(), &before, "one undo");
}

#[test]
fn ripple_delete_closes_the_gap() {
    let mut s = Sample::new();
    let a = s.clip("0", "2");
    let b = s.clip("2", "1");
    let c = s.clip("4", "1");
    let commands = ripple_delete(s.editor.doc(), a).unwrap();
    s.editor.apply("Ripple Delete", commands).unwrap();
    assert!(!s.editor.doc().contains(a));
    assert_eq!(s.placement(b).start, Time::ZERO);
    assert_eq!(s.placement(c).start, t("2"), "the gap after b stays");
    s.round_trip();
}
