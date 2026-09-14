//! Exact immutable handle, representation ownership and resident metadata.
use super::cursor::DatabaseValuePrefixCursor;
use super::overlay::TransactionOverlay;
use super::read_context::{LogicalReadObserver, ReadValueIdentity, TransactionReadContext};
use crate::collections::persistent_map::SharedMap;
use crate::index::compare_prefix;
use crate::{
    Database, Datom, ErrorCategory, IndexOrder, IndexPrefix, Keyword, Schema, SemanticError,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub(super) type ReadFilter = dyn Fn(&DatabaseValue, &Datom) -> bool + Send + Sync;

/// One exact immutable database value used by read-side APIs.
///
/// Its basis may be an eager memory value, an immutable block snapshot, or a
/// speculative delta. Each representation supports the same selective reads
/// and transaction assessor.
/// Temporal and custom filters belong to this value, so every consumer sees
/// the same information instead of receiving a detached `View` hint.
#[derive(Clone)]
pub struct DatabaseValue {
    pub(super) basis: ReadBasis,
    pub(crate) entity_origin: crate::entity_identity::DatabaseOrigin,
    pub(super) read_identity: Arc<ReadValueIdentity>,
    pub(super) as_of_t: Option<u64>,
    pub(super) since_t: Option<u64>,
    pub(super) history: bool,
    pub(super) filters: Arc<[Arc<ReadFilter>]>,
    pub(super) read_observer: Option<Arc<LogicalReadObserver>>,
    pub(super) read_context: Option<Arc<TransactionReadContext>>,
    // A speculative binding has no durable generation reference. Keep its
    // authenticated dependency closure alive independently of cache eviction
    // and program garbage collection. Branches share immutable lookup paths.
    pub(super) programs: SharedMap<crate::ProgramHash, Arc<crate::program::ValidatedProgram>>,
}

#[derive(Clone)]
pub(super) enum ReadBasis {
    Eager(Arc<Database>),
    // Values and lazy entities clone a shared immutable handle, not the
    // snapshot's reader/cache configuration on every navigation step.
    Block(Arc<crate::storage::BlockSnapshot>),
    TransactionOverlay(Arc<TransactionOverlay>),
}

/// The result of a pure transaction. These values never advance a connection
/// or create a durable transaction receipt.
#[derive(Clone, Debug)]
pub struct SpeculativeTransactionReport {
    pub db_before: DatabaseValue,
    pub db_after: DatabaseValue,
    pub tx_data: Vec<Datom>,
    pub tempids: BTreeMap<String, u64>,
}

impl fmt::Debug for DatabaseValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DatabaseValue")
            .field(
                "basis",
                &match &self.basis {
                    ReadBasis::Eager(_) => "eager",
                    ReadBasis::Block(snapshot) => {
                        if snapshot.is_repository() {
                            "backup"
                        } else {
                            "block"
                        }
                    }
                    ReadBasis::TransactionOverlay(_) => "transaction-overlay",
                },
            )
            .field("basis_t", &self.basis_t())
            .field("as_of_t", &self.as_of_t)
            .field("since_t", &self.since_t)
            .field("history", &self.history)
            .field("filter_count", &self.filters.len())
            .finish()
    }
}

impl DatabaseValue {
    pub(crate) fn resolve_program(
        &self,
        hash: crate::ProgramHash,
    ) -> Result<Arc<crate::program::ValidatedProgram>, SemanticError> {
        if let Some(program) = self.programs.get(&hash) {
            return Ok(Arc::clone(program));
        }
        match &self.basis {
            ReadBasis::Block(snapshot) => snapshot.resolve_program(hash),
            ReadBasis::TransactionOverlay(overlay) => overlay.base.resolve_program(hash),
            ReadBasis::Eager(_) => Err(SemanticError::new(
                ErrorCategory::NotFound,
                "postgres/program-not-found",
                "program content is not available in this database value",
            )),
        }
    }

