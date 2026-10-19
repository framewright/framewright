//! Exact rational time.
//!
//! Time is always an exact fraction of seconds. Floating-point time is never
//! used, and arithmetic that overflows is an error rather than being clamped.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("time arithmetic overflowed")]
    Overflow,
    #[error("time has a zero denominator")]
    ZeroDenominator,
    #[error("cannot parse {0:?} as a time (expected e.g. \"1.5\" or \"3/2\")")]
    Parse(String),
}

/// A time in seconds, stored as a reduced fraction with a positive denominator.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Time {
    num: i64,
    den: i64,
}

impl Time {
    pub const ZERO: Time = Time { num: 0, den: 1 };

    pub fn new(num: i64, den: i64) -> Result<Time, TimeError> {
        Time::from_wide(num as i128, den as i128)
    }

    pub fn from_seconds(seconds: i64) -> Time {
        Time {
            num: seconds,
            den: 1,
        }
    }

    pub fn num(self) -> i64 {
        self.num
    }

    pub fn den(self) -> i64 {
        self.den
    }

    pub fn checked_add(self, other: Time) -> Result<Time, TimeError> {
        let num = self.num as i128 * other.den as i128 + other.num as i128 * self.den as i128;
        Time::from_wide(num, self.den as i128 * other.den as i128)
    }

    pub fn checked_sub(self, other: Time) -> Result<Time, TimeError> {
        let num = self.num as i128 * other.den as i128 - other.num as i128 * self.den as i128;
        Time::from_wide(num, self.den as i128 * other.den as i128)
    }

    pub fn checked_mul(self, other: Time) -> Result<Time, TimeError> {
        Time::from_wide(
            self.num as i128 * other.num as i128,
            self.den as i128 * other.den as i128,
        )
    }

    pub fn checked_div(self, other: Time) -> Result<Time, TimeError> {
        Time::from_wide(
            self.num as i128 * other.den as i128,
            self.den as i128 * other.num as i128,
        )
    }

    /// The index of the frame containing this time at `rate` frames per second,
    /// rounding towards negative infinity.
    pub fn frame_index(self, rate: Time) -> Result<i64, TimeError> {
        let frames = self.checked_mul(rate)?;
        Ok(frames.num.div_euclid(frames.den))
    }

    /// The index of the sample containing this time at `sample_rate` Hz,
    /// rounding towards negative infinity.
    pub fn sample_index(self, sample_rate: u32) -> Result<i64, TimeError> {
        self.frame_index(Time::from_seconds(sample_rate as i64))
    }

    /// The start time of frame `index` at `rate` frames per second.
    pub fn of_frame(index: i64, rate: Time) -> Result<Time, TimeError> {
        Time::from_seconds(index).checked_div(rate)
    }

    /// Lossy conversion for interpolation weights and display. Never feed the
    /// result back into time arithmetic.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    fn from_wide(num: i128, den: i128) -> Result<Time, TimeError> {
        if den == 0 {
            return Err(TimeError::ZeroDenominator);
        }
        let divisor = gcd(num.unsigned_abs(), den.unsigned_abs()) as i128;
        let sign = if den < 0 { -1 } else { 1 };
        let num = sign * num / divisor;
        let den = sign * den / divisor;
        Ok(Time {
            num: i64::try_from(num).map_err(|_| TimeError::Overflow)?,
            den: i64::try_from(den).map_err(|_| TimeError::Overflow)?,
        })
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

impl Default for Time {
    fn default() -> Self {
        Time::ZERO
    }
}

impl Ord for Time {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.num as i128 * other.den as i128).cmp(&(other.num as i128 * self.den as i128))
    }
}

impl PartialOrd for Time {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Time({self})")
    }
}

impl FromStr for Time {
    type Err = TimeError;

    /// Parses `"3"`, `"3/2"` or an exact decimal such as `"1.5"` or `"-0.04"`.
    fn from_str(s: &str) -> Result<Time, TimeError> {
        let err = || TimeError::Parse(s.to_owned());
        let s = s.trim();
        if let Some((num, den)) = s.split_once('/') {
            let num: i64 = num.trim().parse().map_err(|_| err())?;
            let den: i64 = den.trim().parse().map_err(|_| err())?;
            return Time::new(num, den);
        }
        let (negative, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
        let valid = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() && frac.is_empty() || !valid(whole) || !valid(frac) {
            return Err(err());
        }
        let mut num: i128 = 0;
        let mut den: i128 = 1;
        for b in whole.bytes().chain(frac.bytes()) {
            num = num.checked_mul(10).ok_or(TimeError::Overflow)? + (b - b'0') as i128;
        }
        for _ in 0..frac.len() {
            den = den.checked_mul(10).ok_or(TimeError::Overflow)?;
        }
        Time::from_wide(if negative { -num } else { num }, den)
    }
}

/// Times are saved as strings like `"3/2"` so they stay exact and readable.
impl Serialize for Time {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Time, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    #[test]
    fn reduces_and_normalises_sign() {
        assert_eq!(Time::new(6, -4).unwrap(), t("-3/2"));
        assert_eq!(Time::new(0, -7).unwrap(), Time::ZERO);
        assert_eq!(Time::new(1, 0), Err(TimeError::ZeroDenominator));
    }

    #[test]
    fn parses_decimals_exactly() {
        assert_eq!(t("1.5"), Time::new(3, 2).unwrap());
        assert_eq!(t("-0.04"), Time::new(-1, 25).unwrap());
        assert_eq!(t("2"), Time::from_seconds(2));
        assert_eq!(t(" 1001/30000 ").to_string(), "1001/30000");
        for bad in ["", ".", "1.2.3", "abc", "1/x", "--1"] {
            assert!(bad.parse::<Time>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn overflow_is_an_error() {
        let big = Time::from_seconds(i64::MAX);
        assert_eq!(
            big.checked_add(Time::from_seconds(1)),
            Err(TimeError::Overflow)
        );
        assert_eq!(
            big.checked_mul(Time::from_seconds(2)),
            Err(TimeError::Overflow)
        );
    }

    #[test]
    fn frames_and_samples() {
        let ntsc = Time::new(30000, 1001).unwrap();
        assert_eq!(Time::of_frame(30, ntsc).unwrap(), t("1001/1000"));
        assert_eq!(t("1001/1000").frame_index(ntsc).unwrap(), 30);
        assert_eq!(t("-1/1000").frame_index(ntsc).unwrap(), -1);
        assert_eq!(t("1.5").sample_index(48000).unwrap(), 72000);
    }

    #[test]
    fn serialises_as_string() {
        assert_eq!(serde_json::to_string(&t("1.5")).unwrap(), "\"3/2\"");
        assert_eq!(serde_json::from_str::<Time>("\"3/2\"").unwrap(), t("1.5"));
    }

    proptest! {
        #[test]
        fn add_then_sub_round_trips(a in -10_000i64..10_000, b in 1i64..10_000, c in -10_000i64..10_000, d in 1i64..10_000) {
            let x = Time::new(a, b).unwrap();
            let y = Time::new(c, d).unwrap();
            prop_assert_eq!(x.checked_add(y).unwrap().checked_sub(y).unwrap(), x);
            prop_assert_eq!(x.to_string().parse::<Time>().unwrap(), x);
        }

        #[test]
        fn ordering_matches_floats(a in -10_000i64..10_000, b in 1i64..10_000, c in -10_000i64..10_000, d in 1i64..10_000) {
            let x = Time::new(a, b).unwrap();
            let y = Time::new(c, d).unwrap();
            if x != y {
                prop_assert_eq!(x < y, x.as_f64() < y.as_f64());
            }
        }
    }
}
