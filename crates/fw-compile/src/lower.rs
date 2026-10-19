//! Lowering: from the document to a flat list of keyed operations, then to steps.

use std::collections::BTreeMap;

use fw_core::{ColorSpace, ContentHash, KeyHasher, NodeId, Rect, Time};
use fw_doc::{Document, KeyTime, Node, PortRef, Timeline, TrackKind};
use fw_plugin::{
    ImageHandle, ImageInfo, MediaRef, NodeError, OUTPUT_TYPE, PixelOp, PlanBuilder, Quality,
    Registry, Request, ResolvedParams, SpaceNeed,
};

use crate::codegen;
use crate::plan::{FillStep, KernelOp, KernelStep, LoadStep, Origin, Plan, Step};
use crate::{CompileError, MediaResolver, Options};

/// One operation in the flat graph. Its index is its [`ImageHandle`].
struct Op {
    kind: OpKind,
    info: ImageInfo,
    key: ContentHash,
}

enum OpKind {
    Load(LoadStep),
    Fill(FillStep),
    Pixel {
        input: ImageHandle,
        /// Further inputs the op reads at the same pixel.
        extra: Vec<ImageHandle>,
        op: PixelOp,
        origin: Origin,
    },
}

/// The clocks a node is evaluated against (design §5).
#[derive(Clone, Copy)]
struct Clock {
    /// What parameters resolve at: local time, plus the clip length for anchors.
    key: KeyTime,
    /// The moment of source media to show.
    source: Time,
}

impl Clock {
    /// Outside any clip all clocks are the document time.
    fn at(t: Time) -> Clock {
        Clock {
            key: KeyTime::at(t),
            source: t,
        }
    }

    fn hash_into(&self, h: &mut KeyHasher) {
        h.time(self.key.local).time(self.source);
        match self.key.length {
            Some(l) => h.u8(1).time(l),
            None => h.u8(0),
        };
    }
}

struct Lowering<'a> {
    doc: &'a Document,
    registry: &'a Registry,
    media: &'a dyn MediaResolver,
    options: &'a Options,
    ops: Vec<Op>,
    /// Nodes already lowered, by node and request, so shared upstream nodes are emitted once.
    done: BTreeMap<(NodeId, ContentHash), ImageHandle>,
}

pub fn compile(
    doc: &Document,
    registry: &Registry,
    media: &dyn MediaResolver,
    options: &Options,
) -> Result<Plan, CompileError> {
    let root = doc.root();
    let mut lowering = Lowering {
        doc,
        registry,
        media,
        options,
        ops: vec![],
        done: BTreeMap::new(),
    };
    let output = if let Some(graph) = doc.graph(root) {
        let outputs: Vec<NodeId> = graph
            .nodes
            .iter()
            .copied()
            .filter(|id| doc.node(*id).map(Node::type_id) == Some(OUTPUT_TYPE))
            .collect();
        let output_node = match outputs.as_slice() {
            [] => return Err(CompileError::NoOutput),
            [one] => *one,
            many => return Err(CompileError::SeveralOutputs(many.len())),
        };
        let request = Request {
            time: options.time,
            region: Rect::INFINITE,
            scale: 1.0,
            quality: options.quality,
        };
        lowering.node(&[root], output_node, &request, Clock::at(options.time))?
    } else if let Some(timeline) = doc.timeline(root) {
        lowering.timeline(root, timeline, options.time)?
    } else {
        return Err(CompileError::Root(
            "it must be a Graph or a Timeline of a known type".into(),
        ));
    };
    lowering.into_plan(output)
}

