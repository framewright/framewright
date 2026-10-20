//! Editing copies (design §12): an intra-frame ProRes Proxy version of a
//! long-GOP clip, so the viewer reaches any frame by decoding only that one.
//!
//! A copy is a folder of chunk files, each a complete `.mov` holding a fixed
//! number of the original's frames in order. Chunks are written front to
//! back, each under a temporary name and renamed when complete, so a chunk
//! that exists is whole and can be used while later ones are still encoding.
//!
//! Frames are found by index, never by timestamp: frame `i` of the original
//! is frame `i % chunk_frames` of chunk `i / chunk_frames`. So a copy shows
//! exactly the original's frame at every time, constant rate or not, however
//! the container stores timestamps.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rsmpeg::avcodec::{AVCodec, AVCodecContext};
use rsmpeg::avformat::AVFormatContextOutput;
use rsmpeg::avutil::AVFrame;
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;

use fw_core::ContentHash;

use crate::MediaError;
use crate::video::{VideoInfo, open_decoder, open_input};

/// How copies are made. Its name is part of where they're stored, so a
/// changed recipe gets a new name and old copies are never misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyProfile {
    pub name: &'static str,
    /// Copies fit within this size, keeping the aspect ratio; smaller
    /// sources keep their size.
    pub max_width: u32,
    pub max_height: u32,
    /// Frames per chunk file.
    pub chunk_frames: u64,
    /// FFmpeg's ProRes profile number: 0 Proxy, 1 LT, 2 Standard, 3 HQ.
    pub prores_profile: i32,
}

/// ProRes Proxy, 10-bit 4:2:2, at most 1920×1080, by FFmpeg's `prores`
/// encoder.
pub const PRORES_PROXY_1080: CopyProfile = CopyProfile {
    name: "prores-proxy-1080-v1",
    max_width: 1920,
    max_height: 1080,
    chunk_frames: 240,
    prores_profile: ffi::AV_PROFILE_PRORES_PROXY as i32,
};

impl CopyProfile {
    /// The size of a copy of a `width` × `height` source: within the
    /// profile's limit, aspect ratio kept, even (4:2:2 halves the width).
    pub fn size_for(&self, width: u32, height: u32) -> (u32, u32) {
        let scale = (self.max_width as f64 / width as f64)
            .min(self.max_height as f64 / height as f64)
            .min(1.0);
        let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
        (even(width as f64 * scale), even(height as f64 * scale))
    }

    /// Whether a copy would help: the source is long-GOP (not every frame is
    /// a keyframe) or larger than the profile. Sources with alpha get none,
    /// as ProRes Proxy has no alpha.
    pub fn helps(&self, info: &VideoInfo) -> bool {
        let long_gop = (info.keyframes.len() as u64) < info.frames.count();
        let larger = info.width > self.max_width || info.height > self.max_height;
        !info.has_alpha && (long_gop || larger)
    }

    pub fn chunk_count(&self, info: &VideoInfo) -> u64 {
        info.frames.count().div_ceil(self.chunk_frames)
    }

    /// Chunk `k`'s file in a copy's folder.
    pub fn chunk_path(dir: &Path, k: u64) -> PathBuf {
        dir.join(format!("chunk-{k:06}.mov"))
    }
}

fn encode_error(path: &Path) -> impl Fn(String) -> MediaError + use<> {
    let path = path.to_owned();
    move |message| MediaError::Encode {
        path: path.clone(),
        message,
    }
}

