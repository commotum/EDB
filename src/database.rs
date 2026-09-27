use crate::index::eager::IndexRoots;
use crate::model::identity::{validate_frontier, validate_supported_eid};
use crate::model::idents::IdentIndex;
use crate::model::vocabulary::{
    DB_EXCISE, DB_EXCISE_BEFORE, DB_EXCISE_BEFORE_T, DB_IDENT, DB_TX_INSTANT,
    canonical_genesis_datoms, supported_system_attributes, supported_system_idents,
};
use crate::reserved_allocation::ReservedAllocation;
use crate::state_commitment::{CommitmentWork, SemanticStateCommitment};
use crate::{
    Cardinality, Datom, ErrorCategory, INITIAL_EIDX_FRONTIER, IndexOrder, IndexPrefix, Schema,
    SemanticError, TX_PARTITION, TxOp, Value, ValueType, eid_to_eidx, eid_to_part, t_to_tx,
    tx_to_t,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchemaChange {
    Install(crate::Attribute),
    Alter(crate::Attribute),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Current,
    AsOf(u64),
    Since(u64),
    History,
}

#[derive(Clone, Debug)]
pub struct TxReport {
    pub db_before: Database,
    pub db_after: Database,
    pub tx_data: Vec<Datom>,
    pub tempids: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CurrentFact {
    entity: u64,
    attribute: u32,
    value: Value,
    tx: u64,
}

#[derive(Clone, Debug)]
struct LogicalDatom {
    entity: u64,
    attribute: u32,
    value: Value,
    added: bool,
}

/// The time boundary exactly as it existed when `:db/excise` was asserted.
/// The effective predicate is additionally capped by `request_t`, so facts
/// added after the request can never be removed by delayed background work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExcisionCutoff {
    BeforeT(u64),
    BeforeInstant(i64),
}

/// One immutable excision predicate reconstructed from ordinary history.
///
/// Excision requests are identified by the transaction that asserted A=15.
/// The request entity is read as-of that transaction. This identity prevents
/// later edits to the request entity from changing work already requested.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenExcisionRequest {
    pub(crate) request_entity: u64,
    pub(crate) request_t: u64,
    pub(crate) target: u64,
    pub(crate) attributes: BTreeSet<u64>,
    pub(crate) cutoff: Option<ExcisionCutoff>,
}

/// Immutable, single-process database value.
///
/// Transactions use the same selective assessor as every `DatabaseValue`.
/// This type owns the eager memory representation; history is chunked by
/// transaction so successor values structurally share all prior chunks.
#[derive(Clone, Debug)]
pub struct Database {
    // Reference identity is deliberately outside durable semantic commitments.
    pub(crate) entity_origin: crate::entity_identity::DatabaseOrigin,
    schema: Arc<Schema>,
    idents: Arc<IdentIndex>,
    basis_t: u64,
    /// Exclusive global entity-index issuance frontier. Entity IDs themselves
    /// retain their partition bits; this field deliberately stores only the
    /// recovered low 42-bit `eidx` boundary.
    eidx_frontier: u64,
    reserved_allocation: Option<ReservedAllocation>,
    last_tx_instant: Option<i64>,
    tx_instant_attribute: Option<u32>,
    current: Arc<[CurrentFact]>,
    /// Canonical immutable t=0 information. This is indexed and queryable but
    /// is not one of the positive user transaction chunks counted by basis_t.
    genesis: Arc<[Datom]>,
    history: Arc<[Arc<[Datom]>]>,
    current_indexes: IndexRoots,
    history_indexes: IndexRoots,
    /// Packing-independent semantic roots carried with the immutable value.
    /// Successors update these roots from their material transaction delta;
    /// recovery/index constructors rebuild them once from authoritative facts.
    semantic_state: SemanticStateCommitment,
    /// Diagnostic work for the transition that produced this value. This is
    /// deliberately outside the semantic digest.
    semantic_commitment_work: CommitmentWork,
}

impl Database {
    /// Wrap this eager kernel snapshot as the exact immutable value consumed
    /// by read-side APIs. Cloning `Database` preserves its shared immutable
    /// index roots.
    pub fn database_value(&self) -> crate::DatabaseValue {
        crate::DatabaseValue::from(self)
    }

    /// Construct the exact native t=0 database. Application schema is
    /// installed by ordinary positive transactions, never appended to
    /// genesis.
    pub fn bootstrap() -> Result<Self, SemanticError> {
        Self::from_genesis(canonical_genesis_datoms())
    }

    /// Convenience constructor that bootstraps the database and installs the
    /// supplied application attributes in one ordinary transaction. The
    /// resulting value is therefore at basis 1 when `schema` is non-empty.
    /// New code that needs to observe creation separately should use
    /// [`Database::bootstrap`] and transact the schema explicitly.
    pub fn new(schema: Schema) -> Result<Self, SemanticError> {
        schema.validate_tuple_definitions()?;
        for attribute in schema.attributes() {
            if supported_system_idents()
                .iter()
                .any(|(entity, _)| *entity == u64::from(attribute.id))
            {
                return Err(SemanticError::incorrect(
                    "schema/reserved-system-entity",
                    format!(
                        "entity {} is reserved by the native system vocabulary",
                        attribute.id
                    ),
                ));
            }
        }
        if schema.ident_aliases().next().is_some() {
            return Err(SemanticError::incorrect(
                "schema/bootstrap-alias",
                "initial schema aliases must be introduced by an ordinary rename transaction",
            ));
        }
        let database = Self::bootstrap()?;
        let operations: Vec<_> = schema
            .attributes()
            .cloned()
            .map(TxOp::InstallAttribute)
            .collect();
        if operations.is_empty() {
            Ok(database)
        } else {
            Ok(database.with(&operations, 0)?.db_after)
        }
    }

    pub(crate) fn from_genesis(genesis: Vec<Datom>) -> Result<Self, SemanticError> {
        validate_genesis_information(&genesis)?;
        let current = replay(genesis.iter());
        let current_datoms = facts_as_datoms(&current);
        let idents = IdentIndex::derive(genesis.iter(), DB_IDENT as u32)?;
        let derived_schema = Schema::derive_from_information(&current_datoms, &idents)?;
        let tx_instant_attribute = DB_TX_INSTANT as u32;
        let tx_instant = derived_schema.attribute(tx_instant_attribute)?;
        if tx_instant.value_type != ValueType::Instant || tx_instant.cardinality != Cardinality::One
        {
            return Err(SemanticError::incorrect(
                "schema/invalid-tx-instant-attribute",
                ":db/txInstant must be a cardinality-one instant",
            ));
        }
        let highest_attribute = derived_schema
            .attributes()
            .map(|attribute| u64::from(attribute.id))
            .max()
            .unwrap_or(0);
        let eidx_frontier = INITIAL_EIDX_FRONTIER.max(highest_attribute.saturating_add(1));
        validate_frontier(eidx_frontier)?;
        let current_indexes = IndexRoots::build(&derived_schema, current_datoms);
        let history_indexes = IndexRoots::build(&derived_schema, genesis.iter().cloned());
        let mut reserved = ReservedAllocation::initial();
        reserved.observe_datoms(genesis.iter())?;
        let mut database = Self {
            entity_origin: crate::entity_identity::DatabaseOrigin::memory(),
            schema: Arc::new(derived_schema),
            idents: Arc::new(idents),
            basis_t: 0,
            eidx_frontier,
            reserved_allocation: Some(reserved),
            last_tx_instant: None,
            tx_instant_attribute: Some(tx_instant_attribute),
            current: current.into(),
            genesis: genesis.into(),
            history: Arc::default(),
            current_indexes,
            history_indexes,
            semantic_state: SemanticStateCommitment::default(),
            semantic_commitment_work: CommitmentWork::default(),
        };
        database.semantic_state = SemanticStateCommitment::recompute(&database)?;
        database.validate_invariants()?;
        Ok(database)
    }

