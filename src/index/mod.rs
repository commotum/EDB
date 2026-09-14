//! Ordered access to immutable database information.
//!
//! Boundaries, recent authenticated events, speculative overlays and durable
//! tree values share model ordering without sharing mutable ownership policy.
mod boundary;
pub(crate) mod cursor;
pub(crate) mod eager;
mod io;
pub(crate) mod metadata;
pub(crate) mod overlay;
pub(crate) mod prepare;
pub mod recent;
pub mod tree;

pub use boundary::{IndexBoundary, IndexComponents, IndexPrefix, IndexTransaction};
pub(crate) use boundary::{NormalizedIndexBoundary, compare_prefix};
pub use io::NodeBlockReadStats;
