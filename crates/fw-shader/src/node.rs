//! A shader node's behaviour: its WGSL body wrapped so it sees its inputs and
//! parameters by name, added to plans as a per-pixel operation.

use std::sync::Arc;

use fw_plugin::kernel::{FusedOp, kernel};
use fw_plugin::{
    ImageBehaviour, ImageHandle, NodeError, NodeTypeDesc, ParamDesc, ParamKind, ParamValue,
    PixelOp, PlanBuilder, Request, ResolvedParams,
};

/// How many parameter slots a parameter of `kind` takes, and the WGSL that
/// reads it from slot `at`.
fn slots(kind: &ParamKind, at: usize) -> (usize, String) {
    let read = |i: usize| format!("prm(base, {}u)", at + i);
    match kind {
        ParamKind::Float { .. } => (1, read(0)),
        ParamKind::Int | ParamKind::Choice { .. } => (1, format!("i32({})", read(0))),
        ParamKind::Bool => (1, format!("{} != 0.0", read(0))),
        ParamKind::Vec2 { .. } => (2, format!("vec2<f32>({}, {})", read(0), read(1))),
        ParamKind::Color => (
            4,
            format!(
                "vec4<f32>({}, {}, {}, {})",
                read(0),
                read(1),
                read(2),
                read(3)
            ),
        ),
        ParamKind::Text | ParamKind::Media => unreachable!("manifests only declare numeric kinds"),
    }
}

/// The per-pixel function body for a shader node: a `let` for each other
/// input and each parameter, then the plugin's own body. Returns the body
/// and how many lines come before the plugin's.
pub(crate) fn op_body(desc: &NodeTypeDesc, shader: &str) -> (String, usize) {
    let mut lines = vec![];
    for (j, input) in desc.inputs.iter().skip(1).enumerate() {
        lines.push(format!("let {} = e{j};", input.id));
    }
    let mut at = 0;
    for param in &desc.params {
        let (count, read) = slots(&param.kind, at);
        lines.push(format!("let {} = {read};", param.id));
        at += count;
    }
    let before = lines.len();
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(shader);
    (body, before)
}

/// Checks a shader node's body by building the kernel it would go into and
/// validating that. Problems point into the shader file where they can.
pub(crate) fn validate(desc: &NodeTypeDesc, shader: &str, file: &str) -> Result<(), String> {
    let (body, before) = op_body(desc, shader);
    let extra: Vec<usize> = (0..desc.inputs.len() - 1).collect();
    let op = FusedOp {
        label: &desc.name,
        wgsl: &body,
        params: desc.params.iter().map(|p| slots(&p.kind, 0).0).sum(),
        extra: &extra,
    };
    let source = kernel(&[op], extra.len());
    // The body starts two lines below `fn op_0`, indented by four spaces.
    let first_line = source
        .lines()
        .position(|l| l.starts_with("fn op_0("))
        .expect("the kernel defines its op")
        + 2;
    let place = |location: Option<naga::SourceLocation>| -> String {
        let Some(location) = location else {
            return String::new();
        };
        let line = location.line_number as usize - 1;
        match line.checked_sub(first_line + before) {
            Some(n) if n < shader.lines().count() => format!(
                "{file}:{}:{}: ",
                n + 1,
                (location.line_position as usize).saturating_sub(4).max(1)
            ),
            _ if line >= first_line && line < first_line + before => {
                "in the inputs and parameters given to the shader: ".into()
            }
            _ => String::new(),
        }
    };
    let module = naga::front::wgsl::parse_str(&source)
        .map_err(|e| format!("{}{}", place(e.location(&source)), e.message()))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| {
        let mut message = e.as_inner().to_string();
        let mut cause = std::error::Error::source(e.as_inner());
        while let Some(c) = cause {
            message.push_str(": ");
            message.push_str(&c.to_string());
            cause = c.source();
        }
        format!("{}{message}", place(e.location(&source)))
    })?;
    Ok(())
}

/// Adds a shader node's body to plans, or reports why it can't.
pub(crate) struct ShaderNode {
    pub label: String,
    /// The parameters, in the order the body reads them.
    pub params: Vec<ParamDesc>,
    /// What [`op_body`] made, ready for every plan.
    pub wgsl: Arc<str>,
    /// Why the shader can't be used, when it can't.
    pub problem: Option<String>,
}

impl ImageBehaviour for ShaderNode {
    fn emit(
        &self,
        params: &ResolvedParams,
        _request: &Request,
        inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        if let Some(problem) = &self.problem {
            return Err(NodeError(format!("its shader doesn't work: {problem}")));
        }
        let space = plan.info(inputs[0]).space;
        let mut values = vec![];
        for desc in &self.params {
            let value = params.get(&desc.id).unwrap_or(&desc.default);
            match (&desc.kind, value) {
                (ParamKind::Choice { options }, ParamValue::Text(chosen)) => {
                    let index = options.iter().position(|o| o.id == *chosen).unwrap_or(0);
                    values.push(index as f32);
                }
                (_, ParamValue::Float(v)) => values.push(*v as f32),
                (_, ParamValue::Int(v)) => values.push(*v as f32),
                (_, ParamValue::Bool(b)) => values.push(if *b { 1.0 } else { 0.0 }),
                (_, ParamValue::Vec2(v)) => values.extend(v.map(|v| v as f32)),
                // Colours arrive in the space the node works in.
                (_, ParamValue::Color(color)) => {
                    let [r, g, b, a] = color.rgba;
                    let [r, g, b] = color.space.convert([r, g, b], space);
                    values.extend([r, g, b, a].map(|v| v as f32));
                }
                (_, ParamValue::Text(_) | ParamValue::Media(_)) => {
                    unreachable!("manifests only declare numeric kinds and choices")
                }
            }
        }
        let op = PixelOp {
            label: self.label.clone(),
            wgsl: self.wgsl.to_string(),
            params: values,
        };
        Ok(plan.per_pixel_with(inputs[0], &inputs[1..], op))
    }
}
