//! Shared collection mechanisms, independent of database and cache policy.
//!
//! Immutable values need path-copied ownership. Cache metadata needs only local
//! mutation: preserving old recency versions would not preserve any user data.

mod lru;
pub(crate) mod persistent_map;

pub(crate) use lru::LruMap;