    pub fn with_tx_instant_attribute(mut self, attribute: u32) -> Result<Self, SemanticError> {
        let schema = self.schema.attribute(attribute)?;
        if schema.value_type != ValueType::Instant || schema.cardinality != Cardinality::One {
            return Err(SemanticError::incorrect(
                "schema/invalid-tx-instant-attribute",
                "transaction instant attribute must be cardinality-one instant",
            ));
        }
        self.tx_instant_attribute = Some(attribute);
        Ok(self)
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Share the immutable resident schema projection without rebuilding it.
    /// Transaction assessment uses this at the unchanged-schema fast path;
    /// the eager kernel already stores the projection behind an `Arc`.
    pub(crate) fn schema_arc(&self) -> Arc<Schema> {
        Arc::clone(&self.schema)
    }

    /// Resolve any entity ident, including enum values and historical aliases.
    pub fn entid(&self, ident: &crate::Keyword) -> Option<u64> {
        self.idents.resolve(ident)
    }

    /// Return the most recently asserted ident for an entity.
    pub fn ident(&self, entity: u64) -> Option<&crate::Keyword> {
        self.idents.ident(entity)
    }

    /// Drop and deterministically rebuild every schema-dependent cache from
    /// immutable information. This has no semantic effect and exists as both
    /// a recovery primitive and an executable proof that the caches are not a
    /// second authority.
    pub fn rebuild_derived_caches(&self) -> Result<Self, SemanticError> {
        let mut rebuilt = self.clone();
        let mut replayed = replay(rebuilt.history_datoms());
        replayed.sort_by(compare_current);
        let current = facts_as_datoms(&replayed);
        let idents = IdentIndex::derive(rebuilt.history_datoms(), DB_IDENT as u32)?;
        let schema = Arc::new(Schema::derive_from_information(&current, &idents)?);
        rebuilt.idents = Arc::new(idents);
        rebuilt.schema = schema;
        rebuilt.current = replayed.into();
        rebuilt.current_indexes = IndexRoots::build(&rebuilt.schema, current);
        rebuilt.history_indexes =
            IndexRoots::build(&rebuilt.schema, rebuilt.history_datoms().cloned());
        rebuilt.validate_invariants()?;
        Ok(rebuilt)
    }

    pub fn basis_t(&self) -> u64 {
        self.basis_t
    }

    /// Transaction time at the current basis. The pure kernel only checks the
    /// lower bound; the authoritative transactor owns the server-clock upper
    /// bound for an explicit override.
    pub(crate) fn last_tx_instant(&self) -> Option<i64> {
        self.last_tx_instant
    }

    /// Exclusive non-negative entity-index issuance frontier.
    pub fn eidx_frontier(&self) -> u64 {
        self.eidx_frontier
    }

    pub(crate) fn reserved_allocation(&self) -> Option<ReservedAllocation> {
        self.reserved_allocation
    }

    /// Recovered `as-of-t` resolution for a millisecond instant.
    ///
    /// The AVET lower-bound seek returns the first transaction at or after the
    /// instant.  An exact match is used directly; otherwise its predecessor T
    /// is used.  Consequently, when several transactions share the exact same
    /// millisecond, the earliest of them wins.  This is the documented
    /// imprecision of instant time points; T/Tx remain exact.  An instant after
    /// the database resolves to the next T, which is observationally current.
    pub fn t_at_or_before_instant(&self, instant: i64) -> u64 {
        let Some(attribute) = self.tx_instant_attribute else {
            return 0;
        };
        let candidate = self
            .current_indexes
            .seek(&IndexPrefix::Avet {
                attribute,
                value: Some(Value::Instant(instant)),
                entity: None,
            })
            .expect("internally constructed index prefix is valid")
            .iter()
            .next()
            .filter(|datom| datom.attribute == attribute);
        match candidate {
            Some(datom) => {
                let Value::Instant(candidate_instant) = &datom.value else {
                    unreachable!("validated :db/txInstant index contains only instants")
                };
                let candidate_t =
                    tx_to_t(datom.tx).expect("validated :db/txInstant datom has a transaction id");
                if *candidate_instant == instant {
                    candidate_t
                } else {
                    candidate_t
                        .checked_sub(1)
                        .expect("positive transaction T has a predecessor boundary")
                }
            }
            None => self.basis_t.saturating_add(1),
        }
    }

    /// Least transaction t whose transaction instant is at or after the
    /// supplied instant, or the next t when the instant is beyond this DB.
    pub fn t_at_or_after_instant(&self, instant: i64) -> u64 {
        let Some(attribute) = self.tx_instant_attribute else {
            return self.basis_t + 1;
        };
        self.current_indexes
            .seek(&IndexPrefix::Avet {
                attribute,
                value: Some(Value::Instant(instant)),
                entity: None,
            })
            .expect("internally constructed index prefix is valid")
            .iter()
            .next()
            .filter(|datom| datom.attribute == attribute)
            .map(|datom| {
                tx_to_t(datom.tx).expect("validated :db/txInstant datom has a transaction id")
            })
            .unwrap_or(self.basis_t + 1)
    }

    pub fn as_of_instant(&self, instant: i64) -> crate::DatabaseValue {
        self.view(View::AsOf(self.t_at_or_before_instant(instant)))
    }

    /// Derive the same owned immutable value used by query, pull, and entity
    /// APIs. Keeping one implementation avoids the old borrowed view's
    /// divergent filter context and whole-index rebuild semantics.
    pub fn view(&self, view: View) -> crate::DatabaseValue {
        let value = self.database_value();
        match view {
            View::Current => value,
            View::AsOf(t) => value.as_of(t),
            View::Since(t) => value.since(t),
            View::History => value.history(),
        }
    }

    pub fn datoms(&self, view: View, order: IndexOrder) -> Vec<Datom> {
        match view {
            View::Current => self.current_indexes.get(order).to_vec(),
            View::History => self.history_indexes.get(order).to_vec(),
            View::AsOf(t) => {
                let facts = replay(
                    self.history_datoms()
                        .filter(|d| tx_to_t(d.tx).is_ok_and(|datom_t| datom_t <= t)),
                );
                IndexRoots::build(&self.schema, facts_as_datoms(&facts))
                    .get(order)
                    .to_vec()
            }
            View::Since(t) => {
                let facts = replay(
                    self.history_datoms()
                        .filter(|d| tx_to_t(d.tx).is_ok_and(|datom_t| datom_t > t)),
                );
                IndexRoots::build(&self.schema, facts_as_datoms(&facts))
                    .get(order)
                    .to_vec()
            }
        }
    }

    /// Borrow one already-resident current or history index without copying
    /// it. `DatabaseValue` uses this memory representation to give its
    /// public fallible cursor the same construction-time laziness as a native
    /// tree cursor.
    pub(crate) fn index_datoms(&self, history: bool, order: IndexOrder) -> &[Datom] {
        if history {
            self.history_indexes.get(order)
        } else {
            self.current_indexes.get(order)
        }
    }

    pub fn values(&self, entity: u64, attribute: u32) -> Vec<&Value> {
        self.current_indexes
            .matching(&IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: None,
            })
            .expect("internally constructed index prefix is valid")
            .iter()
            .map(|datom| &datom.value)
            .collect()
    }

    /// Exact current-database matches for a left-contiguous index prefix.
    pub fn datoms_with_prefix(&self, prefix: &IndexPrefix) -> Result<&[Datom], SemanticError> {
        self.current_indexes.matching(prefix)
    }

    /// Current-database datoms at or after a left-contiguous index prefix.
    pub fn seek_datoms(&self, prefix: &IndexPrefix) -> Result<&[Datom], SemanticError> {
        self.current_indexes.seek(prefix)
    }

    /// Current datoms at or before a left-contiguous prefix, in reverse index
    /// order. The owned result keeps the public API independent of a future
    /// segmented reverse cursor.
    pub fn reverse_seek_datoms(&self, prefix: &IndexPrefix) -> Result<Vec<Datom>, SemanticError> {
        self.current_indexes.reverse_seek(prefix)
    }

    /// Historical assertions and retractions matching an index prefix.
    pub fn history_with_prefix(&self, prefix: &IndexPrefix) -> Result<&[Datom], SemanticError> {
        self.history_indexes.matching(prefix)
    }

