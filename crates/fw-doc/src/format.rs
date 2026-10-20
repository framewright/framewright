//! The saved file format. Nothing outside this module knows how documents are written.
//!
//! Documents are pretty-printed JSON with a stable key order, nested the same
//! way as the containers:
//!
//! ```json
//! {
//!   "format": "framewright.project",
//!   "format_version": 1,
//!   "root": "<id>",
//!   "nodes": {
//!     "<id>": {
//!       "type": "fw.graph",
//!       "version": 1,
//!       "name": "Main",
//!       "graph": { "nodes": { "<id>": { ... } }, "edges": [ ... ], "positions": { "<id>": [x, y] } }
//!     }
//!   }
//! }
//! ```
//!
//! Nodes of unknown or newer types are kept as the exact bytes that were read.

use std::collections::BTreeMap;

use fw_core::{NodeId, Time};
use fw_plugin::{ContainerKind, Registry};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::clip_graph::{add_starting_graph, starts_with_graph};
use crate::command::check_node;
use crate::document::Document;
use crate::node::{Contents, Edge, Graph, KnownNode, Node, UnknownNode};
use crate::param::Param;
use crate::timeline::{Placement, Timeline, TimelineSettings, Track, TrackId};

pub const FORMAT: &str = "framewright.project";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a Framewright project (format is {0:?})")]
    WrongFormat(String),
    #[error("saved by a newer version of Framewright (format version {0})")]
    NewerFormat(u32),
    #[error("node {id}: {problem}")]
    Node { id: String, problem: String },
    #[error("{0}")]
    Structure(String),
}

/// Writes a document as pretty-printed JSON, ending in a newline.
pub fn to_json(doc: &Document) -> String {
    let file = FileOut {
        format: FORMAT,
        format_version: FORMAT_VERSION,
        root: doc.root,
        nodes: BTreeMap::from([(doc.root, node_out(doc, doc.root))]),
    };
    let mut text = serde_json::to_string_pretty(&file).expect("documents always serialise");
    text.push('\n');
    text
}

/// Reads a document. Node types missing from `registry` are kept as unknown nodes.
pub fn from_json(text: &str, registry: &Registry) -> Result<Document, LoadError> {
    let file: FileIn = serde_json::from_str(text)?;
    if file.format != FORMAT {
        return Err(LoadError::WrongFormat(file.format));
    }
    if file.format_version > FORMAT_VERSION {
        return Err(LoadError::NewerFormat(file.format_version));
    }
    if file.nodes.len() != 1 || !file.nodes.contains_key(&file.root) {
        return Err(LoadError::Structure(
            "\"nodes\" must hold exactly the root".into(),
        ));
    }
    let mut doc = Document {
        root: file.root,
        nodes: OrdMap::new(),
        parents: OrdMap::new(),
    };
    read_node(&mut doc, registry, file.root, &file.nodes[&file.root])?;
    if doc
        .node(doc.root)
        .is_none_or(|n| n.graph().is_none() && n.timeline().is_none())
    {
        return Err(LoadError::Structure(
            "the root must be a Graph or Timeline container of a known type".into(),
        ));
    }
    doc.check_invariants().map_err(LoadError::Structure)?;
    Ok(doc)
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    format_version: u32,
    root: NodeId,
    nodes: BTreeMap<NodeId, NodeOut<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum NodeOut<'a> {
    Known(KnownOut<'a>),
    Unknown(&'a RawValue),
}

#[derive(Serialize)]
struct KnownOut<'a> {
    #[serde(rename = "type")]
    type_id: &'a str,
    version: u32,
    name: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<&'a str, &'a Param>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphOut<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeline: Option<TimelineOut<'a>>,
}

#[derive(Serialize)]
struct GraphOut<'a> {
    nodes: BTreeMap<NodeId, NodeOut<'a>>,
    edges: Vec<Edge>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    positions: BTreeMap<NodeId, [f64; 2]>,
}

#[derive(Serialize)]
struct TimelineOut<'a> {
    width: u32,
    height: u32,
    rate: Time,
    sample_rate: u32,
    tracks: Vec<&'a Track>,
    clips: BTreeMap<NodeId, ClipOut<'a>>,
}

#[derive(Serialize)]
struct ClipOut<'a> {
    #[serde(flatten)]
    placement: &'a Placement,
    node: NodeOut<'a>,
}

fn node_out(doc: &Document, id: NodeId) -> NodeOut<'_> {
    match &doc.nodes[&id] {
        Node::Unknown(n) => NodeOut::Unknown(&n.raw),
        Node::Known(n) => NodeOut::Known(KnownOut {
            type_id: &n.type_id,
            version: n.version,
            name: &n.name,
            params: n.params.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            // A clip's graph is left out while it's still the one it started with.
            graph: n
                .graph_contents()
                .filter(|g| {
                    !(starts_with_graph(&n.type_id)
                        && g.positions.is_empty()
                        && doc.keeps_starting_graph(id))
                })
                .map(|g| GraphOut {
                    nodes: g.nodes.iter().map(|c| (*c, node_out(doc, *c))).collect(),
                    edges: g.edges().collect(),
                    positions: g.positions.iter().map(|(id, p)| (*id, *p)).collect(),
                }),
            timeline: n.timeline_contents().map(|t| TimelineOut {
                width: t.settings.width,
                height: t.settings.height,
                rate: t.settings.rate,
                sample_rate: t.settings.sample_rate,
                tracks: t.tracks.iter().collect(),
                clips: t
                    .clips
                    .iter()
                    .map(|(c, placement)| {
                        let clip = ClipOut {
                            placement,
                            node: node_out(doc, *c),
                        };
                        (*c, clip)
                    })
                    .collect(),
            }),
        }),
    }
}