impl Lowering<'_> {
    /// Lowers a Timeline at parent time `t`: active video clips stack bottom
    /// to top, each combined with what's below by its composite mode, in
    /// scene-linear light. A gap is transparent black.
    fn timeline(
        &mut self,
        id: NodeId,
        timeline: &Timeline,
        t: Time,
    ) -> Result<ImageHandle, CompileError> {
        let settings = &timeline.settings;
        let canvas = Rect::from_size(settings.width as f64, settings.height as f64);
        let working = self.options.working_space;
        let video = timeline
            .tracks
            .iter()
            .rev()
            .filter(|track| track.kind == TrackKind::Video);
        // Top first, stopping at a layer that hides everything below it.
        let mut layers: Vec<(ImageHandle, String)> = vec![];
        for track in video {
            let Some((clip, placement)) = timeline.clip_at(track.id, t) else {
                continue;
            };
            let at = placement
                .clip_time(t)
                .ok_or_else(|| CompileError::Root("clip time overflowed".into()))?;
            let request = Request {
                time: at.local,
                region: canvas,
                scale: 1.0,
                quality: self.options.quality,
            };
            let clock = Clock {
                key: KeyTime {
                    local: at.local,
                    length: Some(at.length),
                },
                source: at.source,
            };
            let image = self.node(&[id], clip, &request, clock)?;
            let region = self.ops[image.0 as usize].info.region;
            if region != canvas {
                // TODO: placement is a transform, not a resample (design §4).
                return Err(self.node_error(
                    clip,
                    format!(
                        "media of {}×{} can't be placed on a {}×{} timeline yet",
                        region.width(),
                        region.height(),
                        settings.width,
                        settings.height
                    ),
                ));
            }
            let mode = self.composite_mode(clip, clock)?;
            let hides_below = mode == "normal" && self.ops[image.0 as usize].info.opaque;
            layers.push((image, mode));
            if hides_below {
                break;
            }
        }
        let delivery = self.options.delivery_space;
        match layers.len() {
            0 => return Ok(self.fill([0.0; 4], delivery, canvas)),
            // Nothing to blend, so go straight to the delivery space.
            1 => return Ok(self.convert(layers[0].0, delivery)),
            _ => {}
        }
        let mut bottom_up = layers.into_iter().rev();
        let (bottom, _) = bottom_up.next().expect("checked above");
        // Over transparent black every mode leaves the bottom layer as it is.
        let mut stack = self.convert(bottom, working);
        for (layer, mode) in bottom_up {
            let layer = self.convert(layer, working);
            stack = self.composite(id, stack, layer, &mode);
        }
        Ok(self.convert(stack, delivery))
    }

    /// The clip's composite mode at this moment. Clips without one are "normal".
    fn composite_mode(&self, clip: NodeId, clock: Clock) -> Result<String, CompileError> {
        let Some(Node::Known(known)) = self.doc.node(clip) else {
            return Ok("normal".into());
        };
        let Some(stored) = known.params.get("composite") else {
            return Ok("normal".into());
        };
        let value = stored
            .value_at(clock.key)
            .map_err(|e| self.node_error(clip, format!("parameter \"composite\": {e}")))?;
        let mode = value.as_text().unwrap_or("normal").to_owned();
        if codegen::composite(&mode).is_none() {
            return Err(self.node_error(clip, format!("unknown composite mode {mode:?}")));
        }
        Ok(mode)
    }

    fn composite(
        &mut self,
        container: NodeId,
        below: ImageHandle,
        above: ImageHandle,
        mode: &str,
    ) -> ImageHandle {
        let op = codegen::composite(mode).expect("modes are checked when read");
        let (below_info, above_info) = (&self.ops[below.0 as usize], &self.ops[above.0 as usize]);
        let key = KeyHasher::new("fw.composite")
            .hash(&below_info.key)
            .hash(&above_info.key)
            .str(mode)
            .finish();
        let info = ImageInfo {
            opaque: below_info.info.opaque || above_info.info.opaque,
            ..below_info.info.clone()
        };
        self.push(Op {
            kind: OpKind::Pixel {
                input: below,
                extra: vec![above],
                op,
                origin: Origin::Composite {
                    container,
                    mode: mode.to_owned(),
                },
            },
            info,
            key,
        })
    }

    fn node_error(&self, node: NodeId, message: String) -> CompileError {
        CompileError::Node {
            node,
            name: self
                .doc
                .node(node)
                .map(|n| n.name().to_owned())
                .unwrap_or_default(),
            message,
        }
    }

    fn fill(&mut self, rgba: [f32; 4], space: ColorSpace, region: Rect) -> ImageHandle {
        let mut key = KeyHasher::new("fw.fill");
        for v in rgba {
            key.f64(v as f64);
        }
        space.hash_into(&mut key);
        let (width, height) = (region.width() as u32, region.height() as u32);
        key.u32(width).u32(height);
        let key = key.finish();
        let output = ImageHandle(self.ops.len() as u32);
        self.push(Op {
            kind: OpKind::Fill(FillStep {
                output,
                key,
                rgba,
                space,
                width,
                height,
            }),
            info: ImageInfo {
                space,
                region,
                pixel_aspect: 1.0,
                extra_channels: vec![],
                opaque: rgba[3] >= 1.0,
            },
            key,
        })
    }

    /// Lowers `id`, found inside the containers on `path`, and returns its output.
    fn node(
        &mut self,
        path: &[NodeId],
        id: NodeId,
        request: &Request,
        clock: Clock,
    ) -> Result<ImageHandle, CompileError> {
        let mut done_key = KeyHasher::new("fw.lowered");
        done_key.hash(&request_key(request));
        clock.hash_into(&mut done_key);
        let request_key = done_key.finish();
        if let Some(done) = self.done.get(&(id, request_key)) {
            return Ok(*done);
        }
        let doc = self.doc;
        let node = doc.node(id).expect("graph members exist");
        let fail = |message: String| CompileError::Node {
            node: id,
            name: node.name().to_owned(),
            message,
        };
        let known = match node {
            Node::Known(k) => k,
            Node::Unknown(u) => {
                return Err(fail(format!(
                    "node type {} version {} isn't available",
                    u.type_id, u.version
                )));
            }
        };
        if known.contents.is_some() {
            // TODO: lower nested containers (design §4: containers lower themselves).
            return Err(fail("nested containers can't be rendered yet".into()));
        }
        let ty = self
            .registry
            .get(&known.type_id)
            .ok_or_else(|| fail(format!("node type {} isn't registered", known.type_id)))?;
        let behaviour = ty
            .image
            .clone()
            .ok_or_else(|| fail(format!("{} doesn't produce images", ty.desc.name)))?;

        let mut params = ResolvedParams::default();
        for p in &ty.desc.params {
            let value = match known.params.get(&p.id) {
                Some(stored) => stored
                    .value_at(clock.key)
                    .map_err(|e| fail(format!("parameter {:?}: {e}", p.id)))?,
                None => p.default.clone(),
            };
            params.0.insert(p.id.clone(), value);
        }

        let input_requests = behaviour.requirements(&params, request, ty.desc.inputs.len());
        let mut inputs = vec![];
        let mut node_path = path.to_vec();
        node_path.push(id);
        for (index, (port, input_request)) in ty.desc.inputs.iter().zip(&input_requests).enumerate()
        {
            // TODO: optional inputs, once a node has one.
            let source = doc
                .source_of(&PortRef::new(id, &port.id))
                .ok_or_else(|| fail(format!("input {:?} is not connected", port.name)))?
                .clone();
            let image = self.node(path, source.node, input_request, clock)?;
            let wanted = match behaviour.input_space(&params, index) {
                SpaceNeed::Working => self.options.working_space,
                SpaceNeed::Exact(space) => space,
                SpaceNeed::Delivery => self.options.delivery_space,
            };
            inputs.push(self.convert(image, wanted));
        }

        let mut key = KeyHasher::new("fw.node");
        key.str(&ty.desc.id)
            .u32(ty.desc.version)
            .hash(&ty.impl_hash);
        for (name, value) in &params.0 {
            key.str(name);
            value.hash_into(&mut key);
        }
        for input in &inputs {
            key.hash(&self.ops[input.0 as usize].key);
        }
        key.hash(&request_key_without_time(request));
        if ty.desc.varies_with_time {
            key.time(request.time);
        }

        let compiled_id = path_hash(&node_path);
        let mut builder = NodeBuilder {
            lowering: self,
            node_key: key.finish(),
            origin: Origin::Node {
                compiled_id,
                node: id,
                name: known.name.clone(),
            },
            next_op: 0,
            bad_handle: None,
            source_time: clock.source,
        };
        let result = behaviour.emit(&params, request, &inputs, &mut builder);
        let bad_handle = builder.bad_handle;
        let output = result.map_err(|NodeError(message)| fail(message))?;
        if let Some(h) = bad_handle.or((output.0 as usize >= self.ops.len()).then_some(output)) {
            return Err(fail(format!("used image %{} that doesn't exist", h.0)));
        }
        self.done.insert((id, request_key), output);
        Ok(output)
    }

    fn convert(&mut self, image: ImageHandle, to: fw_core::ColorSpace) -> ImageHandle {
        let from = self.ops[image.0 as usize].info.space;
        let Some(op) = codegen::conversion(from, to) else {
            return image;
        };
        let mut key = KeyHasher::new("fw.convert");
        key.hash(&self.ops[image.0 as usize].key);
        from.hash_into(&mut key);
        to.hash_into(&mut key);
        let info = ImageInfo {
            space: to,
            ..self.ops[image.0 as usize].info.clone()
        };
        self.push(Op {
            kind: OpKind::Pixel {
                input: image,
                extra: vec![],
                op,
                origin: Origin::Conversion { from, to },
            },
            info,
            key: key.finish(),
        })
    }

    fn push(&mut self, op: Op) -> ImageHandle {
        self.ops.push(op);
        ImageHandle(self.ops.len() as u32 - 1)
    }

    /// Keeps the operations the output needs and fuses per-pixel chains.
    fn into_plan(self, output: ImageHandle) -> Result<Plan, CompileError> {
        let n = self.ops.len();
        let mut live = vec![false; n];
        let mut consumers = vec![0u32; n];
        let mut stack = vec![output];
        while let Some(h) = stack.pop() {
            let i = h.0 as usize;
            if std::mem::replace(&mut live[i], true) {
                continue;
            }
            if let OpKind::Pixel { input, extra, .. } = &self.ops[i].kind {
                for h in std::iter::once(input).chain(extra) {
                    consumers[h.0 as usize] += 1;
                    stack.push(*h);
                }
            }
        }

        let region = self.ops[output.0 as usize].info.region;
        let output_space = self.ops[output.0 as usize].info.space;
        if !region.is_finite() || region.is_empty() {
            return Err(CompileError::InfiniteOutput);
        }

        let mut steps: Vec<Step> = vec![];
        // For each fused op, the index of the kernel step it belongs to.
        let mut kernel_of: BTreeMap<usize, usize> = BTreeMap::new();
        for (i, op) in self.ops.into_iter().enumerate() {
            if !live[i] {
                continue;
            }
            match op.kind {
                OpKind::Load(load) => steps.push(Step::Load(load)),
                OpKind::Fill(fill) => steps.push(Step::Fill(fill)),
                OpKind::Pixel {
                    input,
                    extra,
                    op: pixel,
                    origin,
                } => {
                    let mut kernel_op = KernelOp {
                        label: pixel.label,
                        wgsl: pixel.wgsl,
                        params: pixel.params,
                        origin,
                        extra: vec![],
                    };
                    let input_i = input.0 as usize;
                    // Fuse into the input's kernel if nothing else needs the
                    // input, and every extra input is ready before that kernel runs.
                    let fuse_into = kernel_of
                        .get(&input_i)
                        .copied()
                        .filter(|_| consumers[input_i] == 1)
                        .filter(|s| {
                            extra.iter().all(|e| {
                                steps
                                    .iter()
                                    .position(|st| st.output() == *e)
                                    .is_some_and(|p| p < *s)
                            })
                        });
                    let step_index = match fuse_into {
                        Some(s) => s,
                        None => {
                            steps.push(Step::Kernel(KernelStep {
                                output: ImageHandle(i as u32),
                                key: op.key,
                                input,
                                extra_inputs: vec![],
                                ops: vec![],
                                wgsl: String::new(),
                            }));
                            steps.len() - 1
                        }
                    };
                    let Step::Kernel(k) = &mut steps[step_index] else {
                        unreachable!("only kernels are fused into")
                    };
                    for e in extra {
                        let slot = match k.extra_inputs.iter().position(|x| *x == e) {
                            Some(slot) => slot,
                            None => {
                                k.extra_inputs.push(e);
                                k.extra_inputs.len() - 1
                            }
                        };
                        kernel_op.extra.push(slot);
                    }
                    k.ops.push(kernel_op);
                    k.output = ImageHandle(i as u32);
                    k.key = op.key;
                    kernel_of.insert(i, step_index);
                }
            }
        }
        for step in &mut steps {
            if let Step::Kernel(k) = step {
                k.wgsl = codegen::kernel(&k.ops, k.extra_inputs.len());
            }
        }
        Ok(Plan {
            steps,
            output,
            output_space,
            width: region.width().round() as u32,
            height: region.height().round() as u32,
        })
    }
}

