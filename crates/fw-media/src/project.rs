//! Media access for one project: paths resolve against the project's folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use fw_compile::{Decoder, LoadStep, MediaInfo, MediaResolver};
use fw_core::RgbaImage;
use fw_plugin::MediaRef;

use crate::{Format, VideoInfo, VideoReader, has_audio, probe_video};

pub struct ProjectMedia {
    base: PathBuf,
    /// Open video readers by file, so the next frame continues decoding
    /// instead of reopening and seeking.
    /// TODO: bound how many stay open.
    readers: Mutex<HashMap<PathBuf, VideoReader>>,
    /// What each file was found to hold, so compiling a frame never reads a
    /// file again. Entries are trusted while size and modification time match.
    /// TODO: keep this on disk alongside the cache (design §4.3).
    probes: Mutex<HashMap<PathBuf, Probed>>,
    /// How many times a file was actually read to probe it. For tests.
    probe_reads: AtomicUsize,
}

#[derive(Clone)]
struct Probed {
    size: u64,
    modified: SystemTime,
    info: MediaInfo,
    video: Option<VideoInfo>,
}

impl ProjectMedia {
    /// `base` is the folder relative media paths are resolved against,
    /// normally the one holding the project file.
    pub fn new(base: impl Into<PathBuf>) -> ProjectMedia {
        ProjectMedia {
            base: base.into(),
            readers: Mutex::new(HashMap::new()),
            probes: Mutex::new(HashMap::new()),
            probe_reads: AtomicUsize::new(0),
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

impl ProjectMedia {
    /// The probe for `path`, from the cache when the file hasn't changed.
    fn probed(&self, path: &Path) -> Result<Probed, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (size, modified) = (meta.len(), meta.modified().map_err(|e| e.to_string())?);
        let mut probes = self.probes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = probes.get(path)
            && p.size == size
            && p.modified == modified
        {
            return Ok(p.clone());
        }
        self.probe_reads.fetch_add(1, Ordering::Relaxed);
        let probed = if Format::of(path) == Some(Format::Video) {
            let fingerprint = crate::fingerprint(path).map_err(|e| e.to_string())?;
            let video = probe_video(path).map_err(|e| e.to_string())?;
            let info = MediaInfo {
                path: path.to_owned(),
                width: video.width,
                height: video.height,
                space: video.space,
                fingerprint,
                frames: Some(video.frames.clone()),
                opaque: !video.has_alpha,
                has_audio: has_audio(path).map_err(|e| e.to_string())?,
                origin: Some(video.origin()),
            };
            Probed {
                size,
                modified,
                info,
                video: Some(video),
            }
        } else if Format::of(path) == Some(Format::Audio) {
            let info = MediaInfo {
                path: path.to_owned(),
                width: 0,
                height: 0,
                space: fw_core::ColorSpace::REC709_DISPLAY,
                fingerprint: crate::fingerprint(path).map_err(|e| e.to_string())?,
                frames: None,
                opaque: false,
                has_audio: has_audio(path).map_err(|e| e.to_string())?,
                origin: None,
            };
            Probed {
                size,
                modified,
                info,
                video: None,
            }
        } else {
            let probe = crate::probe(path).map_err(|e| e.to_string())?;
            let info = MediaInfo {
                path: path.to_owned(),
                width: probe.width,
                height: probe.height,
                space: probe.space,
                fingerprint: probe.fingerprint,
                frames: None,
                opaque: !probe.has_alpha,
                has_audio: false,
                origin: None,
            };
            Probed {
                size,
                modified,
                info,
                video: None,
            }
        };
        probes.insert(path.to_owned(), probed.clone());
        // A changed file invalidates any reader opened on the old one.
        self.readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
        Ok(probed)
    }

    /// How many times a file has actually been read to probe it.
    pub fn probe_reads(&self) -> usize {
        self.probe_reads.load(Ordering::Relaxed)
    }
}

impl MediaResolver for ProjectMedia {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String> {
        Ok(self.probed(&self.resolve(media))?.info)
    }
}

impl Decoder for ProjectMedia {
    fn decode(&self, load: &LoadStep) -> Result<RgbaImage, String> {
        let Some(frame) = load.frame else {
            return crate::read_image(&load.path)
                .map(|(image, _)| image)
                .map_err(|e| e.to_string());
        };
        let video = self
            .probed(&load.path)?
            .video
            .ok_or_else(|| format!("{} is not a video", load.path.display()))?;
        let mut readers = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if !readers.contains_key(&load.path) {
            let reader = VideoReader::open_probed(&load.path, &video).map_err(|e| e.to_string())?;
            readers.insert(load.path.clone(), reader);
        }
        let reader = readers.get_mut(&load.path).expect("just inserted");
        reader.frame(frame).map_err(|e| e.to_string())
    }
}
