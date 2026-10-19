//! The real-time side: runs inside the audio device's callback.
//!
//! Nothing here allocates, locks, waits or does I/O. New voice sets arrive
//! through a lock-free queue and old ones leave through another, to be freed
//! on a normal thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fw_plugin::{AudioBlock, AudioProcessor};

use crate::CHANNELS;
use crate::clock::SampleClock;

/// One clip's sound, ready for the audio thread.
pub struct RtVoice {
    /// Timeline samples it plays, `[start, end)`.
    pub start: i64,
    pub end: i64,
    /// Decoded samples, fed by the streaming thread, starting at `ring_start`.
    pub samples: rtrb::Consumer<f32>,
    pub ring_start: i64,
    pub processor: Box<dyn AudioProcessor>,
    pub params: Vec<f32>,
}

pub(crate) enum Command {
    Play { voices: Vec<RtVoice>, from: i64 },
    Stop,
}

pub(crate) type Garbage = Vec<RtVoice>;

/// Counts blocks where a voice's samples weren't ready in time.
#[derive(Default)]
pub struct MixerStats {
    pub underruns: AtomicU64,
    pub blocks: AtomicU64,
}

pub struct Mixer {
    voices: Option<Garbage>,
    commands: rtrb::Consumer<Command>,
    garbage: rtrb::Producer<Garbage>,
    playhead: i64,
    playing: bool,
    clock: Arc<SampleClock>,
    stats: Arc<MixerStats>,
    /// Scratch for one voice's input and output, sized for the largest block.
    input: Vec<f32>,
    output: Vec<f32>,
}

impl Mixer {
    pub(crate) fn new(
        commands: rtrb::Consumer<Command>,
        garbage: rtrb::Producer<Garbage>,
        clock: Arc<SampleClock>,
        stats: Arc<MixerStats>,
        max_frames: usize,
    ) -> Mixer {
        Mixer {
            voices: None,
            commands,
            garbage,
            playhead: 0,
            playing: false,
            clock,
            stats,
            input: vec![0.0; max_frames * CHANNELS],
            output: vec![0.0; max_frames * CHANNELS],
        }
    }

    /// Fills `out` (interleaved stereo) with the next block.
    pub fn process(&mut self, out: &mut [f32]) {
        while let Ok(command) = self.commands.pop() {
            match command {
                Command::Play { voices, from } => {
                    if let Some(old) = self.voices.replace(voices) {
                        self.retire(old);
                    }
                    self.playhead = from;
                    self.playing = true;
                    self.clock.set_running(true);
                }
                Command::Stop => {
                    if let Some(old) = self.voices.take() {
                        self.retire(old);
                    }
                    self.playing = false;
                    self.clock.set_running(false);
                }
            }
        }
        out.fill(0.0);
        if !self.playing {
            return;
        }
        self.stats.blocks.fetch_add(1, Ordering::Relaxed);
        let frames = (out.len() / CHANNELS).min(self.input.len() / CHANNELS);
        let (block_start, block_end) = (self.playhead, self.playhead + frames as i64);
        for voice in self.voices.iter_mut().flat_map(|v| v.iter_mut()) {
            let from = block_start.max(voice.start).max(voice.ring_start);
            let to = block_end.min(voice.end);
            if from >= to {
                continue;
            }
            let input = &mut self.input[..frames * CHANNELS];
            input.fill(0.0);
            let offset = (from - block_start) as usize * CHANNELS;
            let wanted = (to - from) as usize * CHANNELS;
            let available = voice.samples.slots().min(wanted);
            if let Ok(chunk) = voice.samples.read_chunk(available) {
                let (a, b) = chunk.as_slices();
                input[offset..offset + a.len()].copy_from_slice(a);
                input[offset + a.len()..offset + a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            if available < wanted {
                self.stats.underruns.fetch_add(1, Ordering::Relaxed);
            }
            let output = &mut self.output[..frames * CHANNELS];
            let mut block = AudioBlock {
                input,
                output,
                frames,
                channels: CHANNELS,
                params: &voice.params,
            };
            voice.processor.process(&mut block);
            for (o, v) in out.iter_mut().zip(block.output.iter()) {
                *o += v;
            }
        }
        self.playhead = block_end;
        self.clock.advance_to(self.playhead);
    }

    /// Hands a voice set back to be freed off the audio thread. If that
    /// queue is somehow full, leaking beats freeing here.
    fn retire(&mut self, old: Garbage) {
        if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
            std::mem::forget(old);
        }
    }
}
