//! The master clock. During playback the audio device sets the pace and
//! everything else, including which video frame is shown, follows it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use fw_core::Time;

/// Where playback is now, on the timeline.
pub trait MasterClock: Send + Sync {
    fn now(&self) -> Time;
}

/// Counts samples as the audio thread writes them.
///
/// Between blocks it interpolates with the wall clock, so frame selection
/// isn't quantized to the audio block size.
pub struct SampleClock {
    rate: u32,
    epoch: Instant,
    /// The timeline sample the next block starts at.
    position: AtomicI64,
    /// When that block was handed to the device, in nanoseconds since `epoch`.
    stamped: AtomicU64,
    /// Samples between writing a block and it being heard.
    latency: AtomicI64,
    running: std::sync::atomic::AtomicBool,
}

impl SampleClock {
    pub fn new(rate: u32) -> SampleClock {
        SampleClock {
            rate,
            epoch: Instant::now(),
            position: AtomicI64::new(0),
            stamped: AtomicU64::new(0),
            latency: AtomicI64::new(0),
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Called by the audio thread after each block. Lock-free.
    pub fn advance_to(&self, position: i64) {
        self.position.store(position, Ordering::Release);
        self.stamped
            .store(self.epoch.elapsed().as_nanos() as u64, Ordering::Release);
    }

    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Release);
    }

    /// How long the device takes to play what's written, in samples.
    pub fn set_latency(&self, samples: i64) {
        self.latency.store(samples, Ordering::Release);
    }

    /// The timeline sample being heard right now.
    pub fn sample_now(&self) -> i64 {
        let position = self.position.load(Ordering::Acquire);
        let latency = self.latency.load(Ordering::Acquire);
        if !self.running.load(Ordering::Acquire) {
            return position - latency;
        }
        // Read the stamp first: the audio thread may stamp again at any moment,
        // and a stamp newer than `elapsed` must not underflow.
        let stamped = self.stamped.load(Ordering::Acquire);
        let since = (self.epoch.elapsed().as_nanos() as u64).saturating_sub(stamped);
        // Never run more than ~50 ms past the last block, so a stalled device
        // stalls the picture rather than letting it drift.
        let since = (since as f64 * self.rate as f64 / 1e9).min(self.rate as f64 / 20.0) as i64;
        position - latency + since
    }
}

impl MasterClock for SampleClock {
    fn now(&self) -> Time {
        Time::new(self.sample_now(), self.rate as i64).unwrap_or(Time::ZERO)
    }
}

/// The wall clock, for playing pictures when there's no audio device.
/// It holds at its starting point until [`WallClock::start`].
pub struct WallClock {
    started: Mutex<Option<Instant>>,
    from: Time,
}

impl WallClock {
    pub fn starting_at(from: Time) -> WallClock {
        WallClock {
            started: Mutex::new(None),
            from,
        }
    }

    pub fn start(&self) {
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
}

impl MasterClock for WallClock {
    fn now(&self) -> Time {
        let started = *self.started.lock().unwrap_or_else(|e| e.into_inner());
        let Some(started) = started else {
            return self.from;
        };
        let micros = started.elapsed().as_micros() as i64;
        self.from
            .checked_add(Time::new(micros, 1_000_000).unwrap_or(Time::ZERO))
            .unwrap_or(self.from)
    }
}

/// A clock tests move by hand.
pub struct FakeClock(Mutex<Time>);

impl FakeClock {
    pub fn new(start: Time) -> FakeClock {
        FakeClock(Mutex::new(start))
    }

    pub fn set(&self, t: Time) {
        *self.0.lock().unwrap() = t;
    }
}

impl MasterClock for FakeClock {
    fn now(&self) -> Time {
        *self.0.lock().unwrap()
    }
}
