//! The audio schedule: which clips sound when, read from the same document
//! as the video plan (design §8: one graph, two schedules).

use std::path::PathBuf;
use std::sync::Arc;

use fw_core::{NodeId, Time};
use fw_doc::{Document, KeyTime, Node, TrackKind};
use fw_plugin::{AudioBehaviour, MediaRef, ParamValue, Registry};

use crate::{CompileError, MediaResolver};

/// Everything the audio engine plays, in timeline samples.
#[derive(Clone)]
pub struct AudioPlan {
    pub sample_rate: u32,
    pub voices: Vec<Voice>,
}

/// One clip's sound.
#[derive(Clone)]
pub struct Voice {
    pub clip: NodeId,
    pub path: PathBuf,
    /// Source time 0 on the file's clock; see [`crate::MediaInfo::origin`].
    pub origin: Option<Time>,
    /// The timeline samples it plays, `[start, end)`.
    pub start: i64,
    pub end: i64,
    /// The source sample heard at `start`.
    pub source_start: i64,
    pub behaviour: Arc<dyn AudioBehaviour>,
    /// Values for `behaviour.params()`, in order.
    /// TODO: automation, resolved per block (design §4.4). Constant for now.
    pub params: Vec<f32>,
}

impl std::fmt::Debug for Voice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Voice")
            .field("clip", &self.clip)
            .field("path", &self.path)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("source_start", &self.source_start)
            .field("params", &self.params)
            .finish()
    }
}

/// The audio schedule of a document's root. Graph documents are silent.
///
/// TODO: track volume, mute and solo; buses and sends (design §8).
pub fn compile_audio(
    doc: &Document,
    registry: &Registry,
    media: &dyn MediaResolver,
) -> Result<AudioPlan, CompileError> {
    let Some(timeline) = doc.timeline(doc.root()) else {
        return Ok(AudioPlan {
            sample_rate: 48000,
            voices: vec![],
        });
    };
    let rate = timeline.settings.sample_rate;
    let fail = |clip: NodeId, message: String| CompileError::Node {
        node: clip,
        name: doc
            .node(clip)
            .map(|n| n.name().to_owned())
            .unwrap_or_default(),
        message,
    };
    let to_samples =
        |clip: NodeId, t: Time| t.sample_index(rate).map_err(|e| fail(clip, e.to_string()));
    let mut voices = vec![];
    for (clip, placement) in &timeline.clips {
        let kind = timeline.track(placement.track).map(|t| t.kind);
        if kind != Some(TrackKind::Audio) {
            continue;
        }
        let Some(Node::Known(known)) = doc.node(*clip) else {
            return Err(fail(*clip, "this clip's type isn't available".into()));
        };
        let ty = registry.get(&known.type_id).ok_or_else(|| {
            fail(
                *clip,
                format!("node type {} isn't registered", known.type_id),
            )
        })?;
        let behaviour = ty
            .audio
            .clone()
            .ok_or_else(|| fail(*clip, format!("{} makes no sound", ty.desc.name)))?;
        if placement.speed != Time::from_seconds(1) {
            return Err(fail(*clip, "retimed audio isn't supported yet".into()));
        }
        let file = match known.params.get("file") {
            Some(p) => p
                .value_at(KeyTime::at(Time::ZERO))
                .map_err(|e| fail(*clip, e.to_string()))?,
            None => return Err(fail(*clip, "no media".into())),
        };
        let ParamValue::Media(reference) = file else {
            return Err(fail(*clip, "no media".into()));
        };
        let found = media
            .probe(&MediaRef {
                path: reference.path.clone(),
            })
            .map_err(|e| fail(*clip, e))?;
        if !found.has_audio {
            return Err(fail(*clip, format!("{} has no sound", reference.path)));
        }
        let at_start = KeyTime {
            local: Time::ZERO,
            length: Some(placement.duration),
        };
        let mut params = vec![];
        for id in behaviour.params() {
            let desc = ty
                .desc
                .param(&id)
                .ok_or_else(|| fail(*clip, format!("audio reads unknown parameter {id:?}")))?;
            let value = match known.params.get(&id) {
                Some(p) => p
                    .value_at(at_start)
                    .map_err(|e| fail(*clip, e.to_string()))?,
                None => desc.default.clone(),
            };
            params.push(value.as_f64().unwrap_or(0.0) as f32);
        }
        voices.push(Voice {
            clip: *clip,
            path: found.path,
            origin: found.origin,
            start: to_samples(*clip, placement.start)?,
            end: to_samples(*clip, placement.end())?,
            source_start: to_samples(*clip, placement.source_in)?,
            behaviour,
            params,
        });
    }
    voices.sort_by_key(|v| (v.start, v.clip));
    Ok(AudioPlan {
        sample_rate: rate,
        voices,
    })
}