    /// Half-open AVET value range for one indexed attribute.
    pub fn avet_range(
        &self,
        attribute: u32,
        start: Option<&Value>,
        end: Option<&Value>,
    ) -> Result<&[Datom], SemanticError> {
        let schema = self.schema.attribute(attribute)?;
        if !(schema.indexed || schema.unique.is_some()) {
            return Err(SemanticError::incorrect(
                "index/attribute-not-in-avet",
                format!(
                    "attribute {} is not present in AVET",
                    schema.ident.qualified_name()
                ),
            ));
        }
        if start
            .zip(end)
            .is_some_and(|(start, end)| start.index_cmp(end).is_gt())
        {
            return Err(SemanticError::incorrect(
                "index/invalid-range",
                "AVET range start must not be greater than its end",
            ));
        }
        Ok(self.current_indexes.avet_range(attribute, start, end))
    }

    fn history_datoms(&self) -> impl Iterator<Item = &Datom> {
        self.genesis
            .iter()
            .chain(self.history.iter().flat_map(|chunk| chunk.iter()))
    }

    /// Cross-check all immutable roots and post-state invariants against the
    /// retained straightforward fact representation.
    pub fn validate_invariants(&self) -> Result<(), SemanticError> {
        validate_frontier(self.eidx_frontier)?;
        if let Some(reserved) = self.reserved_allocation {
            ReservedAllocation::from_frontier(reserved.frontier(), self.eidx_frontier)?;
            let mut observed = ReservedAllocation::initial();
            observed.observe_datoms(self.history_datoms())?;
            if observed.frontier() > reserved.frontier() {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/reserved-frontier-mismatch",
                    "reserved checkpoint does not cover retained partition-zero information",
                ));
            }
        }
        validate_genesis_information(&self.genesis)?;
        self.schema.validate_tuple_definitions()?;
        t_to_tx(self.basis_t).map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "kernel/basis-out-of-range",
                format!("database basis cannot be represented as a transaction id: {error}"),
            )
        })?;
        let current = facts_as_datoms(&self.current);
        let derived_idents = IdentIndex::derive(self.history_datoms(), DB_IDENT as u32)?;
        let derived_schema = Schema::derive_from_information(&current, &derived_idents)?;
        if derived_idents.cmp_observation(&self.idents).is_ne() || derived_schema != *self.schema {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "kernel/derived-schema-divergence",
                "schema or ident cache diverged from ordinary database information",
            ));
        }
        let expected_semantic_state = SemanticStateCommitment::from_current(&current)?;
        if expected_semantic_state.metadata() != self.semantic_state.metadata() {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "kernel/semantic-commitment-divergence",
                "incremental semantic commitment diverged from current datoms",
            ));
        }
        let expected_current = IndexRoots::build(&self.schema, current);
        let history: Vec<_> = self.history_datoms().cloned().collect();
        let expected_history = IndexRoots::build(&self.schema, history);
        for order in [
            IndexOrder::Eavt,
            IndexOrder::Aevt,
            IndexOrder::Avet,
            IndexOrder::Vaet,
        ] {
            if !same_stored_datoms(self.current_indexes.get(order), expected_current.get(order)) {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/current-index-divergence",
                    format!("current {order:?} root diverged from facts"),
                ));
            }
            if !same_stored_datoms(self.history_indexes.get(order), expected_history.get(order)) {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/history-index-divergence",
                    format!("history {order:?} root diverged from transaction chunks"),
                ));
            }
        }
        validate_cardinality(&self.schema, &self.current)?;
        validate_uniqueness(&self.schema, &self.current)?;
        validate_excision_requests(&self.current)?;
        for fact in self.current.iter() {
            let attribute = self.schema.attribute(fact.attribute)?;
            self.schema.validate_value(attribute, &fact.value)?;
        }
        let mut replayed = replay(self.history_datoms());
        replayed.sort_by(compare_current);
        if !same_stored_current(&replayed, &self.current) {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "kernel/history-current-divergence",
                "replaying transaction history does not reproduce current facts",
            ));
        }
        if self.history.len() != self.basis_t as usize {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "kernel/history-basis-divergence",
                "transaction history chunk count does not match database basis",
            ));
        }
        if let Some(tx_instant_attribute) = self.tx_instant_attribute {
            for (offset, chunk) in self.history.iter().enumerate() {
                let t = offset as u64 + 1;
                let tx = t_to_tx(t).map_err(|error| {
                    SemanticError::new(
                        ErrorCategory::Fault,
                        "kernel/invalid-history-transaction",
                        format!("history transaction {t} cannot be represented: {error}"),
                    )
                })?;
                if chunk.iter().any(|datom| datom.tx != tx)
                    || chunk
                        .iter()
                        .filter(|datom| {
                            datom.entity == tx
                                && datom.attribute == tx_instant_attribute
                                && datom.added
                                && matches!(datom.value, Value::Instant(_))
                        })
                        .count()
                        != 1
                {
                    return Err(SemanticError::new(
                        ErrorCategory::Fault,
                        "kernel/invalid-transaction-chunk",
                        format!("history chunk for transaction t={t}, tx={tx} is malformed"),
                    ));
                }
            }
        }
        for datom in self.genesis.iter() {
            validate_stored_entity(datom.entity, self.eidx_frontier, self.basis_t, &self.schema)?;
            validate_stored_value(&datom.value, self.eidx_frontier, self.basis_t, &self.schema)?;
        }
        for datom in self.history.iter().flat_map(|chunk| chunk.iter()) {
            let datom_t = tx_to_t(datom.tx).map_err(|error| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/invalid-datom-transaction",
                    format!("history datom has an invalid transaction entity id: {error}"),
                )
            })?;
            if datom_t == 0 || datom_t > self.basis_t {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/future-datom",
                    "history contains a datom outside the positive database basis",
                ));
            }
            validate_stored_entity(datom.entity, self.eidx_frontier, self.basis_t, &self.schema)?;
            validate_stored_value(&datom.value, self.eidx_frontier, self.basis_t, &self.schema)?;
        }
        Ok(())
    }

    fn validate_schema_change_against(
        &self,
        proposed: &crate::Attribute,
        facts: &[CurrentFact],
        allow_discontinued_ident_retarget: bool,
    ) -> Result<(), SemanticError> {
        let current = self.schema.attribute(proposed.id)?;
        self.schema.validate_attribute(proposed)?;
        if current.value_type != proposed.value_type {
            return Err(SemanticError::incorrect(
                "schema/value-type-immutable",
                "an installed attribute's value type cannot change",
            ));
        }
        if current.fulltext != proposed.fulltext {
            return Err(SemanticError::incorrect(
                "schema/fulltext-immutable",
                "an installed attribute's fulltext property cannot change",
            ));
        }
        if current.tuple != proposed.tuple && !allow_discontinued_ident_retarget {
            return Err(SemanticError::incorrect(
                "schema/tuple-definition-immutable",
                "an installed tuple definition cannot change",
            ));
        }
        if current.tuple_discontinued && !proposed.tuple_discontinued {
            return Err(SemanticError::incorrect(
                "schema/tuple-discontinuation-irreversible",
                "a discontinued composite tuple cannot be resumed",
            ));
        }
        if current.cardinality == Cardinality::Many && proposed.cardinality == Cardinality::One {
            let mut counts = BTreeMap::<u64, usize>::new();
            for fact in facts.iter().filter(|fact| fact.attribute == proposed.id) {
                *counts.entry(fact.entity).or_default() += 1;
            }
            if counts.values().any(|count| *count > 1) {
                return Err(SemanticError::conflict(
                    "schema/cardinality-change-conflict",
                    "current data contains multiple values for one entity",
                ));
            }
        }
        if current.unique.is_none() && proposed.unique.is_some() {
            // Logical `:db/index` is distinct from
            // physical AVET availability.  The eager
            // representation has no asynchronous physical tier, so an existing
            // logical AVET member is ready.  An unindexed attribute may gain
            // uniqueness only when it has never had a value: in that case
            // there is no historical range to backfill and AVET can become
            // available synchronously. This test checks history, not merely
            // current facts.
            let has_historical_values = self
                .history_datoms()
                .any(|datom| datom.attribute == proposed.id);
            if !current.indexed && has_historical_values {
                return Err(SemanticError::incorrect(
                    "schema/unique-requires-avet",
                    "adding uniqueness to an attribute with historical values requires a physically ready AVET index",
                ));
            }
            validate_uniqueness_for_attribute(facts, proposed.id)?;
        }
        Ok(())
    }

    fn schema_changes_to(
        &self,
        successor: &Schema,
        final_current: &[CurrentFact],
        logical: &[LogicalDatom],
    ) -> Result<Vec<SchemaChange>, SemanticError> {
        self.schema.validate_partition_successor(successor)?;
        // The one canonical native genesis is immutable.
        for required in self.genesis.iter() {
            if !contains_fact(
                final_current,
                required.entity,
                required.attribute,
                &required.value,
            ) {
                return Err(SemanticError::incorrect(
                    "schema/native-information-immutable",
                    "native genesis information cannot be retracted or replaced",
                ));
            }
        }
        for system in supported_system_attributes() {
            if self.schema.attribute(system.id).is_ok()
                && successor.attribute(system.id)? != &system
            {
                return Err(SemanticError::incorrect(
                    "schema/native-attribute-immutable",
                    format!(
                        "native attribute {} cannot be altered",
                        system.ident.qualified_name()
                    ),
                ));
            }
        }
        for current in self.schema.attributes() {
            if successor.attribute(current.id).is_err() {
                return Err(SemanticError::incorrect(
                    "schema/removed-attribute",
                    format!(
                        "committed information removed installed attribute {}",
                        current.id
                    ),
                ));
            }
        }
        let mut changes = Vec::new();
        let mut installed = Vec::new();
        for proposed in successor.attributes() {
            match self.schema.attribute(proposed.id) {
                Ok(current) if current == proposed => {}
                Ok(current) => {
                    let tuple_property_touched = logical.iter().any(|datom| {
                        datom.entity == u64::from(proposed.id)
                            && matches!(
                                u64::from(datom.attribute),
                                crate::DB_TUPLE_TYPE
                                    | crate::DB_TUPLE_TYPES
                                    | crate::DB_TUPLE_ATTRS
                            )
                    });
                    let allow_discontinued_ident_retarget = current.tuple_discontinued
                        && proposed.tuple_discontinued
                        && !tuple_property_touched;
                    self.validate_schema_change_against(
                        proposed,
                        final_current,
                        allow_discontinued_ident_retarget,
                    )?;
                    changes.push(SchemaChange::Alter(proposed.clone()));
                }
                Err(_) => {
                    let reserved = supported_system_idents()
                        .iter()
                        .any(|(entity, _)| *entity == u64::from(proposed.id));
                    if reserved {
                        return Err(SemanticError::incorrect(
                            "schema/reserved-system-entity",
                            format!(
                                "entity {} is reserved by the native vocabulary",
                                proposed.id
                            ),
                        ));
                    }
                    installed.push(proposed.id);
                    changes.push(SchemaChange::Install(proposed.clone()));
                }
            }
        }
        successor.validate_tuple_installations(&installed)?;
        Ok(changes)
    }

    pub fn lookup(&self, attribute: u32, value: &Value) -> Result<Option<u64>, SemanticError> {
        let schema = self.schema.attribute(attribute)?;
        if schema.unique.is_none() {
            return Err(SemanticError::incorrect(
                "transaction/lookup-non-unique",
                format!("attribute {} is not unique", schema.ident.qualified_name()),
            ));
        }
        Ok(self
            .current_indexes
            .matching(&IndexPrefix::Avet {
                attribute,
                value: Some(value.clone()),
                entity: None,
            })?
            .first()
            .map(|datom| datom.entity))
    }

    /// Pure transaction application. `tx_instant` is supplied by the caller so
    /// the same inputs always produce the same result.
    pub fn with(&self, ops: &[TxOp], tx_instant: i64) -> Result<TxReport, SemanticError> {
        self.with_defaults(ops, tx_instant, &crate::TransactionDefaults::default())
    }

    /// Pure application with explicit allocation defaults. Neither this value
    /// nor ambient process configuration is changed.
    pub fn with_defaults(
        &self,
        ops: &[TxOp],
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<TxReport, SemanticError> {
        let assessed = crate::transaction::pipeline::assess_operations(
            &self.database_value(),
            ops,
            tx_instant,
            defaults,
            None,
        )?;
        self.materialize_assessment(assessed)
    }

    /// Replay a current block-log entry whose ordinary and reserved high-water
    /// checkpoints were authenticated by its caller. Unlike receipt-based
    /// replay, this does not require caller-named tempids: allocations can leave
    /// no datoms, and excision can remove their last material witnesses. Bounds,
    /// fresh ordinary index uniqueness, and all non-allocation semantics remain
    /// checked. This is not authority to accept unauthenticated frontiers.
    pub(crate) fn apply_committed_with_allocation_frontiers(
        &self,
        transaction: &crate::DurableTransaction,
        prior: ReservedAllocation,
        after: ReservedAllocation,
        allow_excision: bool,
    ) -> Result<Self, SemanticError> {
        if !transaction.tempids.is_empty() {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/unexpected-tempid-map",
                "Block-log replay uses authenticated checkpoints, not a receipt tempid map",
            ));
        }
        self.apply_committed_with_allocation_proof(transaction, prior, after, allow_excision, true)
    }

    fn apply_committed_with_allocation_proof(
        &self,
        transaction: &crate::DurableTransaction,
        prior: ReservedAllocation,
        after: ReservedAllocation,
        allow_excision: bool,
        ordinary_checkpoint: bool,
    ) -> Result<Self, SemanticError> {
        ReservedAllocation::from_frontier(prior.frontier(), self.eidx_frontier)?;
        ReservedAllocation::from_frontier(after.frontier(), transaction.eidx_frontier)?;
        if self.reserved_allocation.is_some_and(|state| state != prior)
            || after.frontier() < prior.frontier()
        {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/reserved-frontier-mismatch",
                "reserved allocation checkpoint does not extend its authenticated predecessor",
            ));
        }
        let mut observed = prior;
        observed.observe_transaction(transaction)?;
        if observed.frontier() > after.frontier() {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/unwitnessed-reserved-allocation",
                "reserved checkpoint does not cover committed identities and references",
            ));
        }
        self.apply_committed_with_excision(
            transaction,
            allow_excision,
            Some(after),
            ordinary_checkpoint,
        )
    }

    fn apply_committed_with_excision(
        &self,
        transaction: &crate::DurableTransaction,
        allow_dangling_retractions: bool,
        reserved_after: Option<ReservedAllocation>,
        ordinary_checkpoint: bool,
    ) -> Result<Self, SemanticError> {
        let t = self.basis_t.checked_add(1).ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/basis-overflow",
                "database basis cannot advance beyond u64",
            )
        })?;
        let tx = t_to_tx(t).map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/basis-out-of-range",
                format!("database basis cannot be represented as a transaction id: {error}"),
            )
        })?;
        if transaction.basis_t != t {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/noncontiguous-basis",
                format!(
                    "expected transaction basis {t}, got {}",
                    transaction.basis_t
                ),
            ));
        }

        let tx_instant_attribute = self.tx_instant_attribute.ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/missing-tx-instant",
                "database has no transaction instant attribute",
            )
        })?;
        let instants: Vec<_> = transaction
            .tx_data
            .iter()
            .filter_map(|datom| {
                (datom.entity == tx && datom.attribute == tx_instant_attribute && datom.added)
                    .then_some(&datom.value)
            })
            .collect();
        let tx_instant = match instants.as_slice() {
            [Value::Instant(instant)] => *instant,
            _ => {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/invalid-tx-instant",
                    "committed transaction must contain exactly one own transaction instant",
                ));
            }
        };
        if self
            .last_tx_instant
            .is_some_and(|previous| tx_instant < previous)
        {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/non-monotonic-instant",
                "committed transaction instant precedes its predecessor",
            ));
        }

        let mut logical = Vec::with_capacity(transaction.tx_data.len());
        let mut seen = StoredFactIndex::default();
        for datom in &transaction.tx_data {
            if datom.tx != tx {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/datom-transaction-mismatch",
                    "committed datom does not name its enclosing transaction",
                ));
            }
            let attribute = self.schema.attribute(datom.attribute)?;
            self.schema.validate_value(attribute, &datom.value)?;
            if seen.contains(datom.entity, datom.attribute, &datom.value) {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/duplicate-datom",
                    "committed transaction contains duplicate or contradictory datoms",
                ));
            }
            seen.insert(datom.entity, datom.attribute, &datom.value, logical.len());
            logical.push(LogicalDatom {
                entity: datom.entity,
                attribute: datom.attribute,
                value: datom.value.clone(),
                added: datom.added,
            });
        }
        drop(seen);

        // Hook assertions are transaction events, not merely current facts.
        // Validate them before the materiality check so malformed explicit
        // hooks fail by the same rules as pure assessment. In particular, a
        // repeated :db.alter/attribute assertion is a legal (and necessary)
        // event even though the same E/A/V is already current.
        self.validate_committed_schema_hooks(&logical)
            .map_err(|error| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/invalid-schema-hooks",
                    format!("committed schema hooks are invalid: {error}"),
                )
            })?;
        let before_index = StoredFactIndex::from_current(&self.current);
        for datom in &transaction.tx_data {
            let existed = before_index.contains(datom.entity, datom.attribute, &datom.value);
            if existed == datom.added
                && !(allow_dangling_retractions && !datom.added && !existed)
                && !(datom.added && datom.attribute == crate::DB_ALTER_ATTRIBUTE as u32)
            {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/nonmaterial-datom",
                    "committed datom is not a material change from db-before",
                ));
            }
        }
        drop(before_index);

        let mut final_current = apply_logical(&self.current, &logical, tx);
        validate_excision_requests(&final_current)?;
        let successor_idents = IdentIndex::derive(
            self.history_datoms().chain(transaction.tx_data.iter()),
            DB_IDENT as u32,
        )?;
        let successor_current = facts_as_datoms(&final_current);
        let derived_schema = Arc::new(Schema::derive_from_information(
            &successor_current,
            &successor_idents,
        )?);
        let recovered_changes = self
            .schema_changes_to(&derived_schema, &final_current, &logical)
            .map_err(|error| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/invalid-schema-transition",
                    format!("committed schema transition is invalid: {error}"),
                )
            })?;
        let (expected_frontier, allocations) = if ordinary_checkpoint {
            let allocations = allocations_under_authenticated_frontier(
                self.eidx_frontier,
                transaction,
                &derived_schema,
            )?;
            (transaction.eidx_frontier, allocations)
        } else {
            expected_frontier_after_commit(
                self.eidx_frontier,
                t,
                &transaction.tempids,
                &recovered_changes,
                &derived_schema,
                reserved_after,
            )?
        };
        if transaction.eidx_frontier != expected_frontier {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/eidx-frontier-mismatch",
                format!(
                    "expected issued entity-index frontier {expected_frontier}, got {}",
                    transaction.eidx_frontier
                ),
            ));
        }
        validate_cardinality(&derived_schema, &final_current)?;
        validate_uniqueness(&derived_schema, &final_current)?;
        if derived_schema.as_ref() != self.schema.as_ref() {
            // Prior values were validated under the predecessor schema. A
            // schema transition must additionally validate all retained values
            // under its successor (including affected tuple definitions).
            for fact in &final_current {
                let attribute = derived_schema.attribute(fact.attribute)?;
                derived_schema.validate_value(attribute, &fact.value)?;
            }
        }
        // A validated predecessor already establishes these bounds for its
        // immutable genesis/history. Check every new stored datum here, even
        // a dangling excision retraction; this is input validation, not an
        // optional internal consistency audit. The enclosing tx identity and
        // positive contiguous basis were checked above.
        for datom in &transaction.tx_data {
            validate_stored_entity(datom.entity, transaction.eidx_frontier, t, &derived_schema)?;
            validate_stored_value(&datom.value, transaction.eidx_frontier, t, &derived_schema)?;
            validate_new_entity_allocation(
                datom.entity,
                self.eidx_frontier,
                &allocations,
                reserved_after,
            )?;
            validate_new_value_allocations(
                &datom.value,
                self.eidx_frontier,
                &allocations,
                reserved_after,
            )?;
        }
        final_current.sort_by(compare_current);

        let mut db_after = self.clone();
        db_after.schema = derived_schema;
        db_after.idents = Arc::new(successor_idents);
        db_after.basis_t = t;
        db_after.eidx_frontier = transaction.eidx_frontier;
        // Preserve the authenticated current reserved allocation checkpoint.
        db_after.reserved_allocation = reserved_after;
        db_after.last_tx_instant = Some(tx_instant);
        let current_datoms = facts_as_datoms(&final_current);
        db_after.current = final_current.into();
        let mut history_chunks: Vec<_> = self.history.iter().cloned().collect();
        history_chunks.push(Arc::from(transaction.tx_data.clone()));
        db_after.history = history_chunks.into();
        db_after.current_indexes = IndexRoots::build(&db_after.schema, current_datoms);
        db_after.history_indexes =
            IndexRoots::build(&db_after.schema, db_after.history_datoms().cloned());
        let (semantic_state, semantic_commitment_work) =
            self.semantic_state.advance(self, &transaction.tx_data)?;
        db_after.semantic_state = semantic_state;
        db_after.semantic_commitment_work = semantic_commitment_work;
        // The predecessor is an already validated native value, and every
        // newly admitted datum and semantic transition has been checked above.
        // Derived indexes/schema/idents and the incremental commitment are
        // trusted constructor outputs. Independently rebuilding all of them
        // and replaying all history after EACH prefix would turn an ordered
        // recovery into repeated full database audits. Explicit deep-recovery
        // boundaries call the unchanged public `validate_invariants` audit;
        // callers also retain their authoritative state/hash comparisons.
        Ok(db_after)
    }

    /// Store the already-assessed delta in the eager memory representation.
    /// Transaction resolution and constraints belong only to the shared assessor.
    pub(crate) fn materialize_assessment(
        &self,
        assessed: crate::transaction::assess::TieredAssessment,
    ) -> Result<TxReport, SemanticError> {
        let logical: Vec<_> = assessed
            .tx_data
            .iter()
            .map(|datom| LogicalDatom {
                entity: datom.entity,
                attribute: datom.attribute,
                value: datom.value.clone(),
                added: datom.added,
            })
            .collect();
        let mut current = apply_logical(&self.current, &logical, t_to_tx(assessed.basis_t)?);
        current.sort_by(compare_current);
        let mut after = self.clone();
        after.schema = assessed.successor_schema;
        after.idents = Arc::new(IdentIndex::derive(
            self.history_datoms().chain(assessed.tx_data.iter()),
            DB_IDENT as u32,
        )?);
        after.basis_t = assessed.basis_t;
        after.eidx_frontier = assessed.eidx_frontier;
        after.reserved_allocation = assessed.db_after.reserved_allocation()?;
        after.last_tx_instant = assessed.db_after.last_tx_instant();
        after.current_indexes = IndexRoots::build(&after.schema, facts_as_datoms(&current));
        after.current = current.into();
        let mut history: Vec<_> = self.history.iter().cloned().collect();
        history.push(Arc::from(assessed.tx_data.clone()));
        after.history = history.into();
        after.history_indexes = IndexRoots::build(&after.schema, after.history_datoms().cloned());
        let (state, work) = self.semantic_state.advance(self, &assessed.tx_data)?;
        after.semantic_state = state;
        after.semantic_commitment_work = work;
        Ok(TxReport {
            db_before: self.clone(),
            db_after: after,
            tx_data: assessed.tx_data,
            tempids: assessed.tempids,
        })
    }
    fn validate_schema_hook_events(
        &self,
        logical: &[LogicalDatom],
    ) -> Result<BTreeMap<u64, u32>, SemanticError> {
        let mut explicit = BTreeMap::<u64, u32>::new();
        for datom in logical.iter().filter(|datom| {
            datom.attribute == crate::DB_INSTALL_ATTRIBUTE as u32
                || datom.attribute == crate::DB_ALTER_ATTRIBUTE as u32
        }) {
            if !datom.added || datom.entity != crate::DB_PART_DB {
                return Err(SemanticError::incorrect(
                    "schema/invalid-hook-datom",
                    "attribute install/alter hooks must be assertions on :db.part/db",
                ));
            }
            let Value::Ref(target) = datom.value else {
                return Err(SemanticError::incorrect(
                    "schema/invalid-hook-target",
                    "attribute install/alter hook values must be entity references",
                ));
            };
            let attribute = crate::schema_eid_to_attr_id(target)?;
            let installed = self.schema.attribute(attribute).is_ok();
            let expected = if installed {
                crate::DB_ALTER_ATTRIBUTE as u32
            } else {
                crate::DB_INSTALL_ATTRIBUTE as u32
            };
            if datom.attribute != expected {
                return Err(SemanticError::incorrect(
                    "schema/wrong-hook-kind",
                    if installed {
                        "an installed attribute must use :db.alter/attribute"
                    } else {
                        "a new attribute must use :db.install/attribute"
                    },
                ));
            }
            if let Some(prior) = explicit.insert(target, datom.attribute)
                && prior != datom.attribute
            {
                return Err(SemanticError::conflict(
                    "schema/conflicting-hooks",
                    "one schema entity cannot be installed and altered together",
                ));
            }
        }

        Ok(explicit)
    }

    /// Hook discovery runs on the same materiality boundary as ordinary
    /// transaction data. Merely mentioning a controlled metadata attribute
    /// is insufficient: its E/A/V membership must differ from db-before.
    fn material_schema_hook_targets(
        &self,
        logical: &[LogicalDatom],
    ) -> Result<BTreeSet<u64>, SemanticError> {
        let mut touched = BTreeSet::new();
        for datom in logical
            .iter()
            .filter(|datom| is_attribute_hook_property(datom.attribute))
        {
            crate::schema_eid_to_attr_id(datom.entity)?;
            let existed = contains_fact(&self.current, datom.entity, datom.attribute, &datom.value);
            if existed != datom.added {
                touched.insert(datom.entity);
            }
        }
        Ok(touched)
    }

    /// Recovery cannot synthesize information that was absent from the
    /// committed transaction. Every material hook-controlled schema property
    /// therefore has to carry the install/alter event that pure assessment
    /// would have synthesized before publication.
    fn validate_committed_schema_hooks(
        &self,
        logical: &[LogicalDatom],
    ) -> Result<(), SemanticError> {
        let explicit = self.validate_schema_hook_events(logical)?;
        let touched = self.material_schema_hook_targets(logical)?;
        for target in &touched {
            if !explicit.contains_key(target) {
                return Err(SemanticError::incorrect(
                    "schema/missing-hook",
                    format!(
                        "schema entity {target} changes a hook-controlled property without its install/alter event"
                    ),
                ));
            }
        }
        for target in explicit.keys() {
            if !touched.contains(target) {
                return Err(SemanticError::incorrect(
                    "schema/orphan-hook",
                    format!(
                        "schema entity {target} has an install/alter event without a material hook-controlled property change"
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn is_attribute_hook_property(attribute: u32) -> bool {
    matches!(
        u64::from(attribute),
        crate::DB_VALUE_TYPE
            | crate::DB_CARDINALITY
            | crate::DB_UNIQUE
            | crate::DB_IS_COMPONENT
            | crate::DB_INDEX
            | crate::DB_NO_HISTORY
            | crate::DB_FULLTEXT
            | crate::DB_TUPLE_TYPE
            | crate::DB_TUPLE_TYPES
            | crate::DB_TUPLE_ATTRS
            | crate::DB_ATTR_PREDS
            | crate::DB_TUPLE_DISCONTINUED
    )
}

fn apply_logical(current: &[CurrentFact], datoms: &[LogicalDatom], tx: u64) -> Vec<CurrentFact> {
    let mut index = StoredFactIndex::from_current(current);
    let mut result: Vec<_> = current.iter().cloned().map(Some).collect();
    for datom in datoms.iter().filter(|datom| !datom.added) {
        for position in index.remove(datom.entity, datom.attribute, &datom.value) {
            result[position] = None;
        }
    }
    for datom in datoms.iter().filter(|datom| datom.added) {
        if let Some(position) = index.first(datom.entity, datom.attribute, &datom.value) {
            // Attribute-alter hooks are deliberately non-redundant events.
            // The current logical coordinate follows the newest such event,
            // while history retains every immutable hook assertion.
            if u64::from(datom.attribute) == crate::DB_ALTER_ATTRIBUTE {
                result[position]
                    .as_mut()
                    .expect("indexed fact is present")
                    .tx = tx;
            }
        } else {
            index.insert(datom.entity, datom.attribute, &datom.value, result.len());
            result.push(Some(CurrentFact {
                entity: datom.entity,
                attribute: datom.attribute,
                value: datom.value.clone(),
                tx,
            }));
        }
    }
    result.into_iter().flatten().collect()
}

fn validate_cardinality(schema: &Schema, facts: &[CurrentFact]) -> Result<(), SemanticError> {
    let mut seen = BTreeSet::new();
    let duplicate_coordinates: BTreeSet<_> = facts
        .iter()
        .filter_map(|fact| {
            let coordinate = (fact.entity, fact.attribute);
            (!seen.insert(coordinate)).then_some(coordinate)
        })
        .collect();
    // Retain the original traversal/error precedence, including unknown schema
    // attributes before or after the first conflicting coordinate.
    for left in facts {
        if schema.attribute(left.attribute)?.cardinality != Cardinality::One {
            continue;
        }
        if duplicate_coordinates.contains(&(left.entity, left.attribute)) {
            return Err(SemanticError::conflict(
                "transaction/cardinality-one-conflict",
                "resulting database has multiple cardinality-one values",
            ));
        }
    }
    Ok(())
}

fn validate_uniqueness(schema: &Schema, facts: &[CurrentFact]) -> Result<(), SemanticError> {
    let unique_attributes: BTreeSet<_> = schema
        .attributes()
        .filter(|attribute| attribute.unique.is_some())
        .map(|attribute| attribute.id)
        .collect();
    let mut holders = BTreeMap::new();
    for fact in facts
        .iter()
        .filter(|fact| unique_attributes.contains(&fact.attribute))
    {
        let (first, multiple) = holders
            .entry((fact.attribute, IndexValue(&fact.value)))
            .or_insert((fact.entity, false));
        *multiple |= *first != fact.entity;
    }
    for left in facts {
        let attribute = schema.attribute(left.attribute)?;
        if attribute.unique.is_none() {
            continue;
        }
        if left.value.is_nan() {
            return Err(SemanticError::incorrect(
                "transaction/nan-cannot-identify",
                "NaN cannot participate in uniqueness",
            ));
        }
        if holders[&(left.attribute, IndexValue(&left.value))].1 {
            return Err(SemanticError::conflict(
                "transaction/unique-conflict",
                "resulting database has multiple holders of a unique value",
            ));
        }
    }
    Ok(())
}

fn validate_excision_requests(facts: &[CurrentFact]) -> Result<(), SemanticError> {
    let request_entities: BTreeSet<_> = facts
        .iter()
        .filter(|fact| fact.attribute == DB_EXCISE as u32)
        .map(|fact| fact.entity)
        .collect();
    for entity in request_entities {
        let before_t = facts
            .iter()
            .find(|fact| fact.entity == entity && fact.attribute == DB_EXCISE_BEFORE_T as u32);
        let before = facts
            .iter()
            .find(|fact| fact.entity == entity && fact.attribute == DB_EXCISE_BEFORE as u32);
        if before_t.is_some() && before.is_some() {
            return Err(SemanticError::incorrect(
                "transaction/excision-cutoff-conflict",
                "an excision request may use at most one of :db.excise/beforeT and :db.excise/before",
            ));
        }
        if let Some(fact) = before_t {
            let Value::Long(value) = &fact.value else {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "kernel/excision-cutoff-type",
                    ":db.excise/beforeT does not contain a long",
                ));
            };
            normalize_excision_before_t(*value)?;
        }
    }
    Ok(())
}

pub(crate) fn normalize_excision_before_t(value: i64) -> Result<u64, SemanticError> {
    let encoded = u64::try_from(value).map_err(|_| {
        SemanticError::incorrect(
            "transaction/excision-before-t",
            ":db.excise/beforeT must be a non-negative t or transaction entity id",
        )
    })?;
    let partition = eid_to_part(encoded).map_err(|error| {
        SemanticError::incorrect(
            "transaction/excision-before-t",
            format!(":db.excise/beforeT is outside the entity-id range: {error}"),
        )
    })?;
    match partition {
        crate::DB_PARTITION => {
            if encoded > crate::MAX_EIDX {
                Err(SemanticError::incorrect(
                    "transaction/excision-before-t",
                    ":db.excise/beforeT exceeds the logical transaction range",
                ))
            } else {
                Ok(encoded)
            }
        }
        crate::TX_PARTITION => tx_to_t(encoded).map_err(|error| {
            SemanticError::incorrect(
                "transaction/excision-before-t",
                format!(":db.excise/beforeT contains an invalid transaction id: {error}"),
            )
        }),
        partition => Err(SemanticError::incorrect(
            "transaction/excision-before-t",
            format!(
                ":db.excise/beforeT entity id belongs to partition {partition}, not the transaction partition"
            ),
        )),
    }
}

fn validate_uniqueness_for_attribute(
    facts: &[CurrentFact],
    attribute: u32,
) -> Result<(), SemanticError> {
    let mut holders = BTreeMap::new();
    for left in facts {
        if left.attribute != attribute {
            continue;
        }
        if *holders
            .entry(IndexValue(&left.value))
            .or_insert(left.entity)
            != left.entity
        {
            return Err(SemanticError::conflict(
                "schema/unique-change-conflict",
                "current values must be unique before adding uniqueness",
            ));
        }
    }
    Ok(())
}

fn contains_fact(facts: &[CurrentFact], entity: u64, attribute: u32, value: &Value) -> bool {
    facts.iter().any(|fact| {
        fact.entity == entity && fact.attribute == attribute && fact.value.stored_eq(value)
    })
}

fn compare_current(left: &CurrentFact, right: &CurrentFact) -> Ordering {
    left.entity
        .cmp(&right.entity)
        .then(left.attribute.cmp(&right.attribute))
        .then_with(|| left.value.stored_cmp(&right.value))
}

fn same_stored_current(left: &[CurrentFact], right: &[CurrentFact]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.entity == right.entity
                && left.attribute == right.attribute
                && left.tx == right.tx
                && left.value.stored_eq(&right.value)
        })
}

fn same_stored_datoms(left: &[Datom], right: &[Datom]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.entity == right.entity
                && left.attribute == right.attribute
                && left.tx == right.tx
                && left.added == right.added
                && left.value.stored_eq(&right.value)
        })
}

