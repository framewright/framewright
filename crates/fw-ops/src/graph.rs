//! The Graph container's description. Its lowering lives in the compiler.

use fw_plugin::{ContainerKind, GRAPH_TYPE, NodeType, NodeTypeDesc};

pub fn node_type() -> NodeType {
    NodeType::new(NodeTypeDesc {
        id: GRAPH_TYPE.into(),
        version: 1,
        name: "Graph".into(),
        category: "Containers".into(),
        // TODO: published inputs and outputs once graphs nest (group input/output nodes).
        inputs: vec![],
        outputs: vec![],
        params: vec![],
        varies_with_time: false,
        container: Some(ContainerKind::Graph),
        can_hold_graph: false,
    })
}
