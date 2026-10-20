//! What playback renders: frames of the latest document. Also keeps track
//! of which frames the frame cache holds, for the timeline to show.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fw_compile::Options;
use fw_core::Time;
use fw_doc::Document;
use fw_playback::{DocumentFrames, FrameSource};
use fw_render::GpuImage;

/// Frames checked against the cache per idle moment of the render thread.
const CHECKS_PER_IDLE: usize = 256;

/// The document playback renders, shared between the engine and the
/// render thread.
pub(crate) struct Latest {
    pub doc: Document,
    /// Bumped whenever what's cached may have changed: an edit, or frames
    /// leaving the cache.
    generation: u64,
    /// Frames that were in the cache, with the generation they were last
    /// known to be there in. Only those of the current generation count.
    cached: BTreeMap<i64, u64>,
}

impl Latest {
    pub fn new(doc: Document) -> Latest {
        Latest {
            doc,
            generation: 0,
            cached: BTreeMap::new(),
        }
    }

    pub fn replace(&mut self, doc: Document) {
        self.doc = doc;
        self.generation += 1;
    }

    /// Runs of frames known to be cached, as `[start, end)` frame indices.
    pub fn cached_ranges(&self) -> Vec<(i64, i64)> {
        let mut ranges: Vec<(i64, i64)> = vec![];
        for (&index, &generation) in &self.cached {
            if generation != self.generation {
                continue;
            }
            match ranges.last_mut() {
                Some(last) if last.1 == index => last.1 = index + 1,
                _ => ranges.push((index, index + 1)),
            }
        }
        ranges
    }
}

pub(crate) type SharedLatest = Arc<Mutex<Latest>>;

/// Renders frames of the latest document.
pub(crate) struct Edited {
    pub latest: SharedLatest,
    pub frames: DocumentFrames,
    /// The renderer's eviction count when last looked at.
    evictions: u64,
}

impl Edited {
    pub fn new(latest: SharedLatest, frames: DocumentFrames) -> Edited {
        Edited {
            latest,
            frames,
            evictions: 0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl FrameSource for Edited {
    type Frame = GpuImage;

    fn frame(&mut self, index: i64) -> Result<GpuImage, String> {
        // A cheap clone: the document is made of persistent maps.
        let (doc, generation) = {
            let latest = self.lock();
            (latest.doc.clone(), latest.generation)
        };
        self.frames.doc = doc;
        let result = self.frames.frame(index);
        if result.is_ok() {
            self.lock().cached.insert(index, generation);
        }
        result
    }

    /// Checks frames whose place in the cache is in doubt: compiling a frame
    /// is cheap, and its key says whether the cache still has it.
    fn idle(&mut self) {
        let evictions = self.frames.renderer.evictions();
        let evicted = evictions != self.evictions;
        self.evictions = evictions;
        let (doc, generation, doubtful) = {
            let mut latest = self.lock();
            if evicted {
                latest.generation += 1;
            }
            let generation = latest.generation;
            let doubtful: Vec<i64> = latest
                .cached
                .iter()
                .filter(|(_, g)| **g != generation)
                .map(|(i, _)| *i)
                .take(CHECKS_PER_IDLE)
                .collect();
            (latest.doc.clone(), generation, doubtful)
        };
        if doubtful.is_empty() {
            return;
        }
        let frames = &self.frames;
        let checked: Vec<(i64, bool)> = doubtful
            .into_iter()
            .map(|index| {
                let held = Time::of_frame(index, frames.rate).ok().and_then(|time| {
                    let options = Options {
                        time,
                        ..frames.options.clone()
                    };
                    fw_compile::compile(&doc, &frames.registry, &frames.media, &options).ok()
                });
                (
                    index,
                    held.is_some_and(|plan| frames.renderer.has_frame(&plan)),
                )
            })
            .collect();
        let mut latest = self.lock();
        if latest.generation != generation {
            return;
        }
        for (index, held) in checked {
            if held {
                latest.cached.insert(index, generation);
            } else {
                latest.cached.remove(&index);
            }
        }
    }
}
