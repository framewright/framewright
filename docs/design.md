# Framewright — Design Foundations

Living document. Started 2026-10-19.

Every item is tagged:

- **[Decided]**: agreed; change only with a deliberate revisit.
- **[Decided]**: recommended and not objected to, but not explicitly confirmed.
- **[Open]**: needs a decision.

UI mockups (Studio direction): https://claude.ai/artifact/Rm9UTcSdBYVVyXRSQY7GWb

---

## 1. Goals

- **[Decided]** A long-term unified photo and video editor with light 3D. The aim is to replace Photoshop and DaVinci Resolve for the author's own needs: a narrower feature set, but no compromise on the foundations.
- **[Decided]** Foundations first. Don't choose features up front; build the strongest base that allows the most future scope.
- **[Decided]** Rust. Linux first; cross-platform is nice to have.
- **[Decided]** Extensibility is a first-class requirement.
- **[Decided]** A polished, professional UI early on, because design decisions happen by using the app.

**[Decided] Non-goals (for now):** 3D modelling (that's Blender's job), collaboration and cloud features, and matching Resolve or Photoshop feature for feature. These are "not now", not "never": the design must not rule any of them out. For example, stable IDs plus a command log keep real-time collaboration possible later.

## 2. Stress scenarios

**[Decided]** The architecture must answer each of these on paper, even before any of them is built:

1. A 100 MP RAW photo with 30 non-destructive adjustment layers stays interactive.
2. A 4K multi-track timeline with nested comps plays back smoothly at proxy resolution.
3. A "lower third" template is used 40 times with different text, and one edit updates all of them.
4. 3D text is composited into footage with motion blur and a matched camera.
5. A user-written shader node appears in every workspace without recompiling the app.
6. A 2-hour project opens instantly, undo works across everything, and a crash loses nothing.
7. A headless batch render is bit-identical on the same machine and driver, and within a tight tolerance on any other machine.
8. A dialogue edit with buses, plugins and loudness targeting plays back glitch-free.

## 3. Product model

### 3.1 Projects, documents and workspaces

- **[Decided]** A **project** is a media library plus any number of **documents**.
- **[Decided]** Each document has a **kind**, and the kind decides the workspace bar. Photo is not a workspace in a video document, and Edit is not a workspace in a photo document.

| Kind | Workspaces |
|---|---|
| Video | Media · Edit · Compose · Grade · 3D · Audio · Deliver |
| Photo | Library · Develop · Photo · Compose · Export |
| Audio-only | Media · Edit · Mix · Deliver |

- **[Decided]** Compose (the node view) is available in every kind, because it's "the graph of whatever you're in".
- **[Decided]** Container kinds declare the workspaces they support. Plugins can add new container kinds and their workspaces.

### 3.2 Editing across kinds by nesting

- **[Decided]** Working on one kind of content inside another means **stepping into a nested container**, not switching apps. The workspace bar changes to the inner kind's set, and a back control returns to the parent.
- **[Decided]** From a video clip there are three ways in:
  - **Open in Compose:** the clip's node graph.
  - **Open Clip in Photo:** layer-based retouching of the whole clip. A toggle sets whether strokes apply to **every frame** or **follow motion** (tracked, rotoscope-style).
  - **A still from the current frame:** a separate Photo document, linked back to the clip. The proposed label is "New Photo from Frame".
- **[Decided]** A Photo document with a video layer offers **Open in Edit**, the same mechanism in reverse.
- **[Decided]** Inside a nested context, workspaces that don't apply are hidden. For example, Library and Export aren't shown inside a clip, because the result flows back to the parent.
- **[Decided]** The navigation stack is **UI state**, not document state, so it never touches undo or saving. The back control always names its destination ("← Main Timeline"), because nesting can be deep.
- **[Open]** Tracked "follow motion" strokes are a large feature. Design for them now, and build them later.

### 3.3 Naming rules

- **[Decided]** Naming of actions and options gets deliberate care. Labels in the mockups are placeholders.
- **[Decided]** **"Open X"** means stepping into the same thing. **"New X"** means creating a separate thing. Never mix the two.

### 3.4 Visual direction

