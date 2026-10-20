//! A project open for editing: the engine as one object (design §10.1).
//!
//! Clients drive it with [`Request`]s and hear back through [`Event`]s, all
//! plain serializable data, so the UI, scripts and tests drive it the same
//! way. Calls return quickly: rendering, decoding and making editing copies
//! happen on the engine's own threads.
//!
//! The one exchange that isn't a message is [`Engine::viewer_frame`], the
//! frame to show as a texture on the engine's GPU, because showing it must
//! not copy.
//!
//! TODO: the command journal, serving requests over a socket (`fw-script`).

mod frames;
mod media;
mod sound;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fw_audio::{MasterClock, WallClock};
use fw_compile::Options;
use fw_core::Time;
use fw_doc::{Command, CommandError, Document, Editor};
use fw_media::{HardwarePolicy, ProjectMedia};
use fw_playback::{DocumentFrames, Player, Transport};
use fw_plugin::{Quality, Registry};
use fw_render::{DEFAULT_CACHE_BYTES, DEFAULT_FRAME_CACHE_BYTES, Gpu, GpuImage, Renderer};
use serde::{Deserialize, Serialize};

pub use fw_ops::{PluginProblem, plugin_folders};
pub use media::{MediaSummary, Picture};

use crate::frames::{Edited, Latest, SharedLatest};
use crate::sound::Sound;

/// How long sound keeps playing what it was given after an edit, so a drag
/// restarts it once rather than on every step.
/// TODO: change parameters on the audio thread without restarting.
const SOUND_SETTLE: Duration = Duration::from_millis(200);

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
    /// Moves the playhead, playing on from there if playing.
    Seek {
        time: Time,
    },
    /// Plays from the playhead, with sound when there is some; from the
    /// start when the playhead is at the end.
    Play,
    Pause,
    /// Writes the document to its file.
    Save,
    /// Reads the plugin folders again and renders with what's there now, as
    /// after an edit.
    ReloadPlugins,
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
    /// Playing started or stopped, at `time`.
    Transport {
        playing: bool,
        time: Time,
    },
    /// The plugins were read again; `problems` says what was wrong with them.
    PluginsReloaded {
        problems: Vec<String>,
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

/// How an engine reads media and plays.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Make editing copies of videos in the background, and scrub from them.
    pub editing_copies: bool,
    /// Decode in hardware when the GPU can, as the policy says; `None` never.
    pub hardware: Option<HardwarePolicy>,
    /// Play sound through the default audio device. Without it, playing
    /// keeps time by the wall clock.
    pub sound: bool,
    /// GPU memory for keeping finished frames, so showing them again is free.
    pub frame_cache_bytes: usize,
    /// Where shader plugins are read from, after the built-in nodes; see
    /// [`fw_ops::plugin_folders`]. None by default, so tests see only the
    /// built-ins.
    pub plugin_folders: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            editing_copies: true,
            hardware: Some(HardwarePolicy::Fastest),
            sound: true,
            frame_cache_bytes: DEFAULT_FRAME_CACHE_BYTES,
            plugin_folders: vec![],
        }
    }
}

/// How playing is going.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaybackStatus {
    pub playing: bool,
    /// Frames shown and dropped, and frames that failed, since opening.
    pub shown: u64,
    pub dropped: u64,
    pub errors: u64,
    /// The audio device, once sound has played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<String>,
    /// Times the audio device asked for sound that wasn't ready.
    pub underruns: u64,
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

/// The audio device, opened when sound first plays.
enum SoundState {
    Closed,
    Open(Box<Sound>),
    /// It couldn't be opened; playing keeps time by the wall clock.
    Unavailable,
}

pub struct Engine {
    path: PathBuf,
    editor: Editor,
    settings: Settings,
    /// The document as playback renders it, replaced on every change.
    latest: SharedLatest,
    rate: Time,
    /// Timeline samples per second; `None` for a graph, which is silent.
    sample_rate: Option<u32>,
    transport: Arc<Transport>,
    player: Player<GpuImage>,
    sound: SoundState,
    /// When the document last changed while sound was playing.
    sound_stale: Option<Instant>,
    serial: u64,
    saved_revision: u64,
    events: Vec<Event>,
    gpu: Arc<Gpu>,
    warnings: Vec<String>,
    /// For looking at media files, apart from the one playback renders with.
    info: ProjectMedia,
    /// [`Engine::media`] as of a revision.
    media: Option<(u64, Vec<MediaSummary>)>,
}

