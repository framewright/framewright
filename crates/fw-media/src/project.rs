//! Media access for one project: paths resolve against the project's folder.

use std::path::{Path, PathBuf};

use fw_compile::{Decoder, LoadStep, MediaInfo, MediaResolver};
use fw_core::RgbaImage;
use fw_plugin::MediaRef;

pub struct ProjectMedia {
    base: PathBuf,
}

impl ProjectMedia {
    /// `base` is the folder relative media paths are resolved against,
    /// normally the one holding the project file.
    pub fn new(base: impl Into<PathBuf>) -> ProjectMedia {
        ProjectMedia { base: base.into() }
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
        let probe = crate::probe(&path).map_err(|e| e.to_string())?;
        Ok(MediaInfo {
            path,
            width: probe.width,
            height: probe.height,
            space: probe.space,
            fingerprint: probe.fingerprint,
            // TODO: video, in step 2's next check-in.
            frames: None,
        })
    }
}

impl Decoder for ProjectMedia {
    fn decode(&self, load: &LoadStep) -> Result<RgbaImage, String> {
        crate::read_image(&load.path)
            .map(|(image, _)| image)
            .map_err(|e| e.to_string())
    }
}
