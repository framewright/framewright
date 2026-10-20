//! Node type descriptions: plain data, with no behaviour.

use serde::Serialize;

use crate::value::{ParamValue, ValueError};

/// The type id of the Graph container. The document's root is one.
pub const GRAPH_TYPE: &str = "fw.graph";

/// The type id of the Timeline container. A video document's root is one.
pub const TIMELINE_TYPE: &str = "fw.timeline";

/// The type id of the clip: media placed on a timeline, with a graph inside
/// that works on it.
pub const CLIP_TYPE: &str = "fw.clip";

/// The type id of the Output node, which marks what a Graph container delivers.
pub const OUTPUT_TYPE: &str = "fw.output";

/// The type id of the Input node: inside a node's graph, what the node
/// supplies (a clip's media). See [`crate::ImageBehaviour::source`].
pub const INPUT_TYPE: &str = "fw.input";

/// Everything the engine and UI need to know about a node type without running it.
///
/// The parameter schema is the single source of truth for the inspector,
/// saving, scripting, expressions and keyframing.
#[derive(Debug, Clone, Serialize)]
pub struct NodeTypeDesc {
    /// Namespaced and stable, e.g. `"fw.exposure"`. Never shown to users.
    pub id: String,
    /// Bumped whenever saved parameters change meaning. Older versions are
    /// migrated on load; newer ones are kept untouched as unknown nodes.
    pub version: u32,
    /// Shown to users, e.g. `"Exposure"`.
    pub name: String,
    /// Groups the type in the node library, e.g. `"Colour"`.
    pub category: String,
    pub inputs: Vec<PortDesc>,
    pub outputs: Vec<PortDesc>,
    pub params: Vec<ParamDesc>,
    /// Set when the output depends on time even with constant parameters
    /// (video sources, noise with a time seed). Otherwise time is left out of
    /// the cache key, so a static node renders once.
    pub varies_with_time: bool,
    /// Set when the type stores its contents in a container structure.
    pub container: Option<ContainerKind>,
}

impl NodeTypeDesc {
    pub fn param(&self, id: &str) -> Option<&ParamDesc> {
        self.params.iter().find(|p| p.id == id)
    }

    pub fn input(&self, id: &str) -> Option<&PortDesc> {
        self.inputs.iter().find(|p| p.id == id)
    }

    pub fn output(&self, id: &str) -> Option<&PortDesc> {
        self.outputs.iter().find(|p| p.id == id)
    }
}

/// The typed containers. Each stores its contents in its own structure and
/// has its own editor, but all share the node interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    Graph,
    Timeline,
    // TODO: LayerStack, Scene3D, Mixer.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Image,
    Mask,
    Audio,
    Scene,
    Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortDesc {
    pub id: String,
    pub name: String,
    pub ty: PortType,
    /// An unconnected optional input is not an error.
    pub optional: bool,
}

