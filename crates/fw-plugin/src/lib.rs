//! The node contract every node type implements, built-in or plugin.
//!
//! A node type is described by plain data ([`NodeTypeDesc`]) so that WASM and
//! shader plugins can provide one as easily as Rust code. Built-in nodes
//! register through the same [`Registry`] as plugins.

pub mod desc;
pub mod image;
pub mod registry;
pub mod value;

pub use desc::{
    ChoiceOption, ContainerKind, GRAPH_TYPE, HandleKind, NodeTypeDesc, OUTPUT_TYPE, ParamDesc,
    ParamKind, PortDesc, PortType, Unit,
};
pub use image::{
    ImageBehaviour, ImageHandle, ImageInfo, NodeError, PixelOp, PlanBuilder, Quality, Request,
    ResolvedParams, SpaceNeed,
};
pub use registry::{MigrateFn, NodeType, Registry, RegistryError};
pub use value::{Color, MediaRef, ParamValue, ValueError};
