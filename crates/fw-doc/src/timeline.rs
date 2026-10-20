//! The contents of a Timeline container: tracks, and clips placed on them.
//!
//! Each clip is a child node (usually `fw.clip`), and its placement
//! lives here in the container, never inside the node.

use std::fmt;

use fw_core::{NodeId, Time};
use imbl::{OrdMap, Vector};
use serde::{Deserialize, Serialize};

/// Identifies a track within its timeline.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(pub NodeId);

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TrackId({})", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
}

/// The timeline's canvas and clocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineSettings {
    /// Canvas size. Canvas units are pixels at full resolution.
    pub width: u32,
    pub height: u32,
    /// Frames per second, exactly, e.g. `"24000/1001"`.
    pub rate: Time,
    /// The session sample rate. Audio sources are resampled to it.
    pub sample_rate: u32,
}

/// Where a clip sits, and which part of its source it plays.
///
/// The three clocks: at parent time `T`, the clip's local (animation) time is
/// `T - start`, and its source time is `source_in + local × speed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub track: TrackId,
    /// Position in the parent timeline.
    pub start: Time,
    /// Length in the parent timeline. Always positive.
    pub duration: Time,
    /// The source time shown at the clip's first frame.
    pub source_in: Time,
    /// Source seconds per timeline second, as an exact ratio. Positive.
    /// TODO: reverse playback and retime curves.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: Time,
}

fn one() -> Time {
    Time::from_seconds(1)
}

fn is_one(t: &Time) -> bool {
    *t == one()
}

impl Placement {
    pub fn end(&self) -> Time {
        self.start
            .checked_add(self.duration)
            .expect("placements are checked for overflow when set")
    }

    pub fn contains(&self, t: Time) -> bool {
        self.start <= t && t < self.end()
    }

    pub fn overlaps(&self, other: &Placement) -> bool {
        self.track == other.track && self.start < other.end() && other.start < self.end()
    }
}

/// The clock values for one clip at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipTime {
    /// Time since the clip's start: what keyframes are resolved against.
    pub local: Time,
    pub length: Time,
    /// The moment of the source to show.
    pub source: Time,
}

impl Placement {
    /// The clip's clocks at parent time `t`, or `None` if the clip isn't active then.
    pub fn clip_time(&self, t: Time) -> Option<ClipTime> {
        if !self.contains(t) {
            return None;
        }
        let local = t.checked_sub(self.start).ok()?;
        let source = self
            .source_in
            .checked_add(local.checked_mul(self.speed).ok()?)
            .ok()?;
        Some(ClipTime {
            local,
            length: self.duration,
            source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub settings: TimelineSettings,
    /// Bottom to top: index 0 is V1 (or A1). For video, higher tracks cover lower ones.
    pub tracks: Vector<Track>,
    /// Every child node, with its placement.
    pub clips: OrdMap<NodeId, Placement>,
}

impl Timeline {
    pub fn new(settings: TimelineSettings) -> Timeline {
        Timeline {
            settings,
            tracks: Vector::new(),
            clips: OrdMap::new(),
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// The clip on `track` active at `t`, if any. Clips on one track never overlap.
    pub fn clip_at(&self, track: TrackId, t: Time) -> Option<(NodeId, &Placement)> {
        self.clips
            .iter()
            .find(|(_, p)| p.track == track && p.contains(t))
            .map(|(id, p)| (*id, p))
    }

    /// Why `placement` can't go here, ignoring the clip `except` (the one being moved).
    pub fn check_placement(
        &self,
        placement: &Placement,
        except: Option<NodeId>,
    ) -> Result<(), String> {
        let track = self
            .track(placement.track)
            .ok_or_else(|| format!("there is no track {}", placement.track))?;
        if placement.duration <= Time::ZERO {
            return Err("a clip must last longer than zero".into());
        }
        if placement.speed <= Time::ZERO {
            return Err("speed must be positive".into());
        }
        if placement.start.checked_add(placement.duration).is_err() {
            return Err("the clip ends too far in the future".into());
        }
        if let Some((other, _)) = self
            .clips
            .iter()
            .find(|(id, p)| Some(**id) != except && p.overlaps(placement))
        {
            return Err(format!("it would overlap clip {other} on {}", track.name));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fw_core::id::{IdGen, SequentialIds};

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    #[test]
    fn clocks() {
        let mut ids = SequentialIds::new(1);
        let p = Placement {
            track: TrackId(ids.next_id()),
            start: t("10"),
            duration: t("4"),
            source_in: t("100"),
            speed: t("1/2"),
        };
        assert_eq!(p.clip_time(t("9.999")), None);
        assert_eq!(p.clip_time(t("14")), None, "the end is exclusive");
        let c = p.clip_time(t("12")).unwrap();
        assert_eq!((c.local, c.length, c.source), (t("2"), t("4"), t("101")));
    }

    #[test]
    fn placements_are_checked() {
        let mut ids = SequentialIds::new(1);
        let v1 = TrackId(ids.next_id());
        let mut tl = Timeline::new(TimelineSettings {
            width: 64,
            height: 32,
            rate: t("24"),
            sample_rate: 48000,
        });
        tl.tracks.push_back(Track {
            id: v1,
            kind: TrackKind::Video,
            name: "V1".into(),
        });
        let at = |start: &str, duration: &str| Placement {
            track: v1,
            start: t(start),
            duration: t(duration),
            source_in: Time::ZERO,
            speed: one(),
        };
        let clip = ids.next_id();
        tl.clips.insert(clip, at("0", "2"));
        assert!(
            tl.check_placement(&at("2", "1"), None).is_ok(),
            "touching is fine"
        );
        assert!(tl.check_placement(&at("1", "2"), None).is_err());
        assert!(
            tl.check_placement(&at("1", "2"), Some(clip)).is_ok(),
            "moving itself"
        );
        assert!(tl.check_placement(&at("5", "0"), None).is_err());
        assert_eq!(tl.clip_at(v1, t("1.5")).map(|(id, _)| id), Some(clip));
        assert_eq!(tl.clip_at(v1, t("2")), None);
    }
}
