//! Read Image: a still image file as a source.

use fw_plugin::{
    ImageBehaviour, ImageHandle, NodeError, NodeType, NodeTypeDesc, ParamDesc, PlanBuilder,
    PortDesc, PortType, Request, ResolvedParams,
};

use crate::color::{SPACE_CHOICES, space_from_choice};

pub fn node_type() -> NodeType {
    let desc = NodeTypeDesc {
        id: "fw.read_image".into(),
        version: 1,
        name: "Read Image".into(),
        category: "Sources".into(),
        inputs: vec![],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![
            ParamDesc::media("file", "File"),
            ParamDesc::choice("space", "Colour space", &SPACE_CHOICES, "auto"),
        ],
        varies_with_time: false,
        container: None,
        can_hold_graph: false,
    };
    NodeType::new(desc).with_image(ReadImage, "read_image r1")
}

struct ReadImage;

impl ImageBehaviour for ReadImage {
    fn emit(
        &self,
        params: &ResolvedParams,
        _request: &Request,
        _inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        let media = params.media("file");
        if media.path.is_empty() {
            return Err(NodeError("no file chosen".into()));
        }
        plan.load(media, space_from_choice(params.text("space")))
    }
}
