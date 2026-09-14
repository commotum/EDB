//! User-facing assembly of independent readers, submission endpoints and optional embedded services.
pub(crate) mod connection;

pub(crate) mod asynchronous;
#[cfg(unix)]
pub(crate) mod transport;
