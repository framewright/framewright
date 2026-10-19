//! Timeline documents: editing rules, saving, and exact undo.

mod common;

use std::sync::Arc;

use common::*;
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::format::{from_json, to_json};
use fw_doc::*;
use fw_plugin::ParamValue;
use proptest::prelude::*;

fn t(s: &str) -> Time {
    s.parse().unwrap()
}

fn settings() -> TimelineSettings {
    TimelineSettings {
        width: 64,
        height: 32,
        rate: t("24000/1001"),
        sample_rate: 48000,
    }
}

fn track(id: NodeId, kind: TrackKind, name: &str) -> Track {
    Track {
        id: TrackId(id),
        kind,
        name: name.into(),
    }
}

fn at(track: NodeId, start: &str, duration: &str) -> Placement {
    Placement {
        track: TrackId(track),
        start: t(start),
        duration: t(duration),
        source_in: Time::ZERO,
        speed: Time::from_seconds(1),
    }
}

fn add_clip(parent: NodeId, id: NodeId, placement: Placement) -> Command {
    Command::AddNode {
        parent,
        id,
        type_id: "test.clip".into(),
        name: None,
        placement: Some(placement),
    }
}

/// V1 and V2, two clips, an end-anchored fade on one.
fn sample() -> (Editor, [NodeId; 5]) {
    let mut ids = SequentialIds::new(7);
    let root = ids.next_id();
    let [v1, v2, a, b] = [(); 4].map(|_| ids.next_id());
    let mut editor = Editor::new(
        Document::new_timeline(root, settings()),
        Arc::new(full_registry()),
    );
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
    let mut slow = at(v2, "2", "1");
    slow.speed = t("1/2");
    slow.source_in = t("10");
    editor
        .apply(
            "Build",
            vec![
                Command::AddTrack {
                    timeline: root,
                    track: track(v1, TrackKind::Video, "V1"),
                    index: 0,
                },
                Command::AddTrack {
                    timeline: root,
                    track: track(v2, TrackKind::Video, "V2"),
                    index: 1,
                },
                add_clip(root, a, at(v1, "0", "4")),
                add_clip(root, b, slow),
                Command::SetParam {
                    node: a,
                    param: "opacity".into(),
                    value: Some(fade),
                },
            ],
        )
        .unwrap();
    (editor, [root, v1, v2, a, b])
}

#[test]
fn timeline_documents_round_trip_and_match_golden_file() {
    let (editor, _) = sample();
    let text = to_json(editor.doc());
    let loaded = from_json(&text, editor.registry()).unwrap();
    assert_eq!(&loaded, editor.doc());
    assert_eq!(to_json(&loaded), text);
    let golden = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/timeline.json");
    if std::env::var_os("FW_BLESS").is_some() {
        std::fs::write(golden, &text).unwrap();
    }
    assert_eq!(
        text,
        std::fs::read_to_string(golden).expect("run with FW_BLESS=1")
    );
}

#[test]
fn unknown_clips_keep_their_bytes_and_placement() {
    let (editor, [.., a, _]) = sample();
    let text = to_json(editor.doc());
    let without_clip_type = registry_with(vec![graph_type(), timeline_type()]);
    let doc = from_json(&text, &without_clip_type).unwrap();
    assert!(matches!(doc.node(a), Some(Node::Unknown(_))));
    assert_eq!(doc.timeline(doc.root()).unwrap().clips.len(), 2);
    assert_eq!(to_json(&doc), text);
}

#[test]
fn editing_rules() {
    let (mut editor, [root, v1, v2, a, _]) = sample();
    let mut ids = SequentialIds::new(99);
    let c = ids.next_id();
    let refused = |e: &mut Editor, cmd: Command| e.apply("x", vec![cmd]).unwrap_err();

    assert!(matches!(
        refused(&mut editor, add_clip(root, c, at(v1, "3", "2"))),
        CommandError::Placement(_)
    ));
    let mut no_placement = add_clip(root, c, at(v1, "9", "1"));
    if let Command::AddNode { placement, .. } = &mut no_placement {
        *placement = None;
    }
    assert!(matches!(
        refused(&mut editor, no_placement),
        CommandError::Placement(_)
    ));
    assert!(matches!(
        refused(
            &mut editor,
            Command::RemoveTrack {
                timeline: root,
                track: TrackId(v1)
            }
        ),
        CommandError::Track(_)
    ));
    assert!(matches!(
        refused(
            &mut editor,
            Command::Connect {
                edge: Edge {
                    from: PortRef::new(a, "image"),
                    to: PortRef::new(a, "image"),
                },
            }
        ),
        CommandError::WrongContainer { .. } | CommandError::NoSuchPort(_)
    ));

    // Moving a clip, then undoing, restores it exactly.
    let before = editor.doc().clone();
    editor
        .apply(
            "Move",
            vec![Command::SetPlacement {
                node: a,
                placement: at(v2, "5", "1"),
            }],
        )
        .unwrap();
    assert_eq!(
        editor.doc().timeline(root).unwrap().clips[&a],
        at(v2, "5", "1")
    );
    editor.undo().unwrap();
    assert_eq!(editor.doc(), &before);

    // Removing a clip and undoing brings back its placement and parameters.
    editor
        .apply("Remove", vec![Command::RemoveNode { id: a }])
        .unwrap();
    editor.undo().unwrap();
    assert_eq!(editor.doc(), &before);
}