impl Engine {
    /// Opens the project file at `path`, rendering on `gpu`.
    pub fn open(path: &Path, gpu: Arc<Gpu>, settings: Settings) -> Result<Engine, EngineError> {
        let (registry, problems) = fw_ops::registry(&settings.plugin_folders);
        let registry = Arc::new(registry);
        let doc = fw_doc::save::load(path, &registry)?;
        let folder = path.parent().unwrap_or(Path::new("."));
        let mut warnings: Vec<String> = problems.iter().map(ToString::to_string).collect();
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
        let timeline = doc.timeline(doc.root()).map(|t| t.settings.clone());
        let rate = timeline.as_ref().map_or(Time::from_seconds(24), |t| t.rate);
        let latest = Arc::new(Mutex::new(Latest::new(doc.clone(), registry.clone())));
        let mut options = Options::at(Time::ZERO);
        options.quality = Quality::Draft;
        let renderer = Renderer::new(gpu.clone(), DEFAULT_CACHE_BYTES)
            .with_frame_cache(settings.frame_cache_bytes);
        let frames = DocumentFrames {
            doc: doc.clone(),
            registry: registry.clone(),
            media,
            renderer,
            rate,
            options,
        };
        let transport = Arc::new(Transport::new(Time::ZERO));
        let mut player = Player::new(
            Box::new(Edited::new(latest.clone(), frames)),
            transport.clone(),
            rate,
            6,
        );
        player.set_playing(false);
        Ok(Engine {
            path: path.to_owned(),
            editor: Editor::new(doc, registry),
            settings,
            latest,
            rate,
            sample_rate: timeline.map(|t| t.sample_rate),
            transport,
            player,
            sound: SoundState::Closed,
            sound_stale: None,
            serial: 0,
            saved_revision: 0,
            events: vec![],
            gpu,
            warnings,
            info: ProjectMedia::new(folder),
            media: None,
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
                if self.is_playing() {
                    self.stop_sound();
                    let clock = self.start_clock(time);
                    self.transport.follow(clock);
                } else {
                    self.transport.hold(time);
                }
            }
            Request::Play => self.play(),
            Request::Pause => self.pause(),
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
            Request::ReloadPlugins => self.reload_plugins(),
        }
        Ok(())
    }

    /// Events since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Problems that didn't stop anything, such as falling back to
    /// software decoding or playing without sound.
    pub fn take_warnings(&mut self) -> Vec<String> {
        if let SoundState::Open(sound) = &self.sound {
            self.warnings.extend(sound.take_errors());
        }
        std::mem::take(&mut self.warnings)
    }

    /// The frame to show now. Call once per display refresh.
    pub fn viewer_frame(&mut self) -> Option<ViewerFrame<'_>> {
        if self.is_playing() {
            self.keep_playing();
        }
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

    pub fn is_playing(&self) -> bool {
        self.transport.is_running()
    }

    pub fn playback(&self) -> PlaybackStatus {
        let stats = self.player.stats();
        let (sound, underruns) = match &self.sound {
            SoundState::Open(s) => (Some(s.device.clone()), s.underruns()),
            _ => (None, 0),
        };
        PlaybackStatus {
            playing: self.is_playing(),
            shown: stats.shown,
            dropped: stats.dropped,
            errors: stats.errors,
            sound,
            underruns,
        }
    }

    /// Runs of frames the frame cache holds for the current document, as
    /// `[start, end)` frame indices.
    /// Where shader plugins are read from.
    pub fn plugin_folders(&self) -> &[PathBuf] {
        &self.settings.plugin_folders
    }