fn validate_genesis_information(genesis: &[Datom]) -> Result<(), SemanticError> {
    let genesis_tx = t_to_tx(0).expect("genesis t is representable");
    if genesis.is_empty()
        || genesis
            .iter()
            .any(|datom| datom.tx != genesis_tx || !datom.added)
        || genesis
            .windows(2)
            .any(|pair| !pair[0].cmp_in(&pair[1], IndexOrder::Eavt).is_lt())
    {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "kernel/noncanonical-genesis",
            "genesis must be a nonempty, strictly ordered set of t=0 assertions",
        ));
    }
    if !same_stored_datoms(genesis, &canonical_genesis_datoms()) {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "kernel/noncanonical-genesis",
            "genesis must equal the current canonical native system information set",
        ));
    }
    Ok(())
}

fn facts_as_datoms(facts: &[CurrentFact]) -> Vec<Datom> {
    facts
        .iter()
        .map(|fact| Datom {
            entity: fact.entity,
            attribute: fact.attribute,
            value: fact.value.clone(),
            tx: fact.tx,
            added: true,
        })
        .collect()
}

/// A borrowed key with the same logical equality as the value indexes. Stored
/// equality cannot itself implement `Ord`: two decimal scales differ, but each
/// can compare stored-equal to a non-decimal number of the same logical value.
#[derive(Clone, Copy)]
struct IndexValue<'a>(&'a Value);

