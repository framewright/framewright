//! Playing through the system's audio device with cpal.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::clock::SampleClock;
use crate::{AudioError, CHANNELS, Mixer};

/// A running output stream. Dropping it stops the sound.
pub struct Output {
    _stream: cpal::Stream,
    pub device: String,
    pub rate: u32,
}

/// The largest block we accept from the device, in frames.
pub const MAX_FRAMES: usize = 8192;

/// Opens the default output device at `rate` and runs `mixer` in its callback.
/// The device's reported latency is fed to `clock`.
///
/// TODO: resample when the device can't run at the session rate.
pub fn start_output(
    mut mixer: Mixer,
    clock: Arc<SampleClock>,
    rate: u32,
) -> Result<Output, AudioError> {
    let err = |e: &dyn std::fmt::Display| AudioError::Output(e.to_string());
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Output("no default output device".into()))?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let config = cpal::StreamConfig {
        channels: CHANNELS as u16,
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let t = info.timestamp();
                let latency = t.playback.duration_since(t.callback);
                clock.set_latency((latency.as_secs_f64() * rate as f64) as i64);
                for block in data.chunks_mut(MAX_FRAMES * CHANNELS) {
                    mixer.process(block);
                }
            },
            |e| eprintln!("fw: audio output: {e}"),
            None,
        )
        .map_err(|e| err(&e))?;
    stream.play().map_err(|e| err(&e))?;
    Ok(Output {
        _stream: stream,
        device: name,
        rate,
    })
}
