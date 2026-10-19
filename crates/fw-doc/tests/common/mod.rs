//! Node types and helpers shared by the document tests.

#![allow(dead_code)]

use fw_plugin::{
    ContainerKind, NodeType, NodeTypeDesc, ParamDesc, PortDesc, PortType, Registry, Unit,
};

pub fn desc(id: &str, name: &str) -> NodeTypeDesc {
    NodeTypeDesc {
        id: id.into(),
        version: 1,
        name: name.into(),
        category: "Test".into(),
        inputs: vec![],
        outputs: vec![],
        params: vec![],
        varies_with_time: false,
        container: None,
    }
}

pub fn graph_type() -> NodeTypeDesc {
    NodeTypeDesc {
        container: Some(ContainerKind::Graph),
        ..desc("fw.graph", "Graph")
    }
}

pub fn source_type() -> NodeTypeDesc {
    NodeTypeDesc {
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![ParamDesc::media("file", "File")],
        ..desc("test.source", "Source")
    }
}

pub fn gain_type() -> NodeTypeDesc {
    NodeTypeDesc {
        inputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![
            ParamDesc::float("gain", "Gain", Unit::Stops, 0.0)
                .soft_range(-5.0, 5.0)
                .hard_range(-20.0, 20.0),
            ParamDesc::choice(
                "mode",
                "Mode",
                &[("multiply", "Multiply"), ("add", "Add")],
                "multiply",
            ),
        ],
        ..desc("test.gain", "Gain")
    }
}

pub fn mask_type() -> NodeTypeDesc {
    NodeTypeDesc {
        outputs: vec![PortDesc::new("mask", "Mask", PortType::Mask)],
        ..desc("test.mask", "Mask")
    }
}

pub fn sink_type() -> NodeTypeDesc {
    NodeTypeDesc {
        inputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        ..desc("test.sink", "Sink")
    }
}

/// A Graph container type other than the root's, so nested containers can go missing.
pub fn group_type() -> NodeTypeDesc {
    NodeTypeDesc {
        container: Some(ContainerKind::Graph),
        inputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        ..desc("test.group", "Group")
    }
}

pub fn timeline_type() -> NodeTypeDesc {
    NodeTypeDesc {
        container: Some(ContainerKind::Timeline),
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        ..desc("fw.timeline", "Timeline")
    }
}

pub fn clip_type() -> NodeTypeDesc {
    NodeTypeDesc {
        outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
        params: vec![
            ParamDesc::media("file", "File"),
            ParamDesc::float("opacity", "Opacity", Unit::Percent, 100.0),
        ],
        ..desc("test.clip", "Clip")
    }
}

pub fn registry_with(types: Vec<NodeTypeDesc>) -> Registry {
    let mut r = Registry::new();
    for t in types {
        r.register(NodeType::new(t)).unwrap();
    }
    r
}

pub fn full_registry() -> Registry {
    registry_with(vec![
        graph_type(),
        source_type(),
        gain_type(),
        mask_type(),
        sink_type(),
        group_type(),
        timeline_type(),
        clip_type(),
    ])
}
