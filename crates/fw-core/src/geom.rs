//! Geometry in canvas units.
//!
//! Positions and regions are never in pixels. A render request maps canvas
//! units to pixels with a scale.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const fn new(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }
}

/// An axis-aligned region `[min, max)`. Infinite extents are allowed, so
/// generators can have an unbounded region of definition.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub const EMPTY: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);
    pub const INFINITE: Rect = Rect::new(
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::INFINITY,
    );

    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect {
            min: Vec2::new(x0, y0),
            max: Vec2::new(x1, y1),
        }
    }

    pub fn from_size(width: f64, height: f64) -> Rect {
        Rect::new(0.0, 0.0, width, height)
    }

    pub fn width(&self) -> f64 {
        (self.max.x - self.min.x).max(0.0)
    }

    pub fn height(&self) -> f64 {
        (self.max.y - self.min.y).max(0.0)
    }

    pub fn is_empty(&self) -> bool {
        !(self.max.x > self.min.x && self.max.y > self.min.y)
    }

    pub fn is_finite(&self) -> bool {
        [self.min.x, self.min.y, self.max.x, self.max.y]
            .iter()
            .all(|v| v.is_finite())
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let r = Rect::new(
            self.min.x.max(other.min.x),
            self.min.y.max(other.min.y),
            self.max.x.min(other.max.x),
            self.max.y.min(other.max.y),
        );
        if r.is_empty() { Rect::EMPTY } else { r }
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Rect::new(
            self.min.x.min(other.min.x),
            self.min.y.min(other.min.y),
            self.max.x.max(other.max.x),
            self.max.y.max(other.max.y),
        )
    }

    /// Grows the rect by `margin` on every side, e.g. for a blur's input region.
    pub fn expand(&self, margin: f64) -> Rect {
        Rect::new(
            self.min.x - margin,
            self.min.y - margin,
            self.max.x + margin,
            self.max.y + margin,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_and_union() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 20.0, 20.0);
        assert_eq!(a.intersect(&b), Rect::new(5.0, 5.0, 10.0, 10.0));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 20.0, 20.0));
        assert!(a.intersect(&Rect::new(11.0, 0.0, 12.0, 1.0)).is_empty());
        assert_eq!(Rect::INFINITE.intersect(&a), a);
        assert!(!Rect::INFINITE.is_finite());
        assert_eq!(a.expand(2.0).width(), 14.0);
    }
}
