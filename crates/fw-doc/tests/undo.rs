//! Property tests: random edits, undo and redo always restore exact states.

mod common;

use std::sync::Arc;

use common::full_registry;
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::format::{from_json, to_json};
use fw_doc::*;
use fw_plugin::ParamValue;
use proptest::prelude::*;

/// An edit described by indices, resolved against whatever nodes exist when it runs.
#[derive(Debug, Clone)]
enum Op {
    Add {
        parent: usize,
        ty: usize,
    },
    Remove {
        node: usize,
    },
    SetGain {
        node: usize,
        value: f64,
        animated: bool,
    },
    ResetGain {
        node: usize,
    },
    SetMode {
        node: usize,
        add: bool,
    },
    Connect {
        from: usize,
        to: usize,
    },
    Disconnect {
        node: usize,
    },
    Rename {
        node: usize,
        name: String,
    },
    /// Moves a node in the node editor, or clears its position.
    Move {
        node: usize,
        to: Option<(i16, i16)>,
    },
    /// Two edits as one undo step.
    Pair(Box<Op>, Box<Op>),
    Undo,
    Redo,
}

const TYPES: [&str; 5] = [
    "test.source",
    "test.gain",
    "test.mask",
    "test.sink",
    "test.group",
];

fn single_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<usize>(), 0..TYPES.len()).prop_map(|(parent, ty)| Op::Add { parent, ty }),
        1 => any::<usize>().prop_map(|node| Op::Remove { node }),
        2 => (any::<usize>(), -25.0..25.0f64, any::<bool>())
            .prop_map(|(node, value, animated)| Op::SetGain { node, value, animated }),
        1 => any::<usize>().prop_map(|node| Op::ResetGain { node }),
        1 => (any::<usize>(), any::<bool>()).prop_map(|(node, add)| Op::SetMode { node, add }),
        4 => (any::<usize>(), any::<usize>()).prop_map(|(from, to)| Op::Connect { from, to }),
        1 => any::<usize>().prop_map(|node| Op::Disconnect { node }),
        1 => (any::<usize>(), "[a-z ]{0,8}").prop_map(|(node, name)| Op::Rename { node, name }),
        2 => (any::<usize>(), proptest::option::of(any::<(i16, i16)>()))
            .prop_map(|(node, to)| Op::Move { node, to }),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => single_op(),
        1 => (single_op(), single_op()).prop_map(|(a, b)| Op::Pair(Box::new(a), Box::new(b))),
        3 => Just(Op::Undo),
        2 => Just(Op::Redo),
    ]
}

fn pick(doc: &Document, i: usize) -> NodeId {
    let ids: Vec<NodeId> = doc.nodes().map(|(id, _)| id).collect();
    ids[i % ids.len()]
}

/// Picks a node of one of `types`, falling back to any node so error paths still run.
fn pick_typed(doc: &Document, i: usize, types: &[&str]) -> NodeId {
    let ids: Vec<NodeId> = doc
        .nodes()
        .filter(|(_, n)| types.contains(&n.type_id()))
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        pick(doc, i)
    } else {
        ids[i % ids.len()]
    }
}

/// Like [`pick_typed`], but among the siblings of `near`.
fn pick_sibling(doc: &Document, near: NodeId, i: usize, types: &[&str]) -> NodeId {
    let ids: Vec<NodeId> = doc
        .nodes()
        .filter(|(id, n)| doc.parent(*id) == doc.parent(near) && types.contains(&n.type_id()))
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        pick(doc, i)
    } else {
        ids[i % ids.len()]
    }
}

const WITH_OUTPUT: [&str; 3] = ["test.source", "test.gain", "test.group"];
const WITH_INPUT: [&str; 3] = ["test.gain", "test.sink", "test.group"];

fn pick_container(doc: &Document, i: usize) -> NodeId {
    let ids: Vec<NodeId> = doc
        .nodes()
        .filter(|(_, n)| n.graph().is_some())
        .map(|(id, _)| id)
        .collect();
    ids[i % ids.len()]
}

