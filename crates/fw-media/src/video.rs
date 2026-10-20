//! Video decoding through FFmpeg.
//!
//! Source time 0 is the first frame's presentation time, so a clip's
//! `source_in` of 0 always means "the first frame", whatever the container's
//! own start time.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use fw_compile::{Decoded, FrameTimes, HardwarePicture};
use fw_core::color::Primaries;
use fw_core::planar::{PlanarEncoding, PlanarModel, Plane, Siting, YuvMatrix, YuvRange};
use fw_core::{ColorSpace, PlanarImage, Time, Transfer};
use rsmpeg::avcodec::AVCodecContext;
use rsmpeg::avformat::AVFormatContextInput;
use rsmpeg::avutil::AVFrame;
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;

use crate::MediaError;
use crate::hardware::{HardwareDevice, VulkanFrame};

/// What a video file holds, learned without decoding any frames.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub frames: FrameTimes,
    /// The colour space the stream is tagged with, or the usual assumption
    /// for untagged video (Rec.709 primaries, BT.1886 display gamma).
    pub space: ColorSpace,
    pub has_alpha: bool,
    /// The first frame's timestamp in stream ticks: source time 0.
    pub first_pts: i64,
    /// Timestamps of the keyframes, where decoding can start, in order.
    pub keyframes: Vec<i64>,
    /// Seconds per timestamp tick.
    pub tick: Time,
    /// The codec, profile and pixel format, e.g. `h264 High yuv420p`: what
    /// decides how fast each decoder is.
    pub format: String,
}

impl VideoInfo {
    /// Source time 0, in seconds of the container's clock. Audio from the
    /// same file uses it too, so picture and sound stay together.
    pub fn origin(&self) -> Time {
        self.tick
            .checked_mul(Time::from_seconds(self.first_pts))
            .unwrap_or(Time::ZERO)
    }
}

pub(crate) fn decode_error(path: &Path) -> impl Fn(String) -> MediaError + use<> {
    let path = path.to_owned();
    move |message| MediaError::Decode {
        path: path.clone(),
        message,
    }
}

pub(crate) fn open_input(path: &Path) -> Result<(AVFormatContextInput, usize), MediaError> {
    let err = decode_error(path);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| err("path contains a NUL byte".into()))?;
    let input = AVFormatContextInput::open(&c_path).map_err(|e| err(e.to_string()))?;
    let (stream, _) = input
        .find_best_stream(ffi::AVMEDIA_TYPE_VIDEO)
        .map_err(|e| err(e.to_string()))?
        .ok_or_else(|| err("no video stream".into()))?;
    Ok((input, stream))
}

/// A decoder for the stream `par` describes, using `threads` threads, or as
/// many as there are cores. Threads split frames and slices; for `intra`
/// streams (every frame decodes alone) only slices, because each frame
/// thread delays the first frame after a seek: about 10 ms against under 1
/// for a ProRes Proxy frame.
///
/// With `hardware`, FFmpeg's Vulkan decoder runs on that device where it can
/// (one thread: more only delay the first frame after a seek), falling back
/// to software for streams it can't decode.
pub(crate) fn open_decoder(
    path: &Path,
    par: &rsmpeg::avcodec::AVCodecParameters,
    threads: Option<usize>,
    intra: bool,
    hardware: Option<&HardwareDevice>,
) -> Result<AVCodecContext, MediaError> {
    let err = decode_error(path);
    let codec = rsmpeg::avcodec::AVCodec::find_decoder(par.codec_id)
        .ok_or_else(|| err("no decoder for this codec".into()))?;
    let mut decoder = AVCodecContext::new(&codec);
    decoder
        .apply_codecpar(par)
        .map_err(|e| err(e.to_string()))?;
    let threads = if hardware.is_some() { Some(1) } else { threads };
    if let Some(device) = hardware {
        // SAFETY: the decoder takes its own reference to the context.
        unsafe {
            let c = decoder.as_mut_ptr();
            (*c).hw_device_ctx = ffi::av_buffer_ref(device.context().as_ptr());
            (*c).get_format = Some(crate::hardware::pick_vulkan);
        }
    }
    let count = threads.map_or("auto".to_owned(), |n| n.to_string());
    let count = std::ffi::CString::new(count).expect("no NUL in a number");
    let mut options = rsmpeg::avutil::AVDictionary::new(c"threads", &count, 0);
    if intra {
        options = options.set(c"thread_type", c"slice", 0);
    }
    decoder
        .open(Some(options))
        .map_err(|e| err(e.to_string()))?;
    Ok(decoder)
}

