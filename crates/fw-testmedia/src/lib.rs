//! Synthetic test media, generated once with the `ffmpeg` CLI into
//! `target/test-media/<version>/` and reused by every test.
//!
//! Only tests depend on this crate. Change [`VERSION`] when a recipe changes
//! so stale files are never reused.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

pub const VERSION: &str = "v2";

/// Frame `n` of the counting clips has every channel equal to this, out of 255.
pub fn level(n: u64) -> u8 {
    ((n * 8) % 256) as u8
}

/// A recipe: the file name and the ffmpeg arguments that make it.
struct Recipe {
    name: &'static str,
    args: &'static [&'static str],
}

/// Frame N's pixels are `level(N)` in every channel, losslessly encoded, so a
/// test can tell exactly which frame it decoded.
const COUNT_CFR: Recipe = Recipe {
    name: "count-24fps.mkv",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x32:r=24:d=2,format=gbrp",
        "-vf",
        "geq=r='mod(N*8,256)':g='mod(N*8,256)':b='mod(N*8,256)'",
        "-c:v",
        "ffv1",
        "-g",
        "1",
    ],
};

/// The same counting frames with variable spacing: alternately 1/30 s and
/// 2/30 s apart (stored at the container's 1 ms resolution).
const COUNT_VFR: Recipe = Recipe {
    name: "count-vfr.mkv",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x32:r=30:d=2,format=gbrp",
        "-vf",
        "geq=r='mod(N*8,256)':g='mod(N*8,256)':b='mod(N*8,256)',setpts='(N+floor(N/2))/30/TB'",
        "-fps_mode",
        "passthrough",
        "-c:v",
        "ffv1",
        "-g",
        "1",
    ],
};

/// Ordinary 23.976 fps H.264 4:2:0 with long GOPs and B-frames, tagged
/// Rec.709, plus a 440 Hz tone. For seeking, colour handling and audio.
const H264: Recipe = Recipe {
    name: "testsrc-h264.mp4",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=320x180:r=24000/1001:d=4",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:d=4",
        "-vf",
        "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "48",
        "-bf",
        "2",
        "-c:a",
        "aac",
        "-shortest",
    ],
};

/// Frames counting as in `count-24fps.mkv`, with alpha 1/2.
const ALPHA: Recipe = Recipe {
    name: "count-alpha.mov",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x32:r=24:d=1,format=rgba",
        "-vf",
        "geq=r='mod(N*8,256)':g='mod(N*8,256)':b='mod(N*8,256)':a=128",
        "-c:v",
        "qtrle",
    ],
};

/// A second of 4:4:4 frames tagged Rec.709 limited range: the left half is video
/// black (Y 16), the right half video white (Y 235).
const LEVELS_YUV: Recipe = Recipe {
    name: "levels-yuv.mkv",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x32:r=24:d=1,format=yuv444p",
        "-vf",
        "geq=lum='if(lt(X,32),16,235)':cb=128:cr=128,setparams=colorspace=bt709:range=tv",
        "-c:v",
        "ffv1",
        "-frames:v",
        "1",
    ],
};

pub fn levels_yuv() -> PathBuf {
    ensure(&LEVELS_YUV)
}

/// Ten seconds of 1080p 23.976 fps H.264 4:2:0 (long GOP, B-frames) with a
/// stereo tone: an ordinary camera-like file, for benchmarks.
const HD_H264: Recipe = Recipe {
    name: "hd-h264.mp4",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=24000/1001:d=10",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:d=10",
        "-vf",
        "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "48",
        "-bf",
        "2",
        "-c:a",
        "aac",
        "-ac",
        "2",
        "-shortest",
    ],
};

pub fn hd_h264() -> PathBuf {
    ensure(&HD_H264)
}

/// Counting frames at 24 fps with sound that starts 0.25 s after the
/// picture, in a container whose clock starts at 2 s. The sound is silent
/// except for a click 0.75 s into it: 1 s after the first frame, so it
/// belongs with frame 24.
const SYNC_CLICK: Recipe = Recipe {
    name: "sync-click.mkv",
    args: &[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x32:r=24:d=2,format=gbrp",
        "-itsoffset",
        "0.25",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc='if(between(t,0.75,0.7505),0.9,0)':s=48000:d=1.5",
        "-vf",
        "geq=r='mod(N*8,256)':g='mod(N*8,256)':b='mod(N*8,256)'",
        "-c:v",
        "ffv1",
        "-g",
        "1",
        "-c:a",
        "pcm_s16le",
        "-output_ts_offset",
        "2",
    ],
};

pub fn sync_click() -> PathBuf {
    ensure(&SYNC_CLICK)
}

pub fn count_24fps() -> PathBuf {
    ensure(&COUNT_CFR)
}

pub fn count_vfr() -> PathBuf {
    ensure(&COUNT_VFR)
}

pub fn h264() -> PathBuf {
    ensure(&H264)
}

pub fn count_alpha() -> PathBuf {
    ensure(&ALPHA)
}

fn dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/fw-testmedia; the workspace target is two up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-media")
        .join(VERSION)
}

/// Makes the file if it isn't there yet. Panics with a clear message if the
/// ffmpeg CLI is missing or fails.
fn ensure(recipe: &Recipe) -> PathBuf {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = dir();
    let path = dir.join(recipe.name);
    if path.exists() {
        return path;
    }
    std::fs::create_dir_all(&dir).expect("cannot create target/test-media");
    // Write under a temporary name so an interrupted run never leaves a bad file.
    let temp = dir.join(format!(".{}.{}.partial", std::process::id(), recipe.name));
    let ext = Path::new(recipe.name)
        .extension()
        .unwrap()
        .to_str()
        .unwrap();
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(recipe.args)
        .args(["-f", muxer(ext)])
        .arg(&temp)
        .status()
        .unwrap_or_else(|e| panic!("these tests need the ffmpeg CLI to make test media: {e}"));
    assert!(status.success(), "ffmpeg failed making {}", recipe.name);
    std::fs::rename(&temp, &path).unwrap();
    path
}

fn muxer(ext: &str) -> &'static str {
    match ext {
        "mkv" => "matroska",
        "mp4" => "mp4",
        "mov" => "mov",
        other => panic!("no muxer for .{other}"),
    }
}
