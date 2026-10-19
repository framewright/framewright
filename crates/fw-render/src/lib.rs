//! Runs compiled plans on the GPU.
//!
//! Images live in `Rgba16Float` textures, premultiplied, in whatever colour
//! space the plan says. Plan outputs and decoded files are kept in a RAM
//! cache by content key, so unchanged work is never redone.
//!
//! Decoded video arrives as planes of integer codes and is converted on the
//! GPU, so the CPU never touches its pixels.

mod cache;
mod gpu;
mod unpack;

use std::collections::HashMap;
use std::sync::Arc;

use fw_compile::{Decoder, FillStep, KernelStep, LoadStep, Plan, Step};
use fw_core::{ColorSpace, ContentHash, DecodedImage, PlanarImage, RgbaImage};
use fw_plugin::ImageHandle;
use half::f16;
use half::slice::HalfFloatSliceExt;

pub use cache::{Cached, RamCache};
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
    /// Time spent preparing and queueing pixels for the GPU, in total.
    pub upload_time: std::time::Duration,
    /// Time spent waiting for the GPU and reading results back, in total.
    pub readback_time: std::time::Duration,
}

/// A rendered image that stays on the GPU: premultiplied `Rgba16Float` in
/// `space`. Work that uses it on the engine's queue runs after the render.
#[derive(Debug, Clone)]
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub space: ColorSpace,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    /// Bind-group layouts by kind. Explicit, so a shader that never reads
    /// some binding still matches the resources we bind.
    layouts: HashMap<LayoutKind, KernelLayout>,
    cache: RamCache,
    pipelines: HashMap<ContentHash, wgpu::ComputePipeline>,
    /// Unused textures by size and format, reused instead of reallocated.
    pool: HashMap<(u32, u32, wgpu::TextureFormat), Vec<wgpu::Texture>>,
    stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LayoutKind {
    /// A plan's kernel with this many extra inputs.
    /// Binding 0: the main input. 1: the output. 2: parameters. 3…: extra inputs.
    Kernel(usize),
    /// Unpacking this many planes. Binding 0: the output. 1…: the planes.
    Planar(usize),
    /// Binding 0: the output.
    Fill,
}

struct KernelLayout {
    bind_group: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
}

impl KernelLayout {
    fn new(device: &wgpu::Device, kind: LayoutKind) -> KernelLayout {
        let texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let output = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let entries = match kind {
            LayoutKind::Kernel(extra_inputs) => {
                let mut entries = vec![
                    texture(0, float),
                    output(1),
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
                entries.extend((0..extra_inputs).map(|k| texture(3 + k as u32, float)));
                entries
            }
            LayoutKind::Planar(planes) => {
                let mut entries = vec![output(0)];
                entries.extend(
                    (0..planes).map(|k| texture(1 + k as u32, wgpu::TextureSampleType::Uint)),
                );
                entries
            }
            LayoutKind::Fill => vec![output(0)],
        };
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
            layouts: HashMap::new(),
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

    /// Renders the plan's output and reads it back: premultiplied pixels in
    /// `plan.output_space`.
    pub fn render(
        &mut self,
        plan: &Plan,
        decoder: &dyn Decoder,
    ) -> Result<Arc<RgbaImage>, RenderError> {
        self.stats.renders += 1;
        let output_key = plan.output_key();
        if let Some(Cached::Image(hit)) = self.cache.get(&output_key) {
            self.stats.cache_hits += 1;
            return Ok(hit);
        }
        let scope = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let (encoder, output) = self.encode(plan, decoder)?;
        let started = std::time::Instant::now();
        let image = self.read_back(encoder, &output);
        self.stats.readback_time += started.elapsed();
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Gpu(error.to_string()));
        }
        self.recycle(output);
        let image = Arc::new(image);
        self.cache.insert(output_key, Cached::Image(image.clone()));
        Ok(image)
    }

