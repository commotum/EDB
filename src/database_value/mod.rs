//! Shared immutable database values, selective reads and speculative branches.
//! Peers capture these handles; transactions and local computation use the same
//! value engine. View/overlay state is immutable; read_context is discardable.
mod cursor;
mod hints;
pub(crate) mod log;
mod overlay;
pub(crate) mod read_context;
pub(crate) mod reference;
mod resolve;
mod value;
mod views;

pub use cursor::{DatabaseValuePrefixCursor, DatabaseValueScanCursor};
pub use resolve::RawIndexValue;
pub use value::{DatabaseValue, SpeculativeTransactionReport};

pub(crate) mod invoke;
