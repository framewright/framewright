//! Image behaviour: how a node describes its work.
//!
//! Nodes never touch pixels. In two phases they say what they need from their
//! inputs ([`ImageBehaviour::requirements`]) and then add operations to a plan
//! ([`ImageBehaviour::emit`]). The engine runs the plan, which is what makes
//! fusion, caching, region-of-interest rendering and determinism possible.

use std::collections::BTreeMap;

use fw_core::{ColorSpace, Rect, Time};

use crate::value::{MediaRef, ParamValue};

/// What is being asked of a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub time: Time,
    /// The region wanted, in canvas units.
    pub region: Rect,
    /// Pixels per canvas unit.
    pub scale: f64,
    pub quality: Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Interactive preview: may cut corners.
    Draft,
    /// Export and final renders: never cuts corners.
    Final,
}

/// A node's parameters, resolved to plain values at the requested time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedParams(pub BTreeMap<String, ParamValue>);

impl ResolvedParams {
    pub fn get(&self, id: &str) -> Option<&ParamValue> {
        self.0.get(id)
    }

    /// The parameter as a number. Panics if the schema doesn't declare a
    /// numeric parameter called `id`, which is a bug in the node.
    pub fn f64(&self, id: &str) -> f64 {
        self.get(id)
            .and_then(ParamValue::as_f64)
            .unwrap_or_else(|| panic!("no numeric parameter {id:?}"))
    }

    pub fn text(&self, id: &str) -> &str {
        self.get(id)
            .and_then(ParamValue::as_text)
            .unwrap_or_else(|| panic!("no text parameter {id:?}"))
    }

    pub fn media(&self, id: &str) -> &MediaRef {
        match self.get(id) {
            Some(ParamValue::Media(m)) => m,
            _ => panic!("no media parameter {id:?}"),
        }
    }
}

/// An image in a plan under construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageHandle(pub u32);

/// What is known about an image without rendering it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    pub space: ColorSpace,
    /// Where the image has pixels, in canvas units. Never implicitly cropped.
    pub region: Rect,
    /// Width over height of one pixel.
    pub pixel_aspect: f64,
    /// TODO: optional extra channels (depth, motion, IDs, normals; design §4.2).
    pub extra_channels: Vec<String>,
    /// Known to have alpha 1 everywhere in `region`. `false` means "unknown",
    /// so it's always safe. Lets a stack skip what an opaque layer hides.
    pub opaque: bool,
}

/// The colour space a node wants an input in. The compiler converts to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceNeed {
    /// The engine's scene-linear working space.
    Working,
    Exact(ColorSpace),
    /// The space being delivered, e.g. what an output file is encoded in.
    /// Inside a graph held by another node, such as a clip's, the image is
    /// left as it is for that node to deliver.
    Delivery,
}

/// A per-pixel operation: reads one pixel (plus the same pixel of any extra
/// inputs) and writes one pixel. Adjacent per-pixel operations are fused into
/// one GPU pass.
#[derive(Debug, Clone, PartialEq)]
pub struct PixelOp {
    /// Shown by `fw inspect`, e.g. `"exposure"`.
    pub label: String,
    /// A WGSL function body. `c` is the premultiplied input `vec4<f32>`,
    /// `e0`, `e1`, … are the extra inputs' pixels, `prm(base, i)` reads
    /// parameter `i`, and it must return a `vec4<f32>`.
    pub wgsl: String,
    pub params: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct NodeError(pub String);

/// The operations a node can add to a plan.
pub trait PlanBuilder {
    fn info(&self, image: ImageHandle) -> &ImageInfo;

    /// Reads an image file. `space` overrides the colour space the file says
    /// it is in.
    fn load(
        &mut self,
        media: &MediaRef,
        space: Option<ColorSpace>,
    ) -> Result<ImageHandle, NodeError>;

    /// Applies a per-pixel operation. The output is in the input's colour space.
    fn per_pixel(&mut self, input: ImageHandle, op: PixelOp) -> ImageHandle {
        self.per_pixel_with(input, &[], op)
    }

    /// A per-pixel operation that also reads `extra` inputs, e.g. a merge
    /// reading the foreground while `input` is the background. Extra inputs
    /// must be in the same colour space as `input`.
    fn per_pixel_with(
        &mut self,
        input: ImageHandle,
        extra: &[ImageHandle],
        op: PixelOp,
    ) -> ImageHandle;
}

pub trait ImageBehaviour: Send + Sync {
    /// The colour space this node works in for input `index`.
    fn input_space(&self, _params: &ResolvedParams, _index: usize) -> SpaceNeed {
        SpaceNeed::Working
    }

    /// What this node needs from each input to produce `request`. The default,
    /// the same time and region for every input, suits most nodes.
    fn requirements(
        &self,
        _params: &ResolvedParams,
        request: &Request,
        inputs: usize,
    ) -> Vec<Request> {
        vec![request.clone(); inputs]
    }

    /// What this node supplies to a graph inside it: the image the graph's
    /// Input node stands for, such as a clip's media. `None` when it supplies
    /// nothing.
    fn source(
        &self,
        _params: &ResolvedParams,
        _request: &Request,
        _plan: &mut dyn PlanBuilder,
    ) -> Result<Option<ImageHandle>, NodeError> {
        Ok(None)
    }

    /// Adds this node's operations and returns its output. When the node is a
    /// Graph container with a behaviour of its own (a clip), `inputs` is its
    /// graph's result alone.
    fn emit(
        &self,
        params: &ResolvedParams,
        request: &Request,
        inputs: &[ImageHandle],
        plan: &mut dyn PlanBuilder,
    ) -> Result<ImageHandle, NodeError>;
}