impl PartialEq for IndexValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for IndexValue<'_> {}

impl PartialOrd for IndexValue<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexValue<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.index_cmp(other.0)
    }
}

type StoredFactKey<'a> = (u64, u32, IndexValue<'a>, Option<i64>);

/// Index original positions, rather than rebuilding facts in key order. This
/// preserves the first stored representation and survivor/insertion order of
/// the simple replay oracle, including redundant adds and alteration hooks.
#[derive(Default)]
struct StoredFactIndex<'a> {
    positions: BTreeMap<StoredFactKey<'a>, Vec<usize>>,
}

impl<'a> StoredFactIndex<'a> {
    fn scale(value: &Value) -> Option<i64> {
        match value {
            Value::BigDec(value) => Some(value.fractional_digit_count()),
            _ => None,
        }
    }

    fn from_current(facts: &'a [CurrentFact]) -> Self {
        let mut index = Self::default();
        for (position, fact) in facts.iter().enumerate() {
            index.insert(fact.entity, fact.attribute, &fact.value, position);
        }
        index
    }

    fn insert(&mut self, entity: u64, attribute: u32, value: &'a Value, position: usize) {
        self.positions
            .entry((entity, attribute, IndexValue(value), Self::scale(value)))
            .or_default()
            .push(position);
    }

