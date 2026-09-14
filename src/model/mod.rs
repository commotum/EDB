//! The information model shared by peers and transaction processing.
//!
//! Value meaning is independent of application syntax, publication and physical
//! storage. Encoders retain exact input representation; logical comparison is a
//! separate contract used by indexes, identity and declarative computation.

pub(crate) mod datom;
pub(crate) mod identity;
pub(crate) mod idents;
pub(crate) mod schema;
pub(crate) mod uri;
pub(crate) mod value;
pub(crate) mod vocabulary;
