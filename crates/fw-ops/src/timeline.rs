//! The Timeline container and the plain clip.
//!
//! The Timeline's lowering lives in the compiler. A plain clip is media placed
//! on a timeline with no graph inside: "media → output", stored compactly.

use fw_plugin::{
    CLIP_TYPE, ContainerKind, ImageBehaviour, ImageHandle, NodeError, NodeType, NodeTypeDesc,
    ParamDesc, PixelOp, PlanBuilder, PortDesc, PortType, Request, ResolvedParams, TIMELINE_TYPE,
    Unit,
};

use crate::color::{SPACE_CHOICES, space_from_choice};

pub fn timeline_type() -> NodeType {
    NodeType::new(NodeTypeDesc {
        id: TIMELINE_TYPE.into(),
        version: 1,
        name: "Timeline".into(),
        category: "Containers".into(),
        inputs: vec![],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![],
        varies_with_time: false,
        container: Some(ContainerKind::Timeline),
    })
}

pub fn clip_type() -> NodeType {
    let desc = NodeTypeDesc {
        id: CLIP_TYPE.into(),
        version: 1,
        name: "Clip".into(),
        category: "Sources".into(),
        inputs: vec![],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![
            ParamDesc::media("file", "File"),
            ParamDesc::choice("space", "Colour space", &SPACE_CHOICES, "auto"),
            ParamDesc::float("opacity", "Opacity", Unit::Percent, 100.0)
                .soft_range(0.0, 100.0)
                .hard_range(0.0, 100.0),
            ParamDesc::choice("composite", "Composite", &COMPOSITE_CHOICES, "normal"),
        ],
        // Time reaches the cache key through the frame the load picks, so a
        // still image keys the same at every moment.
        varies_with_time: false,
        container: None,
    };
    // TODO: "Open in Compose" turns a plain clip into a Graph container in place.
    NodeType::new(desc).with_image(Clip, "clip r2")
}

struct Clip;

impl ImageBehaviour for Clip {
    fn emit(
        &self,
        params: &ResolvedParams,
        _request: &Request,
        _inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        let media = params.media("file");
        if media.path.is_empty() {
            return Err(NodeError("no media".into()));
        }
        let image = plan.load(media, space_from_choice(params.text("space")))?;
        let opacity = params.f64("opacity") / 100.0;
        if opacity == 1.0 {
            return Ok(image);
        }
        // Scaling premultiplied pixels fades them in any colour space.
        let op = PixelOp {
            label: "opacity".into(),
            wgsl: "return c * prm(base, 0u);".into(),
            params: vec![opacity as f32],
        };
        Ok(plan.per_pixel(image, op))
    }
}

/// How a clip combines with the tracks below it.
/// TODO: multiply, screen and the other common modes.
pub const COMPOSITE_CHOICES: [(&str, &str); 2] = [("normal", "Normal"), ("add", "Add")];
