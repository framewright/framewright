//! Playback: the master clock picks the frame, a render thread works ahead.
//!
//! - The frame shown is always the one the clock says is due. When it isn't
//!   ready, the last frame holds and the miss is counted, so playback never
//!   stalls and never drifts from the sound.
//! - The render thread renders from the frame due onwards, skipping any it's
//!   already too late for (stale requests are dropped, not queued).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use fw_audio::MasterClock;
use fw_compile::{Options, compile};
use fw_core::Time;
use fw_doc::Document;
use fw_media::ProjectMedia;
use fw_plugin::Registry;
use fw_render::{GpuImage, Renderer};

/// Makes the image for a frame. Runs on the render thread.
pub trait FrameSource: Send {
    /// What a frame is: for showing, an image on the GPU.
    type Frame: Clone + Send + 'static;

    fn frame(&mut self, index: i64) -> Result<Self::Frame, String>;

    /// Called when there's nothing to render, for upkeep that can wait.
    fn idle(&mut self) {}
}

/// Frames of a document: compiled at `index / rate` and rendered to images
/// that stay on the GPU.
pub struct DocumentFrames {
    pub doc: Document,
    pub registry: Arc<Registry>,
    pub media: ProjectMedia,
    pub renderer: Renderer,
    pub rate: Time,
    pub options: Options,
}

impl FrameSource for DocumentFrames {
    type Frame = GpuImage;

    fn frame(&mut self, index: i64) -> Result<GpuImage, String> {
        let time = Time::of_frame(index, self.rate).map_err(|e| e.to_string())?;
        let options = Options {
            time,
            ..self.options.clone()
        };
        let plan =
            compile(&self.doc, &self.registry, &self.media, &options).map_err(|e| e.to_string())?;
        self.renderer
            .render_gpu(&plan, &self.media)
            .map_err(|e| e.to_string())
    }
}

/// The clock an editor plays by: held at the playhead while paused, and
/// following a running clock (the sound's, or the wall clock) while playing.
pub struct Transport {
    state: Mutex<TransportState>,
}

enum TransportState {
    Held(Time),
    Following(Arc<dyn MasterClock>),
}

impl Transport {
    pub fn new(at: Time) -> Transport {
        Transport {
            state: Mutex::new(TransportState::Held(at)),
        }
    }

    /// Stops at `at`.
    pub fn hold(&self, at: Time) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = TransportState::Held(at);
    }

    /// Runs with `clock` from now on.
    pub fn follow(&self, clock: Arc<dyn MasterClock>) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = TransportState::Following(clock);
    }

    pub fn is_running(&self) -> bool {
        matches!(
            *self.state.lock().unwrap_or_else(|e| e.into_inner()),
            TransportState::Following(_)
        )
    }
}

impl MasterClock for Transport {
    fn now(&self) -> Time {
        match &*self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            TransportState::Held(t) => *t,
            TransportState::Following(clock) => clock.now(),
        }
    }
}

/// A frame to put on screen.
#[derive(Clone)]
pub struct Presented<F> {
    pub index: i64,
    /// When the frame starts on the timeline.
    pub time: Time,
    pub image: F,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    /// Frames put on screen.
    pub shown: u64,
    /// Frames whose moment passed without being shown.
    pub dropped: u64,
    /// Frames that failed to render.
    pub errors: u64,
}

struct Shared<F> {
    ready: Mutex<BTreeMap<i64, Result<F, String>>>,
    rendered: Condvar,
    /// Bumped when frames rendered so far go stale; a render that started
    /// before the bump is thrown away.
    generation: AtomicU64,
    /// While paused only the frame due is rendered.
    playing: AtomicBool,
}

pub struct Player<F> {
    rate: Time,
    clock: Arc<dyn MasterClock>,
    ahead: i64,
    shared: Arc<Shared<F>>,
    wanted: Sender<i64>,
    thread: Option<JoinHandle<()>>,
    shown: Option<Presented<F>>,
    /// The frame shown was rendered before [`Player::invalidate`].
    stale: bool,
    /// The frame due that failed to render, and why. The last frame that
    /// rendered stays on screen.
    failed: Option<(i64, String)>,
    stats: PlaybackStats,
}

