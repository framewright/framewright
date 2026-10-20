//! `fw`: render and inspect Framewright projects without the UI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use fw_compile::{Options, Plan, Step};
use fw_core::id::{IdGen, RandomIds};
use fw_core::{RgbaImage, Time, Transfer};
use fw_doc::{
    Command, Document, Edge, Editor, Param, Placement, PortRef, TimelineSettings, Track, TrackId,
    TrackKind,
};
use fw_media::{Format, ProjectMedia};
use fw_plugin::{MediaRef, ParamValue};
use fw_render::{DEFAULT_CACHE_BYTES, Gpu, Renderer};

#[derive(Parser)]
#[command(
    name = "fw",
    version,
    about = "Render and inspect Framewright projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render one frame to an image file (.exr or .png).
    Render {
        project: PathBuf,
        /// Seconds, as a decimal or a fraction: 1.5 or 3/2.
        #[arg(long, default_value = "0")]
        time: Time,
        #[arg(long)]
        out: PathBuf,
        /// Write 16-bit PNGs instead of 8-bit.
        #[arg(long)]
        sixteen_bit: bool,
    },
    /// Print the compiled plan: steps, fused operations and cache keys.
    Inspect {
        project: PathBuf,
        #[arg(long, default_value = "0")]
        time: Time,
        /// Compile as if rendering to this file, which decides the "auto" output space.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also print the generated shaders.
        #[arg(long)]
        wgsl: bool,
    },
    /// Play a project: sound through the default audio device, which sets the
    /// pace, and pictures rendered ahead and chosen by it. Reports frames
    /// shown and dropped, audio underruns and how late frames appear, every
    /// second. Pictures aren't shown on screen yet.
    Play {
        project: PathBuf,
        /// Where to start, in seconds.
        #[arg(long, default_value = "0")]
        from: Time,
        /// How long to play, in seconds.
        #[arg(long, default_value = "5")]
        seconds: f64,
        /// Don't open an audio device; the wall clock sets the pace.
        #[arg(long)]
        no_audio: bool,
        /// Read originals only: don't make or use editing copies.
        #[arg(long)]
        no_copies: bool,
    },
    /// Make the project's editing copies (intra-frame versions of its videos,
    /// for quick scrubbing) now, reporting progress, instead of in the
    /// background while playing.
    Copies { project: PathBuf },
    /// Write an example project (a gradient through Exposure) into a folder.
    Example { folder: PathBuf },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Cmd::Render {
            project,
            time,
            out,
            sixteen_bit,
        } => render(&project, time, &out, sixteen_bit),
        Cmd::Inspect {
            project,
            time,
            out,
            wgsl,
        } => inspect(&project, time, out.as_deref(), wgsl),
        Cmd::Play {
            project,
            from,
            seconds,
            no_audio,
            no_copies,
        } => play(&project, from, seconds, no_audio, no_copies),
        Cmd::Copies { project } => copies(&project),
        Cmd::Example { folder } => example(&folder),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fw: {message}");
            ExitCode::FAILURE
        }
    }
}

fn output_format(out: &Path) -> Result<Format, String> {
    match Format::of(out) {
        Some(Format::Jpeg | Format::Video | Format::Audio) | None => Err(format!(
            "{}: can only write .exr or .png files",
            out.display()
        )),
        Some(format) => Ok(format),
    }
}

/// Loads a project and compiles it. Returns the plan and the media access for rendering.
fn compile(project: &Path, time: Time, out: Option<&Path>) -> Result<(Plan, ProjectMedia), String> {
    let registry = fw_ops::builtin_registry();
    let doc = fw_doc::save::load(project, &registry).map_err(|e| e.to_string())?;
    let folder = project.parent().unwrap_or(Path::new("."));
    let media = ProjectMedia::new(folder);
    let mut options = Options::at(time);
    if let Some(out) = out {
        options.delivery_space = output_format(out)?.natural_space();
    }
    let plan = fw_compile::compile(&doc, &registry, &media, &options).map_err(|e| e.to_string())?;
    Ok((plan, media))
}

