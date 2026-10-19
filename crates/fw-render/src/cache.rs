//! The RAM cache: images by content key, evicting the least recently used.
//!
//! Nothing is ever invalidated explicitly. A changed input changes the key.
//! TODO: the GPU-memory and persistent disk tiers (design §4.3).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use fw_core::{ContentHash, RgbaImage};

pub struct RamCache {
    budget: usize,
    used: usize,
    clock: u64,
    entries: HashMap<ContentHash, (Arc<RgbaImage>, u64)>,
    by_age: BTreeMap<u64, ContentHash>,
}

impl RamCache {
    pub fn new(budget_bytes: usize) -> RamCache {
        RamCache {
            budget: budget_bytes,
            used: 0,
            clock: 0,
            entries: HashMap::new(),
            by_age: BTreeMap::new(),
        }
    }

    pub fn get(&mut self, key: &ContentHash) -> Option<Arc<RgbaImage>> {
        self.clock += 1;
        let (image, age) = self.entries.get_mut(key)?;
        self.by_age.remove(age);
        *age = self.clock;
        self.by_age.insert(self.clock, *key);
        Some(image.clone())
    }

    /// Stores an image unless it alone is larger than the budget.
    pub fn insert(&mut self, key: ContentHash, image: Arc<RgbaImage>) {
        let size = image.byte_size();
        if size > self.budget {
            return;
        }
        self.remove(&key);
        while self.used + size > self.budget {
            let Some((_, oldest)) = self.by_age.pop_first() else {
                break;
            };
            if let Some((old, _)) = self.entries.remove(&oldest) {
                self.used -= old.byte_size();
            }
        }
        self.clock += 1;
        self.used += size;
        self.by_age.insert(self.clock, key);
        self.entries.insert(key, (image, self.clock));
    }

    pub fn remove(&mut self, key: &ContentHash) {
        if let Some((image, age)) = self.entries.remove(key) {
            self.by_age.remove(&age);
            self.used -= image.byte_size();
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used
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

    fn image() -> Arc<RgbaImage> {
        Arc::new(RgbaImage::new(4, 4, true)) // 256 bytes
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

    #[test]
    fn ignores_images_larger_than_the_budget() {
        let mut cache = RamCache::new(100);
        cache.insert(key(1), image());
        assert!(cache.is_empty());
    }
}