    fn contains(&self, entity: u64, attribute: u32, value: &Value) -> bool {
        self.first(entity, attribute, value).is_some()
    }

    fn first(&self, entity: u64, attribute: u32, value: &Value) -> Option<usize> {
        let logical = IndexValue(value);
        if let Some(scale) = Self::scale(value) {
            [None, Some(scale)]
                .into_iter()
                .filter_map(|scale| {
                    self.positions
                        .get(&(entity, attribute, logical, scale))
                        .and_then(|positions| positions.first().copied())
                })
                .min()
        } else {
            // Non-decimal numbers match every decimal scale in the logical
            // group. Schema-valid scalar attributes have a fixed value type;
            // the range also preserves the old behavior for malformed inputs.
            self.positions
                .range(
                    (entity, attribute, logical, None)
                        ..=(entity, attribute, logical, Some(i64::MAX)),
                )
                .filter_map(|(_, positions)| positions.first().copied())
                .min()
        }
    }

    fn remove(&mut self, entity: u64, attribute: u32, value: &'a Value) -> Vec<usize> {
        let logical = IndexValue(value);
        if let Some(scale) = Self::scale(value) {
            [None, Some(scale)]
                .into_iter()
                .filter_map(|scale| self.positions.remove(&(entity, attribute, logical, scale)))
                .flatten()
                .collect()
        } else {
            let keys: Vec<_> = self
                .positions
                .range(
                    (entity, attribute, logical, None)
                        ..=(entity, attribute, logical, Some(i64::MAX)),
                )
                .map(|(key, _)| *key)
                .collect();
            keys.into_iter()
                .filter_map(|key| self.positions.remove(&key))
                .flatten()
                .collect()
        }
    }
}

