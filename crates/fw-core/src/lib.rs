//! Core types shared by every Framewright crate.
//!
//! Nothing here touches the GPU, the filesystem or the clock.

pub mod color;
pub mod geom;
pub mod hash;
pub mod id;
pub mod image;
pub mod planar;
pub mod time;

pub use color::{ColorSpace, Primaries, Transfer};
pub use geom::{Rect, Vec2};
pub use hash::{ContentHash, KeyHasher};
pub use id::{IdGen, NodeId};
pub use image::RgbaImage;
pub use planar::{DecodedImage, PlanarImage};
pub use time::{Time, TimeError};
