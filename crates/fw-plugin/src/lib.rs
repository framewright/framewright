//! The node contract every node type implements, built-in or plugin.
//!
//! A node type is described by plain data ([`NodeTypeDesc`]) so that WASM and
//! shader plugins can provide one as easily as Rust code. Built-in nodes
//! register through the same [`Registry`] as plugins.
//!
//! TODO: image behaviour (`requirements` / `emit` into a plan builder) arrives
//! with the compiler.

pub mod desc;
pub mod registry;
pub mod value;

pub use desc::{
    ChoiceOption, ContainerKind, HandleKind, NodeTypeDesc, ParamDesc, ParamKind, PortDesc,
    PortType, Unit,
};
pub use registry::{MigrateFn, NodeType, Registry, RegistryError};
pub use value::{Color, MediaRef, ParamValue, ValueError};