    pub(crate) fn committed_block_parts(
        &self,
    ) -> Result<
        (
            crate::storage::BlockSnapshot,
            Option<u64>,
            Option<u64>,
            bool,
        ),
        SemanticError,
    > {
        if !self.filters.is_empty() {
            return Err(SemanticError::new(
                ErrorCategory::Unsupported,
                "database/opaque-filter-snapshot",
                "Opaque filtered values have no portable snapshot reference",
            ));
        }
        match &self.basis {
            ReadBasis::Block(snapshot)
                if snapshot.route_id().is_some() && !snapshot.is_repository() =>
            {
                Ok((
                    snapshot.as_ref().clone(),
                    self.as_of_t,
                    self.since_t,
                    self.history,
                ))
            }
            _ => Err(SemanticError::new(
                ErrorCategory::Unsupported,
                "database/uncommitted-snapshot",
                "Only committed block values have portable references",
            )),
        }
    }

    pub(crate) fn retain_programs(
        mut self,
        programs: crate::program_bindings::ResolvedPrograms,
    ) -> Self {
        for (hash, program) in programs {
            self.programs.insert(hash, program);
        }
        self
    }

    pub fn eager(database: Arc<Database>) -> Self {
        Self {
            entity_origin: database.entity_origin.clone(),
            basis: ReadBasis::Eager(database),
            read_identity: Arc::new(ReadValueIdentity),
            as_of_t: None,
            since_t: None,
            history: false,
            filters: Arc::default(),
            read_observer: None,
            read_context: None,
            programs: SharedMap::default(),
        }
    }

    /// An immutable value over the first-release opaque block store. This uses
    /// the same query/window/speculation algorithms as other database values.
    pub fn block(snapshot: crate::storage::BlockSnapshot) -> Self {
        Self {
            entity_origin: crate::entity_identity::DatabaseOrigin::durable(snapshot.lineage_id()),
            basis: ReadBasis::Block(Arc::new(snapshot)),
            read_identity: Arc::new(ReadValueIdentity),
            as_of_t: None,
            since_t: None,
            history: false,
            filters: Arc::default(),
            read_observer: None,
            read_context: None,
            programs: SharedMap::default(),
        }
    }

    pub(crate) fn block_snapshot(&self) -> Option<crate::storage::BlockSnapshot> {
        match &self.basis {
            ReadBasis::Block(snapshot) => Some(snapshot.as_ref().clone()),
            ReadBasis::TransactionOverlay(overlay) => overlay.base.block_snapshot(),
            _ => None,
        }
    }

    pub(crate) fn block_log_snapshot(&self) -> Option<crate::storage::BlockSnapshot> {
        match &self.basis {
            ReadBasis::Block(snapshot) if self.filters.is_empty() => {
                Some(snapshot.as_ref().clone())
            }
            _ => None,
        }
    }

    pub(crate) fn fulltext_history_cursor(
        &self,
        attribute: u32,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        self.basis_prefix_cursor(
            true,
            &IndexPrefix::Aevt {
                attribute,
                entity: None,
                value: None,
            },
            None,
            None,
        )
    }

    /// Assertion candidates not covered by the captured search attachment.
    /// The authenticated recent tier starts after the exact canonical index
    /// basis to which that attachment is bound; speculative indexes are the
    /// disjoint continuation after the committed value. Attribute-prefix seeks
    /// avoid scanning unrelated facts or materializing the durable database.
    pub(crate) fn fulltext_unindexed_cursor(
        &self,
        attribute: u32,
    ) -> Result<impl Iterator<Item = Datom>, SemanticError> {
        let prefix = IndexPrefix::Aevt {
            attribute,
            entity: None,
            value: None,
        };
        let recent = self
            .block_snapshot()
            .map(|snapshot| snapshot.recent_tier().prefix_cursor(true, &prefix))
            .transpose()?;
        let local = match &self.basis {
            ReadBasis::TransactionOverlay(overlay) => Some(overlay.indexes.cursor(
                true,
                IndexOrder::Aevt,
                |datom| compare_prefix(datom, &prefix),
                false,
                Some(prefix.clone()),
            )),
            _ => None,
        };
        Ok(recent
            .into_iter()
            .flatten()
            .chain(local.into_iter().flatten().map(|datom| (*datom).clone())))
    }

    /// The basis remains the basis of the underlying immutable value even
    /// when an as-of or since window exposes less information.
    pub fn basis_t(&self) -> u64 {
        match &self.basis {
            ReadBasis::Eager(database) => database.basis_t(),
            ReadBasis::Block(snapshot) => snapshot.basis_t(),
            ReadBasis::TransactionOverlay(overlay) => overlay.basis_t,
        }
    }

