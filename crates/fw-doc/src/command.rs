//! Commands: every edit to a document is one of these.
//!
//! Commands are plain serialisable data, so the UI, scripts, the command
//! journal and tests all drive the document the same way. Applying a command
//! returns its inverse, which is how undo works.

use fw_core::NodeId;
use fw_plugin::{ContainerKind, Registry};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::node::{Edge, Graph, KnownNode, Node, PortRef};
use crate::param::{Param, ParamError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// Adds a node of a registered type with default parameters.
    AddNode {
        parent: NodeId,
        id: NodeId,
        type_id: String,
        /// Defaults to the type's name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Removes a node, everything nested inside it, and its connections.
    RemoveNode {
        id: NodeId,
    },
    /// Puts back what [`Command::RemoveNode`] took away. `nodes` starts with
    /// the top node, followed by everything nested inside it.
    RestoreNodes {
        parent: NodeId,
        nodes: Vec<(NodeId, Node)>,
        edges: Vec<Edge>,
    },
    /// Sets a parameter, or resets it to the default with `None`.
    SetParam {
        node: NodeId,
        param: String,
        value: Option<Param>,
    },
    /// Connects an output to an input, replacing the input's existing source.
    Connect {
        edge: Edge,
    },
    Disconnect {
        input: PortRef,
    },
    Rename {
        node: NodeId,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("node {0} does not exist")]
    NoSuchNode(NodeId),
    #[error("node {0} already exists")]
    DuplicateId(NodeId),
    #[error("node type {0:?} is not registered")]
    UnknownType(String),
    #[error("node {0} is not a container")]
    NotAContainer(NodeId),
    #[error("the root container cannot be removed or connected")]
    Root,
    #[error("node {0} has an unregistered or newer type, so it can't be edited")]
    ReadOnly(NodeId),
    #[error("node {node} has no parameter {param:?}")]
    NoSuchParam { node: NodeId, param: String },
    #[error("parameter {param:?}: {error}")]
    InvalidParam { param: String, error: ParamError },
    #[error("there is no port {0}")]
    NoSuchPort(PortRef),
    #[error("{from} and {to} carry different kinds of data")]
    PortTypeMismatch { from: PortRef, to: PortRef },
    #[error("{0} and {1} are in different containers")]
    DifferentContainers(NodeId, NodeId),
    #[error("connecting {from} to {to} would create a loop")]
    Cycle { from: PortRef, to: PortRef },
    #[error("{0} is already connected")]
    AlreadyConnected(PortRef),
    #[error("{0} is not connected")]
    NotConnected(PortRef),
    #[error("invalid restore: {0}")]
    InvalidRestore(String),
}

impl Command {
    /// Applies the command and returns its inverse. On error the document is unchanged.
    pub fn apply(&self, doc: &mut Document, registry: &Registry) -> Result<Command, CommandError> {
        let mut next = doc.clone();
        let inverse = self.apply_to(&mut next, registry)?;
        *doc = next;
        Ok(inverse)
    }

    /// The nodes this command changes, for change events.
    pub fn touched(&self) -> Vec<NodeId> {
        match self {
            Command::AddNode { parent, id, .. } => vec![*parent, *id],
            Command::RemoveNode { id } => vec![*id],
            Command::RestoreNodes { parent, nodes, .. } => std::iter::once(*parent)
                .chain(nodes.iter().map(|(id, _)| *id))
                .collect(),
            Command::SetParam { node, .. } | Command::Rename { node, .. } => vec![*node],
            Command::Connect { edge } => vec![edge.from.node, edge.to.node],
            Command::Disconnect { input } => vec![input.node],
        }
    }

    fn apply_to(&self, doc: &mut Document, registry: &Registry) -> Result<Command, CommandError> {
        match self {
            Command::AddNode {
                parent,
                id,
                type_id,
                name,
            } => {
                graph(doc, *parent)?;
                if doc.contains(*id) {
                    return Err(CommandError::DuplicateId(*id));
                }
                let desc = registry
                    .desc(type_id)
                    .ok_or_else(|| CommandError::UnknownType(type_id.clone()))?;
                let node = Node::Known(KnownNode {
                    type_id: type_id.clone(),
                    version: desc.version,
                    name: name.clone().unwrap_or_else(|| desc.name.clone()),
                    params: OrdMap::new(),
                    graph: (desc.container == Some(ContainerKind::Graph)).then(Graph::default),
                });
                doc.nodes.insert(*id, node);
                doc.parents.insert(*id, *parent);
                graph_mut(doc, *parent)?.nodes.insert(*id);
                Ok(Command::RemoveNode { id: *id })
            }

            Command::RemoveNode { id } => {
                if *id == doc.root {
                    return Err(CommandError::Root);
                }
                let parent = doc.parent(*id).ok_or(CommandError::NoSuchNode(*id))?;
                let nodes: Vec<(NodeId, Node)> = doc
                    .subtree(*id)
                    .into_iter()
                    .map(|n| (n, doc.nodes[&n].clone()))
                    .collect();
                let g = graph_mut(doc, parent)?;
                let edges: Vec<Edge> = g
                    .edges()
                    .filter(|e| e.from.node == *id || e.to.node == *id)
                    .collect();
                for edge in &edges {
                    g.inputs.remove(&edge.to);
                }
                g.nodes.remove(id);
                for (n, _) in &nodes {
                    doc.nodes.remove(n);
                    doc.parents.remove(n);
                }
                Ok(Command::RestoreNodes {
                    parent,
                    nodes,
                    edges,
                })
            }

            Command::RestoreNodes {
                parent,
                nodes,
                edges,
            } => {
                graph(doc, *parent)?;
                let top = nodes
                    .first()
                    .map(|(id, _)| *id)
                    .ok_or_else(|| CommandError::InvalidRestore("no nodes".into()))?;
                for (id, node) in nodes {
                    if doc.contains(*id) {
                        return Err(CommandError::DuplicateId(*id));
                    }
                    check_node(*id, node, registry)?;
                    doc.nodes.insert(*id, node.clone());
                }
                doc.parents.insert(top, *parent);
                for (id, node) in nodes {
                    for child in node.graph().iter().flat_map(|g| g.nodes.iter()) {
                        doc.parents.insert(*child, *id);
                    }
                }
                let mut listed: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
                let mut reached = doc.subtree(top);
                listed.sort();
                reached.sort();
                if listed != reached || reached.iter().any(|n| !doc.nodes.contains_key(n)) {
                    return Err(CommandError::InvalidRestore(
                        "nodes don't form exactly one subtree".into(),
                    ));
                }
                let g = graph_mut(doc, *parent)?;
                g.nodes.insert(top);
                for edge in edges {
                    if !g.nodes.contains(&edge.from.node) || !g.nodes.contains(&edge.to.node) {
                        return Err(CommandError::InvalidRestore(format!(
                            "edge {} → {} leaves the container",
                            edge.from, edge.to
                        )));
                    }
                    if g.inputs
                        .insert(edge.to.clone(), edge.from.clone())
                        .is_some()
                    {
                        return Err(CommandError::AlreadyConnected(edge.to.clone()));
                    }
                }
                Ok(Command::RemoveNode { id: top })
            }

            Command::SetParam { node, param, value } => {
                let known = known_mut(doc, *node)?;
                let desc = registry
                    .desc(&known.type_id)
                    .ok_or_else(|| CommandError::UnknownType(known.type_id.clone()))?;
                let param_desc = desc.param(param).ok_or_else(|| CommandError::NoSuchParam {
                    node: *node,
                    param: param.clone(),
                })?;
                let old = match value {
                    Some(v) => {
                        let checked = v.clone().check(param_desc).map_err(|error| {
                            CommandError::InvalidParam {
                                param: param.clone(),
                                error,
                            }
                        })?;
                        known.params.insert(param.clone(), checked)
                    }
                    None => known.params.remove(param),
                };
                Ok(Command::SetParam {
                    node: *node,
                    param: param.clone(),
                    value: old,
                })
            }

            Command::Connect { edge } => {
                let (from, to) = (&edge.from, &edge.to);
                let parent = shared_parent(doc, from.node, to.node)?;
                check_port(doc, registry, from, Side::Output)?;
                check_port(doc, registry, to, Side::Input)?;
                if let (Some(a), Some(b)) = (
                    port_type(doc, registry, from, Side::Output),
                    port_type(doc, registry, to, Side::Input),
                ) && a != b
                {
                    return Err(CommandError::PortTypeMismatch {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
                let g = graph_mut(doc, parent)?;
                if g.reaches(to.node, from.node) {
                    return Err(CommandError::Cycle {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
                Ok(match g.inputs.insert(to.clone(), from.clone()) {
                    Some(old) => Command::Connect {
                        edge: Edge {
                            from: old,
                            to: to.clone(),
                        },
                    },
                    None => Command::Disconnect { input: to.clone() },
                })
            }

            Command::Disconnect { input } => {
                let parent = doc.parent(input.node).ok_or(CommandError::Root)?;
                let from = graph_mut(doc, parent)?
                    .inputs
                    .remove(input)
                    .ok_or_else(|| CommandError::NotConnected(input.clone()))?;
                Ok(Command::Connect {
                    edge: Edge {
                        from,
                        to: input.clone(),
                    },
                })
            }

            Command::Rename { node, name } => {
                let known = known_mut(doc, *node)?;
                let old = std::mem::replace(&mut known.name, name.clone());
                Ok(Command::Rename {
                    node: *node,
                    name: old,
                })
            }
        }
    }
}

fn graph(doc: &Document, id: NodeId) -> Result<&Graph, CommandError> {
    let node = doc.node(id).ok_or(CommandError::NoSuchNode(id))?;
    node.graph().ok_or(CommandError::NotAContainer(id))
}

fn graph_mut(doc: &mut Document, id: NodeId) -> Result<&mut Graph, CommandError> {
    match doc.nodes.get_mut(&id) {
        Some(Node::Known(KnownNode { graph: Some(g), .. })) => Ok(g),
        Some(_) => Err(CommandError::NotAContainer(id)),
        None => Err(CommandError::NoSuchNode(id)),
    }
}

fn known_mut(doc: &mut Document, id: NodeId) -> Result<&mut KnownNode, CommandError> {
    match doc.nodes.get_mut(&id) {
        Some(Node::Known(n)) => Ok(n),
        Some(Node::Unknown(_)) => Err(CommandError::ReadOnly(id)),
        None => Err(CommandError::NoSuchNode(id)),
    }
}

fn shared_parent(doc: &Document, a: NodeId, b: NodeId) -> Result<NodeId, CommandError> {
    for id in [a, b] {
        if !doc.contains(id) {
            return Err(CommandError::NoSuchNode(id));
        }
    }
    match (doc.parent(a), doc.parent(b)) {
        (Some(pa), Some(pb)) if pa == pb => Ok(pa),
        (Some(_), Some(_)) => Err(CommandError::DifferentContainers(a, b)),
        _ => Err(CommandError::Root),
    }
}

#[derive(Clone, Copy)]
enum Side {
    Input,
    Output,
}

/// Checks the port exists. Ports on unknown nodes can't be checked and are trusted.
fn check_port(
    doc: &Document,
    registry: &Registry,
    port: &PortRef,
    side: Side,
) -> Result<(), CommandError> {
    let Some(Node::Known(node)) = doc.node(port.node) else {
        return Ok(());
    };
    let desc = registry
        .desc(&node.type_id)
        .ok_or_else(|| CommandError::UnknownType(node.type_id.clone()))?;
    let found = match side {
        Side::Input => desc.input(&port.port).is_some(),
        Side::Output => desc.output(&port.port).is_some(),
    };
    if found {
        Ok(())
    } else {
        Err(CommandError::NoSuchPort(port.clone()))
    }
}

fn port_type(
    doc: &Document,
    registry: &Registry,
    port: &PortRef,
    side: Side,
) -> Option<fw_plugin::PortType> {
    let desc = registry.desc(&doc.node(port.node)?.as_known()?.type_id)?;
    let p = match side {
        Side::Input => desc.input(&port.port),
        Side::Output => desc.output(&port.port),
    };
    p.map(|p| p.ty)
}

/// Checks a known node against its registered type. Unknown nodes are kept as they are.
pub(crate) fn check_node(id: NodeId, node: &Node, registry: &Registry) -> Result<(), CommandError> {
    let Node::Known(known) = node else {
        return Ok(());
    };
    let desc = registry
        .desc(&known.type_id)
        .ok_or_else(|| CommandError::UnknownType(known.type_id.clone()))?;
    if known.version != desc.version {
        return Err(CommandError::InvalidRestore(format!(
            "{id} is version {} but {} is version {}",
            known.version, known.type_id, desc.version
        )));
    }
    if known.graph.is_some() != (desc.container == Some(ContainerKind::Graph)) {
        return Err(CommandError::InvalidRestore(format!(
            "{id} has the wrong contents for its type"
        )));
    }
    for (param, value) in &known.params {
        let param_desc = desc.param(param).ok_or_else(|| CommandError::NoSuchParam {
            node: id,
            param: param.clone(),
        })?;
        match value.clone().check(param_desc) {
            Ok(checked) if checked == *value => {}
            Ok(_) => {
                return Err(CommandError::InvalidRestore(format!(
                    "{id}: parameter {param:?} is not stored in its checked form"
                )));
            }
            Err(error) => {
                return Err(CommandError::InvalidParam {
                    param: param.clone(),
                    error,
                });
            }
        }
    }
    Ok(())
}