fn commands(op: &Op, doc: &Document, ids: &mut SequentialIds) -> Vec<Command> {
    let image = |node| PortRef::new(node, "image");
    match op {
        Op::Add { parent, ty } => vec![Command::AddNode {
            parent: pick_container(doc, *parent),
            id: ids.next_id(),
            type_id: TYPES[*ty].into(),
            name: None,
            placement: None,
        }],
        Op::Remove { node } => vec![Command::RemoveNode {
            id: pick(doc, *node),
        }],
        Op::SetGain {
            node,
            value,
            animated,
        } => {
            let v = ParamValue::Float(*value);
            let param = if *animated {
                Param::Keyframed(Keyframes {
                    keyframes: vec![
                        Keyframe {
                            t: Time::ZERO,
                            v: ParamValue::Float(0.0),
                            interp: Interp::Linear,
                            anchor: Anchor::Start,
                        },
                        Keyframe {
                            t: Time::from_seconds(2),
                            v,
                            interp: Interp::Hold,
                            anchor: Anchor::Start,
                        },
                    ],
                })
            } else {
                Param::Constant(v)
            };
            vec![Command::SetParam {
                node: pick_typed(doc, *node, &["test.gain"]),
                param: "gain".into(),
                value: Some(param),
            }]
        }
        Op::ResetGain { node } => {
            vec![Command::SetParam {
                node: pick_typed(doc, *node, &["test.gain"]),
                param: "gain".into(),
                value: None,
            }]
        }
        Op::SetMode { node, add } => vec![Command::SetParam {
            node: pick_typed(doc, *node, &["test.gain"]),
            param: "mode".into(),
            value: Some(Param::Constant(ParamValue::Text(
                if *add { "add" } else { "multiply" }.into(),
            ))),
        }],
        Op::Connect { from, to } => {
            let from_node = pick_typed(doc, *from, &WITH_OUTPUT);
            // Mostly valid targets; every fifth pick is anywhere, to exercise the checks.
            let to_node = if to % 5 == 0 {
                pick(doc, *to)
            } else {
                pick_sibling(doc, from_node, *to, &WITH_INPUT)
            };
            vec![Command::Connect {
                edge: Edge {
                    from: image(from_node),
                    to: image(to_node),
                },
            }]
        }
        Op::Disconnect { node } => {
            vec![Command::Disconnect {
                input: image(pick_typed(doc, *node, &WITH_INPUT)),
            }]
        }
        Op::Rename { node, name } => vec![Command::Rename {
            node: pick(doc, *node),
            name: name.clone(),
        }],
        Op::Move { node, to } => vec![Command::SetPosition {
            node: pick(doc, *node),
            position: to.map(|(x, y)| [f64::from(x) / 4.0, f64::from(y)]),
        }],
        Op::Pair(a, b) => {
            // The second edit is resolved against the document after the first,
            // so it can refer to a node the first one added.
            let first = commands(a, doc, ids);
            let mut after = doc.clone();
            for c in &first {
                if c.apply(&mut after, &full_registry()).is_err() {
                    return first;
                }
            }
            first.into_iter().chain(commands(b, &after, ids)).collect()
        }
        Op::Undo | Op::Redo => unreachable!(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn undo_and_redo_restore_exact_states(ops in prop::collection::vec(op(), 1..60)) {
        let registry = Arc::new(full_registry());
        let mut ids = SequentialIds::new(42);
        let mut editor = Editor::new(Document::new(ids.next_id()), registry.clone());

        // The states an exact undo history should move between.
        let mut states = vec![editor.doc().clone()];
        let mut position: usize = 0;

        for op in &ops {
            match op {
                Op::Undo => {
                    let event = editor.undo().unwrap();
                    prop_assert_eq!(event.is_some(), position > 0);
                    position = position.saturating_sub(1);
                }
                Op::Redo => {
                    let event = editor.redo().unwrap();
                    prop_assert_eq!(event.is_some(), position + 1 < states.len());
                    if event.is_some() {
                        position += 1;
                    }
                }
                op => {
                    let before = editor.doc().clone();
                    let commands = commands(op, editor.doc(), &mut ids);
                    for c in &commands {
                        let json = serde_json::to_string(c).unwrap();
                        prop_assert_eq!(&serde_json::from_str::<Command>(&json).unwrap(), c);
                    }
                    match editor.apply("Edit", commands) {
                        Ok(_) => {
                            states.truncate(position + 1);
                            states.push(editor.doc().clone());
                            position += 1;
                        }
                        Err(_) => prop_assert_eq!(editor.doc(), &before, "a failed edit changed the document"),
                    }
                }
            }
            prop_assert_eq!(editor.doc(), &states[position]);
            prop_assert_eq!(editor.doc().check_invariants(), Ok(()));
            let text = to_json(editor.doc());
            let loaded = from_json(&text, &registry).unwrap();
            prop_assert_eq!(&loaded, editor.doc());
            prop_assert_eq!(to_json(&loaded), text);
        }

        while editor.undo().unwrap().is_some() {}
        prop_assert_eq!(editor.doc(), &states[0]);
        while editor.redo().unwrap().is_some() {}
        prop_assert_eq!(editor.doc(), states.last().unwrap());
    }
}

#[test]
fn inverse_of_remove_restores_nested_contents_and_connections() {
    let registry = full_registry();
    let mut ids = SequentialIds::new(3);
    let root = ids.next_id();
    let [source, group, inner, sink] = [(); 4].map(|_| ids.next_id());
    let mut doc = Document::new(root);
    let add = |parent, id, ty: &str| Command::AddNode {
        parent,
        id,
        type_id: ty.into(),
        name: None,
        placement: None,
    };
    let connect = |a, b| Command::Connect {
        edge: Edge {
            from: PortRef::new(a, "image"),
            to: PortRef::new(b, "image"),
        },
    };
    for c in [
        add(root, source, "test.source"),
        add(root, group, "test.group"),
        add(group, inner, "test.gain"),
        add(root, sink, "test.sink"),
        connect(source, group),
        connect(group, sink),
    ] {
        c.apply(&mut doc, &registry).unwrap();
    }
    let before = doc.clone();
    let inverse = Command::RemoveNode { id: group }
        .apply(&mut doc, &registry)
        .unwrap();
    assert!(!doc.contains(inner));
    assert_eq!(doc.source_of(&PortRef::new(sink, "image")), None);
    inverse.apply(&mut doc, &registry).unwrap();
    assert_eq!(doc, before);
}

#[test]
fn connections_are_checked() {
    let registry = full_registry();
    let mut ids = SequentialIds::new(5);
    let root = ids.next_id();
    let [a, b, mask, group, inner] = [(); 5].map(|_| ids.next_id());
    let mut doc = Document::new(root);
    for (parent, id, ty) in [
        (root, a, "test.gain"),
        (root, b, "test.gain"),
        (root, mask, "test.mask"),
        (root, group, "test.group"),
        (group, inner, "test.gain"),
    ] {
        Command::AddNode {
            parent,
            id,
            type_id: ty.into(),
            name: None,
            placement: None,
        }
        .apply(&mut doc, &registry)
        .unwrap();
    }
    let edge = |from: PortRef, to: PortRef| Command::Connect {
        edge: Edge { from, to },
    };
    let p = PortRef::new;
    edge(p(a, "image"), p(b, "image"))
        .apply(&mut doc, &registry)
        .unwrap();
    assert!(matches!(
        edge(p(b, "image"), p(a, "image")).apply(&mut doc, &registry),
        Err(CommandError::Cycle { .. })
    ));
    assert!(matches!(
        edge(p(a, "image"), p(a, "image")).apply(&mut doc, &registry),
        Err(CommandError::Cycle { .. })
    ));
    assert!(matches!(
        edge(p(mask, "mask"), p(b, "image")).apply(&mut doc, &registry),
        Err(CommandError::PortTypeMismatch { .. })
    ));
    assert!(matches!(
        edge(p(a, "image"), p(inner, "image")).apply(&mut doc, &registry),
        Err(CommandError::DifferentContainers(..))
    ));
    assert!(matches!(
        edge(p(a, "nope"), p(b, "image")).apply(&mut doc, &registry),
        Err(CommandError::NoSuchPort(_))
    ));
    assert_eq!(
        Command::RemoveNode { id: root }.apply(&mut doc, &registry),
        Err(CommandError::Root)
    );
}

#[test]
fn a_gesture_is_one_undo_step() {
    let registry = Arc::new(full_registry());
    let mut ids = SequentialIds::new(4);
    let root = ids.next_id();
    let gain = ids.next_id();
    let mut editor = Editor::new(Document::new(root), registry);
    editor
        .apply(
            "Add Gain",
            vec![Command::AddNode {
                parent: root,
                id: gain,
                type_id: "test.gain".into(),
                name: None,
                placement: None,
            }],
        )
        .unwrap();
    let set = |value: f64| {
        vec![Command::SetParam {
            node: gain,
            param: "gain".into(),
            value: Some(Param::Constant(ParamValue::Float(value))),
        }]
    };
    editor.apply("Set Gain", set(0.5)).unwrap();
    let before_drag = editor.doc().clone();

    // A drag: many values in one gesture.
    for i in 1..=20 {
        editor
            .apply_in_gesture("Drag Gain", set(i as f64 / 10.0), Some(7))
            .unwrap();
    }
    let after_drag = editor.doc().clone();
    assert_eq!(editor.undo_label(), Some("Drag Gain"));
    editor.undo().unwrap();
    assert_eq!(
        editor.doc(),
        &before_drag,
        "one undo takes back the whole drag"
    );
    assert_eq!(editor.undo_label(), Some("Set Gain"));
    editor.redo().unwrap();
    assert_eq!(editor.doc(), &after_drag);

    // A redone gesture doesn't swallow a new one with the same id, and
    // another gesture or a plain edit starts a new step.
    editor
        .apply_in_gesture("Drag Gain", set(0.1), Some(7))
        .unwrap();
    editor
        .apply_in_gesture("Drag Gain", set(0.2), Some(8))
        .unwrap();
    editor.apply("Set Gain", set(0.3)).unwrap();
    for expected in [0.2, 0.1, 2.0] {
        editor.undo().unwrap();
        let node = editor.doc().node(gain).unwrap().as_known().unwrap();
        assert_eq!(
            node.params.get("gain"),
            Some(&Param::Constant(ParamValue::Float(expected)))
        );
    }
}
