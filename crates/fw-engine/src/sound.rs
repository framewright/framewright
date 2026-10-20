//! Playing the document's sound through the audio device, whose sample
//! count is then the master clock (design §8).

use std::path::PathBuf;
use std::sync::Arc;

use fw_audio::{AudioEngine, Output, SampleClock};
use fw_compile::AudioPlan;
use fw_core::NodeId;

/// What a plan plays, for telling whether a new plan sounds different.
type PlanKey = Vec<(NodeId, PathBuf, i64, i64, i64, Vec<f32>)>;

fn key(plan: &AudioPlan) -> PlanKey {
    plan.voices
        .iter()
        .map(|v| {
            (
                v.clip,
                v.path.clone(),
                v.start,
                v.end,
                v.source_start,
                v.params.clone(),
            )
        })
        .collect()
}

/// The audio device, opened on first use.
pub(crate) struct Sound {
    engine: AudioEngine,
    _output: Output,
    pub device: String,
    /// What's playing, if anything.
    playing: Option<PlanKey>,
}

impl Sound {
    pub fn open(rate: u32) -> Result<Sound, String> {
        let (engine, mixer) = AudioEngine::new(rate, fw_audio::output::MAX_FRAMES);
        let output = fw_audio::start_output(mixer, engine.clock().clone(), rate)
            .map_err(|e| e.to_string())?;
        Ok(Sound {
            device: format!("{} at {} Hz", output.device, output.rate),
            engine,
            _output: output,
            playing: None,
        })
    }

    /// Plays `plan` from timeline sample `from`. Returns the clock that
    /// counts the samples heard.
    pub fn play(&mut self, plan: &AudioPlan, from: i64) -> Result<Arc<SampleClock>, String> {
        // Hold at the start until the sound begins.
        self.engine.clock().advance_to(from);
        self.engine.play(plan, from).map_err(|e| e.to_string())?;
        self.playing = Some(key(plan));
        Ok(self.engine.clock().clone())
    }

    pub fn stop(&mut self) {
        if self.playing.take().is_some() {
            self.engine.stop();
        }
    }

    /// Whether `plan` sounds the same as what's playing.
    pub fn is_playing(&self, plan: &AudioPlan) -> bool {
        self.playing.as_ref() == Some(&key(plan))
    }

    pub fn underruns(&self) -> u64 {
        self.engine
            .stats()
            .underruns
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn take_errors(&self) -> Vec<String> {
        self.engine.take_errors()
    }
}
