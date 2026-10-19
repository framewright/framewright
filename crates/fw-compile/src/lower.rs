//! Lowering: from the document to a flat list of keyed operations, then to steps.

use std::collections::BTreeMap;

use fw_core::{ContentHash, KeyHasher, NodeId, Rect};
use fw_doc::{Document, Node, PortRef};
use fw_plugin::{
    ImageHandle, ImageInfo, MediaRef, NodeError, OUTPUT_TYPE, PixelOp, PlanBuilder, Quality,
    Registry, Request, ResolvedParams, SpaceNeed,
};

use crate::codegen;
use crate::plan::{KernelOp, KernelStep, LoadStep, Origin, Plan, Step};
use crate::{CompileError, MediaResolver, Options};

/// One operation in the flat graph. Its index is its [`ImageHandle`].
struct Op {
    kind: OpKind,
    info: ImageInfo,
    key: ContentHash,
}

enum OpKind {
    Load(LoadStep),
    Pixel {
        input: ImageHandle,
        op: PixelOp,
        origin: Origin,
    },
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
    let graph = doc.graph(root).ok_or(CompileError::NoOutput)?;
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
    let mut lowering = Lowering {
        doc,
        registry,
        media,
        options,
        ops: vec![],
        done: BTreeMap::new(),
    };
    let output = lowering.node(&[root], output_node, &request)?;
    lowering.into_plan(output)
}

impl Lowering<'_> {
    /// Lowers `id`, found inside the containers on `path`, and returns its output.
    fn node(
        &mut self,
        path: &[NodeId],
        id: NodeId,
        request: &Request,
    ) -> Result<ImageHandle, CompileError> {
        let request_key = request_key(request);
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
        if known.graph.is_some() {
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
                    .value_at(request.time)
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
            let image = self.node(path, source.node, input_request)?;
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
            if let OpKind::Pixel { input, .. } = &self.ops[i].kind {
                consumers[input.0 as usize] += 1;
                stack.push(*input);
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
                OpKind::Pixel {
                    input,
                    op: pixel,
                    origin,
                } => {
                    let kernel_op = KernelOp {
                        label: pixel.label,
                        wgsl: pixel.wgsl,
                        params: pixel.params,
                        origin,
                    };
                    let input_i = input.0 as usize;
                    let fuse_into = kernel_of
                        .get(&input_i)
                        .copied()
                        .filter(|_| consumers[input_i] == 1);
                    let step_index = match fuse_into {
                        Some(s) => {
                            let Step::Kernel(k) = &mut steps[s] else {
                                unreachable!()
                            };
                            k.ops.push(kernel_op);
                            k.output = ImageHandle(i as u32);
                            k.key = op.key;
                            s
                        }
                        None => {
                            steps.push(Step::Kernel(KernelStep {
                                output: ImageHandle(i as u32),
                                key: op.key,
                                input,
                                ops: vec![kernel_op],
                                wgsl: String::new(),
                            }));
                            steps.len() - 1
                        }
                    };
                    kernel_of.insert(i, step_index);
                }
            }
        }
        for step in &mut steps {
            if let Step::Kernel(k) = step {
                k.wgsl = codegen::kernel(&k.ops);
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
        let space = space.unwrap_or(found.space);
        let mut key = KeyHasher::new("fw.load");
        key.hash(&found.fingerprint)
            .u32(found.width)
            .u32(found.height);
        space.hash_into(&mut key);
        let region = Rect::from_size(found.width as f64, found.height as f64);
        let info = ImageInfo {
            space,
            region,
            pixel_aspect: 1.0,
            extra_channels: vec![],
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
            }),
            info,
            key,
        }))
    }

    fn per_pixel(&mut self, input: ImageHandle, op: PixelOp) -> ImageHandle {
        let Some(input_op) = self.lowering.ops.get(input.0 as usize) else {
            self.bad_handle = Some(input);
            return input;
        };
        let info = input_op.info.clone();
        let key = KeyHasher::new("fw.op")
            .hash(&self.node_key)
            .u32(self.next_op)
            .finish();
        self.next_op += 1;
        self.lowering.push(Op {
            kind: OpKind::Pixel {
                input,
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
