//! `plugin.toml`: what a shader node is called, its inputs and its parameters.
//!
//! ```toml
//! id = "fw.saturation"
//! version = 1
//! name = "Saturation"
//! category = "Color"
//!
//! [[params]]
//! id = "saturation"
//! name = "Saturation"
//! type = "float"
//! unit = "percent"
//! default = 100.0
//! range = [0.0, 200.0]
//! ```
//!
//! Inputs default to one image called `image`. Each parameter's `type` is
//! `float`, `int`, `bool`, `choice`, `vec2` or `color`; `range` is what a
//! slider covers and `limits` what's allowed.

use fw_core::ColorSpace;
use fw_plugin::{
    ChoiceOption, Color, NodeTypeDesc, ParamDesc, ParamKind, ParamValue, PortDesc, PortType, Unit,
};
use serde::Deserialize;

/// The file name of a plugin's manifest.
pub const MANIFEST: &str = "plugin.toml";

/// The shader file read when the manifest names none.
pub const DEFAULT_SHADER: &str = "shader.wgsl";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    id: String,
    version: u32,
    name: String,
    category: String,
    #[serde(default)]
    pub shader: Option<String>,
    #[serde(default)]
    varies_with_time: bool,
    #[serde(default = "default_inputs")]
    inputs: Vec<InputEntry>,
    #[serde(default)]
    params: Vec<ParamEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEntry {
    id: String,
    name: String,
}

fn default_inputs() -> Vec<InputEntry> {
    vec![InputEntry {
        id: "image".into(),
        name: "Image".into(),
    }]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParamEntry {
    id: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    default: Option<toml::Value>,
    unit: Option<String>,
    range: Option<[f64; 2]>,
    limits: Option<[f64; 2]>,
    section: Option<String>,
    animatable: Option<bool>,
    #[serde(default)]
    options: Vec<OptionEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionEntry {
    id: String,
    name: String,
}

/// Names in scope where a shader body runs: the op function's arguments and
/// `base`, and the kernel's own bindings and helpers. Parameters and inputs
/// can't take them.
const TAKEN: &[&str] = &[
    "c", "pos", "frame", "base", "prm", "params", "src", "dst", "main",
];

/// Words WGSL keeps for itself that make likely parameter names.
const KEYWORDS: &[&str] = &[
    "alias", "bool", "break", "case", "const", "continue", "default", "discard", "else", "enable",
    "f16", "f32", "false", "fn", "for", "i32", "if", "let", "loop", "override", "return", "struct",
    "switch", "true", "u32", "var", "while", "mat2x2", "mat3x3", "mat4x4", "vec2", "vec3", "vec4",
    "array", "ptr", "sampler", "texture", "atomic",
];

/// Checks `name` can be used as a WGSL name inside a shader body.
fn check_name(what: &str, name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    if !starts_well || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(format!(
            "{what} id {name:?} must be lower case letters, digits and _, starting with a letter"
        ));
    }
    let numbered = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    };
    let kernel_name = TAKEN.contains(&name)
        || ["e", "ex"].into_iter().any(numbered)
        || name.starts_with("fw_")
        || name.starts_with("op_")
        || name.starts_with("__");
    if kernel_name {
        return Err(format!(
            "{what} id {name:?} is a name the kernel around the shader uses"
        ));
    }
    if KEYWORDS.contains(&name) {
        return Err(format!("{what} id {name:?} is a WGSL word"));
    }
    Ok(())
}

fn unit(name: &str) -> Result<Unit, String> {
    Ok(match name {
        "none" => Unit::None,
        "pixels" => Unit::Pixels,
        "degrees" => Unit::Degrees,
        "seconds" => Unit::Seconds,
        "decibels" => Unit::Decibels,
        "stops" => Unit::Stops,
        "ratio" => Unit::Ratio,
        "percent" => Unit::Percent,
        other => {
            return Err(format!(
                "unknown unit {other:?}: use none, pixels, degrees, seconds, decibels, stops, \
                 ratio or percent"
            ));
        }
    })
}

fn number(value: &toml::Value) -> Option<f64> {
    match value {
        toml::Value::Float(v) => Some(*v),
        toml::Value::Integer(v) => Some(*v as f64),
        _ => None,
    }
}

