//! Reading and writing media files.
//!
//! Nothing else in Framewright touches file formats. Images come in as
//! straight-alpha `f32` RGBA with the color space the file declares, and go
//! out from premultiplied engine pixels.
//!
//! TODO: video and audio through a private FFmpeg build (design §12), RAW
//! stills, ICC profiles and PNG color chunks.

mod audio;
mod copies;
mod ffvk;
mod hardware;
mod project;
mod video;

pub use audio::{AudioReader, CHANNELS, has_audio};

pub use video::{VideoInfo, VideoReader, probe_video};

pub use copies::{CopyProfile, EditingCopies, PRORES_PROXY_1080, write_copy};
pub use hardware::{DecoderChoice, GPU_BIAS, HardwareDevice, prefers_hardware};
pub use project::HardwarePolicy;
pub use project::ProjectMedia;

use std::fs::{self, File};
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};

use exr::prelude::{self as exrp, ReadChannels as _, ReadLayers as _, WritableImage as _};
use fw_core::color::Primaries;
use fw_core::{ColorSpace, ContentHash, RgbaImage, Time, Transfer};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: unsupported file type")]
    Unsupported { path: PathBuf },
    #[error("{path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("{path}: {message}")]
    Encode { path: PathBuf, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Exr,
    /// Anything FFmpeg reads as video. Read-only for now.
    Video,
    /// Sound with no picture. Read-only for now.
    Audio,
}

impl Format {
    pub fn of(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Format::Png),
            "jpg" | "jpeg" => Some(Format::Jpeg),
            "exr" => Some(Format::Exr),
            "mov" | "mp4" | "m4v" | "mkv" | "webm" | "mxf" | "avi" | "mts" | "m2ts" => {
                Some(Format::Video)
            }
            "wav" | "flac" | "mp3" | "aac" | "m4a" | "ogg" | "opus" | "aif" | "aiff" => {
                Some(Format::Audio)
            }
            _ => None,
        }
    }

    /// The color space a file of this type is written in by default.
    pub fn natural_space(self) -> ColorSpace {
        match self {
            Format::Png | Format::Jpeg => ColorSpace::SRGB,
            Format::Video | Format::Audio => ColorSpace::REC709_DISPLAY,
            Format::Exr => ColorSpace::LINEAR_REC709,
        }
    }
}

/// What can be learned about an image without decoding its pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub space: ColorSpace,
    pub fingerprint: ContentHash,
    /// The file stores an alpha channel. Without one every pixel is opaque.
    pub has_alpha: bool,
}

