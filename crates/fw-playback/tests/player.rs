//! Playback, with a clock the test controls.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    type Frame = Arc<RgbaImage>;

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

fn player(delay_ms: u64) -> (Player<Arc<RgbaImage>>, Arc<FakeClock>, Arc<AtomicU64>) {
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

/// Frames whose pixel holds the document version they were rendered from.
struct Versioned {
    version: Arc<AtomicU64>,
    rendered: Arc<Mutex<Vec<i64>>>,
}

impl FrameSource for Versioned {
    type Frame = Arc<RgbaImage>;

    fn frame(&mut self, index: i64) -> Result<Arc<RgbaImage>, String> {
        self.rendered.lock().unwrap().push(index);
        let mut img = RgbaImage::new(1, 1, true);
        let version = self.version.load(Ordering::Relaxed) as f32;
        img.set_pixel(0, 0, [index as f32, version, 0.0, 1.0]);
        Ok(Arc::new(img))
    }
}

type Rendered = Arc<Mutex<Vec<i64>>>;

fn versioned() -> (
    Player<Arc<RgbaImage>>,
    Arc<FakeClock>,
    Arc<AtomicU64>,
    Rendered,
) {
    let clock = Arc::new(FakeClock::new(Time::ZERO));
    let version = Arc::new(AtomicU64::new(0));
    let rendered = Rendered::default();
    let source = Versioned {
        version: version.clone(),
        rendered: rendered.clone(),
    };
    let player = Player::new(Box::new(source), clock.clone(), t("24"), 4);
    (player, clock, version, rendered)
}

/// Ticks until a frame is shown, or gives up after two seconds.
fn next_shown(
    player: &mut Player<Arc<RgbaImage>>,
) -> Option<fw_playback::Presented<Arc<RgbaImage>>> {
    for _ in 0..400 {
        if let Some(shown) = player.tick() {
            return Some(shown);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn an_edit_replaces_the_frame_on_screen() {
    let (mut player, _clock, version, _) = versioned();
    player.set_playing(false);
    let first = next_shown(&mut player).expect("frame 0");
    assert_eq!(first.image.pixel(0, 0)[1], 0.0);
    assert!(player.up_to_date());

    version.store(1, Ordering::Relaxed);
    player.invalidate();
    assert!(
        !player.up_to_date(),
        "the frame on screen is from before the edit"
    );
    assert_eq!(
        player.shown().unwrap().index,
        0,
        "it stays until its replacement is ready"
    );
    let second = next_shown(&mut player).expect("frame 0 again");
    assert_eq!(second.index, 0);
    assert_eq!(second.image.pixel(0, 0)[1], 1.0);
    assert!(player.up_to_date());
}

#[test]
fn paused_renders_only_the_frame_due() {
    let (mut player, clock, _, rendered) = versioned();
    player.set_playing(false);
    for i in [10i64, 40, 20] {
        clock.set(Time::of_frame(i, t("24")).unwrap());
        let shown = next_shown(&mut player).expect("the frame landed on");
        assert_eq!(shown.index, i);
    }
    std::thread::sleep(Duration::from_millis(50));
    let mut rendered = rendered.lock().unwrap().clone();
    // Frame 0 may render before the clock first moves.
    rendered.retain(|i| *i != 0);
    assert_eq!(rendered, [10, 40, 20], "nothing rendered ahead");
    assert_eq!(player.stats().dropped, 0, "jumps while paused aren't drops");
}
