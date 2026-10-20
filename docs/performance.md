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
| `scrub-1080p-copy` | Render a random 1080p frame at draft quality, from its editing copy (GPU finished) | p95 | 50.000 ms |
| `audio-block-8-voices` | Mix one 512-frame block of eight streaming voices on the audio thread | p99 | 0.500 ms |

The benchmark clip is ten seconds of 1080p 23.976 fps H.264 4:2:0 with grain
at 50 Mbit/s and 2 s keyframe groups, so it decodes like camera footage. (The
clean test pattern used before decoded about five times faster.)

## Latest run

2026-10-20, NVIDIA GeForce RTX 4070 (Vulkan, driver 595.91.07), Ryzen 5 7600X
(6 cores, 12 threads), release build, with the default decoder choice, which on this GPU is hardware decoding.
Three runs; the range is shown.

| Workload | Measured | Limit | Result |
| --- | --- | --- | --- |
| `probe-cached` | 0.001–0.002 ms | 0.050 ms | ok |
| `compile-frame` | 0.004–0.007 ms | 1.000 ms | ok |
| `decode-next-1080p` | 5.6–12.9 ms | 20.0 ms | ok (software; the reader alone) |
| `playback-frame-1080p` | 7.0–7.5 ms | 41.7 ms | ok |
| `playback-composite-1080p` | 10.3–12.2 ms | 41.7 ms | ok |
| `scrub-1080p` | 129.9–130.5 ms | 150.0 ms | ok |
| `scrub-1080p-copy` | 4.5–6.5 ms | 50.0 ms | ok |
| `audio-block-8-voices` | 0.007–0.012 ms | 0.500 ms | ok |

### Hardware against software decoding

The same build with `FW_HARDWARE_DECODE=0`, one run each, with the CPU time
the whole workload took (wall time includes setup):

| Workload | Hardware | CPU | Software | CPU |
| --- | --- | --- | --- | --- |
| `playback-frame-1080p` | 7.5 ms | 0.6 s | 2.7 ms | 2.7 s |
| `playback-composite-1080p` | 12.2 ms | 0.9 s | 7.6 ms | 7.0 s |
| `scrub-1080p` | 133.4 ms | 1.0 s | 139.6 ms | 23.0 s |

Earlier software runs of `scrub-1080p` ranged 144–163 ms. Hardware playback
waits for the decoder chip (about 2.6 ms a frame at this bitrate) where
software spreads the work over every core; both are far inside the budget,
and hardware leaves the CPU free.

On the Ryzen's integrated Radeon (RADV) with hardware decoding forced
(`FW_HARDWARE_DECODE=1`): playback 10.2 ms, three layers 18.8 ms, scrubbing
the original 291.9 ms (**missed**: its decoder is slower than the CPU),
scrubbing the editing copy 7.9 ms. By default the engine measures both
decoders and there picks software: playback 4.7 ms, three layers 15.6 ms,
scrubbing the original 170.8 ms (**missed**), against the 4070 machine's
software 140–163 ms. The 4070 picks the GPU, as in the table above.

On llvmpipe (software Vulkan, no decode queue, sharing the CPU with
decoding), single-layer playback is 16.1 ms and the three-layer composite
50.3 ms, over budget; scrubbing from the editing copy is 16.4 ms. Budgets are
set for a real GPU.

### Before and after the optimisation pass

The same clip, before the pass: decoding 35.8 ms, playback 70.7 ms, scrubbing
867 ms. A 1080p playback frame used to go:

| Step | Before | After |
| --- | --- | --- |
| Decoding: FFmpeg on one thread, then swscale and YUV → RGB on the CPU | 28.5 ms | 1.7 ms (FFmpeg on all cores; conversion on the GPU) |
| Half-float packing and upload | 10.3 ms | 0.2 ms (raw planes, 3 MB) |
| GPU work, reading back and converting to `f32` | 14.7 ms | 0.6 ms (no read-back) |

### Scrubbing

Scrubbing the original is bound by decoding: a seek lands on the keyframe
before the target and decodes forward. Its mean is about 96 ms, of which
decoding is 95; the p95 is about 150 ms because the worst targets sit 40-odd
frames into a keyframe group (the 50 Mbit/s keyframe takes about 50 ms on one
thread, then each frame about 2.6 ms).

Editing copies (design §12) are the fix: every frame of a ProRes Proxy copy
stands alone, so a random frame costs opening its chunk (about 4 ms, skipped
when one of the eight most recent is still open) and decoding one frame with
slice threads (under 1 ms). Making the copy of this 10 s clip takes about
1.4 s; it's built once, untimed, and found on disk by later runs.
