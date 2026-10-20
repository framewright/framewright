//! Compiles a document into a render plan.
//!
//! 1. Lower the authoring tree into a flat graph of operations, asking each
//!    node to describe its work ([`fw_plugin::ImageBehavior`]).
//! 2. Resolve animation to plain values at the requested time.
//! 3. Insert color conversions where an input isn't in the space a node
//!    works in.
//! 4. Key every operation by exactly what determines its output.
//! 5. Fuse chains of per-pixel operations into single GPU passes.

mod audio;
mod codegen;
mod frames;
mod hardware;
mod lower;
pub mod plan;

use std::path::PathBuf;

use fw_core::{ColorSpace, ContentHash, NodeId, Time};
use fw_doc::Document;
use fw_plugin::{MediaRef, Quality, Registry};

pub use audio::{AudioPlan, Voice, compile_audio};
pub use codegen::conversion_function;
pub use frames::FrameTimes;
pub use hardware::{Acquired, Decoded, HardwareFrame, HardwarePicture, VulkanVideoDevice};
pub use plan::{FillStep, KernelOp, KernelStep, LoadStep, Origin, Plan, Step};

/// What the compiler needs to know about a media file. Looked up through a
/// [`MediaResolver`] so the compiler itself does no I/O.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// The file to read, resolved from the reference.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// The color space the file says its pixels are in.
    pub space: ColorSpace,
    /// A hash of the file's content, so renamed or moved files keep their cache.
    pub fingerprint: ContentHash,
    /// Frame timing for video; `None` for a still image.
    pub frames: Option<FrameTimes>,
    /// The file has no alpha channel, so every pixel is opaque.
    pub opaque: bool,
    /// The file has sound.
    pub has_audio: bool,
    /// Source time 0 on the file's own clock, shared by its picture and
    /// sound. `None` when the file has no picture to anchor it.
    pub origin: Option<Time>,
}

pub trait MediaResolver {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String>;

    /// The editing copy (design §12) that can stand in for the video frame
    /// starting at `frame`, named by its profile, if one is ready. Asked only
    /// at draft quality.
    fn editing_copy(&self, _media: &MediaInfo, _frame: Time) -> Option<&'static str> {
        None
    }
}

/// Decodes the file a load step names, for the renderer. Kept beside
/// [`MediaResolver`] so one media adapter can serve both.
pub trait Decoder {
    /// The picture, `load.width` × `load.height` (an editing copy may be
    /// smaller): straight-alpha RGBA, planes for the renderer to convert, or
    /// a hardware decoder's image already on the GPU.
    fn decode(&self, load: &LoadStep) -> Result<Decoded, String>;
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