#[derive(Debug, Clone)]
enum Op {
    AddTrack {
        video: bool,
        index: usize,
    },
    RemoveTrack {
        track: usize,
    },
    AddClip {
        track: usize,
        start: i64,
        duration: i64,
    },
    Move {
        clip: usize,
        track: usize,
        start: i64,
        duration: i64,
        slow: bool,
    },
    Remove {
        clip: usize,
    },
    Undo,
    Redo,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (any::<bool>(), any::<usize>()).prop_map(|(video, index)| Op::AddTrack { video, index }),
        1 => any::<usize>().prop_map(|track| Op::RemoveTrack { track }),
        4 => (any::<usize>(), 0..20i64, 1..6i64)
            .prop_map(|(track, start, duration)| Op::AddClip { track, start, duration }),
        3 => (any::<usize>(), any::<usize>(), 0..20i64, 1..6i64, any::<bool>())
            .prop_map(|(clip, track, start, duration, slow)| Op::Move { clip, track, start, duration, slow }),
        1 => any::<usize>().prop_map(|clip| Op::Remove { clip }),
        2 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn timeline_edits_undo_exactly(ops in prop::collection::vec(op(), 1..50)) {
        let mut ids = SequentialIds::new(5);
        let root = ids.next_id();
        let registry = Arc::new(full_registry());
        let mut editor = Editor::new(Document::new_timeline(root, settings()), registry.clone());
        let mut states = vec![editor.doc().clone()];
        let mut position = 0usize;
        for op in &ops {
            let timeline = editor.doc().timeline(root).unwrap().clone();
            let pick_track = |i: usize| timeline.tracks.get(i % timeline.tracks.len().max(1)).map(|tr| tr.id);
            let clips: Vec<NodeId> = timeline.clips.keys().copied().collect();
            let pick_clip = |i: usize| clips.get(i % clips.len().max(1)).copied();
            let secs = |s: i64| Time::from_seconds(s);
            let command = match op {
                Op::Undo => {
                    if editor.undo().unwrap().is_some() { position -= 1; }
                    None
                }
                Op::Redo => {
                    if editor.redo().unwrap().is_some() { position += 1; }
                    None
                }
                Op::AddTrack { video, index } => Some(Command::AddTrack {
                    timeline: root,
                    track: track(ids.next_id(), if *video { TrackKind::Video } else { TrackKind::Audio }, "T"),
                    index: index % (timeline.tracks.len() + 2),
                }),
                Op::RemoveTrack { track } => pick_track(*track).map(|id| Command::RemoveTrack { timeline: root, track: id }),
                Op::AddClip { track, start, duration } => pick_track(*track).map(|tr| {
                    add_clip(root, ids.next_id(), Placement {
                        track: tr, start: secs(*start), duration: secs(*duration),
                        source_in: Time::ZERO, speed: secs(1),
                    })
                }),
                Op::Move { clip, track, start, duration, slow } => pick_clip(*clip).zip(pick_track(*track)).map(|(c, tr)| {
                    Command::SetPlacement { node: c, placement: Placement {
                        track: tr, start: secs(*start), duration: secs(*duration),
                        source_in: secs(1), speed: if *slow { t("1/2") } else { secs(1) },
                    }}
                }),
                Op::Remove { clip } => pick_clip(*clip).map(|c| Command::RemoveNode { id: c }),
            };
            if let Some(command) = command {
                let before = editor.doc().clone();
                let json = serde_json::to_string(&command).unwrap();
                prop_assert_eq!(&serde_json::from_str::<Command>(&json).unwrap(), &command);
                match editor.apply("Edit", vec![command]) {
                    Ok(_) => {
                        states.truncate(position + 1);
                        states.push(editor.doc().clone());
                        position += 1;
                    }
                    Err(_) => prop_assert_eq!(editor.doc(), &before),
                }
            }
            prop_assert_eq!(editor.doc(), &states[position]);
            prop_assert_eq!(editor.doc().check_invariants(), Ok(()));
            let text = to_json(editor.doc());
            prop_assert_eq!(&from_json(&text, &registry).unwrap(), editor.doc());
        }
        while editor.undo().unwrap().is_some() {}
        prop_assert_eq!(editor.doc(), &states[0]);
    }
}
