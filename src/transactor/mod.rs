//! One serialized write authority with independent bounded preparation lanes.
//!
//! Recovered update.clj separates processor, fressianer and batched writer.
//! Here writer::run owns the complete fresh transaction and resolves uncertain
//! publication before accepting another result. Index/excision workers prepare
//! candidates; only an owning transactor may adopt them. This is an explicit scheduling
//! adaptation, not a claim of source pipeline or throughput equivalence.
//! Client admission, immutable reports and local observations grant no authority.

mod activation;
pub(crate) mod authority;
pub(crate) mod client;
mod config;
mod excision_lane;
pub(crate) mod excision_operator;
mod index_lane;
mod indexing;
mod observations;
mod request;
mod service;
mod standby;
mod writer;

pub use authority::{BlockTransactor, BlockWriterOptions};
pub use client::{ReportSubscription, TransactionClient};
pub use config::{
    BackgroundIndexingConfig, CapacityLimits, ServiceOptions, TransactionServiceConfig,
};
pub use observations::{
    BackgroundFulltextStats, BackgroundIndexingFailure, BackgroundIndexingStats,
    OperationalServiceStats, ServiceStats, WriterResidencyStats,
};
pub use request::{IndexRequest, ServiceTransactionReport, TransactionRequest, TransactionTicket};
pub use service::TransactionService;
pub use standby::{StandbyStatus, TransactionStandby};
