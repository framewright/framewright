//! The registry of node types. Built-ins and plugins register the same way.

use std::collections::BTreeMap;
use std::sync::Arc;

use fw_core::{ContentHash, KeyHasher};
use serde_json::{Map, Value};

use crate::audio::AudioBehavior;
use crate::desc::NodeTypeDesc;
use crate::image::ImageBehavior;

/// Upgrades saved parameters from `from_version` to the current version, in place.
pub type MigrateFn = fn(from_version: u32, params: &mut Map<String, Value>) -> Result<(), String>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("node type {0:?} is already registered")]
    Duplicate(String),
    #[error("node type {id:?}: {problem}")]
    Invalid { id: String, problem: String },
}

/// A registered node type: its description plus behavior.
pub struct NodeType {
    pub desc: NodeTypeDesc,
    pub migrate: Option<MigrateFn>,
    pub image: Option<Arc<dyn ImageBehavior>>,
    pub audio: Option<Arc<dyn AudioBehavior>>,
    /// Changes whenever the implementation might produce different pixels, so
    /// cached results from older code are never reused. Part of every cache key.
    pub impl_hash: ContentHash,
}

impl NodeType {
    pub fn new(desc: NodeTypeDesc) -> NodeType {
        let impl_hash = KeyHasher::new("fw.impl")
            .str(&desc.id)
            .u32(desc.version)
            .finish();
        NodeType {
            desc,
            migrate: None,
            image: None,
            audio: None,
            impl_hash,
        }
    }

    /// Adds real-time audio behavior.
    pub fn with_audio(mut self, behavior: impl AudioBehavior + 'static) -> NodeType {
        self.audio = Some(Arc::new(behavior));
        self
    }

    /// Adds image behavior. `implementation` is anything that changes when the
    /// output could change: shader source, a revision string, a build hash.
    pub fn with_image(
        mut self,
        behavior: impl ImageBehavior + 'static,
        implementation: &str,
    ) -> NodeType {
        self.image = Some(Arc::new(behavior));
        self.impl_hash = KeyHasher::new("fw.impl")
            .str(&self.desc.id)
            .u32(self.desc.version)
            .str(implementation)
            .finish();
        self
    }

    pub fn with_migration(mut self, migrate: MigrateFn) -> NodeType {
        self.migrate = Some(migrate);
        self
    }
}

#[derive(Default)]
pub struct Registry {
    types: BTreeMap<String, Arc<NodeType>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn register(&mut self, ty: NodeType) -> Result<(), RegistryError> {
        let desc = &ty.desc;
        if self.types.contains_key(&desc.id) {
            return Err(RegistryError::Duplicate(desc.id.clone()));
        }
        validate(desc).map_err(|problem| RegistryError::Invalid {
            id: desc.id.clone(),
            problem,
        })?;
        self.types.insert(desc.id.clone(), Arc::new(ty));
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Arc<NodeType>> {
        self.types.get(id)
    }

    pub fn desc(&self, id: &str) -> Option<&NodeTypeDesc> {
        self.types.get(id).map(|t| &t.desc)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<NodeType>> {
        self.types.values()
    }
}

fn validate(desc: &NodeTypeDesc) -> Result<(), String> {
    if desc.id.is_empty() || !desc.id.contains('.') {
        return Err("ids must be namespaced, e.g. \"fw.exposure\"".into());
    }
    if desc.version == 0 {
        return Err("versions start at 1".into());
    }
    // Port ids are saved inside "<node id>:<port>" strings.
    for port in desc.inputs.iter().chain(&desc.outputs) {
        if port.id.is_empty() || port.id.contains(':') {
            return Err(format!(
                "port id {:?} must be non-empty and contain no ':'",
                port.id
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in desc.params.iter().map(|p| &p.id) {
        if !seen.insert(("param", id)) {
            return Err(format!("parameter {id:?} is declared twice"));
        }
    }
    for id in desc.inputs.iter().map(|p| &p.id) {
        if !seen.insert(("input", id)) {
            return Err(format!("input {id:?} is declared twice"));
        }
    }
    for id in desc.outputs.iter().map(|p| &p.id) {
        if !seen.insert(("output", id)) {
            return Err(format!("output {id:?} is declared twice"));
        }
    }
    for p in &desc.params {
        p.check(p.default.clone())
            .map_err(|e| format!("default for {:?} is invalid: {e}", p.id))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desc::{ParamDesc, Unit};

    fn desc(id: &str) -> NodeTypeDesc {
        NodeTypeDesc {
            id: id.into(),
            version: 1,
            name: "Test".into(),
            category: "Test".into(),
            inputs: vec![],
            outputs: vec![],
            params: vec![ParamDesc::float("gain", "Gain", Unit::Ratio, 1.0)],
            varies_with_time: false,
            container: None,
        }
    }

    #[test]
    fn rejects_duplicates_and_bad_descriptions() {
        let mut r = Registry::new();
        r.register(NodeType::new(desc("test.a"))).unwrap();
        assert!(matches!(
            r.register(NodeType::new(desc("test.a"))),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(r.register(NodeType::new(desc("nonamespace"))).is_err());
        let mut bad = desc("test.b");
        bad.params[0] = bad.params[0].clone().hard_range(2.0, 3.0);
        assert!(r.register(NodeType::new(bad)).is_err());
        assert_eq!(r.desc("test.a").unwrap().name, "Test");
    }
}