fn replay<'a>(datoms: impl Iterator<Item = &'a Datom>) -> Vec<CurrentFact> {
    let mut index = StoredFactIndex::default();
    let mut result = Vec::<Option<CurrentFact>>::new();
    for datom in datoms {
        if datom.added {
            if let Some(position) = index.first(datom.entity, datom.attribute, &datom.value) {
                if u64::from(datom.attribute) == crate::DB_ALTER_ATTRIBUTE {
                    result[position]
                        .as_mut()
                        .expect("indexed fact is present")
                        .tx = datom.tx;
                }
            } else {
                index.insert(datom.entity, datom.attribute, &datom.value, result.len());
                result.push(Some(CurrentFact {
                    entity: datom.entity,
                    attribute: datom.attribute,
                    value: datom.value.clone(),
                    tx: datom.tx,
                }));
            }
        } else {
            for position in index.remove(datom.entity, datom.attribute, &datom.value) {
                result[position] = None;
            }
        }
    }
    result.into_iter().flatten().collect()
}

fn validate_stored_entity(
    entity: u64,
    frontier: u64,
    basis_t: u64,
    schema: &Schema,
) -> Result<(), SemanticError> {
    validate_supported_eid(entity).map_err(|error| {
        SemanticError::new(
            ErrorCategory::Fault,
            "kernel/invalid-stored-entity-id",
            format!("stored entity id {entity} is invalid: {error}"),
        )
    })?;
    schema
        .validate_partition_bits(eid_to_part(entity)?)
        .map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "kernel/invalid-stored-partition",
                format!("stored entity partition is invalid: {error}"),
            )
        })?;
    let eidx = eid_to_eidx(entity).expect("supported entity id was checked");
    if eidx >= frontier {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "kernel/unissued-stored-entity-id",
            format!("stored entity index {eidx} is not below issued frontier {frontier}"),
        ));
    }
    if eid_to_part(entity).expect("supported entity id was checked") == crate::TX_PARTITION {
        let t = tx_to_t(entity).expect("transaction partition was checked");
        if t == 0 || t > basis_t {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "kernel/stored-transaction-out-of-range",
                format!("stored transaction entity t={t} is outside basis {basis_t}"),
            ));
        }
    }
    Ok(())
}

