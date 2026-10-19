//! Decoding audio: the test clip carries a 440 Hz tone at 48 kHz.

use fw_media::{AudioReader, CHANNELS, has_audio};

fn read(reader: &mut AudioReader, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * CHANNELS];
    let got = reader.read(&mut out).unwrap();
    out.truncate(got * CHANNELS);
    out
}

fn zero_crossings(samples: &[f32]) -> usize {
    let left: Vec<f32> = samples.iter().step_by(CHANNELS).copied().collect();
    left.windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count()
}

#[test]
fn decodes_the_tone_at_the_session_rate() {
    let path = fw_testmedia::h264();
    assert!(has_audio(&path).unwrap());
    assert!(!has_audio(&fw_testmedia::count_24fps()).unwrap());
    let mut reader = AudioReader::open(&path, 48000, None).unwrap();
    let second = read(&mut reader, 48000);
    assert_eq!(second.len(), 48000 * CHANNELS);
    // 440 Hz crosses zero 880 times a second.
    // Skip the first 0.1 s (4800 frames) and count over the remaining 0.9 s.
    let crossings = zero_crossings(&second[4800 * CHANNELS..]);
    assert!(
        (790..=794).contains(&crossings),
        "{crossings} crossings in 0.9 s"
    );
    let peak = second.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!((0.1..0.15).contains(&peak), "peak {peak}");
    // Mono is spread to both channels.
    assert_eq!(second[1000], second[1001]);
}

#[test]
fn resamples_to_other_rates() {
    let mut reader = AudioReader::open(&fw_testmedia::h264(), 44100, None).unwrap();
    let second = read(&mut reader, 44100);
    let crossings = zero_crossings(&second[4410 * CHANNELS..]);
    assert!(
        (790..=794).contains(&crossings),
        "{crossings} crossings in 0.9 s"
    );
}

#[test]
fn seeking_matches_reading_straight_through() {
    let path = fw_testmedia::h264();
    let mut straight = AudioReader::open(&path, 48000, None).unwrap();
    let all = read(&mut straight, 120_000);
    let mut jumping = AudioReader::open(&path, 48000, None).unwrap();
    for start in [96_000usize, 30_000, 70_001] {
        jumping.seek(start as i64).unwrap();
        let got = read(&mut jumping, 4800);
        let want = &all[start * CHANNELS..(start + 4800) * CHANNELS];
        // AAC decoder state after a seek differs by ~1e-5 (far below hearing).
        // Being one sample out would differ by ~6e-3 on this tone.
        let worst = got
            .iter()
            .zip(want)
            .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-4, "seek to {start}: off by {worst}");
    }
}