- **[Decided]** The "Studio" direction: Resolve-like fixed pages with a workspace bar along the bottom, IBM Plex Sans and Mono, charcoal greys and a single amber accent (`#e8a33d`).
- **[Decided]** Every screen uses one control vocabulary: label, slider with a thin handle, monospaced value field, keyframe diamond, and collapsible chevron sections.
- **[Decided]** Panels stay edge to edge with no boxes inside them, and colour is used only where it carries meaning (selection, playhead, data).
- **[Decided]** The current Grade layout is good enough for now; two alternatives were tried and rejected.
- **[Decided]** Fixed page layouts are the default, built on a docking system so users can save their own arrangements.

## 4. Core model

- **[Decided]** Everything is a node, and containers nest without limit (Fusion clips everywhere). This came from the old prototype.
- **[Decided]** Containers are typed: **Graph, Timeline, LayerStack, Scene3D, Mixer**. They share one node interface (typed ports, parameters, evaluation), but each stores its contents in its own structure and has its own editor. Not everything is literally a node graph.
- **[Decided]** The nested **authoring tree compiles to one flat evaluation graph**. Time remaps get folded in, passthroughs are pruned and compatible operations are fused.
  - Node IDs in the compiled graph are a hash of the node's path from the root, so they stay stable across recompiles.
  - Diffing two plans gives exactly which time ranges to re-render.
  - The old prototype proved this works.
- **[Decided]** **Definitions vs instances.** A container is either inline (owned by its parent) or a shared definition referenced from many places. It acts like a function with published parameters. Reference cycles are rejected when the edit is made.
- **[Decided]** **Context flows down, results flow up.** Each container boundary transforms time (offset, speed, retime curve), space (each container has its own canvas, and placement is a transform rather than a resample), quality (proxy level, region of interest) and colour context.
- **[Decided]** **Typed ports:** image, mask, audio (with a channel layout), scene, plus later types such as geometry or text. Connections live in the container's structure, never inside a node's opaque payload.
- **[Decided]** **Plain clips are cheap.** A clip with no graph is stored compactly as "media → output" and only becomes a real graph when opened.
- **[Decided]** (from step 1) Until a container type can lower itself, nested containers and optional inputs stop compilation with a clear "not yet" error that names the node, rather than rendering something wrong.

### 4.1 Node interface

- **[Decided]** **Describe, don't execute.** Nodes never touch pixels. They add operations to a plan, and the engine runs the plan. This is what makes fusing, caching, region-of-interest rendering, determinism and plugin parity possible.
- **[Decided]** A node type is described by plain data (`NodeTypeDesc`: id, version, typed ports, a parameter schema, capabilities), so WASM and shader plugins can provide one as easily as Rust code.
- **[Decided]** **Image behaviour runs in two phases.**
  1. `requirements(params, request)` says what the node needs from its inputs. A blur asks for a bigger region, a retime asks for a different time, a temporal filter asks for neighbouring frames.
  2. `emit(params, request, input handles, plan)` adds operations and returns a handle.

  A sensible default ("same time, same region") means simple nodes implement nothing for the first phase.
- **[Decided]** **Containers lower themselves** into simpler nodes: a Timeline into time-remap and switch nodes, a LayerStack into merges, a Mixer into gain, pan and bus nodes. That lowering is the compile step. Editor and workspace registration for a container lives on the UI side.
- **[Decided]** **Audio behaviour is a separate real-time contract** (`prepare`, `process`, `latency`), with no allocation or locks inside `process`. A node may have image behaviour, audio behaviour, or both.
- **[Decided]** **Nodes never see animation.** The engine resolves keyframes, expressions and automation into plain values at the requested time before calling the node.
- **[Decided]** **The parameter schema is the single source of truth** for the inspector UI, saving, the Python API, expressions and keyframing.
- **[Decided]** **The interface works across a plugin boundary:** data in, descriptions out, with no Rust-only features such as generics or borrowed references in the core contract. Rust convenience macros sit on top for built-ins.

### 4.2 Ports and parameters

- **[Decided]** **Port types:**
  - **Image:** pixels plus colour-space tag, premultiplied alpha, region of definition, pixel aspect, optional extra channels, pass-through metadata.
  - **Mask:** single-channel image, its own type.
  - **Audio:** f32 samples with a channel layout, at one session sample rate (sources resampled at input).
  - **Scene:** a 3D scene handle, so 3D nodes can be chained before a render node.
  - **Value:** numbers, vectors, colours, curves, tracks.
