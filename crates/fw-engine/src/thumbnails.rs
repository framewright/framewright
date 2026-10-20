//! Small pictures of media files for the media panel, decoded on a thread of
//! their own and kept in memory.
//!
//! TODO: cache them on disk beside the editing copies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use fw_core::{ColorSpace, RgbaImage, Time};
use fw_media::{Format, VideoReader, probe_video};

/// The widest a thumbnail is.
pub const THUMBNAIL_WIDTH: u32 = 160;

/// A thumbnail: 8-bit sRGB with straight alpha, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

enum State {
    Pending,
    Ready(Arc<Thumbnail>),
    /// The file has no picture, or couldn't be read.
    None,
}

pub(crate) struct Thumbnails {
    states: Arc<Mutex<HashMap<PathBuf, State>>>,
    requests: Sender<PathBuf>,
}

impl Thumbnails {
    pub fn new() -> Thumbnails {
        let states: Arc<Mutex<HashMap<PathBuf, State>>> = Arc::default();
        let (requests, queue) = channel::<PathBuf>();
        let shared = states.clone();
        let spawned = std::thread::Builder::new()
            .name("fw-thumbnails".into())
            .spawn(move || {
                for path in queue {
                    let state = match make(&path) {
                        Some(thumbnail) => State::Ready(Arc::new(thumbnail)),
                        None => State::None,
                    };
                    shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(path, state);
                }
            });
        if let Err(e) = spawned {
            eprintln!("fw-engine: no thumbnails: {e}");
        }
        Thumbnails { states, requests }
    }

    /// The thumbnail of the file at `path`, asking for it if it hasn't been
    /// made yet.
    pub fn get(&self, path: &Path) -> Option<Arc<Thumbnail>> {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        match states.get(path) {
            Some(State::Ready(t)) => Some(t.clone()),
            Some(State::Pending | State::None) => None,
            None => {
                states.insert(path.to_owned(), State::Pending);
                let _ = self.requests.send(path.to_owned());
                None
            }
        }
    }

    /// Whether any thumbnail asked for is still being made.
    pub fn pending(&self) -> bool {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.values().any(|s| matches!(s, State::Pending))
    }
}

/// A frame a third of the way into a video, or a still, made small.
fn make(path: &Path) -> Option<Thumbnail> {
    let (image, space) = match Format::of(path)? {
        Format::Video => {
            let info = probe_video(path).ok()?;
            let count = info.frames.count();
            let at = info.frames.time_of(count / 3).unwrap_or(Time::ZERO);
            let mut reader = VideoReader::open_probed(path, &info).ok()?;
            // Planes become RGBA in the stream's own encoding, close enough
            // to sRGB for a thumbnail.
            (reader.frame(at).ok()?.to_rgba(), ColorSpace::SRGB)
        }
        Format::Audio => return None,
        _ => fw_media::read_image(path).ok()?,
    };
    Some(shrink(&image, space))
}

/// `image` averaged down to at most [`THUMBNAIL_WIDTH`] wide, in sRGB.
fn shrink(image: &RgbaImage, space: ColorSpace) -> Thumbnail {
    let width = image.width.clamp(1, THUMBNAIL_WIDTH);
    let height = ((image.height as u64 * width as u64 / image.width.max(1) as u64) as u32).max(1);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let (y0, y1) = span(y, height, image.height);
        for x in 0..width {
            let (x0, x1) = span(x, width, image.width);
            let mut sum = [0.0f64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = image.pixel(sx, sy);
                    // Average premultiplied, so transparent pixels don't darken edges.
                    for c in 0..3 {
                        sum[c] += f64::from(p[c] * p[3]);
                    }
                    sum[3] += f64::from(p[3]);
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as f64;
            let alpha = sum[3] / n;
            let straight = |c: usize| if alpha > 0.0 { sum[c] / n / alpha } else { 0.0 };
            let rgb = space.convert([straight(0), straight(1), straight(2)], ColorSpace::SRGB);
            let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba.extend([byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), byte(alpha)]);
        }
    }
    Thumbnail {
        width,
        height,
        rgba,
    }
}

/// The source pixels `[start, end)` that output pixel `i` of `out` covers.
fn span(i: u32, out: u32, source: u32) -> (u32, u32) {
    let start = (i as u64 * source as u64 / out as u64) as u32;
    let end = (((i + 1) as u64 * source as u64).div_ceil(out as u64) as u32).max(start + 1);
    (start, end.min(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_averages_and_keeps_the_shape() {
        let mut image = RgbaImage::new(320, 180, false);
        for y in 0..180 {
            for x in 0..320 {
                let v = if x % 2 == 0 { 1.0 } else { 0.0 };
                image.set_pixel(x, y, [v, v, v, 1.0]);
            }
        }
        let thumb = shrink(&image, ColorSpace::SRGB);
        assert_eq!((thumb.width, thumb.height), (160, 90));
        // Each pixel averages one white and one black column.
        assert_eq!(&thumb.rgba[..4], &[128, 128, 128, 255]);
    }
}