fn render(project: &Path, time: Time, out: &Path, sixteen_bit: bool) -> Result<(), String> {
    let format = output_format(out)?;
    let (plan, mut media) = compile(project, time, Some(out))?;
    if format == Format::Exr && plan.output_space.transfer != Transfer::Linear {
        return Err(format!(
            "the Output node delivers {}, which EXR can't hold; choose a linear space",
            plan.output_space
        ));
    }
    let started = Instant::now();
    let gpu = Arc::new(Gpu::new().map_err(|e| e.to_string())?);
    use_hardware(&mut media, &gpu);
    let setup = started.elapsed();
    let mut renderer = Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES);
    let image: Arc<RgbaImage> = renderer.render(&plan, &media).map_err(|e| e.to_string())?;
    let rendered = started.elapsed() - setup;
    match format {
        Format::Exr => fw_media::write_exr(out, &image, plan.output_space),
        Format::Png => {
            if plan.output_space.transfer == Transfer::Linear {
                eprintln!(
                    "fw: warning: writing linear {} values to a PNG",
                    plan.output_space
                );
            }
            fw_media::write_png(out, &image, sixteen_bit)
        }
        Format::Jpeg | Format::Video | Format::Audio => unreachable!("rejected by output_format"),
    }
    .map_err(|e| e.to_string())?;
    let written = started.elapsed() - setup - rendered;
    println!(
        "{}: {}×{} {} at {}s on {}",
        out.display(),
        image.width,
        image.height,
        plan.output_space,
        time,
        gpu.describe(),
    );
    println!(
        "  GPU setup {} ms, decode and render {} ms, encode and write {} ms",
        setup.as_millis(),
        rendered.as_millis(),
        written.as_millis()
    );
    Ok(())
}

/// Decodes video on the GPU when it can (design §12; `FW_HARDWARE_DECODE=0`
/// turns it off).
fn use_hardware(media: &mut ProjectMedia, gpu: &Arc<Gpu>) {
    if let Some(device) = gpu.video_device()
        && let Err(e) = media.use_hardware(device)
    {
        eprintln!("fw: decoding in software: {e}");
    }
}

/// Media access for a project, making and using editing copies when `copies`.
fn project_media(folder: &Path, copies: bool) -> ProjectMedia {
    if copies {
        ProjectMedia::with_copies(
            folder,
            ProjectMedia::copy_root(folder),
            fw_media::PRORES_PROXY_1080,
        )
    } else {
        ProjectMedia::new(folder)
    }
}

