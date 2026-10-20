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
    /// How many nodes use it.
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

pub(crate) fn summarise_media(doc: &Document, media: &ProjectMedia) -> Vec<MediaSummary> {
    use fw_compile::{FrameTimes, MediaResolver};
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
            if let Some(summary) = summaries.iter_mut().find(|s| s.path == reference.path) {
                summary.users += 1;
                continue;
            }
            let (picture, has_audio, error) = match media.probe(reference) {
                Ok(info) => {
                    let picture = Picture {
                        width: info.width,
                        height: info.height,
                        frames: info.frames.as_ref().map(FrameTimes::count),
                        rate: match info.frames {
                            Some(FrameTimes::Constant { rate, .. }) => Some(rate),
                            _ => None,
                        },
                    };
                    (Some(picture), info.has_audio, None)
                }
                Err(e) => (None, false, Some(e)),
            };
            summaries.push(MediaSummary {
                path: reference.path.clone(),
                picture,
                has_audio,
                users: 1,
                error,
            });
        }
    }
    summaries
}
