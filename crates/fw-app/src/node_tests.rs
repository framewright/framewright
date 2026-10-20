//! The node editor driven as a user would, against a real engine.

use std::path::Path;
use std::sync::Arc;

use egui::{Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fw_core::NodeId;
use fw_core::id::{IdGen, SequentialIds};
use fw_doc::{Command, Document, Edge, Editor, Param, PortRef};
use fw_engine::{Engine, Settings};
use fw_plugin::{MediaRef, ParamValue};

use crate::app::App;
use crate::tests::{drag, gpu, harness};

struct Project {
    _dir: tempfile::TempDir,
    read: NodeId,
    exposure: NodeId,
    output: NodeId,
    engine: Engine,
}

/// Read Image → Exposure → Output, with no saved positions.
fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    write_png(&dir.path().join("gray.png"));
    let mut ids = SequentialIds::new(3);
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
    editor
        .apply(
            "Build",
            vec![
                add(read, "fw.read_image"),
                Command::SetParam {
                    node: read,
                    param: "file".into(),
                    value: Some(Param::Constant(ParamValue::Media(MediaRef {
                        path: "gray.png".into(),
                    }))),
                },
                add(exposure, "fw.exposure"),
                add(output, "fw.output"),
                connect(read, exposure),
                connect(exposure, output),
            ],
        )
        .unwrap();
    let path = dir.path().join("graph.json");
    fw_doc::save::save(editor.doc(), &path).unwrap();
    let settings = Settings {
        editing_copies: false,
        sound: false,
        ..Settings::default()
    };
    let engine = Engine::open(&path, gpu(), settings).unwrap();
    Project {
        _dir: dir,
        read,
        exposure,
        output,
        engine,
    }
}

fn connect(from: NodeId, to: NodeId) -> Command {
    Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    }
}

fn write_png(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, 8, 8);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[128; 8 * 8 * 4]).unwrap();
}

fn source_of(engine: &Engine, node: NodeId) -> Option<NodeId> {
    engine
        .document()
        .source_of(&PortRef::new(node, "image"))
        .map(|p| p.node)
}

fn position(engine: &Engine, node: NodeId) -> Option<[f64; 2]> {
    let doc = engine.document();
    doc.graph(doc.root())?.positions.get(&node).copied()
}

fn undo(harness: &mut Harness<'_, (App, Engine)>) {
    harness.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
}

fn center(harness: &Harness<'_, (App, Engine)>, label: &str) -> egui::Pos2 {
    harness.get_by_label(label).rect().center()
}

