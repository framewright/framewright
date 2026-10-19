//! Runs compiled plans on the GPU.
//!
//! Images live in `Rgba16Float` textures, premultiplied, in whatever colour
//! space the plan says. Plan outputs and decoded files are kept in a RAM
//! cache by content key, so unchanged work is never redone.

mod cache;
mod gpu;

use std::collections::HashMap;
use std::sync::Arc;

use fw_compile::{Decoder, KernelStep, LoadStep, Plan, Step};
use fw_core::{ContentHash, RgbaImage};
use fw_plugin::ImageHandle;
use half::f16;

pub use cache::RamCache;
pub use gpu::Gpu;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no usable GPU: {0}")]
    NoAdapter(String),
    #[error("GPU error: {0}")]
    Gpu(String),
    #[error("cannot read {path}: {message}")]
    Decode { path: String, message: String },
}

/// Counters for tests and `fw render --stats`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub renders: u64,
    /// Renders answered from the RAM cache without touching the GPU.
    pub cache_hits: u64,
    pub decodes: u64,
    pub kernels: u64,
    pub pipelines_built: u64,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    cache: RamCache,
    pipelines: HashMap<ContentHash, wgpu::ComputePipeline>,
    /// Unused textures by size, reused instead of reallocated.
    pool: HashMap<(u32, u32), Vec<wgpu::Texture>>,
    stats: Stats,
}

/// A default RAM cache budget of 2 GiB.
pub const DEFAULT_CACHE_BYTES: usize = 2 << 30;

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, cache_bytes: usize) -> Renderer {
        Renderer {
            gpu,
            cache: RamCache::new(cache_bytes),
            pipelines: HashMap::new(),
            pool: HashMap::new(),
            stats: Stats::default(),
        }
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Renders the plan's output: premultiplied pixels in `plan.output_space`.
    pub fn render(
        &mut self,
        plan: &Plan,
        decoder: &dyn Decoder,
    ) -> Result<Arc<RgbaImage>, RenderError> {
        self.stats.renders += 1;
        let output_key = plan.output_key();
        if let Some(hit) = self.cache.get(&output_key) {
            self.stats.cache_hits += 1;
            return Ok(hit);
        }
        let device = &self.gpu.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut textures: HashMap<ImageHandle, wgpu::Texture> = HashMap::new();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fw-plan"),
        });
        for step in &plan.steps {
            let texture = match step {
                Step::Load(load) => self.upload(load, decoder)?,
                Step::Kernel(kernel) => {
                    let input = &textures[&kernel.input];
                    let output = self.texture(input.width(), input.height());
                    self.dispatch(&mut encoder, kernel, input, &output);
                    output
                }
            };
            textures.insert(step.output(), texture);
        }
        let image = self.read_back(encoder, &textures[&plan.output]);
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Gpu(error.to_string()));
        }
        for texture in textures.into_values() {
            self.pool
                .entry((texture.width(), texture.height()))
                .or_default()
                .push(texture);
        }
        let image = Arc::new(image);
        self.cache.insert(output_key, image.clone());
        Ok(image)
    }

    fn texture(&mut self, width: u32, height: u32) -> wgpu::Texture {
        if let Some(t) = self.pool.get_mut(&(width, height)).and_then(Vec::pop) {
            return t;
        }
        self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fw-image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn upload(
        &mut self,
        load: &LoadStep,
        decoder: &dyn Decoder,
    ) -> Result<wgpu::Texture, RenderError> {
        let image = match self.cache.get(&load.key) {
            Some(image) => image,
            None => {
                self.stats.decodes += 1;
                let error = |message: String| RenderError::Decode {
                    path: load.path.display().to_string(),
                    message,
                };
                let mut image = decoder.decode(load).map_err(error)?;
                if (image.width, image.height) != (load.width, load.height) {
                    return Err(error(format!(
                        "expected {}×{}, decoded {}×{}",
                        load.width, load.height, image.width, image.height
                    )));
                }
                image.premultiply();
                let image = Arc::new(image);
                self.cache.insert(load.key, image.clone());
                image
            }
        };
        let texture = self.texture(image.width, image.height);
        let halves: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(|v| f16::from_f32(*v).to_le_bytes())
            .collect();
        self.gpu.queue.write_texture(
            texture.as_image_copy(),
            &halves,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 8),
                rows_per_image: Some(image.height),
            },
            texture.size(),
        );
        Ok(texture)
    }

    fn dispatch(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        kernel: &KernelStep,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
    ) {
        self.stats.kernels += 1;
        let device = &self.gpu.device;
        let shader_key = ContentHash::of_bytes(kernel.wgsl.as_bytes());
        let pipeline = self.pipelines.entry(shader_key).or_insert_with(|| {
            self.stats.pipelines_built += 1;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fw-kernel"),
                source: wgpu::ShaderSource::Wgsl(kernel.wgsl.as_str().into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fw-kernel"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        let mut params = kernel.params();
        if params.is_empty() {
            params.push(0.0); // storage bindings can't be empty
        }
        let bytes: Vec<u8> = params.iter().flat_map(|p| p.to_le_bytes()).collect();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fw-params"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu.queue.write_buffer(&buffer, 0, &bytes);
        let input_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fw-kernel"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("fw-kernel"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(output.width().div_ceil(8), output.height().div_ceil(8), 1);
    }

    /// Copies a texture back to the CPU. Rows in the staging buffer are padded
    /// to wgpu's 256-byte alignment.
    fn read_back(&self, mut encoder: wgpu::CommandEncoder, texture: &wgpu::Texture) -> RgbaImage {
        let (width, height) = (texture.width(), texture.height());
        let row = width * 8;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fw-readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        self.gpu.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.gpu.wait();
        let mut image = RgbaImage::new(width, height, true);
        {
            let data = buffer
                .slice(..)
                .get_mapped_range()
                .expect("buffer is mapped");
            for y in 0..height as usize {
                let start = y * padded as usize;
                let src = &data[start..start + row as usize];
                let dst = &mut image.pixels[y * width as usize * 4..(y + 1) * width as usize * 4];
                for (out, half) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *out = f16::from_le_bytes([half[0], half[1]]).to_f32();
                }
            }
        }
        buffer.unmap();
        image
    }
}