#[derive(Deserialize)]
struct FileIn {
    format: String,
    format_version: u32,
    root: NodeId,
    nodes: BTreeMap<NodeId, Box<RawValue>>,
}

/// Just enough of a node to decide whether we understand it.
#[derive(Deserialize)]
struct Header {
    #[serde(rename = "type")]
    type_id: String,
    version: u32,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnownIn {
    #[serde(rename = "type")]
    _type_id: String,
    #[serde(rename = "version")]
    _version: u32,
    name: String,
    #[serde(default)]
    params: Map<String, Value>,
    graph: Option<GraphIn>,
    timeline: Option<TimelineIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelineIn {
    width: u32,
    height: u32,
    rate: Time,
    sample_rate: u32,
    #[serde(default)]
    tracks: Vec<Track>,
    #[serde(default)]
    clips: BTreeMap<NodeId, ClipIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipIn {
    track: TrackId,
    start: Time,
    duration: Time,
    source_in: Time,
    #[serde(default = "one")]
    speed: Time,
    node: Box<RawValue>,
}

fn one() -> Time {
    Time::from_seconds(1)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphIn {
    #[serde(default)]
    nodes: BTreeMap<NodeId, Box<RawValue>>,
    #[serde(default)]
    edges: Vec<Edge>,
    #[serde(default)]
    positions: BTreeMap<NodeId, [f64; 2]>,
}

fn read_node(
    doc: &mut Document,
    registry: &Registry,
    id: NodeId,
    raw: &RawValue,
) -> Result<(), LoadError> {
    let err = |problem: String| LoadError::Node {
        id: id.to_string(),
        problem,
    };
    if doc.nodes.contains_key(&id) {
        return Err(err("the id is used twice".into()));
    }
    let header: Header = serde_json::from_str(raw.get()).map_err(|e| err(e.to_string()))?;
    let known_type = registry
        .get(&header.type_id)
        .filter(|t| header.version <= t.desc.version);
    let Some(ty) = known_type else {
        let unknown = UnknownNode {
            type_id: header.type_id,
            version: header.version,
            name: header.name,
            raw: raw.to_owned(),
        };
        doc.nodes.insert(id, Node::Unknown(unknown));
        return Ok(());
    };
    let desc = &ty.desc;
    let mut node: KnownIn = serde_json::from_str(raw.get()).map_err(|e| err(e.to_string()))?;
    if header.version < desc.version {
        let migrate = ty.migrate.ok_or_else(|| {
            err(format!(
                "{} version {} can't be upgraded to version {}",
                desc.id, header.version, desc.version
            ))
        })?;
        migrate(header.version, &mut node.params).map_err(err)?;
    }
    let mut params = OrdMap::new();
    for (name, value) in node.params {
        let param_desc = desc
            .param(&name)
            .ok_or_else(|| err(format!("{} has no parameter {name:?}", desc.id)))?;
        let param: Param =
            serde_json::from_value(value).map_err(|e| err(format!("parameter {name:?}: {e}")))?;
        let param = param
            .check(param_desc)
            .map_err(|e| err(format!("parameter {name:?}: {e}")))?;
        params.insert(name, param);
    }
    let kind = match (&node.graph, &node.timeline) {
        (None, None) => None,
        (Some(_), None) => Some(ContainerKind::Graph),
        (None, Some(_)) => Some(ContainerKind::Timeline),
        (Some(_), Some(_)) => {
            return Err(err("a node can't hold both a graph and a timeline".into()));
        }
    };
    let mut contents = None;
    if kind.is_none() && starts_with_graph(&desc.id) {
        let graph = add_starting_graph(doc, registry, id).map_err(|e| err(e.to_string()))?;
        contents = Some(Contents::Graph(graph));
    } else if kind != desc.container {
        return Err(err(format!("contents don't match the type {}", desc.id)));
    }
    if let Some(graph) = node.graph {
        let mut g = Graph::default();
        for (child, child_raw) in &graph.nodes {
            read_node(doc, registry, *child, child_raw)?;
            doc.parents.insert(*child, id);
            g.nodes.insert(*child);
        }
        for edge in graph.edges {
            if g.inputs.insert(edge.to.clone(), edge.from).is_some() {
                return Err(err(format!("input {} is connected twice", edge.to)));
            }
        }
        for (child, position) in graph.positions {
            if !g.nodes.contains(&child) {
                return Err(err(format!(
                    "{child} has a position but isn't in the graph"
                )));
            }
            if !position.iter().all(|v| v.is_finite()) {
                return Err(err(format!("{child}'s position isn't finite")));
            }
            g.positions.insert(child, position);
        }
        contents = Some(Contents::Graph(g));
    }
    if let Some(timeline) = node.timeline {
        let mut t = Timeline::new(TimelineSettings {
            width: timeline.width,
            height: timeline.height,
            rate: timeline.rate,
            sample_rate: timeline.sample_rate,
        });
        t.tracks = timeline.tracks.into_iter().collect();
        for (child, clip) in timeline.clips {
            read_node(doc, registry, child, &clip.node)?;
            doc.parents.insert(child, id);
            let placement = Placement {
                track: clip.track,
                start: clip.start,
                duration: clip.duration,
                source_in: clip.source_in,
                speed: clip.speed,
            };
            t.clips.insert(child, placement);
        }
        contents = Some(Contents::Timeline(t));
    }
    let node = Node::Known(KnownNode {
        type_id: desc.id.clone(),
        version: desc.version,
        name: node.name,
        params,
        contents,
    });
    check_node(id, &node, registry).map_err(|e| err(e.to_string()))?;
    doc.nodes.insert(id, node);
    Ok(())
}
