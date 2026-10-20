//! The UI driven as a user would, through egui's input and its
//! accessibility tree, against a real engine.

use std::path::Path;
use std::sync::Arc;

use egui::{Modifiers, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::{
    Command, Document, Editor, Param, Placement, TimelineSettings, Track, TrackId, TrackKind,
};
use fw_engine::{Engine, Settings};
use fw_plugin::{MediaRef, ParamValue};
use fw_render::Gpu;

use crate::app::App;

struct Project {
    _dir: tempfile::TempDir,
    clip: NodeId,
    engine: Engine,
}

/// A timeline with one two-second clip on V1, opened in an engine.
fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let video = fw_testmedia::count_24fps();
    let clip = write_project(&path, &video);
    let settings = Settings {
        editing_copies: false,
        sound: false,
        ..Settings::default()
    };
    let engine = Engine::open(&path, gpu(), settings).unwrap();
    Project {
        _dir: dir,
        clip,
        engine,
    }
}

/// One device for every test, as the app has: many devices at once can hang
/// or crash the driver.
pub(crate) fn gpu() -> Arc<Gpu> {
    static GPU: std::sync::OnceLock<Arc<Gpu>> = std::sync::OnceLock::new();
    GPU.get_or_init(|| Arc::new(Gpu::new().unwrap())).clone()
}

fn write_project(path: &Path, video: &Path) -> NodeId {
    let mut ids = SequentialIds::new(5);
    let root = ids.next_id();
    let (track, clip) = (TrackId(ids.next_id()), ids.next_id());
    let info = fw_media::probe_video(video).unwrap();
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
                    name: Some("Counting".into()),
                    placement: Some(Placement {
                        track,
                        start: Time::ZERO,
                        duration: Time::from_seconds(2),
                        source_in: Time::ZERO,
                        speed: Time::from_seconds(1),
                    }),
                },
                Command::SetParam {
                    node: clip,
                    param: "file".into(),
                    value: Some(Param::Constant(ParamValue::Media(MediaRef {
                        path: video.display().to_string(),
                    }))),
                },
            ],
        )
        .unwrap();
    fw_doc::save::save(editor.doc(), path).unwrap();
    clip
}

pub(crate) fn harness(engine: Engine) -> Harness<'static, (App, Engine)> {
    let app = App::new(&engine);
    Harness::builder()
        .with_size(Vec2::new(1600.0, 1000.0))
        .with_context_setup(crate::style::install)
        .build_ui_state(
            |ui, (app, engine): &mut (App, Engine)| app.ui(ui, engine),
            (app, engine),
        )
}

/// Presses at `from`, moves to `to` in steps, and lets go.
pub(crate) fn drag(harness: &mut Harness<'_, (App, Engine)>, from: Pos2, to: Pos2) {
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for i in 1..=8 {
        harness.hover_at(from + (to - from) * (i as f32 / 8.0));
        harness.step();
    }
    harness.drop_at(to);
    harness.step();
}

fn opacity(engine: &Engine, clip: NodeId) -> Option<f64> {
    match engine
        .document()
        .node(clip)?
        .as_known()?
        .params
        .get("opacity")
    {
        Some(Param::Constant(ParamValue::Float(v))) => Some(*v),
        None => Some(100.0),
        _ => None,
    }
}

fn select_clip(harness: &mut Harness<'_, (App, Engine)>) {
    harness.get_by_label("Clip Counting").click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Opacity 100.0%").is_some(),
        "the inspector shows the clip's settings"
    );
}

#[test]
fn dragging_a_value_is_one_undo_step() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    select_clip(&mut harness);

    let field = harness.get_by_label("Opacity 100.0%").rect().center();
    drag(&mut harness, field, field - egui::vec2(60.0, 0.0));
    let engine = &harness.state().1;
    let dragged = opacity(engine, clip).unwrap();
    assert!(dragged < 90.0, "opacity went down to {dragged}");
    assert_eq!(engine.undo_label(), Some("Opacity"));

    harness.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
    let engine = &harness.state().1;
    assert_eq!(
        opacity(engine, clip),
        Some(100.0),
        "one undo takes back the drag"
    );
    assert_eq!(engine.undo_label(), None, "nothing left to undo");
}

#[test]
fn typing_a_value_sets_it() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    select_clip(&mut harness);

    harness.get_by_label("Opacity 100.0%").click();
    harness.run_steps(2);
    let field = harness.get_by_role(egui::accesskit::Role::TextInput);
    field.type_text("25");
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert_eq!(opacity(&harness.state().1, clip), Some(25.0));
}

