//! Decoded video pictures as the decoder made them: separate planes of
//! integer codes, before any matrix, range or chroma upsampling is applied.
//!
//! The GPU turns these into RGBA (fw-render's unpack kernel). The CPU
//! conversion here is the reference that kernel is tested against, and the
//! route for anything that needs pixels without a GPU.

use crate::RgbaImage;

/// One plane of samples, rows packed with no padding. Samples are one byte
/// each, or little-endian `u16` when [`PlanarImage::bytes_per_sample`] is 2.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    /// The luma weights of red and blue, Kr and Kb.
    pub fn kr_kb(self) -> (f32, f32) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvRange {
    /// Luma 16–235 and chroma 16–240, in 8-bit terms ("TV" or "video" range).
    Limited,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanarModel {
    /// Planes are Y, Cb, Cr.
    Yuv { matrix: YuvMatrix, range: YuvRange },
    /// Planes are R, G, B.
    Rgb,
}

/// Where chroma samples sit relative to luma, along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Siting {
    /// On the first luma sample of each group (MPEG-2 and H.264 horizontally).
    Cosited,
    /// Midway between the group's luma samples (JPEG, and most vertically).
    Centred,
}

/// How a [`PlanarImage`]'s codes become colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanarEncoding {
    pub model: PlanarModel,
    /// Bits per code, e.g. 8 or 10.
    pub bits: u32,
    /// Codes are stored multiplied by `2^shift` (unpackers that widen to
    /// 16 bits by shifting). Usually 0.
    pub shift: u32,
    /// log2 of the chroma subsampling: 4:2:0 is (1, 1), 4:2:2 is (1, 0).
    pub chroma_shift: (u32, u32),
    pub siting: (Siting, Siting),
    /// A fourth plane holds alpha, stored with all `bits + shift` bits used.
    pub alpha: bool,
}

/// A picture as separate planes: Y, Cb, Cr (or R, G, B), then alpha if any.
/// The first plane is full size; chroma planes may be subsampled.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanarImage {
    pub width: u32,
    pub height: u32,
    pub encoding: PlanarEncoding,
    pub planes: Vec<Plane>,
}

impl PlanarImage {
    pub fn bytes_per_sample(&self) -> u32 {
        self.encoding.bytes_per_sample()
    }

    pub fn byte_size(&self) -> usize {
        self.planes.iter().map(|p| p.data.len()).sum()
    }

    /// Converts to straight-alpha RGBA in the stream's own encoding (no
    /// transfer function is undone). The reference for the GPU kernel.
    pub fn to_rgba(&self) -> RgbaImage {
        let e = &self.encoding;
        let coeffs = e.coefficients();
        let mut out = RgbaImage::new(self.width, self.height, false);
        let read = |plane: &Plane, x: u32, y: u32| -> f32 {
            let i = (y * plane.width + x) as usize;
            match self.bytes_per_sample() {
                1 => plane.data[i] as f32,
                _ => u16::from_le_bytes([plane.data[2 * i], plane.data[2 * i + 1]]) as f32,
            }
        };
        // Bilinear between the chroma samples either side of this pixel.
        let chroma = |plane: &Plane, x: u32, y: u32| -> f32 {
            let (fx, x0, x1) = e.chroma_taps(x, e.chroma_shift.0, e.siting.0, plane.width);
            let (fy, y0, y1) = e.chroma_taps(y, e.chroma_shift.1, e.siting.1, plane.height);
            let top = read(plane, x0, y0) * (1.0 - fx) + read(plane, x1, y0) * fx;
            let bottom = read(plane, x0, y1) * (1.0 - fx) + read(plane, x1, y1) * fx;
            top * (1.0 - fy) + bottom * fy
        };
        for y in 0..self.height {
            for x in 0..self.width {
                let codes = [
                    read(&self.planes[0], x, y),
                    chroma(&self.planes[1], x, y),
                    chroma(&self.planes[2], x, y),
                ];
                let alpha = if e.alpha {
                    read(&self.planes[3], x, y) / coeffs.alpha_max
                } else {
                    1.0
                };
                let [r, g, b] = coeffs.rgb(codes);
                out.set_pixel(x, y, [r, g, b, alpha]);
            }
        }
        out
    }
}

/// A picture as a decoder hands it over: still images as RGBA, video as planes.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedImage {
    /// Straight alpha.
    Rgba(RgbaImage),
    Planar(PlanarImage),
}

impl DecodedImage {
    pub fn size(&self) -> (u32, u32) {
        match self {
            DecodedImage::Rgba(i) => (i.width, i.height),
            DecodedImage::Planar(i) => (i.width, i.height),
        }
    }

    pub fn byte_size(&self) -> usize {
        match self {
            DecodedImage::Rgba(i) => i.byte_size(),
            DecodedImage::Planar(i) => i.byte_size(),
        }
    }

    /// Straight-alpha RGBA, converting planes on the CPU.
    pub fn to_rgba(&self) -> RgbaImage {
        match self {
            DecodedImage::Rgba(i) => i.clone(),
            DecodedImage::Planar(i) => i.to_rgba(),
        }
    }
}