/// Reads the container's packets (without decoding) to learn every frame's
/// exact presentation time.
///
/// TODO: use the container's index, and remember results by fingerprint, so
/// long files aren't read end to end on every probe.
pub fn probe_video(path: &Path) -> Result<VideoInfo, MediaError> {
    let err = decode_error(path);
    let (mut input, stream) = open_input(path)?;
    let (par, time_base, avg_rate) = {
        let s = &input.streams()[stream];
        (s.codecpar().clone(), s.time_base, s.avg_frame_rate)
    };
    let mut pts = vec![];
    let mut keyframes = vec![];
    while let Some(packet) = input.read_packet().map_err(|e| err(e.to_string()))? {
        if packet.stream_index as usize != stream {
            continue;
        }
        let t = if packet.pts != ffi::AV_NOPTS_VALUE {
            packet.pts
        } else {
            packet.dts
        };
        if t != ffi::AV_NOPTS_VALUE {
            pts.push(t);
            if packet.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0 {
                keyframes.push(t);
            }
        }
    }
    keyframes.sort_unstable();
    pts.sort_unstable();
    pts.dedup();
    let first = *pts.first().ok_or_else(|| err("no frames".into()))?;
    let tick =
        Time::new(time_base.num as i64, time_base.den as i64).map_err(|e| err(e.to_string()))?;
    let relative: Vec<Time> = pts
        .iter()
        .map(|p| tick.checked_mul(Time::from_seconds(p - first)))
        .collect::<Result<_, _>>()
        .map_err(|e| err(e.to_string()))?;
    let declared = Time::new(avg_rate.num as i64, avg_rate.den as i64)
        .ok()
        .filter(|r| *r > Time::ZERO);
    let frames = match declared {
        // Containers round timestamps to their own tick (Matroska: 1 ms), so
        // 24 fps is stored as 0, 42, 83, … ms. If every timestamp is the
        // declared rate's, rounded, the rate is constant and exact.
        Some(rate) if matches_rate(&relative, rate, tick) => FrameTimes::Constant {
            start: Time::ZERO,
            rate,
            count: relative.len() as u64,
        },
        _ if relative.len() == 1 => FrameTimes::Constant {
            start: Time::ZERO,
            rate: declared.unwrap_or(Time::from_seconds(1)),
            count: 1,
        },
        _ => FrameTimes::Variable(relative),
    };
    Ok(VideoInfo {
        width: par.width as u32,
        height: par.height as u32,
        frames,
        space: tagged_space(par.color_primaries, par.color_trc),
        has_alpha: has_alpha(par.format),
        first_pts: first,
        keyframes,
        tick,
        format: format_name(&par),
    })
}

fn format_name(par: &rsmpeg::avcodec::AVCodecParameters) -> String {
    let name = |p: *const std::ffi::c_char| {
        (!p.is_null()).then(|| {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        })
    };
    // SAFETY: FFmpeg returns static strings, or null for unknown values.
    let (codec, profile, pixels) = unsafe {
        (
            name(ffi::avcodec_get_name(par.codec_id)),
            name(ffi::avcodec_profile_name(par.codec_id, par.profile)),
            name(ffi::av_get_pix_fmt_name(par.format)),
        )
    };
    [codec, profile, pixels]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether frame `i` sits at `i / rate`, to within half a timestamp tick.
fn matches_rate(times: &[Time], rate: Time, tick: Time) -> bool {
    let half_tick = tick.as_f64() / 2.0 + 1e-12;
    times
        .iter()
        .enumerate()
        .all(|(i, t)| match Time::of_frame(i as i64, rate) {
            Ok(exact) => (t.as_f64() - exact.as_f64()).abs() <= half_tick,
            Err(_) => false,
        })
}

fn is_rgb(format: i32) -> bool {
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format) };
    !desc.is_null() && unsafe { (*desc).flags } & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0
}