/// The plan builder handed to one node's `emit`.
struct NodeBuilder<'b, 'a> {
    lowering: &'b mut Lowering<'a>,
    node_key: ContentHash,
    origin: Origin,
    next_op: u32,
    bad_handle: Option<ImageHandle>,
    /// Media loads show this moment of their source.
    source_time: Time,
}

impl PlanBuilder for NodeBuilder<'_, '_> {
    fn info(&self, image: ImageHandle) -> &ImageInfo {
        &self.lowering.ops[image.0 as usize].info
    }

    fn load(
        &mut self,
        media: &MediaRef,
        space: Option<fw_core::ColorSpace>,
    ) -> Result<ImageHandle, NodeError> {
        let found = self.lowering.media.probe(media).map_err(NodeError)?;
        if found.width == 0 || found.height == 0 {
            return Err(NodeError(format!("{} has no picture", media.path)));
        }
        let space = space.unwrap_or(found.space);
        let frame = match &found.frames {
            Some(frames) => Some(
                frames
                    .frame_at(self.source_time)
                    .ok_or_else(|| NodeError("the video has no frames".into()))?,
            ),
            None => None,
        };
        let mut key = KeyHasher::new("fw.load");
        key.hash(&found.fingerprint)
            .u32(found.width)
            .u32(found.height);
        space.hash_into(&mut key);
        if let Some(frame) = frame {
            key.time(frame);
        }
        let region = Rect::from_size(found.width as f64, found.height as f64);
        let info = ImageInfo {
            space,
            region,
            pixel_aspect: 1.0,
            extra_channels: vec![],
            opaque: found.opaque,
        };
        let key = key.finish();
        let output = ImageHandle(self.lowering.ops.len() as u32);
        Ok(self.lowering.push(Op {
            kind: OpKind::Load(LoadStep {
                output,
                key,
                path: found.path,
                fingerprint: found.fingerprint,
                space,
                width: found.width,
                height: found.height,
                region,
                frame,
                node: match &self.origin {
                    Origin::Node { node, .. } => Some(*node),
                    _ => None,
                },
            }),
            info,
            key,
        }))
    }

