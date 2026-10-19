//! Runs compiled plans on the GPU.
//!
//! Images live in `Rgba16Float` textures, premultiplied, in whatever colour
//! space the plan says. Plan outputs and decoded files are kept in a RAM
//! cache by content key, so unchanged work is never redone.

mod cache;
mod gpu;

use std::collections::HashMap;
use std::sync::Arc;

use fw_compile::{Decoder, FillStep, KernelStep, LoadStep, Plan, Step};
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub renders: u64,
    /// Renders answered from the RAM cache without touching the GPU.
    pub cache_hits: u64,
    pub decodes: u64,
    pub kernels: u64,
    pub pipelines_built: u64,
    /// Time spent decoding media (inside the decoder), in total.
    pub decode_time: std::time::Duration,
    /// Time spent converting and uploading pixels to the GPU, in total.
    pub upload_time: std::time::Duration,
    /// Time spent waiting for the GPU and reading results back, in total.
    pub readback_time: std::time::Duration,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    /// Kernel layouts by number of extra inputs. Explicit, so a shader that
    /// never reads some binding still matches the resources we bind.
    kernel_layouts: HashMap<usize, KernelLayout>,
    cache: RamCache,
    pipelines: HashMap<ContentHash, wgpu::ComputePipeline>,
    /// Unused textures by size, reused instead of reallocated.
    pool: HashMap<(u32, u32), Vec<wgpu::Texture>>,
    stats: Stats,
}

struct KernelLayout {
    bind_group: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
}

impl KernelLayout {
    /// Binding 0: the main input. 1: the output. 2: parameters. 3…: extra inputs.
    fn new(device: &wgpu::Device, extra_inputs: usize) -> KernelLayout {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mut entries = vec![
            texture(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];
        entries.extend((0..extra_inputs).map(|k| texture(3 + k as u32)));
        let bind_group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fw-kernel"),
            entries: &entries,
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fw-kernel"),
            bind_group_layouts: &[Some(&bind_group)],
            immediate_size: 0,
        });
        KernelLayout {
            bind_group,
            pipeline,
        }
    }
}

/// A default RAM cache budget of 2 GiB.
pub const DEFAULT_CACHE_BYTES: usize = 2 << 30;

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, cache_bytes: usize) -> Renderer {
        Renderer {
            gpu,
            kernel_layouts: HashMap::new(),
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
                Step::Fill(fill) => self.fill(fill),
                Step::Kernel(kernel) => {
                    let input = &textures[&kernel.input];
                    let extra: Vec<&wgpu::Texture> =
                        kernel.extra_inputs.iter().map(|e| &textures[e]).collect();
                    let output = self.texture(input.width(), input.height());
                    self.dispatch(&mut encoder, kernel, input, &extra, &output);
                    output
                }
            };
            textures.insert(step.output(), texture);
        }
        let started = std::time::Instant::now();
        let image = self.read_back(encoder, &textures[&plan.output]);
        self.stats.readback_time += started.elapsed();
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

    fn fill(&mut self, fill: &FillStep) -> wgpu::Texture {
        let texture = self.texture(fill.width, fill.height);
        let pixel: Vec<u8> = fill
            .rgba
            .iter()
            .flat_map(|v| f16::from_f32(*v).to_le_bytes())
            .collect();
        let data = pixel.repeat(fill.width as usize * fill.height as usize);
        self.gpu.queue.write_texture(
            texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(fill.width * 8),
                rows_per_image: Some(fill.height),
            },
            texture.size(),
        );
        texture
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
                let started = std::time::Instant::now();
                let mut image = decoder.decode(load).map_err(error)?;
                self.stats.decode_time += started.elapsed();
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
        let started = std::time::Instant::now();
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
        self.stats.upload_time += started.elapsed();
        Ok(texture)
    }

    fn dispatch(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        kernel: &KernelStep,
        input: &wgpu::Texture,
        extra: &[&wgpu::Texture],
        output: &wgpu::Texture,
    ) {
        self.stats.kernels += 1;
        let device = &self.gpu.device;
        let layout = self
            .kernel_layouts
            .entry(extra.len())
            .or_insert_with(|| KernelLayout::new(device, extra.len()));
        let shader_key = ContentHash::of_bytes(kernel.wgsl.as_bytes());
        let pipeline = self.pipelines.entry(shader_key).or_insert_with(|| {
            self.stats.pipelines_built += 1;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fw-kernel"),
                source: wgpu::ShaderSource::Wgsl(kernel.wgsl.as_str().into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fw-kernel"),
                layout: Some(&layout.pipeline),
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
        let extra_views: Vec<wgpu::TextureView> = extra
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let mut entries = vec![
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
        ];
        entries.extend(
            extra_views
                .iter()
                .enumerate()
                .map(|(k, view)| wgpu::BindGroupEntry {
                    binding: 3 + k as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                }),
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fw-kernel"),
            layout: &layout.bind_group,
            entries: &entries,
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
