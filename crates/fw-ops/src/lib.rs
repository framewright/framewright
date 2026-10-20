//! Framewright's built-in nodes.
//!
//! They register through the same [`Registry`] API that plugins use, so
//! nothing here gets special treatment from the engine.

mod color;
mod exposure;
mod graph;
mod input;
mod output;
mod read_image;
mod timeline;

use fw_plugin::{Registry, RegistryError};

pub use color::{SPACE_CHOICES, space_from_choice};

/// Registers every built-in node type.
pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(graph::node_type())?;
    registry.register(read_image::node_type())?;
    registry.register(exposure::node_type())?;
    registry.register(output::node_type())?;
    registry.register(input::node_type())?;
    registry.register(timeline::timeline_type())?;
    registry.register(timeline::clip_type())?;
    Ok(())
}

/// A registry holding just the built-in nodes.
pub fn builtin_registry() -> Registry {
    let mut registry = Registry::new();
    register(&mut registry).expect("built-in node types are valid");
    registry
}

#[cfg(test)]
mod tests {
    #[test]
    fn built_ins_register() {
        let registry = super::builtin_registry();
        for id in [
            "fw.graph",
            "fw.read_image",
            "fw.exposure",
            "fw.output",
            "fw.input",
            "fw.timeline",
            "fw.clip",
        ] {
            assert!(registry.get(id).is_some(), "{id} is missing");
        }
    }
}
