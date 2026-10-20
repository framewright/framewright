//! Audio behavior: the real-time contract.
//!
//! Separate from image behavior because it runs differently: continuously,
//! in small blocks, on the audio thread. A node may have either or both.

/// Makes processors for a node type. Called off the audio thread.
pub trait AudioBehavior: Send + Sync {
    fn create(&self) -> Box<dyn AudioProcessor>;

    /// The numeric parameters `process` reads, by id. The engine resolves
    /// them (automation included) and passes them in this order.
    fn params(&self) -> Vec<String> {
        vec![]
    }
}

/// One instance, running on the audio thread.
///
/// `process` must not allocate, lock, wait or do I/O: anything that can
/// take unbounded time causes audible dropouts. `prepare` is where buffers
/// are allocated.
pub trait AudioProcessor: Send {
    fn prepare(&mut self, sample_rate: u32, max_frames: usize);

    fn process(&mut self, block: &mut AudioBlock<'_>);

    /// Samples of delay this processor adds, so the engine can compensate.
    fn latency(&self) -> u32 {
        0
    }
}

/// One block of audio. Samples are interleaved; `input` and `output` hold
/// `frames × channels` values.
///
/// TODO: decide and document whether `output` arrives zeroed or processors
/// must write every sample (today they write every sample).
pub struct AudioBlock<'a> {
    pub input: &'a [f32],
    pub output: &'a mut [f32],
    pub frames: usize,
    pub channels: usize,
    /// The parameters named by [`AudioBehavior::params`], in that order,
    /// already resolved for this block. Nodes never see automation.
    pub params: &'a [f32],
}
