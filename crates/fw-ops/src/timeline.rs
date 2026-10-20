//! The Timeline container and the plain clip.
//!
//! The Timeline's lowering lives in the compiler. A plain clip is media placed
//! on a timeline with no graph inside: "media → output", stored compactly.

use fw_plugin::{
    AudioBehaviour, AudioBlock, AudioProcessor, CLIP_TYPE, ContainerKind, ImageBehaviour,
    ImageHandle, NodeError, NodeType, NodeTypeDesc, ParamDesc, PixelOp, PlanBuilder, PortDesc,
    PortType, Request, ResolvedParams, TIMELINE_TYPE, Unit,
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
        can_hold_graph: false,
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
            ParamDesc::float("volume", "Volume", Unit::Decibels, 0.0)
                .soft_range(-60.0, 12.0)
                .hard_range(-120.0, 24.0),
        ],
        // Time reaches the cache key through the frame the load picks, so a
        // still image keys the same at every moment.
        varies_with_time: false,
        container: None,
        // Open in Compose gives a clip a graph, starting from its media.
        can_hold_graph: true,
    };
    NodeType::new(desc)
        .with_image(Clip, "clip r2")
        .with_audio(ClipAudio)
}

struct Clip;

impl ImageBehaviour for Clip {
    /// The clip's media, which its graph's Input node stands for.
    fn source(
        &self,
        params: &ResolvedParams,
        _request: &Request,
        plan: &mut dyn PlanBuilder,
    ) -> Result<Option<ImageHandle>, NodeError> {
        let media = params.media("file");
        if media.path.is_empty() {
            return Err(NodeError("no media".into()));
        }
        plan.load(media, space_from_choice(params.text("space")))
            .map(Some)
    }

    /// The media, or the clip's graph when it has one, at the clip's opacity.
    fn emit(
        &self,
        params: &ResolvedParams,
        request: &Request,
        inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError> {
        let image = match inputs.first() {
            Some(graph) => *graph,
            None => self
                .source(params, request, plan)?
                .expect("a clip supplies its media"),
        };
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

/// A clip's sound: its media's audio at the clip's Volume.
struct ClipAudio;

impl AudioBehaviour for ClipAudio {
    fn create(&self) -> Box<dyn AudioProcessor> {
        Box::new(Gain)
    }

    fn params(&self) -> Vec<String> {
        vec!["volume".into()]
    }
}

/// Scales samples by a level in decibels (the first parameter).
/// TODO: smooth level changes across blocks so automation doesn't click.
struct Gain;

impl AudioProcessor for Gain {
    fn prepare(&mut self, _sample_rate: u32, _max_frames: usize) {}

    fn process(&mut self, block: &mut AudioBlock<'_>) {
        let gain = 10f32.powf(block.params.first().copied().unwrap_or(0.0) / 20.0);
        for (out, input) in block.output.iter_mut().zip(block.input) {
            *out = input * gain;
        }
    }
}
