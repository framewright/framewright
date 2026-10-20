//! Framewright's built-in nodes.
//!
//! They register through the same [`Registry`] API that plugins use, so
//! nothing here gets special treatment from the engine. The per-pixel ones
//! are shader plugins (in `plugins/`), embedded in the binary.

use std::path::PathBuf;

mod color;
mod graph;
mod input;
mod output;
mod read_image;
mod timeline;

use fw_plugin::{Registry, RegistryError};

pub use color::{SPACE_CHOICES, space_from_choice};
pub use fw_shader::{Problem as PluginProblem, plugin_folders};

/// The built-in shader plugins: each one's manifest and shader.
const SHADER_PLUGINS: [(&str, &str); 5] = [
    (
        include_str!("../plugins/exposure/plugin.toml"),
        include_str!("../plugins/exposure/shader.wgsl"),
    ),
    (
        include_str!("../plugins/saturation/plugin.toml"),
        include_str!("../plugins/saturation/shader.wgsl"),
    ),
    (
        include_str!("../plugins/invert/plugin.toml"),
        include_str!("../plugins/invert/shader.wgsl"),
    ),
    (
        include_str!("../plugins/vignette/plugin.toml"),
        include_str!("../plugins/vignette/shader.wgsl"),
    ),
    (
        include_str!("../plugins/mix/plugin.toml"),
        include_str!("../plugins/mix/shader.wgsl"),
    ),
];

/// Registers every built-in node type.
pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(graph::node_type())?;
    registry.register(read_image::node_type())?;
    for (manifest, shader) in SHADER_PLUGINS {
        let invalid = |problem| RegistryError::Invalid {
            id: "a built-in shader plugin".into(),
            problem,
        };
        let plugin =
            fw_shader::plugin(manifest, shader, fw_shader::DEFAULT_SHADER).map_err(invalid)?;
        if let Some(problem) = plugin.problem {
            return Err(invalid(problem));
        }
        registry.register(plugin.node_type)?;
    }
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

/// The registry the app and the command line use: the built-in nodes, then
/// the shader plugins under each of `plugin_folders` (see
/// [`plugin_folders`]). Also returns what was wrong with plugins.
pub fn registry(plugin_folders: &[PathBuf]) -> (Registry, Vec<PluginProblem>) {
    let mut registry = builtin_registry();
    let problems = fw_shader::register_folders(&mut registry, plugin_folders);
    (registry, problems)
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
            "fw.saturation",
            "fw.invert",
            "fw.vignette",
            "fw.mix",
            "fw.output",
            "fw.input",
            "fw.timeline",
            "fw.clip",
        ] {
            assert!(registry.get(id).is_some(), "{id} is missing");
        }
    }
}
