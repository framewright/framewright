//! The engine, driven block by block as an audio device would.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use fw_audio::{AudioEngine, CHANNELS, Mixer};
use fw_compile::{AudioPlan, compile_audio};
use fw_core::Time;
use fw_core::id::{IdGen, SequentialIds};
use fw_doc::{
    Command, Document, Editor, Param, Placement, TimelineSettings, Track, TrackId, TrackKind,
};
use fw_media::{AudioReader, ProjectMedia};
use fw_plugin::{MediaRef, ParamValue};

// Any allocation inside `assert_no_alloc` aborts the test.
#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48000;
const BLOCK: usize = 512;

/// One audio clip: on the timeline 1 s to 3 s, playing the source from 0.5 s.
fn plan(volume_db: f64) -> AudioPlan {
    let mut ids = SequentialIds::new(9);
    let root = ids.next_id();
    let (a1, clip) = (TrackId(ids.next_id()), ids.next_id());
    let settings = TimelineSettings {
        width: 64,
        height: 32,
        rate: Time::from_seconds(24),
        sample_rate: RATE,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    let set = |param: &str, value| Command::SetParam {
        node: clip,
        param: param.into(),
        value: Some(Param::Constant(value)),
    };
    editor
        .apply(
            "Build",
            vec![
                Command::AddTrack {
                    timeline: root,
                    track: Track {
                        id: a1,
                        kind: TrackKind::Audio,
                        name: "A1".into(),
                    },
                    index: 0,
                },
                Command::AddNode {
                    parent: root,
                    id: clip,
                    type_id: "fw.clip".into(),
                    name: None,
                    placement: Some(Placement {
                        track: a1,
                        start: Time::from_seconds(1),
                        duration: Time::from_seconds(2),
                        source_in: "1/2".parse().unwrap(),
                        speed: Time::from_seconds(1),
                    }),
                },
                set(
                    "file",
                    ParamValue::Media(MediaRef {
                        path: fw_testmedia::h264().display().to_string(),
                    }),
                ),
                set("volume", ParamValue::Float(volume_db)),
            ],
        )
        .unwrap();
    compile_audio(editor.doc(), editor.registry(), &ProjectMedia::new("/")).unwrap()
}

/// Runs the mixer for `frames`, checking it never allocates.
fn run(mixer: &mut Mixer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * CHANNELS];
    for block in out.chunks_mut(BLOCK * CHANNELS) {
        assert_no_alloc(|| mixer.process(block));
    }
    out
}

fn source(from_sample: i64, frames: usize, gain: f32) -> Vec<f32> {
    let mut reader = AudioReader::open(&fw_testmedia::h264(), RATE, None).unwrap();
    reader.seek(from_sample).unwrap();
    let mut out = vec![0.0; frames * CHANNELS];
    let got = reader.read(&mut out).unwrap();
    assert_eq!(got, frames);
    out.iter().map(|v| v * gain).collect()
}

fn assert_close(got: &[f32], want: &[f32], what: &str) {
    let worst = got
        .iter()
        .zip(want)
        .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst < 1e-4, "{what}: off by {worst}");
}

#[test]
fn the_plan_lists_each_clip_once() {
    let plan = plan(-6.0);
    assert_eq!(plan.voices.len(), 1);
    let v = &plan.voices[0];
    assert_eq!((v.start, v.end, v.source_start), (48000, 144000, 24000));
    assert_eq!(v.params, [-6.0]);
}

#[test]
fn plays_the_clip_in_its_place_at_its_volume() {
    let (mut engine, mut mixer) = AudioEngine::new(RATE, BLOCK);
    engine.play(&plan(-6.0), 0).unwrap();
    let out = run(&mut mixer, 3 * RATE as usize + RATE as usize / 2);
    let gain = 10f32.powf(-6.0 / 20.0);
    let second = RATE as usize * CHANNELS;
    assert!(
        out[..second].iter().all(|v| *v == 0.0),
        "silent before the clip"
    );
    assert_close(
        &out[second..3 * second],
        &source(24000, 2 * RATE as usize, gain),
        "the clip",
    );
    assert!(
        out[3 * second..].iter().all(|v| *v == 0.0),
        "silent after the clip"
    );
    assert_eq!(engine.stats().underruns.load(Ordering::Relaxed), 0);
    assert!(engine.take_errors().is_empty());
}

#[test]
fn starting_mid_clip_lands_on_the_right_sample() {
    let (mut engine, mut mixer) = AudioEngine::new(RATE, BLOCK);
    // Start at 2 s: one second into the clip, so 1.5 s into the source.
    engine.play(&plan(0.0), 2 * RATE as i64).unwrap();
    let out = run(&mut mixer, RATE as usize / 2);
    assert_close(&out, &source(72000, RATE as usize / 2, 1.0), "mid-clip");
    let heard = engine.clock().sample_now();
    let written = 2 * RATE as i64 + RATE as i64 / 2;
    assert!(
        (written..written + RATE as i64 / 20).contains(&heard),
        "clock {heard}, written {written}"
    );
}

#[test]
fn a_new_plan_swaps_in_while_playing() {
    let (mut engine, mut mixer) = AudioEngine::new(RATE, BLOCK);
    engine.play(&plan(0.0), RATE as i64).unwrap();
    run(&mut mixer, RATE as usize / 4);
    // Quieter, from where playback has got to.
    let at = RATE as i64 + RATE as i64 / 4;
    engine.play(&plan(-20.0), at).unwrap();
    let out = run(&mut mixer, RATE as usize / 4);
    assert_close(
        &out,
        &source(24000 + RATE as i64 / 4, RATE as usize / 4, 0.1),
        "after the swap",
    );
    engine.stop();
    let silent = run(&mut mixer, BLOCK);
    assert!(silent.iter().all(|v| *v == 0.0));
}