#[test]
fn dragging_a_clip_moves_it_by_whole_frames() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let start = harness.get_by_label("Clip Counting").rect().center();
    drag(&mut harness, start, start + egui::vec2(150.0, 0.0));
    let engine = &harness.state().1;
    let doc = engine.document();
    let placement = &doc.timeline(doc.root()).unwrap().clips[&clip];
    assert!(placement.start > Time::ZERO, "the clip moved right");
    let frames = placement.start.as_f64() * 24.0;
    assert_eq!(frames.fract(), 0.0, "to a whole frame, not {frames}");
    assert_eq!(engine.undo_label(), Some("Move Clip"));

    harness.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
    let doc = harness.state().1.document();
    assert_eq!(
        doc.timeline(doc.root()).unwrap().clips[&clip].start,
        Time::ZERO
    );
}

#[test]
fn clicking_the_ruler_moves_the_playhead() {
    let Project { _dir, engine, .. } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let clip = harness.get_by_label("Clip Counting").rect();
    // Halfway along the clip, in the ruler above it.
    let at = egui::pos2(clip.center().x, clip.top() - 16.0);
    harness.hover_at(at);
    harness.step();
    harness.drag_at(at);
    harness.step();
    harness.drop_at(at);
    harness.run_steps(2);
    let engine = &harness.state().1;
    let index = engine
        .playhead()
        .frame_index(Time::from_seconds(24))
        .unwrap();
    assert!(
        (20..=28).contains(&index),
        "the playhead is near 1 s, at frame {index}"
    );
}

#[test]
fn delete_removes_the_selected_clip_and_undo_brings_it_back() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    select_clip(&mut harness);
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    assert!(!harness.state().1.document().contains(clip));
    assert!(harness.query_by_label("Clip Counting").is_none());
    harness.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
    assert!(harness.state().1.document().contains(clip));
}

#[test]
fn space_plays_and_pauses() {
    let Project { _dir, engine, .. } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    harness.key_press(egui::Key::Space);
    harness.run_steps(1);
    assert!(harness.state().1.is_playing());
    std::thread::sleep(std::time::Duration::from_millis(300));
    harness.key_press(egui::Key::Space);
    harness.run_steps(1);
    let engine = &harness.state().1;
    assert!(!engine.is_playing());
    assert!(
        engine.playhead() > Time::ZERO,
        "the playhead moved while playing"
    );
}

fn clip_graph(engine: &Engine, clip: NodeId) -> Option<usize> {
    engine.document().graph(clip).map(|g| g.nodes.len())
}

#[test]
fn open_in_compose_from_the_clip_menu_and_back() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    let at = harness.get_by_label("Clip Counting").rect().center();
    harness.hover_at(at);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.step();
    }
    harness.run_steps(2);
    harness.get_by_label_contains("Open in Compose").click();
    harness.run_steps(3);
    let engine = &harness.state().1;
    assert_eq!(clip_graph(engine, clip), Some(2), "Input and Output");
    assert_eq!(engine.undo_label(), Some("Create Clip Graph"));
    assert!(harness.query_by_label("Node Input").is_some());
    assert!(harness.query_by_label("Node Output").is_some());
    assert!(
        harness.query_by_label("Clip Counting").is_none(),
        "the timeline isn't on the Compose page"
    );

    // The graph passes the media through, so the viewer still shows a frame.
    let engine = &mut harness.state_mut().1;
    assert!(engine.wait_for_frame(std::time::Duration::from_secs(20)));
    assert_eq!(engine.failure(), None);

    harness.get_by_label("← Main Timeline").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Clip Counting").is_some());
    assert!(harness.query_by_label("Node Input").is_none());

    // Opening again steps into the same graph without another edit.
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(harness.query_by_label("Node Input").is_some());
    assert_eq!(harness.state().1.undo_label(), Some("Create Clip Graph"));
}

#[test]
fn undo_while_composing_returns_to_the_timeline() {
    let Project { _dir, clip, engine } = project();
    let mut harness = harness(engine);
    harness.run_steps(2);
    harness.get_by_label("Compose").click();
    harness.run_steps(2);
    assert_eq!(
        clip_graph(&harness.state().1, clip),
        None,
        "nothing to open without a selected clip"
    );
    select_clip(&mut harness);
    harness.get_by_label("Compose").click();
    harness.run_steps(2);
    assert_eq!(clip_graph(&harness.state().1, clip), Some(2));
    assert!(harness.query_by_label("Node Input").is_some());

    harness.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(3);
    assert_eq!(clip_graph(&harness.state().1, clip), None);
    assert!(
        harness.query_by_label("Clip Counting").is_some(),
        "back on the Edit page"
    );
}
