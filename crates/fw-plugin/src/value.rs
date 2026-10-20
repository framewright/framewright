//! Plain parameter values, as nodes see them after animation is resolved.

use fw_core::{ColorSpace, KeyHasher};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValueError {
    #[error("expected {expected}, got {got}")]
    WrongKind {
        expected: &'static str,
        got: &'static str,
    },
    #[error("{0} is not a finite number")]
    NotFinite(f64),
    #[error("{value} is outside the allowed range {min}..={max}")]
    OutOfRange { value: f64, min: f64, max: f64 },
    #[error("{0:?} is not one of the options")]
    UnknownChoice(String),
    #[error("this parameter cannot be animated")]
    NotAnimatable,
}

/// A color parameter. Values are unpremultiplied and may exceed 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Color {
    pub rgba: [f64; 4],
    pub space: ColorSpace,
}

/// A reference to a media file.
///
/// TODO: a project media pool with content fingerprints, so relinking keeps
/// caches (design §4.3). For now this is a path, relative to the project file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRef {
    pub path: String,
}

/// A parameter value.
///
/// Saved in its natural JSON form (`0.5`, `true`, `[1.0, 2.0]`, `"text"`), so
/// documents stay readable. Choices are stored as text and checked against the
/// schema. Integers and floats are distinguished by the schema too: see
/// [`crate::ParamDesc::check`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Vec2([f64; 2]),
    Text(String),
    Color(Color),
    Media(MediaRef),
}

impl ParamValue {
    pub fn kind_name(&self) -> &'static str {
        match self {
            ParamValue::Bool(_) => "bool",
            ParamValue::Int(_) => "int",
            ParamValue::Float(_) => "float",
            ParamValue::Vec2(_) => "vec2",
            ParamValue::Text(_) => "text",
            ParamValue::Color(_) => "color",
            ParamValue::Media(_) => "media",
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            ParamValue::Float(v) => Some(v),
            ParamValue::Int(v) => Some(v as f64),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ParamValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Interpolates between two values of the same kind. Kinds that can't be
    /// blended (text, bool, media) hold `self` until `t` reaches 1.
    pub fn lerp(&self, other: &ParamValue, t: f64) -> ParamValue {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        match (self, other) {
            (ParamValue::Float(a), ParamValue::Float(b)) => ParamValue::Float(mix(*a, *b)),
            (ParamValue::Int(a), ParamValue::Int(b)) => {
                ParamValue::Int(mix(*a as f64, *b as f64).round() as i64)
            }
            (ParamValue::Vec2(a), ParamValue::Vec2(b)) => {
                ParamValue::Vec2([mix(a[0], b[0]), mix(a[1], b[1])])
            }
            (ParamValue::Color(a), ParamValue::Color(b)) if a.space == b.space => {
                ParamValue::Color(Color {
                    rgba: [0, 1, 2, 3].map(|i| mix(a.rgba[i], b.rgba[i])),
                    space: a.space,
                })
            }
            _ if t >= 1.0 => other.clone(),
            _ => self.clone(),
        }
    }

    pub fn hash_into(&self, h: &mut KeyHasher) {
        match self {
            ParamValue::Bool(v) => h.u8(0).bool(*v),
            ParamValue::Int(v) => h.u8(1).i64(*v),
            ParamValue::Float(v) => h.u8(2).f64(*v),
            ParamValue::Vec2([x, y]) => h.u8(3).f64(*x).f64(*y),
            ParamValue::Text(s) => h.u8(4).str(s),
            ParamValue::Color(c) => {
                h.u8(5);
                for v in c.rgba {
                    h.f64(v);
                }
                c.space.hash_into(h);
                h
            }
            ParamValue::Media(m) => h.u8(6).str(&m.path),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_json_round_trips() {
        let cases = [
            ("true", ParamValue::Bool(true)),
            ("3", ParamValue::Int(3)),
            ("0.5", ParamValue::Float(0.5)),
            ("1.0", ParamValue::Float(1.0)),
            ("[1.0,2.5]", ParamValue::Vec2([1.0, 2.5])),
            ("\"srgb\"", ParamValue::Text("srgb".into())),
            (
                "{\"path\":\"a.png\"}",
                ParamValue::Media(MediaRef {
                    path: "a.png".into(),
                }),
            ),
        ];
        for (json, value) in cases {
            assert_eq!(
                serde_json::from_str::<ParamValue>(json).unwrap(),
                value,
                "{json}"
            );
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
        }
    }

    #[test]
    fn lerp_blends_numbers_and_holds_text() {
        let a = ParamValue::Float(0.0);
        assert_eq!(
            a.lerp(&ParamValue::Float(2.0), 0.25),
            ParamValue::Float(0.5)
        );
        let s = ParamValue::Text("a".into());
        assert_eq!(s.lerp(&ParamValue::Text("b".into()), 0.9), s);
    }
}
