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
        Some(Format::Jpeg) | None => Err(format!(
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
    let (plan, media) = compile(project, time, Some(out))?;
    if format == Format::Exr && plan.output_space.transfer != Transfer::Linear {
        return Err(format!(
            "the Output node delivers {}, which EXR can't hold; choose a linear space",
            plan.output_space
        ));
    }
    let started = Instant::now();
    let gpu = Arc::new(Gpu::new().map_err(|e| e.to_string())?);
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
        Format::Jpeg => unreachable!("rejected by output_format"),
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
