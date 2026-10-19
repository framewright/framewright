//! Stable identifiers.
//!
//! IDs are allocated by whoever builds a command, never inside the document,
//! so replaying a command journal produces the same document every time.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Identifies a node (including containers) within a document.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(Ulid);

impl NodeId {
    pub fn from_ulid(ulid: Ulid) -> NodeId {
        NodeId(ulid)
    }

    pub fn to_bytes(self) -> [u8; 16] {
        self.0.to_bytes()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl FromStr for NodeId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<NodeId, Self::Err> {
        Ulid::from_string(s).map(NodeId)
    }
}

/// A source of fresh IDs.
pub trait IdGen {
    fn next_id(&mut self) -> NodeId;
}

/// Time-ordered random IDs, for interactive use.
#[derive(Default)]
pub struct RandomIds(ulid::Generator);

impl IdGen for RandomIds {
    fn next_id(&mut self) -> NodeId {
        // The generator only fails if more than 2^80 IDs are made in one millisecond.
        NodeId(self.0.generate().expect("ULID generator overflowed"))
    }
}

/// Predictable IDs counting up from a seed, for tests and golden files.
pub struct SequentialIds {
    next: u128,
}

impl SequentialIds {
    pub fn new(seed: u64) -> SequentialIds {
        SequentialIds {
            next: (seed as u128) << 64 | 1,
        }
    }
}

impl IdGen for SequentialIds {
    fn next_id(&mut self) -> NodeId {
        let id = NodeId(Ulid(self.next));
        self.next += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_ids_are_predictable_and_round_trip() {
        let mut ids = SequentialIds::new(7);
        let a = ids.next_id();
        let b = ids.next_id();
        assert_ne!(a, b);
        assert_eq!(SequentialIds::new(7).next_id(), a);
        assert_eq!(a.to_string().parse::<NodeId>().unwrap(), a);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<NodeId>(&json).unwrap(), a);
    }
}
