//! `framewright`: the UI (design §10). egui and egui-winit driven by hand on
//! our own window, drawing with egui-wgpu as a guest on the engine's device.

mod app;
mod capture;
mod chrome;
mod icons;
mod style;
mod viewer;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use fw_engine::{Engine, Settings};
use fw_render::Gpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::app::App;
use crate::chrome::WindowAction;
use crate::viewer::Shown;
use crate::window::Window;

#[derive(Parser)]
#[command(name = "framewright", version, about = "Framewright")]
struct Args {
    /// The project file to open.
    project: PathBuf,
    /// Open in a fixed state, save the window as a PNG once the viewer's
    /// frame is ready, and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// The window's size in logical pixels.
    #[arg(long, default_value = "1600x1000", value_parser = parse_size)]
    size: (f64, f64),
    /// Read originals only: don't make or use editing copies.
    #[arg(long)]
    no_copies: bool,
}

fn parse_size(s: &str) -> Result<(f64, f64), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let number = |v: &str| v.parse::<f64>().map_err(|e| e.to_string());
    Ok((number(w)?, number(h)?))
}

/// Frames drawn after the viewer's frame is ready, before the screenshot,
/// so layout has settled.
const SCREENSHOT_SETTLE_FRAMES: u32 = 3;

#[derive(Debug)]
enum UserEvent {
    AccessKit(egui_winit::accesskit_winit::Event),
}

impl From<egui_winit::accesskit_winit::Event> for UserEvent {
    fn from(event: egui_winit::accesskit_winit::Event) -> UserEvent {
        UserEvent::AccessKit(event)
    }
}

/// Field order matters: the window and its surface drop before the engine
/// holding the device.
struct Shell {
    running: Option<Running>,
    /// Opened once the window exists, on a device that can draw to it.
    engine: Option<Engine>,
    args: Args,
    proxy: EventLoopProxy<UserEvent>,
    /// When egui next wants a frame, if it asked for one later.
    next_frame: Option<Instant>,
    error: Option<String>,
}

struct Running {
    window: Window,
    app: App,
    /// Frames drawn since the viewer's frame was ready, for the screenshot.
    settled: u32,
}

impl ApplicationHandler<UserEvent> for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        match self.open(event_loop) {
            Ok((window, engine)) => {
                let app = App::new(&engine);
                window.window.request_redraw();
                self.engine = Some(engine);
                self.running = Some(Running {
                    window,
                    app,
                    settled: 0,
                });
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let UserEvent::AccessKit(event) = event;
        use egui_winit::accesskit_winit::WindowEvent as Access;
        let window = &mut running.window;
        match event.window_event {
            Access::InitialTreeRequested => window.ctx.enable_accesskit(),
            Access::ActionRequested(request) => window.state.on_accesskit_action_request(request),
            Access::AccessibilityDeactivated => window.ctx.disable_accesskit(),
        }
        window.window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let response = running
            .window
            .state
            .on_window_event(&running.window.window, &event);
        match event {
            // TODO: offer to save unsaved changes.
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(engine) = &self.engine {
                    running.window.resize(engine.gpu());
                }
                running.window.window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {
                if response.repaint {
                    running.window.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(running) = self.running.as_ref() else {
            return;
        };
        match self.next_frame {
            Some(at) if at <= Instant::now() => {
                self.next_frame = None;
                running.window.window.request_redraw();
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

impl Shell {
    /// Opens the window, then the engine on a device that can draw to it.
    fn open(&self, event_loop: &ActiveEventLoop) -> Result<(Window, Engine), String> {
        let (w, h) = self.args.size;
        let title = format!("{} · Framewright", self.args.project.display());
        let instance = Gpu::instance();
        let (window, surface) = window::open(
            event_loop,
            &instance,
            &title,
            winit::dpi::LogicalSize::new(w, h),
        )?;
        let gpu = Gpu::for_surface(instance, Some(&surface)).map_err(|e| e.to_string())?;
        let settings = Settings {
            editing_copies: !self.args.no_copies,
            ..Settings::default()
        };
        let mut engine =
            Engine::open(&self.args.project, Arc::new(gpu), settings).map_err(|e| e.to_string())?;
        for warning in engine.take_warnings() {
            eprintln!("framewright: {warning}");
        }
        let window = Window::new(
            event_loop,
            engine.gpu(),
            window,
            surface,
            self.proxy.clone(),
        )?;
        println!(
            "framewright: {} on {}, {:?} surface",
            self.args.project.display(),
            engine.gpu().describe(),
            window.format()
        );
        Ok((window, engine))
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(running), Some(engine)) = (self.running.as_mut(), self.engine.as_mut()) else {
            return;
        };
        if let Some(frame) = engine.viewer_frame()
            && running.app.shown.as_ref().map(|s| s.serial) != Some(frame.serial)
        {
            running.app.shown = Some(Shown {
                image: frame.image.clone(),
                serial: frame.serial,
            });
        }
        // TODO: react to events once panels cache what they show.
        engine.take_events();

        let window = &mut running.window;
        running.app.maximised = window.window.is_maximized();
        let input = window.state.take_egui_input(&window.window);
        let app = &mut running.app;
        let mut output = window.ctx.run_ui(input, |ui| app.ui(ui, engine));
        let platform = std::mem::take(&mut output.platform_output);
        window
            .state
            .handle_platform_output(&window.window, platform);
        let repaint = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);

        for action in app.actions.drain(..) {
            let w = &window.window;
            match action {
                WindowAction::Drag => {
                    let _ = w.drag_window();
                }
                WindowAction::Resize(direction) => {
                    let _ = w.drag_resize_window(direction);
                }
                WindowAction::Minimise => w.set_minimized(true),
                WindowAction::ToggleMaximise => w.set_maximized(!w.is_maximized()),
                WindowAction::Close => event_loop.exit(),
            }
        }

        let up_to_date = engine.up_to_date();
        let mut screenshot = None;
        if let Some(path) = &self.args.screenshot
            && up_to_date
        {
            running.settled += 1;
            if running.settled > SCREENSHOT_SETTLE_FRAMES {
                screenshot = Some(path.clone());
            }
        }
        match window.render(engine.gpu(), output, screenshot.as_deref()) {
            Ok(_) => {
                if let Some(path) = screenshot {
                    println!("framewright: saved {}", path.display());
                    event_loop.exit();
                    return;
                }
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
                return;
            }
        }
        // Keep drawing while the engine's frame is on its way.
        if !up_to_date || repaint.is_zero() || self.args.screenshot.is_some() {
            window.window.request_redraw();
        } else if repaint < Duration::from_secs(3600) {
            self.next_frame = Some(Instant::now() + repaint);
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            eprintln!("framewright: can't open a window: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut shell = Shell {
        running: None,
        engine: None,
        args,
        proxy: event_loop.create_proxy(),
        next_frame: None,
        error: None,
    };
    if let Err(e) = event_loop.run_app(&mut shell) {
        eprintln!("framewright: {e}");
        return ExitCode::FAILURE;
    }
    match shell.error {
        Some(e) => {
            eprintln!("framewright: {e}");
            ExitCode::FAILURE
        }
        None => ExitCode::SUCCESS,
    }
}
