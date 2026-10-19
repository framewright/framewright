# Performance budgets

Each budget is a named workload, a statistic and a limit. The source of truth
is `crates/fw-bench/src/main.rs`; `cargo run --release -p fw-bench -- --markdown`
prints this table, and `--check` fails when a budget is missed. The harness
and its test media never ship.

| Workload | What it measures | Statistic | Limit |
| --- | --- | --- | --- |
| `probe-cached` | Look up a 1080p clip's probe once it's known (every compile does this) | p50 | 0.050 ms |
| `compile-frame` | Compile one frame of a three-track 1080p timeline | p95 | 1.000 ms |
| `decode-next-1080p` | Decode the next frame of 1080p H.264, ready to upload | p95 | 20.000 ms |
| `playback-frame-1080p` | Compile, decode and render the next 1080p frame, until the GPU has finished it | p95 | 41.700 ms |
| `playback-composite-1080p` | The same, blending three layers decoded from three places in one file | p95 | 41.700 ms |
| `scrub-1080p` | Render a random 1080p frame (seek, decode, render, GPU finished) | p95 | 150.000 ms |
| `audio-block-8-voices` | Mix one 512-frame block of eight streaming voices on the audio thread | p99 | 0.500 ms |

The benchmark clip is ten seconds of 1080p 23.976 fps H.264 4:2:0 with grain
at 50 Mbit/s and 2 s keyframe groups, so it decodes like camera footage. (The
clean test pattern used before decoded about five times faster.)

## Latest run

2026-10-19, NVIDIA GeForce RTX 4070 (Vulkan, driver 595.91.07), 12-thread CPU,
release build. Three runs; the range is shown.

| Workload | Measured | Limit | Result |
| --- | --- | --- | --- |
| `probe-cached` | 0.001 ms | 0.050 ms | ok |
| `compile-frame` | 0.004–0.005 ms | 1.000 ms | ok |
| `decode-next-1080p` | 5.9–6.8 ms | 20.0 ms | ok |
| `playback-frame-1080p` | 3.8–4.1 ms | 41.7 ms | ok |
| `playback-composite-1080p` | 8.1–9.7 ms | 41.7 ms | ok |
| `scrub-1080p` | 148.4–152.9 ms | 150.0 ms | **at the limit**: missed in one run of three (earlier runs 142–169 ms) |
| `audio-block-8-voices` | 0.006–0.012 ms | 0.500 ms | ok |

On llvmpipe (software Vulkan, sharing the CPU with decoding), single-layer
playback is 16.1 ms and the three-layer composite 50.3 ms, over budget.
Budgets are set for a real GPU.

### Before and after the optimisation pass

The same clip, before the pass: decoding 35.8 ms, playback 70.7 ms, scrubbing
867 ms. A 1080p playback frame used to go:

| Step | Before | After |
| --- | --- | --- |
| Decoding: FFmpeg on one thread, then swscale and YUV → RGB on the CPU | 28.5 ms | 1.7 ms (FFmpeg on all cores; conversion on the GPU) |
| Half-float packing and upload | 10.3 ms | 0.2 ms (raw planes, 3 MB) |
| GPU work, reading back and converting to `f32` | 14.7 ms | 0.6 ms (no read-back) |

### Scrubbing

Scrubbing is bound by decoding: a seek lands on the keyframe before the target
and decodes forward. The 50 Mbit/s keyframe takes about 50 ms (one thread; the
clip has no slices to share out), then each frame about 2.6 ms, so the worst
targets, 40-odd frames into a group, take 150–200 ms. The next levers are
hardware decoding (design §12, Open) and, in the viewer, showing the nearest
ready frame while the exact one decodes.
