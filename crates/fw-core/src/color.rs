//! Colour-space descriptors.
//!
//! These are plain data plus the maths to convert between them. The GPU side of
//! conversion is generated from the same descriptors by the compiler.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::KeyHasher;

/// The red, green and blue primaries and white point of an RGB space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    /// sRGB and Rec.709, D65 white.
    Rec709,
    /// Rec.2020, D65 white.
    Rec2020,
    /// Display P3, D65 white.
    P3D65,
    /// ACES AP0, ACES white.
    AcesAp0,
    /// ACES AP1 (ACEScg, ACEScct), ACES white.
    AcesAp1,
}

/// How encoded values relate to linear light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Linear,
    /// The piecewise sRGB curve.
    Srgb,
    /// A pure 2.4 power, as BT.1886 displays use.
    Gamma24,
    /// ACEScct log encoding.
    AcesCct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorSpace {
    pub primaries: Primaries,
    pub transfer: Transfer,
}

impl ColorSpace {
    /// The engine's scene-linear working space.
    pub const ACESCG: ColorSpace = ColorSpace::new(Primaries::AcesAp1, Transfer::Linear);
    pub const ACESCCT: ColorSpace = ColorSpace::new(Primaries::AcesAp1, Transfer::AcesCct);
    pub const SRGB: ColorSpace = ColorSpace::new(Primaries::Rec709, Transfer::Srgb);
    pub const LINEAR_REC709: ColorSpace = ColorSpace::new(Primaries::Rec709, Transfer::Linear);
    pub const REC709_DISPLAY: ColorSpace = ColorSpace::new(Primaries::Rec709, Transfer::Gamma24);

    pub const fn new(primaries: Primaries, transfer: Transfer) -> ColorSpace {
        ColorSpace {
            primaries,
            transfer,
        }
    }

    pub fn hash_into(&self, h: &mut KeyHasher) {
        h.str(self.primaries.id()).str(self.transfer.id());
    }

    pub fn name(&self) -> String {
        match *self {
            ColorSpace::ACESCG => "ACEScg".into(),
            ColorSpace::ACESCCT => "ACEScct".into(),
            ColorSpace::SRGB => "sRGB".into(),
            ColorSpace::LINEAR_REC709 => "Linear Rec.709".into(),
            ColorSpace::REC709_DISPLAY => "Rec.709 (2.4)".into(),
            ColorSpace {
                primaries,
                transfer,
            } => format!("{primaries:?} / {transfer:?}"),
        }
    }
}

impl fmt::Display for ColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Primaries {
    /// A stable identifier, matching the saved form.
    pub fn id(self) -> &'static str {
        match self {
            Primaries::Rec709 => "rec709",
            Primaries::Rec2020 => "rec2020",
            Primaries::P3D65 => "p3_d65",
            Primaries::AcesAp0 => "aces_ap0",
            Primaries::AcesAp1 => "aces_ap1",
        }
    }

    /// Chromaticities `[red, green, blue, white]` as CIE xy.
    pub fn chromaticities(self) -> [[f64; 2]; 4] {
        const D65: [f64; 2] = [0.3127, 0.3290];
        const ACES_WHITE: [f64; 2] = [0.32168, 0.33767];
        match self {
            Primaries::Rec709 => [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], D65],
            Primaries::Rec2020 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65],
            Primaries::P3D65 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65],
            Primaries::AcesAp0 => [[0.7347, 0.2653], [0.0, 1.0], [0.0001, -0.0770], ACES_WHITE],
            Primaries::AcesAp1 => [[0.713, 0.293], [0.165, 0.830], [0.128, 0.044], ACES_WHITE],
        }
    }

    /// The matrix taking linear RGB in these primaries to CIE XYZ.
    pub fn to_xyz(self) -> Mat3 {
        let [r, g, b, w] = self.chromaticities();
        let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
        let (r, g, b) = (xyz(r), xyz(g), xyz(b));
        let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let s = mul_vec(&invert(&m), xyz(w));
        let mut out = m;
        for row in &mut out {
            for (c, scale) in row.iter_mut().zip(s) {
                *c *= scale;
            }
        }
        out
    }
}

/// The matrix converting linear RGB from one set of primaries to another,
/// with Bradford adaptation when the white points differ.
pub fn conversion_matrix(from: Primaries, to: Primaries) -> Mat3 {
    if from == to {
        return IDENTITY;
    }
    let white = |p: Primaries| {
        let [x, y] = p.chromaticities()[3];
        [x / y, 1.0, (1.0 - x - y) / y]
    };
    let (w_from, w_to) = (white(from), white(to));
    let mut adapt = IDENTITY;
    if w_from != w_to {
        const BRADFORD: Mat3 = [
            [0.8951, 0.2664, -0.1614],
            [-0.7502, 1.7135, 0.0367],
            [0.0389, -0.0685, 1.0296],
        ];
        let src = mul_vec(&BRADFORD, w_from);
        let dst = mul_vec(&BRADFORD, w_to);
        let scale = [
            [dst[0] / src[0], 0.0, 0.0],
            [0.0, dst[1] / src[1], 0.0],
            [0.0, 0.0, dst[2] / src[2]],
        ];
        adapt = mul(&invert(&BRADFORD), &mul(&scale, &BRADFORD));
    }
    mul(&invert(&to.to_xyz()), &mul(&adapt, &from.to_xyz()))
}

