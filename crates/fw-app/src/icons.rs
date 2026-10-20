//! Icons: the mockups' 24×24 stroked SVG paths, drawn as egui lines. egui
//! has no vector paths of its own, and rasterising SVGs would need a texture
//! per size and colour.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

pub const PLAY: &str = "M7 4l13 8-13 8z";
pub const SKIP_BACK: &str = "M19 20L9 12l10-8zM5 4v16";
pub const SKIP_FWD: &str = "M5 4l10 8-10 8zM19 4v16";
pub const STEP_BACK: &str = "M15 18l-6-6 6-6";
pub const STEP_FWD: &str = "M9 18l6-6-6-6";
pub const FOLDER: &str = "M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z";
pub const FILM: &str = "M3 4h18v16H3zM7 4v16M17 4v16M3 9h4M3 15h4M17 9h4M17 15h4";
pub const GRAPH: &str = "M4 5h6v5H4zM14 14h6v5h-6zM10 7.5h2a2 2 0 012 2V14";
pub const GRADE: &str = "M12 21a9 9 0 110-18 9 9 0 010 18zM12 7.5v.01M7.5 14v.01M16.5 14v.01";
pub const CUBE: &str = "M12 2l9 5v10l-9 5-9-5V7zM12 22V12M21 7l-9 5-9-5";
pub const WAVE: &str = "M3 12h2M7 8v8M11 5v14M15 9v6M19 11v2";
pub const DELIVER: &str = "M12 3v12M7 8l5-5 5 5M4 15v4a2 2 0 002 2h12a2 2 0 002-2v-4";
pub const UNDO: &str = "M9 14L4 9l5-5M4 9h11a5 5 0 010 10h-3";
pub const REDO: &str = "M15 14l5-5-5-5M20 9H9a5 5 0 000 10h3";
pub const CHECK: &str = "M5 12l4 4L19 6";
pub const MINIMISE: &str = "M6 12h12";
pub const MAXIMISE: &str = "M6 6h12v12H6z";
pub const RESTORE: &str = "M6 9h9v9H6zM9 9V6h9v9h-3";
pub const CLOSE: &str = "M6 6l12 12M18 6L6 18";
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn skip_sep(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] == b' ' || self.b[self.i] == b',') {
            self.i += 1;
        }
    }
    fn peek_cmd(&mut self) -> Option<char> {
        self.skip_sep();
        self.b
            .get(self.i)
            .map(|&c| c as char)
            .filter(|c| c.is_ascii_alphabetic())
    }
    fn at_number(&mut self) -> bool {
        self.skip_sep();
        matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'.')
    }
    fn num(&mut self) -> f32 {
        self.skip_sep();
        let s = self.i;
        if self.i < self.b.len() && self.b[self.i] == b'-' {
            self.i += 1;
        }
        let mut dot = false;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_digit() || (c == b'.' && !dot) {
                dot |= c == b'.';
                self.i += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.b[s..self.i])
            .unwrap()
            .parse()
            .unwrap_or(0.0)
    }
    /// Arc flags are single characters and may be packed ("012-2").
    fn flag(&mut self) -> bool {
        self.skip_sep();
        let f = self.b.get(self.i) == Some(&b'1');
        self.i += 1;
        f
    }
}

