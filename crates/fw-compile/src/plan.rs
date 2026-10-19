//! The compiled plan: what the renderer executes.

use std::fmt::Write as _;
use std::path::PathBuf;

use fw_core::{ColorSpace, ContentHash, NodeId, Rect};
use fw_plugin::ImageHandle;

/// A frame's work, in execution order. Every step's output has a cache key;
/// a renderer that already holds the output's key can skip the whole plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// The image the plan delivers.
    pub output: ImageHandle,
    pub output_space: ColorSpace,
    /// The delivered region in pixels. Step 1 renders whole frames at scale 1,
    /// so this is also every intermediate image's region.
    pub width: u32,
    pub height: u32,
}

impl Plan {
    pub fn output_key(&self) -> ContentHash {
        self.step_for(self.output)
            .map(Step::key)
            .expect("the output is produced by a step")
    }

    pub fn step_for(&self, image: ImageHandle) -> Option<&Step> {
        self.steps.iter().find(|s| s.output() == image)
    }

    /// A readable listing for `fw inspect`.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "plan: {} step(s), output %{} as {} at {}×{}, key {}",
            self.steps.len(),
            self.output.0,
            self.output_space,
            self.width,
            self.height,
            self.output_key().short()
        );
        for step in &self.steps {
            match step {
                Step::Load(load) => {
                    let frame = load
                        .frame
                        .map(|f| format!(" frame at {f}s"))
                        .unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "  %{} = load {}{frame} as {} ({}×{}, content {})  key {}",
                        load.output.0,
                        load.path.display(),
                        load.space,
                        load.width,
                        load.height,
                        load.fingerprint.short(),
                        load.key.short()
                    );
                }
                Step::Fill(fill) => {
                    let _ = writeln!(
                        out,
                        "  %{} = fill {:?} as {} ({}×{})  key {}",
                        fill.output.0,
                        fill.rgba,
                        fill.space,
                        fill.width,
                        fill.height,
                        fill.key.short()
                    );
                }
                Step::Kernel(kernel) => {
                    let fused = if kernel.ops.len() > 1 { ", fused" } else { "" };
                    let extra: String = kernel
                        .extra_inputs
                        .iter()
                        .map(|e| format!(", %{}", e.0))
                        .collect();
                    let _ = writeln!(
                        out,
                        "  %{} = kernel(%{}{extra}) {} op(s){fused}  key {}",
                        kernel.output.0,
                        kernel.input.0,
                        kernel.ops.len(),
                        kernel.key.short()
                    );
                    for op in &kernel.ops {
                        let params: Vec<String> =
                            op.params.iter().map(|p| format!("{p}")).collect();
                        let _ = writeln!(
                            out,
                            "      {} [{}]  ← {}",
                            op.label,
                            params.join(", "),
                            op.origin
                        );
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Load(LoadStep),
    Fill(FillStep),
    Kernel(KernelStep),
}

impl Step {
    pub fn output(&self) -> ImageHandle {
        match self {
            Step::Load(s) => s.output,
            Step::Fill(s) => s.output,
            Step::Kernel(s) => s.output,
        }
    }

    pub fn key(&self) -> ContentHash {
        match self {
            Step::Load(s) => s.key,
            Step::Fill(s) => s.key,
            Step::Kernel(s) => s.key,
        }
    }
}

/// Decode an image file, premultiply it and upload it.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadStep {
    pub output: ImageHandle,
    pub key: ContentHash,
    pub path: PathBuf,
    pub fingerprint: ContentHash,
    /// The space the pixels are in, after any override.
    pub space: ColorSpace,
    pub width: u32,
    pub height: u32,
    pub region: Rect,
    /// For video, the start time of the frame to decode; `None` for a still.
    pub frame: Option<fw_core::Time>,
    /// The node that loads it. Not part of the key: decoders use it to keep
    /// one reader per use of a file, so two clips of a file don't fight.
    pub node: Option<NodeId>,
}

/// An image of one colour, e.g. the transparent black of a timeline gap.
#[derive(Debug, Clone, PartialEq)]
pub struct FillStep {
    pub output: ImageHandle,
    pub key: ContentHash,
    /// Premultiplied, in `space`.
    pub rgba: [f32; 4],
    pub space: ColorSpace,
    pub width: u32,
    pub height: u32,
}

/// One GPU pass running one or more fused per-pixel operations.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelStep {
    pub output: ImageHandle,
    /// The key of the last fused operation, which is what this pass produces.
    pub key: ContentHash,
    pub input: ImageHandle,
    /// Images some ops read besides the main input, bound in this order.
    pub extra_inputs: Vec<ImageHandle>,
    pub ops: Vec<KernelOp>,
    /// The generated compute shader. Parameters come from a storage buffer
    /// holding every op's `params`, concatenated in order.
    pub wgsl: String,
}

impl KernelStep {
    pub fn params(&self) -> Vec<f32> {
        self.ops
            .iter()
            .flat_map(|op| op.params.iter().copied())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KernelOp {
    pub label: String,
    pub wgsl: String,
    pub params: Vec<f32>,
    pub origin: Origin,
    /// For each of the op's extra inputs (`e0`, `e1`, …), its index in the
    /// kernel's `extra_inputs`.
    pub extra: Vec<usize>,
}

/// Where an operation came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Node {
        /// Stable across recompiles: a hash of the node's path from the root.
        compiled_id: ContentHash,
        node: NodeId,
        name: String,
    },
    /// Inserted by the compiler to meet a node's colour-space needs.
    Conversion { from: ColorSpace, to: ColorSpace },
    /// A container combining its children, e.g. a timeline's tracks.
    Composite { container: NodeId, mode: String },
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Node {
                compiled_id, name, ..
            } => write!(f, "{name:?} ({})", compiled_id.short()),
            Origin::Conversion { from, to } => write!(f, "inserted: {from} → {to}"),
            Origin::Composite { mode, .. } => write!(f, "composite: {mode}"),
        }
    }
}
