//! WGSL generation: color conversions and fused kernels.

use fw_core::color::{IDENTITY, conversion_matrix};
use fw_core::{ColorSpace, Transfer};
use fw_plugin::PixelOp;
use fw_plugin::kernel::{FusedOp, HELPERS};

use crate::plan::KernelOp;

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

/// A per-pixel operation converting premultiplied pixels between color spaces,
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
    // Transfer curves apply to unpremultiplied color; a matrix alone works on premultiplied.
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

/// WGSL for shaders outside plans, such as the viewer's: the color helpers
/// and `fn <name>(c: vec4<f32>) -> vec4<f32>`, converting premultiplied
/// pixels from `from` to `to`.
pub fn conversion_function(name: &str, from: ColorSpace, to: ColorSpace) -> String {
    let body = match conversion(from, to) {
        None => "return c;".to_owned(),
        Some(op) => {
            // The matrix inline, rather than read from a parameter buffer.
            let columns: Vec<String> = op
                .params
                .chunks(3)
                .map(|c| format!("vec3<f32>({:?}, {:?}, {:?})", c[0], c[1], c[2]))
                .collect();
            op.wgsl.replace(
                "fw_mat3(base)",
                &format!("mat3x3<f32>({})", columns.join(", ")),
            )
        }
    };
    let mut out = String::from(HELPERS);
    out.push_str(&format!("\nfn {name}(c: vec4<f32>) -> vec4<f32> {{\n"));
    for line in body.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

/// A compute shader running `ops` in order on every pixel.
pub fn kernel(ops: &[KernelOp], extra_inputs: usize) -> String {
    let ops: Vec<FusedOp<'_>> = ops
        .iter()
        .map(|op| FusedOp {
            label: &op.label,
            wgsl: &op.wgsl,
            params: op.params.len(),
            extra: &op.extra,
        })
        .collect();
    fw_plugin::kernel::kernel(&ops, extra_inputs)
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
