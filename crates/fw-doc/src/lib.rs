//! The Framewright document.
//!
//! A document is a persistent tree of nodes. Containers (only Graph so far)
//! hold child nodes and the connections between them. Every edit is a
//! [`Command`], applied through an [`Editor`] that keeps undo history.

pub mod clip_graph;
pub mod command;
pub mod document;
pub mod format;
pub mod history;
pub mod node;
pub mod param;
pub mod save;
pub mod timeline;

pub use clip_graph::clip_graph_ids;
pub use command::{Command, CommandError};
pub use document::Document;
pub use history::{Cause, Editor, Event};
pub use node::{Contents, Edge, GRAPH_TYPE, Graph, KnownNode, Node, PortRef, UnknownNode};
pub use param::{Anchor, EvalError, Interp, KeyTime, Keyframe, Keyframes, Param, ParamError};
pub use timeline::{ClipTime, Placement, Timeline, TimelineSettings, Track, TrackId, TrackKind};