impl PortDesc {
    pub fn new(id: &str, name: &str, ty: PortType) -> PortDesc {
        PortDesc {
            id: id.into(),
            name: name.into(),
            ty,
            optional: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    None,
    Pixels,
    Degrees,
    Seconds,
    Decibels,
    Stops,
    Ratio,
    Percent,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ParamKind {
    Float { unit: Unit },
    Int,
    Bool,
    Choice { options: Vec<ChoiceOption> },
    Vec2 { unit: Unit },
    Color,
    Text,
    Media,
    // TODO: point, rect, transform, curve, gradient, font, file.
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

/// An on-viewer handle, so plugins get gizmos without writing UI code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleKind {
    Point,
    // TODO: rect, angle, radius, transform.
}

#[derive(Debug, Clone, Serialize)]
pub struct ParamDesc {
    /// Stable, used in saved files and scripts, e.g. `"exposure"`.
    pub id: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: ParamValue,
    /// The range sliders cover. Typed values may go beyond it.
    pub soft_range: Option<(f64, f64)>,
    /// Values outside this range are rejected.
    pub hard_range: Option<(f64, f64)>,
    pub animatable: bool,
    /// The inspector section, e.g. `"Advanced"`. `None` is the main section.
    pub section: Option<String>,
    pub handle: Option<HandleKind>,
}

impl ParamDesc {
    pub fn new(id: &str, label: &str, kind: ParamKind, default: ParamValue) -> ParamDesc {
        ParamDesc {
            id: id.into(),
            label: label.into(),
            kind,
            default,
            soft_range: None,
            hard_range: None,
            animatable: true,
            section: None,
            handle: None,
        }
    }

    pub fn float(id: &str, label: &str, unit: Unit, default: f64) -> ParamDesc {
        ParamDesc::new(
            id,
            label,
            ParamKind::Float { unit },
            ParamValue::Float(default),
        )
    }

    pub fn choice(id: &str, label: &str, options: &[(&str, &str)], default: &str) -> ParamDesc {
        let options = options
            .iter()
            .map(|(id, label)| ChoiceOption {
                id: (*id).into(),
                label: (*label).into(),
            })
            .collect();
        ParamDesc::new(
            id,
            label,
            ParamKind::Choice { options },
            ParamValue::Text(default.into()),
        )
        .not_animatable()
    }

    pub fn media(id: &str, label: &str) -> ParamDesc {
        let empty = crate::MediaRef {
            path: String::new(),
        };
        ParamDesc::new(id, label, ParamKind::Media, ParamValue::Media(empty)).not_animatable()
    }

    pub fn soft_range(mut self, min: f64, max: f64) -> ParamDesc {
        self.soft_range = Some((min, max));
        self
    }

    pub fn hard_range(mut self, min: f64, max: f64) -> ParamDesc {
        self.hard_range = Some((min, max));
        self
    }

    pub fn not_animatable(mut self) -> ParamDesc {
        self.animatable = false;
        self
    }

    /// Checks a value against the schema, converting where the saved form is
    /// ambiguous (an integer for a float parameter, text for a choice).
    pub fn check(&self, value: ParamValue) -> Result<ParamValue, ValueError> {
        let wrong = |value: &ParamValue, expected| ValueError::WrongKind {
            expected,
            got: value.kind_name(),
        };
        let value = match (&self.kind, value) {
            (ParamKind::Float { .. }, ParamValue::Int(v)) => ParamValue::Float(v as f64),
            (ParamKind::Float { .. }, v @ ParamValue::Float(_)) => v,
            (ParamKind::Int, v @ ParamValue::Int(_)) => v,
            (ParamKind::Bool, v @ ParamValue::Bool(_)) => v,
            (ParamKind::Choice { options }, ParamValue::Text(s)) => {
                if !options.iter().any(|o| o.id == s) {
                    return Err(ValueError::UnknownChoice(s));
                }
                ParamValue::Text(s)
            }
            (ParamKind::Vec2 { .. }, ParamValue::Vec2(v)) => ParamValue::Vec2(v),
            (ParamKind::Vec2 { .. }, ParamValue::Int(_) | ParamValue::Float(_)) => {
                return Err(ValueError::WrongKind {
                    expected: "vec2",
                    got: "number",
                });
            }
            (ParamKind::Color, v @ ParamValue::Color(_)) => v,
            (ParamKind::Text, v @ ParamValue::Text(_)) => v,
            (ParamKind::Media, v @ ParamValue::Media(_)) => v,
            (kind, v) => return Err(wrong(&v, kind_name(kind))),
        };
        let numbers: Vec<f64> = match &value {
            ParamValue::Float(v) => vec![*v],
            ParamValue::Vec2(v) => v.to_vec(),
            ParamValue::Color(c) => c.rgba.to_vec(),
            _ => vec![],
        };
        for v in numbers {
            if !v.is_finite() {
                return Err(ValueError::NotFinite(v));
            }
            if let Some((min, max)) = self.hard_range
                && !(min..=max).contains(&v)
            {
                return Err(ValueError::OutOfRange { value: v, min, max });
            }
        }
        if let (ParamValue::Int(v), Some((min, max))) = (&value, self.hard_range)
            && !(min..=max).contains(&(*v as f64))
        {
            return Err(ValueError::OutOfRange {
                value: *v as f64,
                min,
                max,
            });
        }
        Ok(value)
    }
}

fn kind_name(kind: &ParamKind) -> &'static str {
    match kind {
        ParamKind::Float { .. } => "float",
        ParamKind::Int => "int",
        ParamKind::Bool => "bool",
        ParamKind::Choice { .. } => "choice",
        ParamKind::Vec2 { .. } => "vec2",
        ParamKind::Color => "color",
        ParamKind::Text => "text",
        ParamKind::Media => "media",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_converts_and_validates() {
        let ev = ParamDesc::float("exposure", "Exposure", Unit::Stops, 0.0).hard_range(-20.0, 20.0);
        assert_eq!(ev.check(ParamValue::Int(2)), Ok(ParamValue::Float(2.0)));
        assert!(matches!(
            ev.check(ParamValue::Float(f64::NAN)),
            Err(ValueError::NotFinite(_))
        ));
        assert!(matches!(
            ev.check(ParamValue::Float(30.0)),
            Err(ValueError::OutOfRange { .. })
        ));
        assert!(matches!(
            ev.check(ParamValue::Bool(true)),
            Err(ValueError::WrongKind { .. })
        ));

        let space = ParamDesc::choice(
            "space",
            "Colour space",
            &[("srgb", "sRGB"), ("acescg", "ACEScg")],
            "srgb",
        );
        assert!(space.check(ParamValue::Text("acescg".into())).is_ok());
        assert_eq!(
            space.check(ParamValue::Text("p3".into())),
            Err(ValueError::UnknownChoice("p3".into()))
        );
    }
}
