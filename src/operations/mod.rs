//! Explicit administration over storage and transactor-owned publication mechanisms.
mod deployment;
pub(crate) mod excision;
mod index;
mod inspect;
mod model;
mod operator;
pub(crate) mod projection;
mod reclamation;
pub use excision::{ExcisionConfig, ExcisionProgress};
pub use index::IndexMaintenanceReceipt;
pub use model::*;
pub use operator::PostgresOperator;
pub use reclamation::RetiredDatabaseReclamation;
