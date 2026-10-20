//! The text of fused per-pixel kernels. The per-pixel function contract is
//! part of the node API ([`crate::PixelOp`]), so it lives here, where shader
//! plugins can check their code against the real thing.

/// Every kernel's bindings, and its parameter helpers.
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
"#;

/// Colour helpers every kernel can use. Out-of-range branches of `select` may
/// compute NaN (e.g. `pow` of a negative), but `select` never returns them.
pub const HELPERS: &str = r#"

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

/// How many parameters every kernel starts with: the image's region, before
/// its operations' own.
pub const FRAME_PARAMS: usize = 4;

/// One operation of a fused kernel, as far as the kernel's text goes.
pub struct FusedOp<'a> {
    /// Shown in a comment above the operation's function.
    pub label: &'a str,
    /// The body, as in [`crate::PixelOp::wgsl`].
    pub wgsl: &'a str,
    /// How many parameters it reads.
    pub params: usize,
    /// For each of its extra inputs (`e0`, `e1`, …), the kernel's binding
    /// slot for it.
    pub extra: &'a [usize],
}

/// A compute shader running `ops` in order on every pixel. Extra inputs are
/// bound from binding 3 onwards, in slot order. Parameters come from one
/// buffer: the image's region (`min.x, min.y, max.x, max.y`), then each
/// operation's in order.
pub fn kernel(ops: &[FusedOp<'_>], extra_inputs: usize) -> String {
    let mut out = String::from(PRELUDE);
    out.push_str(HELPERS);
    for k in 0..extra_inputs {
        out.push_str(&format!(
            "@group(0) @binding({}) var ex{k}: texture_2d<f32>;\n",
            3 + k
        ));
    }
    let mut base = FRAME_PARAMS;
    let mut calls = String::new();
    for (i, op) in ops.iter().enumerate() {
        let extra_params: String = (0..op.extra.len())
            .map(|j| format!(", e{j}: vec4<f32>"))
            .collect();
        out.push_str(&format!(
            "\n// {}\nfn op_{i}(c: vec4<f32>{extra_params}, pos: vec2<f32>, frame: vec4<f32>) -> vec4<f32> {{\n    let base = {base}u;\n",
            op.label
        ));
        for line in op.wgsl.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("}\n");
        let extra_args: String = op.extra.iter().map(|slot| format!(", x{slot}")).collect();
        calls.push_str(&format!("    c = op_{i}(c{extra_args}, pos, frame);\n"));
        base += op.params;
    }
    out.push_str(
        "\n@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {\n    \
         let size = textureDimensions(dst);\n    \
         if (id.x >= size.x || id.y >= size.y) {\n        return;\n    }\n    \
         let frame = vec4<f32>(params[0], params[1], params[2], params[3]);\n    \
         let pos = frame.xy + (vec2<f32>(id.xy) + 0.5) * (frame.zw - frame.xy) / vec2<f32>(size);\n    \
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
