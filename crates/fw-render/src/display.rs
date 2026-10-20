//! Showing a rendered frame in a window: the display boundary (design §10).
//!
//! The frame is drawn straight from its texture inside the UI's own render
//! pass, so nothing is copied. Pixels go from the frame's color space to
//! linear Rec.709, over a background, then are encoded for the window's
//! surface. Exports never pass through here.
//!
//! TODO: a real display and view transform in place of the sRGB curve.

use fw_core::ColorSpace;

use crate::GpuImage;

/// How the window's surface expects color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceEncoding {
    /// An `…Srgb` format: the hardware applies the sRGB curve.
    SrgbFormat,
    /// A plain `Unorm` format (8 or 10 bits): the shader applies the curve.
    Unorm,
    /// A float format: extended linear (scRGB), values may pass 1.
    LinearFloat,
}

impl SurfaceEncoding {
    pub fn of(format: wgpu::TextureFormat) -> SurfaceEncoding {
        if format.is_srgb() {
            SurfaceEncoding::SrgbFormat
        } else if matches!(
            format,
            wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float
        ) {
            SurfaceEncoding::LinearFloat
        } else {
            SurfaceEncoding::Unorm
        }
    }
}

/// Where the frame lands and what's behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayView {
    /// The part of the frame shown, in texture coordinates (0–1).
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    /// Shown through transparent pixels: linear Rec.709.
    pub background: [f32; 3],
}

impl Default for DisplayView {
    fn default() -> DisplayView {
        DisplayView {
            uv_offset: [0.0; 2],
            uv_scale: [1.0; 2],
            background: [0.0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    uv_offset: [f32; 2],
    uv_scale: [f32; 2],
    background: [f32; 4],
    /// 0: hardware sRGB, 1: apply the sRGB curve, 2: linear float.
    encoding: u32,
    _pad: [u32; 3],
}

/// Draws one frame at a time into a render pass for surfaces of one format.
pub struct Display {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    /// One pipeline per color space frames have arrived in.
    pipelines: Vec<(ColorSpace, wgpu::RenderPipeline)>,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    /// The frame drawn: its serial, space and bindings.
    current: Option<(u64, ColorSpace, wgpu::BindGroup)>,
}

impl Display {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Display {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fw-display"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fw-display"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fw-display"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fw-display"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Display {
            format,
            layout,
            pipeline_layout,
            pipelines: Vec::new(),
            uniforms,
            sampler,
            current: None,
        }
    }

    /// Points the display at `image`. `serial` identifies the image: when it
    /// changes, the image is bound afresh.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &GpuImage,
        serial: u64,
        view: DisplayView,
    ) {
        if !self.pipelines.iter().any(|(s, _)| *s == image.space) {
            let pipeline = self.pipeline(device, image.space);
            self.pipelines.push((image.space, pipeline));
        }
        if self.current.as_ref().map(|c| c.0) != Some(serial) {
            let texture = image
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fw-display"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&texture),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.current = Some((serial, image.space, bind));
        }
        let [r, g, b] = view.background;
        let uniforms = Uniforms {
            uv_offset: view.uv_offset,
            uv_scale: view.uv_scale,
            background: [r, g, b, 1.0],
            encoding: match SurfaceEncoding::of(self.format) {
                SurfaceEncoding::SrgbFormat => 0,
                SurfaceEncoding::Unorm => 1,
                SurfaceEncoding::LinearFloat => 2,
            },
            _pad: [0; 3],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Draws the prepared frame over the pass's whole viewport; the caller
    /// sets the viewport to where the frame goes.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some((_, space, bind)) = &self.current else {
            return;
        };
        let Some((_, pipeline)) = self.pipelines.iter().find(|(s, _)| s == space) else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    fn pipeline(&self, device: &wgpu::Device, space: ColorSpace) -> wgpu::RenderPipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fw-display"),
            source: wgpu::ShaderSource::Wgsl(shader(space).into()),
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fw-display"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

/// The display shader for frames in `space`.
fn shader(space: ColorSpace) -> String {
    let mut wgsl = fw_compile::conversion_function("to_linear", space, ColorSpace::LINEAR_REC709);
    wgsl.push_str(
        r#"
struct View {
    uv_offset: vec2<f32>,
    uv_scale: vec2<f32>,
    background: vec4<f32>,
    encoding: u32,
};
@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var frame: texture_2d<f32>;
@group(0) @binding(2) var frame_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the viewport.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VertexOut {
    let t = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(t.x * 2.0 - 1.0, 1.0 - t.y * 2.0, 0.0, 1.0);
    out.uv = view.uv_offset + t * view.uv_scale;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let c = to_linear(textureSample(frame, frame_sampler, in.uv));
    let rgb = c.rgb + view.background.rgb * (1.0 - c.a);
    switch view.encoding {
        case 1u: {
            return vec4<f32>(fw_srgb_encode(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
        }
        case 2u: {
            return vec4<f32>(rgb, 1.0);
        }
        default: {
            return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
        }
    }
}
"#,
    );
    wgsl
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_shaders_are_valid_wgsl() {
        for space in [
            ColorSpace::LINEAR_REC709,
            ColorSpace::SRGB,
            ColorSpace::ACESCG,
            ColorSpace::ACESCCT,
        ] {
            let wgsl = shader(space);
            let module = naga::front::wgsl::parse_str(&wgsl)
                .unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(&wgsl)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
        }
    }
}
