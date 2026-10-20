//! The media files a document uses, as the media panel lists them.

use fw_core::Time;
use fw_doc::Document;
use fw_media::ProjectMedia;
use serde::{Deserialize, Serialize};

/// A media file the document uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaSummary {
    /// As the document refers to it.
    pub path: String,
    /// Empty when the file couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<Picture>,
    pub has_audio: bool,
    /// How long it plays: a video's frames or a sound's length. `None` for
    /// a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Time>,
    /// How many nodes use it. Media imported but unused has none.
    pub users: usize,
    /// Why the file couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Frames, for a video; `None` for a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    /// The frame rate, for a constant-rate video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Time>,
}

pub(crate) fn summarize_media(doc: &Document, media: &ProjectMedia) -> Vec<MediaSummary> {
    let mut summaries: Vec<MediaSummary> = vec![];
    let mut nodes: Vec<_> = doc.nodes().collect();
    // Timeline clips in time order, so the list reads like the edit.
    let placements = doc.timeline(doc.root()).map(|t| t.clips.clone());
    nodes.sort_by_key(|(id, _)| placements.as_ref().and_then(|p| p.get(id).map(|p| p.start)));
    for (_, node) in nodes {
        let Some(known) = node.as_known() else {
            continue;
        };
        for param in known.params.values() {
            let fw_doc::Param::Constant(fw_plugin::ParamValue::Media(reference)) = param else {
                continue;
            };
            if reference.path.is_empty() {
                continue;
            }
            match summaries.iter_mut().find(|s| s.path == reference.path) {
                Some(summary) => summary.users += 1,
                None => summaries.push(summarize(&reference.path, 1, media)),
            }
        }
    }
    // Then what's imported but not used yet.
    for path in doc.media_pool() {
        if !summaries.iter().any(|s| s.path == path) {
            summaries.push(summarize(path, 0, media));
        }
    }
    summaries
}

fn summarize(path: &str, users: usize, media: &ProjectMedia) -> MediaSummary {
    use fw_compile::{FrameTimes, MediaResolver};
    let reference = fw_plugin::MediaRef { path: path.into() };
    let (picture, has_audio, duration, error) = match media.probe(&reference) {
        Ok(info) => {
            let picture = (info.width > 0).then(|| Picture {
                width: info.width,
                height: info.height,
                frames: info.frames.as_ref().map(FrameTimes::count),
                rate: match info.frames {
                    Some(FrameTimes::Constant { rate, .. }) => Some(rate),
                    _ => None,
                },
            });
            let duration = match &info.frames {
                Some(frames) => frames.duration(),
                None if info.has_audio => fw_media::audio_duration(&info.path).ok().flatten(),
                None => None,
            };
            (picture, info.has_audio, duration, None)
        }
        Err(e) => (None, false, None, Some(e)),
    };
    MediaSummary {
        path: path.into(),
        picture,
        has_audio,
        duration,
        users,
        error,
    }
}
