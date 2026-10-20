//! Audio decoding through FFmpeg, resampled to the session's rate as
//! interleaved stereo `f32`.
//!
//! TODO: channel layouts beyond stereo (design §8), and sample-exact
//! alignment through the resampler's delay when rates differ.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use fw_core::Time;
use rsmpeg::avcodec::{AVCodec, AVCodecContext};
use rsmpeg::avformat::AVFormatContextInput;
use rsmpeg::avutil::AVFrame;
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swresample::SwrContext;

use crate::MediaError;

/// Output channels. Everything is mixed to stereo for now.
pub const CHANNELS: usize = 2;

/// Decoding this much before a seek target lets codecs with overlapping
/// frames (AAC, Opus) produce the target exactly.
const PREROLL_SECONDS: f64 = 0.2;

fn decode_error(path: &Path) -> impl Fn(String) -> MediaError + use<> {
    let path = path.to_owned();
    move |message| MediaError::Decode {
        path: path.clone(),
        message,
    }
}

/// Whether a file has an audio stream.
pub fn has_audio(path: &Path) -> Result<bool, MediaError> {
    let err = decode_error(path);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| err("path contains a NUL byte".into()))?;
    let input = AVFormatContextInput::open(&c_path).map_err(|e| err(e.to_string()))?;
    Ok(input
        .find_best_stream(ffi::AVMEDIA_TYPE_AUDIO)
        .map_err(|e| err(e.to_string()))?
        .is_some())
}

/// How long a file's sound lasts, as its container says, or `None` when it
/// doesn't say.
pub fn audio_duration(path: &Path) -> Result<Option<Time>, MediaError> {
    let err = decode_error(path);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| err("path contains a NUL byte".into()))?;
    let input = AVFormatContextInput::open(&c_path).map_err(|e| err(e.to_string()))?;
    let (stream, _) = input
        .find_best_stream(ffi::AVMEDIA_TYPE_AUDIO)
        .map_err(|e| err(e.to_string()))?
        .ok_or_else(|| err("no audio stream".into()))?;
    let s = &input.streams()[stream];
    let from_stream = (s.duration != ffi::AV_NOPTS_VALUE && s.duration > 0)
        .then(|| Time::new(s.duration * s.time_base.num as i64, s.time_base.den as i64).ok())
        .flatten();
    let from_container = (input.duration != ffi::AV_NOPTS_VALUE && input.duration > 0)
        .then(|| Time::new(input.duration, ffi::AV_TIME_BASE as i64).ok())
        .flatten();
    Ok(from_stream.or(from_container))
}

/// Streams one file's audio. Not `Send`: create it on the thread that reads it.
pub struct AudioReader {
    path: PathBuf,
    input: AVFormatContextInput,
    decoder: AVCodecContext,
    stream: usize,
    tick: Time,
    rate: u32,
    /// Source time 0 on the container's clock, in seconds.
    origin: Time,
    resampler: Option<SwrContext>,
    /// Decoded, resampled samples not yet read, interleaved.
    buffered: VecDeque<f32>,
    /// The source sample (at `rate`) that `buffered` starts at.
    position: i64,
    /// After a seek: samples before this are dropped.
    skip_to: Option<i64>,
    draining: bool,
    finished: bool,
}

