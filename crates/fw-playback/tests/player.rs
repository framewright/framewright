//! Playback, with a clock the test controls.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fw_audio::FakeClock;
use fw_core::{RgbaImage, Time};
use fw_playback::{FrameSource, Player};

/// Frames whose single pixel holds their own index; optionally slow.
struct Counting {
    delay: Duration,
    rendered: Arc<AtomicU64>,
}

impl FrameSource for Counting {
    fn frame(&mut self, index: i64) -> Result<Arc<RgbaImage>, String> {
        std::thread::sleep(self.delay);
        self.rendered.fetch_add(1, Ordering::Relaxed);
        let mut img = RgbaImage::new(1, 1, true);
        img.set_pixel(0, 0, [index as f32, 0.0, 0.0, 1.0]);
        Ok(Arc::new(img))
    }
}

fn t(s: &str) -> Time {
    s.parse().unwrap()
}

fn player(delay_ms: u64) -> (Player, Arc<FakeClock>, Arc<AtomicU64>) {
    let clock = Arc::new(FakeClock::new(Time::ZERO));
    let rendered = Arc::new(AtomicU64::new(0));
    let source = Counting {
        delay: Duration::from_millis(delay_ms),
        rendered: rendered.clone(),
    };
    let player = Player::new(Box::new(source), clock.clone(), t("24"), 4);
    (player, clock, rendered)
}

#[test]
fn shows_the_frame_the_clock_says_is_due() {
    let (mut player, clock, _) = player(0);
    for i in 0..48i64 {
        // Anywhere inside frame i's 1/24 s must show frame i.
        let inside = Time::new(i * 1000 + 999, 24_000).unwrap();
        clock.set(inside);
        // Rendering ahead means frame i is usually ready before its moment.
        assert!(player.wait_for(i, Duration::from_secs(2)));
        let shown = player.tick().expect("a new frame");
        assert_eq!(shown.index, i);
        assert_eq!(shown.image.pixel(0, 0)[0], i as f32);
        assert!(shown.time <= inside && inside < Time::of_frame(i + 1, t("24")).unwrap());
        assert!(
            player.tick().is_none(),
            "nothing new until the clock moves on"
        );
    }
    assert_eq!(player.stats().shown, 48);
    assert_eq!(player.stats().dropped, 0);
}

#[test]
fn a_slow_renderer_drops_frames_instead_of_drifting() {
    // Frames take 3× their own length to render.
    let (mut player, clock, _) = player(125);
    let mut last = -1;
    for i in 0..24i64 {
        clock.set(Time::of_frame(i, t("24")).unwrap());
        std::thread::sleep(Duration::from_millis(42));
        if let Some(shown) = player.tick() {
            assert!(shown.index > last, "never goes backwards");
            assert_eq!(shown.index, i, "only ever shows the frame that's due");
            last = shown.index;
        }
    }
    let stats = player.stats();
    assert!(stats.dropped > 0, "{stats:?}");
    assert!(stats.shown > 0, "{stats:?}");
}

#[test]
fn seeking_back_shows_the_earlier_frame() {
    let (mut player, clock, _) = player(0);
    clock.set(t("2"));
    player.tick();
    assert!(player.wait_for(48, Duration::from_secs(2)));
    assert_eq!(player.tick().unwrap().index, 48);
    clock.set(t("1/2"));
    player.tick();
    assert!(player.wait_for(12, Duration::from_secs(2)));
    assert_eq!(player.tick().unwrap().index, 12);
}

#[test]
fn renders_ahead_of_the_playhead() {
    let (mut player, clock, rendered) = player(0);
    clock.set(Time::ZERO);
    player.tick();
    assert!(
        player.wait_for(4, Duration::from_secs(2)),
        "frames 0–4 render without being shown"
    );
    assert!(rendered.load(Ordering::Relaxed) >= 5);
}
