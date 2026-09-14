use super::*;
use crate::{Digest, PostgresConnectionConfig, SemanticError};
use std::time::Duration;

pub struct PostgresOperator {
    pub(super) connection: PostgresConnectionConfig,
    pub(super) maintenance: crate::MaintenanceControl,
    pub(super) fulltext_build_limits: crate::FulltextBuildLimits,
    pub(super) tree_config: crate::index::tree::TreeConfig,
}

impl PostgresOperator {
    pub fn connect(connection: &str) -> Result<Self, SemanticError> {
        Self::connect_configured(&PostgresConnectionConfig::parse(connection)?)
    }

    pub fn connect_configured(
        connection: &PostgresConnectionConfig,
    ) -> Result<Self, SemanticError> {
        crate::storage::PgBlockStore::connect(connection)?;
        Ok(Self {
            connection: connection.clone(),
            maintenance: crate::MaintenanceControl::default(),
            fulltext_build_limits: crate::FulltextBuildLimits::default(),
            tree_config: crate::index::tree::TreeConfig::default(),
        })
    }

    pub fn with_maintenance_control(mut self, control: crate::MaintenanceControl) -> Self {
        self.maintenance = control;
        self
    }

    /// Configure physical tree construction for consolidation and explicit
    /// current-index recovery. Existing untouched nodes remain shared; this
    /// does not change logical value limits or the independent search builder.
    pub fn with_tree_config(
        mut self,
        config: crate::index::tree::TreeConfig,
    ) -> Result<Self, SemanticError> {
        config.validate()?;
        self.tree_config = config;
        Ok(self)
    }

    /// Configure search-build admission for consolidation, explicit current
    /// index recovery, search rebuild and excision. Defaults are unchanged.
    pub fn with_fulltext_build_limits(
        mut self,
        limits: crate::FulltextBuildLimits,
    ) -> Result<Self, SemanticError> {
        limits.validate()?;
        self.fulltext_build_limits = limits;
        Ok(self)
    }

    /// Validate and protect immutable native code before binding its hash in a
    /// transaction. Fixed dependencies must already exist; the complete code
    /// closure is authenticated and protected under one collector epoch.
    /// Staging survives for the collection grace period; no live handle is needed.
    pub fn deploy_program(
        &mut self,
        program: &crate::Program,
    ) -> Result<crate::ProgramHash, SemanticError> {
        deployment::deploy(&self.connection, program, &self.maintenance)
    }

    /// Stage a program closure and bind its ident in one application operation.
    /// Dependencies can be supplied in any order. Exact retries return the
    /// committed outcome before touching deployment dependencies.
    pub fn install_program(
        &mut self,
        client: &crate::TransactionClient,
        request_key: &str,
        ident: crate::Keyword,
        program: &crate::Program,
        dependencies: &[crate::Program],
        timeout: Duration,
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        deployment::install(
            self,
            client,
            request_key,
            ident,
            program,
            dependencies,
            timeout,
        )
    }

    /// Explicitly consolidate a captured transaction prefix. Damaged current
    /// indexes may be reconstructed from the authenticated canonical log;
    /// this never drops old receipt or read-authorization roots.
    pub fn consolidate_database(
        &mut self,
        database_id: &str,
    ) -> Result<IndexMaintenanceReceipt, SemanticError> {
        index::maintain(
            &self.connection,
            database_id,
            false,
            None,
            &self.maintenance,
            &self.fulltext_build_limits,
            &self.tree_config,
        )
    }

    /// Rebuild derived search with no predecessor dependency. An explicit
    /// expected index guards the exact repair target; no old objects are
    /// physically removed, including objects retained by captured readers.
    pub fn rebuild_fulltext(
        &mut self,
        database_id: &str,
        expected_index: Option<Digest>,
    ) -> Result<IndexMaintenanceReceipt, SemanticError> {
        index::maintain(
            &self.connection,
            database_id,
            true,
            expected_index,
            &self.maintenance,
            &self.fulltext_build_limits,
            &self.tree_config,
        )
    }

    /// Authenticate one immutable root. Deep mode additionally validates all
    /// retained object links, log replay, exact receipts and index projections.
    pub fn inspect_database(
        &mut self,
        database_id: &str,
        deep_derived: bool,
    ) -> Result<IntegrityReport, SemanticError> {
        inspect::inspect(
            &self.connection,
            database_id,
            deep_derived,
            &self.maintenance,
        )
    }

    /// Read-only provider metadata and collector status; never starts a cycle.
    pub fn garbage_inventory(
        &mut self,
        older_than: Duration,
    ) -> Result<GarbageInventory, SemanticError> {
        inspect::inventory(&self.connection, older_than, &self.maintenance)
    }

    /// Advance bounded, resumable collection. Complete means this sealed cycle
    /// finished, not global quiescence or revocation of still-owned values.
    pub fn collect_garbage(
        &mut self,
        older_than: Duration,
    ) -> Result<GarbageInventory, SemanticError> {
        inspect::collect(&self.connection, older_than, &self.maintenance)
    }

    /// Process ordinary A=15 (`:db/excise`) facts through a background,
    /// copy-on-write log generation. A backup remains strongly recommended,
    /// matching Datomic's operational guidance, but it is not an authorization
    /// token and is therefore absent from this semantic API.
    /// `database_id` is the stable ID returned by the catalog, not a mutable name.
    pub fn process_excision_requests(
        &mut self,
        database_id: &str,
    ) -> Result<ExcisionReceipt, SemanticError> {
        self.process_excision_requests_with_fault(database_id, ExcisionFault::None)
    }

    #[doc(hidden)]
    pub fn process_excision_requests_with_fault(
        &mut self,
        database_id: &str,
        fault_point: ExcisionFault,
    ) -> Result<ExcisionReceipt, SemanticError> {
        crate::transactor::excision_operator::process_requests(
            &self.connection,
            database_id,
            fault_point,
            &self.maintenance,
            &self.fulltext_build_limits,
        )
    }

    /// True only when every A=15 request at or before `through_t` is reflected
    /// by the active generation and its root-last completion marker.
    pub fn sync_excise(
        &mut self,
        database_id: &str,
        through_t: u64,
    ) -> Result<bool, SemanticError> {
        crate::storage::excision::sync_requests(
            &self.connection,
            database_id,
            through_t,
            &self.maintenance,
        )
    }
}
