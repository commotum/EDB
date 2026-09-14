//! Service ownership: activate one identity and join/release its workers.
use super::activation;
use super::client::TransactionClient;
use super::config::{BackgroundIndexingConfig, ServiceOptions, TransactionServiceConfig};
use super::observations::{BackgroundIndexingStats, WriterResidencyStats};
use crate::model::identity::DatabaseIdentity;
use crate::{PostgresConnectionConfig, ProgramCacheStats, RecoveryStats, SemanticError};
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

pub struct TransactionService {
    pub(super) client: TransactionClient,
    pub(super) recovery_stats: RecoveryStats,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) index_worker: Option<JoinHandle<()>>,
}

pub(super) fn resolve_service_database_name(
    connection: &PostgresConnectionConfig,
    name: &str,
) -> Result<crate::storage::BlockDatabase, SemanticError> {
    crate::storage::BlockDatabase::resolve(connection, name)
}

impl TransactionService {
    /// Start a compiled Rust transactor with an immutable native deployment
    /// registry. Accepted retries do not consult this execution configuration.
    pub fn start_with_execution_options(
        config: TransactionServiceConfig,
        options: crate::TransactionExecutionOptions,
    ) -> Result<Self, SemanticError> {
        Self::start_with_indexing_and_execution_options(
            config,
            BackgroundIndexingConfig::default(),
            options,
        )
    }

    pub fn start_with_defaults(
        config: TransactionServiceConfig,
        defaults: crate::TransactionDefaults,
    ) -> Result<Self, SemanticError> {
        Self::start_with_indexing_and_defaults(
            config,
            BackgroundIndexingConfig::default(),
            defaults,
        )
    }

    /// Start every service-owned connection with the configuration's transport policy.
    pub fn start(config: TransactionServiceConfig) -> Result<Self, SemanticError> {
        Self::start_with_indexing(config, BackgroundIndexingConfig::default())
    }
    pub fn start_with_indexing(
        config: TransactionServiceConfig,
        indexing_config: BackgroundIndexingConfig,
    ) -> Result<Self, SemanticError> {
        Self::start_with_indexing_and_defaults(
            config,
            indexing_config,
            crate::TransactionDefaults::default(),
        )
    }

    /// Start a writer with explicit fresh-transaction allocation policy. The
    /// policy is not transaction data and never changes a stored retry result.
    pub fn start_with_indexing_and_defaults(
        config: TransactionServiceConfig,
        indexing_config: BackgroundIndexingConfig,
        defaults: crate::TransactionDefaults,
    ) -> Result<Self, SemanticError> {
        Self::start_with_indexing_and_execution_options(
            config,
            indexing_config,
            crate::TransactionExecutionOptions {
                defaults,
                native: crate::NativeRegistry::default(),
            },
        )
    }

    pub fn start_with_indexing_and_execution_options(
        config: TransactionServiceConfig,
        indexing_config: BackgroundIndexingConfig,
        options: crate::TransactionExecutionOptions,
    ) -> Result<Self, SemanticError> {
        Self::start_with_options(
            config,
            ServiceOptions {
                indexing: indexing_config,
                execution: options,
                ..Default::default()
            },
        )
    }

    pub fn start_with_options(
        config: TransactionServiceConfig,
        options: ServiceOptions,
    ) -> Result<Self, SemanticError> {
        Self::validate_config(&config)?;
        options.indexing.validate()?;
        options.excision.validate()?;
        options.fulltext_build_limits.validate()?;
        options.hint_prefetch.validate()?;
        crate::index::prepare::validate_index_preparation_parallelism(
            options.index_preparation_parallelism,
        )?;
        let database = resolve_service_database_name(&config.connection, &config.database_id)?;
        Self::start_identity_with_options(config, options, database)
    }

    pub(super) fn validate_config(config: &TransactionServiceConfig) -> Result<(), SemanticError> {
        if config.database_id.is_empty()
            || config.queue_capacity == 0
            || config.lease_duration.is_zero()
            || config.renew_interval.is_zero()
            || config.renew_interval >= config.lease_duration
        {
            return Err(SemanticError::incorrect(
                "service/invalid-config",
                "database id and queue capacity are required and renewal must precede lease expiry",
            ));
        }
        Ok(())
    }

    // Only callers that already captured an immutable block identity may enter.
    // BlockTransactor acquires authority through guarded opaque references.
    pub(super) fn start_identity_with_options(
        config: TransactionServiceConfig,
        options: ServiceOptions,
        database: crate::storage::BlockDatabase,
    ) -> Result<Self, SemanticError> {
        activation::start(config, options, database)
    }

    pub fn client(&self) -> TransactionClient {
        self.client.clone()
    }

    /// Stable catalog identity retained by this service activation.
    pub fn identity(&self) -> DatabaseIdentity {
        self.client.identity()
    }

    pub fn recovery_stats(&self) -> RecoveryStats {
        self.recovery_stats
    }

    /// Cumulative decoded-program work in the actual writer's bounded cache.
    pub fn program_cache_stats(&self) -> ProgramCacheStats {
        crate::program_cache::stats(&self.client.shared.program_cache)
    }

    pub fn background_indexing_stats(&self) -> BackgroundIndexingStats {
        self.client.background_indexing_stats()
    }

    pub fn writer_residency_stats(&self) -> WriterResidencyStats {
        self.client.writer_residency_stats()
    }

    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    pub(super) fn stop_and_join(&mut self) {
        self.client.shared.accepting.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.client.shared.indexing.shutdown();
        if let Some(worker) = self.index_worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for TransactionService {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
