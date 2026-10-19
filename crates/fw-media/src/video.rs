//! Video decoding through FFmpeg.
//!
//! Source time 0 is the first frame's presentation time, so a clip's
//! `source_in` of 0 always means "the first frame", whatever the container's
//! own start time.

use std::path::{Path, PathBuf};

use fw_compile::FrameTimes;
use fw_core::color::Primaries;
use fw_core::{ColorSpace, RgbaImage, Time, Transfer};
use rsmpeg::avcodec::AVCodecContext;
use rsmpeg::avformat::AVFormatContextInput;
use rsmpeg::avutil::AVFrame;
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;

use crate::MediaError;

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
}

fn decode_error(path: &Path) -> impl Fn(String) -> MediaError + use<> {
    let path = path.to_owned();
    move |message| MediaError::Decode {
        path: path.clone(),
        message,
    }
}

fn open_input(path: &Path) -> Result<(AVFormatContextInput, usize), MediaError> {
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
        }
    }
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
    })
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
    /// The most recently decoded frame, kept so asking for it again is free.
    held: Option<AVFrame>,
    /// The demuxer has run out and the decoder has been told to drain.
    draining: bool,
    scaler: Option<(i32, SwsContext)>,
}

/// Decoding forward this far (in seconds) is assumed cheaper than seeking.
const FORWARD_LIMIT: i64 = 2;

impl VideoReader {
    pub fn open(path: &Path) -> Result<VideoReader, MediaError> {
        let err = decode_error(path);
        let first_pts = {
            // A cheap demux pass for the first timestamp; probe_video reads them all.
            let (mut input, stream) = open_input(path)?;
            let mut first = i64::MAX;
            let mut seen = 0;
            while let Some(packet) = input.read_packet().map_err(|e| err(e.to_string()))? {
                if packet.stream_index as usize == stream && packet.pts != ffi::AV_NOPTS_VALUE {
                    first = first.min(packet.pts);
                    seen += 1;
                    // B-frames can put the earliest timestamp a few packets in.
                    if seen > 16 {
                        break;
                    }
                }
            }
            if first == i64::MAX { 0 } else { first }
        };
        let (input, stream) = open_input(path)?;
        let (par, time_base) = {
            let s = &input.streams()[stream];
            (s.codecpar().clone(), s.time_base)
        };
        let codec = rsmpeg::avcodec::AVCodec::find_decoder(par.codec_id)
            .ok_or_else(|| err("no decoder for this codec".into()))?;
        let mut decoder = AVCodecContext::new(&codec);
        decoder
            .apply_codecpar(&par)
            .map_err(|e| err(e.to_string()))?;
        decoder.open(None).map_err(|e| err(e.to_string()))?;
        Ok(VideoReader {
            path: path.to_owned(),
            input,
            decoder,
            stream,
            tick: Time::new(time_base.num as i64, time_base.den as i64)
                .map_err(|e| err(e.to_string()))?,
            first_pts,
            held: None,
            draining: false,
            scaler: None,
        })
    }

    /// The frame starting at source time `at` (a time from the frame index),
    /// as straight-alpha RGBA in the stream's own encoding.
    pub fn frame(&mut self, at: Time) -> Result<RgbaImage, MediaError> {
        let err = decode_error(&self.path);
        // To the nearest tick, as muxers round when they store timestamps.
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
        let target = self.first_pts + ticks;
        let forward_limit = FORWARD_LIMIT * self.tick.den() / self.tick.num().max(1);
        let current = self.held.as_ref().map(|f| f.best_effort_timestamp);
        let must_seek = match current {
            Some(p) => target < p || target - p > forward_limit,
            None => target - self.first_pts > forward_limit,
        };
        if current == Some(target) {
            return self.convert_held();
        }
        if must_seek {
            self.seek(target)?;
        }
        loop {
            let Some(frame) = self.next_frame()? else {
                // Past the end: the last frame holds.
                return match self.held {
                    Some(_) => self.convert_held(),
                    None => Err(err("no frame decoded".into())),
                };
            };
            let pts = frame.best_effort_timestamp;
            self.held = Some(frame);
            if pts >= target {
                return self.convert_held();
            }
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

    /// Converts the held frame to straight RGBA in the stream's encoding.
    ///
    /// swscale only unpacks: YUV to 16-bit 4:4:4 YUV, RGB to 16-bit planar
    /// RGB. Both widen by shifting, which is the convention for YUV (16-bit
    /// video black is 16 × 256) but not for full-range values, so the matrix,
    /// range and scaling are applied here with exact constants.
    ///
    /// TODO: do this on the GPU, and honour chroma siting.
    fn convert_held(&mut self) -> Result<RgbaImage, MediaError> {
        let err = decode_error(&self.path);
        let frame = self.held.as_ref().expect("a frame is held");
        let (w, h, format) = (frame.width, frame.height, frame.format);
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
        let plane = w as usize * h as usize;
        let mut planes = vec![0u16; plane * 4];
        let base = planes.as_mut_ptr() as *mut u8;
        let dst = unsafe { [0, 1, 2, 3].map(|i| base.add(i * plane * 2)) };
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
        let bits = depth(format).clamp(1, 16);
        // The widened value of the source's largest code.
        let full_scale = (((1u32 << bits) - 1) << (16 - bits)) as f32;
        // swscale widens alpha by repeating bits (128 → 32896), unlike colour.
        let alpha_scale = 65535.0;
        let [p0, p1, p2, p3] = [0, 1, 2, 3].map(|i| &planes[i * plane..(i + 1) * plane]);
        let mut pixels = Vec::with_capacity(plane * 4);
        if rgb_source {
            // Planes are G, B, R, A.
            for i in 0..plane {
                pixels.extend([p2[i], p0[i], p1[i]].map(|v| v as f32 / full_scale));
                pixels.push(p3[i] as f32 / alpha_scale);
            }
        } else {
            let (kr, kb) = match frame.colorspace {
                ffi::AVCOL_SPC_BT470BG | ffi::AVCOL_SPC_SMPTE170M => (0.299, 0.114),
                ffi::AVCOL_SPC_BT2020_NCL | ffi::AVCOL_SPC_BT2020_CL => (0.2627, 0.0593),
                ffi::AVCOL_SPC_BT709 => (0.2126, 0.0722),
                // Untagged: standard definition is usually Rec.601, HD Rec.709.
                _ if h < 720 => (0.299, 0.114),
                _ => (0.2126, 0.0722),
            };
            let kg = 1.0 - kr - kb;
            let shift = (1u32 << (16 - bits)) as f32;
            let (y_black, y_range, c_range) = if frame.color_range == ffi::AVCOL_RANGE_JPEG {
                (0.0, full_scale, full_scale)
            } else {
                // Limited range in n-bit codes: luma 16–235, chroma 16–240, scaled to n bits.
                let unit = (1u32 << (bits - 8)) as f32 * shift;
                (16.0 * unit, 219.0 * unit, 224.0 * unit)
            };
            let c_mid = 32768.0;
            for i in 0..plane {
                let y = (p0[i] as f32 - y_black) / y_range;
                let cb = (p1[i] as f32 - c_mid) / c_range;
                let cr = (p2[i] as f32 - c_mid) / c_range;
                let r = y + 2.0 * (1.0 - kr) * cr;
                let b = y + 2.0 * (1.0 - kb) * cb;
                let g = (y - kr * r - kb * b) / kg;
                pixels.extend([r, g, b, p3[i] as f32 / alpha_scale]);
            }
        }
        Ok(RgbaImage {
            width: w as u32,
            height: h as u32,
            pixels,
            premultiplied: false,
        })
    }
}
