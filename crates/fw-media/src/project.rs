//! Media access for one project: paths resolve against the project's folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fw_compile::{Decoder, LoadStep, MediaInfo, MediaResolver};
use fw_core::RgbaImage;
use fw_plugin::MediaRef;

use crate::{Format, VideoReader, probe_video};

pub struct ProjectMedia {
    base: PathBuf,
    /// Open video readers by file, so the next frame continues decoding
    /// instead of reopening and seeking.
    /// TODO: bound how many stay open, and remember probes by fingerprint.
    readers: Mutex<HashMap<PathBuf, VideoReader>>,
}

impl ProjectMedia {
    /// `base` is the folder relative media paths are resolved against,
    /// normally the one holding the project file.
    pub fn new(base: impl Into<PathBuf>) -> ProjectMedia {
        ProjectMedia {
            base: base.into(),
            readers: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, media: &MediaRef) -> PathBuf {
        let path = Path::new(&media.path);
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.base.join(path)
        }
    }
}

impl MediaResolver for ProjectMedia {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String> {
        let path = self.resolve(media);
        if Format::of(&path) == Some(Format::Video) {
            let fingerprint = crate::fingerprint(&path).map_err(|e| e.to_string())?;
            let video = probe_video(&path).map_err(|e| e.to_string())?;
            return Ok(MediaInfo {
                path,
                width: video.width,
                height: video.height,
                space: video.space,
                fingerprint,
                frames: Some(video.frames),
                opaque: !video.has_alpha,
            });
        }
        let probe = crate::probe(&path).map_err(|e| e.to_string())?;
        Ok(MediaInfo {
            path,
            width: probe.width,
            height: probe.height,
            space: probe.space,
            fingerprint: probe.fingerprint,
            frames: None,
            opaque: !probe.has_alpha,
        })
    }
}

impl Decoder for ProjectMedia {
    fn decode(&self, load: &LoadStep) -> Result<RgbaImage, String> {
        let Some(frame) = load.frame else {
            return crate::read_image(&load.path)
                .map(|(image, _)| image)
                .map_err(|e| e.to_string());
        };
        let mut readers = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if !readers.contains_key(&load.path) {
            let reader = VideoReader::open(&load.path).map_err(|e| e.to_string())?;
            readers.insert(load.path.clone(), reader);
        }
        let reader = readers.get_mut(&load.path).expect("just inserted");
        reader.frame(frame).map_err(|e| e.to_string())
    }
}
