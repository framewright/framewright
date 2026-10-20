//! Kernels that put decoded pictures on the GPU: planes of integer codes to
//! premultiplied RGBA in the stream's own encoding, and solid fills.
//!
//! The constants come from [`PlanarEncoding::coefficients`], the same ones
//! the CPU reference ([`fw_core::PlanarImage::to_rgba`]) uses.

use fw_core::planar::{PlanarEncoding, Siting};

/// Where a picture's codes are.
pub(crate) enum Source {
    /// One integer texture per component (Y, Cb, Cr or R, G, B, then alpha),
    /// each exactly its plane's size: decoded planes we uploaded.
    Planes,
    /// A hardware decoder's image, viewed as a luma and an interleaved
    /// Cb/Cr texture of normalised values, codes being `value × max`. The
    /// image may be padded beyond the picture, so the last sample of each
    /// plane is given rather than taken from the texture.
    TwoPlane {
        max: f32,
        luma_last: (u32, u32),
        chroma_last: (u32, u32),
    },
}

/// Binding 0 is the output; the picture's textures follow from binding 1.
/// With `scale` (picture size over output size, per axis) the picture is
/// resampled bilinearly to fill the output, as for an editing copy smaller
/// than its original; without it, codes are read exactly, one per output
/// pixel.
pub(crate) fn planar(e: &PlanarEncoding, scale: Option<[f32; 2]>, source: &Source) -> String {
    let c = e.coefficients();
    let mut w = String::new();
    w.push_str("@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;\n");
    match source {
        Source::Planes => {
            let planes = if e.alpha { 4 } else { 3 };
            let mut code = String::new();
            let mut last = String::new();
            for i in 0..planes {
                w.push_str(&format!(
                    "@group(0) @binding({}) var p{i}: texture_2d<u32>;\n",
                    i + 1
                ));
                code.push_str(&format!(
                    "        case {i}u: {{ return f32(textureLoad(p{i}, p, 0).r); }}\n"
                ));
                last.push_str(&format!(
                    "        case {i}u: {{ return textureDimensions(p{i}) - vec2<u32>(1u); }}\n"
                ));
            }
            w.push_str(&format!(
                r#"
// Component `c`'s code at `p`.
fn code(c: u32, p: vec2<u32>) -> f32 {{
    switch c {{
{code}        default: {{ return 0.0; }}
    }}
}}

// Component `c`'s last sample.
fn last(c: u32) -> vec2<u32> {{
    switch c {{
{last}        default: {{ return vec2<u32>(0u); }}
    }}
}}
"#
            ));
        }
        Source::TwoPlane {
            max,
            luma_last,
            chroma_last,
        } => {
            w.push_str(&format!(
                r#"@group(0) @binding(1) var y_plane: texture_2d<f32>;
@group(0) @binding(2) var cbcr_plane: texture_2d<f32>;

// Component `c`'s code at `p`. Normalised values back to exact codes.
fn code(c: u32, p: vec2<u32>) -> f32 {{
    switch c {{
        case 0u: {{ return round(textureLoad(y_plane, p, 0).r * {max:?}); }}
        case 1u: {{ return round(textureLoad(cbcr_plane, p, 0).r * {max:?}); }}
        default: {{ return round(textureLoad(cbcr_plane, p, 0).g * {max:?}); }}
    }}
}}

// Component `c`'s last sample within the picture.
fn last(c: u32) -> vec2<u32> {{
    if (c == 0u) {{
        return vec2<u32>({}u, {}u);
    }}
    return vec2<u32>({}u, {}u);
}}
"#,
                luma_last.0, luma_last.1, chroma_last.0, chroma_last.1
            ));
        }
    }
    let first = |shift: u32, siting: Siting| match siting {
        Siting::Cosited => 0.0,
        Siting::Centred => ((1u32 << shift) as f32 - 1.0) / 2.0,
    };
    let (sx, sy) = e.chroma_shift;
    w.push_str(
        r#"
// Bilinear between the four codes of component `c` around `u`, in its own
// sample grid.
fn bilinear(c: u32, u: vec2<f32>) -> f32 {
    let end = last(c);
    let x = max(u, vec2<f32>(0.0));
    let i = min(vec2<u32>(floor(x)), end);
    let j = min(i + vec2<u32>(1u), end);
    let f = x - vec2<f32>(i);
    let top = code(c, i) * (1.0 - f.x) + code(c, vec2<u32>(j.x, i.y)) * f.x;
    let bottom = code(c, vec2<u32>(i.x, j.y)) * (1.0 - f.x) + code(c, j) * f.x;
    return top * (1.0 - f.y) + bottom * f.y;
}
"#,
    );
    // Where each output pixel falls in the picture's luma grid.
    let (position, luma, alpha) = match scale {
        None => ("vec2<f32>(p)".to_owned(), "code(0u, p)", "code(3u, p)"),
        Some([kx, ky]) => (
            format!("(vec2<f32>(p) + 0.5) * vec2<f32>({kx:?}, {ky:?}) - 0.5"),
            "bilinear(0u, s)",
            "bilinear(3u, s)",
        ),
    };
    w.push_str(&format!(
        r#"
// Chroma component `c` at luma position `s`, honouring siting.
fn chroma(c: u32, s: vec2<f32>) -> f32 {{
    return bilinear(c, (s - vec2<f32>({fx:?}, {fy:?})) / vec2<f32>({kx:?}, {ky:?}));
}}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let p = id.xy;
    if (any(p >= textureDimensions(dst))) {{
        return;
    }}
    let s = {position};
    let codes = vec3<f32>({luma}, chroma(1u, s), chroma(2u, s));
    let v = (codes - vec3<f32>({o0:?}, {o1:?}, {o2:?})) / vec3<f32>({s0:?}, {s1:?}, {s2:?});
"#,
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
        w.push_str(&format!("    let a = {alpha} / {:?};\n", c.alpha_max));
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

#[cfg(test)]
mod tests {
    use fw_core::planar::{PlanarModel, YuvMatrix, YuvRange};

    use super::*;

    fn validate(wgsl: &str) {
        let module = naga::front::wgsl::parse_str(wgsl)
            .unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
    }

    #[test]
    fn every_kind_of_conversion_is_valid_wgsl() {
        let yuv = PlanarEncoding {
            model: PlanarModel::Yuv {
                matrix: YuvMatrix::Bt709,
                range: YuvRange::Limited,
            },
            bits: 8,
            shift: 0,
            chroma_shift: (1, 1),
            siting: (Siting::Cosited, Siting::Centred),
            alpha: false,
        };
        let rgba = PlanarEncoding {
            model: PlanarModel::Rgb,
            bits: 8,
            shift: 8,
            chroma_shift: (0, 0),
            alpha: true,
            ..yuv
        };
        let p010 = PlanarEncoding {
            bits: 10,
            shift: 6,
            ..yuv
        };
        let two_plane = Source::TwoPlane {
            max: 65535.0,
            luma_last: (1919, 1079),
            chroma_last: (959, 539),
        };
        for scale in [None, Some([0.5, 0.5])] {
            for e in [&yuv, &rgba] {
                validate(&planar(e, scale, &Source::Planes));
            }
            for e in [&yuv, &p010] {
                validate(&planar(e, scale, &two_plane));
            }
        }
        validate(&fill([0.0, 0.5, 1.0, 1.0]));
    }
}
