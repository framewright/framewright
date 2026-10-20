//! Reading back what a window shows, for the screenshot mode.

use std::path::Path;

use fw_render::Gpu;

/// A window image copied into a buffer, waiting to be read.
pub struct Capture {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    row_bytes: u32,
    format: wgpu::TextureFormat,
}

/// Records a copy of `texture` into `encoder`, or `None` when the texture
/// can't be copied from or its format isn't one we read.
pub fn record(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    encoder: &mut wgpu::CommandEncoder,
) -> Option<Capture> {
    use wgpu::TextureFormat as F;
    let format = texture.format();
    let readable = matches!(
        format,
        F::Rgb10a2Unorm | F::Bgra8Unorm | F::Bgra8UnormSrgb | F::Rgba8Unorm | F::Rgba8UnormSrgb
    );
    if !readable || !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return None;
    }
    let (width, height) = (texture.width(), texture.height());
    let row_bytes = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fw-screenshot"),
        size: row_bytes as u64 * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    Some(Capture {
        buffer,
        width,
        height,
        row_bytes,
        format,
    })
}

impl Capture {
    /// Waits for the copy and writes an 8-bit sRGB PNG.
    pub fn save(self, gpu: &Gpu, path: &Path) -> Result<(), String> {
        let slice = self.buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.wait();
        let data = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        for y in 0..self.height {
            let row = &data[(y * self.row_bytes) as usize..][..(self.width * 4) as usize];
            for p in row.chunks_exact(4) {
                use wgpu::TextureFormat as F;
                let pixel = match self.format {
                    F::Rgb10a2Unorm => {
                        let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                        [v, v >> 10, v >> 20].map(|c| ((c & 1023) * 255 / 1023) as u8)
                    }
                    F::Bgra8Unorm | F::Bgra8UnormSrgb => [p[2], p[1], p[0]],
                    _ => [p[0], p[1], p[2]],
                };
                rgb.extend_from_slice(&pixel);
            }
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())
    }
}