- **[Decided]** **Colour-tagged ports with automatic conversion.** A node declares the space it works in (scene-linear, log/ACEScct, display-referred), and the compiler inserts conversions and fuses adjacent ones. Nodes never do colour maths themselves.
- **[Decided]** **Always premultiplied alpha** inside the engine. Straight alpha exists only at file input and output.
- **[Decided]** **Region of definition on every image, no implicit cropping.** Pixels past the canvas are kept, generators can be infinite, and edge filters read real pixels.
- **[Decided]** **Optional extra channels** (depth, motion, IDs, normals), allocated only when something produces them.
- **[Decided]** **Value ports and parameter linking** (tracker to transform, audio level to parameter). Links are graph edges, so they get the same caching and invalidation as everything else.
- **[Decided]** **Resolution-independent units.** Positions are in canvas units, never pixels; times are rational time, never frame numbers.
- **[Decided]** **Parameter kinds:**
  - scalars with units (px, degrees, seconds, dB, EV, ratio), int, bool, choice
  - vectors; colours (with colour space, HDR values allowed)
  - point, rect, transform
  - curve, gradient
  - text, font, media and file references
- **[Decided]** **The schema declares, for each parameter:** default, soft and hard range, display hint, inspector section, whether it can be animated, and an **on-viewer handle**, so plugins get gizmos without writing UI code.
- **[Decided]** **A stored value is one of:** a constant, a keyframe curve, an expression, or a link to a Value output.

### 4.3 Evaluation and caching contract

- **[Decided]** **The purity rule.** A node's output depends only on: its type, version and implementation hash; its resolved parameters at the requested time; its inputs; the request (time, region, scale, quality); and declared resources identified by content (media, fonts, LUTs, colour config). No clock, global state, unseeded randomness, environment variables, or undeclared file reads. WASM sandboxing enforces this for plugins; tests enforce it for built-ins.
- **[Decided]** **The cache key is a hash of exactly that list.** Nothing is ever explicitly invalidated. The implementation hash means plugin or app updates miss automatically.
- **[Decided]** **Things that don't change aren't re-rendered.** Nodes declare whether they vary over time, and constant parameters don't change the key, so a static layer under a long clip renders once.
- **[Decided]** **Media is identified by a content fingerprint,** checked against size and modification time, not by path. Relinking keeps the cache.
- **[Decided]** **Determinism:** bit-identical on the same machine and driver, within a tight tolerance across machines. Golden tests use tolerances. Randomness is always seeded from the node's path plus a user seed.
- **[Decided]** **Three cache tiers:** GPU memory, RAM, and a persistent, bounded disk cache with least-recently-used eviction. What gets cached is chosen from measured cost, not every intermediate.
- **[Decided]** **Volatile sources** (live input) are never cached across time. **Sinks** (outputs, file writes) are kept out of the pure graph. **No feedback loops,** except through explicit simulate/bake nodes.
- **[Decided]** **Cancellation, priority and progressive refinement:** scrubbing cancels stale requests, interactive requests beat background renders, and the viewer refines from low resolution.
- **[Decided]** **Errors are values:** a placeholder with the message in the viewer, a clear failure in render jobs, and offline media with a relink prompt. The engine and app never crash because of a node.

### 4.4 Plan and execution model

- **[Decided]** A frame is made in three steps:
  1. **Specialise:** resolve parameters, compute keys, and prune cached subtrees.
  2. **Schedule:** order the work, allocate memory from known lifetimes, batch submissions.
  3. **Execute:** in slices, pipelined across frames during playback.
- **[Decided]** **The plan is a render graph with known resource lifetimes.** Transient textures are pooled and aliased.
- **[Decided]** **Automatic fusion of per-pixel operations** into generated shaders. Operations that read neighbouring pixels stay as separate passes.
- **[Decided]** **Adaptive tiling and a power-of-two scale pyramid.** Normal frames render whole. Big stills and zoomed views render visible tiles only, with the margin each filter needs taken from its requirements.
- **[Decided]** **Sliced GPU submission** of a few milliseconds each, so heavy engine work never freezes the UI.
- **[Decided]** **The playback pipeline:** a decode thread pool feeding GPU uploads through ring buffers; read-ahead in the play direction and speed; rendering several frames ahead; the audio clock selecting the displayed frame; dropped frames shown with a visible indicator rather than stalling; optional background rendering of heavy sections into the disk cache.
- **[Decided]** **The audio graph runs on a real-time thread.** New graphs are compiled off-thread and swapped in without locks at a block boundary, and old ones are freed off-thread. Automation is pre-computed per block, and disk streaming has its own thread. Visuals that react to audio use offline analysis through Value ports.
- **[Decided]** **Export is the same plan at final quality.** There is no separate export path; encoding is pipelined in parallel.
- **[Decided]** **Operations are tagged CPU or GPU,** and the scheduler handles transfers. Multi-GPU isn't planned, but the plan stays device-agnostic.

