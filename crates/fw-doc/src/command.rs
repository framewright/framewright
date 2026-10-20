//! Commands: every edit to a document is one of these.
//!
//! Commands are plain serializable data, so the UI, scripts, the command
//! journal and tests all drive the document the same way. Applying a command
//! returns its inverse, which is how undo works.

use fw_core::NodeId;
use fw_plugin::{ContainerKind, Registry};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};

use crate::clip_graph::{add_starting_graph, clip_graph_ids, starts_with_graph};
use crate::document::Document;
use crate::node::{Contents, Edge, Graph, KnownNode, Node, PortRef};
use crate::param::{Param, ParamError};
use crate::timeline::{Placement, Timeline, TimelineSettings, Track, TrackId};

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
        /// Required when the parent is a Timeline, and only then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placement: Option<Placement>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placement: Option<Placement>,
        /// The top node's position in the node editor, when its parent is a graph.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
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
    /// Moves, trims or retimes a clip on its timeline.
    SetPlacement {
        node: NodeId,
        placement: Placement,
    },
    /// Moves a node in the node editor, or leaves its placing to the editor
    /// with `None`. The node must be in a graph.
    SetPosition {
        node: NodeId,
        position: Option<[f64; 2]>,
    },
    /// Inserts a track at `index` (0 is the bottom).
    AddTrack {
        timeline: NodeId,
        track: Track,
        index: usize,
    },
    /// Removes an empty track.
    RemoveTrack {
        timeline: NodeId,
        track: TrackId,
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
    #[error("{container} is not a {expected}")]
    WrongContainer {
        container: NodeId,
        expected: &'static str,
    },
    #[error("clip placement: {0}")]
    Placement(String),
    #[error("track: {0}")]
    Track(String),
    #[error("node position: {0}")]
    Position(String),
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
            Command::AddNode {
                parent,
                id,
                type_id,
                ..
            } => {
                let mut touched = vec![*parent, *id];
                if starts_with_graph(type_id) {
                    let (input, output) = clip_graph_ids(*id);
                    touched.extend([input, output]);
                }
                touched
            }
            Command::RemoveNode { id } => vec![*id],
            Command::RestoreNodes { parent, nodes, .. } => std::iter::once(*parent)
                .chain(nodes.iter().map(|(id, _)| *id))
                .collect(),
            Command::SetParam { node, .. } | Command::Rename { node, .. } => vec![*node],
            Command::Connect { edge } => vec![edge.from.node, edge.to.node],
            Command::Disconnect { input } => vec![input.node],
            Command::SetPlacement { node, .. } | Command::SetPosition { node, .. } => vec![*node],
            Command::AddTrack { timeline, .. } | Command::RemoveTrack { timeline, .. } => {
                vec![*timeline]
            }
        }
    }

    fn apply_to(&self, doc: &mut Document, registry: &Registry) -> Result<Command, CommandError> {
        match self {
            Command::AddNode {
                parent,
                id,
                type_id,
                name,
                placement,
            } => {
                if doc.contains(*id) {
                    return Err(CommandError::DuplicateId(*id));
                }
                let desc = registry
                    .desc(type_id)
                    .ok_or_else(|| CommandError::UnknownType(type_id.clone()))?;
                let contents = if starts_with_graph(type_id) {
                    Some(Contents::Graph(add_starting_graph(doc, registry, *id)?))
                } else {
                    desc.container.map(empty_contents)
                };
                let node = Node::Known(KnownNode {
                    type_id: type_id.clone(),
                    version: desc.version,
                    name: name.clone().unwrap_or_else(|| desc.name.clone()),
                    params: OrdMap::new(),
                    contents,
                });
                place(doc, *parent, *id, placement.clone(), None, &[])?;
                doc.nodes.insert(*id, node);
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
                let (edges, placement, position) = unplace(doc, parent, *id)?;
                for (n, _) in &nodes {
                    doc.nodes.remove(n);
                    doc.parents.remove(n);
                }
                Ok(Command::RestoreNodes {
                    parent,
                    nodes,
                    edges,
                    placement,
                    position,
                })
            }

            Command::RestoreNodes {
                parent,
                nodes,
                edges,
                placement,
                position,
            } => {
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
                for (id, node) in nodes {
                    for child in node.children() {
                        doc.parents.insert(child, *id);
                    }
                }
                place(doc, *parent, top, placement.clone(), *position, edges)?;
                let mut listed: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
                let mut reached = doc.subtree(top);
                listed.sort();
                reached.sort();
                if listed != reached || reached.iter().any(|n| !doc.nodes.contains_key(n)) {
                    return Err(CommandError::InvalidRestore(
                        "nodes don't form exactly one subtree".into(),
                    ));
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

            Command::SetPlacement { node, placement } => {
                let parent = doc.parent(*node).ok_or(CommandError::Root)?;
                let t = timeline_mut(doc, parent)?;
                t.check_placement(placement, Some(*node))
                    .map_err(CommandError::Placement)?;
                let old = t
                    .clips
                    .insert(*node, placement.clone())
                    .expect("timeline children always have placements");
                Ok(Command::SetPlacement {
                    node: *node,
                    placement: old,
                })
            }

            Command::SetPosition { node, position } => {
                if position.is_some_and(|p| !p.iter().all(|v| v.is_finite())) {
                    return Err(CommandError::Position("it isn't finite".into()));
                }
                let parent = doc.parent(*node).ok_or(CommandError::Root)?;
                let g = graph_mut(doc, parent)?;
                let old = match position {
                    Some(p) => g.positions.insert(*node, *p),
                    None => g.positions.remove(node),
                };
                Ok(Command::SetPosition {
                    node: *node,
                    position: old,
                })
            }

            Command::AddTrack {
                timeline,
                track,
                index,
            } => {
                let t = timeline_mut(doc, *timeline)?;
                if t.track(track.id).is_some() {
                    return Err(CommandError::Track(format!("{} already exists", track.id)));
                }
                if *index > t.tracks.len() {
                    return Err(CommandError::Track(format!(
                        "index {index} is past the {} tracks",
                        t.tracks.len()
                    )));
                }
                t.tracks.insert(*index, track.clone());
                Ok(Command::RemoveTrack {
                    timeline: *timeline,
                    track: track.id,
                })
            }

            Command::RemoveTrack { timeline, track } => {
                let t = timeline_mut(doc, *timeline)?;
                let index = t
                    .tracks
                    .iter()
                    .position(|tr| tr.id == *track)
                    .ok_or_else(|| CommandError::Track(format!("there is no track {track}")))?;
                if t.clips.values().any(|p| p.track == *track) {
                    return Err(CommandError::Track(format!(
                        "{} still has clips on it",
                        t.tracks[index].name
                    )));
                }
                let removed = t.tracks.remove(index);
                Ok(Command::AddTrack {
                    timeline: *timeline,
                    track: removed,
                    index,
                })
            }
        }
    }
}

fn empty_contents(kind: ContainerKind) -> Contents {
    match kind {
        ContainerKind::Graph => Contents::Graph(Graph::default()),
        // TODO: nested timelines should take their settings from where they're used.
        ContainerKind::Timeline => Contents::Timeline(Timeline::new(TimelineSettings {
            width: 1920,
            height: 1080,
            rate: fw_core::Time::from_seconds(24),
            sample_rate: 48000,
        })),
    }
}

/// Puts `id` into `parent`'s structure: as a graph member with `edges` and
/// maybe a `position`, or as a clip with `placement`.
fn place(
    doc: &mut Document,
    parent: NodeId,
    id: NodeId,
    placement: Option<Placement>,
    position: Option<[f64; 2]>,
    edges: &[Edge],
) -> Result<(), CommandError> {
    match doc.nodes.get_mut(&parent) {
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Graph(g)),
            ..
        })) => {
            if placement.is_some() {
                return Err(CommandError::Placement(
                    "nodes in a graph have no placement".into(),
                ));
            }
            g.nodes.insert(id);
            if let Some(p) = position {
                g.positions.insert(id, p);
            }
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
        }
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Timeline(t)),
            ..
        })) => {
            let placement = placement.ok_or_else(|| {
                CommandError::Placement("a clip on a timeline needs a placement".into())
            })?;
            if !edges.is_empty() || position.is_some() {
                return Err(CommandError::InvalidRestore(
                    "timeline clips have no connections or node positions".into(),
                ));
            }
            t.check_placement(&placement, None)
                .map_err(CommandError::Placement)?;
            t.clips.insert(id, placement);
        }
        Some(_) => return Err(CommandError::NotAContainer(parent)),
        None => return Err(CommandError::NoSuchNode(parent)),
    }
    doc.parents.insert(id, parent);
    Ok(())
}