    /// Renders the plan's output into a texture that stays on the GPU, for
    /// showing. Returns once the work is queued, without waiting for it.
    pub fn render_gpu(
        &mut self,
        plan: &Plan,
        decoder: &dyn Decoder,
    ) -> Result<GpuImage, RenderError> {
        self.stats.renders += 1;
        let scope = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let (encoder, texture) = self.encode(plan, decoder)?;
        self.gpu.queue.submit([encoder.finish()]);
        // Lets wgpu free the frame's finished buffers and bind groups.
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Gpu(error.to_string()));
        }
        Ok(GpuImage {
            texture,
            space: plan.output_space,
        })
    }

    /// Records the plan's work. Returns the output texture, which the caller
    /// owns; every other texture goes back to the pool.
    fn encode(
        &mut self,
        plan: &Plan,
        decoder: &dyn Decoder,
    ) -> Result<(wgpu::CommandEncoder, wgpu::Texture), RenderError> {
        let mut textures: HashMap<ImageHandle, wgpu::Texture> = HashMap::new();
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fw-plan"),
            });
        for step in &plan.steps {
            let texture = match step {
                Step::Load(load) => self.upload(&mut encoder, load, decoder)?,
                Step::Fill(fill) => self.fill(&mut encoder, fill),
                Step::Kernel(kernel) => {
                    let input = &textures[&kernel.input];
                    let extra: Vec<&wgpu::Texture> =
                        kernel.extra_inputs.iter().map(|e| &textures[e]).collect();
                    let output = self.texture(input.width(), input.height(), FORMAT);
                    self.dispatch(&mut encoder, kernel, input, &extra, &output);
                    output
                }
            };
            textures.insert(step.output(), texture);
        }
        let output = textures
            .remove(&plan.output)
            .expect("the plan makes its output");
        for texture in textures.into_values() {
            self.recycle(texture);
        }
        Ok((encoder, output))
    }

    /// Returns a texture to the pool. Reusing it later is safe: the queue
    /// runs work in order.
    fn recycle(&mut self, texture: wgpu::Texture) {
        self.pool
            .entry((texture.width(), texture.height(), texture.format()))
            .or_default()
            .push(texture);
    }

    fn texture(&mut self, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        if let Some(t) = self
            .pool
            .get_mut(&(width, height, format))
            .and_then(Vec::pop)
        {
            return t;
        }
        let usage = if format == FORMAT {
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
        } else {
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
        };
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
            format,
            usage,
            view_formats: &[],
        })
    }

    fn fill(&mut self, encoder: &mut wgpu::CommandEncoder, fill: &FillStep) -> wgpu::Texture {
        let texture = self.texture(fill.width, fill.height, FORMAT);
        let view = texture.create_view(&Default::default());
        self.run(
            encoder,
            LayoutKind::Fill,
            &unpack::fill(fill.rgba),
            &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
            (fill.width, fill.height),
        );
        texture
    }

    fn upload(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        load: &LoadStep,
        decoder: &dyn Decoder,
    ) -> Result<wgpu::Texture, RenderError> {
        let decoded = match self.cache.get(&load.key) {
            Some(Cached::Decoded(decoded)) => decoded,
            _ => {
                self.stats.decodes += 1;
                let error = |message: String| RenderError::Decode {
                    path: load.path.display().to_string(),
                    message,
                };
                let started = std::time::Instant::now();
                let mut decoded = decoder.decode(load).map_err(error)?;
                self.stats.decode_time += started.elapsed();
                let (width, height) = decoded.size();
                if (width, height) != (load.width, load.height) {
                    return Err(error(format!(
                        "expected {}×{}, decoded {width}×{height}",
                        load.width, load.height
                    )));
                }
                if let DecodedImage::Rgba(image) = &mut decoded {
                    image.premultiply();
                }
                let decoded = Arc::new(decoded);
                self.cache
                    .insert(load.key, Cached::Decoded(decoded.clone()));
                decoded
            }
        };
        let started = std::time::Instant::now();
        let texture = match &*decoded {
            DecodedImage::Rgba(image) => self.upload_rgba(image),
            DecodedImage::Planar(picture) => self.upload_planar(encoder, picture),
        };
        self.stats.upload_time += started.elapsed();
        Ok(texture)
    }

    /// Premultiplied `f32` pixels, packed to half floats.
    fn upload_rgba(&mut self, image: &RgbaImage) -> wgpu::Texture {
        let texture = self.texture(image.width, image.height, FORMAT);
        let mut halves = vec![f16::ZERO; image.pixels.len()];
        halves.convert_from_f32_slice(&image.pixels);
        self.gpu.queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&halves),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 8),
                rows_per_image: Some(image.height),
            },
            texture.size(),
        );
        texture
    }

    /// Uploads the planes as they are and converts them on the GPU.
    fn upload_planar(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        picture: &PlanarImage,
    ) -> wgpu::Texture {
        let bytes = picture.bytes_per_sample();
        let format = if bytes == 1 {
            wgpu::TextureFormat::R8Uint
        } else {
            wgpu::TextureFormat::R16Uint
        };
        let planes: Vec<wgpu::Texture> = picture
            .planes
            .iter()
            .map(|plane| {
                let texture = self.texture(plane.width, plane.height, format);
                self.gpu.queue.write_texture(
                    texture.as_image_copy(),
                    &plane.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(plane.width * bytes),
                        rows_per_image: Some(plane.height),
                    },
                    texture.size(),
                );
                texture
            })
            .collect();
        let output = self.texture(picture.width, picture.height, FORMAT);
        let output_view = output.create_view(&Default::default());
        let plane_views: Vec<wgpu::TextureView> = planes
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&output_view),
        }];
        entries.extend(
            plane_views
                .iter()
                .enumerate()
                .map(|(k, view)| wgpu::BindGroupEntry {
                    binding: 1 + k as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                }),
        );
        self.run(
            encoder,
            LayoutKind::Planar(planes.len()),
            &unpack::planar(&picture.encoding),
            &entries,
            (picture.width, picture.height),
        );
        for plane in planes {
            self.recycle(plane);
        }
        output
    }

    fn dispatch(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        kernel: &KernelStep,
        input: &wgpu::Texture,
        extra: &[&wgpu::Texture],
        output: &wgpu::Texture,
    ) {
        let device = &self.gpu.device;
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
        self.stats.kernels += 1;
        self.run(
            encoder,
            LayoutKind::Kernel(extra.len()),
            &kernel.wgsl,
            &entries,
            (output.width(), output.height()),
        );
    }

    /// Records one dispatch of `wgsl` over `size`, building its pipeline the
    /// first time the shader is seen.
    fn run(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        kind: LayoutKind,
        wgsl: &str,
        entries: &[wgpu::BindGroupEntry],
        size: (u32, u32),
    ) {
        let device = &self.gpu.device;
        let layout = self
            .layouts
            .entry(kind)
            .or_insert_with(|| KernelLayout::new(device, kind));
        let shader_key = ContentHash::of_bytes(wgsl.as_bytes());
        let pipeline = self.pipelines.entry(shader_key).or_insert_with(|| {
            self.stats.pipelines_built += 1;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fw-kernel"),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fw-kernel"),
            layout: &layout.bind_group,
            entries,
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("fw-kernel"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
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
                let src: &[f16] = bytemuck::cast_slice(&data[start..start + row as usize]);
                let dst = &mut image.pixels[y * width as usize * 4..(y + 1) * width as usize * 4];
                src.convert_to_f32_slice(dst);
            }
        }
        buffer.unmap();
        image
    }
}
