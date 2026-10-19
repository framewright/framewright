# Performance budgets

Each budget is a named workload, a statistic and a limit. The source of truth
is `crates/fw-bench/src/main.rs`; `cargo run --release -p fw-bench -- --markdown`
prints this table, and `--check` fails when a budget is missed. The harness
and its test media never ship.

| Workload | What it measures | Statistic | Limit |
| --- | --- | --- | --- |
| `probe-cached` | Look up a 1080p clip's probe once it's known (every compile does this) | p50 | 0.050 ms |
| `compile-frame` | Compile one frame of a three-track 1080p timeline | p95 | 1.000 ms |
| `decode-next-1080p` | Decode the next frame of 1080p H.264 to RGBA | p95 | 20.000 ms |
| `playback-frame-1080p` | Compile, decode, render and read back the next 1080p frame | p95 | 41.700 ms |
| `scrub-1080p` | Show a random 1080p frame (seek, decode, render) | p95 | 150.000 ms |
| `audio-block-8-voices` | Mix one 512-frame block of eight streaming voices on the audio thread | p99 | 0.500 ms |

## Latest run

2026-10-19, NVIDIA GeForce RTX 4070 (Vulkan, driver 595.91.07), release build.

| Workload | Measured | Limit | Result |
| --- | --- | --- | --- |
| `probe-cached` | 0.001 ms | 0.050 ms | ok |
| `compile-frame` | 0.004 ms | 1.000 ms | ok |
| `decode-next-1080p` | 12.6 ms | 20.0 ms | ok |
| `playback-frame-1080p` | 51.1 ms | 41.7 ms | **missed** |
| `scrub-1080p` | 159.6 ms | 150.0 ms | **missed** |
| `audio-block-8-voices` | 0.009 ms | 0.500 ms | ok |

### Where a 1080p playback frame goes

About 15 ms decoding (FFmpeg plus our YUV → RGB on the CPU), 9 ms converting
to half floats and uploading, and 15 ms of GPU work and reading the result
back to the CPU.

- The read-back exists only because headless playback hands back CPU images.
  The viewer will show the GPU texture directly (design §10.1).
- YUV → RGB and the half-float packing belong on the GPU: upload the decoded
  planes and convert in a fused kernel. That also prepares for zero-copy
  hardware decode.
- Scrubbing adds a seek and decoding from the previous keyframe (up to two
  seconds of frames in the test clip's 48-frame groups).
