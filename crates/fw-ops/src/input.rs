//! Input: inside a node's graph, what the node supplies, such as a clip's
//! media. The compiler lowers it to the container's source.

use fw_plugin::{INPUT_TYPE, NodeType, NodeTypeDesc, PortDesc, PortType};

pub fn node_type() -> NodeType {
    NodeType::new(NodeTypeDesc {
        id: INPUT_TYPE.into(),
        version: 1,
        name: "Input".into(),
        category: "Sources".into(),
        inputs: vec![],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![],
        varies_with_time: false,
        container: None,
        can_hold_graph: false,
    })
}
