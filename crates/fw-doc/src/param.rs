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
    /// Keys sharing an anchor are in time order, and no two keys share both
    /// anchor and time. Keys with different anchors may interleave.
    pub keyframes: Vec<Keyframe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    /// Seconds from the anchor point. Negative before it.
    pub t: Time,
    pub v: ParamValue,
    /// How to get from this key to the next.
    #[serde(default, skip_serializing_if = "Interp::is_linear")]
    pub interp: Interp,
    #[serde(default, skip_serializing_if = "Anchor::is_start")]
    pub anchor: Anchor,
}

/// What a key's time is measured from, so trimming and retiming a clip move
/// keys predictably: an end-anchored fade-out stays at the end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// The clip's start (or time zero outside any clip).
    #[default]
    Start,
    /// The clip's end.
    End,
    /// A fraction of the clip's length, e.g. `1/2` for the middle.
    Fraction(Time),
}

impl Anchor {
    fn is_start(&self) -> bool {
        *self == Anchor::Start
    }
}

/// The moment parameters are resolved at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyTime {
    /// Time since the start of the enclosing clip, or document time outside clips.
    pub local: Time,
    /// The enclosing clip's length, which end and fraction anchors need.
    pub length: Option<Time>,
}

impl KeyTime {
    /// A time outside any clip, where only start anchors make sense.
    pub fn at(local: Time) -> KeyTime {
        KeyTime {
            local,
            length: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EvalError {
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error("keys anchored to a clip's end or length only work inside a clip")]
    NeedsClip,
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
    /// The plain value at `at`.
    pub fn value_at(&self, at: KeyTime) -> Result<ParamValue, EvalError> {
        let keys = match self {
            Param::Constant(v) => return Ok(v.clone()),
            Param::Keyframed(k) => &k.keyframes,
        };
        // Where each key lands for this clip length, in time order.
        let mut placed = Vec::with_capacity(keys.len());
        for key in keys {
            let when = match key.anchor {
                Anchor::Start => key.t,
                Anchor::End => at.length.ok_or(EvalError::NeedsClip)?.checked_add(key.t)?,
                Anchor::Fraction(f) => at
                    .length
                    .ok_or(EvalError::NeedsClip)?
                    .checked_mul(f)?
                    .checked_add(key.t)?,
            };
            placed.push((when, key));
        }
        placed.sort_by_key(|(when, _)| *when);
        let t = at.local;
        let (first, last) = match (placed.first(), placed.last()) {
            (Some(first), Some(last)) => (first, last),
            _ => unreachable!("checked parameters always have at least one key"),
        };
        if t <= first.0 {
            return Ok(first.1.v.clone());
        }
        if t >= last.0 {
            return Ok(last.1.v.clone());
        }
        let next = placed.partition_point(|(when, _)| *when <= t);
        let ((ta, a), (tb, b)) = (placed[next - 1], placed[next]);
        Ok(match a.interp {
            Interp::Hold => a.v.clone(),
            Interp::Linear => {
                let w = t.checked_sub(ta)?.checked_div(tb.checked_sub(ta)?)?;
                a.v.lerp(&b.v, w.as_f64())
            }
        })
    }

    /// The two halves of a clip's parameter when the clip, `length` long, is
    /// split `at` from its start. Each half is keyed so it shows the same
    /// values it did before: start-anchored keys move with the second half's
    /// new start, and end-anchored keys stay with the end they were measured
    /// from. Keys outside a half stay, since they shape the curve inside it.
    pub fn split(&self, at: Time, length: Time) -> Result<(Param, Param), TimeError> {
        let Param::Keyframed(k) = self else {
            return Ok((self.clone(), self.clone()));
        };
        let mut first = vec![];
        let mut second = vec![];
        for key in &k.keyframes {
            let when = match key.anchor {
                Anchor::Start => key.t,
                Anchor::End => length.checked_add(key.t)?,
                Anchor::Fraction(f) => length.checked_mul(f)?.checked_add(key.t)?,
            };
            let start_at = |t| Keyframe {
                t,
                anchor: Anchor::Start,
                ..key.clone()
            };
            first.push(match key.anchor {
                Anchor::Start => key.clone(),
                _ => start_at(when),
            });
            second.push(match key.anchor {
                Anchor::End => key.clone(),
                _ => start_at(when.checked_sub(at)?),
            });
        }
        Ok((Param::keyed(first), Param::keyed(second)))
    }

    /// Keys in order, keeping the last where two land on the same anchor
    /// and time.
    fn keyed(mut keys: Vec<Keyframe>) -> Param {
        keys.sort_by_key(|k| (k.anchor != Anchor::Start, k.t));
        let mut kept: Vec<Keyframe> = vec![];
        for key in keys {
            match kept.last_mut() {
                Some(last) if last.anchor == key.anchor && last.t == key.t => *last = key,
                _ => kept.push(key),
            }
        }
        Param::Keyframed(Keyframes { keyframes: kept })
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
                for (i, a) in k.keyframes.iter().enumerate() {
                    let later_same_anchor =
                        k.keyframes[i + 1..].iter().filter(|b| b.anchor == a.anchor);
                    if later_same_anchor.into_iter().any(|b| b.t <= a.t) {
                        return Err(ParamError::KeyframeOrder);
                    }
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
    #[error("keyframes with the same anchor must be in time order, with no two at the same time")]
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
            anchor: Anchor::Start,
        }
    }

    #[test]
    fn interpolates_and_clamps_to_ends() {
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![key("1", 0.0), key("3", 2.0)],
        });
        assert_eq!(
            p.value_at(KeyTime::at(t("0"))).unwrap(),
            ParamValue::Float(0.0)
        );
        assert_eq!(
            p.value_at(KeyTime::at(t("2"))).unwrap(),
            ParamValue::Float(1.0)
        );
        assert_eq!(
            p.value_at(KeyTime::at(t("5"))).unwrap(),
            ParamValue::Float(2.0)
        );
        assert!(p.varies_with_time());
    }

