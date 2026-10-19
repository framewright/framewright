//! Images in memory, on the CPU side of the engine.

/// RGBA pixels as `f32`, row-major from the top-left, four values per pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
    /// Inside the engine images are always premultiplied. Straight alpha only
    /// exists at file input and output.
    pub premultiplied: bool,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, premultiplied: bool) -> RgbaImage {
        RgbaImage {
            width,
            height,
            pixels: vec![0.0; width as usize * height as usize * 4],
            premultiplied,
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [f32; 4]) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.pixels[i..i + 4].copy_from_slice(&rgba);
    }

    pub fn byte_size(&self) -> usize {
        self.pixels.len() * 4
    }

    pub fn premultiply(&mut self) {
        if !self.premultiplied {
            for px in self.pixels.chunks_exact_mut(4) {
                let a = px[3];
                px[0] *= a;
                px[1] *= a;
                px[2] *= a;
            }
            self.premultiplied = true;
        }
    }

    /// Divides colour by alpha. Fully transparent pixels become black.
    pub fn unpremultiply(&mut self) {
        if self.premultiplied {
            for px in self.pixels.chunks_exact_mut(4) {
                let a = px[3];
                let scale = if a > 0.0 { 1.0 / a } else { 0.0 };
                px[0] *= scale;
                px[1] *= scale;
                px[2] *= scale;
            }
            self.premultiplied = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_round_trips() {
        let mut img = RgbaImage::new(2, 1, false);
        img.set_pixel(0, 0, [0.5, 0.25, 1.0, 0.5]);
        img.set_pixel(1, 0, [1.0, 1.0, 1.0, 0.0]);
        img.premultiply();
        assert_eq!(img.pixel(0, 0), [0.25, 0.125, 0.5, 0.5]);
        img.unpremultiply();
        assert_eq!(img.pixel(0, 0), [0.5, 0.25, 1.0, 0.5]);
        assert_eq!(img.pixel(1, 0), [0.0, 0.0, 0.0, 0.0]);
    }
}
