//! Runs compiled plans on the GPU.
//!
//! Images live in `Rgba16Float` textures, premultiplied, in whatever colour
//! space the plan says. Plan outputs and decoded files are kept in a RAM
//! cache by content key, so unchanged work is never redone.
//!
//! Decoded video arrives as planes of integer codes, or as a hardware
//! decoder's image already on the GPU, and is converted on the GPU, so the
//! CPU never touches its pixels.

mod cache;
mod display;
mod gpu;
mod unpack;

use std::collections::HashMap;
use std::sync::Arc;

use ash::vk;
use fw_compile::{
    Acquired, Decoded, Decoder, FillStep, HardwareFrame, HardwarePicture, KernelStep, LoadStep,
    Plan, Step,
};
use fw_core::{ColorSpace, ContentHash, DecodedImage, PlanarImage, RgbaImage};
use fw_plugin::ImageHandle;
use half::f16;
use half::slice::HalfFloatSliceExt;

pub use cache::{Cached, DEFAULT_GPU_CACHE_BYTES, RamCache};
pub use display::{Display, DisplayView, SurfaceEncoding};
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
    /// Textures whose queued uploads haven't been submitted yet. They return
    /// to the pool only after the submit: uploads run at the start of the
    /// next submit, so reusing one within a plan would overwrite it.
    uploading: Vec<wgpu::Texture>,
    /// Hardware pictures acquired for the plan being recorded, released by
    /// its submit or abandoned if it fails. Each is acquired at most once a
    /// plan: twice would make its two waits depend on each other.
    acquired: Vec<(Arc<dyn HardwareFrame>, Acquired)>,
    /// Textures wrapping those pictures' images, by frame. Never pooled.
    wrapped: HashMap<usize, wgpu::Texture>,
    stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LayoutKind {
    /// A plan's kernel with this many extra inputs.
    /// Binding 0: the main input. 1: the output. 2: parameters. 3…: extra inputs.
    Kernel(usize),
    /// Unpacking this many planes. Binding 0: the output. 1…: the planes.
    Planar(usize),
    /// Converting a hardware decoder's image. Binding 0: the output.
    /// 1: luma. 2: interleaved chroma.
    TwoPlane,
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
            LayoutKind::TwoPlane => vec![output(0), texture(1, float), texture(2, float)],
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
            uploading: Vec::new(),
            acquired: Vec::new(),
            wrapped: HashMap::new(),
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
        self.submitted();
        if let Some(error) = pollster::block_on(scope.pop()) {
            self.flush_rejected();
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
        self.submit(encoder);
        self.submitted();
        // Lets wgpu free the frame's finished buffers and bind groups.
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        if let Some(error) = pollster::block_on(scope.pop()) {
            self.flush_rejected();
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
        // Left over from a plan that failed before its submit.
        self.submitted();
        self.abandon();
        let result = self.record(plan, decoder);
        if result.is_err() {
            self.abandon();
        }
        result
    }

    fn record(
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

    /// Submits a plan's work. Hardware pictures it read are waited for, and
    /// their decoders told when reading is done, by semaphores staged under
    /// the submit lock so no other submit can take them.
    fn submit(&mut self, encoder: wgpu::CommandEncoder) {
        let gpu = self.gpu.clone();
        let _submits = gpu.lock_submits();
        if !self.acquired.is_empty() {
            // SAFETY: only staging semaphores for the next submit.
            let queue = unsafe { gpu.queue.as_hal::<wgpu::hal::api::Vulkan>() }
                .expect("hardware pictures only exist on Vulkan");
            for (_, a) in &self.acquired {
                let semaphore = vk::Handle::from_raw(a.semaphore);
                queue.add_wait_semaphore(
                    semaphore,
                    Some(a.wait),
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                );
                queue.add_signal_semaphore(semaphore, Some(a.signal));
            }
        }
        gpu.queue.submit([encoder.finish()]);
        self.acquired.clear();
        // wgpu destroys them once the GPU is done, returning the frames.
        self.wrapped.clear();
    }

    /// After wgpu rejects a submit, the semaphore waits and signals staged
    /// for it are still pending. An empty submit carries them, so no decoder
    /// waits forever for a signal.
    fn flush_rejected(&self) {
        let _submits = self.gpu.lock_submits();
        self.gpu.queue.submit([]);
    }

    /// Lets the decoders of pictures acquired for a plan that won't be
    /// submitted go on.
    fn abandon(&mut self) {
        for (frame, acquired) in self.acquired.drain(..) {
            frame.abandon(&acquired);
        }
        self.wrapped.clear();
    }

    /// Returns textures held for uploads to the pool, once their plan is submitted.
    fn submitted(&mut self) {
        for texture in std::mem::take(&mut self.uploading) {
            self.recycle(texture);
        }
    }

    /// Returns a texture to the pool. Reusing it in a later submit is safe:
    /// the queue runs work in order.
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
                // An editing copy may be smaller; it's scaled up on the GPU.
                let smaller_copy = load.copy.is_some()
                    && matches!(decoded, Decoded::Cpu(DecodedImage::Planar(_)))
                    && width <= load.width
                    && height <= load.height;
                if (width, height) != (load.width, load.height) && !smaller_copy {
                    return Err(error(format!(
                        "expected {}×{}, decoded {width}×{height}",
                        load.width, load.height
                    )));
                }
                if let Decoded::Cpu(DecodedImage::Rgba(image)) = &mut decoded {
                    image.premultiply();
                }
                let decoded = Arc::new(decoded);
                self.cache
                    .insert(load.key, Cached::Decoded(decoded.clone()));
                decoded
            }
        };
        let started = std::time::Instant::now();
        let size = (load.width, load.height);
        let texture = match &*decoded {
            Decoded::Cpu(DecodedImage::Rgba(image)) => self.upload_rgba(image),
            Decoded::Cpu(DecodedImage::Planar(picture)) => {
                self.upload_planar(encoder, picture, size)
            }
            Decoded::Hardware(picture) => {
                self.upload_hardware(encoder, picture, size)
                    .map_err(|message| RenderError::Decode {
                        path: load.path.display().to_string(),
                        message,
                    })?
            }
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

    /// Uploads the planes as they are and converts them on the GPU to a
    /// `size` texture, resampling if the picture is a different size.
    fn upload_planar(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        picture: &PlanarImage,
        size: (u32, u32),
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
        let output = self.texture(size.0, size.1, FORMAT);
        let output_view = output.create_view(&Default::default());
        let scale = (size != (picture.width, picture.height)).then(|| {
            [
                picture.width as f32 / size.0 as f32,
                picture.height as f32 / size.1 as f32,
            ]
        });
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
            &unpack::planar(&picture.encoding, scale, &unpack::Source::Planes),
            &entries,
            size,
        );
        self.uploading.extend(planes);
        output
    }

    /// Converts a hardware decoder's image where it lies. The image is
    /// acquired (once per plan) and wrapped as a texture until the submit.
    fn upload_hardware(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        picture: &HardwarePicture,
        size: (u32, u32),
    ) -> Result<wgpu::Texture, String> {
        let wide = picture.encoding.bytes_per_sample() == 2;
        let (format, luma_format, chroma_format, max) = if wide {
            (
                wgpu::TextureFormat::P010,
                wgpu::TextureFormat::R16Unorm,
                wgpu::TextureFormat::Rg16Unorm,
                65535.0,
            )
        } else {
            (
                wgpu::TextureFormat::NV12,
                wgpu::TextureFormat::R8Unorm,
                wgpu::TextureFormat::Rg8Unorm,
                255.0,
            )
        };
        let id = Arc::as_ptr(&picture.frame) as *const () as usize;
        if !self.wrapped.contains_key(&id) {
            let acquired = picture.frame.acquire()?;
            self.acquired.push((picture.frame.clone(), acquired));
            let texture = self.wrap(picture, acquired.image, format);
            self.wrapped.insert(id, texture);
        }
        let image = &self.wrapped[&id];
        let plane = |format, aspect| {
            image.create_view(&wgpu::TextureViewDescriptor {
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let luma = plane(luma_format, wgpu::TextureAspect::Plane0);
        let chroma = plane(chroma_format, wgpu::TextureAspect::Plane1);
        let output = self.texture(size.0, size.1, FORMAT);
        let output_view = output.create_view(&Default::default());
        let (w, h) = (picture.width, picture.height);
        let scale = (size != (w, h)).then(|| [w as f32 / size.0 as f32, h as f32 / size.1 as f32]);
        let source = unpack::Source::TwoPlane {
            max,
            luma_last: (w - 1, h - 1),
            chroma_last: (w.div_ceil(2) - 1, h.div_ceil(2) - 1),
        };
        let entries = [
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&output_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&luma),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&chroma),
            },
        ];
        self.run(
            encoder,
            LayoutKind::TwoPlane,
            &unpack::planar(&picture.encoding, scale, &source),
            &entries,
            size,
        );
        Ok(output)
    }

    /// A texture over a hardware picture's image, which keeps the frame
    /// alive until wgpu has finished with it. The image is in the layout
    /// `acquire` left it in, which wgpu is told.
    fn wrap(
        &self,
        picture: &HardwarePicture,
        image: u64,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width: picture.coded.0,
            height: picture.coded.1,
            depth_or_array_layers: 1,
        };
        let frame = picture.frame.clone();
        // SAFETY: the image is the frame's, `size` and `format` describe it,
        // and the frame (so the image) lives until the drop callback.
        unsafe {
            let hal = self
                .gpu
                .device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .expect("hardware pictures only exist on Vulkan")
                .texture_from_raw(
                    vk::Handle::from_raw(image),
                    &wgpu::hal::TextureDescriptor {
                        label: Some("fw-hardware-picture"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUses::RESOURCE,
                        memory_flags: wgpu::hal::MemoryFlags::empty(),
                        view_formats: vec![],
                    },
                    Some(Box::new(move || drop(frame))),
                    wgpu::hal::vulkan::TextureMemory::External,
                );
            self.gpu
                .device
                .create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                    hal,
                    &wgpu::TextureDescriptor {
                        label: Some("fw-hardware-picture"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::TextureUses::RESOURCE,
                )
        }
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
    fn read_back(
        &mut self,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> RgbaImage {
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
        self.submit(encoder);
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
