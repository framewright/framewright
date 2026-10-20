//! The graph every clip holds. It starts as Input → Output, the clip's media
//! passed straight through. The two nodes' ids are worked out from the clip's,
//! so a clip whose graph is still just that is saved without it and gets it
//! back on load.

use fw_core::NodeId;
use fw_plugin::{CLIP_TYPE, INPUT_TYPE, OUTPUT_TYPE, Registry};
use imbl::OrdMap;

use crate::command::CommandError;
use crate::document::Document;
use crate::node::{Graph, KnownNode, Node, PortRef};

/// The ids of a clip's starting Input and Output nodes.
pub fn clip_graph_ids(clip: NodeId) -> (NodeId, NodeId) {
    (clip.derived("input"), clip.derived("output"))
}

/// Whether nodes of this type start with Input → Output inside. Only clips
/// supply an image to their graph so far.
pub(crate) fn starts_with_graph(type_id: &str) -> bool {
    type_id == CLIP_TYPE
}

/// The starting nodes, with the names they're given.
fn starting_nodes(clip: NodeId) -> [(NodeId, &'static str, &'static str); 2] {
    let (input, output) = clip_graph_ids(clip);
    [
        (input, INPUT_TYPE, "Input"),
        (output, OUTPUT_TYPE, "Output"),
    ]
}

/// Adds `clip`'s starting Input and Output nodes to the document and returns
/// the graph holding them.
pub(crate) fn add_starting_graph(
    doc: &mut Document,
    registry: &Registry,
    clip: NodeId,
) -> Result<Graph, CommandError> {
    let mut graph = Graph::default();
    for (id, type_id, name) in starting_nodes(clip) {
        if doc.contains(id) {
            return Err(CommandError::DuplicateId(id));
        }
        let desc = registry
            .desc(type_id)
            .ok_or_else(|| CommandError::UnknownType(type_id.into()))?;
        let node = Node::Known(KnownNode {
            type_id: type_id.into(),
            version: desc.version,
            name: name.into(),
            params: OrdMap::new(),
            contents: None,
        });
        doc.nodes.insert(id, node);
        doc.parents.insert(id, clip);
        graph.nodes.insert(id);
    }
    let (input, output) = clip_graph_ids(clip);
    graph
        .inputs
        .insert(PortRef::new(output, "image"), PortRef::new(input, "image"));
    Ok(graph)
}

impl Document {
    /// Whether `clip`'s graph only passes its media through: one Input
    /// connected to one Output with its settings left alone.
    pub fn passes_media_through(&self, clip: NodeId) -> bool {
        self.passthrough_nodes(clip).is_some()
    }

    /// Whether `clip`'s graph is still exactly the one it started with, so
    /// the file can leave it out.
    pub(crate) fn keeps_starting_graph(&self, clip: NodeId) -> bool {
        let Some([input, output]) = self.passthrough_nodes(clip) else {
            return false;
        };
        let named = |id, name| self.node(id).is_some_and(|n| n.name() == name);
        (input, output) == clip_graph_ids(clip)
            && named(input, "Input")
            && named(output, "Output")
            && self.graph(clip).is_some_and(|g| g.positions.is_empty())
    }

    /// The Input and Output of a graph that only passes media through.
    fn passthrough_nodes(&self, clip: NodeId) -> Option<[NodeId; 2]> {
        let graph = self.graph(clip)?;
        let of_type = |type_id: &str| {
            graph.nodes.iter().copied().find(|id| {
                self.node(*id).and_then(Node::as_known).is_some_and(|n| {
                    n.type_id == type_id && n.params.is_empty() && n.contents.is_none()
                })
            })
        };
        let (input, output) = (of_type(INPUT_TYPE)?, of_type(OUTPUT_TYPE)?);
        let connected = graph.inputs.len() == 1
            && graph.inputs.get(&PortRef::new(output, "image"))
                == Some(&PortRef::new(input, "image"));
        (graph.nodes.len() == 2 && connected).then_some([input, output])
    }
}
