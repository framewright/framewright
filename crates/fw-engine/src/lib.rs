//! A project open for editing: the engine as one object (design §10.1).
//!
//! Clients drive it with [`Request`]s and hear back through [`Event`]s, all
//! plain serialisable data, so the UI, scripts and tests drive it the same
//! way. Calls return quickly: rendering, decoding and making editing copies
//! happen on the engine's own threads.
//!
//! The one exchange that isn't a message is [`Engine::viewer_frame`], the
//! frame to show as a texture on the engine's GPU, because showing it must
//! not copy.
//!
//! TODO: playing with sound, the command journal, serving requests over a
//! socket (`fw-script`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fw_audio::FakeClock;
use fw_compile::Options;
use fw_core::Time;
use fw_doc::{Command, CommandError, Document, Editor};
use fw_media::{HardwarePolicy, ProjectMedia};
use fw_playback::{DocumentFrames, FrameSource, Player};
use fw_plugin::{Quality, Registry};
use fw_render::{DEFAULT_CACHE_BYTES, Gpu, GpuImage, Renderer};
use serde::{Deserialize, Serialize};

/// What clients ask the engine to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "request")]
pub enum Request {
    /// Applies `commands` as one undoable step. Edits carrying the same
    /// `gesture`, one straight after another, share a step: a slider drag.
    Edit {
        label: String,
        commands: Vec<Command>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gesture: Option<u64>,
    },
    Undo,
    Redo,
    /// Moves the playhead.
    Seek {
        time: Time,
    },
    /// Writes the document to its file.
    Save,
}

/// What the engine tells clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    /// The document changed: an edit, an undo or a redo.
    Document(fw_doc::Event),
    Saved {
        path: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Open(#[from] fw_doc::save::OpenError),
    #[error(transparent)]
    Edit(#[from] CommandError),
    #[error("cannot save {path}: {source}")]
    Save {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// How an engine reads media.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Make editing copies of videos in the background, and scrub from them.
    pub editing_copies: bool,
    /// Decode in hardware when the GPU can, as the policy says; `None` never.
    pub hardware: Option<HardwarePolicy>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            editing_copies: true,
            hardware: Some(HardwarePolicy::Fastest),
        }
    }
}

/// The frame to show.
pub struct ViewerFrame<'a> {
    pub index: i64,
    /// When the frame starts on the timeline.
    pub time: Time,
    pub image: &'a GpuImage,
    /// Changes whenever the image does.
    pub serial: u64,
}

pub struct Engine {
    path: PathBuf,
    editor: Editor,
    /// The document as playback should render it, replaced on every change.
    latest: Arc<Mutex<Document>>,
    rate: Time,
    /// Where the playhead is. TODO: the sample clock while playing.
    clock: Arc<FakeClock>,
    player: Player<GpuImage>,
    serial: u64,
    saved_revision: u64,
    events: Vec<Event>,
    gpu: Arc<Gpu>,
    warnings: Vec<String>,
}

