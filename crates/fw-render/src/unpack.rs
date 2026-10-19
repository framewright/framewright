//! Kernels that put decoded pictures on the GPU: planes of integer codes to
//! premultiplied RGBA in the stream's own encoding, and solid fills.
//!
//! The constants come from [`PlanarEncoding::coefficients`], the same ones
//! the CPU reference ([`fw_core::PlanarImage::to_rgba`]) uses.

use fw_core::planar::{PlanarEncoding, Siting};

/// Binding 0 is the output; planes follow from binding 1.
pub(crate) fn planar(e: &PlanarEncoding) -> String {
    let c = e.coefficients();
    let planes = if e.alpha { 4 } else { 3 };
    let mut w = String::new();
    w.push_str("@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;\n");
    for i in 0..planes {
        w.push_str(&format!(
            "@group(0) @binding({}) var p{i}: texture_2d<u32>;\n",
            i + 1
        ));
    }
    let first = |shift: u32, siting: Siting| match siting {
        Siting::Cosited => 0.0,
        Siting::Centred => ((1u32 << shift) as f32 - 1.0) / 2.0,
    };
    let (sx, sy) = e.chroma_shift;
    w.push_str(&format!(
        r#"
fn code(t: texture_2d<u32>, p: vec2<u32>) -> f32 {{
    return f32(textureLoad(t, p, 0).r);
}}

// Bilinear between the chroma samples either side of luma position `p`.
fn chroma(t: texture_2d<u32>, p: vec2<u32>) -> f32 {{
    if ({subsampled}) {{
        let last = textureDimensions(t) - vec2<u32>(1u);
        let u = max((vec2<f32>(p) - vec2<f32>({fx:?}, {fy:?})) / vec2<f32>({kx:?}, {ky:?}), vec2<f32>(0.0));
        let i = min(vec2<u32>(floor(u)), last);
        let j = min(i + vec2<u32>(1u), last);
        let f = u - vec2<f32>(i);
        let top = code(t, i) * (1.0 - f.x) + code(t, vec2<u32>(j.x, i.y)) * f.x;
        let bottom = code(t, vec2<u32>(i.x, j.y)) * (1.0 - f.x) + code(t, j) * f.x;
        return top * (1.0 - f.y) + bottom * f.y;
    }}
    return code(t, p);
}}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let p = id.xy;
    if (any(p >= textureDimensions(dst))) {{
        return;
    }}
    let codes = vec3<f32>(code(p0, p), chroma(p1, p), chroma(p2, p));
    let v = (codes - vec3<f32>({o0:?}, {o1:?}, {o2:?})) / vec3<f32>({s0:?}, {s1:?}, {s2:?});
"#,
        subsampled = sx > 0 || sy > 0,
        fx = first(sx, e.siting.0),
        fy = first(sy, e.siting.1),
        kx = (1u32 << sx) as f32,
        ky = (1u32 << sy) as f32,
        o0 = c.offset[0],
        o1 = c.offset[1],
        o2 = c.offset[2],
        s0 = c.scale[0],
        s1 = c.scale[1],
        s2 = c.scale[2],
    ));
    match c.kr_kb {
        None => w.push_str("    let rgb = v;\n"),
        Some((kr, kb)) => w.push_str(&format!(
            r#"    let r = v.x + 2.0 * (1.0 - {kr:?}) * v.z;
    let b = v.x + 2.0 * (1.0 - {kb:?}) * v.y;
    let g = (v.x - {kr:?} * r - {kb:?} * b) / (1.0 - {kr:?} - {kb:?});
    let rgb = vec3<f32>(r, g, b);
"#
        )),
    }
    if e.alpha {
        w.push_str(&format!("    let a = code(p3, p) / {:?};\n", c.alpha_max));
    } else {
        w.push_str("    let a = 1.0;\n");
    }
    w.push_str("    textureStore(dst, p, vec4<f32>(rgb * a, a));\n}\n");
    w
}

/// Fills binding 0 with one premultiplied colour.
pub(crate) fn fill(rgba: [f32; 4]) -> String {
    let [r, g, b, a] = rgba;
    format!(
        r#"@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if (all(id.xy < textureDimensions(dst))) {{
        textureStore(dst, id.xy, vec4<f32>({r:?}, {g:?}, {b:?}, {a:?}));
    }}
}}
"#
    )
}
