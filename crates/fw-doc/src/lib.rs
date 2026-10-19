//! The Framewright document.
//!
//! A document is a persistent tree of nodes. Containers (only Graph so far)
//! hold child nodes and the connections between them. Every edit is a
//! [`Command`], applied through an [`Editor`] that keeps undo history.

pub mod command;
pub mod document;
pub mod format;
pub mod history;
pub mod node;
pub mod param;
pub mod save;

pub use command::{Command, CommandError};
pub use document::Document;
pub use history::{Cause, Editor, Event};
pub use node::{Edge, GRAPH_TYPE, Graph, KnownNode, Node, PortRef, UnknownNode};
pub use param::{Interp, Keyframe, Keyframes, Param, ParamError};
