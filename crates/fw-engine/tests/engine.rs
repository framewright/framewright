//! The engine driven through requests, as the UI drives it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fw_core::id::{IdGen, SequentialIds};
use fw_core::{NodeId, Time};
use fw_doc::{
    Command, Document, Editor, Param, Placement, TimelineSettings, Track, TrackId, TrackKind,
};
use fw_engine::{Engine, Event, Request, Settings};
use fw_plugin::{MediaRef, ParamValue};
use fw_render::Gpu;

/// A project file holding a timeline with one clip of `video`. Returns the
/// clip's id.
fn write_project(path: &Path, video: &Path) -> NodeId {
    let mut ids = SequentialIds::new(9);
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
                    name: None,
                    placement: Some(Placement {
                        track,
                        start: Time::ZERO,
                        duration: Time::from_seconds(2),
                        source_in: Time::ZERO,
                        speed: Time::from_seconds(1),
                    }),
                },
                opacity(clip, 100.0),
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

fn opacity(clip: NodeId, value: f64) -> Command {
    Command::SetParam {
        node: clip,
        param: "opacity".into(),
        value: Some(Param::Constant(ParamValue::Float(value))),
    }
}

fn open(path: &Path) -> Engine {
    let gpu = Arc::new(Gpu::new().unwrap());
    let settings = Settings {
        editing_copies: false,
        sound: false,
        ..Settings::default()
    };
    Engine::open(path, gpu, settings).unwrap()
}

fn opacity_of(engine: &Engine, clip: NodeId) -> Option<Param> {
    let node = engine.document().node(clip)?.as_known()?;
    node.params.get("opacity").cloned()
}

#[test]
fn edits_through_requests_re_render_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let clip = write_project(&path, &fw_testmedia::h264());
    let mut engine = open(&path);
    assert!(
        engine.wait_for_frame(Duration::from_secs(20)),
        "the first frame"
    );
    let first = engine.viewer_frame().unwrap().serial;

    // A drag: three values in one gesture.
    for value in [80.0, 60.0, 40.0] {
        engine
            .request(Request::Edit {
                label: "Opacity".into(),
                commands: vec![opacity(clip, value)],
                gesture: Some(1),
            })
            .unwrap();
    }
    let events = engine.take_events();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Event::Document(_)));
    assert!(
        !engine.up_to_date(),
        "the frame on screen is from before the edit"
    );
    assert!(engine.wait_for_frame(Duration::from_secs(20)));
    assert!(engine.viewer_frame().unwrap().serial > first);
    assert!(engine.is_modified());

    engine.request(Request::Undo).unwrap();
    assert_eq!(
        opacity_of(&engine, clip),
        Some(Param::Constant(ParamValue::Float(100.0))),
        "one undo takes back the whole drag"
    );
    engine.request(Request::Redo).unwrap();
    engine.request(Request::Save).unwrap();
    assert!(!engine.is_modified());
    assert!(matches!(
        engine.take_events().last(),
        Some(Event::Saved { .. })
    ));
    drop(engine);

    let reopened = open(&path);
    assert_eq!(
        opacity_of(&reopened, clip),
        Some(Param::Constant(ParamValue::Float(40.0)))
    );
}

#[test]
fn seeking_shows_the_frame_landed_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    write_project(&path, &fw_testmedia::count_24fps());
    let mut engine = open(&path);
    for (seconds, frame) in [("1", 24), ("1/2", 12), ("5", 48)] {
        engine
            .request(Request::Seek {
                time: seconds.parse().unwrap(),
            })
            .unwrap();
        assert!(engine.wait_for_frame(Duration::from_secs(20)));
        assert_eq!(engine.failure(), None);
        // Past the end the playhead stops at the end of the last clip.
        assert_eq!(engine.viewer_frame().unwrap().index, frame);
    }
}

#[test]
fn lists_the_media_the_document_uses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let video = fw_testmedia::count_24fps();
    write_project(&path, &video);
    let mut engine = open(&path);
    let media = engine.media();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].path, video.display().to_string());
    assert_eq!(media[0].users, 1);
    let picture = media[0].picture.as_ref().expect("readable");
    assert_eq!((picture.width, picture.height), (64, 32));
    assert_eq!(picture.rate, Some(Time::from_seconds(24)));
}