fn depth(format: i32) -> i32 {
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format) };
    if desc.is_null() {
        8
    } else {
        unsafe { (*desc).comp[0].depth }
    }
}

fn has_alpha(format: i32) -> bool {
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format) };
    !desc.is_null() && unsafe { (*desc).flags } & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0
}

/// Untagged video is assumed to be Rec.709 shown on a BT.1886 display.
fn tagged_space(
    primaries: ffi::AVColorPrimaries,
    trc: ffi::AVColorTransferCharacteristic,
) -> ColorSpace {
    let primaries = match primaries {
        ffi::AVCOL_PRI_BT2020 => Primaries::Rec2020,
        ffi::AVCOL_PRI_SMPTE432 => Primaries::P3D65,
        _ => Primaries::Rec709,
    };
    let transfer = match trc {
        ffi::AVCOL_TRC_IEC61966_2_1 => Transfer::Srgb,
        ffi::AVCOL_TRC_LINEAR => Transfer::Linear,
        // TODO: PQ and HLG once the colour core has them.
        _ => Transfer::Gamma24,
    };
    ColorSpace::new(primaries, transfer)
}

/// Decodes frames of one file, continuing forward when it can and seeking
/// when it must.
pub struct VideoReader {
    path: PathBuf,
    input: AVFormatContextInput,
    decoder: AVCodecContext,
    stream: usize,
    /// Seconds per timestamp tick.
    tick: Time,
    first_pts: i64,
    keyframes: Vec<i64>,
    /// The most recently decoded frame, kept so asking for it again is free.
    held: Option<AVFrame>,
    /// The demuxer has run out and the decoder has been told to drain.
    draining: bool,
    scaler: Option<(i32, SwsContext)>,
    /// The device a hardware decoder runs on, if this reader has one.
    hardware: Option<Arc<HardwareDevice>>,
}

/// Without keyframe timestamps, decoding forward this far (in seconds) is
/// assumed cheaper than seeking.
const FORWARD_LIMIT: f64 = 2.0;

/// What seeking costs (flushing and refilling the decoder's threads), as
/// seconds of decoding forward.
const SEEK_COST: f64 = 0.25;

impl VideoReader {
    pub fn open(path: &Path) -> Result<VideoReader, MediaError> {
        VideoReader::open_probed(path, &probe_video(path)?)
    }

    /// Opens a file already probed, without reading it again.
    pub fn open_probed(path: &Path, info: &VideoInfo) -> Result<VideoReader, MediaError> {
        VideoReader::open_with(path, info, false, None)
    }

    /// Opens an intra-frame file already probed (such as an editing copy),
    /// decoding for the quickest single frames rather than throughput.
    pub fn open_intra(path: &Path, info: &VideoInfo) -> Result<VideoReader, MediaError> {
        VideoReader::open_with(path, info, true, None)
    }

    /// Opens a file already probed, decoding on `device` where it can. Read
    /// it with [`VideoReader::frame_decoded`].
    pub fn open_hardware(
        path: &Path,
        info: &VideoInfo,
        device: Arc<HardwareDevice>,
    ) -> Result<VideoReader, MediaError> {
        VideoReader::open_with(path, info, false, Some(device))
    }