    #[test]
    fn hold_keeps_the_left_value() {
        let mut a = key("0", 1.0);
        a.interp = Interp::Hold;
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![a, key("1", 5.0)],
        });
        assert_eq!(
            p.value_at(KeyTime::at(t("0.99"))).unwrap(),
            ParamValue::Float(1.0)
        );
    }

    #[test]
    fn anchors_follow_the_clip() {
        // A fade-out over the last second, whatever the clip's length.
        let mut fade = key("-1", 1.0);
        fade.anchor = Anchor::End;
        let mut out = key("0", 0.0);
        out.anchor = Anchor::End;
        let mut mid = key("0", 0.5);
        mid.anchor = Anchor::Fraction(t("1/2"));
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![fade, out],
        });
        let clip = |local: &str, length: &str| KeyTime {
            local: t(local),
            length: Some(t(length)),
        };
        assert_eq!(
            p.value_at(clip("9.5", "10")).unwrap(),
            ParamValue::Float(0.5)
        );
        assert_eq!(
            p.value_at(clip("4.5", "5")).unwrap(),
            ParamValue::Float(0.5)
        );
        assert_eq!(p.value_at(KeyTime::at(t("1"))), Err(EvalError::NeedsClip));

        // Keys from different anchors interleave by where they land.
        let p = Param::Keyframed(Keyframes {
            keyframes: vec![key("0", 0.0), mid],
        });
        assert_eq!(p.value_at(clip("2", "8")).unwrap(), ParamValue::Float(0.25));
    }

    #[test]
    fn anchored_keys_serialize_compactly() {
        let mut k = key("-1", 1.0);
        k.anchor = Anchor::End;
        assert_eq!(
            serde_json::to_string(&k).unwrap(),
            r#"{"t":"-1","v":1.0,"anchor":"end"}"#
        );
        k.anchor = Anchor::Fraction(t("1/2"));
        assert_eq!(
            serde_json::to_string(&k).unwrap(),
            r#"{"t":"-1","v":1.0,"anchor":{"fraction":"1/2"}}"#
        );
        let back: Keyframe =
            serde_json::from_str(r#"{"t":"-1","v":1.0,"anchor":{"fraction":"1/2"}}"#).unwrap();
        assert_eq!(back, k);
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
