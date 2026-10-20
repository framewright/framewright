//! Pictures a hardware decoder left on the engine's GPU (design §12), and
//! the device they share. Plain data and a trait, so the renderer (which
//! owns wgpu) and the media layer (which owns FFmpeg) meet here without
//! depending on each other.

use std::any::Any;
use std::ffi::CStr;
use std::fmt;
use std::sync::Arc;

use fw_core::planar::PlanarEncoding;
use fw_core::{DecodedImage, RgbaImage};

/// A decoded picture: in memory, or an image on the engine's GPU.
#[derive(Debug, Clone)]
pub enum Decoded {
    Cpu(DecodedImage),
    Hardware(HardwarePicture),
}

impl Decoded {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Decoded::Cpu(d) => d.size(),
            Decoded::Hardware(h) => (h.width, h.height),
        }
    }

    /// Bytes held: in RAM, or for a hardware picture in GPU memory.
    pub fn byte_size(&self) -> usize {
        match self {
            Decoded::Cpu(d) => d.byte_size(),
            Decoded::Hardware(h) => h.byte_size(),
        }
    }

    pub fn cpu(&self) -> Option<&DecodedImage> {
        match self {
            Decoded::Cpu(d) => Some(d),
            Decoded::Hardware(_) => None,
        }
    }

    /// Straight-alpha RGBA, for a picture in memory; `None` for one on the GPU.
    pub fn to_rgba(&self) -> Option<RgbaImage> {
        self.cpu().map(DecodedImage::to_rgba)
    }
}

/// A two-plane YUV image (NV12, or P010 for more than 8 bits) written by a
/// hardware decoder on the engine's device.
#[derive(Clone)]
pub struct HardwarePicture {
    /// The picture's size; the image may be larger (`coded`), padded to the
    /// codec's block size.
    pub width: u32,
    pub height: u32,
    pub coded: (u32, u32),
    /// 8 bits for NV12; 10 bits shifted up by 6 for P010. Chroma is halved
    /// both ways.
    pub encoding: PlanarEncoding,
    pub frame: Arc<dyn HardwareFrame>,
}

impl HardwarePicture {
    pub fn byte_size(&self) -> usize {
        let bytes = self.encoding.bytes_per_sample() as usize;
        self.coded.0 as usize * self.coded.1 as usize * bytes * 3 / 2
    }
}

impl fmt::Debug for HardwarePicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HardwarePicture")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("coded", &self.coded)
            .field("encoding", &self.encoding)
            .finish_non_exhaustive()
    }
}

/// One decoded image, still owned by its decoder's pool. Dropping the last
/// reference returns it to the pool.
///
/// Reading it on the engine's queue goes: [`HardwareFrame::acquire`], then
/// exactly one of submitting work that waits for `Acquired::wait` and
/// signals `Acquired::signal` on its semaphore, or
/// [`HardwareFrame::abandon`]. Either way the decoder can rely on the signal
/// arriving; until it does, it won't write the image again.
pub trait HardwareFrame: Send + Sync {
    /// Moves the image into a layout shaders can read once decoding has
    /// finished, and reserves the semaphore value the reader signals.
    fn acquire(&self) -> Result<Acquired, String>;

    /// The reader never submitted: signal from the host instead.
    fn abandon(&self, acquired: &Acquired);
}

/// Raw Vulkan handles for reading an acquired image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acquired {
    /// `VkImage`, multi-planar NV12 or P010.
    pub image: u64,
    /// A timeline `VkSemaphore`.
    pub semaphore: u64,
    /// Wait for this value before reading.
    pub wait: u64,
    /// Signal this value when done reading.
    pub signal: u64,
}

/// The engine's Vulkan device, as a hardware decoder needs to share it.
/// Created by the renderer when the GPU has a video decode queue.
#[derive(Clone)]
pub struct VulkanVideoDevice {
    /// `PFN_vkGetInstanceProcAddr`.
    pub get_instance_proc_addr: usize,
    /// `VkInstance`, `VkPhysicalDevice`, `VkDevice`.
    pub instance: u64,
    pub physical_device: u64,
    pub device: u64,
    /// The queue family of the renderer's own queue (never given to the
    /// decoder), a compute family with a queue the renderer never uses, and
    /// the decode family.
    pub render_family: u32,
    pub compute_family: u32,
    pub decode_family: u32,
    /// The device extensions enabled for video.
    pub extensions: Vec<&'static CStr>,
    /// Keeps the device alive while the decoder or its pictures exist.
    pub keep_alive: Arc<dyn Any + Send + Sync>,
}

impl fmt::Debug for VulkanVideoDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanVideoDevice")
            .field("device", &self.device)
            .field("compute_family", &self.compute_family)
            .field("decode_family", &self.decode_family)
            .field("extensions", &self.extensions)
            .finish_non_exhaustive()
    }
}