    fn open_with(
        path: &Path,
        info: &VideoInfo,
        intra: bool,
        hardware: Option<Arc<HardwareDevice>>,
    ) -> Result<VideoReader, MediaError> {
        let err = decode_error(path);
        let first_pts = info.first_pts;
        let (input, stream) = open_input(path)?;
        let (par, time_base) = {
            let s = &input.streams()[stream];
            (s.codecpar().clone(), s.time_base)
        };
        let decoder = open_decoder(path, &par, None, intra, hardware.as_deref())?;
        Ok(VideoReader {
            path: path.to_owned(),
            input,
            decoder,
            stream,
            tick: Time::new(time_base.num as i64, time_base.den as i64)
                .map_err(|e| err(e.to_string()))?,
            first_pts,
            keyframes: info.keyframes.clone(),
            held: None,
            draining: false,
            scaler: None,
            hardware,
        })
    }

    /// The frame starting at source time `at` (a time from the frame index),
    /// as decoded planes; [`PlanarImage::to_rgba`] makes straight-alpha RGBA
    /// in the stream's own encoding.
    pub fn frame(&mut self, at: Time) -> Result<PlanarImage, MediaError> {
        self.reach(at)?;
        self.picture_held()
    }

    /// The frame starting at source time `at`: planes, or a hardware
    /// decoder's image on the GPU.
    pub fn frame_decoded(&mut self, at: Time) -> Result<Decoded, MediaError> {
        self.reach(at)?;
        let frame = self.held.as_ref().expect("a frame is held");
        match &self.hardware {
            Some(device) if frame.format == ffi::AV_PIX_FMT_VULKAN => {
                hardware_picture(&self.path, frame, device.clone()).map(Decoded::Hardware)
            }
            _ => self
                .picture_held()
                .map(|p| Decoded::Cpu(fw_core::DecodedImage::Planar(p))),
        }
    }

    /// Waits until the GPU has finished decoding the frame held, if it was
    /// decoded in hardware. Hardware decoding runs ahead of the CPU.
    pub fn wait_for_gpu(&self) -> Result<(), MediaError> {
        match (&self.hardware, &self.held) {
            (Some(device), Some(frame)) if frame.format == ffi::AV_PIX_FMT_VULKAN => device
                .wait_decoded(frame)
                .map_err(|e| decode_error(&self.path)(e)),
            _ => Ok(()),
        }
    }

    /// Decodes until the frame starting at `at` is held.
    fn reach(&mut self, at: Time) -> Result<(), MediaError> {
        let err = decode_error(&self.path);
        let target = self.target(at)?;
        let current = self.held.as_ref().map(|f| f.best_effort_timestamp);
        if current == Some(target) {
            return Ok(());
        }
        if self.must_seek(target) {
            self.seek(target)?;
        }
        loop {
            let Some(frame) = self.next_frame()? else {
                // Past the end: the last frame holds.
                return match self.held {
                    Some(_) => Ok(()),
                    None => Err(err("no frame decoded".into())),
                };
            };
            let pts = frame.best_effort_timestamp;
            self.held = Some(frame);
            if pts >= target {
                return Ok(());
            }
        }
    }

    /// How far reaching the frame at `at` would take, in timestamp ticks:
    /// 0 when it's the frame held, `None` when it means seeking.
    pub fn distance_to(&self, at: Time) -> Option<i64> {
        let target = self.target(at).ok()?;
        let current = self.held.as_ref()?.best_effort_timestamp;
        (!self.must_seek(target)).then(|| target - current)
    }

    /// The timestamp of the frame at source time `at`, to the nearest tick,
    /// as muxers round when they store timestamps.
    fn target(&self, at: Time) -> Result<i64, MediaError> {
        let err = decode_error(&self.path);
        let per_second = Time::from_seconds(1)
            .checked_div(self.tick)
            .map_err(|e| err(e.to_string()))?;
        let half_tick = self
            .tick
            .checked_div(Time::from_seconds(2))
            .map_err(|e| err(e.to_string()))?;
        let ticks = at
            .checked_add(half_tick)
            .and_then(|t| t.frame_index(per_second))
            .map_err(|e| err(e.to_string()))?;
        Ok(self.first_pts + ticks)
    }

