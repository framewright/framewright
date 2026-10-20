//! Exposure: scales scene-linear light by a number of stops.

use fw_plugin::{
    ImageBehaviour, ImageHandle, NodeError, NodeType, NodeTypeDesc, ParamDesc, PixelOp,
    PlanBuilder, PortDesc, PortType, Request, ResolvedParams, Unit,
};

/// Premultiplied, linear light: scaling colour leaves alpha alone.
const WGSL: &str = "return vec4<f32>(c.rgb * exp2(prm(base, 0u)), c.a);";

pub fn node_type() -> NodeType {
    let desc = NodeTypeDesc {
        id: "fw.exposure".into(),
        version: 1,
        name: "Exposure".into(),
        category: "Colour".into(),
        inputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![
            ParamDesc::float("exposure", "Exposure", Unit::Stops, 0.0)
                .soft_range(-5.0, 5.0)
                .hard_range(-64.0, 64.0),
        ],
        varies_with_time: false,
        container: None,
    };
    NodeType::new(desc).with_image(Exposure, WGSL)
}

struct Exposure;

impl ImageBehaviour for Exposure {
    fn emit(
        &self,
        params: &ResolvedParams,
        _request: &Request,
        inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        let op = PixelOp {
            label: "exposure".into(),
            wgsl: WGSL.into(),
            params: vec![params.f64("exposure") as f32],
        };
        Ok(plan.per_pixel(inputs[0], op))
    }
}