fn parse(d: &str) -> Vec<(Vec<Pos2>, bool)> {
    let mut c = Cursor {
        b: d.as_bytes(),
        i: 0,
    };
    let mut paths: Vec<(Vec<Pos2>, bool)> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut p = pos2(0.0, 0.0);
    let mut start = p;
    let mut cmd = 'M';
    fn flush(cur: &mut Vec<Pos2>, paths: &mut Vec<(Vec<Pos2>, bool)>, closed: bool) {
        if cur.len() > 1 {
            paths.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    }
    loop {
        if let Some(k) = c.peek_cmd() {
            cmd = k;
            c.i += 1;
            if k == 'Z' || k == 'z' {
                flush(&mut cur, &mut paths, true);
                p = start;
                continue;
            }
        } else if !c.at_number() {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let o = if rel { p.to_vec2() } else { egui::Vec2::ZERO };
        match cmd.to_ascii_uppercase() {
            'M' => {
                flush(&mut cur, &mut paths, false);
                let (x, y) = (c.num(), c.num());
                p = pos2(x, y) + o;
                start = p;
                cur.push(p);
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                let (x, y) = (c.num(), c.num());
                p = pos2(x, y) + o;
                cur.push(p);
            }
            'H' => {
                let x = c.num();
                p.x = x + if rel { p.x } else { 0.0 };
                cur.push(p);
            }
            'V' => {
                let y = c.num();
                p.y = y + if rel { p.y } else { 0.0 };
                cur.push(p);
            }
            'C' => {
                let n: Vec<f32> = (0..6).map(|_| c.num()).collect();
                let c1 = pos2(n[0], n[1]) + o;
                let c2 = pos2(n[2], n[3]) + o;
                let e = pos2(n[4], n[5]) + o;
                let s = p;
                for k in 1..=8 {
                    let t = k as f32 / 8.0;
                    let mt = 1.0 - t;
                    let v = s.to_vec2() * mt * mt * mt
                        + c1.to_vec2() * 3.0 * mt * mt * t
                        + c2.to_vec2() * 3.0 * mt * t * t
                        + e.to_vec2() * t * t * t;
                    cur.push(v.to_pos2());
                }
                p = e;
            }
            'A' => {
                let (rx, ry, _rot) = (c.num(), c.num(), c.num());
                let (large, sweep) = (c.flag(), c.flag());
                let (x, y) = (c.num(), c.num());
                let e = pos2(x, y) + o;
                if cur.is_empty() {
                    cur.push(p);
                }
                arc(&mut cur, p, e, rx, ry, large, sweep);
                p = e;
            }
            _ => break,
        }
    }
    flush(&mut cur, &mut paths, false);
    paths
}

/// Endpoint-parameterised elliptical arc (rotation assumed 0, as in the icons).
fn arc(out: &mut Vec<Pos2>, p0: Pos2, p1: Pos2, rx: f32, ry: f32, large: bool, sweep: bool) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        out.push(p1);
        return;
    }
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let lam = (dx * dx) / (rx * rx) + (dy * dy) / (ry * ry);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * dy * dy - ry * ry * dx * dx;
    let den = rx * rx * dy * dy + ry * ry * dx * dx;
    let mut co = (num / den).max(0.0).sqrt();
    if large == sweep {
        co = -co;
    }
    let cxp = co * rx * dy / ry;
    let cyp = -co * ry * dx / rx;
    let cx = cxp + (p0.x + p1.x) / 2.0;
    let cy = cyp + (p0.y + p1.y) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (dx - cxp) / rx, (dy - cyp) / ry);
    let mut dt = ang(
        (dx - cxp) / rx,
        (dy - cyp) / ry,
        (-dx - cxp) / rx,
        (-dy - cyp) / ry,
    );
    if !sweep && dt > 0.0 {
        dt -= std::f32::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f32::consts::TAU;
    }
    let n = ((dt.abs() / 0.25).ceil() as usize).max(2);
    for k in 1..=n {
        let t = t1 + dt * k as f32 / n as f32;
        out.push(pos2(cx + rx * t.cos(), cy + ry * t.sin()));
    }
}

/// Paint a mockup icon (24x24 viewBox) into `rect` with a stroke of
/// `width` in *icon* units (the mockup uses 1.6). `fill` fills closed paths.
pub fn paint(painter: &Painter, rect: Rect, d: &str, color: Color32, width: f32, fill: bool) {
    let s = rect.width().min(rect.height()) / 24.0;
    let map = |q: Pos2| pos2(rect.min.x + q.x * s, rect.min.y + q.y * s);
    for (pts, closed) in parse(d) {
        let pts: Vec<Pos2> = pts.into_iter().map(map).collect();
        if fill && closed {
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        } else if closed {
            painter.add(Shape::closed_line(pts, Stroke::new(width * s, color)));
        } else {
            painter.add(Shape::line(pts, Stroke::new(width * s, color)));
        }
    }
}