## 5. Time

- **[Decided]** Time is exact rational numbers everywhere. Floating-point time is never used. Overflow is an error, never silently clamped (the old prototype clamped silently).
- **[Decided]** Time resolves exactly to **frames and to audio samples**.
- **[Decided]** Each placement has three clocks: position in the parent, animation time, and source time.
- **[Decided]** Keyframes are anchored to a clip's start, end or a percentage of its length, so trimming and retiming behave predictably.
- **[Decided]** Variable frame rate is kept as a fact about the source: exact per-frame timestamps, and footage is never rewritten to a constant rate.

## 6. Colour

- **[Decided]** The working space is scene-linear float (ACEScg-style), with premultiplied RGBA16F as the default and RGBA32F where precision needs it. 8-bit exists only at input and output edges.
- **[Decided]** **Input, working, display and export are separate colour boundaries.** Changing the monitor setup never changes exported bytes.
- **[Decided]** We write our own colour core (matrices, transfer functions, LUTs as GPU operations) using OCIO-compatible concepts. OCIO configs are scoped to the project and their fingerprint goes into cache keys. The `$OCIO` environment variable is never read. ICC support via `lcms2` for photo and print work.
- **[Decided]** (from step 1) The Output node's colour space defaults to **Auto**, meaning the delivery space. A render chooses it from the output file type (EXR: linear Rec.709, PNG: sRGB) at compile time, so it is part of the plan and its cache keys.
- **Note** (measured in step 1): half-float intermediates passed through a large-coefficient matrix (ACEScg → Rec.709 has entries up to 1.7, with heavy cancellation) and then the sRGB curve's 12.92× linear segment lose about 1–2% on near-zero and negative values. This is the case "RGBA32F where precision needs it" is for. Nothing needs doing yet.
- **[Decided]** One delivery spec drives both the final pixel transform and the container metadata, so the two can't drift apart.

## 7. Rendering and caching

- **[Decided]** wgpu for all GPU work.
- **[Decided]** **The engine creates and owns the GPU device. The UI is a guest.** The UI toolkit must be able to render on a device we provide, and give us control over swapchain format (10-bit and HDR) and present mode. This lesson comes from the old prototype, where GPUI owning the device broke 10-bit output and HDR monitoring.
- **[Decided]** Nodes work in image-space coordinates rather than pixels. Rendering is region-of-interest and tiled, so proxies and huge stills come almost free.
- **[Decided]** Evaluation is deterministic, and the cache is keyed by a content hash (`blake3`) of each node's inputs and parameters. Never hash the whole project, and never build keys from `Debug` output (both were old-prototype bugs).
- **[Decided]** A memory-pressure ladder: lower preview quality first, never export quality.
- **[Decided]** There is **one** engine. There is no full duplicate CPU reference implementation of every effect; correctness comes from golden-image tests.

## 8. Audio

- **[Decided]** Audio is in scope: moderate work now, with room for more later. There is an Audio workspace.
- **[Decided]** A **real-time audio engine** separate from video scheduling: processing in small blocks, no memory allocation or locks on the audio thread, and the audio device as master clock during playback.
- **[Decided]** The graph compiler produces **two schedules** from one graph: video (pulled per frame, cacheable) and audio (a continuous real-time stream).
- **[Decided]** **Channel layouts are part of the audio port type** (mono, stereo, 5.1, and room for Atmos-style beds), so surround never needs a rewrite.
- **[Decided]** **Buses, sends and inserts are just graph structure.** The mixer is another container editor, not a separate system.
- **[Decided]** Automatic plugin latency compensation in the compiler.
- **[Decided]** **One automation system.** Audio automation and video keyframes are the same animated-parameter mechanism.
- **[Decided]** CLAP plugin hosting, run out of process or sandboxed. LV2 and VST3 later.

## 9. Document, persistence and undo