fn numbers<const N: usize>(value: &toml::Value) -> Option<[f64; N]> {
    let array = value.as_array()?;
    let values: Vec<f64> = array.iter().map(number).collect::<Option<_>>()?;
    values.try_into().ok()
}

impl ParamEntry {
    fn to_desc(&self) -> Result<ParamDesc, String> {
        check_name("parameter", &self.id)?;
        let unit = self
            .unit
            .as_deref()
            .map(unit)
            .transpose()?
            .unwrap_or(Unit::None);
        let wrong_default =
            |expected: &str| format!("parameter {:?}: the default must be {expected}", self.id);
        let default = self.default.as_ref();
        let (kind, default) = match self.kind.as_str() {
            "float" => (
                ParamKind::Float { unit },
                ParamValue::Float(
                    default
                        .map_or(Some(0.0), number)
                        .ok_or_else(|| wrong_default("a number"))?,
                ),
            ),
            "int" => (
                ParamKind::Int,
                ParamValue::Int(
                    default
                        .map_or(Some(0), toml::Value::as_integer)
                        .ok_or_else(|| wrong_default("a whole number"))?,
                ),
            ),
            "bool" => (
                ParamKind::Bool,
                ParamValue::Bool(
                    default
                        .map_or(Some(false), toml::Value::as_bool)
                        .ok_or_else(|| wrong_default("true or false"))?,
                ),
            ),
            "choice" => {
                if self.options.is_empty() {
                    return Err(format!("choice parameter {:?} has no options", self.id));
                }
                let first = self.options[0].id.clone();
                let chosen = match default {
                    None => first,
                    Some(v) => v
                        .as_str()
                        .ok_or_else(|| wrong_default("one of the options' ids"))?
                        .to_owned(),
                };
                let options = self
                    .options
                    .iter()
                    .map(|o| ChoiceOption {
                        id: o.id.clone(),
                        label: o.name.clone(),
                    })
                    .collect();
                (ParamKind::Choice { options }, ParamValue::Text(chosen))
            }
            "vec2" => (
                ParamKind::Vec2 { unit },
                ParamValue::Vec2(
                    default
                        .map_or(Some([0.0; 2]), numbers::<2>)
                        .ok_or_else(|| wrong_default("two numbers, [x, y]"))?,
                ),
            ),
            "color" => {
                let rgba = match default {
                    None => [0.0, 0.0, 0.0, 1.0],
                    Some(v) => numbers::<4>(v)
                        .or_else(|| numbers::<3>(v).map(|[r, g, b]| [r, g, b, 1.0]))
                        .ok_or_else(|| {
                            wrong_default("[r, g, b] or [r, g, b, a] in linear Rec.709")
                        })?,
                };
                let color = Color {
                    rgba,
                    space: ColorSpace::LINEAR_REC709,
                };
                (ParamKind::Color, ParamValue::Color(color))
            }
            other => {
                return Err(format!(
                    "parameter {:?} has unknown type {other:?}: use float, int, bool, choice, vec2 \
                     or color",
                    self.id
                ));
            }
        };
        if !matches!(kind, ParamKind::Choice { .. }) && !self.options.is_empty() {
            return Err(format!(
                "parameter {:?}: only choices have options",
                self.id
            ));
        }
        let mut desc = ParamDesc::new(&self.id, &self.name, kind, default);
        desc.soft_range = self.range.map(|[a, b]| (a, b));
        desc.hard_range = self.limits.map(|[a, b]| (a, b));
        desc.section = self.section.clone();
        // Choices pick code paths, so they don't animate unless asked to.
        desc.animatable = self
            .animatable
            .unwrap_or(!matches!(desc.kind, ParamKind::Choice { .. }));
        let checked = desc
            .check(desc.default.clone())
            .map_err(|e| format!("parameter {:?}: the default: {e}", self.id))?;
        desc.default = checked;
        Ok(desc)
    }
}

