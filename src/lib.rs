//! Native Rust/PostgreSQL Datomic-inspired database.
//!
//! The kernel applies declarative transaction information to immutable
//! database values. PostgreSQL stores the serialized durable log and indexes;
//! independent peers provide local queries, pull, temporal views, and native
//! immutable snapshots. Memory and durable writes share transaction assessment;
//! the memory representation is not an independent semantic oracle.

pub use application::asynchronous::{
    AsyncClient, AsyncStream, AsyncStreamOptions, AsyncTransaction,
};
pub use runtime::executor::{AsyncConfig, AsyncExecutor, AsyncOperation, AsyncStats};
mod application;
mod backup;
mod collections;
mod database;
mod observation;
mod runtime;
pub use storage::catalog::{CreateDatabaseResult, DatabaseCatalog, DatabaseCatalogEntry};
mod database_stats;
mod database_value;
pub mod edn;
pub mod edn_pull;
pub mod edn_query;
pub mod edn_transaction;
pub mod edn_value;
mod encoding;
mod entity_identity;
mod error;
mod excision;
mod fulltext;
pub mod index;
mod operations;
mod partitions;
mod reserved_allocation;
pub use partitions::TransactionDefaults;
mod peer;
mod postgres_connection;
mod program;
mod program_bindings;
mod program_cache;
mod pull;
mod query;
#[cfg(unix)]
pub use application::transport::remote::config::{
    remote_client_config_from_env, remote_server_credentials_from_env,
};
mod io_diagnostics;
mod maintenance_control;
mod model;
pub mod sql_io;
mod ssd_cache;
mod state_commitment;
pub mod storage;
mod telemetry;
mod time_point;
mod transaction;
mod transaction_hints;
mod transaction_stats;
mod transactor;
mod uuid;

