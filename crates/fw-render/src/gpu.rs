//! The engine's GPU device. The engine creates and owns it; a UI is a guest.

use crate::RenderError;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
}

impl Gpu {
    /// Creates a headless device. `FW_ADAPTER=<part of the name>` picks an
    /// adapter (e.g. `nvidia`, `llvmpipe`); otherwise the high-performance one.
    /// `WGPU_BACKEND` restricts the backends tried.
    pub fn new() -> Result<Gpu, RenderError> {
        let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = match std::env::var("FW_ADAPTER") {
            Ok(want) => {
                let want = want.to_lowercase();
                pollster::block_on(instance.enumerate_adapters(backends))
                    .into_iter()
                    .find(|a| a.get_info().name.to_lowercase().contains(&want))
                    .ok_or_else(|| {
                        RenderError::NoAdapter(format!("FW_ADAPTER={want} matched nothing"))
                    })?
            }
            Err(_) => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            }))
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?,
        };
        let info = adapter.get_info();
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fw-engine"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: limits.max_texture_dimension_2d.min(16384),
                ..wgpu::Limits::default()
            },
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(|e| RenderError::Gpu(e.to_string()))?;
        Ok(Gpu {
            device,
            queue,
            info,
        })
    }

    /// e.g. `"NVIDIA GeForce RTX 3070 (Vulkan, driver 580.95)"`.
    pub fn describe(&self) -> String {
        format!(
            "{} ({:?}, driver {} {})",
            self.info.name, self.info.backend, self.info.driver, self.info.driver_info
        )
    }

    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}
