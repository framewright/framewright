//! `fw-bench`: measures named workloads against performance budgets.
//!
//! Run with `cargo run --release -p fw-bench`. `--check` exits with an error
//! when a budget is missed; `--markdown` prints the budget table for
//! `docs/performance.md`; any other argument picks workloads by name.
//!
//! The budgets below are the source of truth.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fw_audio::{AudioEngine, CHANNELS};
use fw_compile::{MediaResolver, Options, compile, compile_audio};
use fw_core::Time;
use fw_core::id::{IdGen, SequentialIds};
use fw_doc::{
    Command, Document, Editor, Param, Placement, TimelineSettings, Track, TrackId, TrackKind,
};
use fw_media::{ProjectMedia, VideoReader, probe_video};
use fw_playback::{DocumentFrames, FrameSource};
use fw_plugin::{MediaRef, ParamValue};
use fw_render::{DEFAULT_CACHE_BYTES, Gpu, Renderer};

#[derive(Clone, Copy)]
enum Statistic {
    P50,
    P95,
    P99,
}

impl Statistic {
    fn name(self) -> &'static str {
        match self {
            Statistic::P50 => "p50",
            Statistic::P95 => "p95",
            Statistic::P99 => "p99",
        }
    }

    fn of(self, samples: &mut [Duration]) -> Duration {
        samples.sort();
        let q = match self {
            Statistic::P50 => 0.50,
            Statistic::P95 => 0.95,
            Statistic::P99 => 0.99,
        };
        samples[((samples.len() - 1) as f64 * q).round() as usize]
    }
}

struct Budget {
    name: &'static str,
    what: &'static str,
    statistic: Statistic,
    limit: Duration,
    run: fn(&Fixture) -> Vec<Duration>,
}

const fn ms(n: f64) -> Duration {
    Duration::from_nanos((n * 1e6) as u64)
}

/// The budgets. Frame work at 23.976 fps has 41.7 ms per frame in total.
const BUDGETS: &[Budget] = &[
    Budget {
        name: "probe-cached",
        what: "Look up a 1080p clip's probe once it's known (every compile does this)",
        statistic: Statistic::P50,
        limit: ms(0.05),
        run: probe_cached,
    },
    Budget {
        name: "compile-frame",
        what: "Compile one frame of a three-track 1080p timeline",
        statistic: Statistic::P95,
        limit: ms(1.0),
        run: compile_frame,
    },
    Budget {
        name: "decode-next-1080p",
        what: "Decode the next frame of 1080p H.264 to RGBA",
        statistic: Statistic::P95,
        limit: ms(20.0),
        run: decode_next,
    },
    Budget {
        name: "playback-frame-1080p",
        what: "Compile, decode, render and read back the next 1080p frame",
        statistic: Statistic::P95,
        limit: ms(41.7),
        run: playback_frame,
    },
    Budget {
        name: "scrub-1080p",
        what: "Show a random 1080p frame (seek, decode, render)",
        statistic: Statistic::P95,
        limit: ms(150.0),
        run: scrub,
    },
    Budget {
        name: "audio-block-8-voices",
        what: "Mix one 512-frame block of eight streaming voices on the audio thread",
        statistic: Statistic::P99,
        limit: ms(0.5),
        run: audio_block,
    },
];

/// A 1080p timeline with the test clip on three video tracks and eight audio
/// tracks, shared by the workloads.
struct Fixture {
    editor: Editor,
    folder: &'static Path,
    rate: Time,
}

impl Fixture {
    fn new() -> Fixture {
        let clip_path = fw_testmedia::hd_h264();
        let mut ids = SequentialIds::new(1);
        let root = ids.next_id();
        let rate: Time = "24000/1001".parse().unwrap();
        let settings = TimelineSettings {
            width: 1920,
            height: 1080,
            rate,
            sample_rate: 48000,
        };
        let mut editor = Editor::new(
            Document::new_timeline(root, settings),
            Arc::new(fw_ops::builtin_registry()),
        );
        let mut commands = vec![];
        for (index, kind) in [TrackKind::Video; 3]
            .into_iter()
            .chain([TrackKind::Audio; 8])
            .enumerate()
        {
            let track = TrackId(ids.next_id());
            let clip = ids.next_id();
            commands.push(Command::AddTrack {
                timeline: root,
                track: Track {
                    id: track,
                    kind,
                    name: format!("T{index}"),
                },
                index,
            });
            commands.push(Command::AddNode {
                parent: root,
                id: clip,
                type_id: "fw.clip".into(),
                name: None,
                placement: Some(Placement {
                    track,
                    start: Time::ZERO,
                    duration: Time::from_seconds(10),
                    source_in: Time::ZERO,
                    speed: Time::from_seconds(1),
                }),
            });
            commands.push(Command::SetParam {
                node: clip,
                param: "file".into(),
                value: Some(Param::Constant(ParamValue::Media(MediaRef {
                    path: clip_path.display().to_string(),
                }))),
            });
        }
        editor.apply("Bench timeline", commands).unwrap();
        Fixture {
            editor,
            folder: Path::new("/"),
            rate,
        }
    }