/// The numbers a conversion needs, shared by the CPU reference and the GPU
/// kernel's generator so both use identical constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    /// Subtracted from each stored code (Y or R, Cb or G, Cr or B).
    pub offset: [f32; 3],
    /// Then divided by these.
    pub scale: [f32; 3],
    /// Kr and Kb for YUV; `None` for RGB.
    pub kr_kb: Option<(f32, f32)>,
    /// The stored alpha code meaning fully opaque.
    pub alpha_max: f32,
}

impl Coefficients {
    /// Stored codes (Y, Cb, Cr or R, G, B) to RGB.
    pub fn rgb(&self, codes: [f32; 3]) -> [f32; 3] {
        let [a, b, c] = [0, 1, 2].map(|i| (codes[i] - self.offset[i]) / self.scale[i]);
        match self.kr_kb {
            None => [a, b, c],
            Some((kr, kb)) => {
                let (y, cb, cr) = (a, b, c);
                let r = y + 2.0 * (1.0 - kr) * cr;
                let b = y + 2.0 * (1.0 - kb) * cb;
                let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
                [r, g, b]
            }
        }
    }
}

impl PlanarEncoding {
    pub fn bytes_per_sample(&self) -> u32 {
        if self.bits + self.shift <= 8 { 1 } else { 2 }
    }

    pub fn coefficients(&self) -> Coefficients {
        let n = self.bits;
        let stored = (1u32 << self.shift) as f32;
        let max = ((1u64 << n) - 1) as f32 * stored;
        let alpha_max = ((1u64 << (n + self.shift)) - 1) as f32;
        match self.model {
            PlanarModel::Rgb => Coefficients {
                offset: [0.0; 3],
                scale: [max; 3],
                kr_kb: None,
                alpha_max,
            },
            PlanarModel::Yuv { matrix, range } => {
                let mid = (1u32 << (n - 1)) as f32 * stored;
                let (black, y_range, c_range) = match range {
                    YuvRange::Full => (0.0, max, max),
                    YuvRange::Limited => {
                        // 8-bit codes scaled to n bits.
                        let unit = (1u32 << n.saturating_sub(8)) as f32 * stored;
                        (16.0 * unit, 219.0 * unit, 224.0 * unit)
                    }
                };
                Coefficients {
                    offset: [black, mid, mid],
                    scale: [y_range, c_range, c_range],
                    kr_kb: Some(matrix.kr_kb()),
                    alpha_max,
                }
            }
        }
    }

    /// The two chroma samples either side of luma position `p` and the
    /// weight of the second, along one axis subsampled by `2^shift`.
    pub fn chroma_taps(&self, p: u32, shift: u32, siting: Siting, size: u32) -> (f32, u32, u32) {
        if shift == 0 {
            return (0.0, p, p);
        }
        let s = (1u32 << shift) as f32;
        let first = match siting {
            Siting::Cosited => 0.0,
            Siting::Centred => (s - 1.0) / 2.0,
        };
        let u = ((p as f32 - first) / s).max(0.0);
        let i = (u.floor() as u32).min(size - 1);
        (u - i as f32, i, (i + 1).min(size - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoding(model: PlanarModel, bits: u32, shift: u32) -> PlanarEncoding {
        PlanarEncoding {
            model,
            bits,
            shift,
            chroma_shift: (1, 1),
            siting: (Siting::Cosited, Siting::Centred),
            alpha: false,
        }
    }

    #[test]
    fn video_black_and_white_are_exact() {
        for (bits, shift) in [(8, 0), (10, 0), (8, 8), (12, 4)] {
            let e = encoding(
                PlanarModel::Yuv {
                    matrix: YuvMatrix::Bt709,
                    range: YuvRange::Limited,
                },
                bits,
                shift,
            );
            let c = e.coefficients();
            let unit = (1u32 << (bits - 8 + shift)) as f32;
            let mid = (1u32 << (bits - 1 + shift)) as f32;
            assert_eq!(c.rgb([16.0 * unit, mid, mid]), [0.0; 3], "{bits}+{shift}");
            assert_eq!(c.rgb([235.0 * unit, mid, mid]), [1.0; 3], "{bits}+{shift}");
        }
    }

    #[test]
    fn widened_full_range_codes_keep_their_scale() {
        // An 8-bit code shifted to 16 bits: 255 << 8 is white, not 65535.
        let c = encoding(PlanarModel::Rgb, 8, 8).coefficients();
        assert_eq!(c.rgb([65280.0, 0.0, 32640.0]), [1.0, 0.0, 0.5]);
        assert_eq!(c.alpha_max, 65535.0);
    }

    #[test]
    fn chroma_taps_follow_siting() {
        let e = encoding(PlanarModel::Rgb, 8, 0);
        // Co-sited: luma 2 sits on chroma 1; luma 3 is halfway to chroma 2.
        assert_eq!(e.chroma_taps(2, 1, Siting::Cosited, 8), (0.0, 1, 2));
        assert_eq!(e.chroma_taps(3, 1, Siting::Cosited, 8), (0.5, 1, 2));
        // Centred: chroma 0 sits at luma 0.5.
        assert_eq!(e.chroma_taps(0, 1, Siting::Centred, 8), (0.0, 0, 1));
        assert_eq!(e.chroma_taps(1, 1, Siting::Centred, 8), (0.25, 0, 1));
        // The last samples clamp at the edge.
        assert_eq!(e.chroma_taps(15, 1, Siting::Centred, 8), (0.25, 7, 7));
    }
}
