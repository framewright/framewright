//! Content hashing for cache keys and media fingerprints.
//!
//! Keys are built from explicit, canonical bytes. Never hash `Debug` output or
//! serialised JSON: both can change without the value changing.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{NodeId, Time};

/// A blake3 hash of some content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn of_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash(*blake3::hash(bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 12 hex digits, for logs and `fw inspect`.
    pub fn short(&self) -> String {
        self.to_string()[..12].to_owned()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.short())
    }
}

impl FromStr for ContentHash {
    type Err = String;

    fn from_str(s: &str) -> Result<ContentHash, String> {
        if s.len() != 64 || !s.is_ascii() {
            return Err(format!("{s:?} is not a 64-digit hex hash"));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| format!("{s:?} is not a 64-digit hex hash"))?;
        }
        Ok(ContentHash(out))
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<ContentHash, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Builds a hash from typed, unambiguous writes.
///
/// Every variable-length write is length-prefixed, so `("ab", "c")` and
/// `("a", "bc")` hash differently.
pub struct KeyHasher(blake3::Hasher);

impl KeyHasher {
    /// `domain` separates unrelated kinds of key, e.g. `"fw.node-output"`.
    pub fn new(domain: &str) -> KeyHasher {
        let mut h = KeyHasher(blake3::Hasher::new());
        h.str(domain);
        h
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.u64(bytes.len() as u64);
        self.0.update(bytes);
        self
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.update(&[v]);
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    /// Hashes the value, not the bit pattern: `-0.0` equals `0.0` and every NaN is one NaN.
    pub fn f64(&mut self, v: f64) -> &mut Self {
        let v = if v == 0.0 {
            0.0
        } else if v.is_nan() {
            f64::NAN
        } else {
            v
        };
        self.u64(v.to_bits())
    }

    pub fn time(&mut self, t: Time) -> &mut Self {
        self.i64(t.num()).i64(t.den())
    }

    pub fn node_id(&mut self, id: NodeId) -> &mut Self {
        self.0.update(&id.to_bytes());
        self
    }

    pub fn hash(&mut self, h: &ContentHash) -> &mut Self {
        self.0.update(h.as_bytes());
        self
    }

    pub fn finish(&self) -> ContentHash {
        ContentHash(*self.0.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_unambiguous() {
        let a = KeyHasher::new("t").str("ab").str("c").finish();
        let b = KeyHasher::new("t").str("a").str("bc").finish();
        assert_ne!(a, b);
        assert_ne!(KeyHasher::new("x").finish(), KeyHasher::new("y").finish());
    }

    #[test]
    fn floats_hash_by_value() {
        let k = |v: f64| KeyHasher::new("t").f64(v).finish();
        assert_eq!(k(0.0), k(-0.0));
        assert_eq!(k(f64::NAN), k(-f64::NAN));
        assert_ne!(k(1.0), k(1.0 + f64::EPSILON));
    }

    #[test]
    fn hex_round_trips() {
        let h = ContentHash::of_bytes(b"framewright");
        assert_eq!(h.to_string().parse::<ContentHash>().unwrap(), h);
        assert_eq!(h.short().len(), 12);
        assert!("zz".parse::<ContentHash>().is_err());
    }
}
