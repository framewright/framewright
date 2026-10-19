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
//!       "graph": { "nodes": { "<id>": { ... } }, "edges": [ ... ] }
//!     }
//!   }
//! }
//! ```
//!
//! Nodes of unknown or newer types are kept as the exact bytes that were read.

use std::collections::BTreeMap;

use fw_core::NodeId;
use fw_plugin::{ContainerKind, Registry};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::command::check_node;
use crate::document::Document;
use crate::node::{Edge, Graph, KnownNode, Node, UnknownNode};
use crate::param::Param;

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
    if doc.graph(doc.root).is_none() {
        return Err(LoadError::Structure(
            "the root must be a Graph container of a known type".into(),
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
}

#[derive(Serialize)]
struct GraphOut<'a> {
    nodes: BTreeMap<NodeId, NodeOut<'a>>,
    edges: Vec<Edge>,
}

fn node_out(doc: &Document, id: NodeId) -> NodeOut<'_> {
    match &doc.nodes[&id] {
        Node::Unknown(n) => NodeOut::Unknown(&n.raw),
        Node::Known(n) => NodeOut::Known(KnownOut {
            type_id: &n.type_id,
            version: n.version,
            name: &n.name,
            params: n.params.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            graph: n.graph.as_ref().map(|g| GraphOut {
                nodes: g.nodes.iter().map(|c| (*c, node_out(doc, *c))).collect(),
                edges: g.edges().collect(),
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphIn {
    #[serde(default)]
    nodes: BTreeMap<NodeId, Box<RawValue>>,
    #[serde(default)]
    edges: Vec<Edge>,
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
    let is_graph = desc.container == Some(ContainerKind::Graph);
    if node.graph.is_some() != is_graph {
        return Err(err(format!("contents don't match the type {}", desc.id)));
    }
    let mut graph = None;
    if let Some(contents) = node.graph {
        let mut g = Graph::default();
        for (child, child_raw) in &contents.nodes {
            read_node(doc, registry, *child, child_raw)?;
            doc.parents.insert(*child, id);
            g.nodes.insert(*child);
        }
        for edge in contents.edges {
            if g.inputs.insert(edge.to.clone(), edge.from).is_some() {
                return Err(err(format!("input {} is connected twice", edge.to)));
            }
        }
        graph = Some(g);
    }
    let node = Node::Known(KnownNode {
        type_id: desc.id.clone(),
        version: desc.version,
        name: node.name,
        params,
        graph,
    });
    check_node(id, &node, registry).map_err(|e| err(e.to_string()))?;
    doc.nodes.insert(id, node);
    Ok(())
}
