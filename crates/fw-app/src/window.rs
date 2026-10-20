//! One OS window: its surface on the engine's device, and egui driven by
//! hand on it (design §10).

use std::path::Path;
use std::sync::Arc;

use fw_render::{Display, Gpu};
use winit::event_loop::ActiveEventLoop;

/// Surface formats in order of preference. `FW_FORMAT` (`rgb10a2`,
/// `rgba16f`, `bgra8`) asks for one first.
const FORMATS: [wgpu::TextureFormat; 3] = [
    wgpu::TextureFormat::Rgb10a2Unorm,
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Rgba8Unorm,
];

/// Field order matters: fields drop top to bottom, and the surface must go
/// before the window it was made from.
pub struct Window {
    pub renderer: egui_wgpu::Renderer,
    surface: wgpu::Surface<'static>,
    pub state: egui_winit::State,
    pub ctx: egui::Context,
    config: wgpu::SurfaceConfiguration,
    pub window: Arc<winit::window::Window>,
}

/// Opens an OS window and its surface, made from `instance`. The engine's
/// device is chosen after this, so that it can draw to the surface.
pub fn open(
    event_loop: &ActiveEventLoop,
    instance: &wgpu::Instance,
    title: &str,
    size: winit::dpi::LogicalSize<f64>,
) -> Result<(Arc<winit::window::Window>, wgpu::Surface<'static>), String> {
    let attributes = winit::window::Window::default_attributes()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0))
        // The title bar is ours (design §10).
        .with_decorations(false);
    let window = Arc::new(
        event_loop
            .create_window(attributes)
            .map_err(|e| e.to_string())?,
    );
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| e.to_string())?;
    Ok((window, surface))
}

impl Window {
    /// Sets up `surface`, from [`open`], on the engine's device.
    pub fn new<T: From<egui_winit::accesskit_winit::Event> + Send>(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        proxy: winit::event_loop::EventLoopProxy<T>,
    ) -> Result<Window, String> {
        let caps = surface.get_capabilities(&gpu.adapter);
        let wanted = std::env::var("FW_FORMAT")
            .ok()
            .and_then(|f| match f.as_str() {
                "rgb10a2" => Some(wgpu::TextureFormat::Rgb10a2Unorm),
                "rgba16f" => Some(wgpu::TextureFormat::Rgba16Float),
                "bgra8" => Some(wgpu::TextureFormat::Bgra8Unorm),
                _ => None,
            });
        let format = wanted
            .into_iter()
            .chain(FORMATS)
            .find(|f| caps.formats.contains(f))
            .unwrap_or(caps.formats[0]);
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
            // For the screenshot mode.
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let pixels = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: pixels.width.max(1),
            height: pixels.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
                wgpu::CompositeAlphaMode::Opaque
            } else {
                caps.alpha_modes[0]
            },
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);

        let ctx = egui::Context::default();
        crate::style::install(&ctx);
        let max_texture = gpu.device.limits().max_texture_dimension_2d as usize;
        let mut state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture),
        );
        state.init_accesskit(event_loop, &window, proxy);
        let mut renderer =
            egui_wgpu::Renderer::new(&gpu.device, format, egui_wgpu::RendererOptions::default());
        renderer
            .callback_resources
            .insert(Display::new(&gpu.device, format));
        Ok(Window {
            renderer,
            surface,
            state,
            ctx,
            config,
            window,
        })
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn resize(&mut self, gpu: &Gpu) {
        let pixels = self.window.inner_size();
        if pixels.width > 0 && pixels.height > 0 {
            self.config.width = pixels.width;
            self.config.height = pixels.height;
            self.surface.configure(&gpu.device, &self.config);
        }
    }

    /// Draws egui's output and presents it. With `screenshot`, the finished
    /// image is also saved there as a PNG. Returns false when there was no
    /// image to draw into this time.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        mut output: egui::FullOutput,
        screenshot: Option<&Path>,
    ) -> Result<bool, String> {
        // egui's texture changes apply whether or not there's an image to draw.
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&gpu.device, &gpu.queue, *id, delta);
            }
        }
        let mut textures = std::mem::take(&mut output.textures_delta);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.resize(gpu);
                None
            }
            _ => None,
        };
        let Some(frame) = frame else {
            for id in &textures.free {
                self.renderer.free_texture(id);
            }
            textures.clear();
            return Ok(false);
        };
        let pixels_per_point = output.pixels_per_point;
        let jobs = self.ctx.tessellate(output.shapes, pixels_per_point);
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fw-ui"),
            });
        let mut commands =
            self.renderer
                .update_buffers(&gpu.device, &gpu.queue, &mut encoder, &jobs, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fw-ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_colour(self.config.format)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        let capture =
            screenshot.and_then(|_| crate::capture::record(gpu, &frame.texture, &mut encoder));
        commands.push(encoder.finish());
        {
            // Every submit on the engine's queue takes the lock, and so does
            // presenting, which can submit too.
            let _submits = gpu.lock_submits();
            gpu.queue.submit(commands);
            self.window.pre_present_notify();
            gpu.queue.present(frame);
        }
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        textures.clear();
        match (screenshot, capture) {
            (Some(path), Some(capture)) => capture.save(gpu, path)?,
            (Some(_), None) => {
                return Err("this surface can't be read back for a screenshot".into());
            }
            _ => {}
        }
        Ok(true)
    }
}

/// The background behind every panel, as the surface format wants it.
fn clear_colour(format: wgpu::TextureFormat) -> wgpu::Color {
    let [r, g, b, _] = crate::style::tokens::BG.to_array();
    let mut c = [r, g, b].map(|v| v as f64 / 255.0);
    if fw_render::SurfaceEncoding::of(format) != fw_render::SurfaceEncoding::Unorm {
        for v in &mut c {
            *v = if *v <= 0.04045 {
                *v / 12.92
            } else {
                ((*v + 0.055) / 1.055).powf(2.4)
            };
        }
    }
    wgpu::Color {
        r: c[0],
        g: c[1],
        b: c[2],
        a: 1.0,
    }
}