pub use application::connection::{Connection, ConnectionTransactionTicket};
#[cfg(unix)]
pub use application::transport::local::{
    CommittedTransaction, LocalTransactionEndpoint, LocalTransactionServer, LocalTransportConfig,
};
#[cfg(unix)]
pub use application::transport::remote::routing::RemoteWriter;
#[cfg(unix)]
pub use application::transport::remote::{
    RemoteAuthToken, RemoteClientConfig, RemoteTransactionEndpoint, RemoteTransactionServer,
    RemoteTransportConfig, RemoteTransportStats, RemoteWriterEndpoint,
};
pub use backup::snapshot::{BackupConnection, BackupReadConfig, BackupReadStats};
pub use backup::{
    BackupFault, BackupPoint, BackupVerification, PortableBackup, RestoreFault, RestoreResult,
};
pub use database::{Database, TxReport, View};
pub use database_stats::{AttributeStats, DatabaseStats};
pub use database_value::invoke::{InvokeControl, InvokeRole};
pub use database_value::log::{LogCursor, LogCursorStats, LogTransaction, LogValue};
pub use database_value::reference::{SnapshotKey, SnapshotReference};
pub use database_value::{
    DatabaseValue, DatabaseValuePrefixCursor, DatabaseValueScanCursor, RawIndexValue,
    SpeculativeTransactionReport,
};
pub use encoding::{
    Digest, DurableTransaction, canonical_datom_bytes, decode_genesis, decode_program,
    decode_transaction, encode_genesis, encode_program, encode_program_output, encode_transaction,
    program_hash, program_request_digest, request_digest, sha256, submission_request_digest,
    transaction_hash,
};
pub use entity_identity::EntityIdentity;
pub use error::{ErrorCategory, SemanticError};
pub use fulltext::{
    FulltextBuildLimits, FulltextBuildStats, FulltextCacheStats, FulltextCursor, FulltextHit,
    FulltextOptions, FulltextProjection, FulltextReadLimits, FulltextReadStats, FulltextRecord,
    FulltextReport, FulltextStats, NativeFulltextReader,
};
pub use index::NodeBlockReadStats;
pub use index::{IndexBoundary, IndexComponents, IndexPrefix, IndexTransaction};
pub use io_diagnostics::{CacheIoStats, CacheTier, IndexIoStats, ReadIoStats};
pub use maintenance_control::{MaintenanceControl, MaintenanceStats};
pub use model::datom::{Datom, IndexOrder};
pub use model::identity::DatabaseIdentity;
pub use model::identity::{
    DB_PARTITION, EIDX_BITS, EIDX_MASK, INITIAL_EIDX_FRONTIER, MAX_EID, MAX_EIDX, MAX_PARTITION,
    PARTITION_BITS, TX_PARTITION, USER_PARTITION, eid_to_eidx, eid_to_part, implicit_part,
    implicit_part_id, make_eid, partition_eid, t_to_tx, tx_to_t,
};
pub use model::schema::{Attribute, Cardinality, Schema, TupleSpec, Unique, ValueType};
pub use model::value::{Keyword, Symbol, Value};
pub use model::vocabulary::{
    DB_ADD, DB_ALTER_ATTRIBUTE, DB_ATTR_PREDS, DB_CARDINALITY, DB_CARDINALITY_MANY,
    DB_CARDINALITY_ONE, DB_DOC, DB_ENSURE, DB_ENTITY_ATTRS, DB_ENTITY_PREDS, DB_EXCISE,
    DB_EXCISE_ATTRS, DB_EXCISE_BEFORE, DB_EXCISE_BEFORE_T, DB_FN, DB_FN_CAS, DB_FN_RETRACT_ENTITY,
    DB_FULLTEXT, DB_IDENT, DB_INDEX, DB_INSTALL_ATTRIBUTE, DB_INSTALL_PARTITION, DB_IS_COMPONENT,
    DB_NO_HISTORY, DB_PART_DB, DB_PART_TX, DB_PART_USER, DB_RETRACT, DB_TUPLE_ATTRS,
    DB_TUPLE_DISCONTINUED, DB_TUPLE_TYPE, DB_TUPLE_TYPES, DB_TX_INSTANT, DB_TYPE_BIGDEC,
    DB_TYPE_BIGINT, DB_TYPE_BOOLEAN, DB_TYPE_BYTES, DB_TYPE_DOUBLE, DB_TYPE_FLOAT, DB_TYPE_FN,
    DB_TYPE_INSTANT, DB_TYPE_KEYWORD, DB_TYPE_LONG, DB_TYPE_REF, DB_TYPE_STRING, DB_TYPE_SYMBOL,
    DB_TYPE_TUPLE, DB_TYPE_URI, DB_TYPE_UUID, DB_UNIQUE, DB_UNIQUE_IDENTITY, DB_UNIQUE_VALUE,
    DB_VALUE_TYPE, MAX_SCHEMA_ATTRIBUTE_ID, canonical_genesis_datoms, schema_eid_to_attr_id,
};
pub use observation::consumer::{
    ChangeCheckpoint, ChangeConsumer, ChangeConsumerConfig, ChangeConsumerStats, ChangeEvent,
};
pub use observation::notices::{
    NoticeListenerStats, NoticePublisherStats, ObservationConfig, notice_listener_stats,
    notice_publisher_stats,
};
pub use operations::{
    ExcisionConfig, ExcisionFault, ExcisionProgress, ExcisionReceipt, GarbageInventory,
    IndexMaintenanceReceipt, IntegrityProblem, IntegrityReport, MAX_COLLECTION_STEPS,
    OperationalMetrics, PostgresOperator, RECOMMENDED_GARBAGE_COLLECTION_AGE,
    RetiredDatabaseReclamation,
};
pub use peer::{
    CacheStats, Peer, PeerCursorStats, PeerIndexCursor, PeerLoadStats, PeerSnapshot, RecoveryStats,
};
pub use postgres_connection::environment::postgres_config_from_env;
pub use postgres_connection::{PostgresConnectionConfig, PostgresIoPolicy};
pub use program::{
    CallableRef, Instruction, MAX_QUERY_PATTERNS, MAX_QUERY_VARIABLES, PROGRAM_ABI_VERSION,
    Program, ProgramBudget, ProgramCall, ProgramControl, ProgramHash, ProgramInvocation,
    ProgramKind, ProgramLimits, ProgramOutput, ProgramRuntime, QueryPattern, QueryTemplate,
    QueryTemplateSource, QueryTemplateTime, QueryTerm, RuntimeValue, is_exact_true,
    require_exact_true,
};
pub use program_cache::ProgramCacheStats;
pub use pull::{
    AttributeName, Entity, EntityIdentifier, EntityValue, IndexPullCursor, IndexPullOptions,
    PullAttribute, PullControl, PullDirection, PullLimit, PullNested, PullPattern, PullTransform,
};
pub use query::{
    Aggregate, AggregateArg, AggregateCall, AggregateGroup, AggregateSource, AggregateValue,
    Binding, Clause, DataPattern, FindElement, FindSpec, Function, InputSpec, PlanStep, Predicate,
    PreparedQuery, PreparedQueryCache, PreparedQueryCacheStats, Query, QueryClauseStep,
    QueryControl, QueryDataSource, QueryDiagnosticOptions, QueryDiagnostics, QueryEngine,
    QueryExtensions, QueryInput, QueryOutcome, QueryPhase, QueryPhaseKind, QueryResult,
    QuerySequence, QuerySource, QuerySourceValue, QueryStats, QueryStepStatus, QueryStepWork,
    QueryValue, QueryWarning, RelationPattern, Rule, Term, Variable, query_diagnostics_to_edn,
};
pub use query::{ReturnMap, ReturnMapShape, ReturnMaps};
pub use sql_io::{
    OperationContext, OperationKind, SqlCallKind, SqlCallStats, SqlIoReport, SqlIoStats,
    SqlMetricCallback, process_sql_stats,
};
pub use ssd_cache::{SsdCache, SsdCacheConfig, SsdCacheLimits, SsdCacheStats};
pub use telemetry::{
    TelemetryConfig, TelemetryEmitter, TelemetryPhase, TelemetryPublisher, TelemetrySnapshot,
    TelemetryStats, TelemetryWarnings, io_report_to_edn,
};
pub use time_point::TimePoint;
pub use transaction::SpeculationLimits;
pub use transaction::native::{
    NativeCallContext, NativeRegistry, NativeRegistryBuilder, TransactionExecutionOptions,
    native_deployment_attribute, native_deployment_ident,
};
pub use transaction::{
    AttributeRef, EntityMap, EntityRef, MapValue, TxCall, TxForm, TxFunctions, TxOp, TxValue,
};
pub use transaction_hints::{
    HintExecution, HintLimits, HintPrefetchConcurrency, HintPrefetchOptions, HintPrefetchStats,
    HintTraceStats, HintedSpeculation, ReadHint, TransactionHints,
};
pub use transaction_stats::{TransactionDiagnostics, TransactionWorkStats};
pub use transactor::{
    BackgroundFulltextStats, BackgroundIndexingConfig, BackgroundIndexingFailure,
    BackgroundIndexingStats, IndexRequest, OperationalServiceStats, ReportSubscription,
    ServiceOptions, ServiceStats, ServiceTransactionReport, StandbyStatus, TransactionClient,
    TransactionRequest, TransactionService, TransactionServiceConfig, TransactionStandby,
    TransactionTicket,
};
pub use transactor::{BlockTransactor, BlockWriterOptions, CapacityLimits, WriterResidencyStats};
pub use uuid::{squuid, squuid_at, squuid_time_millis, uuid_v7, uuid_v7_at, uuid_v7_time_millis};
