//! Compiles a document into a render plan.
//!
//! 1. Lower the authoring tree into a flat graph of operations, asking each
//!    node to describe its work ([`fw_plugin::ImageBehaviour`]).
//! 2. Resolve animation to plain values at the requested time.
//! 3. Insert colour conversions where an input isn't in the space a node
//!    works in.
//! 4. Key every operation by exactly what determines its output.
//! 5. Fuse chains of per-pixel operations into single GPU passes.

mod codegen;
mod frames;
mod lower;
pub mod plan;

use std::path::PathBuf;

use fw_core::{ColorSpace, ContentHash, NodeId, Time};
use fw_doc::Document;
use fw_plugin::{MediaRef, Quality, Registry};

pub use frames::FrameTimes;
pub use plan::{FillStep, KernelOp, KernelStep, LoadStep, Origin, Plan, Step};

/// What the compiler needs to know about a media file. Looked up through a
/// [`MediaResolver`] so the compiler itself does no I/O.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// The file to read, resolved from the reference.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// The colour space the file says its pixels are in.
    pub space: ColorSpace,
    /// A hash of the file's content, so renamed or moved files keep their cache.
    pub fingerprint: ContentHash,
    /// Frame timing for video; `None` for a still image.
    pub frames: Option<FrameTimes>,
    /// The file has no alpha channel, so every pixel is opaque.
    pub opaque: bool,
}

pub trait MediaResolver {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String>;
}

/// Decodes the file a load step names, for the renderer. Kept beside
/// [`MediaResolver`] so one media adapter can serve both.
pub trait Decoder {
    /// Straight-alpha pixels, `load.width` × `load.height`.
    fn decode(&self, load: &LoadStep) -> Result<fw_core::RgbaImage, String>;
}

#[derive(Debug, Clone)]
pub struct Options {
    pub time: Time,
    pub quality: Quality,
    /// The scene-linear space nodes work in unless they say otherwise.
    pub working_space: ColorSpace,
    /// What an Output node set to "auto" delivers, e.g. chosen from the output file type.
    pub delivery_space: ColorSpace,
}

impl Options {
    pub fn at(time: Time) -> Options {
        Options {
            time,
            quality: Quality::Final,
            working_space: ColorSpace::ACESCG,
            delivery_space: ColorSpace::LINEAR_REC709,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompileError {
    #[error("the root graph has no Output node")]
    NoOutput,
    #[error("the root graph has {0} Output nodes; one is supported for now")]
    SeveralOutputs(usize),
    #[error("{name:?} ({node}): {message}")]
    Node {
        node: NodeId,
        name: String,
        message: String,
    },
    #[error("the output has no finite size")]
    InfiniteOutput,
    #[error("the root container can't be rendered: {0}")]
    Root(String),
}

/// Compiles the document's root graph at `options.time`.
pub fn compile(
    doc: &Document,
    registry: &Registry,
    media: &dyn MediaResolver,
    options: &Options,
) -> Result<Plan, CompileError> {
    lower::compile(doc, registry, media, options)
}