fn format_of(path: &Path) -> Result<Format, MediaError> {
    Format::of(path).ok_or_else(|| MediaError::Unsupported {
        path: path.to_owned(),
    })
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> MediaError + '_ {
    move |source| MediaError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A hash of the file's bytes.
///
/// TODO: remember fingerprints by path, size and modification time, so large
/// files aren't re-read on every probe (design §4.3).
pub fn fingerprint(path: &Path) -> Result<ContentHash, MediaError> {
    let file = File::open(path).map_err(io_error(path))?;
    ContentHash::of_reader(io::BufReader::new(file)).map_err(io_error(path))
}

/// Reads the header and fingerprints the file.
pub fn probe(path: &Path) -> Result<Probe, MediaError> {
    let decode = |message: String| MediaError::Decode {
        path: path.to_owned(),
        message,
    };
    let (width, height, space, has_alpha) = match format_of(path)? {
        Format::Video => {
            let video = probe_video(path)?;
            (video.width, video.height, video.space, video.has_alpha)
        }
        Format::Audio => (0, 0, ColorSpace::REC709_DISPLAY, false),
        Format::Png | Format::Jpeg => {
            use image::ImageDecoder as _;
            let decoder = image::ImageReader::open(path)
                .map_err(|e| decode(e.to_string()))?
                .with_guessed_format()
                .map_err(|e| decode(e.to_string()))?
                .into_decoder()
                .map_err(|e| decode(e.to_string()))?;
            let (w, h) = decoder.dimensions();
            (w, h, ColorSpace::SRGB, decoder.color_type().has_alpha())
        }
        Format::Exr => {
            let meta = exr::meta::MetaData::read_from_file(path, false)
                .map_err(|e| decode(e.to_string()))?;
            let header = meta
                .headers
                .first()
                .ok_or_else(|| decode("no layers".into()))?;
            let size = header.layer_size;
            let has_alpha = header
                .channels
                .list
                .iter()
                .any(|c| c.name.to_string() == "A");
            (
                size.width() as u32,
                size.height() as u32,
                exr_space(header.shared_attributes.chromaticities),
                has_alpha,
            )
        }
    };
    Ok(Probe {
        width,
        height,
        space,
        fingerprint: fingerprint(path)?,
        has_alpha,
    })
}

/// Decodes an image to straight-alpha `f32` RGBA, with the space the file declares.
pub fn read_image(path: &Path) -> Result<(RgbaImage, ColorSpace), MediaError> {
    let decode = |message: String| MediaError::Decode {
        path: path.to_owned(),
        message,
    };
    match format_of(path)? {
        Format::Audio => Err(MediaError::Decode {
            path: path.to_owned(),
            message: "this file has no picture".into(),
        }),
        Format::Video => {
            let mut reader = VideoReader::open(path)?;
            let space = probe_video(path)?.space;
            Ok((reader.frame(Time::ZERO)?.to_rgba(), space))
        }
        Format::Png | Format::Jpeg => {
            let decoded = image::open(path)
                .map_err(|e| decode(e.to_string()))?
                .into_rgba32f();
            let image = RgbaImage {
                width: decoded.width(),
                height: decoded.height(),
                pixels: decoded.into_raw(),
                premultiplied: false,
            };
            Ok((image, ColorSpace::SRGB))
        }
        Format::Exr => {
            let read = exrp::read()
                .no_deep_data()
                .largest_resolution_level()
                .rgba_channels(
                    |size, _| RgbaImage::new(size.width() as u32, size.height() as u32, true),
                    |img: &mut RgbaImage, pos, (r, g, b, a): (f32, f32, f32, f32)| {
                        img.set_pixel(pos.x() as u32, pos.y() as u32, [r, g, b, a]);
                    },
                )
                .first_valid_layer()
                .all_attributes()
                .from_file(path)
                .map_err(|e| decode(e.to_string()))?;
            let space = exr_space(read.attributes.chromaticities);
            let mut image = read.layer_data.channel_data.pixels;
            // EXR stores premultiplied color; the engine's input contract is straight.
            image.unpremultiply();
            Ok((image, space))
        }
    }
}

/// Writes premultiplied pixels to a half-float EXR, tagged with the space's primaries.
/// Only linear spaces can be written to EXR.
pub fn write_exr(path: &Path, image: &RgbaImage, space: ColorSpace) -> Result<(), MediaError> {
    let encode = |message: String| MediaError::Encode {
        path: path.to_owned(),
        message,
    };
    if space.transfer != Transfer::Linear {
        return Err(encode(format!("EXR files hold linear light, not {space}")));
    }
    let mut image = image.clone();
    image.premultiply();
    let w = image.width as usize;
    let pixels = |pos: exrp::Vec2<usize>| {
        let i = (pos.y() * w + pos.x()) * 4;
        let p = &image.pixels[i..i + 4];
        (
            exrp::f16::from_f32(p[0]),
            exrp::f16::from_f32(p[1]),
            exrp::f16::from_f32(p[2]),
            exrp::f16::from_f32(p[3]),
        )
    };
    let channels = exrp::SpecificChannels::rgba(pixels);
    // Increasing line order: with "unspecified" the encoder writes chunks in
    // whatever order its threads finish, so identical pixels give different files.
    let encoding = exrp::Encoding {
        compression: exrp::Compression::ZIP16,
        blocks: exrp::Blocks::ScanLines,
        line_order: exrp::LineOrder::Increasing,
    };
    let layer = exrp::Layer::new(
        (image.width as usize, image.height as usize),
        exrp::LayerAttributes::default(),
        encoding,
        channels,
    );
    let mut out = exrp::Image::from_layer(layer);
    out.attributes.chromaticities = Some(exr_chromaticities(space.primaries));
    let mut bytes = Cursor::new(Vec::new());
    out.write()
        .to_buffered(&mut bytes)
        .map_err(|e| encode(e.to_string()))?;
    publish(path, bytes.get_ref())
}

/// Writes straight-alpha PNG. Values are clamped to 0–1 and quantized.
pub fn write_png(path: &Path, image: &RgbaImage, sixteen_bit: bool) -> Result<(), MediaError> {
    let encode = |message: String| MediaError::Encode {
        path: path.to_owned(),
        message,
    };
    let mut image = image.clone();
    image.unpremultiply();
    let (w, h) = (image.width, image.height);
    let dynamic = if sixteen_bit {
        let data = image
            .pixels
            .iter()
            .map(|v| quantize(*v, 65535.0) as u16)
            .collect();
        image::DynamicImage::ImageRgba16(
            image::ImageBuffer::from_raw(w, h, data).expect("sizes match"),
        )
    } else {
        let data = image
            .pixels
            .iter()
            .map(|v| quantize(*v, 255.0) as u8)
            .collect();
        image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_raw(w, h, data).expect("sizes match"),
        )
    };
    let mut bytes = Cursor::new(Vec::new());
    dynamic
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| encode(e.to_string()))?;
    publish(path, bytes.get_ref())
}

fn quantize(v: f32, max: f32) -> f32 {
    (v.clamp(0.0, 1.0) * max).round()
}

/// Writes the finished file under a temporary name, then renames it into place,
/// so a half-written output is never visible.
fn publish(path: &Path, bytes: &[u8]) -> Result<(), MediaError> {
    let name = path.file_name().ok_or_else(|| MediaError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "no file name"),
    })?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(".writing");
    let temp = path.with_file_name(temp_name);
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(io_error(path))
}

const KNOWN_PRIMARIES: [Primaries; 5] = [
    Primaries::Rec709,
    Primaries::AcesAp1,
    Primaries::AcesAp0,
    Primaries::Rec2020,
    Primaries::P3D65,
];

fn exr_chromaticities(p: Primaries) -> exr::meta::attribute::Chromaticities {
    let [r, g, b, w] = p.chromaticities();
    let v = |[x, y]: [f64; 2]| exrp::Vec2(x as f32, y as f32);
    exr::meta::attribute::Chromaticities {
        red: v(r),
        green: v(g),
        blue: v(b),
        white: v(w),
    }
}

/// EXR pixels are linear; the chromaticities say which primaries. Rec.709 is
/// the EXR default when they are missing.
fn exr_space(chromaticities: Option<exr::meta::attribute::Chromaticities>) -> ColorSpace {
    let primaries = chromaticities
        .and_then(|c| {
            KNOWN_PRIMARIES.into_iter().find(|p| {
                let [r, g, b, w] = p.chromaticities();
                [(c.red, r), (c.green, g), (c.blue, b), (c.white, w)]
                    .iter()
                    .all(|(have, want)| {
                        (have.x() as f64 - want[0]).abs() < 1e-3
                            && (have.y() as f64 - want[1]).abs() < 1e-3
                    })
            })
        })
        .unwrap_or(Primaries::Rec709);
    ColorSpace::new(primaries, Transfer::Linear)
}
