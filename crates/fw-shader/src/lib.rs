//! Shader-node plugins: a folder holding `plugin.toml` and a WGSL file
//! becomes a node type (design §11).
//!
//! The WGSL is the body of a per-pixel function, the same as a built-in
//! [`fw_plugin::PixelOp`]'s. It sees `c` (the first input's premultiplied
//! pixel), the other inputs by their port ids, each parameter by its id,
//! `pos` (the pixel's center in canvas units) and `frame` (the image's
//! region, `min.xy, max.xy`), and returns a `vec4<f32>`. Colors arrive
//! unpremultiplied in the space the node works in, and a choice arrives as
//! the index of the option picked.
//!
//! TODO: helper functions outside the body, operations that read
//! neighboring pixels, and a time seed for noise.

mod manifest;
mod node;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fw_plugin::{NodeType, Registry, RegistryError};

pub use manifest::{DEFAULT_SHADER, MANIFEST};

use crate::manifest::Manifest;
use crate::node::{ShaderNode, op_body, validate};

/// A shader plugin, ready to register.
pub struct Plugin {
    pub node_type: NodeType,
    /// Why its shader can't be used. The type still registers, so documents
    /// keep their nodes, and rendering one reports this.
    pub problem: Option<String>,
}

/// The node type a manifest and shader describe. `shader_file` names the
/// shader in messages. Fails only when the manifest can't be used.
pub fn plugin(manifest: &str, shader: &str, shader_file: &str) -> Result<Plugin, String> {
    let desc = Manifest::parse(manifest)?.to_desc()?;
    let problem = validate(&desc, shader, shader_file).err();
    let behavior = ShaderNode {
        label: desc.name.to_lowercase(),
        params: desc.params.clone(),
        wgsl: Arc::from(op_body(&desc, shader).0),
        problem: problem.clone(),
    };
    // Editing either file changes every cache key the node is part of.
    let implementation = format!("{manifest}\0{shader}");
    Ok(Plugin {
        node_type: NodeType::new(desc).with_image(behavior, &implementation),
        problem,
    })
}

/// Reads the plugin in `folder`.
pub fn load(folder: &Path) -> Result<Plugin, String> {
    let read = |name: &str| {
        let path = folder.join(name);
        std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))
    };
    let manifest = read(MANIFEST)?;
    let shader_file = Manifest::parse(&manifest)?
        .shader
        .unwrap_or_else(|| DEFAULT_SHADER.into());
    let shader = read(&shader_file)?;
    plugin(&manifest, &shader, &shader_file)
}

/// Where plugins are looked for after the built-in ones, in order:
/// `$XDG_DATA_HOME/framewright/plugins` (or `~/.local/share/…`), then each
/// folder in `FW_PLUGIN_PATH`.
pub fn plugin_folders() -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")));
    let mut folders: Vec<PathBuf> = data
        .map(|d| d.join("framewright/plugins"))
        .into_iter()
        .collect();
    if let Some(path) = std::env::var_os("FW_PLUGIN_PATH") {
        folders.extend(std::env::split_paths(&path).filter(|p| !p.as_os_str().is_empty()));
    }
    folders
}

/// The plugins under `folder`: itself if it is one, and each folder in it
/// that is one, in name order. A folder that doesn't exist holds none.
pub fn plugins_in(folder: &Path) -> Vec<PathBuf> {
    if folder.join(MANIFEST).is_file() {
        return vec![folder.to_owned()];
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return vec![];
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    found.sort();
    found
}

/// Something wrong with a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    /// The plugin's folder.
    pub plugin: PathBuf,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.plugin.display(), self.message)
    }
}

/// Registers the plugins under each of `folders`, in order. A plugin that
/// reuses an id already registered is skipped. Returns what went wrong.
pub fn register_folders(registry: &mut Registry, folders: &[PathBuf]) -> Vec<Problem> {
    let mut problems = vec![];
    for folder in folders.iter().flat_map(|f| plugins_in(f)) {
        let mut report = |message: String| {
            problems.push(Problem {
                plugin: folder.clone(),
                message,
            })
        };
        let plugin = match load(&folder) {
            Ok(plugin) => plugin,
            Err(message) => {
                report(message);
                continue;
            }
        };
        if let Some(problem) = &plugin.problem {
            report(format!("its shader doesn't work: {problem}"));
        }
        match registry.register(plugin.node_type) {
            Ok(()) => {}
            Err(RegistryError::Duplicate(id)) => {
                report(format!("{id} is already loaded, so this one is skipped"));
            }
            Err(e) => report(e.to_string()),
        }
    }
    problems
}