impl Manifest {
    pub(crate) fn parse(text: &str) -> Result<Manifest, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// The node type it describes, checked as far as the manifest goes.
    pub(crate) fn to_desc(&self) -> Result<NodeTypeDesc, String> {
        if self.inputs.is_empty() {
            // TODO: generators, which make pixels from nothing.
            return Err("a shader node needs at least one image input".into());
        }
        let mut inputs = vec![];
        for input in &self.inputs {
            check_name("input", &input.id)?;
            inputs.push(PortDesc::new(&input.id, &input.name, PortType::Image));
        }
        let params = self
            .params
            .iter()
            .map(ParamEntry::to_desc)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(p) = params
            .iter()
            .find(|p| self.inputs.iter().any(|i| i.id == p.id))
        {
            return Err(format!("{:?} is both an input and a parameter", p.id));
        }
        Ok(NodeTypeDesc {
            id: self.id.clone(),
            version: self.version,
            name: self.name.clone(),
            category: self.category.clone(),
            inputs,
            outputs: vec![PortDesc::new("image", "Image", PortType::Image)],
            params,
            varies_with_time: self.varies_with_time,
            container: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(text: &str) -> Result<NodeTypeDesc, String> {
        Manifest::parse(text)?.to_desc()
    }

    const HEAD: &str = "id = \"test.node\"\nversion = 1\nname = \"Node\"\ncategory = \"Test\"\n";

    #[test]
    fn every_parameter_type_becomes_a_parameter() {
        let text = format!(
            "{HEAD}
[[params]]
id = \"amount\"
name = \"Amount\"
type = \"float\"
unit = \"percent\"
default = 50
range = [0, 100]
limits = [0, 400]

[[params]]
id = \"count\"
name = \"Count\"
type = \"int\"
default = 3

[[params]]
id = \"flip\"
name = \"Flip\"
type = \"bool\"
section = \"Advanced\"

[[params]]
id = \"mode\"
name = \"Mode\"
type = \"choice\"
options = [{{ id = \"soft\", name = \"Soft\" }}, {{ id = \"hard\", name = \"Hard\" }}]
default = \"hard\"

[[params]]
id = \"center\"
name = \"Center\"
type = \"vec2\"
default = [0.5, 0.5]

[[params]]
id = \"tint\"
name = \"Tint\"
type = \"color\"
default = [1, 0.5, 0.25]
"
        );
        let desc = desc(&text).unwrap();
        assert_eq!(desc.inputs.len(), 1, "one image input by default");
        let amount = desc.param("amount").unwrap();
        assert_eq!(amount.default, ParamValue::Float(50.0));
        assert_eq!(amount.soft_range, Some((0.0, 100.0)));
        assert_eq!(amount.hard_range, Some((0.0, 400.0)));
        assert_eq!(desc.param("count").unwrap().default, ParamValue::Int(3));
        assert_eq!(
            desc.param("flip").unwrap().section.as_deref(),
            Some("Advanced")
        );
        let mode = desc.param("mode").unwrap();
        assert_eq!(mode.default, ParamValue::Text("hard".into()));
        assert!(!mode.animatable);
        assert_eq!(
            desc.param("center").unwrap().default,
            ParamValue::Vec2([0.5, 0.5])
        );
        let ParamValue::Color(tint) = desc.param("tint").unwrap().default else {
            panic!("a color");
        };
        assert_eq!(tint.rgba, [1.0, 0.5, 0.25, 1.0]);
    }

    #[test]
    fn names_the_kernel_uses_are_refused() {
        for name in [
            "c", "pos", "e0", "ex12", "fw_mat3", "op_1", "let", "Amount", "2x",
        ] {
            let text =
                format!("{HEAD}[[params]]\nid = \"{name}\"\nname = \"N\"\ntype = \"float\"\n");
            assert!(desc(&text).is_err(), "{name} was allowed");
        }
        let clash = format!("{HEAD}[[params]]\nid = \"image\"\nname = \"N\"\ntype = \"float\"\n");
        assert!(
            desc(&clash)
                .unwrap_err()
                .contains("both an input and a parameter")
        );
    }

    #[test]
    fn mistakes_are_explained() {
        let unknown = format!("{HEAD}color = \"red\"\n");
        assert!(desc(&unknown).unwrap_err().contains("unknown field"));
        let out_of_range = format!(
            "{HEAD}[[params]]\nid = \"a\"\nname = \"A\"\ntype = \"float\"\ndefault = 5\nlimits = [0, 1]\n"
        );
        assert!(desc(&out_of_range).unwrap_err().contains("the default"));
        let bad_choice = format!(
            "{HEAD}[[params]]\nid = \"a\"\nname = \"A\"\ntype = \"choice\"\noptions = [{{ id = \"x\", name = \"X\" }}]\ndefault = \"y\"\n"
        );
        assert!(desc(&bad_choice).is_err());
    }
}