- **[Decided]** Stable IDs (ULID) for every node and container.
- **[Decided]** The document is built on **persistent data structures** (`imbl`). Snapshots are cheap, and render threads read frozen snapshots while the UI edits.
- **[Decided]** **Every edit is a command.** Undo and redo work through commands or structural diffs, never by copying the whole document (the old prototype did that, twice).
- **[Decided]** A versioned, diffable, readable file format with schema versions per node type and legacy readers.
  - Unknown or future node types round-trip byte-exactly, and edits that would destroy them are refused.
  - Node payloads are structured data, never JSON stored as bytes.
- **[Decided]** Atomic saves (write a temp file, fsync, rename, fsync the directory), rolling backups, and tests that interrupt a save at each step.
- **[Decided]** **Project documents are pretty-printed JSON** with stable key order and tagged enums. Unknown nodes are held byte-exactly via `serde_json::RawValue`. Large binary data (paint strokes, caches, thumbnails) lives in sidecar files referenced by content hash, never embedded; optional zstd compression may come later. **Hand-written files** (plugin manifests, app config) are TOML. The format sits behind one module.
- **[Decided]** (from step 1) **File layout details:**
  - Containers nest in the file the same way they nest in the document, so an unknown container carries its whole contents inside its preserved bytes.
  - Parameters are saved only once set, as natural JSON values (`"exposure": 0.5`). Animation is saved as `{"keyframes": [...]}`.
  - Times are exact strings (`"3/2"`). Connections are `"<node id>:<port>"` strings.
  - Floats round-trip exactly (serde_json `float_roundtrip`).
  - Node IDs are allocated by whoever builds a command, never inside the document, so replaying a journal is deterministic.

## 10. Engine/UI separation and UI toolkit

- **[Decided]** The engine is a library that runs headless, driven equally by the UI, scripts, the CLI and tests. The UI holds no document logic.
- **[Decided]** Deterministic UI launch modes (open the app in a fixed state for screenshots), useful for design iteration and visual tests.
- **[Open]** **UI toolkit: iced or egui.** Decide with a bake-off: build the Edit page shell in both, on an engine-owned wgpu device, with docking and a 10-bit texture in the viewport, then pick by using them. GPUI is ruled out (see §7). Whichever we choose, we build our own design-system layer on top.

### 10.1 Process model

- **[Decided]** **The interactive app is one process.** UI and engine share the GPU device, so the viewer displays engine textures with no copying.
- **[Decided]** **The engine is a library behind a command/event API.** The UI uses only that API, as serialisable messages, even in-process.
- **[Decided]** **The same API is served over a local socket** for Python, the CLI and other tools to attach.
- **[Decided]** **Render queue jobs run in headless worker processes** (`fw render`), so a crashing render never takes the UI down and renders can outlive the UI.
- **[Decided]** **A command journal on disk** provides crash recovery: replay it after a crash and nothing is lost.
- **[Decided]** **Rendering on another machine is not a requirement.** The door stays open, because the API is the boundary: a headless `fw serve` mode could be added later for remote or farm rendering.

## 11. Extensibility

- **[Decided]** **Built-in nodes register through the same API as plugins.** If a built-in can't be written as a plugin, the API is incomplete.
- **[Decided]** Tiers:
  1. **Shader nodes:** WGSL plus a manifest; hot-reloadable.
  2. **WASM nodes** (wasmtime with WIT interfaces): sandboxed CPU logic in any language.
  3. **Scripting** (see below).
  4. **Native Rust:** in-tree only, because Rust has no stable binary interface. Possibly OpenFX hosting in the distant future.
  - Audio: CLAP hosting (§8).
- **[Decided]** "Recipe" plugins that compile parameters into ordinary graphs, so plugin code never runs per frame. This idea comes from the old prototype.
- **[Decided]** Node dispatch goes through a registry. Adding a node type touches one place (the old prototype needed 8 to 9 files).

### Scripting

- **[Decided]** Two layers:
  - **Automation: Python, running out of process,** talking to the app over a local socket using the same command API as the UI. This gives the full Python ecosystem (numpy, OpenCV, ML) without embedding pain or a bad script crashing the editor. Other client languages, such as TypeScript, cost little to add later.
  - **Expressions: a small built-in expression language** for parameters (`wiggle(2, 0.1)`, `time * 30`). It compiles into the evaluation graph, runs deterministically and in parallel, and could run on the GPU. Anything heavier becomes a WASM or shader node.
  - Embedding a general-purpose language in-process (Luau if ever) is deferred until scripted UI panels are needed.