/// What restoring a node into its parent's structure needs: its connections,
/// its placement on a timeline, or its position in a graph.
type Place = (Vec<Edge>, Option<Placement>, Option<[f64; 2]>);

/// Takes `id` out of `parent`'s structure, returning what restoring it needs.
fn unplace(doc: &mut Document, parent: NodeId, id: NodeId) -> Result<Place, CommandError> {
    match doc.nodes.get_mut(&parent) {
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Graph(g)),
            ..
        })) => {
            let edges: Vec<Edge> = g
                .edges()
                .filter(|e| e.from.node == id || e.to.node == id)
                .collect();
            for edge in &edges {
                g.inputs.remove(&edge.to);
            }
            g.nodes.remove(&id);
            Ok((edges, None, g.positions.remove(&id)))
        }
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Timeline(t)),
            ..
        })) => Ok((vec![], t.clips.remove(&id), None)),
        _ => Err(CommandError::NotAContainer(parent)),
    }
}

fn graph_mut(doc: &mut Document, id: NodeId) -> Result<&mut Graph, CommandError> {
    match doc.nodes.get_mut(&id) {
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Graph(g)),
            ..
        })) => Ok(g),
        Some(_) => Err(CommandError::WrongContainer {
            container: id,
            expected: "graph",
        }),
        None => Err(CommandError::NoSuchNode(id)),
    }
}

fn timeline_mut(doc: &mut Document, id: NodeId) -> Result<&mut Timeline, CommandError> {
    match doc.nodes.get_mut(&id) {
        Some(Node::Known(KnownNode {
            contents: Some(Contents::Timeline(t)),
            ..
        })) => Ok(t),
        Some(_) => Err(CommandError::WrongContainer {
            container: id,
            expected: "timeline",
        }),
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
    let kind = known.contents.as_ref().map(|c| match c {
        Contents::Graph(_) => ContainerKind::Graph,
        Contents::Timeline(_) => ContainerKind::Timeline,
    });
    if kind != desc.container {
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