    fn per_pixel_with(
        &mut self,
        input: ImageHandle,
        extra: &[ImageHandle],
        op: PixelOp,
    ) -> ImageHandle {
        let ops = &self.lowering.ops;
        if let Some(bad) = std::iter::once(&input)
            .chain(extra)
            .find(|h| h.0 as usize >= ops.len())
        {
            self.bad_handle = Some(*bad);
            return input;
        }
        let info = ImageInfo {
            // A node's own maths might change alpha; only it could say otherwise.
            opaque: false,
            ..ops[input.0 as usize].info.clone()
        };
        let key = KeyHasher::new("fw.op")
            .hash(&self.node_key)
            .u32(self.next_op)
            .finish();
        self.next_op += 1;
        self.lowering.push(Op {
            kind: OpKind::Pixel {
                input,
                extra: extra.to_vec(),
                op,
                origin: self.origin.clone(),
            },
            info,
            key,
        })
    }
}

fn path_hash(path: &[NodeId]) -> ContentHash {
    let mut h = KeyHasher::new("fw.path");
    for id in path {
        h.node_id(*id);
    }
    h.finish()
}

fn request_key_without_time(r: &Request) -> ContentHash {
    let mut h = KeyHasher::new("fw.request");
    h.f64(r.region.min.x)
        .f64(r.region.min.y)
        .f64(r.region.max.x)
        .f64(r.region.max.y)
        .f64(r.scale)
        .u8(match r.quality {
            Quality::Draft => 0,
            Quality::Final => 1,
        });
    h.finish()
}

fn request_key(r: &Request) -> ContentHash {
    KeyHasher::new("fw.request-at")
        .hash(&request_key_without_time(r))
        .time(r.time)
        .finish()
}