impl<F: Clone + Send + 'static> Player<F> {
    /// `rate` is frames per second; `ahead` how many frames to render ahead.
    pub fn new(
        source: Box<dyn FrameSource<Frame = F>>,
        clock: Arc<dyn MasterClock>,
        rate: Time,
        ahead: i64,
    ) -> Player<F> {
        let shared = Arc::new(Shared {
            ready: Mutex::new(BTreeMap::new()),
            rendered: Condvar::new(),
            generation: AtomicU64::new(0),
            playing: AtomicBool::new(true),
        });
        let (wanted, requests) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("fw-render-ahead".into())
            .spawn(move || render_ahead(source, requests, thread_shared, ahead, rate))
            .expect("can start the render thread");
        Player {
            rate,
            clock,
            ahead,
            shared,
            wanted,
            thread: Some(thread),
            shown: None,
            stale: false,
            failed: None,
            stats: PlaybackStats::default(),
        }
    }

    pub fn stats(&self) -> &PlaybackStats {
        &self.stats
    }

    /// Throws away every frame rendered so far, because what they show has
    /// changed. The frame on screen stays until its replacement is ready.
    pub fn invalidate(&mut self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared
            .ready
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.stale = self.shown.is_some();
        self.failed = None;
    }

    /// Playing (the default) renders ahead and counts dropped frames. Paused,
    /// only the frame due is rendered, so moving the clock by hand (scrubbing)
    /// renders just the frames landed on.
    pub fn set_playing(&mut self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Release);
    }

    /// Whether the frame on screen is the one due, rendered from what's
    /// current.
    pub fn up_to_date(&self) -> bool {
        let due = Some(self.due());
        self.failed.as_ref().map(|f| f.0) == due
            || !self.stale && self.shown.as_ref().map(|s| s.index) == due
    }

    /// Why the frame due failed to render, if it did.
    pub fn failure(&self) -> Option<&str> {
        let (index, message) = self.failed.as_ref()?;
        (*index == self.due()).then_some(message.as_str())
    }

    /// The frame on screen.
    pub fn shown(&self) -> Option<&Presented<F>> {
        self.shown.as_ref()
    }

    /// The frame the clock says is due now.
    pub fn due(&self) -> i64 {
        self.clock.now().frame_index(self.rate).unwrap_or(0)
    }

    /// Called once per display refresh. Returns a frame when the picture
    /// should change.
    pub fn tick(&mut self) -> Option<Presented<F>> {
        let due = self.due();
        let _ = self.wanted.send(due);
        let mut ready = self.shared.ready.lock().unwrap_or_else(|e| e.into_inner());
        // Frames well behind the playhead won't be shown again soon.
        ready.retain(|i, _| *i >= due - 1 && *i <= due + self.ahead * 2);
        let last = self.shown.as_ref().map(|s| s.index);
        if last == Some(due) && !self.stale {
            return None;
        }
        let image = match ready.get(&due) {
            Some(Ok(image)) => image.clone(),
            Some(Err(message)) => {
                // Kept in `ready`, so it isn't rendered again until an edit.
                if self.failed.as_ref().map(|f| f.0) != Some(due) {
                    self.failed = Some((due, message.clone()));
                    self.stats.errors += 1;
                }
                return None;
            }
            // Not ready: hold the last frame; the miss is counted once it's shown past.
            None => return None,
        };
        drop(ready);
        if let Some(last) = last
            && due > last
            && self.shared.playing.load(Ordering::Acquire)
        {
            self.stats.dropped += (due - last - 1) as u64;
        }
        self.stale = false;
        self.failed = None;
        self.stats.shown += 1;
        let presented = Presented {
            index: due,
            time: Time::of_frame(due, self.rate).unwrap_or(Time::ZERO),
            image,
        };
        self.shown = Some(presented.clone());
        Some(presented)
    }

    /// Blocks until `index` is rendered or `timeout` passes. For tests and
    /// for starting playback with the first frame ready. It doesn't change
    /// what's wanted; [`Player::tick`] does that.
    pub fn wait_for(&self, index: i64, timeout: Duration) -> bool {
        let ready = self.shared.ready.lock().unwrap_or_else(|e| e.into_inner());
        let (ready, _) = self
            .shared
            .rendered
            .wait_timeout_while(ready, timeout, |r| !r.contains_key(&index))
            .unwrap_or_else(|e| e.into_inner());
        ready.contains_key(&index)
    }
}

impl<F> Drop for Player<F> {
    fn drop(&mut self) {
        // Closing the channel stops the render thread.
        let (dead, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.wanted, dead));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Renders ahead of the most recently wanted frame. It aims as far ahead as
/// rendering a frame takes, so a slow source still finishes frames before
/// their moment instead of always finishing them too late.
fn render_ahead<F: Clone + Send + 'static>(
    mut source: Box<dyn FrameSource<Frame = F>>,
    requests: Receiver<i64>,
    shared: Arc<Shared<F>>,
    ahead: i64,
    rate: Time,
) {
    let mut due = 0;
    // A running average of how long a frame takes to render, in seconds.
    let mut render_seconds = 0.0f64;
    loop {
        // Take the newest request; older ones are stale.
        match requests.recv_timeout(Duration::from_millis(5)) {
            Ok(i) => due = i,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(i) = requests.try_recv() {
            due = i;
        }
        // Frames that will pass while one renders, with a quarter to spare.
        let (lead, ahead) = if shared.playing.load(Ordering::Acquire) {
            (
                (render_seconds * rate.as_f64() * 1.25).round() as i64,
                ahead,
            )
        } else {
            (0, 0)
        };
        let next = {
            let ready = shared.ready.lock().unwrap_or_else(|e| e.into_inner());
            (due + lead..=due + lead + ahead).find(|i| !ready.contains_key(i))
        };
        let Some(index) = next else {
            source.idle();
            continue;
        };
        let generation = shared.generation.load(Ordering::Acquire);
        let started = std::time::Instant::now();
        let result = source.frame(index);
        let took = started.elapsed().as_secs_f64();
        render_seconds = if render_seconds == 0.0 {
            took
        } else {
            render_seconds * 0.8 + took * 0.2
        };
        let mut ready = shared.ready.lock().unwrap_or_else(|e| e.into_inner());
        // Checked under the lock `invalidate` clears under.
        if shared.generation.load(Ordering::Acquire) == generation {
            ready.insert(index, result);
        }
        drop(ready);
        shared.rendered.notify_all();
    }
}
