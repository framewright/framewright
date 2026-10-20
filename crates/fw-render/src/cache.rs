//! The RAM cache: rendered images and decoded pictures by content key,
//! evicting the least recently used. Hardware decoders' pictures live in GPU
//! memory, so they count against a budget of their own.
//!
//! Nothing is ever invalidated explicitly. A changed input changes the key.
//! TODO: the GPU-memory and persistent disk tiers (design §4.3).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use fw_compile::Decoded;
use fw_core::{ContentHash, RgbaImage};

#[derive(Debug, Clone)]
pub enum Cached {
    /// A plan's output, premultiplied.
    Image(Arc<RgbaImage>),
    /// A file's picture as decoded (straight RGBA is premultiplied first).
    Decoded(Arc<Decoded>),
}

impl Cached {
    fn byte_size(&self) -> usize {
        match self {
            Cached::Image(i) => i.byte_size(),
            Cached::Decoded(d) => d.byte_size(),
        }
    }

    fn on_gpu(&self) -> bool {
        matches!(self, Cached::Decoded(d) if matches!(**d, Decoded::Hardware(_)))
    }
}

/// A default budget for hardware pictures held in GPU memory: 1 GiB, about
/// 340 1080p NV12 pictures.
pub const DEFAULT_GPU_CACHE_BYTES: usize = 1 << 30;

pub struct RamCache {
    budget: usize,
    used: usize,
    gpu_budget: usize,
    gpu_used: usize,
    clock: u64,
    entries: HashMap<ContentHash, (Cached, u64)>,
    by_age: BTreeMap<u64, ContentHash>,
}

impl RamCache {
    pub fn new(budget_bytes: usize) -> RamCache {
        RamCache::with_gpu_budget(budget_bytes, DEFAULT_GPU_CACHE_BYTES)
    }

    pub fn with_gpu_budget(budget_bytes: usize, gpu_budget_bytes: usize) -> RamCache {
        RamCache {
            budget: budget_bytes,
            used: 0,
            gpu_budget: gpu_budget_bytes,
            gpu_used: 0,
            clock: 0,
            entries: HashMap::new(),
            by_age: BTreeMap::new(),
        }
    }

    pub fn get(&mut self, key: &ContentHash) -> Option<Cached> {
        self.clock += 1;
        let (image, age) = self.entries.get_mut(key)?;
        self.by_age.remove(age);
        *age = self.clock;
        self.by_age.insert(self.clock, *key);
        Some(image.clone())
    }

    /// Stores an entry unless it alone is larger than its budget, evicting
    /// the least recently used entries of the same kind (in RAM or on the
    /// GPU) to make room.
    pub fn insert(&mut self, key: ContentHash, image: Cached) {
        let size = image.byte_size();
        let on_gpu = image.on_gpu();
        let budget = if on_gpu { self.gpu_budget } else { self.budget };
        if size > budget {
            return;
        }
        self.remove(&key);
        while self.used_by(on_gpu) + size > budget {
            let oldest = self
                .by_age
                .iter()
                .find(|(_, k)| self.entries[*k].0.on_gpu() == on_gpu)
                .map(|(age, k)| (*age, *k));
            let Some((age, oldest)) = oldest else {
                break;
            };
            self.by_age.remove(&age);
            if let Some((old, _)) = self.entries.remove(&oldest) {
                *self.used_mut(on_gpu) -= old.byte_size();
            }
        }
        self.clock += 1;
        *self.used_mut(on_gpu) += size;
        self.by_age.insert(self.clock, key);
        self.entries.insert(key, (image, self.clock));
    }

    pub fn remove(&mut self, key: &ContentHash) {
        if let Some((image, age)) = self.entries.remove(key) {
            self.by_age.remove(&age);
            *self.used_mut(image.on_gpu()) -= image.byte_size();
        }
    }

    fn used_by(&self, on_gpu: bool) -> usize {
        if on_gpu { self.gpu_used } else { self.used }
    }

    fn used_mut(&mut self, on_gpu: bool) -> &mut usize {
        if on_gpu {
            &mut self.gpu_used
        } else {
            &mut self.used
        }
    }

    /// Bytes held in RAM.
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Bytes of hardware pictures held in GPU memory.
    pub fn gpu_used_bytes(&self) -> usize {
        self.gpu_used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Cached {
        Cached::Image(Arc::new(RgbaImage::new(4, 4, true))) // 256 bytes
    }

    fn key(n: u8) -> ContentHash {
        ContentHash::of_bytes(&[n])
    }

    #[test]
    fn evicts_least_recently_used_within_budget() {
        let mut cache = RamCache::new(600);
        cache.insert(key(1), image());
        cache.insert(key(2), image());
        assert!(cache.get(&key(1)).is_some()); // 1 is now newer than 2
        cache.insert(key(3), image());
        assert!(cache.get(&key(2)).is_none());
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(3)).is_some());
        assert_eq!(cache.used_bytes(), 512);
    }

    struct NoFrame;

    impl fw_compile::HardwareFrame for NoFrame {
        fn acquire(&self) -> Result<fw_compile::Acquired, String> {
            Err("not a real frame".into())
        }
        fn abandon(&self, _: &fw_compile::Acquired) {}
    }

    /// A 16×16 NV12 hardware picture: 384 bytes of GPU memory.
    fn hardware() -> Cached {
        use fw_core::planar::{PlanarEncoding, PlanarModel, Siting, YuvMatrix, YuvRange};
        Cached::Decoded(Arc::new(Decoded::Hardware(fw_compile::HardwarePicture {
            width: 16,
            height: 16,
            coded: (16, 16),
            encoding: PlanarEncoding {
                model: PlanarModel::Yuv {
                    matrix: YuvMatrix::Bt709,
                    range: YuvRange::Limited,
                },
                bits: 8,
                shift: 0,
                chroma_shift: (1, 1),
                siting: (Siting::Cosited, Siting::Centred),
                alpha: false,
            },
            frame: Arc::new(NoFrame),
        })))
    }

    #[test]
    fn hardware_pictures_have_their_own_budget() {
        let mut cache = RamCache::with_gpu_budget(600, 800);
        cache.insert(key(1), image());
        cache.insert(key(2), image());
        for k in 10..13 {
            cache.insert(key(k), hardware());
        }
        // Only two pictures fit in GPU memory; RAM entries weren't touched.
        assert_eq!((cache.used_bytes(), cache.gpu_used_bytes()), (512, 768));
        assert!(cache.get(&key(10)).is_none());
        assert!(cache.get(&key(1)).is_some() && cache.get(&key(2)).is_some());
    }

    #[test]
    fn ignores_images_larger_than_the_budget() {
        let mut cache = RamCache::new(100);
        cache.insert(key(1), image());
        assert!(cache.is_empty());
    }
}