/// Writes the copy of `source` into `dir`, front to back, with `threads`
/// threads each for decoding and encoding. Chunks already there are kept.
/// `done(k)` is called as chunk `k` lands. Returns early, without error,
/// when `stop` is set.
///
/// Each frame's timestamp is checked against the one the probe found for
/// its index, so a chunk only lands when every frame in it is the right one.
/// If the decoder skips or loses a frame, this fails, and chunks already
/// landed stay correct.
///
/// TODO: when resuming, seek to the first missing chunk instead of decoding
/// from the start.
pub fn write_copy(
    source: &Path,
    info: &VideoInfo,
    profile: &CopyProfile,
    dir: &Path,
    threads: usize,
    stop: &AtomicBool,
    done: impl FnMut(u64),
) -> Result<(), MediaError> {
    let mut chunk = None;
    let result = encode(source, info, profile, dir, threads, stop, done, &mut chunk);
    // A chunk still open didn't finish: it never lands.
    if let Some(c) = chunk {
        let _ = fs::remove_file(&c.part);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn encode(
    source: &Path,
    info: &VideoInfo,
    profile: &CopyProfile,
    dir: &Path,
    threads: usize,
    stop: &AtomicBool,
    mut done: impl FnMut(u64),
    chunk: &mut Option<ChunkWriter>,
) -> Result<(), MediaError> {
    let err = encode_error(dir);
    fs::create_dir_all(dir).map_err(|e| err(e.to_string()))?;
    let (mut input, stream) = open_input(source)?;
    let (par, time_base) = {
        let s = &input.streams()[stream];
        (s.codecpar().clone(), s.time_base)
    };
    let mut decoder = open_decoder(source, &par, Some(threads), false, None)?;
    let (width, height) = profile.size_for(info.width, info.height);
    let mut scaler: Option<Scaler> = None;
    let mut index = 0u64;
    let mut draining = false;
    loop {
        let frame =
            match decoder.receive_frame() {
                Ok(frame) => frame,
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    if draining {
                        break;
                    }
                    match input.read_packet().map_err(|e| err(e.to_string()))? {
                        Some(p) if p.stream_index as usize == stream => decoder
                            .send_packet(Some(&p))
                            .map_err(|e| err(e.to_string()))?,
                        Some(_) => {}
                        None => {
                            draining = true;
                            decoder.send_packet(None).map_err(|e| err(e.to_string()))?;
                        }
                    }
                    continue;
                }
                Err(e) => return Err(err(e.to_string())),
            };
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let expected = expected_pts(info, index);
        if expected != Some(frame.best_effort_timestamp) {
            return Err(err(format!(
                "frame {index} has timestamp {}, the probe found {expected:?}",
                frame.best_effort_timestamp
            )));
        }
        let k = index / profile.chunk_frames;
        if index.is_multiple_of(profile.chunk_frames) {
            if let Some(c) = chunk.take() {
                c.finish()?;
                done(k - 1);
            }
            let path = CopyProfile::chunk_path(dir, k);
            if !path.exists() {
                *chunk = Some(ChunkWriter::start(
                    &path,
                    &frame,
                    (width, height),
                    time_base,
                    profile.prores_profile,
                    threads,
                )?);
            }
        }
        index += 1;
        let Some(writer) = chunk.as_mut() else {
            continue;
        };
        if scaler.as_ref().is_none_or(|s| s.format != frame.format) {
            scaler = Some(
                Scaler::new(&frame, width, height)
                    .ok_or_else(|| err("cannot convert this pixel format for a copy".into()))?,
            );
        }
        let mut out = AVFrame::new();
        out.set_width(width as i32);
        out.set_height(height as i32);
        out.set_format(ffi::AV_PIX_FMT_YUV422P10LE);
        out.alloc_buffer().map_err(|e| err(e.to_string()))?;
        let scaler = scaler.as_mut().expect("just set");
        scaler
            .context
            .scale_frame(&frame, 0, frame.height, &mut out)
            .map_err(|e| err(e.to_string()))?;
        out.set_pts(frame.best_effort_timestamp);
        // MOV gives a sample with no duration no length, and drops it.
        unsafe { (*out.as_mut_ptr()).duration = duration_of(info, index - 1)? };
        writer.write(&out)?;
    }
    // A short stream would leave the last chunk without its last frames.
    if index != info.frames.count() {
        return Err(err(format!(
            "decoded {index} frames, the probe found {}",
            info.frames.count()
        )));
    }
    if let Some(c) = chunk.take() {
        c.finish()?;
        done((index - 1) / profile.chunk_frames);
    }
    Ok(())
}

/// The timestamp frame `index` has in the source, rounded to the nearest
/// tick as `VideoReader` rounds.
fn expected_pts(info: &VideoInfo, index: u64) -> Option<i64> {
    let at = info.frames.time_of(index)?;
    let per_second = fw_core::Time::from_seconds(1).checked_div(info.tick).ok()?;
    let half_tick = info.tick.checked_div(fw_core::Time::from_seconds(2)).ok()?;
    let ticks = at
        .checked_add(half_tick)
        .ok()?
        .frame_index(per_second)
        .ok()?;
    Some(info.first_pts + ticks)
}

/// How long frame `index` lasts, in stream ticks: until the next frame, or
/// for the last, as long as the one before it.
fn duration_of(info: &VideoInfo, index: u64) -> Result<i64, MediaError> {
    let ticks = |a: u64, b: u64| -> Option<i64> {
        let (a, b) = (info.frames.time_of(a)?, info.frames.time_of(b)?);
        let seconds = b.checked_sub(a).ok()?;
        Some((seconds.as_f64() / info.tick.as_f64()).round() as i64)
    };
    let last = info.frames.count().saturating_sub(1);
    let d = if index < last {
        ticks(index, index + 1)
    } else if index > 0 {
        ticks(index - 1, index)
    } else {
        Some((1.0 / 24.0 / info.tick.as_f64()).round() as i64)
    };
    Ok(d.unwrap_or(1).max(1))
}

/// Converts decoded frames to the copy's size and 10-bit 4:2:2, keeping
/// the source's YUV codes as they are (same matrix, same range).
struct Scaler {
    format: i32,
    context: SwsContext,
}

impl Scaler {
    fn new(frame: &AVFrame, width: u32, height: u32) -> Option<Scaler> {
        let tags = Tags::of(frame);
        let full = tags.range == ffi::AVCOL_RANGE_JPEG;
        unsafe {
            let raw = ffi::sws_alloc_context();
            let context = SwsContext::from_raw(std::ptr::NonNull::new(raw)?);
            let set = |name: &std::ffi::CStr, value: i64| {
                ffi::av_opt_set_int(raw as *mut _, name.as_ptr(), value, 0) >= 0
            };
            // Bilinear, as the GPU upsamples chroma; downscaling widens it to a
            // tent over the footprint.
            let flags = ffi::SWS_BILINEAR | ffi::SWS_ACCURATE_RND | ffi::SWS_BITEXACT;
            // Where chroma sits, in 1/256 of a luma sample. swscale otherwise
            // assumes its own and shifts colour at edges. The copy keeps the
            // source's horizontal siting; 4:2:2 has no vertical subsampling.
            let (mut x, mut y) = (0, 128);
            if !tags.rgb && tags.chroma != ffi::AVCHROMA_LOC_UNSPECIFIED {
                ffi::av_chroma_location_enum_to_pos(&mut x, &mut y, tags.chroma);
            }
            let ok = set(c"srcw", frame.width as i64)
                && set(c"srch", frame.height as i64)
                && set(c"src_format", frame.format as i64)
                && set(c"dstw", width as i64)
                && set(c"dsth", height as i64)
                && set(c"dst_format", ffi::AV_PIX_FMT_YUV422P10LE as i64)
                && set(c"sws_flags", flags as i64)
                && (tags.rgb || set(c"src_h_chr_pos", x as i64) && set(c"src_v_chr_pos", y as i64))
                && set(c"dst_h_chr_pos", tags.copy_x as i64)
                && set(c"dst_v_chr_pos", 0);
            if !ok || ffi::sws_init_context(raw, std::ptr::null_mut(), std::ptr::null_mut()) < 0 {
                return None;
            }
            // swscale reads range from the pixel format alone, so `yuvj` would
            // be squeezed to limited range. Say the output keeps the input's
            // range; RGB sources are converted with the copy's matrix.
            let coefficients = ffi::sws_getCoefficients(match tags.matrix {
                ffi::AVCOL_SPC_BT470BG => ffi::SWS_CS_ITU601 as i32,
                ffi::AVCOL_SPC_BT2020_NCL => ffi::SWS_CS_BT2020 as i32,
                _ => ffi::SWS_CS_ITU709 as i32,
            });
            ffi::sws_setColorspaceDetails(
                raw,
                coefficients,
                full as i32,
                coefficients,
                full as i32,
                0,
                1 << 16,
                1 << 16,
            );
            Some(Scaler {
                format: frame.format,
                context,
            })
        }
    }
}

/// The colour tags a copy carries: the source's, made explicit where the
/// source left them to be guessed, so the copy decodes to the same colours.
struct Tags {
    rgb: bool,
    /// The copy's horizontal chroma position, in 1/256 of a luma sample.
    copy_x: i32,
    matrix: ffi::AVColorSpace,
    range: ffi::AVColorRange,
    primaries: ffi::AVColorPrimaries,
    transfer: ffi::AVColorTransferCharacteristic,
    chroma: ffi::AVChromaLocation,
}

impl Tags {
    fn of(frame: &AVFrame) -> Tags {
        let desc = unsafe { ffi::av_pix_fmt_desc_get(frame.format).as_ref() };
        let rgb = desc.is_some_and(|d| d.flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0);
        let yuvj = desc.is_some_and(|d| {
            unsafe { std::ffi::CStr::from_ptr(d.name) }
                .to_bytes()
                .starts_with(b"yuvj")
        });
        // As `video::encoding_of` guesses for untagged YUV.
        let matrix = match frame.colorspace {
            _ if rgb => ffi::AVCOL_SPC_BT709,
            ffi::AVCOL_SPC_BT470BG | ffi::AVCOL_SPC_SMPTE170M => ffi::AVCOL_SPC_BT470BG,
            ffi::AVCOL_SPC_BT2020_NCL | ffi::AVCOL_SPC_BT2020_CL => ffi::AVCOL_SPC_BT2020_NCL,
            ffi::AVCOL_SPC_BT709 => ffi::AVCOL_SPC_BT709,
            _ if frame.height < 720 => ffi::AVCOL_SPC_BT470BG,
            _ => ffi::AVCOL_SPC_BT709,
        };
        let range = if !rgb && (frame.color_range == ffi::AVCOL_RANGE_JPEG || yuvj) {
            ffi::AVCOL_RANGE_JPEG
        } else {
            ffi::AVCOL_RANGE_MPEG
        };
        // Untagged chroma is left-sited, as `video::encoding_of` assumes.
        let chroma = match frame.chroma_location {
            _ if rgb => ffi::AVCHROMA_LOC_LEFT,
            ffi::AVCHROMA_LOC_UNSPECIFIED => ffi::AVCHROMA_LOC_LEFT,
            loc => loc,
        };
        let centred = matches!(
            chroma,
            ffi::AVCHROMA_LOC_CENTER | ffi::AVCHROMA_LOC_TOP | ffi::AVCHROMA_LOC_BOTTOM
        );
        Tags {
            rgb,
            copy_x: if centred { 128 } else { 0 },
            matrix,
            range,
            primaries: frame.color_primaries,
            transfer: frame.color_trc,
            chroma,
        }
    }
}

/// One chunk being encoded, under a temporary name until it's finished.
struct ChunkWriter {
    output: AVFormatContextOutput,
    encoder: AVCodecContext,
    part: PathBuf,
    path: PathBuf,
    encoder_tb: ffi::AVRational,
}

impl ChunkWriter {
    fn start(
        path: &Path,
        first: &AVFrame,
        (width, height): (u32, u32),
        time_base: ffi::AVRational,
        prores_profile: i32,
        threads: usize,
    ) -> Result<ChunkWriter, MediaError> {
        let err = encode_error(path);
        let part = path.with_extension("part.mov");
        let codec = AVCodec::find_encoder_by_name(c"prores")
            .ok_or_else(|| err("FFmpeg has no prores encoder".into()))?;
        let mut encoder = AVCodecContext::new(&codec);
        encoder.set_width(width as i32);
        encoder.set_height(height as i32);
        encoder.set_pix_fmt(ffi::AV_PIX_FMT_YUV422P10LE);
        encoder.set_time_base(time_base);
        let tags = Tags::of(first);
        let c_part = std::ffi::CString::new(part.as_os_str().as_encoded_bytes())
            .map_err(|_| err("path contains a NUL byte".into()))?;
        let mut output = AVFormatContextOutput::create(&c_part).map_err(|e| err(e.to_string()))?;
        unsafe {
            let c = encoder.as_mut_ptr();
            (*c).profile = prores_profile;
            (*c).colorspace = tags.matrix;
            (*c).color_range = tags.range;
            (*c).color_primaries = tags.primaries;
            (*c).color_trc = tags.transfer;
            (*c).chroma_sample_location = if tags.copy_x == 128 {
                ffi::AVCHROMA_LOC_CENTER
            } else {
                ffi::AVCHROMA_LOC_LEFT
            };
            (*c).thread_count = threads as i32;
            (*c).thread_type = ffi::FF_THREAD_FRAME as i32;
            if (*output.oformat).flags & ffi::AVFMT_GLOBALHEADER as i32 != 0 {
                (*c).flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
            }
        }
        encoder.open(None).map_err(|e| err(e.to_string()))?;
        {
            let mut stream = output.new_stream();
            stream.set_codecpar(encoder.extract_codecpar());
            stream.set_time_base(time_base);
        }
        // No edit list: it would carry the chunk's start time, and frames it
        // doesn't cover are discarded when read. Chunks are read by frame
        // index, so their own times can start at zero.
        let mut options = Some(rsmpeg::avutil::AVDictionary::new(c"use_editlist", c"0", 0));
        output
            .write_header(&mut options)
            .map_err(|e| err(e.to_string()))?;
        Ok(ChunkWriter {
            output,
            encoder,
            part,
            path: path.to_owned(),
            encoder_tb: time_base,
        })
    }

    fn write(&mut self, frame: &AVFrame) -> Result<(), MediaError> {
        self.encoder
            .send_frame(Some(frame))
            .map_err(|e| encode_error(&self.path)(e.to_string()))?;
        self.drain()
    }

    fn drain(&mut self) -> Result<(), MediaError> {
        let err = encode_error(&self.path);
        let stream_tb = self.output.streams()[0].time_base;
        loop {
            match self.encoder.receive_packet() {
                Ok(mut packet) => {
                    packet.set_stream_index(0);
                    packet.rescale_ts(self.encoder_tb, stream_tb);
                    self.output
                        .interleaved_write_frame(&mut packet)
                        .map_err(|e| err(e.to_string()))?;
                }
                Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => {
                    return Ok(());
                }
                Err(e) => return Err(err(e.to_string())),
            }
        }
    }

    fn finish(mut self) -> Result<(), MediaError> {
        let err = encode_error(&self.path);
        self.encoder
            .send_frame(None)
            .map_err(|e| err(e.to_string()))?;
        self.drain()?;
        self.output
            .write_trailer()
            .map_err(|e| err(e.to_string()))?;
        drop(self.output);
        fs::rename(&self.part, &self.path).map_err(|e| err(e.to_string()))
    }
}

/// Editing copies for one project: which chunks exist, and a background
/// worker making the rest, one copy at a time, front to back.
///
/// TODO: make chunks near the playhead first; per-project and per-clip
/// settings once the UI has somewhere to keep them.
pub struct EditingCopies {
    root: PathBuf,
    profile: CopyProfile,
    shared: std::sync::Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Shared {
    state: std::sync::Mutex<State>,
    /// Signalled when a job is queued, a job finishes, or work should stop.
    changed: std::sync::Condvar,
    stop: AtomicBool,
}

#[derive(Default)]
struct State {
    copies: std::collections::HashMap<ContentHash, Copy>,
    queue: std::collections::VecDeque<Job>,
    busy: bool,
}

struct Copy {
    job: Job,
    /// Which chunks are complete on disk.
    ready: Vec<bool>,
    /// Queued or being written.
    pending: bool,
    /// Why making it failed. It isn't tried again this session.
    failed: Option<String>,
}

#[derive(Clone)]
struct Job {
    source: PathBuf,
    fingerprint: ContentHash,
    info: VideoInfo,
    dir: PathBuf,
}

impl EditingCopies {
    /// Copies kept under `root`, in a folder per profile and source.
    pub fn new(root: impl Into<PathBuf>, profile: CopyProfile) -> EditingCopies {
        let shared = std::sync::Arc::new(Shared::default());
        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("editing copies".into())
                .spawn(move || work(&shared, &profile))
                .expect("spawn the editing copy worker")
        };
        EditingCopies {
            root: root.into(),
            profile,
            shared,
            worker: Some(worker),
        }
    }

    pub fn profile(&self) -> &CopyProfile {
        &self.profile
    }

    fn dir(&self, fingerprint: &ContentHash) -> PathBuf {
        self.root
            .join(self.profile.name)
            .join(fingerprint.to_string())
    }

    /// Makes a copy of `source` if it would help and isn't made or queued
    /// already. Chunks left by an earlier session are found and kept.
    pub fn request(&self, source: &Path, fingerprint: ContentHash, info: &VideoInfo) {
        if !self.profile.helps(info) {
            return;
        }
        let mut state = self.shared.lock();
        if state.copies.contains_key(&fingerprint) {
            return;
        }
        let dir = self.dir(&fingerprint);
        let ready: Vec<bool> = (0..self.profile.chunk_count(info))
            .map(|k| CopyProfile::chunk_path(&dir, k).exists())
            .collect();
        let job = Job {
            source: source.to_owned(),
            fingerprint,
            info: info.clone(),
            dir,
        };
        let pending = !ready.iter().all(|r| *r);
        if pending {
            state.queue.push_back(job.clone());
            self.shared.changed.notify_all();
        }
        state.copies.insert(
            fingerprint,
            Copy {
                job,
                ready,
                pending,
                failed: None,
            },
        );
    }

    /// Chunk `k` of the source's copy turned out to be missing or unreadable
    /// (the cache was cleared, say): stop using it, and make it again.
    pub fn invalidate(&self, fingerprint: &ContentHash, k: u64) {
        let mut state = self.shared.lock();
        let State { copies, queue, .. } = &mut *state;
        let Some(copy) = copies.get_mut(fingerprint) else {
            return;
        };
        if let Some(ready) = copy.ready.get_mut(k as usize) {
            *ready = false;
        }
        let _ = fs::remove_file(CopyProfile::chunk_path(&copy.job.dir, k));
        if !copy.pending && copy.failed.is_none() {
            copy.pending = true;
            queue.push_back(copy.job.clone());
            self.shared.changed.notify_all();
        }
    }

    /// Why the source's copy couldn't be made, if it couldn't.
    pub fn failure(&self, fingerprint: &ContentHash) -> Option<String> {
        self.shared.lock().copies.get(fingerprint)?.failed.clone()
    }

    /// Where frame `index` of the source is in its copy, if that chunk is
    /// ready: the chunk file and the frame's index within it.
    pub fn locate(&self, fingerprint: &ContentHash, index: u64) -> Option<(PathBuf, u64)> {
        let state = self.shared.lock();
        let copy = state.copies.get(fingerprint)?;
        let k = index / self.profile.chunk_frames;
        copy.ready
            .get(k as usize)
            .copied()
            .unwrap_or(false)
            .then(|| {
                (
                    CopyProfile::chunk_path(&copy.job.dir, k),
                    index % self.profile.chunk_frames,
                )
            })
    }

    /// Ready chunks and all chunks of the source's copy, if it has one.
    pub fn progress(&self, fingerprint: &ContentHash) -> Option<(usize, usize)> {
        let state = self.shared.lock();
        let copy = state.copies.get(fingerprint)?;
        Some((copy.ready.iter().filter(|r| **r).count(), copy.ready.len()))
    }

    /// Blocks until every requested copy is complete (or has failed).
    pub fn wait(&self) {
        let mut state = self.shared.lock();
        while state.busy || !state.queue.is_empty() {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for EditingCopies {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The worker: takes jobs in order until told to stop. It runs at low
/// priority with half the cores, so editing stays responsive.
fn work(shared: &Shared, profile: &CopyProfile) {
    // Linux gives each thread its own niceness; FFmpeg's threads inherit it.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
    }
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get() / 2)
        .max(1);
    loop {
        let job = {
            let mut state = shared.lock();
            loop {
                if shared.stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(job) = state.queue.pop_front() {
                    state.busy = true;
                    break job;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = write_copy(
            &job.source,
            &job.info,
            profile,
            &job.dir,
            threads,
            &shared.stop,
            |k| {
                let mut state = shared.lock();
                if let Some(copy) = state.copies.get_mut(&job.fingerprint) {
                    copy.ready[k as usize] = true;
                }
            },
        );
        let mut state = shared.lock();
        if let Some(copy) = state.copies.get_mut(&job.fingerprint) {
            copy.pending = false;
            // Chunks that landed are right; the rest of the clip reads the
            // original. TODO: show the failure in the UI.
            copy.failed = result.err().map(|e| e.to_string());
        }
        state.busy = false;
        drop(state);
        shared.changed.notify_all();
    }
}
