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
        what: "Decode the next frame of 1080p H.264, ready to upload",
        statistic: Statistic::P95,
        limit: ms(20.0),
        run: decode_next,
    },
    Budget {
        name: "playback-frame-1080p",
        what: "Compile, decode and render the next 1080p frame, until the GPU has finished it",
        statistic: Statistic::P95,
        limit: ms(41.7),
        run: playback_frame,
    },
    Budget {
        name: "playback-composite-1080p",
        what: "The same, blending three layers decoded from three places in one file",
        statistic: Statistic::P95,
        limit: ms(41.7),
        run: playback_composite,
    },
    Budget {
        name: "scrub-1080p",
        what: "Render a random 1080p frame (seek, decode, render, GPU finished)",
        statistic: Statistic::P95,
        limit: ms(150.0),
        run: scrub,
    },
    Budget {
        name: "scrub-1080p-copy",
        what: "Render a random 1080p frame at draft quality, from its editing copy (GPU finished)",
        statistic: Statistic::P95,
        limit: ms(50.0),
        run: scrub_copy,
    },
    Budget {
        name: "audio-block-8-voices",
        what: "Mix one 512-frame block of eight streaming voices on the audio thread",
        statistic: Statistic::P99,
        limit: ms(0.5),
        run: audio_block,
    },
];

/// 1080p timelines of the test clip shared by the workloads:
///
/// - `editor`: three video tracks and eight audio tracks, all from the start
///   of the clip. The top layer covers the others, so one layer is rendered.
/// - `composite`: three video tracks from 0, 3 and 6 s into the clip, the
///   top two at 50%, so all three are decoded and blended.
struct Fixture {
    editor: Editor,
    composite: Editor,
    folder: &'static Path,
    rate: Time,
}

/// A video clip on its own track: where in the source it starts, and its opacity.
struct Layer {
    source_in: Time,
    opacity: f64,
}

impl Fixture {
    fn new() -> Fixture {
        let rate: Time = "24000/1001".parse().unwrap();
        let full = || Layer {
            source_in: Time::ZERO,
            opacity: 100.0,
        };
        let composite = [(0, 100.0), (3, 50.0), (6, 50.0)].map(|(s, opacity)| Layer {
            source_in: Time::from_seconds(s),
            opacity,
        });
        Fixture {
            editor: timeline(rate, &[full(), full(), full()], 8),
            composite: timeline(rate, &composite, 0),
            folder: Path::new("/"),
            rate,
        }
    }

    /// Frames of `editor`, decoding in hardware when that's faster
    /// (`FW_HARDWARE_DECODE=0` measures software, `1` hardware).
    fn frames(&self, editor: &Editor) -> DocumentFrames {
        let gpu = Arc::new(Gpu::new().expect("a GPU"));
        let mut media = ProjectMedia::new(self.folder);
        if let Some(device) = gpu.video_device() {
            media
                .use_hardware(device, hardware_policy())
                .expect("hardware decoding starts");
        }
        DocumentFrames {
            doc: editor.doc().clone(),
            registry: editor.registry().clone(),
            media,
            renderer: Renderer::new(gpu, DEFAULT_CACHE_BYTES),
            rate: self.rate,
            options: Options::at(Time::ZERO),
        }
    }
}

