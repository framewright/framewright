//! Stored parameter values: constants or animation.
//!
//! Nodes never see these. The compiler resolves them to plain
//! [`ParamValue`]s at the requested time.

use fw_core::{Time, TimeError};
use fw_plugin::{ParamDesc, ParamValue, ValueError};
use serde::{Deserialize, Serialize};

/// A stored parameter: saved as the bare value for a constant (`"exposure": 0.5`)
/// or as `{"keyframes": [...]}` when animated.
///
/// TODO: expressions and links to Value outputs (design §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Param {
    Keyframed(Keyframes),
    Constant(ParamValue),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframes {
    /// Sorted by time, with no two keys at the same time.
    pub keyframes: Vec<Keyframe>,
}

/// TODO: anchor keys to a clip's start, end or a percentage of its length
/// once Timeline exists (design §5). Until then times are from the start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    pub t: Time,
    pub v: ParamValue,
    /// How to get from this key to the next.
    #[serde(default, skip_serializing_if = "Interp::is_linear")]
    pub interp: Interp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    #[default]
    Linear,
    Hold,
    // TODO: bezier with tangents.
}

impl Interp {
    fn is_linear(&self) -> bool {
        *self == Interp::Linear
    }
}

impl Param {
    /// The plain value at time `t`.
    pub fn value_at(&self, t: Time) -> Result<ParamValue, TimeError> {
        let keys = match self {
            Param::Constant(v) => return Ok(v.clone()),
            Param::Keyframed(k) => &k.keyframes,
        };
        let (first, last) = match (keys.first(), keys.last()) {
            (Some(first), Some(last)) => (first, last),
            _ => unreachable!("checked parameters always have at least one key"),
        };
        if t <= first.t {
            return Ok(first.v.clone());
        }
        if t >= last.t {
            return Ok(last.v.clone());
        }
        let next = keys.partition_point(|k| k.t <= t);
        let (a, b) = (&keys[next - 1], &keys[next]);
        Ok(match a.interp {
            Interp::Hold => a.v.clone(),
            Interp::Linear => {
                let w = t.checked_sub(a.t)?.checked_div(b.t.checked_sub(a.t)?)?;
                a.v.lerp(&b.v, w.as_f64())
            }
        })
    }

    /// Whether the value can differ between two times.
    pub fn varies_with_time(&self) -> bool {
        match self {
            Param::Constant(_) => false,
            Param::Keyframed(k) => k.keyframes.windows(2).any(|w| w[0].v != w[1].v),
        }
    }

    /// Checks every value against the schema and the keys' ordering.
    pub fn check(self, desc: &ParamDesc) -> Result<Param, ParamError> {
        Ok(match self {
            Param::Constant(v) => Param::Constant(desc.check(v)?),
            Param::Keyframed(k) => {
                if !desc.animatable {
                    return Err(ValueError::NotAnimatable.into());
                }
                if k.keyframes.is_empty() {
                    return Err(ParamError::NoKeyframes);
                }
                if k.keyframes.windows(2).any(|w| w[0].t >= w[1].t) {
                    return Err(ParamError::KeyframeOrder);
                }
                let keyframes = k
                    .keyframes
                    .into_iter()
                    .map(|key| {
                        Ok(Keyframe {
                            v: desc.check(key.v)?,
                            ..key
                        })
                    })
                    .collect::<Result<_, ParamError>>()?;
                Param::Keyframed(Keyframes { keyframes })
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParamError {
    #[error(transparent)]
    Value(#[from] ValueError),
    #[error("an animated parameter needs at least one keyframe")]
    NoKeyframes,
    #[error("keyframes must be in time order, with no two at the same time")]
    KeyframeOrder,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fw_plugin::Unit;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    fn key(time: &str, v: f64) -> Keyframe {
        Keyframe {
            t: t(time),
            v: ParamValue::Float(v),
            interp: Interp::Linear,
        }
    }

    #[test]
    fn interpolates_and_clamps_to_ends() {
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![key("1", 0.0), key("3", 2.0)],
        });
        assert_eq!(p.value_at(t("0")).unwrap(), ParamValue::Float(0.0));
        assert_eq!(p.value_at(t("2")).unwrap(), ParamValue::Float(1.0));
        assert_eq!(p.value_at(t("5")).unwrap(), ParamValue::Float(2.0));
        assert!(p.varies_with_time());
    }

    #[test]
    fn hold_keeps_the_left_value() {
        let mut a = key("0", 1.0);
        a.interp = Interp::Hold;
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![a, key("1", 5.0)],
        });
        assert_eq!(p.value_at(t("0.99")).unwrap(), ParamValue::Float(1.0));
    }

    #[test]
    fn equal_keys_do_not_vary() {
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![key("0", 1.0), key("1", 1.0)],
        });
        assert!(!p.varies_with_time());
    }

    #[test]
    fn saved_forms() {
        let constant: Param = serde_json::from_str("0.5").unwrap();
        assert_eq!(constant, Param::Constant(ParamValue::Float(0.5)));
        let animated: Param = serde_json::from_str(
            r#"{"keyframes": [{"t": "0", "v": 0.0}, {"t": "3/2", "v": 1.0, "interp": "hold"}]}"#,
        )
        .unwrap();
        let Param::Keyframed(k) = &animated else {
            panic!()
        };
        assert_eq!(k.keyframes[1].interp, Interp::Hold);
        assert_eq!(
            serde_json::to_string(&animated).unwrap(),
            r#"{"keyframes":[{"t":"0","v":0.0},{"t":"3/2","v":1.0,"interp":"hold"}]}"#
        );
    }

    #[test]
    fn check_rejects_bad_animation() {
        let desc = ParamDesc::float("gain", "Gain", Unit::Ratio, 1.0);
        let unordered = Param::Keyframed(Keyframes {
            keyframes: vec![key("1", 0.0), key("1", 2.0)],
        });
        assert_eq!(unordered.check(&desc), Err(ParamError::KeyframeOrder));
        let fixed = desc.clone().not_animatable();
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![key("0", 1.0)],
        });
        assert!(p.check(&fixed).is_err());
    }
}