fn validate_stored_value(
    value: &Value,
    frontier: u64,
    basis_t: u64,
    schema: &Schema,
) -> Result<(), SemanticError> {
    match value {
        Value::Ref(entity) => validate_stored_entity(*entity, frontier, basis_t, schema),
        Value::Tuple(slots) => {
            for value in slots.iter().flatten() {
                validate_stored_value(value, frontier, basis_t, schema)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn allocations_under_authenticated_frontier(
    prior_frontier: u64,
    transaction: &crate::DurableTransaction,
    schema: &Schema,
) -> Result<BTreeMap<u64, u64>, SemanticError> {
    validate_frontier(transaction.eidx_frontier)?;
    let start = prior_frontier.max(transaction.basis_t.saturating_add(1));
    if transaction.eidx_frontier < start {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "recovery/eidx-frontier-mismatch",
            "Authenticated ordinary frontier regresses or does not cover transaction time",
        ));
    }
    let mut allocations = BTreeMap::new();
    let mut observe = |entity| -> Result<(), SemanticError> {
        validate_supported_eid(entity)?;
        let partition = eid_to_part(entity)?;
        schema.validate_partition_bits(partition)?;
        let index = eid_to_eidx(entity)?;
        // The reserved cursor is independent of ordinary allocation; its exact
        // coverage is checked by observe_transaction and the common validator.
        if partition == TX_PARTITION || partition == crate::DB_PARTITION || index < prior_frontier {
            return Ok(());
        }
        if index < start || index >= transaction.eidx_frontier {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/unwitnessed-entity-allocation",
                "Fresh ordinary identity lies outside its authenticated allocation interval",
            ));
        }
        if allocations
            .insert(index, entity)
            .is_some_and(|previous| previous != entity)
        {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/duplicate-tempid-index",
                "Distinct fresh ordinary entities share a global entity index",
            ));
        }
        Ok(())
    };
    for datom in &transaction.tx_data {
        observe(datom.entity)?;
        let mut pending = vec![&datom.value];
        while let Some(value) = pending.pop() {
            match value {
                Value::Ref(entity) => observe(*entity)?,
                Value::Tuple(slots) => pending.extend(slots.iter().flatten()),
                _ => {}
            }
        }
    }
    Ok(allocations)
}

fn expected_frontier_after_commit(
    current_frontier: u64,
    t: u64,
    tempids: &BTreeMap<String, u64>,
    schema_changes: &[SchemaChange],
    schema: &Schema,
    reserved_after: Option<ReservedAllocation>,
) -> Result<(u64, BTreeMap<u64, u64>), SemanticError> {
    validate_frontier(current_frontier)?;
    let start = current_frontier.max(t.checked_add(1).ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::Fault,
            "recovery/basis-overflow",
            "transaction time cannot advance the issued frontier",
        )
    })?);
    validate_frontier(start)?;

    let mut allocated = BTreeMap::new();
    let reserved_tempids: BTreeSet<_> = if reserved_after.is_some() {
        tempids
            .values()
            .copied()
            .filter(|entity| eid_to_part(*entity) == Ok(crate::DB_PARTITION))
            .collect()
    } else {
        BTreeSet::new()
    };
    for change in schema_changes {
        let SchemaChange::Install(attribute) = change else {
            continue;
        };
        let eidx = u64::from(attribute.id);
        if eidx >= current_frontier && !reserved_tempids.contains(&eidx) {
            allocated.insert(eidx, eidx);
        }
    }
    for entity in tempids.values().copied() {
        validate_supported_eid(entity).map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/invalid-tempid-entity",
                format!("committed tempid resolves to an invalid entity id: {error}"),
            )
        })?;
        let partition = eid_to_part(entity).expect("supported entity id was checked");
        schema.validate_partition_bits(partition).map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/invalid-tempid-partition",
                format!("committed tempid names an invalid partition: {error}"),
            )
        })?;
        let eidx = eid_to_eidx(entity).expect("supported entity id was checked");
        if partition == crate::TX_PARTITION {
            if eidx == 0 || eidx > t {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/invalid-tempid-allocation",
                    "transaction tempid is outside the committed basis",
                ));
            }
            continue;
        }
        if partition == crate::DB_PARTITION && reserved_after.is_some() {
            continue;
        }
        if eidx >= current_frontier {
            if eidx < start {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/invalid-tempid-allocation",
                    "fresh tempid allocations must use contiguous global entity indices",
                ));
            }
            if allocated
                .insert(eidx, entity)
                .is_some_and(|prior| prior != entity)
            {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "recovery/duplicate-tempid-index",
                    "distinct fresh entities share a global entity index",
                ));
            }
        }
    }
    for (offset, actual) in allocated.keys().copied().enumerate() {
        let expected = start.checked_add(offset as u64).ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Fault,
                "recovery/entity-id-overflow",
                "committed tempid allocation overflows the entity-index space",
            )
        })?;
        if actual != expected {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "recovery/noncontiguous-tempid-allocation",
                format!("expected allocated entity index {expected}, got {actual}"),
            ));
        }
    }
    let frontier = start.checked_add(allocated.len() as u64).ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::Fault,
            "recovery/entity-id-overflow",
            "committed tempid allocation overflows the entity-index space",
        )
    })?;
    let frontier = reserved_after.map_or(frontier, |reserved| frontier.max(reserved.frontier()));
    validate_frontier(frontier)?;
    Ok((frontier, allocated))
}

fn validate_new_entity_allocation(
    entity: u64,
    prior_frontier: u64,
    allocations: &BTreeMap<u64, u64>,
    reserved_after: Option<ReservedAllocation>,
) -> Result<(), SemanticError> {
    let index = eid_to_eidx(entity)?;
    if eid_to_part(entity)? == crate::DB_PARTITION
        && let Some(reserved) = reserved_after
    {
        if index < reserved.frontier() {
            return Ok(());
        }
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "recovery/unwitnessed-reserved-allocation",
            "reserved identity exceeds its authenticated checkpoint",
        ));
    }
    if eid_to_part(entity)? != TX_PARTITION
        && index >= prior_frontier
        && allocations.get(&index) != Some(&entity)
    {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "recovery/unwitnessed-entity-allocation",
            "new entity or reference does not match its exact global allocation witness",
        ));
    }
    Ok(())
}

fn validate_new_value_allocations(
    value: &Value,
    prior_frontier: u64,
    allocations: &BTreeMap<u64, u64>,
    reserved_after: Option<ReservedAllocation>,
) -> Result<(), SemanticError> {
    match value {
        Value::Ref(entity) => {
            validate_new_entity_allocation(*entity, prior_frontier, allocations, reserved_after)
        }
        Value::Tuple(slots) => {
            for value in slots.iter().flatten() {
                validate_new_value_allocations(value, prior_frontier, allocations, reserved_after)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