    /// Exclusive entity-index issuance frontier at this immutable basis.
    pub fn eidx_frontier(&self) -> u64 {
        match &self.basis {
            ReadBasis::Eager(database) => database.eidx_frontier(),
            ReadBasis::Block(snapshot) => snapshot.eidx_frontier(),
            ReadBasis::TransactionOverlay(overlay) => overlay.eidx_frontier,
        }
    }

    pub(crate) fn reserved_allocation(
        &self,
    ) -> Result<Option<crate::reserved_allocation::ReservedAllocation>, SemanticError> {
        match &self.basis {
            ReadBasis::Eager(database) => Ok(database.reserved_allocation()),
            ReadBasis::Block(snapshot) => Ok(snapshot.reserved_allocation()),
            ReadBasis::TransactionOverlay(overlay) => Ok(overlay.reserved_allocation),
        }
    }

    /// The most recent transaction instant at this immutable basis.
    ///
    /// Every representation already retains this coordinate: eager state,
    /// authenticated block metadata, or the speculative successor. Views and
    /// clones preserve that basis, so reading it needs no scan or mutable memo.
    pub fn last_tx_instant(&self) -> Option<i64> {
        match &self.basis {
            ReadBasis::Eager(database) => database.last_tx_instant(),
            ReadBasis::Block(snapshot) => snapshot.last_tx_instant(),
            ReadBasis::TransactionOverlay(overlay) => Some(overlay.last_tx_instant),
        }
    }

    pub fn schema(&self) -> &Schema {
        match &self.basis {
            ReadBasis::Eager(database) => database.schema(),
            ReadBasis::Block(snapshot) => snapshot.schema(),
            ReadBasis::TransactionOverlay(overlay) => &overlay.schema,
        }
    }

    /// Share the immutable schema projection carried by this database value.
    /// This is the ownership counterpart of [`Self::schema`]: assessment can
    /// retain an unchanged projection without a whole-schema clone.
    pub(crate) fn schema_arc(&self) -> Arc<Schema> {
        match &self.basis {
            ReadBasis::Eager(database) => database.schema_arc(),
            ReadBasis::Block(snapshot) => snapshot.schema_arc(),
            ReadBasis::TransactionOverlay(overlay) => Arc::clone(&overlay.schema),
        }
    }

    /// Resolve an ident from the immutable basis' derived ident cache.
    /// Datomic's temporal predicates window index data, not this dictionary.
    pub fn entid(&self, ident: &Keyword) -> Option<u64> {
        match &self.basis {
            ReadBasis::Eager(database) => database.entid(ident),
            ReadBasis::Block(snapshot) => snapshot.entid(ident),
            ReadBasis::TransactionOverlay(overlay) => overlay
                .indexes
                .entids
                .get(ident)
                .copied()
                .or_else(|| overlay.base.entid(ident)),
        }
    }

    pub fn ident(&self, entity: u64) -> Option<&Keyword> {
        match &self.basis {
            ReadBasis::Eager(database) => database.ident(entity),
            ReadBasis::Block(snapshot) => snapshot.ident(entity),
            ReadBasis::TransactionOverlay(overlay) => overlay
                .indexes
                .idents
                .get(&entity)
                .or_else(|| overlay.base.ident(entity)),
        }
    }

    /// Coordinate retained by a queued native transaction report. This is
    /// observability for queued root/generation retention, not a read capability.
    pub(crate) fn native_retention_coordinate(&self) -> Option<(u64, Option<crate::Digest>)> {
        match &self.basis {
            ReadBasis::Eager(_) => None,
            ReadBasis::Block(snapshot) => (!snapshot.is_repository())
                .then_some((snapshot.generation(), snapshot.captured_root().indexes)),
            ReadBasis::TransactionOverlay(overlay) => overlay.base.native_retention_coordinate(),
        }
    }
}

impl From<Database> for DatabaseValue {
    fn from(database: Database) -> Self {
        Self::eager(Arc::new(database))
    }
}

impl From<&Database> for DatabaseValue {
    fn from(database: &Database) -> Self {
        Self::from(database.clone())
    }
}

impl From<Arc<Database>> for DatabaseValue {
    fn from(database: Arc<Database>) -> Self {
        Self::eager(database)
    }
}
