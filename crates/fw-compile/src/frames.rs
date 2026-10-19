//! Which frame of a video shows at a given source time.

use fw_core::Time;

/// The presentation times of a video's frames.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameTimes {
    /// Constant rate: frame `i` starts at `start + i / rate`.
    Constant { start: Time, rate: Time, count: u64 },
    /// Variable rate: every frame's exact start time, in order. Footage is
    /// never rewritten to a constant rate (design §5).
    Variable(Vec<Time>),
}

impl FrameTimes {
    pub fn count(&self) -> u64 {
        match self {
            FrameTimes::Constant { count, .. } => *count,
            FrameTimes::Variable(times) => times.len() as u64,
        }
    }

    /// The start time of the frame showing at `t`. Before the first frame this
    /// is the first frame; after the last it holds the last.
    /// TODO: let clips choose what happens past the media's ends.
    pub fn frame_at(&self, t: Time) -> Option<Time> {
        match self {
            FrameTimes::Constant { start, rate, count } => {
                if *count == 0 {
                    return None;
                }
                let since = t.checked_sub(*start).ok()?;
                let index = since.frame_index(*rate).ok()?.clamp(0, *count as i64 - 1);
                start.checked_add(Time::of_frame(index, *rate).ok()?).ok()
            }
            FrameTimes::Variable(times) => {
                let after = times.partition_point(|f| *f <= t);
                times.get(after.saturating_sub(1)).copied()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    #[test]
    fn constant_rate() {
        let ntsc = FrameTimes::Constant {
            start: Time::ZERO,
            rate: t("24000/1001"),
            count: 48,
        };
        assert_eq!(ntsc.frame_at(t("0")), Some(t("0")));
        assert_eq!(ntsc.frame_at(t("1001/24000")), Some(t("1001/24000")));
        assert_eq!(
            ntsc.frame_at(t("1001/24001")),
            Some(t("0")),
            "just before frame 1"
        );
        assert_eq!(ntsc.frame_at(t("-5")), Some(t("0")));
        assert_eq!(
            ntsc.frame_at(t("100")),
            Some(t("47047/24000")),
            "holds the last frame"
        );
    }

    #[test]
    fn variable_rate() {
        let vfr = FrameTimes::Variable(vec![t("0"), t("1/30"), t("1/10"), t("1/5")]);
        assert_eq!(vfr.frame_at(t("0.05")), Some(t("1/30")));
        assert_eq!(vfr.frame_at(t("1/10")), Some(t("1/10")));
        assert_eq!(vfr.frame_at(t("9")), Some(t("1/5")));
        assert_eq!(vfr.frame_at(t("-1")), Some(t("0")));
        assert_eq!(FrameTimes::Variable(vec![]).frame_at(t("0")), None);
    }
}
