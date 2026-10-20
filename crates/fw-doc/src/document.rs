//! The document: a persistent tree of nodes.
//!
//! Built on persistent maps, so cloning is cheap. Render threads read a frozen
//! clone while the UI keeps editing.

use fw_core::NodeId;
use imbl::{OrdMap, OrdSet};

use crate::node::{Contents, GRAPH_TYPE, Graph, KnownNode, Node, PortRef};
use crate::timeline::{Timeline, TimelineSettings};
use fw_plugin::TIMELINE_TYPE;

/// TODO: a project holds several documents (photo, video, audio; design §3.1).
/// For now there is one root container.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub(crate) root: NodeId,
    pub(crate) nodes: OrdMap<NodeId, Node>,
    /// The container holding each node. The root has no entry.
    pub(crate) parents: OrdMap<NodeId, NodeId>,
    /// Media imported into the project, used or not, as paths relative to
    /// the project file (the same form clips refer to it by).
    pub(crate) media: OrdSet<String>,
}

impl Document {
    /// An empty document whose root is a Graph container (a photo or compositing document).
    pub fn new(root: NodeId) -> Document {
        Document::with_root(root, GRAPH_TYPE, "Main", Contents::Graph(Graph::default()))
    }

    /// An empty document whose root is a Timeline (a video document).
    pub fn new_timeline(root: NodeId, settings: TimelineSettings) -> Document {
        let timeline = Timeline::new(settings);
        Document::with_root(
            root,
            TIMELINE_TYPE,
            "Main Timeline",
            Contents::Timeline(timeline),
        )
    }

    fn with_root(root: NodeId, type_id: &str, name: &str, contents: Contents) -> Document {
        let node = Node::Known(KnownNode {
            type_id: type_id.into(),
            version: 1,
            name: name.into(),
            params: OrdMap::new(),
            contents: Some(contents),
        });
        Document {
            root,
            nodes: OrdMap::unit(root, node),
            parents: OrdMap::new(),
            media: OrdSet::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The media pool: files imported into the project, in path order.
    pub fn media_pool(&self) -> impl Iterator<Item = &str> {
        self.media.iter().map(String::as_str)
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Every node, including those inside nested containers, in id order.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(id, n)| (*id, n))
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The container holding `id`, or `None` for the root.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parents.get(&id).copied()
    }

    /// The contents of `container`, if it is a Graph.
    pub fn graph(&self, container: NodeId) -> Option<&Graph> {
        self.node(container)?.graph()
    }

    /// The contents of `container`, if it is a Timeline.
    pub fn timeline(&self, container: NodeId) -> Option<&Timeline> {
        self.node(container)?.timeline()
    }

    /// The output feeding `input`, if it is connected.
    pub fn source_of(&self, input: &PortRef) -> Option<&PortRef> {
        self.graph(self.parent(input.node)?)?.inputs.get(input)
    }

    /// `id` and everything nested inside it.
    pub fn subtree(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = vec![];
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(node) = self.node(id) {
                stack.extend(node.children().into_iter().rev());
            }
        }
        out
    }

    /// Checks the structural invariants. Used by tests and after loading.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut reached = OrdSet::new();
        for id in self.subtree(self.root) {
            if reached.insert(id).is_some() {
                return Err(format!("{id} is reachable twice"));
            }
            let Some(node) = self.node(id) else {
                return Err(format!("{id} is listed in a container but missing"));
            };
            for child in node.children() {
                if self.parent(child) != Some(id) {
                    return Err(format!("{child} has the wrong parent"));
                }
            }
            if let Some(t) = node.timeline() {
                let placements: Vec<_> = t.clips.iter().collect();
                for (i, (clip, p)) in placements.iter().enumerate() {
                    let track = t
                        .track(p.track)
                        .ok_or_else(|| format!("{clip} is on a missing track"))?;
                    if let Some((other, _)) =
                        placements[i + 1..].iter().find(|(_, q)| p.overlaps(q))
                    {
                        return Err(format!("{clip} and {other} overlap on {}", track.name));
                    }
                    if p.duration <= fw_core::Time::ZERO || p.speed <= fw_core::Time::ZERO {
                        return Err(format!("{clip} has a non-positive duration or speed"));
                    }
                }
            }
            if let Some(g) = node.graph() {
                if let Some(id) = g.positions.keys().find(|id| !g.nodes.contains(id)) {
                    return Err(format!("{id} has a position but isn't in its graph"));
                }
                for edge in g.edges() {
                    for end in [&edge.from, &edge.to] {
                        if !g.nodes.contains(&end.node) {
                            return Err(format!(
                                "edge {} → {} leaves its graph",
                                edge.from, edge.to
                            ));
                        }
                    }
                }
            }
        }
        if reached.len() != self.nodes.len() {
            return Err("some nodes are not inside any container".into());
        }
        if self.parents.contains_key(&self.root) {
            return Err("the root has a parent".into());
        }
        Ok(())
    }
}
