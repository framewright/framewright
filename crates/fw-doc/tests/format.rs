//! Saving and loading.

mod common;

use std::io;
use std::sync::Arc;

use common::*;
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::format::{LoadError, from_json, to_json};
use fw_doc::save::{self, SaveStep, write_atomic_with};
use fw_doc::*;
use fw_plugin::{MediaRef, NodeType, ParamDesc, ParamValue, Registry, Unit};

fn port(node: NodeId, port: &str) -> PortRef {
    PortRef::new(node, port)
}

fn connect(from: NodeId, from_port: &str, to: NodeId, to_port: &str) -> Command {
    Command::Connect {
        edge: Edge {
            from: port(from, from_port),
            to: port(to, to_port),
        },
    }
}

fn add(parent: NodeId, id: NodeId, type_id: &str) -> Command {
    Command::AddNode {
        parent,
        id,
        type_id: type_id.into(),
        name: None,
        placement: None,
    }
}

/// source → gain (animated) → group { gain } → sink, built with predictable ids.
fn sample(registry: Registry) -> Editor {
    let mut ids = SequentialIds::new(1);
    let root = ids.next_id();
    let [source, gain, group, inner, sink] = [(); 5].map(|_| ids.next_id());
    let mut editor = Editor::new(Document::new(root), Arc::new(registry));
    let keys = Param::Keyframed(Keyframes {
        keyframes: vec![
            Keyframe {
                t: Time::ZERO,
                v: ParamValue::Float(0.0),
                interp: Interp::Linear,
                anchor: Anchor::Start,
            },
            Keyframe {
                t: "3/2".parse().unwrap(),
                v: ParamValue::Float(1.5),
                interp: Interp::Hold,
                anchor: Anchor::Start,
            },
        ],
    });
    editor
        .apply(
            "Build sample",
            vec![
                add(root, source, "test.source"),
                Command::SetParam {
                    node: source,
                    param: "file".into(),
                    value: Some(Param::Constant(ParamValue::Media(MediaRef {
                        path: "media/cliffs.png".into(),
                    }))),
                },
                add(root, gain, "test.gain"),
                Command::SetParam {
                    node: gain,
                    param: "gain".into(),
                    value: Some(keys),
                },
                add(root, group, "test.group"),
                add(group, inner, "test.gain"),
                Command::SetParam {
                    node: inner,
                    param: "mode".into(),
                    value: Some(Param::Constant(ParamValue::Text("add".into()))),
                },
                add(root, sink, "test.sink"),
                connect(source, "image", gain, "image"),
                connect(gain, "image", group, "image"),
                connect(group, "image", sink, "image"),
                Command::Rename {
                    node: sink,
                    name: "Output".into(),
                },
                Command::SetPosition {
                    node: source,
                    position: Some([0.0, 40.5]),
                },
                Command::SetPosition {
                    node: inner,
                    position: Some([-120.0, 0.0]),
                },
            ],
        )
        .unwrap();
    editor
}

#[test]
fn save_load_save_is_byte_identical() {
    let editor = sample(full_registry());
    let text = to_json(editor.doc());
    let loaded = from_json(&text, editor.registry()).unwrap();
    assert_eq!(&loaded, editor.doc());
    assert_eq!(to_json(&loaded), text);
}

/// The sample document as saved. Rewrite it with `FW_BLESS=1` after a deliberate format change.
#[test]
fn saved_form_matches_golden_file() {
    let text = to_json(sample(full_registry()).doc());
    let golden = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.json");
    if std::env::var_os("FW_BLESS").is_some() {
        std::fs::write(golden, &text).unwrap();
    }
    let expected =
        std::fs::read_to_string(golden).expect("run with FW_BLESS=1 to create the golden file");
    assert_eq!(text, expected);
}

#[test]
fn unknown_nodes_round_trip_byte_for_byte() {
    // Written by a version that has test.group, read by one that doesn't.
    let text = to_json(sample(full_registry()).doc());
    let reader = registry_with(vec![graph_type(), source_type(), gain_type(), sink_type()]);
    let doc = from_json(&text, &reader).unwrap();
    assert_eq!(to_json(&doc), text);

    let group = doc
        .nodes()
        .find(|(_, n)| n.type_id() == "test.group")
        .map(|(id, _)| id)
        .unwrap();
    assert!(matches!(doc.node(group), Some(Node::Unknown(_))));
    // The group's contents travel inside it rather than becoming loose nodes.
    assert_eq!(doc.len(), 5);

    // Edits that would change it are refused; editing around it is fine.
    let mut editor = Editor::new(doc, Arc::new(reader));
    let refused = editor.apply(
        "Rename",
        vec![Command::Rename {
            node: group,
            name: "x".into(),
        }],
    );
    assert_eq!(refused, Err(CommandError::ReadOnly(group)));
    let sink = editor
        .doc()
        .nodes()
        .find(|(_, n)| n.name() == "Output")
        .map(|(id, _)| id)
        .unwrap();
    editor
        .apply(
            "Disconnect",
            vec![Command::Disconnect {
                input: port(sink, "image"),
            }],
        )
        .unwrap();
    editor.undo().unwrap();
    assert_eq!(to_json(editor.doc()), text);
}