    /// Whether seeking reaches `target` sooner than decoding on from the
    /// frame held (or from the start, when nothing has been decoded).
    fn must_seek(&self, target: i64) -> bool {
        let from = match self.held.as_ref() {
            Some(frame) => frame.best_effort_timestamp,
            None => self.first_pts,
        };
        if target < from {
            return true;
        }
        let ticks_per = |seconds: f64| (seconds / self.tick.as_f64()).round() as i64;
        // The last keyframe at or before the target, where a seek lands.
        let landing = self.keyframes.partition_point(|&k| k <= target);
        match landing.checked_sub(1).map(|i| self.keyframes[i]) {
            Some(key) => key > from && target - from > target - key + ticks_per(SEEK_COST),
            None if self.keyframes.is_empty() => target - from > ticks_per(FORWARD_LIMIT),
            None => false,
        }
    }

    fn seek(&mut self, target: i64) -> Result<(), MediaError> {
        let err = decode_error(&self.path);
        self.input
            .seek(self.stream as i32, target, ffi::AVSEEK_FLAG_BACKWARD as i32)
            .map_err(|e| err(e.to_string()))?;
        self.decoder.flush_buffers();
        self.held = None;
        self.draining = false;
        Ok(())
    }

    /// The next frame in presentation order, or `None` at the end.
    fn next_frame(&mut self) -> Result<Option<AVFrame>, MediaError> {
        let err = decode_error(&self.path);
        loop {
            match self.decoder.receive_frame() {
                Ok(frame) => return Ok(Some(frame)),
                Err(RsmpegError::DecoderDrainError) | Err(RsmpegError::DecoderFlushedError) => {
                    if self.draining {
                        return Ok(None);
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
                None => {
                    if !self.draining {
                        self.draining = true;
                        self.decoder
                            .send_packet(None)
                            .map_err(|e| err(e.to_string()))?;
                    }
                }
            }
        }
    }

    /// The held frame's planes, as decoded.
    ///
    /// Planar formats are copied as they are. Anything else (packed RGB,
    /// big-endian, …) is unpacked by swscale to 16-bit 4:4:4 YUV or planar
    /// RGB. swscale widens by shifting, which the encoding's `shift` records,
    /// except alpha, whose bits it repeats (128 → 32896) so all 16 are used.
    fn picture_held(&mut self) -> Result<PlanarImage, MediaError> {
        let err = decode_error(&self.path);
        let frame = self.held.as_ref().expect("a frame is held");
        let (w, h, format) = (frame.width, frame.height, frame.format);
        if format == ffi::AV_PIX_FMT_VULKAN {
            return Err(err("decoded on the GPU; read it with frame_decoded".into()));
        }
        if let Some(layout) = native_layout(format) {
            let encoding = encoding_of(frame, &layout, 0);
            let bytes = encoding.bytes_per_sample() as usize;
            let planes = layout
                .planes
                .iter()
                .enumerate()
                .map(|(c, &p)| {
                    let (sx, sy) = if layout.chroma(c) {
                        encoding.chroma_shift
                    } else {
                        (0, 0)
                    };
                    // Rounding up, as FFmpeg sizes chroma planes.
                    let (pw, ph) = ((w + (1 << sx) - 1) >> sx, (h + (1 << sy) - 1) >> sy);
                    let row = pw as usize * bytes;
                    let mut data = Vec::with_capacity(row * ph as usize);
                    for y in 0..ph as isize {
                        // SAFETY: FFmpeg gives each plane `ph` rows of at least
                        // `row` bytes, `linesize` apart.
                        data.extend_from_slice(unsafe {
                            std::slice::from_raw_parts(
                                frame.data[p].offset(y * frame.linesize[p] as isize),
                                row,
                            )
                        });
                    }
                    Plane {
                        width: pw as u32,
                        height: ph as u32,
                        data,
                    }
                })
                .collect();
            return Ok(PlanarImage {
                width: w as u32,
                height: h as u32,
                encoding,
                planes,
            });
        }
        let rgb_source = is_rgb(format);
        let out_format = if rgb_source {
            ffi::AV_PIX_FMT_GBRAP16LE
        } else {
            ffi::AV_PIX_FMT_YUVA444P16LE
        };
        if self.scaler.as_ref().is_none_or(|(f, _)| *f != format) {
            // BITEXACT keeps the unpacking identical on every CPU.
            let flags = ffi::SWS_BICUBIC
                | ffi::SWS_ACCURATE_RND
                | ffi::SWS_FULL_CHR_H_INT
                | ffi::SWS_BITEXACT;
            let scaler =
                SwsContext::get_context(w, h, format, w, h, out_format, flags, None, None, None)
                    .ok_or_else(|| err("cannot unpack this pixel format".into()))?;
            self.scaler = Some((format, scaler));
        }
        let (_, scaler) = self.scaler.as_mut().expect("just set");
        let plane = w as usize * h as usize * 2;
        let mut planes = vec![vec![0u8; plane]; 4];
        let dst = [0, 1, 2, 3].map(|i| planes[i].as_mut_ptr());
        let stride = [w * 2; 4];
        unsafe {
            scaler
                .scale(
                    frame.data.as_ptr() as _,
                    frame.linesize.as_ptr(),
                    0,
                    h,
                    dst.as_ptr(),
                    stride.as_ptr(),
                )
                .map_err(|e| err(e.to_string()))?;
        }
        if rgb_source {
            // GBRAP to R, G, B, A.
            planes.swap(0, 2);
            planes.swap(1, 2);
        }
        let bits = depth(format).clamp(1, 16) as u32;
        let layout = NativeLayout {
            planes: vec![0, 1, 2, 3],
            rgb: rgb_source,
            bits: 16,
            chroma_shift: (0, 0),
            full_range_format: false,
        };
        let mut encoding = encoding_of(frame, &layout, 16 - bits);
        encoding.bits = bits;
        Ok(PlanarImage {
            width: w as u32,
            height: h as u32,
            encoding,
            planes: planes
                .into_iter()
                .map(|data| Plane {
                    width: w as u32,
                    height: h as u32,
                    data,
                })
                .collect(),
        })
    }
}

/// A hardware decoder's frame as a picture the renderer reads in place:
/// NV12, or P010 (10 bits in the top of 16).
fn hardware_picture(
    path: &Path,
    frame: &AVFrame,
    device: Arc<HardwareDevice>,
) -> Result<HardwarePicture, MediaError> {
    let err = decode_error(path);
    // SAFETY: a hardware frame always has its frames context.
    let frames = unsafe { &*((*frame.hw_frames_ctx).data as *const ffi::AVHWFramesContext) };
    let (bits, shift) = match frames.sw_format {
        ffi::AV_PIX_FMT_NV12 => (8, 0),
        ffi::AV_PIX_FMT_P010LE => (10, 6),
        other => {
            return Err(err(format!(
                "hardware decoding gave pixel format {other}, which isn't read on the GPU"
            )));
        }
    };
    let layout = NativeLayout {
        planes: vec![0, 1, 2],
        rgb: false,
        bits,
        chroma_shift: (1, 1),
        full_range_format: false,
    };
    Ok(HardwarePicture {
        width: frame.width as u32,
        height: frame.height as u32,
        coded: (frames.width as u32, frames.height as u32),
        encoding: encoding_of(frame, &layout, shift),
        frame: Arc::new(VulkanFrame::new(frame, device)),
    })
}

/// A pixel format the GPU can take as it is: one plane per component, codes
/// in the low bits of little-endian 8- or 16-bit samples.
struct NativeLayout {
    /// The FFmpeg plane holding each of Y, Cb, Cr (R, G, B) and alpha.
    planes: Vec<usize>,
    rgb: bool,
    bits: u32,
    chroma_shift: (u32, u32),
    /// The deprecated `yuvj` formats, which mean full range whatever the tag.
    full_range_format: bool,
}

impl NativeLayout {
    fn chroma(&self, component: usize) -> bool {
        !self.rgb && (component == 1 || component == 2)
    }
}

fn native_layout(format: i32) -> Option<NativeLayout> {
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format).as_ref()? };
    let flags = desc.flags;
    let rejected = (ffi::AV_PIX_FMT_FLAG_BE
        | ffi::AV_PIX_FMT_FLAG_PAL
        | ffi::AV_PIX_FMT_FLAG_HWACCEL
        | ffi::AV_PIX_FMT_FLAG_BITSTREAM
        | ffi::AV_PIX_FMT_FLAG_FLOAT
        | ffi::AV_PIX_FMT_FLAG_BAYER) as u64;
    let alpha = flags & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0;
    let count = desc.nb_components as usize;
    if flags & ffi::AV_PIX_FMT_FLAG_PLANAR as u64 == 0
        || flags & rejected != 0
        || count != if alpha { 4 } else { 3 }
    {
        return None;
    }
    let comps = &desc.comp[..count];
    let bits = comps[0].depth as u32;
    let step = if bits <= 8 { 1 } else { 2 };
    let mut planes: Vec<usize> = comps.iter().map(|c| c.plane as usize).collect();
    let uniform = comps
        .iter()
        .all(|c| c.depth as u32 == bits && c.step == step && c.shift == 0 && c.offset == 0);
    let mut distinct = planes.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if !uniform || distinct.len() != count || bits > 16 {
        return None;
    }
    let rgb = flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0;
    if rgb && (desc.log2_chroma_w, desc.log2_chroma_h) != (0, 0) {
        return None;
    }
    planes.truncate(count);
    let name = unsafe { std::ffi::CStr::from_ptr(desc.name) };
    Some(NativeLayout {
        planes,
        rgb,
        bits,
        chroma_shift: (desc.log2_chroma_w as u32, desc.log2_chroma_h as u32),
        full_range_format: name.to_bytes().starts_with(b"yuvj"),
    })
}

