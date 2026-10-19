//! Nodes and container contents.

use std::fmt;
use std::str::FromStr;

use fw_core::NodeId;
use imbl::{OrdMap, OrdSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::param::Param;

/// The type id of the Graph container.
pub const GRAPH_TYPE: &str = "fw.graph";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Known(KnownNode),
    /// A node whose type isn't registered, or was saved by a newer version.
    /// It is kept byte-for-byte and edits that would change it are refused.
    Unknown(UnknownNode),
}

impl Node {
    pub fn type_id(&self) -> &str {
        match self {
            Node::Known(n) => &n.type_id,
            Node::Unknown(n) => &n.type_id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Node::Known(n) => &n.name,
            Node::Unknown(n) => &n.name,
        }
    }

    pub fn as_known(&self) -> Option<&KnownNode> {
        match self {
            Node::Known(n) => Some(n),
            Node::Unknown(_) => None,
        }
    }

    pub fn graph(&self) -> Option<&Graph> {
        self.as_known().and_then(|n| n.graph.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownNode {
    pub type_id: String,
    pub version: u32,
    pub name: String,
    /// Only parameters that have been set. The rest take the schema default.
    pub params: OrdMap<String, Param>,
    /// Present when the type is a Graph container.
    pub graph: Option<Graph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownNode {
    pub type_id: String,
    pub version: u32,
    pub name: String,
    /// The node exactly as it was read, including any contents.
    pub raw: Box<RawValue>,
}

impl PartialEq for UnknownNode {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
            && self.version == other.version
            && self.name == other.name
            && self.raw.get() == other.raw.get()
    }
}

/// The contents of a Graph container. Connections live here, never inside a
/// node's own data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(into = "GraphRepr", from = "GraphRepr")]
pub struct Graph {
    pub nodes: OrdSet<NodeId>,
    /// Each connected input, mapped to the output feeding it. An input has at
    /// most one source.
    pub inputs: OrdMap<PortRef, PortRef>,
}

impl Graph {
    pub fn edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.inputs.iter().map(|(to, from)| Edge {
            from: from.clone(),
            to: to.clone(),
        })
    }

    /// Whether `target` can be reached by following connections downstream from `start`.
    pub fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut stack = vec![start];
        let mut seen = OrdSet::new();
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if seen.insert(node).is_some() {
                continue;
            }
            stack.extend(
                self.inputs
                    .iter()
                    .filter(|(_, from)| from.node == node)
                    .map(|(to, _)| to.node),
            );
        }
        false
    }
}

#[derive(Serialize, Deserialize)]
struct GraphRepr {
    nodes: Vec<NodeId>,
    edges: Vec<Edge>,
}

impl From<Graph> for GraphRepr {
    fn from(g: Graph) -> GraphRepr {
        GraphRepr {
            nodes: g.nodes.iter().copied().collect(),
            edges: g.edges().collect(),
        }
    }
}

impl From<GraphRepr> for Graph {
    fn from(r: GraphRepr) -> Graph {
        Graph {
            nodes: r.nodes.into_iter().collect(),
            inputs: r.edges.into_iter().map(|e| (e.to, e.from)).collect(),
        }
    }
}

/// A port on a node. Saved as `"<node id>:<port id>"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortRef {
    pub node: NodeId,
    pub port: String,
}

impl PortRef {
    pub fn new(node: NodeId, port: &str) -> PortRef {
        PortRef {
            node,
            port: port.into(),
        }
    }
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.node, self.port)
    }
}

impl FromStr for PortRef {
    type Err = String;

    fn from_str(s: &str) -> Result<PortRef, String> {
        let (node, port) = s
            .split_once(':')
            .ok_or_else(|| format!("{s:?} is not \"<node id>:<port>\""))?;
        let node = node.parse().map_err(|e| format!("{s:?}: {e}"))?;
        Ok(PortRef {
            node,
            port: port.into(),
        })
    }
}

impl Serialize for PortRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PortRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<PortRef, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A connection from an output port to an input port.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}
