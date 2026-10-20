//! Output: marks what a graph delivers, and in which colour space.
//!
//! Writing files is not the node's job. Sinks stay out of the pure graph; the
//! caller (the CLI, the render queue) writes what the plan produces.

use fw_plugin::{
    ImageBehaviour, ImageHandle, NodeError, NodeType, NodeTypeDesc, OUTPUT_TYPE, ParamDesc,
    PlanBuilder, PortDesc, PortType, Request, ResolvedParams, SpaceNeed,
};

use crate::color::{SPACE_CHOICES, space_from_choice};

pub fn node_type() -> NodeType {
    let desc = NodeTypeDesc {
        id: OUTPUT_TYPE.into(),
        version: 1,
        name: "Output".into(),
        category: "Outputs".into(),
        inputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        outputs: vec![],
        params: vec![ParamDesc::choice(
            "space",
            "Colour space",
            &SPACE_CHOICES,
            "auto",
        )],
        varies_with_time: false,
        container: None,
        can_hold_graph: false,
    };
    NodeType::new(desc).with_image(Output, "output r1")
}

struct Output;

impl ImageBehaviour for Output {
    fn input_space(&self, params: &ResolvedParams, _index: usize) -> SpaceNeed {
        match space_from_choice(params.text("space")) {
            Some(space) => SpaceNeed::Exact(space),
            None => SpaceNeed::Delivery,
        }
    }

    fn emit(
        &self,
        _params: &ResolvedParams,
        _request: &Request,
        inputs: &[ImageHandle],
        _plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        Ok(inputs[0])
    }
}