#[test]
fn requests_and_events_are_plain_data() {
    let request = Request::Edit {
        label: "Opacity".into(),
        commands: vec![opacity(SequentialIds::new(1).next_id(), 50.0)],
        gesture: Some(3),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    let seek: Request = serde_json::from_str(r#"{"request":"seek","time":"3/2"}"#).unwrap();
    assert_eq!(
        seek,
        Request::Seek {
            time: "3/2".parse().unwrap()
        }
    );
}

/// Asks for frames until `done` or `seconds` pass; true if it was done.
fn until(engine: &mut Engine, seconds: u64, mut done: impl FnMut(&mut Engine) -> bool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    while std::time::Instant::now() < end {
        engine.viewer_frame();
        if done(engine) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn plays_from_the_playhead_and_stops_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    write_project(&path, &fw_testmedia::count_24fps());
    let mut engine = open(&path);
    engine
        .request(Request::Seek {
            time: "3/2".parse().unwrap(),
        })
        .unwrap();
    assert!(engine.wait_for_frame(Duration::from_secs(20)));
    engine.request(Request::Play).unwrap();
    assert!(engine.is_playing());
    assert_eq!(
        engine.take_events(),
        [Event::Transport {
            playing: true,
            time: "3/2".parse().unwrap()
        }]
    );
    // The clip ends at 2 s: playing stops on its last frame.
    assert!(
        until(&mut engine, 10, |e| !e.is_playing()),
        "stopped by itself"
    );
    let last = Time::of_frame(47, Time::from_seconds(24)).unwrap();
    assert_eq!(engine.playhead(), last);
    assert_eq!(
        engine.take_events(),
        [Event::Transport {
            playing: false,
            time: last
        }]
    );
    assert!(engine.wait_for_frame(Duration::from_secs(20)));
    assert_eq!(engine.viewer_frame().unwrap().index, 47);
    let status = engine.playback();
    assert!(
        status.shown >= 5,
        "frames were shown while playing: {status:?}"
    );

    // Playing again from the end starts over.
    engine.request(Request::Play).unwrap();
    assert!(engine.playhead() < Time::from_seconds(1));
    engine.request(Request::Pause).unwrap();
    let at = engine.playhead();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(engine.playhead(), at, "paused holds still");
    assert_eq!(
        at,
        Time::of_frame(
            at.frame_index(Time::from_seconds(24)).unwrap(),
            Time::from_seconds(24)
        )
        .unwrap(),
        "on a frame's start"
    );
}

#[test]
fn the_frame_cache_keeps_frames_an_edit_and_its_undo_leave_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let clip = write_project(&path, &fw_testmedia::count_24fps());
    let mut engine = open(&path);
    // Every frame, by seeking: playing under load may drop some.
    for frame in 0..48 {
        let time = Time::of_frame(frame, Time::from_seconds(24)).unwrap();
        engine.request(Request::Seek { time }).unwrap();
        assert!(engine.wait_for_frame(Duration::from_secs(20)));
    }
    let cached = |e: &Engine| e.cached_frames().iter().map(|(a, b)| b - a).sum::<i64>();
    assert!(
        until(&mut engine, 10, |e| cached(e) >= 40),
        "shown frames are cached: {:?}",
        engine.cached_frames()
    );

    engine
        .request(Request::Edit {
            label: "Opacity".into(),
            commands: vec![opacity(clip, 50.0)],
            gesture: None,
        })
        .unwrap();
    assert!(
        until(&mut engine, 10, |e| cached(e) <= 1),
        "every frame changed: {:?}",
        engine.cached_frames()
    );
    engine.request(Request::Undo).unwrap();
    assert!(
        until(&mut engine, 10, |e| cached(e) >= 40),
        "after undo the old frames count again: {:?}",
        engine.cached_frames()
    );
}

const TINT: &str = r#"
id = "test.tint"
version = 1
name = "Tint"
category = "Color"

[[params]]
id = "amount"
name = "Amount"
type = "float"
default = 1
"#;

/// A plugin folder holding Tint with `shader`.
fn write_tint(folder: &Path, shader: &str) {
    let plugin = folder.join("tint");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.toml"), TINT).unwrap();
    std::fs::write(plugin.join("shader.wgsl"), shader).unwrap();
}

#[test]
fn reloading_plugins_renders_with_the_new_shader() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    write_tint(&plugins, "return vec4<f32>(c.rgb * amount, c.a);");
    let path = dir.path().join("project.json");
    let clip = write_project(&path, &fw_testmedia::count_24fps());

    // Put Tint inside the clip's graph.
    let (registry, problems) = fw_ops::registry(std::slice::from_ref(&plugins));
    assert!(problems.is_empty(), "{problems:?}");
    let mut editor = Editor::new(
        fw_doc::save::load(&path, &registry).unwrap(),
        Arc::new(registry),
    );
    let (input, output) = fw_doc::clip_graph_ids(clip);
    let tint = SequentialIds::new(77).next_id();
    let connect = |from, to| Command::Connect {
        edge: fw_doc::Edge {
            from: fw_doc::PortRef::new(from, "image"),
            to: fw_doc::PortRef::new(to, "image"),
        },
    };
    editor
        .apply(
            "Tint",
            vec![
                Command::AddNode {
                    parent: clip,
                    id: tint,
                    type_id: "test.tint".into(),
                    name: None,
                    placement: None,
                },
                connect(input, tint),
                connect(tint, output),
            ],
        )
        .unwrap();
    fw_doc::save::save(editor.doc(), &path).unwrap();

    let settings = Settings {
        editing_copies: false,
        sound: false,
        plugin_folders: vec![plugins.clone()],
        ..Settings::default()
    };
    let mut engine = Engine::open(&path, Arc::new(Gpu::new().unwrap()), settings).unwrap();
    for frame in 0..12 {
        let time = Time::of_frame(frame, Time::from_seconds(24)).unwrap();
        engine.request(Request::Seek { time }).unwrap();
        assert!(engine.wait_for_frame(Duration::from_secs(20)));
    }
    assert_eq!(engine.failure(), None);
    let cached = |e: &Engine| e.cached_frames().iter().map(|(a, b)| b - a).sum::<i64>();
    assert!(until(&mut engine, 10, |e| cached(e) >= 12));

    // Reading the same files again leaves every frame in the cache.
    engine.request(Request::ReloadPlugins).unwrap();
    assert!(
        until(&mut engine, 10, |e| cached(e) >= 12),
        "an unchanged shader keeps its frames: {:?}",
        engine.cached_frames()
    );

    // A new shader renders anew.
    write_tint(&plugins, "return vec4<f32>(c.rgb * amount * 0.5, c.a);");
    let shown = engine.viewer_frame().unwrap().serial;
    engine.request(Request::ReloadPlugins).unwrap();
    assert!(
        engine
            .take_events()
            .contains(&Event::PluginsReloaded { problems: vec![] })
    );
    assert!(until(&mut engine, 10, |e| cached(e) <= 1));
    assert!(until(&mut engine, 20, |e| e
        .viewer_frame()
        .is_some_and(|f| f.serial > shown)));

    // A broken one says why, and the frame says what failed.
    write_tint(&plugins, "return nonsense;");
    engine.request(Request::ReloadPlugins).unwrap();
    let events = engine.take_events();
    let Some(Event::PluginsReloaded { problems }) = events.last() else {
        panic!("{events:?}");
    };
    assert!(problems[0].contains("nonsense"), "{problems:?}");
    assert!(until(&mut engine, 20, |e| e.failure().is_some()));
    assert!(
        engine.failure().unwrap().contains("shader"),
        "{:?}",
        engine.failure()
    );
}

fn quiet() -> Settings {
    Settings {
        editing_copies: false,
        sound: false,
        ..Settings::default()
    }
}

#[test]
fn a_new_project_takes_imported_media_and_saves_as_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("first/project.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let settings = TimelineSettings {
        width: 320,
        height: 180,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    fw_engine::create_project(&path, settings).unwrap();
    let gpu = Arc::new(Gpu::new().unwrap());
    let mut engine = Engine::open(&path, gpu, quiet()).unwrap();
    let doc = engine.document();
    let tracks = &doc.timeline(doc.root()).unwrap().tracks;
    assert_eq!(tracks.len(), 2, "a video track and an audio track");

    // Media beside the project is referred to relatively.
    let video = path.parent().unwrap().join("count.mp4");
    std::fs::copy(fw_testmedia::count_24fps(), &video).unwrap();
    let reference = engine.media_reference(&video);
    assert_eq!(reference, "count.mp4");
    engine
        .request(Request::Edit {
            label: "Import".into(),
            commands: vec![Command::AddMedia {
                path: reference.clone(),
            }],
            gesture: None,
        })
        .unwrap();
    let media = engine.media().to_vec();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].users, 0, "imported, not used yet");
    assert_eq!(media[0].duration, Some(Time::from_seconds(2)));
    assert_eq!(media[0].error, None);

    // Its thumbnail arrives from the thumbnail thread.
    assert_eq!(engine.thumbnail(&reference), None);
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while engine.thumbnails_pending() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let thumbnail = engine.thumbnail(&reference).expect("a thumbnail");
    let picture = media[0].picture.as_ref().unwrap();
    assert_eq!(
        thumbnail.width,
        picture.width.min(fw_engine::THUMBNAIL_WIDTH),
        "small media isn't enlarged"
    );
    assert_eq!(
        thumbnail.rgba.len(),
        (thumbnail.width * thumbnail.height * 4) as usize
    );

    // Saving in the same folder keeps the undo history.
    let renamed = path.parent().unwrap().join("renamed.json");
    engine
        .request(Request::SaveAs {
            path: renamed.clone(),
        })
        .unwrap();
    assert_eq!(engine.path(), renamed);
    assert_eq!(engine.undo_label(), Some("Import"));

    // Elsewhere, the media path is rewritten to still find the file.
    let moved = dir.path().join("second/deeper/project.json");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    engine
        .request(Request::SaveAs {
            path: moved.clone(),
        })
        .unwrap();
    assert_eq!(engine.path(), moved);
    let pool: Vec<String> = engine.document().media_pool().map(String::from).collect();
    assert_eq!(pool, ["../../first/count.mp4"]);
    assert_eq!(engine.media()[0].error, None);
    assert!(!engine.is_modified());
}