## 12. Media I/O

- **[Decided]** FFmpeg, as a private, pinned, checksummed LGPL build. The app never uses system FFmpeg. Patent-encumbered components are loaded at runtime. FFmpeg is hidden behind our own media interface and nothing else touches it.
- **[Decided]** Images: `image`, `exr` (pure Rust), and `rawler` for RAW, with LibRaw bindings if coverage falls short.
- **[Decided]** Audio decoding through FFmpeg or `symphonia`, `cpal` for output, `rubato` for resampling. Stream audio; never decode whole files into RAM.
- **[Decided]** Hardware decode: start with CPU decode plus upload. Zero-copy hardware decode (VA-API or NVDEC into wgpu) later; the old prototype showed it needs wgpu patches.
- **[Decided]** Bounded export queues with byte accounting, and outputs published atomically.
- **[Decided]** (from step 1) **Outputs are byte-reproducible:** the same pixels always give the same file bytes. EXR is written with increasing line order, because the encoder otherwise writes chunks in thread-completion order.

## 13. 3D

- **[Decided]** 3D is a Scene3D container that renders into the image graph: an outliner, cameras, lights, glTF import, extruded text, shadow catchers and camera solves. It is not a modelling tool.
- **[Decided]** Our own wgpu renderer (raster first, path tracing later). Bevy is not used, because its ECS fights our document model.

## 14. Crate layout and build order

**[Decided]** Dependencies only point downward:

```
fw-core      ids, rational time, math, colour-space descriptors, hashing (no GPU, no I/O)
fw-plugin    the node contract: node type descriptions, parameter schemas, registry
fw-doc       document model, containers, commands, undo, serialisation, migrations
fw-compile   authoring tree → flat evaluation graph(s): video and audio schedules
fw-render    GPU executor, resource pool, content-hashed cache, scheduling
fw-audio     real-time audio engine
fw-media     decode/encode, image I/O (FFmpeg lives only here)
fw-ops       built-in nodes, registered via fw-plugin
fw-script    command protocol server, expression language
fw-cli       headless render/inspect
fw-app       UI
```

**[Decided]** (from step 1) `fw-plugin` sits directly above `fw-core`, because the document needs parameter schemas to check edits and decide which nodes are unknown. Plugin hosts (shader nodes, WASM, CLAP) get their own crates later, so the document never pulls in wasmtime.

**[Decided]** Build a thin slice through every layer first:

1. Core types, document, serialisation, and a three-node graph (image → exposure → output) rendered headlessly to EXR, colour-managed, with golden-image tests.
2. Time: a video source and the Timeline container, compiled to the flat graph, plus audio playback with A/V sync.
3. The UI shell in the Studio style (after the toolkit bake-off): viewer, parameter editing, undo, frame cache.
4. Shader-node plugins, to prove early that built-ins use the plugin API.
5. Then widen: LayerStack and Photo documents, masks, text, mixer, 3D.

**[Decided]** Process guardrails: a performance budget table (named workload, statistic, limit), synthetic test media generated with FFmpeg, no benchmark or qualification code in the shipping binary, no files over about 1.5k lines, plain names rather than invented subsystem names, and `TODO` rather than euphemisms like "forthcoming".

## 15. Lessons from the old prototype

The old code is at `~/projects/video-editor`, with a GPUI fork at `~/projects/framewright-zed`. It's an idea source only.

**Keep:** rational time and the three clocks, anchored keyframes, the compiled flat plan with path-hashed IDs and diffing for invalidation, round-tripping unknown nodes, separate colour boundaries, project-scoped OCIO, the private FFmpeg build, VFR as a source fact, the audio clock as master, atomic writes, the memory-pressure ladder, recipe plugins, performance budgets, deterministic screenshot launch modes.

**Avoid:** the UI host owning the GPU device; a duplicate CPU engine; JSON payloads stored as bytes with wiring hidden inside them; closed type-switch dispatch; whole-project rebuilds, undo snapshots and cache keys; silently clamping time arithmetic; heavy process far ahead of features; giant files; debt hidden behind euphemisms.

## 16. Open questions

- UI toolkit: iced vs egui (bake-off).
- How paint and retouch strokes are stored and evaluated inside video clips (every frame vs follow motion).
- Plugin interface details: manifest schema, versioning, sandboxing limits.