fn copies(project: &Path) -> Result<(), String> {
    use fw_compile::MediaResolver;
    let registry = fw_ops::builtin_registry();
    let doc = fw_doc::save::load(project, &registry).map_err(|e| e.to_string())?;
    let folder = project.parent().unwrap_or(Path::new("."));
    let media = project_media(folder, true);
    let copies = media.copies().expect("copies are on");
    let started = Instant::now();
    let mut videos = vec![];
    for (_, node) in doc.nodes() {
        let Some(known) = node.as_known() else {
            continue;
        };
        for param in known.params.values() {
            if let Param::Constant(ParamValue::Media(reference)) = param {
                let info = media.probe(reference)?;
                if info.frames.is_some() && !videos.iter().any(|(_, f)| *f == info.fingerprint) {
                    videos.push((reference.path.clone(), info.fingerprint));
                }
            }
        }
    }
    let report = || {
        let mut done = true;
        for (path, fingerprint) in &videos {
            match copies.progress(fingerprint) {
                Some((ready, all)) => {
                    done &= ready == all;
                    println!("  {path}: {ready} of {all} chunks");
                }
                None => println!("  {path}: needs no copy"),
            }
        }
        done
    };
    println!(
        "editing copies in {}",
        ProjectMedia::copy_root(folder).display()
    );
    while !report() {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    copies.wait();
    println!("done in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn play(
    project: &Path,
    from: Time,
    seconds: f64,
    no_audio: bool,
    no_copies: bool,
) -> Result<(), String> {
    use fw_audio::{AudioEngine, MasterClock, WallClock};
    use fw_playback::{DocumentFrames, Player};

    let registry = Arc::new(fw_ops::builtin_registry());
    let doc = fw_doc::save::load(project, &registry).map_err(|e| e.to_string())?;
    let folder = project.parent().unwrap_or(Path::new("."));
    let (rate, sample_rate) = match doc.timeline(doc.root()) {
        Some(t) => (t.settings.rate, t.settings.sample_rate),
        None => (Time::from_seconds(24), 48000),
    };
    let plan = fw_compile::compile_audio(&doc, &registry, &ProjectMedia::new(folder))
        .map_err(|e| e.to_string())?;

    // When sound plays, its sample count is the master clock.
    let from_sample = from.sample_index(sample_rate).map_err(|e| e.to_string())?;
    let mut audio = None;
    let wall = Arc::new(WallClock::starting_at(from));
    let clock: Arc<dyn MasterClock> = if no_audio || plan.voices.is_empty() {
        wall.clone()
    } else {
        let (engine, mixer) = AudioEngine::new(sample_rate, fw_audio::output::MAX_FRAMES);
        match fw_audio::start_output(mixer, engine.clock().clone(), sample_rate) {
            Ok(output) => {
                println!("audio: {} at {} Hz", output.device, output.rate);
                // Hold at the start until the sound begins.
                engine.clock().advance_to(from_sample);
                let clock: Arc<dyn MasterClock> = engine.clock().clone();
                audio = Some((engine, output));
                clock
            }
            Err(e) => {
                eprintln!("fw: {e}; playing pictures only, timed by the wall clock");
                wall.clone()
            }
        }
    };

    let gpu = Arc::new(Gpu::new().map_err(|e| e.to_string())?);
    println!(
        "video: {} at {} fps on {}",
        project.display(),
        rate,
        gpu.describe()
    );
    let mut options = Options::at(from);
    options.quality = fw_plugin::Quality::Draft;
    let mut media = project_media(folder, !no_copies);
    use_hardware(&mut media, &gpu);
    let frames = DocumentFrames {
        doc,
        registry,
        media,
        renderer: Renderer::new(gpu, DEFAULT_CACHE_BYTES),
        rate,
        options,
    };
    let mut player = Player::new(Box::new(frames), clock.clone(), rate, 6);

    // Start with the first frame ready, then let the clock run.
    player.tick();
    let first = from.frame_index(rate).map_err(|e| e.to_string())?;
    if !player.wait_for(first, std::time::Duration::from_secs(10)) {
        return Err("the first frame took longer than 10 s to render".into());
    }
    match &mut audio {
        Some((engine, _)) => engine.play(&plan, from_sample).map_err(|e| e.to_string())?,
        None => wall.start(),
    }
    let end = from.as_f64() + seconds;
    let mut next_report = from.as_f64() + 1.0;
    let mut last = player.stats().clone();
    let mut offsets: Vec<f64> = vec![];
    while clock.now().as_f64() < end {
        if let Some(shown) = player.tick() {
            // Lateness: how far into its own frame the clock is when it appears.
            offsets.push((clock.now().as_f64() - shown.time.as_f64()) * 1000.0);
        }
        let now = clock.now().as_f64();
        if now >= next_report {
            let stats = player.stats();
            let underruns = audio
                .as_ref()
                .map(|(e, _)| {
                    e.stats()
                        .underruns
                        .load(std::sync::atomic::Ordering::Relaxed)
                })
                .unwrap_or(0);
            let (mean, worst) = summarise(&offsets);
            println!(
                "{now:7.2}s  shown {:3}  dropped {:3}  audio underruns {underruns}  frame lateness mean {mean:5.1} ms, worst {worst:5.1} ms",
                stats.shown - last.shown,
                stats.dropped - last.dropped,
            );
            last = stats.clone();
            offsets.clear();
            next_report += 1.0;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let stats = player.stats();
    println!(
        "played {seconds} s: {} frames shown, {} dropped, {} failed",
        stats.shown, stats.dropped, stats.errors
    );
    if let Some((engine, _output)) = &mut audio {
        engine.stop();
        for e in engine.take_errors() {
            eprintln!("fw: audio: {e}");
        }
    }
    Ok(())
}

/// Mean and largest of a set of millisecond lateness values.
fn summarise(offsets: &[f64]) -> (f64, f64) {
    if offsets.is_empty() {
        return (0.0, 0.0);
    }
    let mean = offsets.iter().sum::<f64>() / offsets.len() as f64;
    let worst = offsets.iter().fold(0f64, |m, v| m.max(v.abs()));
    (mean, worst)
}

fn inspect(project: &Path, time: Time, out: Option<&Path>, wgsl: bool) -> Result<(), String> {
    let (plan, _) = compile(project, time, out)?;
    print!("{}", plan.describe());
    if wgsl {
        for step in &plan.steps {
            if let Step::Kernel(kernel) = step {
                println!("\n// ---- %{} ----\n{}", kernel.output.0, kernel.wgsl);
            }
        }
    }
    Ok(())
}

fn example(folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let image_path = folder.join("gradient.png");
    fw_media::write_png(&image_path, &example_image(1920, 1080), true)
        .map_err(|e| e.to_string())?;

    let mut ids = RandomIds::default();
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
    let connect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    let set = |node, param: &str, value| Command::SetParam {
        node,
        param: param.into(),
        value: Some(Param::Constant(value)),
    };
    editor
        .apply(
            "Example",
            vec![
                add(read, "fw.read_image"),
                add(exposure, "fw.exposure"),
                add(output, "fw.output"),
                set(
                    read,
                    "file",
                    ParamValue::Media(MediaRef {
                        path: "gradient.png".into(),
                    }),
                ),
                set(exposure, "exposure", ParamValue::Float(1.0)),
                connect(read, exposure),
                connect(exposure, output),
            ],
        )
        .map_err(|e| e.to_string())?;
    let project = folder.join("project.json");
    fw_doc::save::save(editor.doc(), &project)
        .map_err(|e| format!("{}: {e}", project.display()))?;

    let timeline = folder.join("timeline.json");
    fw_doc::save::save(&example_timeline(&mut ids)?, &timeline)
        .map_err(|e| format!("{}: {e}", timeline.display()))?;
    println!(
        "wrote {}, {} and {}",
        project.display(),
        timeline.display(),
        image_path.display()
    );
    println!("try: fw render {} --out frame.exr", project.display());
    println!(
        "     fw render {} --time 2.5 --out gap.png   (the gradient plays 0–2 s and 3–5 s)",
        timeline.display()
    );
    Ok(())
}

/// A 1080p, 24 fps timeline: the gradient on V1 from 0 to 2 s and on V2 from
/// 3 to 5 s, with a gap between.
fn example_timeline(ids: &mut RandomIds) -> Result<Document, String> {
    let root = ids.next_id();
    let settings = TimelineSettings {
        width: 1920,
        height: 1080,
        rate: Time::from_seconds(24),
        sample_rate: 48000,
    };
    let mut editor = Editor::new(
        Document::new_timeline(root, settings),
        Arc::new(fw_ops::builtin_registry()),
    );
    let mut commands = vec![];
    for (index, (name, start)) in [("V1", 0), ("V2", 3)].into_iter().enumerate() {
        let track = TrackId(ids.next_id());
        let clip = ids.next_id();
        commands.push(Command::AddTrack {
            timeline: root,
            track: Track {
                id: track,
                kind: TrackKind::Video,
                name: name.into(),
            },
            index,
        });
        commands.push(Command::AddNode {
            parent: root,
            id: clip,
            type_id: "fw.clip".into(),
            name: Some("Gradient".into()),
            placement: Some(Placement {
                track,
                start: Time::from_seconds(start),
                duration: Time::from_seconds(2),
                source_in: Time::ZERO,
                speed: Time::from_seconds(1),
            }),
        });
        commands.push(Command::SetParam {
            node: clip,
            param: "file".into(),
            value: Some(Param::Constant(ParamValue::Media(MediaRef {
                path: "gradient.png".into(),
            }))),
        });
    }
    editor
        .apply("Example timeline", commands)
        .map_err(|e| e.to_string())?;
    Ok(editor.doc().clone())
}

/// A smooth hue sweep fading to grey, with an alpha ramp at the bottom. Written
/// as a 16-bit sRGB PNG, so any banding in a render comes from the engine.
fn example_image(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height, false);
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / (width - 1) as f32;
            let fy = y as f32 / (height - 1) as f32;
            let hue = fx * 6.0;
            let channel = |offset: f32| {
                let d = ((hue - offset).rem_euclid(6.0) - 3.0).abs();
                (d - 1.0).clamp(0.0, 1.0)
            };
            let grey = fx;
            let mix = |c: f32| c + (grey - c) * fy;
            let alpha = if fy > 0.9 {
                1.0 - (fy - 0.9) * 10.0 * (1.0 - fx)
            } else {
                1.0
            };
            img.set_pixel(
                x,
                y,
                [
                    mix(channel(0.0)),
                    mix(channel(4.0)),
                    mix(channel(2.0)),
                    alpha,
                ],
            );
        }
    }
    img
}