/// Video layers bottom to top, then `audio` audio tracks, each holding the
/// test clip from its `source_in` to its end. The clip is 10 s long.
fn timeline(rate: Time, layers: &[Layer], audio: usize) -> Editor {
    let clip_path = fw_testmedia::hd_h264();
    let mut ids = SequentialIds::new(1);
    let root = ids.next_id();
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
    let tracks = layers
        .iter()
        .map(|l| (TrackKind::Video, l.source_in, l.opacity))
        .chain((0..audio).map(|_| (TrackKind::Audio, Time::ZERO, 100.0)));
    for (index, (kind, source_in, opacity)) in tracks.enumerate() {
        let track = TrackId(ids.next_id());
        let clip = ids.next_id();
        let set = |param: &str, value| Command::SetParam {
            node: clip,
            param: param.into(),
            value: Some(Param::Constant(value)),
        };
        commands.extend([
            Command::AddTrack {
                timeline: root,
                track: Track {
                    id: track,
                    kind,
                    name: format!("T{index}"),
                },
                index,
            },
            Command::AddNode {
                parent: root,
                id: clip,
                type_id: "fw.clip".into(),
                name: None,
                placement: Some(Placement {
                    track,
                    start: Time::ZERO,
                    duration: Time::from_seconds(10).checked_sub(source_in).unwrap(),
                    source_in,
                    speed: Time::from_seconds(1),
                }),
            },
            set(
                "file",
                ParamValue::Media(MediaRef {
                    path: clip_path.display().to_string(),
                }),
            ),
            set("opacity", ParamValue::Float(opacity)),
        ]);
    }
    editor.apply("Bench timeline", commands).unwrap();
    editor
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

fn hardware_policy() -> fw_media::HardwarePolicy {
    match std::env::var("FW_HARDWARE_DECODE").as_deref() {
        Ok("1") => fw_media::HardwarePolicy::Always,
        _ => fw_media::HardwarePolicy::Fastest,
    }
}

/// Which decoder each kind of video used, for the breakdowns.
fn decoders(media: &ProjectMedia) -> String {
    media
        .decoder_choices()
        .iter()
        .map(|c| {
            format!(
                "; decoding {} (measured: GPU {:.0} ms, software {:.0} ms)",
                if c.hardware {
                    "on the GPU"
                } else {
                    "in software"
                },
                c.hardware_time.as_secs_f64() * 1e3,
                c.software_time.as_secs_f64() * 1e3
            )
        })
        .collect()
}

/// Renders a frame and waits for the GPU to finish it, which showing it
/// would. Returns how long the wait was.
fn render_frame(frames: &mut DocumentFrames, index: i64) -> Duration {
    std::hint::black_box(frames.frame(index).unwrap());
    let waited = Instant::now();
    frames.renderer.gpu().wait();
    waited.elapsed()
}

fn playback_frame(f: &Fixture) -> Vec<Duration> {
    playback(f, &f.editor)
}

fn playback_composite(f: &Fixture) -> Vec<Duration> {
    playback(f, &f.composite)
}

fn playback(f: &Fixture, editor: &Editor) -> Vec<Duration> {
    let mut frames = f.frames(editor);
    render_frame(&mut frames, 0);
    let before = frames.renderer.stats().clone();
    let mut gpu = Duration::ZERO;
    // The composite's shortest layer lasts 4 s.
    let samples: Vec<Duration> = (1..90)
        .map(|i| time(|| gpu += render_frame(&mut frames, i)))
        .collect();
    let after = frames.renderer.stats();
    let per = |d: Duration| format_ms(d / samples.len() as u32);
    eprintln!(
        "  per frame: decode {}, upload {}, waiting for the GPU {}{}",
        per(after.decode_time - before.decode_time),
        per(after.upload_time - before.upload_time),
        per(gpu),
        decoders(&frames.media),
    );
    samples
}

fn scrub(f: &Fixture) -> Vec<Duration> {
    scrub_frames(f.frames(&f.editor))
}

fn scrub_copy(f: &Fixture) -> Vec<Duration> {
    let mut frames = f.frames(&f.editor);
    frames.media = ProjectMedia::with_copies(
        f.folder,
        fw_testmedia::hd_h264().with_file_name("copies"),
        fw_media::PRORES_PROXY_1080,
    );
    if let Some(device) = frames.renderer.gpu().video_device() {
        frames
            .media
            .use_hardware(device, hardware_policy())
            .expect("hardware decoding starts");
    }
    frames.options.quality = fw_plugin::Quality::Draft;
    // Making the copy is setup, not timed; later runs find it on disk.
    render_frame(&mut frames, 0);
    frames.media.copies().expect("copies are on").wait();
    scrub_frames(frames)
}

fn scrub_frames(mut frames: DocumentFrames) -> Vec<Duration> {
    render_frame(&mut frames, 0);
    let before = frames.renderer.stats().clone();
    let mut gpu = Duration::ZERO;
    // A fixed, scattered order, so runs are comparable.
    let mut index = 17i64;
    let samples: Vec<Duration> = (0..30)
        .map(|_| {
            index = (index * 97 + 31) % 239;
            time(|| gpu += render_frame(&mut frames, index))
        })
        .collect();
    let after = frames.renderer.stats();
    let per = |d: Duration| format_ms(d / samples.len() as u32);
    let mut sorted = samples.clone();
    sorted.sort();
    eprintln!(
        "  per frame: mean {}, median {}; decode {}, upload {}, waiting for the GPU {}{}",
        per(samples.iter().sum()),
        format_ms(sorted[sorted.len() / 2]),
        per(after.decode_time - before.decode_time),
        per(after.upload_time - before.upload_time),
        per(gpu),
        decoders(&frames.media),
    );
    samples
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
