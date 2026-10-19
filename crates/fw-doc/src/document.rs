//! The document: a persistent tree of nodes.
//!
//! Built on persistent maps, so cloning is cheap. Render threads read a frozen
//! clone while the UI keeps editing.

use fw_core::NodeId;
use imbl::{OrdMap, OrdSet};

use crate::node::{GRAPH_TYPE, Graph, KnownNode, Node, PortRef};

/// TODO: a project holds several documents (photo, video, audio; design §3.1).
/// For now there is one root container.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub(crate) root: NodeId,
    pub(crate) nodes: OrdMap<NodeId, Node>,
    /// The container holding each node. The root has no entry.
    pub(crate) parents: OrdMap<NodeId, NodeId>,
}

impl Document {
    /// An empty document whose root is a Graph container.
    pub fn new(root: NodeId) -> Document {
        let node = Node::Known(KnownNode {
            type_id: GRAPH_TYPE.into(),
            version: 1,
            name: "Main".into(),
            params: OrdMap::new(),
            graph: Some(Graph::default()),
        });
        Document {
            root,
            nodes: OrdMap::unit(root, node),
            parents: OrdMap::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
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
            if let Some(g) = self.graph(id) {
                stack.extend(g.nodes.iter().rev().copied());
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
            if let Some(g) = node.graph() {
                for child in &g.nodes {
                    if self.parent(*child) != Some(id) {
                        return Err(format!("{child} has the wrong parent"));
                    }
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
