//! Media access for one project: paths resolve against the project's folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use fw_compile::{Decoded, Decoder, LoadStep, MediaInfo, MediaResolver};
use fw_core::{DecodedImage, NodeId};
use fw_plugin::MediaRef;

use crate::copies::{CopyProfile, EditingCopies};
use crate::hardware::HardwareDevice;
use crate::{Format, VideoInfo, VideoReader, has_audio, probe_video};

pub struct ProjectMedia {
    base: PathBuf,
    /// Open video readers by file, so the next frame continues decoding
    /// instead of reopening and seeking. A file used at several places at
    /// once (the same clip on two tracks) gets a reader for each.
    /// TODO: bound how many files stay open.
    readers: Mutex<HashMap<PathBuf, Vec<Reader>>>,
    /// What each file was found to hold, so compiling a frame never reads a
    /// file again. Entries are trusted while size and modification time match.
    /// TODO: keep this on disk alongside the cache (design §4.3).
    probes: Mutex<HashMap<PathBuf, Probed>>,
    /// How many times a file was actually read to probe it. For tests.
    probe_reads: AtomicUsize,
    /// Intra-frame copies that stand in for video at draft quality.
    copies: Option<EditingCopies>,
    /// Open readers on copy chunks, most recently used last.
    copy_readers: Mutex<Vec<CopyReader>>,
    /// The engine's device, when video is decoded on it (design §12).
    hardware: Option<Arc<HardwareDevice>>,
    /// Files whose hardware decoding failed: they decode in software.
    software_only: Mutex<std::collections::HashSet<PathBuf>>,
    /// Pictures decoded in hardware. For tests.
    hardware_frames: AtomicUsize,
}

/// Copy chunk readers kept open, at most. Opening one costs about 4 ms.
const COPY_READERS: usize = 8;

struct CopyReader {
    chunk: PathBuf,
    info: VideoInfo,
    reader: VideoReader,
}

/// Readers kept open per file, at most.
const READERS_PER_FILE: usize = 4;

struct Reader {
    reader: VideoReader,
    /// The node it last decoded for.
    node: Option<NodeId>,
    /// When it was last used, for replacing the least recently used.
    used: u64,
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
            copies: None,
            copy_readers: Mutex::new(Vec::new()),
            hardware: None,
            software_only: Mutex::new(Default::default()),
            hardware_frames: AtomicUsize::new(0),
        }
    }

    /// Decodes video on the engine's GPU where it can, from now on.
    pub fn use_hardware(&mut self, device: fw_compile::VulkanVideoDevice) -> Result<(), String> {
        self.hardware = Some(HardwareDevice::new(device)?);
        self.readers
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }

    /// How many pictures were decoded in hardware.
    pub fn hardware_frames(&self) -> usize {
        self.hardware_frames.load(Ordering::Relaxed)
    }

    /// Also makes editing copies (design §12) under `root`, in the
    /// background, and reads them for draft-quality frames once ready.
    pub fn with_copies(
        base: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        profile: CopyProfile,
    ) -> ProjectMedia {
        ProjectMedia {
            copies: Some(EditingCopies::new(root, profile)),
            ..ProjectMedia::new(base)
        }
    }

    /// The usual place for a project's editing copies: its cache folder.
    pub fn copy_root(base: &Path) -> PathBuf {
        base.join(".framewright").join("cache").join("copies")
    }

    pub fn copies(&self) -> Option<&EditingCopies> {
        self.copies.as_ref()
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
        let probed = self.probed(&self.resolve(media))?;
        if let (Some(copies), Some(video)) = (&self.copies, &probed.video) {
            copies.request(&probed.info.path, probed.info.fingerprint, video);
        }
        Ok(probed.info)
    }

    fn editing_copy(&self, media: &MediaInfo, frame: fw_core::Time) -> Option<&'static str> {
        let copies = self.copies.as_ref()?;
        let index = media.frames.as_ref()?.index_at(frame)?;
        copies
            .locate(&media.fingerprint, index)
            .map(|_| copies.profile().name)
    }
}