impl AudioReader {
    /// Opens the best audio stream, resampling to `rate`. `origin` is source
    /// time 0 in seconds on the container's clock, normally the video's first
    /// frame; `None` uses the audio's own first timestamp.
    pub fn open(path: &Path, rate: u32, origin: Option<Time>) -> Result<AudioReader, MediaError> {
        let err = decode_error(path);
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| err("path contains a NUL byte".into()))?;
        let input = AVFormatContextInput::open(&c_path).map_err(|e| err(e.to_string()))?;
        let (stream, codec) = input
            .find_best_stream(ffi::AVMEDIA_TYPE_AUDIO)
            .map_err(|e| err(e.to_string()))?
            .ok_or_else(|| err("no audio stream".into()))?;
        let (par, time_base, start) = {
            let s = &input.streams()[stream];
            (s.codecpar().clone(), s.time_base, s.start_time)
        };
        let tick = Time::new(time_base.num as i64, time_base.den as i64)
            .map_err(|e| err(e.to_string()))?;
        let origin = match origin {
            Some(o) => o,
            None if start != ffi::AV_NOPTS_VALUE => tick
                .checked_mul(Time::from_seconds(start))
                .unwrap_or(Time::ZERO),
            None => Time::ZERO,
        };
        let codec = AVCodec::find_decoder(codec.id)
            .ok_or_else(|| err("no decoder for this codec".into()))?;
        let mut decoder = AVCodecContext::new(&codec);
        decoder
            .apply_codecpar(&par)
            .map_err(|e| err(e.to_string()))?;
        decoder.open(None).map_err(|e| err(e.to_string()))?;
        Ok(AudioReader {
            path: path.to_owned(),
            input,
            decoder,
            stream,
            tick,
            rate,
            origin,
            resampler: None,
            buffered: VecDeque::new(),
            position: 0,
            // Align to source time 0 like after a seek, in case the audio
            // starts before or after the video's first frame.
            skip_to: Some(0),
            draining: false,
            finished: false,
        })
    }

    /// Moves to source sample `sample` (at the output rate).
    pub fn seek(&mut self, sample: i64) -> Result<(), MediaError> {
        let err = decode_error(&self.path);
        let target_seconds =
            sample as f64 / self.rate as f64 + self.origin.as_f64() - PREROLL_SECONDS;
        let ts = (target_seconds / self.tick.as_f64()).floor() as i64;
        self.input
            .seek(self.stream as i32, ts, ffi::AVSEEK_FLAG_BACKWARD as i32)
            .map_err(|e| err(e.to_string()))?;
        self.decoder.flush_buffers();
        self.resampler = None;
        self.buffered.clear();
        self.skip_to = Some(sample);
        self.draining = false;
        self.finished = false;
        Ok(())
    }

    /// Fills `out` (interleaved stereo) from the current position. Returns
    /// the number of stereo frames written; fewer than asked means the end.
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize, MediaError> {
        let wanted = out.len() / CHANNELS;
        while self.buffered.len() / CHANNELS < wanted && !self.finished {
            self.decode_more()?;
        }
        let frames = wanted.min(self.buffered.len() / CHANNELS);
        for (o, s) in out.iter_mut().zip(self.buffered.drain(..frames * CHANNELS)) {
            *o = s;
        }
        self.position += frames as i64;
        Ok(frames)
    }

    fn decode_more(&mut self) -> Result<(), MediaError> {
        let err = decode_error(&self.path);
        match self.decoder.receive_frame() {
            Ok(frame) => return self.take(frame),
            Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                if self.draining {
                    self.finished = true;
                    return Ok(());
                }
            }
            Err(e) => return Err(err(e.to_string())),
        }
        match self.input.read_packet().map_err(|e| err(e.to_string()))? {
            Some(packet) if packet.stream_index as usize == self.stream => {
                self.decoder
                    .send_packet(Some(&packet))
                    .map_err(|e| err(e.to_string()))?;
            }
            Some(_) => {}
            None if !self.draining => {
                self.draining = true;
                self.decoder
                    .send_packet(None)
                    .map_err(|e| err(e.to_string()))?;
            }
            None => {}
        }
        Ok(())
    }

    /// Resamples a decoded frame onto the end of the buffer, aligning to its
    /// timestamp after a seek.
    fn take(&mut self, frame: AVFrame) -> Result<(), MediaError> {
        let err = decode_error(&self.path);
        if self.resampler.is_none() {
            let mut stereo = unsafe { std::mem::zeroed::<ffi::AVChannelLayout>() };
            unsafe { ffi::av_channel_layout_default(&mut stereo, CHANNELS as i32) };
            let mut swr = SwrContext::new(
                &stereo,
                ffi::AV_SAMPLE_FMT_FLT,
                self.rate as i32,
                &frame.ch_layout,
                frame.format,
                frame.sample_rate,
            )
            .map_err(|e| err(e.to_string()))?;
            swr.init().map_err(|e| err(e.to_string()))?;
            self.resampler = Some(swr);
            if let Some(target) = self.skip_to {
                // Where this frame starts, in output samples from source time 0.
                let pts = frame.best_effort_timestamp;
                let seconds = self.tick.as_f64() * pts as f64 - self.origin.as_f64();
                self.position = (seconds * self.rate as f64).round() as i64;
                if self.position > target {
                    // The stream starts after the target: lead in with silence.
                    let gap = (self.position - target) as usize;
                    self.buffered
                        .extend(std::iter::repeat_n(0.0, gap * CHANNELS));
                    self.position = target;
                }
            }
        }
        let swr = self.resampler.as_mut().expect("just set");
        let capacity = swr.get_out_samples(frame.nb_samples).max(0) as usize;
        let mut out = vec![0f32; capacity * CHANNELS];
        let mut out_ptr = out.as_mut_ptr() as *mut u8;
        let written = unsafe {
            swr.convert(
                &mut out_ptr,
                capacity as i32,
                frame.extended_data as *const *const u8,
                frame.nb_samples,
            )
        }
        .map_err(|e| err(e.to_string()))? as usize;
        out.truncate(written * CHANNELS);
        self.buffered.extend(out);
        if let Some(target) = self.skip_to {
            let ahead = target - self.position;
            let drop = (ahead.max(0) as usize).min(self.buffered.len() / CHANNELS);
            self.buffered.drain(..drop * CHANNELS);
            self.position += drop as i64;
            if self.position >= target {
                self.skip_to = None;
            }
        }
        Ok(())
    }
}