#[test]
fn newer_node_versions_are_kept_as_unknown() {
    // Pretend a newer Framewright saved the top-level gain node as version 2.
    let text = to_json(sample(full_registry()).doc());
    let v1 = "\"type\": \"test.gain\",\n            \"version\": 1";
    let v2 = "\"type\": \"test.gain\",\n            \"version\": 2";
    assert_eq!(text.matches(v1).count(), 1);
    let newer = text.replace(v1, v2);

    let doc = from_json(&newer, &full_registry()).unwrap();
    let kinds: Vec<bool> = doc
        .nodes()
        .filter(|(_, n)| n.type_id() == "test.gain")
        .map(|(_, n)| matches!(n, Node::Unknown(_)))
        .collect();
    assert_eq!(kinds, [true, false]);
    assert_eq!(to_json(&doc), newer);
}

#[test]
fn older_node_versions_are_migrated() {
    let text = to_json(sample(full_registry()).doc());
    let mut v2 = gain_type();
    v2.version = 2;
    v2.params[0] = ParamDesc::float("stops", "Stops", Unit::Stops, 0.0);
    let migrate: fw_plugin::MigrateFn = |from, params| {
        assert_eq!(from, 1);
        if let Some(v) = params.remove("gain") {
            params.insert("stops".into(), v);
        }
        Ok(())
    };
    let mut reader = registry_with(vec![graph_type(), source_type(), sink_type(), group_type()]);
    reader
        .register(NodeType::new(v2).with_migration(migrate))
        .unwrap();
    let doc = from_json(&text, &reader).unwrap();
    let gain = doc
        .nodes()
        .filter_map(|(_, n)| n.as_known())
        .find(|n| n.type_id == "test.gain" && n.params.contains_key("stops"))
        .unwrap();
    assert_eq!(gain.version, 2);

    let mut no_migration = gain_type();
    no_migration.version = 2;
    let reader = registry_with(vec![
        graph_type(),
        source_type(),
        no_migration,
        sink_type(),
        group_type(),
    ]);
    assert!(matches!(
        from_json(&text, &reader),
        Err(LoadError::Node { .. })
    ));
}

#[test]
fn rejects_bad_files() {
    let registry = full_registry();
    let text = to_json(sample(full_registry()).doc());
    let wrong_format = text.replace("framewright.project", "something.else");
    assert!(matches!(
        from_json(&wrong_format, &registry),
        Err(LoadError::WrongFormat(_))
    ));
    let newer = text.replace("\"format_version\": 1", "\"format_version\": 99");
    assert!(matches!(
        from_json(&newer, &registry),
        Err(LoadError::NewerFormat(99))
    ));
    let bad_value = text.replace("\"mode\": \"add\"", "\"mode\": \"divide\"");
    assert!(matches!(
        from_json(&bad_value, &registry),
        Err(LoadError::Node { .. })
    ));
    assert!(matches!(from_json("{", &registry), Err(LoadError::Json(_))));
}

#[test]
fn integers_load_as_floats_where_the_schema_says_so() {
    let mut ids = SequentialIds::new(9);
    let root = ids.next_id();
    let gain = ids.next_id();
    let mut editor = Editor::new(Document::new(root), Arc::new(full_registry()));
    editor
        .apply(
            "Add",
            vec![
                add(root, gain, "test.gain"),
                Command::SetParam {
                    node: gain,
                    param: "gain".into(),
                    value: Some(Param::Constant(ParamValue::Float(2.0))),
                },
            ],
        )
        .unwrap();
    let text = to_json(editor.doc());
    assert!(text.contains("\"gain\": 2.0"));
    let hand_edited = text.replace("\"gain\": 2.0", "\"gain\": 2");
    assert_eq!(
        &from_json(&hand_edited, editor.registry()).unwrap(),
        editor.doc()
    );
}

#[test]
fn interrupted_saves_leave_the_old_file_or_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    for step in SaveStep::ALL {
        std::fs::write(&path, "old").unwrap();
        let result = write_atomic_with(&path, b"new", |s| {
            if s == step {
                Err(io::Error::other("power cut"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        let content = std::fs::read_to_string(&path).unwrap();
        let expected = if step == SaveStep::SyncDir {
            "new"
        } else {
            "old"
        };
        assert_eq!(content, expected, "failing before {step:?}");
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            leftovers, 1,
            "temporary file left behind after failing before {step:?}"
        );
    }
}

#[test]
fn save_and_load_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let editor = sample(full_registry());
    save::save(editor.doc(), &path).unwrap();
    assert_eq!(&save::load(&path, editor.registry()).unwrap(), editor.doc());
}

#[test]
fn restoring_an_unknown_node_survives_serialization() {
    // The path a crash-recovery journal takes: an inverse holding unknown bytes,
    // written out as JSON and read back.
    let text = to_json(sample(full_registry()).doc());
    let reader = registry_with(vec![graph_type(), source_type(), gain_type(), sink_type()]);
    let mut doc = from_json(&text, &reader).unwrap();
    let group = doc
        .nodes()
        .find(|(_, n)| n.type_id() == "test.group")
        .map(|(id, _)| id)
        .unwrap();
    let inverse = Command::RemoveNode { id: group }
        .apply(&mut doc, &reader)
        .unwrap();
    let journal = serde_json::to_string(&inverse).unwrap();
    let replayed: Command = serde_json::from_str(&journal).unwrap();
    assert_eq!(replayed, inverse);
    replayed.apply(&mut doc, &reader).unwrap();
    assert_eq!(to_json(&doc), text);
}
