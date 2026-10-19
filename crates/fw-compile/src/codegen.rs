//! WGSL generation: colour conversions and fused kernels.

use fw_core::color::{IDENTITY, conversion_matrix};
use fw_core::{ColorSpace, Transfer};
use fw_plugin::PixelOp;

use crate::plan::KernelOp;

/// Helpers every kernel can use. Out-of-range branches of `select` may compute
/// NaN (e.g. `pow` of a negative), but `select` never returns them.
const PRELUDE: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read> params: array<f32>;

fn prm(base: u32, i: u32) -> f32 {
    return params[base + i];
}

fn fw_mat3(base: u32) -> mat3x3<f32> {
    return mat3x3<f32>(
        vec3<f32>(prm(base, 0u), prm(base, 1u), prm(base, 2u)),
        vec3<f32>(prm(base, 3u), prm(base, 4u), prm(base, 5u)),
        vec3<f32>(prm(base, 6u), prm(base, 7u), prm(base, 8u)),
    );
}

fn fw_unpremultiply(c: vec4<f32>) -> vec4<f32> {
    if (c.a > 0.0) {
        return vec4<f32>(c.rgb / c.a, c.a);
    }
    return c;
}

fn fw_premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

fn fw_srgb_decode(v: vec3<f32>) -> vec3<f32> {
    return select(pow((v + 0.055) / 1.055, vec3<f32>(2.4)), v / 12.92, v <= vec3<f32>(0.04045));
}

fn fw_srgb_encode(v: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3<f32>(0.0031308));
}

fn fw_gamma24_decode(v: vec3<f32>) -> vec3<f32> {
    return pow(max(v, vec3<f32>(0.0)), vec3<f32>(2.4));
}

fn fw_gamma24_encode(v: vec3<f32>) -> vec3<f32> {
    return pow(max(v, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4));
}

fn fw_acescct_decode(v: vec3<f32>) -> vec3<f32> {
    return select(exp2(v * 17.52 - 9.72), (v - 0.0729055341958355) / 10.5402377416545, v <= vec3<f32>(0.155251141552511));
}

fn fw_acescct_encode(v: vec3<f32>) -> vec3<f32> {
    return select((log2(v) + 9.72) / 17.52, 10.5402377416545 * v + 0.0729055341958355, v <= vec3<f32>(0.0078125));
}
"#;

fn transfer_fn(t: Transfer, encode: bool) -> Option<&'static str> {
    let name = match (t, encode) {
        (Transfer::Linear, _) => return None,
        (Transfer::Srgb, false) => "fw_srgb_decode",
        (Transfer::Srgb, true) => "fw_srgb_encode",
        (Transfer::Gamma24, false) => "fw_gamma24_decode",
        (Transfer::Gamma24, true) => "fw_gamma24_encode",
        (Transfer::AcesCct, false) => "fw_acescct_decode",
        (Transfer::AcesCct, true) => "fw_acescct_encode",
    };
    Some(name)
}

/// A per-pixel operation converting premultiplied pixels between colour spaces,
/// or `None` when the spaces are the same.
pub fn conversion(from: ColorSpace, to: ColorSpace) -> Option<PixelOp> {
    if from == to {
        return None;
    }
    let matrix = conversion_matrix(from.primaries, to.primaries);
    let decode = transfer_fn(from.transfer, false);
    let encode = transfer_fn(to.transfer, true);
    let mut params = vec![];
    let mut body = String::new();
    // Transfer curves apply to unpremultiplied colour; a matrix alone works on premultiplied.
    let curves = decode.is_some() || encode.is_some();
    body.push_str(if curves {
        "var rgb = fw_unpremultiply(c).rgb;\n"
    } else {
        "var rgb = c.rgb;\n"
    });
    if let Some(f) = decode {
        body.push_str(&format!("rgb = {f}(rgb);\n"));
    }
    if matrix != IDENTITY {
        // Column-major, as mat3x3 is built from columns.
        params.extend((0..3).flat_map(|col| (0..3).map(move |row| matrix[row][col] as f32)));
        body.push_str("rgb = fw_mat3(base) * rgb;\n");
    }
    if let Some(f) = encode {
        body.push_str(&format!("rgb = {f}(rgb);\n"));
    }
    body.push_str(if curves {
        "return fw_premultiply(vec4<f32>(rgb, c.a));"
    } else {
        "return vec4<f32>(rgb, c.a);"
    });
    Some(PixelOp {
        label: format!("convert {from} → {to}"),
        wgsl: body,
        params,
    })
}

/// A compute shader running `ops` in order on every pixel.
/// A compute shader running `ops` in order on every pixel. Extra inputs are
/// bound from binding 3 onwards, in `extra_inputs` order.
pub fn kernel(ops: &[KernelOp], extra_inputs: usize) -> String {
    let mut out = String::from(PRELUDE);
    for k in 0..extra_inputs {
        out.push_str(&format!(
            "@group(0) @binding({}) var ex{k}: texture_2d<f32>;\n",
            3 + k
        ));
    }
    let mut base = 0usize;
    let mut calls = String::new();
    for (i, op) in ops.iter().enumerate() {
        let extra_params: String = (0..op.extra.len())
            .map(|j| format!(", e{j}: vec4<f32>"))
            .collect();
        out.push_str(&format!(
            "\n// {}\nfn op_{i}(c: vec4<f32>{extra_params}) -> vec4<f32> {{\n    let base = {base}u;\n",
            op.label
        ));
        for line in op.wgsl.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("}\n");
        let extra_args: String = op.extra.iter().map(|slot| format!(", x{slot}")).collect();
        calls.push_str(&format!("    c = op_{i}(c{extra_args});\n"));
        base += op.params.len();
    }
    out.push_str(
        "\n@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {\n    \
         let size = textureDimensions(dst);\n    \
         if (id.x >= size.x || id.y >= size.y) {\n        return;\n    }\n    \
         var c = textureLoad(src, vec2<i32>(id.xy), 0);\n",
    );
    for k in 0..extra_inputs {
        out.push_str(&format!(
            "    let x{k} = textureLoad(ex{k}, vec2<i32>(id.xy), 0);\n"
        ));
    }
    out.push_str(&calls);
    out.push_str("    textureStore(dst, vec2<i32>(id.xy), c);\n}\n");
    out
}

/// Combines a foreground (`e0`) over the background (`c`), both premultiplied,
/// or `None` for a mode we don't know.
pub fn composite(mode: &str) -> Option<PixelOp> {
    let wgsl = match mode {
        // Porter–Duff "over".
        "normal" => "return e0 + c * (1.0 - e0.a);",
        // Light adds; coverage combines like "over".
        "add" => "return vec4<f32>(c.rgb + e0.rgb, e0.a + c.a * (1.0 - e0.a));",
        _ => return None,
    };
    Some(PixelOp {
        label: format!("composite {mode}"),
        wgsl: wgsl.into(),
        params: vec![],
    })
}