/// How the frame's codes become colour, from its tags.
fn encoding_of(frame: &AVFrame, layout: &NativeLayout, shift: u32) -> PlanarEncoding {
    let model = if layout.rgb {
        PlanarModel::Rgb
    } else {
        let matrix = match frame.colorspace {
            ffi::AVCOL_SPC_BT470BG | ffi::AVCOL_SPC_SMPTE170M => YuvMatrix::Bt601,
            ffi::AVCOL_SPC_BT2020_NCL | ffi::AVCOL_SPC_BT2020_CL => YuvMatrix::Bt2020,
            ffi::AVCOL_SPC_BT709 => YuvMatrix::Bt709,
            // Untagged: standard definition is usually Rec.601, HD Rec.709.
            _ if frame.height < 720 => YuvMatrix::Bt601,
            _ => YuvMatrix::Bt709,
        };
        let range = if frame.color_range == ffi::AVCOL_RANGE_JPEG || layout.full_range_format {
            YuvRange::Full
        } else {
            YuvRange::Limited
        };
        PlanarModel::Yuv { matrix, range }
    };
    let siting = match frame.chroma_location {
        ffi::AVCHROMA_LOC_CENTER => (Siting::Centred, Siting::Centred),
        ffi::AVCHROMA_LOC_TOPLEFT => (Siting::Cosited, Siting::Cosited),
        ffi::AVCHROMA_LOC_TOP => (Siting::Centred, Siting::Cosited),
        // TODO: bottom siting (rare); treated as centred vertically.
        ffi::AVCHROMA_LOC_BOTTOM => (Siting::Centred, Siting::Centred),
        // Left, bottom-left and untagged: the MPEG-2 / H.264 / HEVC default.
        _ => (Siting::Cosited, Siting::Centred),
    };
    PlanarEncoding {
        model,
        bits: layout.bits,
        shift,
        chroma_shift: layout.chroma_shift,
        siting,
        alpha: layout.planes.len() == 4,
    }
}