    fn frames(&self) -> DocumentFrames {
        DocumentFrames {
            doc: self.editor.doc().clone(),
            registry: self.editor.registry().clone(),
            media: ProjectMedia::new(self.folder),
            renderer: Renderer::new(Arc::new(Gpu::new().expect("a GPU")), DEFAULT_CACHE_BYTES),
            rate: self.rate,
            options: Options::at(Time::ZERO),
        }
    }
}

fn time<T>(f: impl FnOnce() -> T) -> Duration {
    let started = Instant::now();
    std::hint::black_box(f());
    started.elapsed()
}

fn probe_cached(_: &Fixture) -> Vec<Duration> {
    let media = ProjectMedia::new("/");
    let reference = MediaRef {
        path: fw_testmedia::hd_h264().display().to_string(),
    };
    media.probe(&reference).unwrap();
    (0..2000)
        .map(|_| time(|| media.probe(&reference).unwrap()))
        .collect()
}

fn compile_frame(f: &Fixture) -> Vec<Duration> {
    let media = ProjectMedia::new(f.folder);
    let doc = f.editor.doc();
    let registry = f.editor.registry();
    compile(doc, registry, &media, &Options::at(Time::ZERO)).unwrap();
    (0..200)
        .map(|i| {
            let options = Options::at(Time::of_frame(i, f.rate).unwrap());
            time(|| compile(doc, registry, &media, &options).unwrap())
        })
        .collect()
}

fn decode_next(f: &Fixture) -> Vec<Duration> {
    let path = fw_testmedia::hd_h264();
    let info = probe_video(&path).unwrap();
    let mut reader = VideoReader::open_probed(&path, &info).unwrap();
    reader.frame(Time::ZERO).unwrap();
    (1..120)
        .map(|i| time(|| reader.frame(Time::of_frame(i, f.rate).unwrap()).unwrap()))
        .collect()
}

fn playback_frame(f: &Fixture) -> Vec<Duration> {
    let mut frames = f.frames();
    frames.frame(0).unwrap();
    let before = frames.renderer.stats().clone();
    let samples: Vec<Duration> = (1..120)
        .map(|i| time(|| frames.frame(i).unwrap()))
        .collect();
    let after = frames.renderer.stats();
    let per = |a: Duration, b: Duration| format_ms((a - b) / samples.len() as u32);
    eprintln!(
        "  per frame: decode {}, convert + upload {}, GPU + read back {}",
        per(after.decode_time, before.decode_time),
        per(after.upload_time, before.upload_time),
        per(after.readback_time, before.readback_time),
    );
    samples
}

fn scrub(f: &Fixture) -> Vec<Duration> {
    let mut frames = f.frames();
    frames.frame(0).unwrap();
    // A fixed, scattered order, so runs are comparable.
    let mut index = 17i64;
    (0..30)
        .map(|_| {
            index = (index * 97 + 31) % 239;
            time(|| frames.frame(index).unwrap())
        })
        .collect()
}

fn audio_block(f: &Fixture) -> Vec<Duration> {
    let plan = compile_audio(
        f.editor.doc(),
        f.editor.registry(),
        &ProjectMedia::new(f.folder),
    )
    .unwrap();
    assert_eq!(plan.voices.len(), 8);
    let (mut engine, mut mixer) = AudioEngine::new(48000, 512);
    engine.play(&plan, 0).unwrap();
    let mut block = vec![0.0f32; 512 * CHANNELS];
    // Pace like a device would, so the streaming thread keeps up as it must.
    (0..400)
        .map(|_| {
            let took = time(|| mixer.process(&mut block));
            std::thread::sleep(Duration::from_micros(10_667).saturating_sub(took));
            took
        })
        .skip(10)
        .collect()
}

fn format_ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--markdown") {
        println!("| Workload | What it measures | Statistic | Limit |");
        println!("| --- | --- | --- | --- |");
        for b in BUDGETS {
            println!(
                "| `{}` | {} | {} | {} |",
                b.name,
                b.what,
                b.statistic.name(),
                format_ms(b.limit)
            );
        }
        return;
    }
    let check = args.iter().any(|a| a == "--check");
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if cfg!(debug_assertions) {
        eprintln!("fw-bench: debug build; numbers are meaningless. Use --release.");
    }
    let fixture = Fixture::new();
    let mut missed = 0;
    println!(
        "{:<22} {:>5} {:>12} {:>12}",
        "workload", "stat", "measured", "limit"
    );
    for b in BUDGETS
        .iter()
        .filter(|b| only.is_empty() || only.iter().any(|o| *o == b.name))
    {
        let mut samples = (b.run)(&fixture);
        let measured = b.statistic.of(&mut samples);
        let ok = measured <= b.limit;
        if !ok {
            missed += 1;
        }
        println!(
            "{:<22} {:>5} {:>12} {:>12}  {}",
            b.name,
            b.statistic.name(),
            format_ms(measured),
            format_ms(b.limit),
            if ok { "ok" } else { "MISSED" }
        );
    }
    if check && missed > 0 {
        eprintln!("fw-bench: {missed} budget(s) missed");
        std::process::exit(1);
    }
}