impl ProjectMedia {
    /// The frame starting at `frame`, from the source's editing copy.
    fn decode_copy(
        &self,
        load: &LoadStep,
        video: &VideoInfo,
        frame: fw_core::Time,
    ) -> Result<Decoded, String> {
        let copies = self.copies.as_ref().ok_or("editing copies are off")?;
        let index = video
            .frames
            .index_at(frame)
            .ok_or("the video has no frames")?;
        let (chunk, within) = copies
            .locate(&load.fingerprint, index)
            .ok_or_else(|| format!("no editing copy of {}", load.path.display()))?;
        let k = index / copies.profile().chunk_frames;
        let result = self.read_chunk(&chunk, within);
        if result.is_err() {
            // Missing or damaged: drop it and fall back to the original
            // until it's made again.
            self.copy_readers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|r| r.chunk != chunk);
            copies.invalidate(&load.fingerprint, k);
        }
        result
    }

    /// Frame `within` of a copy chunk, through the open readers.
    fn read_chunk(&self, chunk: &Path, within: u64) -> Result<Decoded, String> {
        let mut readers = self.copy_readers.lock().unwrap_or_else(|e| e.into_inner());
        let at = match readers.iter().position(|r| r.chunk == chunk) {
            Some(i) => {
                let r = readers.remove(i);
                readers.push(r);
                readers.len() - 1
            }
            None => {
                let info = probe_video(chunk).map_err(|e| e.to_string())?;
                let reader = VideoReader::open_intra(chunk, &info).map_err(|e| e.to_string())?;
                if readers.len() == COPY_READERS {
                    readers.remove(0);
                }
                readers.push(CopyReader {
                    chunk: chunk.to_owned(),
                    info,
                    reader,
                });
                readers.len() - 1
            }
        };
        let r = &mut readers[at];
        let time = r
            .info
            .frames
            .time_of(within)
            .ok_or("the copy chunk is short")?;
        r.reader
            .frame(time)
            .map(|p| Decoded::Cpu(DecodedImage::Planar(p)))
            .map_err(|e| e.to_string())
    }
}

impl Decoder for ProjectMedia {
    fn decode(&self, load: &LoadStep) -> Result<Decoded, String> {
        let Some(frame) = load.frame else {
            return crate::read_image(&load.path)
                .map(|(image, _)| Decoded::Cpu(DecodedImage::Rgba(image)))
                .map_err(|e| e.to_string());
        };
        let video = self
            .probed(&load.path)?
            .video
            .ok_or_else(|| format!("{} is not a video", load.path.display()))?;
        if load.copy.is_some() {
            return self.decode_copy(load, &video, frame);
        }
        let hardware = self.hardware.as_ref().filter(|_| {
            !self
                .software_only
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&load.path)
        });
        match self.decode_video(load, &video, frame, hardware) {
            Ok(decoded) => {
                if matches!(decoded, Decoded::Hardware(_)) {
                    self.hardware_frames.fetch_add(1, Ordering::Relaxed);
                }
                Ok(decoded)
            }
            // Hardware decoding failed: this file decodes in software from now on.
            Err(_) if hardware.is_some() => {
                self.software_only
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(load.path.clone());
                self.readers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&load.path);
                self.decode_video(load, &video, frame, None)
            }
            Err(e) => Err(e),
        }
    }
}

impl ProjectMedia {
    /// The frame starting at `frame` from the original, through the open
    /// readers, decoding on `hardware` if given.
    fn decode_video(
        &self,
        load: &LoadStep,
        video: &VideoInfo,
        frame: fw_core::Time,
        hardware: Option<&Arc<HardwareDevice>>,
    ) -> Result<Decoded, String> {
        let mut readers = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        let readers = readers.entry(load.path.clone()).or_default();
        let used = readers.iter().map(|r| r.used).max().unwrap_or(0) + 1;
        // The reader that gets there by decoding least; failing that the
        // node's own reader, which will seek; failing that a new one, or the
        // least recently used one.
        let nearest = readers
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.reader.distance_to(frame).map(|d| (d, i)))
            .min()
            .map(|(_, i)| i);
        let own = readers
            .iter()
            .position(|r| load.node.is_some() && r.node == load.node);
        let index = match nearest.or(own) {
            Some(i) => i,
            None if readers.len() < READERS_PER_FILE => {
                let reader = match hardware {
                    Some(device) => VideoReader::open_hardware(&load.path, video, device.clone()),
                    None => VideoReader::open_probed(&load.path, video),
                }
                .map_err(|e| e.to_string())?;
                readers.push(Reader {
                    reader,
                    node: load.node,
                    used,
                });
                readers.len() - 1
            }
            None => {
                let (i, _) = readers
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, r)| r.used)
                    .expect("readers is full");
                i
            }
        };
        let reader = &mut readers[index];
        reader.used = used;
        reader.node = load.node;
        reader
            .reader
            .frame_decoded(frame)
            .map_err(|e| e.to_string())
    }
}