impl Engine {
    /// Opens the project file at `path`, rendering on `gpu`.
    pub fn open(path: &Path, gpu: Arc<Gpu>, settings: Settings) -> Result<Engine, EngineError> {
        let registry = Arc::new(fw_ops::builtin_registry());
        let doc = fw_doc::save::load(path, &registry)?;
        let folder = path.parent().unwrap_or(Path::new("."));
        let mut warnings = vec![];
        let mut media = if settings.editing_copies {
            ProjectMedia::with_copies(
                folder,
                ProjectMedia::copy_root(folder),
                fw_media::PRORES_PROXY_1080,
            )
        } else {
            ProjectMedia::new(folder)
        };
        if let Some(policy) = settings.hardware
            && let Some(device) = gpu.video_device()
            && let Err(e) = media.use_hardware(device, policy)
        {
            warnings.push(format!("decoding in software: {e}"));
        }
        let rate = doc
            .timeline(doc.root())
            .map_or(Time::from_seconds(24), |t| t.settings.rate);
        let latest = Arc::new(Mutex::new(doc.clone()));
        let mut options = Options::at(Time::ZERO);
        options.quality = Quality::Draft;
        let frames = Edited {
            latest: latest.clone(),
            frames: DocumentFrames {
                doc: doc.clone(),
                registry: registry.clone(),
                media,
                renderer: Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES),
                rate,
                options,
            },
        };
        let clock = Arc::new(FakeClock::new(Time::ZERO));
        let mut player = Player::new(Box::new(frames), clock.clone(), rate, 6);
        player.set_playing(false);
        Ok(Engine {
            path: path.to_owned(),
            editor: Editor::new(doc, registry),
            latest,
            rate,
            clock,
            player,
            serial: 0,
            saved_revision: 0,
            events: vec![],
            gpu,
            warnings,
        })
    }

    /// Does what `request` asks. What changed arrives through
    /// [`Engine::take_events`].
    pub fn request(&mut self, request: Request) -> Result<(), EngineError> {
        match request {
            Request::Edit {
                label,
                commands,
                gesture,
            } => {
                let event = self.editor.apply_in_gesture(label, commands, gesture)?;
                self.changed(event);
            }
            Request::Undo => {
                if let Some(event) = self.editor.undo()? {
                    self.changed(event);
                }
            }
            Request::Redo => {
                if let Some(event) = self.editor.redo()? {
                    self.changed(event);
                }
            }
            Request::Seek { time } => {
                let end = self.duration();
                let time = if time < Time::ZERO {
                    Time::ZERO
                } else if end > Time::ZERO && time > end {
                    end
                } else {
                    time
                };
                self.clock.set(time);
            }
            Request::Save => {
                fw_doc::save::save(self.editor.doc(), &self.path).map_err(|source| {
                    EngineError::Save {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                self.saved_revision = self.editor.revision();
                self.events.push(Event::Saved {
                    path: self.path.clone(),
                });
            }
        }
        Ok(())
    }

    /// Events since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Problems met while opening that didn't stop it, such as falling back
    /// to software decoding.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// The frame to show now. Call once per display refresh.
    pub fn viewer_frame(&mut self) -> Option<ViewerFrame<'_>> {
        if self.player.tick().is_some() {
            self.serial += 1;
        }
        let serial = self.serial;
        self.player.shown().map(|shown| ViewerFrame {
            index: shown.index,
            time: shown.time,
            image: &shown.image,
            serial,
        })
    }

    /// Whether the frame shown is the one due, rendered from the current
    /// document. Until it is, keep asking for [`Engine::viewer_frame`].
    pub fn up_to_date(&self) -> bool {
        self.player.up_to_date()
    }

    /// Why the frame due couldn't be rendered, if it couldn't. The viewer
    /// keeps the last frame that rendered.
    pub fn failure(&self) -> Option<&str> {
        self.player.failure()
    }

    /// Asks for frames until the one due is shown or `timeout` passes. For
    /// tests and the screenshot mode.
    pub fn wait_for_frame(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            self.viewer_frame();
            if self.up_to_date() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    pub fn document(&self) -> &Document {
        self.editor.doc()
    }

    pub fn registry(&self) -> &Arc<Registry> {
        self.editor.registry()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.editor.undo_label()
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.editor.redo_label()
    }

    /// Whether there are changes since the file was opened or saved.
    pub fn is_modified(&self) -> bool {
        self.editor.revision() != self.saved_revision
    }

    /// Frames per second of the timeline (24 for a graph).
    pub fn frame_rate(&self) -> Time {
        self.rate
    }

    pub fn playhead(&self) -> Time {
        use fw_audio::MasterClock;
        self.clock.now()
    }

    /// Where the last clip ends; zero for a graph or an empty timeline.
    pub fn duration(&self) -> Time {
        let doc = self.editor.doc();
        doc.timeline(doc.root())
            .and_then(|t| t.clips.values().map(|p| p.end()).max())
            .unwrap_or(Time::ZERO)
    }

    fn changed(&mut self, event: fw_doc::Event) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = self.editor.doc().clone();
        self.player.invalidate();
        self.events.push(Event::Document(event));
    }
}

/// Renders frames of the latest document.
struct Edited {
    latest: Arc<Mutex<Document>>,
    frames: DocumentFrames,
}

impl FrameSource for Edited {
    type Frame = GpuImage;

    fn frame(&mut self, index: i64) -> Result<GpuImage, String> {
        // A cheap clone: the document is made of persistent maps.
        self.frames.doc = self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.frames.frame(index)
    }
}