#[test]
fn shows_the_graph_and_dragging_a_node_moves_it_in_one_step() {
    let Project {
        _dir,
        exposure,
        engine,
        ..
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    for name in ["Read Image", "Exposure", "Output"] {
        assert!(harness.query_by_label(&format!("Node {name}")).is_some());
    }
    let from = center(&harness, "Node Exposure");
    drag(&mut harness, from, from + vec2(40.0, 60.0));
    let engine = &harness.state().1;
    let [x, y] = position(engine, exposure).expect("the move was saved");
    assert_eq!((x.fract(), y.fract()), (0.0, 0.0), "to whole units");
    assert_eq!(engine.undo_label(), Some("Move Node"));
    assert!(
        harness.query_by_label("Exposure 0.00 EV").is_some(),
        "the dragged node is in the inspector"
    );
    let moved = center(&harness, "Node Exposure");
    assert!(
        (moved - from - vec2(40.0, 60.0)).length() < 2.0,
        "it followed the pointer"
    );

    undo(&mut harness);
    let engine = &harness.state().1;
    assert_eq!(
        position(engine, exposure),
        None,
        "one undo takes back the drag"
    );
    assert_eq!(engine.undo_label(), None);
}

#[test]
fn dragging_from_an_output_to_an_input_connects_them() {
    let Project {
        _dir,
        read,
        output,
        engine,
        ..
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let from = center(&harness, "Output Image of Read Image");
    let to = center(&harness, "Input Image of Output");
    drag(&mut harness, from, to);
    let engine = &harness.state().1;
    assert_eq!(
        source_of(engine, output),
        Some(read),
        "the new source replaces the old"
    );
    assert_eq!(engine.undo_label(), Some("Connect"));
}

#[test]
fn dragging_a_connection_off_its_input_disconnects_it() {
    let Project {
        _dir,
        exposure,
        output,
        engine,
        ..
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let from = center(&harness, "Input Image of Output");
    drag(&mut harness, from, from + vec2(0.0, 120.0));
    assert_eq!(source_of(&harness.state().1, output), None);
    assert_eq!(harness.state().1.undo_label(), Some("Disconnect"));
    undo(&mut harness);
    assert_eq!(source_of(&harness.state().1, output), Some(exposure));
}

#[test]
fn a_connection_making_a_loop_is_refused() {
    let Project {
        _dir,
        read,
        exposure,
        engine,
        ..
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let from = center(&harness, "Output Image of Exposure");
    let to = center(&harness, "Input Image of Exposure");
    drag(&mut harness, from, to);
    let engine = &harness.state().1;
    assert_eq!(source_of(engine, exposure), Some(read), "nothing changed");
    assert_eq!(engine.undo_label(), None);
}

#[test]
fn tab_opens_the_library_and_enter_adds_the_match() {
    let Project { _dir, engine, .. } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let below = center(&harness, "Node Exposure") + vec2(0.0, 150.0);
    harness.hover_at(below);
    harness.step();
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    for shader_plugin in ["Add Saturation", "Add Invert", "Add Vignette", "Add Mix"] {
        assert!(
            harness.query_by_label(shader_plugin).is_some(),
            "{shader_plugin} is listed"
        );
    }
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text("expo");
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    let engine = &harness.state().1;
    let doc = engine.document();
    let graph = doc.graph(doc.root()).unwrap();
    assert_eq!(graph.nodes.len(), 4);
    assert_eq!(engine.undo_label(), Some("Add Exposure"));
    let added = graph
        .nodes
        .iter()
        .find(|id| graph.positions.contains_key(id))
        .expect("the new node has a position");
    assert_eq!(doc.node(*added).unwrap().type_id(), "fw.exposure");
    assert!(
        harness.query_by_label("Add Exposure").is_none(),
        "the library closed"
    );
}

#[test]
fn delete_removes_the_selected_nodes() {
    let Project {
        _dir,
        read,
        exposure,
        output,
        engine,
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    harness.get_by_label("Node Exposure").click();
    harness.run_steps(1);
    let read_at = center(&harness, "Node Read Image");
    harness.hover_at(read_at);
    harness.step();
    for pressed in [true, false] {
        let click = egui::Event::PointerButton {
            pos: read_at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::SHIFT,
        };
        harness.event_modifiers(click, Modifiers::SHIFT);
        harness.step();
    }
    harness.run_steps(2);
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    let engine = &harness.state().1;
    assert!(!engine.document().contains(exposure));
    assert!(!engine.document().contains(read));
    assert!(engine.document().contains(output));
    assert_eq!(engine.undo_label(), Some("Delete Nodes"));
    undo(&mut harness);
    assert_eq!(source_of(&harness.state().1, output), Some(exposure));
}

#[test]
fn tab_with_a_node_selected_changes_nothing_but_the_library() {
    let Project {
        _dir,
        exposure,
        engine,
        ..
    } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    harness.get_by_label("Node Exposure").click();
    harness.run_steps(2);
    let below = center(&harness, "Node Exposure") + vec2(0.0, 150.0);
    harness.hover_at(below);
    harness.step();
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Add Exposure").is_some(),
        "the library opened"
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    let engine = &harness.state().1;
    assert_eq!(
        engine.undo_label(),
        None,
        "focus passing through the inspector edited nothing"
    );
    let doc = engine.document();
    assert!(
        doc.node(exposure)
            .unwrap()
            .as_known()
            .unwrap()
            .params
            .is_empty()
    );
    assert!(
        harness.query_by_label("Add Exposure").is_none(),
        "Escape closed the library"
    );
}