    pub fn cached_frames(&self) -> Vec<(i64, i64)> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cached_ranges()
    }

    /// The media files the document uses, in the order first used.
    pub fn media(&mut self) -> &[MediaSummary] {
        let revision = self.editor.revision();
        if self.media.as_ref().map(|m| m.0) != Some(revision) {
            let summaries = media::summarize_media(self.editor.doc(), &self.info);
            self.media = Some((revision, summaries));
        }
        &self.media.as_ref().expect("just filled").1
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
        self.transport.now()
    }

    /// Where the last clip ends; zero for a graph or an empty timeline.
    pub fn duration(&self) -> Time {
        let doc = self.editor.doc();
        doc.timeline(doc.root())
            .and_then(|t| t.clips.values().map(|p| p.end()).max())
            .unwrap_or(Time::ZERO)
    }

    fn changed(&mut self, event: fw_doc::Event) {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(self.editor.doc().clone());
        self.player.invalidate();
        if self.is_playing() {
            self.sound_stale = Some(Instant::now());
        }
        self.media = None;
        self.events.push(Event::Document(event));
    }

    // TODO: nodes that loaded as unknown because their plugin was missing stay
    // unknown until the document is opened again.
    fn reload_plugins(&mut self) {
        let (registry, problems) = fw_ops::registry(&self.settings.plugin_folders);
        let registry = Arc::new(registry);
        self.editor.set_registry(registry.clone());
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace_registry(registry);
        self.player.invalidate();
        if self.is_playing() {
            self.sound_stale = Some(Instant::now());
        }
        let problems = problems.iter().map(ToString::to_string).collect();
        self.events.push(Event::PluginsReloaded { problems });
    }

    fn play(&mut self) {
        if self.is_playing() {
            return;
        }
        let mut from = self.frame_start(self.playhead());
        let end = self.duration();
        if end > Time::ZERO && self.last_frame().is_some_and(|last| from >= last) {
            from = Time::ZERO;
        }
        self.player.set_playing(true);
        let clock = self.start_clock(from);
        self.transport.follow(clock);
        self.events.push(Event::Transport {
            playing: true,
            time: from,
        });
    }

    fn pause(&mut self) {
        if !self.is_playing() {
            return;
        }
        let mut at = self.frame_start(self.playhead());
        if let Some(last) = self.last_frame() {
            at = at.min(last);
        }
        self.stop_sound();
        self.transport.hold(at);
        self.player.set_playing(false);
        self.sound_stale = None;
        self.events.push(Event::Transport {
            playing: false,
            time: at,
        });
    }

    /// Upkeep while playing: stop at the end, and let sound catch up with
    /// edits once they settle.
    fn keep_playing(&mut self) {
        if let Some(last) = self.last_frame()
            && self.playhead() >= last
        {
            self.pause();
            return;
        }
        if self
            .sound_stale
            .is_some_and(|t| t.elapsed() >= SOUND_SETTLE)
        {
            self.sound_stale = None;
            let plan = self.audio_plan();
            let same = match (&self.sound, &plan) {
                (SoundState::Open(sound), Some(plan)) => sound.is_playing(plan),
                (SoundState::Open(_), None) => false,
                // Not playing sound; a plan with voices would start some.
                (_, plan) => plan.is_none(),
            };
            if !same {
                let now = self.playhead();
                self.stop_sound();
                let clock = self.start_clock(now);
                self.transport.follow(clock);
            }
        }
    }

    /// Starts sound from `from` and returns its clock, or the wall clock
    /// when there's no sound to play.
    fn start_clock(&mut self, from: Time) -> Arc<dyn MasterClock> {
        if let (Some(plan), Some(rate)) = (self.audio_plan(), self.sample_rate)
            && let Some(sound) = self.open_sound(rate)
        {
            let started = from
                .sample_index(rate)
                .map_err(|e| e.to_string())
                .and_then(|sample| sound.play(&plan, sample));
            match started {
                Ok(clock) => return clock,
                Err(e) => self.warnings.push(format!("playing without sound: {e}")),
            }
        }
        let wall = Arc::new(WallClock::starting_at(from));
        wall.start();
        wall
    }

    fn stop_sound(&mut self) {
        if let SoundState::Open(sound) = &mut self.sound {
            sound.stop();
        }
    }

    fn open_sound(&mut self, rate: u32) -> Option<&mut Sound> {
        if matches!(self.sound, SoundState::Closed) {
            self.sound = match Sound::open(rate) {
                Ok(sound) => SoundState::Open(Box::new(sound)),
                Err(e) => {
                    self.warnings.push(format!("playing without sound: {e}"));
                    SoundState::Unavailable
                }
            };
        }
        match &mut self.sound {
            SoundState::Open(sound) => Some(sound),
            _ => None,
        }
    }

    /// The document's sound, when sound is wanted and there is some.
    fn audio_plan(&mut self) -> Option<fw_compile::AudioPlan> {
        if !self.settings.sound {
            return None;
        }
        let doc = self.editor.doc();
        match fw_compile::compile_audio(doc, self.editor.registry(), &self.info) {
            Ok(plan) if !plan.voices.is_empty() => Some(plan),
            Ok(_) => None,
            Err(e) => {
                self.warnings.push(format!("playing without sound: {e}"));
                None
            }
        }
    }

    fn frame_start(&self, t: Time) -> Time {
        let index = t.frame_index(self.rate).unwrap_or(0);
        Time::of_frame(index.max(0), self.rate).unwrap_or(Time::ZERO)
    }

    /// The start of the timeline's last frame; `None` when it's empty.
    fn last_frame(&self) -> Option<Time> {
        let end = self.duration();
        if end <= Time::ZERO {
            return None;
        }
        // The frame the moment just before the end falls in.
        let frames = (end.as_f64() * self.rate.as_f64()).ceil() as i64;
        Time::of_frame((frames - 1).max(0), self.rate).ok()
    }
}
