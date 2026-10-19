//! The real-time audio engine.
//!
//! - [`Mixer`] runs inside the audio device's callback, and never allocates,
//!   locks or waits.
//! - A streaming thread decodes each playing clip ahead into a lock-free ring.
//! - [`AudioEngine`] is the control side: it builds voice sets off the audio
//!   thread, swaps them in without locks, and frees old ones.
//! - [`SampleClock`] counts samples played and is the master clock.

mod clock;
mod mixer;
pub mod output;
mod stream;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use fw_compile::AudioPlan;

pub use clock::{FakeClock, MasterClock, SampleClock, WallClock};
pub use mixer::{Mixer, MixerStats, RtVoice};
pub use output::{Output, start_output};

use mixer::{Command, Garbage};

/// Interleaved channels everywhere in the engine. TODO: surround layouts.
pub const CHANNELS: usize = fw_media::CHANNELS;

/// How far ahead each voice is decoded, in seconds.
const RING_SECONDS: usize = 1;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("the plan is at {plan} Hz but the engine runs at {engine} Hz")]
    RateMismatch { plan: u32, engine: u32 },
    #[error("the streaming thread didn't respond")]
    Streamer,
    #[error("no audio output: {0}")]
    Output(String),
}

pub struct AudioEngine {
    rate: u32,
    max_frames: usize,
    commands: rtrb::Producer<Command>,
    garbage: rtrb::Consumer<Garbage>,
    streamer: mpsc::Sender<stream::Message>,
    thread: Option<JoinHandle<()>>,
    clock: Arc<SampleClock>,
    stats: Arc<MixerStats>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl AudioEngine {
    /// Makes an engine and the mixer to put in the device callback.
    /// `max_frames` is the largest block the device will ask for.
    pub fn new(rate: u32, max_frames: usize) -> (AudioEngine, Mixer) {
        let (commands, command_rx) = rtrb::RingBuffer::new(16);
        let (garbage_tx, garbage) = rtrb::RingBuffer::new(16);
        let clock = Arc::new(SampleClock::new(rate));
        let stats = Arc::new(MixerStats::default());
        let errors = Arc::new(Mutex::new(vec![]));
        let (streamer, messages) = mpsc::channel();
        let thread_errors = errors.clone();
        let thread = std::thread::Builder::new()
            .name("fw-audio-stream".into())
            .spawn(move || stream::run(messages, thread_errors))
            .expect("can start the streaming thread");
        let mixer = Mixer::new(
            command_rx,
            garbage_tx,
            clock.clone(),
            stats.clone(),
            max_frames,
        );
        let engine = AudioEngine {
            rate,
            max_frames,
            commands,
            garbage,
            streamer,
            thread: Some(thread),
            clock,
            stats,
            errors,
        };
        (engine, mixer)
    }

    pub fn clock(&self) -> &Arc<SampleClock> {
        &self.clock
    }

    pub fn stats(&self) -> &Arc<MixerStats> {
        &self.stats
    }

    /// Problems the streaming thread met, such as files it couldn't open.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Starts playing `plan` from timeline sample `from`. Returns once every
    /// voice has its first second decoded, so playback starts cleanly.
    pub fn play(&mut self, plan: &AudioPlan, from: i64) -> Result<(), AudioError> {
        if plan.sample_rate != self.rate {
            return Err(AudioError::RateMismatch {
                plan: plan.sample_rate,
                engine: self.rate,
            });
        }
        self.collect_garbage();
        let mut voices = Vec::with_capacity(plan.voices.len());
        let mut jobs = vec![];
        for v in plan.voices.iter().filter(|v| v.end > from) {
            let ring_start = from.max(v.start);
            let (ring, samples) =
                rtrb::RingBuffer::new(self.rate as usize * CHANNELS * RING_SECONDS);
            let mut processor = v.behaviour.create();
            processor.prepare(self.rate, self.max_frames);
            voices.push(RtVoice {
                start: v.start,
                end: v.end,
                samples,
                ring_start,
                processor,
                params: v.params.clone(),
            });
            jobs.push(stream::Job {
                path: v.path.clone(),
                origin: v.origin,
                rate: self.rate,
                source_from: v.source_start + (ring_start - v.start),
                frames: v.end - ring_start,
                ring,
            });
        }
        let (ready, filled) = mpsc::channel();
        self.streamer
            .send(stream::Message::Jobs(jobs, ready))
            .map_err(|_| AudioError::Streamer)?;
        filled
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| AudioError::Streamer)?;
        self.send(Command::Play { voices, from });
        Ok(())
    }

    pub fn stop(&mut self) {
        self.send(Command::Stop);
        self.collect_garbage();
    }

    /// Frees voice sets the audio thread has finished with.
    pub fn collect_garbage(&mut self) {
        while self.garbage.pop().is_ok() {}
    }

    fn send(&mut self, command: Command) {
        // The queue only fills if the audio thread has stopped calling us;
        // then the newest command is the one that matters, so wait briefly.
        let mut command = command;
        for _ in 0..100 {
            match self.commands.push(command) {
                Ok(()) => return,
                Err(rtrb::PushError::Full(c)) => {
                    command = c;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.streamer.send(stream::Message::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