impl Transfer {
    /// A stable identifier, matching the saved form.
    pub fn id(self) -> &'static str {
        match self {
            Transfer::Linear => "linear",
            Transfer::Srgb => "srgb",
            Transfer::Gamma24 => "gamma24",
            Transfer::AcesCct => "aces_cct",
        }
    }

    /// Encoded value to linear light.
    pub fn decode(self, v: f64) -> f64 {
        match self {
            Transfer::Linear => v,
            Transfer::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Transfer::Gamma24 => v.max(0.0).powf(2.4),
            Transfer::AcesCct => {
                if v <= 0.155_251_141_552_511 {
                    (v - 0.072_905_534_195_835_5) / 10.540_237_741_654_5
                } else {
                    2f64.powf(v * 17.52 - 9.72)
                }
            }
        }
    }

    /// Linear light to encoded value.
    pub fn encode(self, v: f64) -> f64 {
        match self {
            Transfer::Linear => v,
            Transfer::Srgb => {
                if v <= 0.003_130_8 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Transfer::Gamma24 => v.max(0.0).powf(1.0 / 2.4),
            Transfer::AcesCct => {
                if v <= 0.007_812_5 {
                    10.540_237_741_654_5 * v + 0.072_905_534_195_835_5
                } else {
                    (v.log2() + 9.72) / 17.52
                }
            }
        }
    }
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

pub fn invert(m: &Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let inv = 1.0 / det;
    [
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: &Mat3, b: &Mat3, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!((a[i][j] - b[i][j]).abs() < tol, "{a:?} != {b:?}");
            }
        }
    }

    #[test]
    fn rec709_to_xyz_matches_published_matrix() {
        let expected = [
            [0.4124564, 0.3575761, 0.1804375],
            [0.2126729, 0.7151522, 0.0721750],
            [0.0193339, 0.1191920, 0.9503041],
        ];
        // Published tables use a slightly different D65 than the xy chromaticities give.
        assert_close(&Primaries::Rec709.to_xyz(), &expected, 3e-4);
    }

    #[test]
    fn rec709_to_ap1_matches_published_matrix() {
        // The commonly published Bradford-adapted sRGB → ACEScg matrix.
        let expected = [
            [0.6131, 0.3395, 0.0474],
            [0.0702, 0.9164, 0.0134],
            [0.0206, 0.1096, 0.8698],
        ];
        assert_close(
            &conversion_matrix(Primaries::Rec709, Primaries::AcesAp1),
            &expected,
            2e-3,
        );
    }

    #[test]
    fn white_maps_to_white_and_round_trips() {
        let m = conversion_matrix(Primaries::Rec709, Primaries::AcesAp1);
        for c in mul_vec(&m, [1.0, 1.0, 1.0]) {
            assert!((c - 1.0).abs() < 1e-9);
        }
        let back = conversion_matrix(Primaries::AcesAp1, Primaries::Rec709);
        assert_close(&mul(&back, &m), &IDENTITY, 1e-9);
    }

    #[test]
    fn ids_match_saved_form() {
        for p in [
            Primaries::Rec709,
            Primaries::Rec2020,
            Primaries::P3D65,
            Primaries::AcesAp0,
            Primaries::AcesAp1,
        ] {
            assert_eq!(
                serde_json::to_string(&p).unwrap(),
                format!("\"{}\"", p.id())
            );
        }
        for t in [
            Transfer::Linear,
            Transfer::Srgb,
            Transfer::Gamma24,
            Transfer::AcesCct,
        ] {
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.id())
            );
        }
    }

    #[test]
    fn transfers_round_trip() {
        for t in [
            Transfer::Linear,
            Transfer::Srgb,
            Transfer::Gamma24,
            Transfer::AcesCct,
        ] {
            for v in [0.0, 0.001, 0.18, 0.5, 1.0] {
                let back = t.decode(t.encode(v));
                assert!((back - v).abs() < 1e-9, "{t:?} at {v}: {back}");
            }
        }
        assert!((Transfer::Srgb.encode(0.18) - 0.4613).abs() < 1e-3);
        assert!((Transfer::AcesCct.encode(0.18) - 0.4135).abs() < 1e-3);
        // Both ACEScct segments meet at the published break point.
        assert!((Transfer::AcesCct.encode(0.0078125) - 0.155_251_141_552_511).abs() < 1e-12);
        assert!(
            (10.540_237_741_654_5 * 0.0078125 + 0.072_905_534_195_835_5 - 0.155_251_141_552_511f64)
                .abs()
                < 1e-12
        );
    }
}
