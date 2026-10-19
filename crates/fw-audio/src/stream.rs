//! The streaming thread: decodes each voice ahead into its ring buffer.
//!
//! Audio is streamed, never decoded whole into memory (design §12). Readers
//! are created on this thread because FFmpeg's resampler isn't `Send`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fw_core::Time;
use fw_media::AudioReader;

use crate::CHANNELS;

/// Frames decoded per step.
const CHUNK: usize = 2048;

pub(crate) struct Job {
    pub path: PathBuf,
    pub origin: Option<Time>,
    pub rate: u32,
    /// The source sample to start streaming from.
    pub source_from: i64,
    /// How many frames to stream before stopping.
    pub frames: i64,
    pub ring: rtrb::Producer<f32>,
}

pub(crate) enum Message {
    /// Replace every job; reply once each ring has its first fill.
    Jobs(Vec<Job>, Sender<()>),
    Quit,
}

struct Running {
    job: Job,
    reader: Option<AudioReader>,
    failed: bool,
}

pub(crate) fn run(messages: Receiver<Message>, errors: Arc<Mutex<Vec<String>>>) {
    let mut running: Vec<Running> = vec![];
    let mut buffer = vec![0f32; CHUNK * CHANNELS];
    loop {
        // Fill every ring as far as it goes, then wait a little for news.
        let mut ack = None;
        match messages.recv_timeout(Duration::from_millis(2)) {
            Ok(Message::Jobs(jobs, reply)) => {
                running = jobs
                    .into_iter()
                    .map(|job| Running {
                        job,
                        reader: None,
                        failed: false,
                    })
                    .collect();
                ack = Some(reply);
            }
            Ok(Message::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        for r in &mut running {
            fill(r, &mut buffer, &errors);
        }
        if let Some(reply) = ack {
            let _ = reply.send(());
        }
    }
}

fn fill(r: &mut Running, buffer: &mut [f32], errors: &Mutex<Vec<String>>) {
    let job = &mut r.job;
    if r.reader.is_none() && !r.failed {
        let opened = AudioReader::open(&job.path, job.rate, job.origin).and_then(|mut reader| {
            reader.seek(job.source_from)?;
            Ok(reader)
        });
        match opened {
            Ok(reader) => r.reader = Some(reader),
            Err(e) => {
                r.failed = true;
                errors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(e.to_string());
            }
        }
    }
    while job.frames > 0 && job.ring.slots() >= CHUNK * CHANNELS {
        let frames = (job.frames as usize).min(CHUNK);
        let out = &mut buffer[..frames * CHANNELS];
        let got = match &mut r.reader {
            Some(reader) => reader.read(out).unwrap_or(0),
            None => 0,
        };
        // Past the end of the media (or after an error): silence.
        out[got * CHANNELS..].fill(0.0);
        if job.ring.push_entire_slice(out).is_err() {
            return;
        }
        job.frames -= frames as i64;
    }
}
