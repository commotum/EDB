//! Incremental transaction assessment over one exact immutable database value.
//!
//! All memory, speculative and durable transactions use this assessor. It
//! obtains left-prefix ranges from db-before and represents the successor as
//! an immutable delta, independently of how its caller stores the result.

use crate::database::normalize_excision_before_t;
use crate::database_value::read_context::TransactionReadContext;
use crate::model::identity::{validate_frontier, validate_supported_eid};
use crate::model::idents::IdentIndex;
use crate::model::vocabulary::{supported_system_attributes, supported_system_idents};
use crate::transaction::input::{UpsertIdentityValue, validated_entity_tempids};
use crate::transaction_stats::count as count_work;
use crate::{
    Cardinality, DB_ALTER_ATTRIBUTE, DB_ATTR_PREDS, DB_CARDINALITY, DB_ENSURE, DB_ENTITY_ATTRS,
    DB_ENTITY_PREDS, DB_EXCISE, DB_EXCISE_BEFORE, DB_EXCISE_BEFORE_T, DB_IDENT, DB_INDEX,
    DB_INSTALL_ATTRIBUTE, DB_IS_COMPONENT, DB_NO_HISTORY, DB_PART_DB, DB_TUPLE_ATTRS,
    DB_TUPLE_DISCONTINUED, DB_TUPLE_TYPE, DB_TUPLE_TYPES, DB_TX_INSTANT, DB_UNIQUE, DB_VALUE_TYPE,
    DatabaseValue, Datom, EntityRef, ErrorCategory, IndexOrder, IndexPrefix, Schema, SemanticError,
    TX_PARTITION, TupleSpec, TxFunctions, TxOp, TxValue, Unique, Value, ValueType, eid_to_eidx,
    eid_to_part, make_eid, t_to_tx,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AssessmentReadWork {
    /// Exact-prefix source cursor misses. Repeated transaction-local requests
    /// served by the shared attempt memo are counted in `prefix_hits`.
    pub(crate) prefixes: u64,
    pub(crate) prefix_hits: u64,
    pub(crate) datoms: u64,
    pub(crate) retained_bytes: u64,
    /// Active composites selected through the schema's recovered
    /// constituent reverse index. This must follow touched attributes rather
    /// than total installed schema width.
    pub(crate) composite_candidates: u64,
    /// Definition checks include preparation, metadata derivation and final
    /// successor validation, charged inside the schema validators themselves.
    pub(crate) schema_validation: crate::model::schema::SchemaValidationWork,
    /// Descriptor visits comparing db-before and proposed schema.
    pub(crate) schema_transition_attributes: u64,
    /// Descriptor visits rebuilding resident schema information/idents.
    pub(crate) schema_projection_attributes: u64,
    pub(crate) schema_reuses: u64,
    /// Actual reverse-index requests/edges, including edges later deduplicated.
    pub(crate) dependency_lookups: u64,
    pub(crate) dependency_edges: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AssessmentLimits {
    /// Writer admission controls.
    ///
    /// Reads against db-before enforce invariants and set a lower bound on
    /// transaction latency. The PostgreSQL writer configures these limits
    /// explicitly so one transaction
    /// cannot monopolize its finite resources.  Streaming validators must
    /// still charge every db-before datom they consume; they must not turn the
    /// limit into a reason to retain the complete range in memory.
    pub(crate) max_read_datoms: u64,
    pub(crate) max_read_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnsureRequirement {
    pub(crate) entity: u64,
    pub(crate) spec: u64,
    pub(crate) required: Vec<u32>,
    pub(crate) predicates: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TieredAssessment {
    pub(crate) db_before: DatabaseValue,
    pub(crate) db_after: DatabaseValue,
    pub(crate) basis_t: u64,
    pub(crate) eidx_frontier: u64,
    pub(crate) tx_data: Vec<Datom>,
    pub(crate) tempids: BTreeMap<String, u64>,
    pub(crate) ensures: Vec<EnsureRequirement>,
    pub(crate) successor_schema: Arc<Schema>,
    pub(crate) read_work: AssessmentReadWork,
}

impl TieredAssessment {
    /// Resolve only predicates this assessed transaction can execute. Missing
    /// required attributes short-circuit before predicate bindings are looked up.
    pub(crate) fn predicate_requirements(
        &self,
    ) -> Result<BTreeMap<String, crate::transaction::functions::PredicateRole>, SemanticError> {
        self.validate_ensure_attributes()?;
        let mut required = BTreeMap::new();
        for datom in self.tx_data.iter().filter(|datom| datom.added) {
            let attribute = self.db_before.schema().attribute(datom.attribute)?;
            for predicate in &attribute.predicates {
                insert_predicate_role(
                    &mut required,
                    predicate,
                    crate::transaction::functions::PredicateRole::Attribute,
                )?;
            }
        }
        for ensure in &self.ensures {
            for predicate in &ensure.predicates {
                insert_predicate_role(
                    &mut required,
                    predicate,
                    crate::transaction::functions::PredicateRole::Entity,
                )?;
            }
        }
        Ok(required)
    }

    /// Run the delayed add-data and ensure hooks against the exact immutable
    /// db-before/db-after pair. All callbacks receive the same `DatabaseValue`.
    pub(crate) fn validate_exact(
        &self,
        functions: Option<&TxFunctions>,
    ) -> Result<(), SemanticError> {
        for datom in self.tx_data.iter().filter(|datom| datom.added) {
            let attribute = self.db_before.schema().attribute(datom.attribute)?;
            for predicate in &attribute.predicates {
                let result = functions
                    .ok_or_else(|| {
                        SemanticError::incorrect(
                            "transaction/missing-predicate-context",
                            format!(
                                "attribute {} requires predicate {predicate}",
                                attribute.ident.qualified_name()
                            ),
                        )
                    })?
                    .validate_attribute_predicate(predicate, &datom.value)?;
                if !crate::is_exact_true(&result) {
                    return Err(SemanticError::incorrect(
                        "transaction/attribute-predicate",
                        format!(
                            "entity {} attribute {} value {:?} failed predicate {predicate} with result {result:?}",
                            datom.entity,
                            attribute.ident.qualified_name(),
                            datom.value,
                        ),
                    )
                    .detail("entity", datom.entity.to_string())
                    .detail("attribute", attribute.ident.qualified_name())
                    .detail("value", format!("{:?}", datom.value))
                    .detail("predicate", predicate.clone())
                    .detail("pred_return", format!("{result:?}")));
                }
            }
        }
        self.validate_ensure_attributes()?;
        for ensure in &self.ensures {
            for predicate in &ensure.predicates {
                let result = functions
                    .ok_or_else(|| {
                        SemanticError::incorrect(
                            "transaction/missing-predicate-context",
                            format!("entity spec {} requires predicate {predicate}", ensure.spec),
                        )
                    })?
                    .validate_entity_predicate(predicate, &self.db_after, ensure.entity)?;
                if !crate::is_exact_true(&result) {
                    return Err(SemanticError::incorrect(
                        "transaction/entity-predicate",
                        format!(
                            "entity {} failed predicate {predicate} of spec {}",
                            ensure.entity, ensure.spec
                        ),
                    )
                    .detail("pred_return", format!("{result:?}")));
                }
            }
        }
        Ok(())
    }

    fn validate_ensure_attributes(&self) -> Result<(), SemanticError> {
        for ensure in &self.ensures {
            let mut missing = Vec::new();
            for attribute in &ensure.required {
                if self.db_after.values(ensure.entity, *attribute)?.is_empty() {
                    missing.push(*attribute);
                }
            }
            if !missing.is_empty() {
                return Err(SemanticError::incorrect(
                    "transaction/entity-spec",
                    format!(
                        "entity {} is missing attributes {missing:?} of spec {}",
                        ensure.entity, ensure.spec
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn insert_predicate_role(
    required: &mut BTreeMap<String, crate::transaction::functions::PredicateRole>,
    predicate: &str,
    role: crate::transaction::functions::PredicateRole,
) -> Result<(), SemanticError> {
    required
        .entry(predicate.to_owned())
        .and_modify(|existing| *existing = existing.include(role))
        .or_insert(role);
    Ok(())
}

#[derive(Clone, Debug)]
struct LogicalDatom {
    entity: u64,
    attribute: u32,
    value: Value,
    added: bool,
}

struct Reader<'a> {
    base: &'a DatabaseValue,
    limits: AssessmentLimits,
    work: AssessmentReadWork,
}

impl<'a> Reader<'a> {
    fn new(base: &'a DatabaseValue, limits: AssessmentLimits) -> Self {
        Self {
            base,
            limits,
            work: AssessmentReadWork::default(),
        }
    }

    fn charge(&mut self, datom: &Datom) -> Result<(), SemanticError> {
        let next_datoms = self.work.datoms.checked_add(1).ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Busy,
                "transaction/read-capacity",
                "transaction assessment read count overflowed",
            )
        })?;
        let next_bytes = self
            .work
            .retained_bytes
            .checked_add(datom.retained_bytes())
            .ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::Busy,
                    "transaction/read-capacity",
                    "transaction assessment read-byte count overflowed",
                )
            })?;
        if next_datoms > self.limits.max_read_datoms || next_bytes > self.limits.max_read_bytes {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "transaction/read-capacity",
                format!(
                    "transaction assessment exceeds {} datoms or {} retained bytes",
                    self.limits.max_read_datoms, self.limits.max_read_bytes
                ),
            ));
        }
        self.work.datoms = next_datoms;
        self.work.retained_bytes = next_bytes;
        Ok(())
    }

    fn prefix(&mut self, prefix: &IndexPrefix) -> Result<Vec<Datom>, SemanticError> {
        // Identity, composite, redundancy, and uniqueness checks can reuse
        // previously read ranges. Memoize an exact
        // structural IndexPrefix for the complete transaction attempt,
        // avoiding repeated PostgreSQL/tree work without inventing an
        // asynchronous hint system.
        let mut cursor = self.base.current_prefix_cursor(prefix)?;
        if cursor.is_memo_hit() {
            self.work.prefix_hits = self.work.prefix_hits.saturating_add(1);
        } else {
            self.work.prefixes = self.work.prefixes.saturating_add(1);
        }
        let mut datoms = Vec::new();
        for datom in &mut cursor {
            let datom = datom?;
            self.charge(&datom)?;
            datoms.push(datom);
        }
        Ok(datoms)
    }

    fn has_historical_values(&mut self, attribute: u32) -> Result<bool, SemanticError> {
        let prefix = IndexPrefix::Aevt {
            attribute,
            entity: None,
            value: None,
        };
        let mut cursor = self.base.history_prefix_cursor(&prefix)?;
        if cursor.is_memo_hit() {
            self.work.prefix_hits = self.work.prefix_hits.saturating_add(1);
        } else {
            self.work.prefixes = self.work.prefixes.saturating_add(1);
        }
        let first = cursor.next().transpose()?;
        if let Some(datom) = &first {
            self.charge(datom)?;
        }
        Ok(first.is_some())
    }

    fn values(&mut self, entity: u64, attribute: u32) -> Result<Vec<Value>, SemanticError> {
        let mut values = self
            .prefix(&IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: None,
            })?
            .into_iter()
            .map(|datom| datom.value)
            .collect::<Vec<_>>();
        values.sort_by(Value::stored_cmp);
        values.dedup_by(|right, left| left.stored_eq(right));
        Ok(values)
    }

    fn contains_source(
        &mut self,
        entity: u64,
        attribute: u32,
        value: &Value,
        source: u64,
    ) -> Result<bool, SemanticError> {
        Ok(self
            .prefix(&IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: Some(value.clone()),
            })?
            .into_iter()
            .any(|datom| datom.source == source && datom.value.stored_eq(value)))
    }

    fn contains(
        &mut self,
        entity: u64,
        attribute: u32,
        value: &Value,
    ) -> Result<bool, SemanticError> {
        Ok(self
            .prefix(&IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: Some(value.clone()),
            })?
            .into_iter()
            .any(|datom| datom.value.stored_eq(value)))
    }

    fn lookup(&mut self, attribute: u32, value: &Value) -> Result<Option<u64>, SemanticError> {
        let descriptor = self.base.schema().attribute(attribute)?;
        if descriptor.unique.is_none() {
            return Err(SemanticError::incorrect(
                "transaction/lookup-non-unique",
                format!(
                    "attribute {} is not unique",
                    descriptor.ident.qualified_name()
                ),
            ));
        }
        self.base.schema().validate_value(descriptor, value)?;
        count_work(|work| &mut work.identity_lookups, 1);
        if u64::from(attribute) == DB_IDENT {
            // Ident aliases belong to this captured dictionary, even when a
            // rename has withdrawn their earlier :db/ident fact from AVET.
            return self.base.lookup(attribute, value);
        }
        Ok(self
            .prefix(&IndexPrefix::Avet {
                attribute,
                value: Some(value.clone()),
                entity: None,
            })?
            .first()
            .map(|datom| datom.entity))
    }
}

pub(crate) fn assess_tiered_with_remaining_limits_and_defaults(
    base: &DatabaseValue,
    ops: &[TxOp],
    tx_instant: i64,
    source: &EntityRef,
    limits: AssessmentLimits,
    defaults: &crate::TransactionDefaults,
) -> Result<TieredAssessment, SemanticError> {
    count_work(|work| &mut work.assessments, 1);
    count_work(|work| &mut work.input_operations, ops.len());
    // PostgreSQL supplies one context before persisted generation begins. The
    // standalone memory entry still needs the same cross-phase
    // behavior, so give it a private unbounded observer while Reader enforces
    // the caller's explicit assessment allowance.
    crate::transaction::input::validate_ops_input(ops)?;
    crate::transaction::input::validate_entity_input(source)?;
    let default_partition = defaults.resolve(base.schema(), |name| base.entid(name))?;
    let base = if base.transaction_read_context().is_some() {
        base.clone()
    } else {
        base.clone()
            .with_transaction_read_context(Arc::new(TransactionReadContext::new(
                u64::MAX,
                u64::MAX,
            )))
    };
    let base = &base;
    let lowered_source = match source {
        EntityRef::Edn(text) => Some(crate::edn_transaction::lower_entity_ref(base, text)?),
        _ => None,
    };
    let source = lowered_source.as_ref().unwrap_or(source);
    crate::transaction::input::validate_entity_input(source)?;
    if base
        .last_tx_instant()
        .is_some_and(|prior| tx_instant < prior)
    {
        return Err(SemanticError::incorrect(
            "transaction/non-monotonic-instant",
            format!("transaction instant {tx_instant} precedes db-before"),
        ));
    }
    let basis_t = base.basis_t().checked_add(1).ok_or_else(|| {
        SemanticError::incorrect(
            "transaction/basis-overflow",
            "database basis cannot advance beyond u64",
        )
    })?;
    let tx = t_to_tx(basis_t)?;
    let initial_allocation_start =
        base.eidx_frontier()
            .max(basis_t.checked_add(1).ok_or_else(|| {
                SemanticError::incorrect(
                    "transaction/basis-overflow",
                    "transaction time cannot advance the issued frontier",
                )
            })?);
    validate_frontier(initial_allocation_start)?;

    let mut ordered = ops.to_vec();
    crate::transaction::input::lower_ops_entity_refs(base, &mut ordered)?;
    ordered.sort_by(crate::transaction::canonical::compare_tx_op);
    validate_tx_instant_forms(&ordered, tx_instant)?;
    let mut reader = Reader::new(base, limits);
    let (mut logical, allocation_start) =
        prepare_schema_information(&mut reader, &ordered, tx, initial_allocation_start)?;
    let mut reserved_allocation = base.reserved_allocation()?;
    if let Some(reserved) = &mut reserved_allocation {
        crate::transaction::input::observe_reserved_transaction_inputs(reserved, &ordered, source)?;
    }
    let (tempids, eidx_frontier) = resolve_tempids(
        &mut reader,
        &ordered,
        source,
        tx,
        allocation_start,
        default_partition,
        &mut reserved_allocation,
    )?;
    let source = resolve_entity(&mut reader, source, tx, &tempids)?;
    let mut ensures = Vec::new();
    let mut touched = BTreeSet::new();
    for op in &ordered {
        expand_op(
            &mut reader,
            op,
            tx,
            &tempids,
            &mut logical,
            &mut ensures,
            &mut touched,
        )?;
    }
    ensures.sort_by_key(|ensure| (ensure.entity, ensure.spec));
    ensures.dedup_by_key(|ensure| (ensure.entity, ensure.spec));

    dedupe(&mut logical);
    validate_same_transaction(base.schema(), &logical)?;
    add_cardinality_one_retractions(&mut reader, &mut logical)?;
    dedupe(&mut logical);
    validate_same_transaction(base.schema(), &logical)?;
    synthesize_schema_hooks(&mut reader, &mut logical)?;
    dedupe(&mut logical);
    validate_same_transaction(base.schema(), &logical)?;
    derive_composites(&mut reader, &mut logical, &touched)?;
    logical.push(LogicalDatom {
        entity: tx,
        attribute: DB_TX_INSTANT as u32,
        value: Value::Instant(tx_instant),
        added: true,
    });
    dedupe(&mut logical);
    validate_same_transaction(base.schema(), &logical)?;

    let successor_schema = derive_successor_schema(&mut reader, &logical, tx)?;
    validate_schema_transition(&mut reader, &successor_schema, &logical)?;
    validate_delta_successor(&mut reader, &successor_schema, &logical)?;
    validate_ensure_attributes(&mut reader, &logical, &ensures)?;
    let mut tx_data = material_changes(&mut reader, &logical, tx, source)?;
    count_work(|work| &mut work.produced_datoms, tx_data.len());
    tx_data.sort_by(|left, right| left.cmp_in(right, IndexOrder::Eavt));

    let db_after = DatabaseValue::transaction_overlay_with_allocation(
        base.clone(),
        Arc::from(tx_data.clone()),
        Arc::clone(&successor_schema),
        basis_t,
        eidx_frontier,
        tx_instant,
        reserved_allocation,
    )?;

    Ok(TieredAssessment {
        db_before: base.clone(),
        db_after,
        basis_t,
        eidx_frontier,
        tx_data,
        tempids,
        ensures,
        successor_schema,
        read_work: reader.work,
    })
}

fn prepare_schema_information(
    reader: &mut Reader<'_>,
    ops: &[TxOp],
    tx: u64,
    mut allocation_frontier: u64,
) -> Result<(Vec<LogicalDatom>, u64), SemanticError> {
    if !ops
        .iter()
        .any(|op| matches!(op, TxOp::InstallAttribute(_) | TxOp::AlterAttribute(_)))
    {
        return Ok((Vec::new(), allocation_frontier));
    }

    let mut seen = BTreeSet::new();
    let mut installed = Vec::new();
    // A real schema edit needs one mutable candidate for cross-attribute
    // validation. Ordinary data transactions return above and never copy the
    // resident schema merely to discover that it did not change.
    reader.work.schema_projection_attributes = reader
        .work
        .schema_projection_attributes
        .saturating_add(reader.base.schema().attribute_count() as u64);
    let mut candidate = reader.base.schema().clone();
    let mut changes = Vec::<(crate::Attribute, bool)>::new();

    for op in ops {
        let (attribute, install) = match op {
            TxOp::InstallAttribute(attribute) => (attribute, true),
            TxOp::AlterAttribute(attribute) => (attribute, false),
            _ => continue,
        };
        if !seen.insert(attribute.id) {
            return Err(SemanticError::conflict(
                "schema/multiple-changes",
                format!(
                    "attribute {} has multiple schema changes in one transaction",
                    attribute.id
                ),
            ));
        }
        crate::schema_eid_to_attr_id(u64::from(attribute.id))?;
        if install {
            let reserved = supported_system_idents()
                .iter()
                .any(|(entity, _)| *entity == u64::from(attribute.id));
            if reserved {
                return Err(SemanticError::incorrect(
                    "schema/reserved-system-entity",
                    format!(
                        "entity {} is reserved by the native vocabulary",
                        attribute.id
                    ),
                ));
            }
            if u64::from(attribute.id) >= allocation_frontier {
                if u64::from(attribute.id) != allocation_frontier {
                    return Err(SemanticError::incorrect(
                        "schema/noncontiguous-attribute-id",
                        format!(
                            "fresh schema entity {} must use issued frontier {}",
                            attribute.id, allocation_frontier
                        ),
                    ));
                }
                allocation_frontier = allocation_frontier.checked_add(1).ok_or_else(|| {
                    SemanticError::incorrect(
                        "schema/attribute-id-overflow",
                        "schema entity allocation exhausted the entity-index space",
                    )
                })?;
            }
            candidate.install_with_work(attribute.clone(), &mut reader.work.schema_validation)?;
            installed.push(attribute.id);
        } else {
            candidate.alter_with_work(attribute.clone(), &mut reader.work.schema_validation)?;
        }
        changes.push((attribute.clone(), install));
    }
    candidate
        .validate_tuple_installations_with_work(&installed, &mut reader.work.schema_validation)?;

    let metadata_attributes = [
        DB_IDENT as u32,
        DB_VALUE_TYPE as u32,
        DB_CARDINALITY as u32,
        DB_UNIQUE as u32,
        DB_IS_COMPONENT as u32,
        DB_INDEX as u32,
        DB_NO_HISTORY as u32,
        crate::DB_FULLTEXT as u32,
        DB_TUPLE_TYPE as u32,
        DB_TUPLE_TYPES as u32,
        DB_TUPLE_ATTRS as u32,
        DB_ATTR_PREDS as u32,
        DB_TUPLE_DISCONTINUED as u32,
    ];
    let mut logical = Vec::new();
    for (attribute, install) in changes {
        let desired = crate::model::schema::attribute_information_datoms(
            &attribute,
            &candidate,
            tx,
            crate::DB_SOURCE_SYSTEM,
        )?;
        let current = (!install)
            .then(|| reader.base.schema().attribute(attribute.id))
            .transpose()?;
        let property_changed = |metadata_attribute| {
            install
                || current.is_some_and(|current| {
                    attribute_property_changed(current, &attribute, metadata_attribute)
                })
        };
        for fact in reader.prefix(&IndexPrefix::Eavt {
            entity: u64::from(attribute.id),
            attribute: None,
            value: None,
        })? {
            if metadata_attributes.contains(&fact.attribute)
                && property_changed(fact.attribute)
                && !desired.iter().any(|wanted| same_eav(wanted, &fact))
            {
                logical.push(LogicalDatom {
                    entity: fact.entity,
                    attribute: fact.attribute,
                    value: fact.value,
                    added: false,
                });
            }
        }
        for datom in desired {
            // A changed property's asserted values can already be present
            // under another source. Only the material boundary can decide
            // whether this transaction's contribution is redundant.
            if property_changed(datom.attribute) {
                logical.push(LogicalDatom {
                    entity: datom.entity,
                    attribute: datom.attribute,
                    value: datom.value,
                    added: true,
                });
            }
        }
    }
    validate_frontier(allocation_frontier)?;
    Ok((logical, allocation_frontier))
}

fn synthesize_schema_hooks(
    reader: &mut Reader<'_>,
    logical: &mut Vec<LogicalDatom>,
) -> Result<(), SemanticError> {
    let mut explicit = BTreeMap::<u64, u32>::new();
    for datom in logical.iter().filter(|datom| {
        matches!(
            u64::from(datom.attribute),
            DB_INSTALL_ATTRIBUTE | DB_ALTER_ATTRIBUTE
        )
    }) {
        if !datom.added || datom.entity != DB_PART_DB {
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
        let expected = if reader.base.schema().attribute(attribute).is_ok() {
            DB_ALTER_ATTRIBUTE as u32
        } else {
            DB_INSTALL_ATTRIBUTE as u32
        };
        if datom.attribute != expected {
            return Err(SemanticError::incorrect(
                "schema/wrong-hook-kind",
                "schema hook kind does not match db-before installation state",
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

    let mut touched = BTreeSet::new();
    for datom in logical
        .iter()
        .filter(|datom| is_attribute_hook_property(datom.attribute))
    {
        crate::schema_eid_to_attr_id(datom.entity)?;
        if reader.contains(datom.entity, datom.attribute, &datom.value)? != datom.added {
            touched.insert(datom.entity);
        }
    }
    logical.retain(|datom| {
        !matches!(
            u64::from(datom.attribute),
            DB_INSTALL_ATTRIBUTE | DB_ALTER_ATTRIBUTE
        )
    });
    for target in touched {
        let attribute = crate::schema_eid_to_attr_id(target)?;
        logical.push(LogicalDatom {
            entity: DB_PART_DB,
            attribute: if reader.base.schema().attribute(attribute).is_ok() {
                DB_ALTER_ATTRIBUTE as u32
            } else {
                DB_INSTALL_ATTRIBUTE as u32
            },
            value: Value::Ref(target),
            added: true,
        });
    }
    Ok(())
}

fn derive_successor_schema(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    tx: u64,
) -> Result<Arc<Schema>, SemanticError> {
    // Schema and ident projections are resident authenticated metadata. Most
    // transactions do not touch either; preserve the immutable projection
    // directly instead of issuing one durable EAVT seek per attribute merely
    // to rediscover an unchanged schema.
    if !logical.iter().any(|datom| {
        reader.base.schema().datom_may_change_schema(
            datom.entity,
            datom.attribute,
            &datom.value,
            datom.added,
        )
    }) {
        return Ok(reader.base.schema_arc());
    }

    // The authenticated Schema is the resident cache recovered `Db` keeps for
    // exactly this purpose. Reconstruct unchanged information from that cache;
    // issuing one EAVT seek for every installed attribute made a one-attribute
    // alter proportional to the whole schema and allowed an unrelated schema
    // size to exhaust the transaction read budget.
    //
    // For an entity whose raw schema-as-data facts are actually being edited,
    // retain the physical spelling of its current information. This matters
    // for details intentionally absent from the typed projection, notably a
    // composite's historical constituent ident after an ident rename. An
    // ident repurpose can also affect an active composite without touching the
    // composite entity, so include only composites that depend on the ident's
    // previous target. Discontinued composites no longer have active derivation
    // links, but their stored constituent names must still reconstruct the same
    // schema projection as replay after an ident is repurposed.
    let mut physical_entities = BTreeSet::new();
    for datom in logical.iter().filter(|datom| {
        schema_information_attribute(datom.attribute)
            && datom.entity != DB_PART_DB
            && reader
                .base
                .schema()
                .attribute(u32::try_from(datom.entity).unwrap_or(u32::MAX))
                .is_ok()
    }) {
        physical_entities.insert(u32::try_from(datom.entity).map_err(|_| {
            SemanticError::incorrect(
                "schema/attribute-id-range",
                "schema entity does not fit the native attribute-id domain",
            )
        })?);
    }
    for target in logical.iter().filter_map(|datom| {
        (datom.entity == DB_PART_DB
            && matches!(
                u64::from(datom.attribute),
                DB_INSTALL_ATTRIBUTE | DB_ALTER_ATTRIBUTE
            ))
        .then_some(&datom.value)
    }) {
        if let Value::Ref(target) = target
            && let Ok(attribute) = crate::schema_eid_to_attr_id(*target)
            && reader.base.schema().attribute(attribute).is_ok()
        {
            physical_entities.insert(attribute);
        }
    }
    let retargeted_attributes = logical
        .iter()
        .filter_map(|datom| {
            if !datom.added || u64::from(datom.attribute) != DB_IDENT {
                return None;
            }
            let Value::Keyword(ident) = &datom.value else {
                return None;
            };
            let previous = reader.base.schema().resolve_ident(ident)?;
            (u64::from(previous) != datom.entity).then_some(previous)
        })
        .collect::<BTreeSet<_>>();
    for attribute in &retargeted_attributes {
        reader.work.dependency_lookups = reader.work.dependency_lookups.saturating_add(1);
        for composite in reader.base.schema().composites_for_constituent(*attribute) {
            reader.work.dependency_edges = reader.work.dependency_edges.saturating_add(1);
            physical_entities.insert(composite);
        }
    }

    let mut current = Vec::new();
    // Reserved vocabulary may already have its ident before its attribute
    // descriptor is explicitly installed. Lowering correctly omits that
    // nonmaterial assertion; retain the authenticated existing ident here.
    for datom in logical
        .iter()
        .filter(|datom| datom.added && u64::from(datom.attribute) == DB_INSTALL_ATTRIBUTE)
    {
        if let Value::Ref(entity) = datom.value
            && reader
                .base
                .schema()
                .attribute(u32::try_from(entity).unwrap_or(u32::MAX))
                .is_err()
            && let Some(ident) = reader.base.ident(entity)
        {
            current.push(ident_datom(entity, ident.clone(), 0));
        }
    }
    for (partition, _) in reader.base.schema().partitions() {
        current.push(Datom {
            entity: DB_PART_DB,
            attribute: crate::DB_INSTALL_PARTITION as u32,
            value: Value::Ref(u64::from(partition)),
            tx: 0,
            source: crate::DB_SOURCE_SYSTEM,
            added: true,
        });
    }
    for attribute in reader.base.schema().attributes() {
        reader.work.schema_projection_attributes =
            reader.work.schema_projection_attributes.saturating_add(1);
        current.push(Datom {
            entity: DB_PART_DB,
            attribute: DB_INSTALL_ATTRIBUTE as u32,
            value: Value::Ref(u64::from(attribute.id)),
            tx: 0,
            source: crate::DB_SOURCE_SYSTEM,
            added: true,
        });
        let discontinued_dependency = attribute.tuple_discontinued
            && matches!(&attribute.tuple, Some(TupleSpec::Composite(constituents))
                if constituents.iter().any(|id| retargeted_attributes.contains(id)));
        if physical_entities.contains(&attribute.id) || discontinued_dependency {
            current.extend(
                reader
                    .prefix(&IndexPrefix::Eavt {
                        entity: u64::from(attribute.id),
                        attribute: None,
                        value: None,
                    })?
                    .into_iter()
                    .filter(|datom| schema_information_attribute(datom.attribute)),
            );
        } else {
            current.extend(crate::model::schema::attribute_information_datoms(
                attribute,
                reader.base.schema(),
                0,
                crate::DB_SOURCE_SYSTEM,
            )?);
        }
    }
    for datom in logical.iter().filter(|datom| {
        schema_information_attribute(datom.attribute)
            || matches!(
                u64::from(datom.attribute),
                DB_INSTALL_ATTRIBUTE | DB_ALTER_ATTRIBUTE
            )
    }) {
        if datom.added {
            if !current.iter().any(|fact| {
                fact.entity == datom.entity
                    && fact.attribute == datom.attribute
                    && fact.value.stored_eq(&datom.value)
            }) {
                current.push(Datom {
                    entity: datom.entity,
                    attribute: datom.attribute,
                    value: datom.value.clone(),
                    tx,
                    source: crate::DB_SOURCE_SYSTEM,
                    added: true,
                });
            }
        } else {
            current.retain(|fact| {
                !(fact.entity == datom.entity
                    && fact.attribute == datom.attribute
                    && fact.value.stored_eq(&datom.value))
            });
        }
    }

    // Only attribute, partition and system aliases are required to derive Schema. The
    // full general ident cache remains resident in the concrete tiered value
    // and is updated delta-first by TransactionOverlay.
    let mut ident_assertions = Vec::new();
    for (partition, ident) in reader.base.schema().partitions() {
        ident_assertions.push(ident_datom(u64::from(partition), ident.clone(), 0));
    }
    // A partition can receive its ident in an earlier transaction. Its
    // ordinary current ident is already resident authenticated metadata;
    // installing the later marker must not require repeating the ident fact.
    for datom in logical
        .iter()
        .filter(|datom| datom.added && u64::from(datom.attribute) == crate::DB_INSTALL_PARTITION)
    {
        if let Value::Ref(entity) = datom.value
            && let Some(ident) = reader.base.ident(entity)
        {
            ident_assertions.push(ident_datom(entity, ident.clone(), 0));
        }
    }
    for (entity, ident) in supported_system_idents() {
        ident_assertions.push(ident_datom(entity, ident, 0));
    }
    for attribute in reader.base.schema().attributes() {
        reader.work.schema_projection_attributes =
            reader.work.schema_projection_attributes.saturating_add(1);
        ident_assertions.push(ident_datom(
            u64::from(attribute.id),
            attribute.ident.clone(),
            0,
        ));
    }
    for (ident, attribute) in reader.base.schema().ident_aliases() {
        ident_assertions.push(ident_datom(u64::from(attribute), ident.clone(), 0));
    }
    ident_assertions.extend(
        logical
            .iter()
            .filter(|datom| datom.added && u64::from(datom.attribute) == DB_IDENT)
            .map(|datom| Datom {
                entity: datom.entity,
                attribute: datom.attribute,
                value: datom.value.clone(),
                tx,
                source: crate::DB_SOURCE_SYSTEM,
                added: true,
            }),
    );
    let idents = IdentIndex::derive(ident_assertions.iter(), DB_IDENT as u32)?;
    Schema::derive_from_information_with_work(&current, &idents, &mut reader.work.schema_validation)
        .map(Arc::new)
}

fn ident_datom(entity: u64, ident: crate::Keyword, tx: u64) -> Datom {
    Datom {
        entity,
        attribute: DB_IDENT as u32,
        value: Value::Keyword(ident),
        tx,
        source: crate::DB_SOURCE_SYSTEM,
        added: true,
    }
}

fn validate_schema_transition(
    reader: &mut Reader<'_>,
    successor: &Schema,
    logical: &[LogicalDatom],
) -> Result<(), SemanticError> {
    let unchanged = std::ptr::eq(reader.base.schema(), successor);
    if !unchanged {
        // Keep the original failure precedence for real schema changes.
        reader
            .base
            .schema()
            .validate_partition_successor(successor)?;
    }
    for required in crate::canonical_genesis_datoms() {
        if logical.iter().any(|datom| {
            !datom.added
                && datom.entity == required.entity
                && datom.attribute == required.attribute
                && datom.value.stored_eq(&required.value)
        }) {
            return Err(SemanticError::incorrect(
                "schema/native-information-immutable",
                "native genesis information cannot be retracted or replaced",
            ));
        }
    }
    // Every database construction/recovery validates its schema, and schemas
    // exposed by database values cannot be mutated. Reusing that exact
    // allocation is therefore proof that no definition/partition transition
    // occurred, not a comparison or an untrusted caller-set validity flag.
    // The genesis-retraction guard above remains unconditional.
    if unchanged {
        reader.work.schema_reuses = reader.work.schema_reuses.saturating_add(1);
        return Ok(());
    }
    for system in supported_system_attributes() {
        if reader.base.schema().attribute(system.id).is_ok()
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
    for current in reader.base.schema().attributes() {
        reader.work.schema_transition_attributes =
            reader.work.schema_transition_attributes.saturating_add(1);
        if successor.attribute(current.id).is_err() {
            return Err(SemanticError::incorrect(
                "schema/removed-attribute",
                format!("committed information removed attribute {}", current.id),
            ));
        }
    }

    let mut installed = Vec::new();
    for proposed in successor.attributes() {
        reader.work.schema_transition_attributes =
            reader.work.schema_transition_attributes.saturating_add(1);
        match reader.base.schema().attribute(proposed.id) {
            Ok(current) if current == proposed => {}
            Ok(current) => {
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
                let tuple_property_touched = logical.iter().any(|datom| {
                    datom.entity == u64::from(proposed.id)
                        && matches!(
                            u64::from(datom.attribute),
                            DB_TUPLE_TYPE | DB_TUPLE_TYPES | DB_TUPLE_ATTRS
                        )
                });
                let discontinued_ident_retarget = current.tuple_discontinued
                    && proposed.tuple_discontinued
                    && !tuple_property_touched;
                if current.tuple != proposed.tuple && !discontinued_ident_retarget {
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
                if current.cardinality == Cardinality::Many
                    && proposed.cardinality == Cardinality::One
                {
                    validate_many_to_one_successor(reader, logical, proposed.id)?;
                }
                if current.unique.is_none() && proposed.unique.is_some() {
                    // `:db/index true` is only the logical schema fact. The
                    // recovered Attribute.hasAVET also requires physical
                    // storage availability; while a background backfill is
                    // pending, adding uniqueness must still fail. The sole
                    // synchronous exception is an attribute with no history,
                    // for which there is nothing to backfill.
                    if !reader.base.physical_avet_ready(proposed.id)
                        && reader.has_historical_values(proposed.id)?
                    {
                        return Err(SemanticError::incorrect(
                            "schema/unique-requires-avet",
                            "adding uniqueness to an attribute with historical values requires a physically ready AVET index",
                        ));
                    }
                    validate_unique_successor(reader, logical, proposed.id)?;
                }
            }
            Err(_) => {
                let reserved = supported_system_idents()
                    .iter()
                    .any(|(entity, _)| *entity == u64::from(proposed.id));
                if reserved
                    && supported_system_attributes()
                        .iter()
                        .find(|attribute| attribute.id == proposed.id)
                        != Some(proposed)
                {
                    return Err(SemanticError::incorrect(
                        "schema/reserved-system-entity",
                        format!(
                            "entity {} is reserved by the native vocabulary",
                            proposed.id
                        ),
                    ));
                }
                installed.push(proposed.id);
            }
        }
    }
    successor.validate_tuple_installations_with_work(&installed, &mut reader.work.schema_validation)
}

/// Visit the complete successor value of one attribute in a physical index
/// order without constructing that value.
///
/// The base prefix remains lazy; only references to this transaction's delta
/// are sorted and retained.  Exact stored E/A/V identity decides whether an
/// assertion is redundant or a retraction removes a base fact. This supplies
/// the successor attribute values for cardinality and uniqueness checks.
fn visit_successor_attribute(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    attribute: u32,
    order: IndexOrder,
    mut visit: impl FnMut(u64, &Value) -> Result<(), SemanticError>,
) -> Result<(), SemanticError> {
    debug_assert!(matches!(order, IndexOrder::Aevt | IndexOrder::Avet));
    let delta = ordered_attribute_delta(logical, attribute, order);
    let prefix = match order {
        IndexOrder::Aevt => IndexPrefix::Aevt {
            attribute,
            entity: None,
            value: None,
        },
        IndexOrder::Avet => IndexPrefix::Avet {
            attribute,
            value: None,
            entity: None,
        },
        _ => unreachable!("successor attribute validation uses AEVT or AVET"),
    };

    // Own a cheap immutable-value clone so the cursor's borrow is independent
    // of `reader`; `Reader::charge` can then account each item as it is pulled.
    let base = reader.base.clone();
    let mut cursor = base.current_prefix_cursor(&prefix)?;
    if cursor.is_memo_hit() {
        reader.work.prefix_hits = reader.work.prefix_hits.saturating_add(1);
    } else {
        reader.work.prefixes = reader.work.prefixes.saturating_add(1);
    }
    let mut previous = None;
    let mut base_next = next_charged(reader, &mut cursor, &mut previous)?;
    let mut delta_next = 0;

    loop {
        match (base_next.as_ref(), delta.get(delta_next).copied()) {
            (None, None) => return Ok(()),
            (Some(base_datom), None) => {
                visit(base_datom.entity, &base_datom.value)?;
                base_next = next_charged(reader, &mut cursor, &mut previous)?;
            }
            (None, Some(delta_datom)) => {
                if delta_datom.added {
                    visit(delta_datom.entity, &delta_datom.value)?;
                }
                delta_next += 1;
            }
            (Some(base_datom), Some(delta_datom)) => {
                match compare_attribute_eav(
                    base_datom.entity,
                    &base_datom.value,
                    delta_datom.entity,
                    &delta_datom.value,
                    order,
                ) {
                    Ordering::Less => {
                        visit(base_datom.entity, &base_datom.value)?;
                        base_next = next_charged(reader, &mut cursor, &mut previous)?;
                    }
                    Ordering::Greater => {
                        if delta_datom.added {
                            visit(delta_datom.entity, &delta_datom.value)?;
                        }
                        delta_next += 1;
                    }
                    Ordering::Equal => {
                        // One exact base fact plus an assertion is still one
                        // successor fact; a retraction suppresses it.
                        if delta_datom.added {
                            visit(base_datom.entity, &base_datom.value)?;
                        }
                        base_next = next_charged(reader, &mut cursor, &mut previous)?;
                        delta_next += 1;
                    }
                }
            }
        }
    }
}

/// Visit an attribute delta in the requested successor order when an AEVT
/// history probe has established that the immutable base has no values.
fn visit_empty_base_attribute(
    logical: &[LogicalDatom],
    attribute: u32,
    order: IndexOrder,
    mut visit: impl FnMut(u64, &Value) -> Result<(), SemanticError>,
) -> Result<(), SemanticError> {
    for datom in ordered_attribute_delta(logical, attribute, order) {
        if datom.added {
            visit(datom.entity, &datom.value)?;
        }
    }
    Ok(())
}

fn ordered_attribute_delta(
    logical: &[LogicalDatom],
    attribute: u32,
    order: IndexOrder,
) -> Vec<&LogicalDatom> {
    let mut delta = logical
        .iter()
        .filter(|datom| datom.attribute == attribute)
        .collect::<Vec<_>>();
    delta.sort_unstable_by(|left, right| {
        compare_attribute_eav(left.entity, &left.value, right.entity, &right.value, order)
            .then_with(|| right.added.cmp(&left.added))
    });
    delta
}

fn compare_attribute_eav(
    left_entity: u64,
    left_value: &Value,
    right_entity: u64,
    right_value: &Value,
    order: IndexOrder,
) -> Ordering {
    match order {
        IndexOrder::Aevt => left_entity
            .cmp(&right_entity)
            .then_with(|| left_value.stored_cmp(right_value)),
        IndexOrder::Avet => left_value
            .stored_cmp(right_value)
            .then(left_entity.cmp(&right_entity)),
        _ => unreachable!("successor attribute validation uses AEVT or AVET"),
    }
}

fn next_charged(
    reader: &mut Reader<'_>,
    cursor: &mut impl Iterator<Item = Result<Datom, SemanticError>>,
    previous: &mut Option<Datom>,
) -> Result<Option<Datom>, SemanticError> {
    for next in cursor {
        let datom = next?;
        reader.charge(&datom)?;
        if previous
            .as_ref()
            .is_some_and(|prior| same_eav(prior, &datom))
        {
            continue;
        }
        *previous = Some(datom.clone());
        return Ok(Some(datom));
    }
    Ok(None)
}

fn validate_many_to_one_successor(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    attribute: u32,
) -> Result<(), SemanticError> {
    // Recovered `card-one-violator` reduces AEVT and compares adjacent entity
    // ids.  AEVT makes the complete group contiguous, so one prior id is the
    // entire retained validation state.
    let mut prior_entity = None;
    visit_successor_attribute(reader, logical, attribute, IndexOrder::Aevt, |entity, _| {
        if prior_entity == Some(entity) {
            return Err(SemanticError::conflict(
                "schema/cardinality-change-conflict",
                "current data contains multiple values for one entity",
            ));
        }
        prior_entity = Some(entity);
        Ok(())
    })
}

fn validate_unique_successor(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    attribute: u32,
) -> Result<(), SemanticError> {
    // `Value::stored_cmp` refines `index_cmp`, so all physically distinct
    // representations of one logical value (notably BigDecimal scales) stay
    // adjacent in AVET.  Retaining one value and owner is therefore enough.
    let mut prior: Option<(Value, u64)> = None;
    let mut validate = |entity: u64, value: &Value| {
        count_work(|work| &mut work.uniqueness_checks, 1);
        if value.is_nan() {
            return Err(SemanticError::incorrect(
                "transaction/nan-cannot-identify",
                "NaN cannot participate in uniqueness",
            ));
        }
        match &prior {
            Some((prior_value, prior_entity))
                if prior_value.index_cmp(value).is_eq() && *prior_entity != entity =>
            {
                return Err(SemanticError::conflict(
                    "schema/unique-change-conflict",
                    "current values must be unique before adding uniqueness",
                ));
            }
            Some((prior_value, _)) if prior_value.index_cmp(value).is_eq() => {}
            _ => prior = Some((value.clone(), entity)),
        }
        Ok(())
    };

    if reader.base.physical_avet_ready(attribute) {
        // This is the exact recovered `unique-violator` access path.
        visit_successor_attribute(reader, logical, attribute, IndexOrder::Avet, &mut validate)
    } else {
        // The caller's complete history-AEVT probe established that this
        // immutable base never held a value.  There is no missing AVET range;
        // sorting only the bounded transaction delta is sufficient.
        visit_empty_base_attribute(logical, attribute, IndexOrder::Avet, &mut validate)
    }
}

fn schema_information_attribute(attribute: u32) -> bool {
    matches!(
        u64::from(attribute),
        DB_IDENT
            | crate::DB_INSTALL_PARTITION
            | DB_INSTALL_ATTRIBUTE
            | DB_ALTER_ATTRIBUTE
            | DB_VALUE_TYPE
            | DB_CARDINALITY
            | DB_UNIQUE
            | DB_IS_COMPONENT
            | DB_INDEX
            | DB_NO_HISTORY
            | crate::DB_FULLTEXT
            | DB_TUPLE_TYPE
            | DB_TUPLE_TYPES
            | DB_TUPLE_ATTRS
            | DB_TUPLE_DISCONTINUED
            | DB_ATTR_PREDS
    )
}

fn is_attribute_hook_property(attribute: u32) -> bool {
    matches!(
        u64::from(attribute),
        DB_VALUE_TYPE
            | DB_CARDINALITY
            | DB_UNIQUE
            | DB_IS_COMPONENT
            | DB_INDEX
            | DB_NO_HISTORY
            | crate::DB_FULLTEXT
            | DB_TUPLE_TYPE
            | DB_TUPLE_TYPES
            | DB_TUPLE_ATTRS
            | DB_ATTR_PREDS
            | DB_TUPLE_DISCONTINUED
    )
}

fn attribute_property_changed(
    current: &crate::Attribute,
    proposed: &crate::Attribute,
    attribute: u32,
) -> bool {
    match u64::from(attribute) {
        DB_IDENT => current.ident != proposed.ident,
        DB_VALUE_TYPE => current.value_type != proposed.value_type,
        DB_CARDINALITY => current.cardinality != proposed.cardinality,
        DB_UNIQUE => current.unique != proposed.unique,
        DB_IS_COMPONENT => current.component != proposed.component,
        DB_INDEX => current.indexed != proposed.indexed,
        DB_NO_HISTORY => current.no_history != proposed.no_history,
        crate::DB_FULLTEXT => current.fulltext != proposed.fulltext,
        DB_TUPLE_TYPE | DB_TUPLE_TYPES | DB_TUPLE_ATTRS => current.tuple != proposed.tuple,
        DB_ATTR_PREDS => current.predicates != proposed.predicates,
        DB_TUPLE_DISCONTINUED => current.tuple_discontinued != proposed.tuple_discontinued,
        _ => false,
    }
}

fn same_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.stored_eq(&right.value)
}

fn resolve_tempids(
    reader: &mut Reader<'_>,
    ops: &[TxOp],
    source: &EntityRef,
    tx: u64,
    allocation_start: u64,
    default_partition: u32,
    reserved: &mut Option<crate::reserved_allocation::ReservedAllocation>,
) -> Result<(BTreeMap<String, u64>, u64), SemanticError> {
    let names = validated_entity_tempids(ops, source)?;
    let base = reader.base;
    let policy = crate::partitions::PartitionPolicy::with_default(
        ops,
        &names,
        base.schema(),
        default_partition,
        |entity| resolve_entity(reader, entity, tx, &BTreeMap::new()),
    )?;
    let names = names.into_iter().collect::<Vec<_>>();
    let positions = names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut union = UnionFind::new(names.len());
    let mut identities = Vec::new();
    for op in ops {
        let TxOp::Add {
            entity: EntityRef::Temp(name),
            attribute,
            value,
        } = op
        else {
            continue;
        };
        let descriptor = reader.base.schema().attribute(*attribute)?;
        let Some(unique) = descriptor.unique else {
            continue;
        };
        if unique == Unique::Identity {
            count_work(|work| &mut work.identity_claims, 1);
        }
        let value = resolve_upsert_identity_value(reader, *attribute, value, tx)?;
        if value.is_nan() {
            return Err(SemanticError::incorrect(
                "transaction/nan-cannot-identify",
                "NaN cannot participate in upsert or uniqueness",
            ));
        }
        identities.push((positions[name], *attribute, value, unique));
    }
    union_identity_assertions(&mut union, &identities);

    let existing_by_root =
        resolve_identity_dependencies(reader, &identities, &positions, &names, &mut union)?;

    let mut partitions = BTreeMap::new();
    for (index, name) in names.iter().enumerate() {
        let root = union.root(index);
        if existing_by_root.contains_key(&root) {
            continue;
        }
        if let Some(partition) = policy.explicit_partition(name)?
            && partitions
                .insert(root, partition)
                .is_some_and(|previous| previous != partition)
        {
            return Err(SemanticError::conflict(
                "transaction/partition-conflict",
                "unified tempids request distinct partitions",
            ));
        }
    }
    let mut next = allocation_start;
    let mut allocated_by_root = BTreeMap::new();
    let mut result = BTreeMap::new();
    for (index, name) in names.iter().enumerate() {
        let root = union.root(index);
        let entity = if let Some(existing) = existing_by_root.get(&root) {
            *existing
        } else if let Some(allocated) = allocated_by_root.get(&root) {
            *allocated
        } else {
            let partition = partitions.get(&root).copied().unwrap_or(default_partition);
            let allocated = if partition == TX_PARTITION {
                tx
            } else if partition == crate::DB_PARTITION && reserved.is_some() {
                reserved
                    .as_mut()
                    .expect("reserved mode checked")
                    .allocate()?
            } else {
                let allocated = make_eid(partition, next)?;
                next = next.checked_add(1).ok_or_else(|| {
                    SemanticError::incorrect(
                        "transaction/entity-id-overflow",
                        "tempid allocation exhausted the entity-index space",
                    )
                })?;
                allocated
            };
            allocated_by_root.insert(root, allocated);
            allocated
        };
        result.insert(name.clone(), entity);
    }
    if let Some(reserved) = reserved {
        next = next.max(reserved.frontier());
    }
    validate_frontier(next)?;
    Ok((result, next))
}

/// Root unification and newly known existing identities wake only claims that
/// refer to affected names. Symbolic root names decrease monotonically; binding
/// happens once. Each fully resolved claim performs one charged db-before lookup.
fn resolve_identity_dependencies(
    reader: &mut Reader<'_>,
    identities: &[(usize, u32, UpsertIdentityValue, Unique)],
    positions: &BTreeMap<String, usize>,
    names: &[String],
    union: &mut UnionFind,
) -> Result<BTreeMap<usize, u64>, SemanticError> {
    let mut dependents = vec![Vec::new(); positions.len()];
    for (index, (_, _, value, _)) in identities.iter().enumerate() {
        value.visit_temp_refs(&mut |name| dependents[positions[name]].push(index));
    }
    let mut pending = (0..identities.len()).collect::<BTreeSet<_>>();
    let mut previous_keys: Vec<Option<UpsertIdentityValue>> = vec![None; identities.len()];
    let mut bindings = vec![None; positions.len()];
    let mut identity_keys: BTreeMap<IdentityClaimKey, BTreeSet<usize>> = BTreeMap::new();
    while let Some(index) = pending.pop_first() {
        let (temp, attribute, key, unique) = &identities[index];
        let key = key.canonical_refs(&mut |name| {
            let root = union.root(positions[name]);
            match bindings[root] {
                Some(entity) => UpsertIdentityValue::Resolved(Value::Ref(entity)),
                None => UpsertIdentityValue::TempRef(names[root].clone()),
            }
        });
        if previous_keys[index]
            .as_ref()
            .is_some_and(|prior| prior.same_key(&key))
        {
            continue;
        }
        if let Some(prior) = previous_keys[index].replace(key.clone())
            && *unique == Unique::Identity
        {
            count_work(|work| &mut work.identity_claims, 1);
            let prior = IdentityClaimKey(*attribute, prior);
            if let Some(group) = identity_keys.get_mut(&prior) {
                group.remove(&index);
                if group.is_empty() {
                    identity_keys.remove(&prior);
                }
            }
        }
        if *unique == Unique::Identity {
            // Reuse the initial identity grouping's comparison for symbolic
            // roots as well as scalar/tuple values, independent of source.
            let group = identity_keys
                .entry(IdentityClaimKey(*attribute, key.clone()))
                .or_default();
            let other = group.first().copied();
            group.insert(index);
            if let Some(other) = other {
                let left = union.root(*temp);
                let right = union.root(identities[other].0);
                if left != right {
                    match (bindings[left], bindings[right]) {
                        (Some(existing), _) => bind_identity_group(
                            union,
                            right,
                            existing,
                            &mut bindings,
                            &dependents,
                            &mut pending,
                        )?,
                        (_, Some(existing)) => bind_identity_group(
                            union,
                            left,
                            existing,
                            &mut bindings,
                            &dependents,
                            &mut pending,
                        )?,
                        (None, None) => {
                            // Only the absorbed group's symbolic name changes.
                            // Linked membership avoids copying the kept group.
                            for member in union.members(left.max(right)) {
                                pending.extend(dependents[member].iter().copied());
                            }
                        }
                    }
                    union.join(left, right);
                }
            }
        }
        let Some(value) = key.resolved() else {
            continue;
        };
        if let Some(existing) = reader.lookup(*attribute, value)? {
            if *unique == Unique::Value {
                return Err(SemanticError::conflict(
                    "transaction/unique-value-conflict",
                    format!("unique value is already held by entity {existing}"),
                ));
            }
            count_work(|work| &mut work.upsert_resolutions, 1);
            let root = union.root(*temp);
            bind_identity_group(
                union,
                root,
                existing,
                &mut bindings,
                &dependents,
                &mut pending,
            )?;
        }
    }
    Ok(bindings
        .into_iter()
        .enumerate()
        .filter_map(|(index, binding)| {
            binding
                .filter(|_| union.root(index) == index)
                .map(|entity| (index, entity))
        })
        .collect())
}

#[derive(Clone)]
struct IdentityClaimKey(u32, UpsertIdentityValue);

impl Ord for IdentityClaimKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .cmp(&other.0)
            .then_with(|| compare_upsert_identity_values(&self.1, &other.1))
    }
}

impl PartialOrd for IdentityClaimKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for IdentityClaimKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for IdentityClaimKey {}

fn bind_identity_group(
    union: &UnionFind,
    root: usize,
    existing: u64,
    bindings: &mut [Option<u64>],
    dependents: &[Vec<usize>],
    pending: &mut BTreeSet<usize>,
) -> Result<(), SemanticError> {
    if let Some(previous) = bindings[root] {
        if previous != existing {
            return Err(SemanticError::conflict(
                "transaction/upsert-conflict",
                format!("one tempid resolves to both {previous} and {existing}"),
            ));
        }
        return Ok(());
    }
    // Membership chains concatenate in O(1) when groups join. Only an unbound
    // group is visited, so each name notifies its dependents at most once.
    for member in union.members(root) {
        bindings[member] = Some(existing);
        pending.extend(dependents[member].iter().copied());
    }
    Ok(())
}

/// Group equivalent identity assertions in index-key order before replacing
/// tempids. Sorting preserves keyed grouping and deterministic
/// lowest-index representatives without the former all-pairs walk. Key-sort
/// comparisons alone do not bound the union-find's parent traversal.
fn union_identity_assertions(
    union: &mut UnionFind,
    identities: &[(usize, u32, UpsertIdentityValue, Unique)],
) {
    let mut ordered = identities
        .iter()
        .enumerate()
        .filter_map(|(index, (_, _, _, unique))| (*unique == Unique::Identity).then_some(index))
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let (_, left_attribute, left_value, _) = &identities[*left];
        let (_, right_attribute, right_value, _) = &identities[*right];
        left_attribute
            .cmp(right_attribute)
            .then_with(|| compare_upsert_identity_values(left_value, right_value))
    });
    for pair in ordered.windows(2) {
        let (left_temp, left_attribute, left_value, _) = &identities[pair[0]];
        let (right_temp, right_attribute, right_value, _) = &identities[pair[1]];
        if left_attribute == right_attribute && left_value.same_key(right_value) {
            union.join(*left_temp, *right_temp);
        }
    }
}

fn compare_upsert_identity_values(
    left: &UpsertIdentityValue,
    right: &UpsertIdentityValue,
) -> Ordering {
    match (left, right) {
        (UpsertIdentityValue::Resolved(left), UpsertIdentityValue::Resolved(right)) => {
            left.index_cmp(right)
        }
        (UpsertIdentityValue::TempRef(left), UpsertIdentityValue::TempRef(right)) => {
            left.cmp(right)
        }
        (UpsertIdentityValue::Resolved(_), UpsertIdentityValue::TempRef(_)) => Ordering::Less,
        (UpsertIdentityValue::TempRef(_), UpsertIdentityValue::Resolved(_)) => Ordering::Greater,
        (UpsertIdentityValue::Tuple(left), UpsertIdentityValue::Tuple(right)) => {
            for (left, right) in left.iter().zip(right) {
                let compared = match (left, right) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (Some(left), Some(right)) => compare_upsert_identity_values(left, right),
                };
                if !compared.is_eq() {
                    return compared;
                }
            }
            left.len().cmp(&right.len())
        }
        (UpsertIdentityValue::Tuple(_), _) => Ordering::Greater,
        (_, UpsertIdentityValue::Tuple(_)) => Ordering::Less,
    }
}

fn resolve_upsert_identity_value(
    reader: &mut Reader<'_>,
    attribute: u32,
    value: &TxValue,
    tx: u64,
) -> Result<UpsertIdentityValue, SemanticError> {
    let descriptor = reader.base.schema().attribute(attribute)?;
    match value {
        TxValue::Tuple(slots) => {
            for value in slots.iter().flatten() {
                if let TxValue::Scalar(value) = value {
                    validate_explicit_value_refs(reader.base, value)?;
                }
            }
            let schema = reader.base.schema_arc();
            crate::transaction::input::resolve_tuple_input(
                &schema,
                schema.attribute(attribute)?,
                slots,
                |entity| match entity {
                    EntityRef::Temp(name) => Ok(UpsertIdentityValue::TempRef(name.clone())),
                    entity => resolve_entity(reader, entity, tx, &BTreeMap::new())
                        .map(|entity| UpsertIdentityValue::Resolved(Value::Ref(entity))),
                },
            )
        }
        TxValue::Scalar(value) => {
            validate_explicit_value_refs(reader.base, value)?;
            reader.base.schema().validate_value(descriptor, value)?;
            Ok(UpsertIdentityValue::Resolved(value.clone()))
        }
        TxValue::Entity(EntityRef::Temp(name)) if descriptor.value_type == ValueType::Ref => {
            Ok(UpsertIdentityValue::TempRef(name.clone()))
        }
        TxValue::Entity(entity) if descriptor.value_type == ValueType::Ref => {
            let value = Value::Ref(resolve_entity(reader, entity, tx, &BTreeMap::new())?);
            reader.base.schema().validate_value(descriptor, &value)?;
            Ok(UpsertIdentityValue::Resolved(value))
        }
        TxValue::Entity(_) => Err(SemanticError::incorrect(
            "transaction/value-type",
            format!(
                "attribute {} requires {:?}",
                descriptor.ident.qualified_name(),
                descriptor.value_type
            ),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_op(
    reader: &mut Reader<'_>,
    op: &TxOp,
    tx: u64,
    tempids: &BTreeMap<String, u64>,
    logical: &mut Vec<LogicalDatom>,
    ensures: &mut Vec<EnsureRequirement>,
    touched: &mut BTreeSet<(u64, u32)>,
) -> Result<(), SemanticError> {
    match op {
        TxOp::Add {
            entity,
            attribute,
            value,
        } => {
            let entity = resolve_entity(reader, entity, tx, tempids)?;
            let value = resolve_value(reader, *attribute, value, tx, tempids)?;
            validate_entity_predicate_name(*attribute, &value)?;
            if u64::from(*attribute) == DB_ENSURE {
                let Value::Ref(spec) = value else {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-ensure",
                        ":db/ensure must name an entity spec",
                    ));
                };
                ensures.push(resolve_entity_spec(reader, entity, spec)?);
            } else if !is_derived_composite(reader.base.schema(), *attribute)? {
                touched.insert((entity, *attribute));
                logical.push(LogicalDatom {
                    entity,
                    attribute: *attribute,
                    value,
                    added: true,
                });
            }
        }
        TxOp::Retract {
            entity,
            attribute,
            value,
        } => {
            let entity = resolve_entity(reader, entity, tx, tempids)?;
            reader.base.schema().attribute(*attribute)?;
            if u64::from(*attribute) == DB_ENSURE {
                return Err(SemanticError::incorrect(
                    "transaction/virtual-ensure",
                    ":db/ensure is virtual and cannot be retracted",
                ));
            }
            if is_derived_composite(reader.base.schema(), *attribute)? {
                if let Some(value) = value {
                    let _ = resolve_value(reader, *attribute, value, tx, tempids)?;
                }
            } else {
                touched.insert((entity, *attribute));
                if let Some(value) = value {
                    logical.push(LogicalDatom {
                        entity,
                        attribute: *attribute,
                        value: resolve_value(reader, *attribute, value, tx, tempids)?,
                        added: false,
                    });
                } else {
                    for datom in reader.prefix(&IndexPrefix::Eavt {
                        entity,
                        attribute: Some(*attribute),
                        value: None,
                    })? {
                        logical.push(LogicalDatom {
                            entity,
                            attribute: *attribute,
                            value: datom.value,
                            added: false,
                        });
                    }
                }
            }
        }
        TxOp::Cas {
            entity,
            attribute,
            old,
            new,
        } => {
            let entity = resolve_entity(reader, entity, tx, tempids)?;
            let descriptor = reader.base.schema().attribute(*attribute)?;
            if u64::from(*attribute) == DB_ENSURE {
                return Err(SemanticError::incorrect(
                    "transaction/virtual-ensure",
                    ":db/ensure is virtual and cannot be compared and swapped",
                ));
            }
            if descriptor.cardinality != Cardinality::One {
                return Err(SemanticError::incorrect(
                    "transaction/cas-requires-cardinality-one",
                    "compare-and-swap requires a cardinality-one attribute",
                ));
            }
            let observed = reader.values(entity, *attribute)?;
            let expected = old
                .as_ref()
                .map(|value| resolve_value(reader, *attribute, value, tx, tempids))
                .transpose()?;
            let matches = match (expected.as_ref(), observed.as_slice()) {
                (None, []) => true,
                (Some(expected), [observed]) => observed.stored_eq(expected),
                _ => false,
            };
            if !matches {
                return Err(SemanticError::new(
                    ErrorCategory::Conflict,
                    "transaction/cas-failed",
                    "compare-and-swap did not match db-before",
                ));
            }
            let value = resolve_value(reader, *attribute, new, tx, tempids)?;
            validate_entity_predicate_name(*attribute, &value)?;
            if !is_derived_composite(reader.base.schema(), *attribute)? {
                touched.insert((entity, *attribute));
                logical.push(LogicalDatom {
                    entity,
                    attribute: *attribute,
                    value,
                    added: true,
                });
            }
        }
        TxOp::RetractEntity(entity) => {
            let entity = resolve_entity(reader, entity, tx, tempids)?;
            if eid_to_part(entity)? == TX_PARTITION {
                return Err(SemanticError::incorrect(
                    "transaction/reset-tx-instant",
                    "transaction entities cannot be retracted",
                ));
            }
            expand_retract_entity(reader, entity, logical, touched, &mut BTreeSet::new())?;
        }
        TxOp::Ensure { entity, spec } => {
            let entity = resolve_entity(reader, entity, tx, tempids)?;
            let spec = resolve_entity(reader, spec, tx, tempids)?;
            ensures.push(resolve_entity_spec(reader, entity, spec)?);
        }
        TxOp::InstallAttribute(_)
        | TxOp::AlterAttribute(_)
        | TxOp::ForcePartition { .. }
        | TxOp::MatchPartition { .. } => {}
    }
    Ok(())
}

fn expand_retract_entity(
    reader: &mut Reader<'_>,
    entity: u64,
    logical: &mut Vec<LogicalDatom>,
    touched: &mut BTreeSet<(u64, u32)>,
    visited: &mut BTreeSet<u64>,
) -> Result<(), SemanticError> {
    // Explicit continuations preserve depth-first read order,
    // including ordinary Reader read/byte admission checks at every prefix.
    // A visited set alone stops cycles but cannot make deep paths stack-safe.
    enum Work {
        Visit(u64),
        Fact(Datom),
        Retract(Datom),
        Incoming(u64),
    }
    let mut pending = vec![Work::Visit(entity)];
    while let Some(work) = pending.pop() {
        match work {
            Work::Visit(entity) => {
                if !visited.insert(entity) {
                    continue;
                }
                let facts = reader.prefix(&IndexPrefix::Eavt {
                    entity,
                    attribute: None,
                    value: None,
                })?;
                pending.push(Work::Incoming(entity));
                pending.extend(facts.into_iter().rev().map(Work::Fact));
            }
            Work::Fact(fact) => {
                let component = reader.base.schema().attribute(fact.attribute)?.component;
                let child = match &fact.value {
                    Value::Ref(child) if component => Some(*child),
                    _ => None,
                };
                pending.push(Work::Retract(fact));
                if let Some(child) = child {
                    pending.push(Work::Visit(child));
                }
            }
            Work::Retract(fact) => {
                touched.insert((fact.entity, fact.attribute));
                logical.push(LogicalDatom {
                    entity: fact.entity,
                    attribute: fact.attribute,
                    value: fact.value,
                    added: false,
                });
            }
            Work::Incoming(entity) => {
                for fact in reader.prefix(&IndexPrefix::Vaet {
                    value: Value::Ref(entity),
                    attribute: None,
                    entity: None,
                })? {
                    touched.insert((fact.entity, fact.attribute));
                    logical.push(LogicalDatom {
                        entity: fact.entity,
                        attribute: fact.attribute,
                        value: fact.value,
                        added: false,
                    });
                }
            }
        }
    }
    Ok(())
}

fn resolve_entity(
    reader: &mut Reader<'_>,
    entity: &EntityRef,
    tx: u64,
    tempids: &BTreeMap<String, u64>,
) -> Result<u64, SemanticError> {
    match entity {
        EntityRef::Id(entity) => {
            validate_explicit_entity_id(reader.base, *entity)?;
            Ok(*entity)
        }
        EntityRef::Ident(ident) => reader.base.entid(ident).ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/unknown-ident",
                format!("unknown ident {}", ident.qualified_name()),
            )
        }),
        EntityRef::Temp(name) => tempids.get(name).copied().ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/unknown-tempid",
                format!("unknown tempid {name}"),
            )
        }),
        EntityRef::Lookup { attribute, value } => {
            reader.lookup(*attribute, value)?.ok_or_else(|| {
                SemanticError::incorrect(
                    "transaction/lookup-not-found",
                    "lookup ref did not resolve in db-before",
                )
            })
        }
        EntityRef::LookupInput { attribute, value } => reader
            .base
            .clone()
            .resolve_lookup_input_with(*attribute, value, &mut |attribute, value| {
                reader.lookup(attribute, value)
            })?
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "transaction/lookup-not-found",
                    "lookup ref did not resolve in db-before",
                )
            }),
        EntityRef::Tx => Ok(tx),
        EntityRef::Edn(text) => {
            let lowered = crate::edn_transaction::lower_entity_ref(reader.base, text)?;
            resolve_entity(reader, &lowered, tx, tempids)
        }
    }
}

fn validate_explicit_entity_id(base: &DatabaseValue, entity: u64) -> Result<(), SemanticError> {
    validate_supported_eid(entity)?;
    base.schema()
        .validate_partition_bits(eid_to_part(entity)?)?;
    let eidx = eid_to_eidx(entity)?;
    if eidx >= base.eidx_frontier() {
        return Err(SemanticError::incorrect(
            "transaction/invalid-entity-id",
            format!(
                "entity id {entity} has unissued index {eidx}; issued indexes are below {}",
                base.eidx_frontier()
            ),
        ));
    }
    Ok(())
}

fn resolve_value(
    reader: &mut Reader<'_>,
    attribute: u32,
    value: &TxValue,
    tx: u64,
    tempids: &BTreeMap<String, u64>,
) -> Result<Value, SemanticError> {
    let descriptor = reader.base.schema().attribute(attribute)?;
    let value = match (descriptor.value_type, value) {
        (ValueType::Tuple, TxValue::Tuple(slots)) => {
            for value in slots.iter().flatten() {
                if let TxValue::Scalar(value) = value {
                    validate_explicit_value_refs(reader.base, value)?;
                }
            }
            let schema = reader.base.schema_arc();
            crate::transaction::input::resolve_tuple_input(
                &schema,
                schema.attribute(attribute)?,
                slots,
                |entity| {
                    resolve_entity(reader, entity, tx, tempids)
                        .map(|entity| UpsertIdentityValue::Resolved(Value::Ref(entity)))
                },
            )?
            .resolved()
            .expect("allocated tuple has no symbolic references")
            .clone()
        }
        (ValueType::Ref, TxValue::Entity(entity)) => {
            Value::Ref(resolve_entity(reader, entity, tx, tempids)?)
        }
        (_, TxValue::Scalar(value)) => {
            validate_explicit_value_refs(reader.base, value)?;
            value.clone()
        }
        _ => {
            return Err(SemanticError::incorrect(
                "transaction/value-type",
                format!(
                    "attribute {} requires {:?}",
                    descriptor.ident.qualified_name(),
                    descriptor.value_type
                ),
            ));
        }
    };
    reader.base.schema().validate_value(descriptor, &value)?;
    Ok(value)
}

fn validate_explicit_value_refs(base: &DatabaseValue, value: &Value) -> Result<(), SemanticError> {
    match value {
        Value::Ref(entity) => validate_explicit_entity_id(base, *entity),
        Value::Tuple(slots) => {
            for value in slots.iter().flatten() {
                validate_explicit_value_refs(base, value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn resolve_entity_spec(
    reader: &mut Reader<'_>,
    entity: u64,
    spec: u64,
) -> Result<EnsureRequirement, SemanticError> {
    let mut required = Vec::new();
    for value in reader.values(spec, DB_ENTITY_ATTRS as u32)? {
        let Value::Keyword(ident) = value else {
            return Err(SemanticError::incorrect(
                "transaction/invalid-entity-spec",
                ":db.entity/attrs values must be attribute keywords",
            ));
        };
        let attribute = reader
            .base
            .entid(&ident)
            .and_then(|entity| crate::schema_eid_to_attr_id(entity).ok())
            .filter(|attribute| reader.base.schema().attribute(*attribute).is_ok())
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "transaction/unknown-spec-attribute",
                    format!(
                        "entity spec names unknown attribute {}",
                        ident.qualified_name()
                    ),
                )
            })?;
        required.push(attribute);
    }
    required.sort_unstable();
    required.dedup();
    let mut predicates = Vec::new();
    for value in reader.values(spec, DB_ENTITY_PREDS as u32)? {
        let Value::Symbol(symbol) = value else {
            return Err(SemanticError::incorrect(
                "transaction/invalid-entity-spec",
                ":db.entity/preds values must be symbols",
            ));
        };
        if symbol
            .namespace
            .as_deref()
            .is_none_or(|namespace| namespace.is_empty() || namespace.contains('/'))
            || symbol.name.is_empty()
            || symbol.name.contains('/')
        {
            return Err(SemanticError::incorrect(
                "transaction/unqualified-entity-predicate",
                ":db.entity/preds values must be fully qualified symbols",
            ));
        }
        predicates.push(symbol.qualified_name());
    }
    predicates.sort();
    predicates.dedup();
    Ok(EnsureRequirement {
        entity,
        spec,
        required,
        predicates,
    })
}

fn add_cardinality_one_retractions(
    reader: &mut Reader<'_>,
    logical: &mut Vec<LogicalDatom>,
) -> Result<(), SemanticError> {
    let additions = logical
        .iter()
        .filter(|datom| datom.added)
        .cloned()
        .collect::<Vec<_>>();
    for addition in additions {
        if reader
            .base
            .schema()
            .attribute(addition.attribute)?
            .cardinality
            != Cardinality::One
        {
            continue;
        }
        for existing in reader.prefix(&IndexPrefix::Eavt {
            entity: addition.entity,
            attribute: Some(addition.attribute),
            value: None,
        })? {
            if !existing.value.stored_eq(&addition.value) {
                logical.push(LogicalDatom {
                    entity: existing.entity,
                    attribute: existing.attribute,
                    value: existing.value,
                    added: false,
                });
            }
        }
    }
    Ok(())
}

fn derive_composites(
    reader: &mut Reader<'_>,
    logical: &mut Vec<LogicalDatom>,
    touched: &BTreeSet<(u64, u32)>,
) -> Result<(), SemanticError> {
    // Recovered `Db` maintains a constituent -> composite reverse map and its
    // transaction prefetcher consults that map for each touched attribute
    // (`composites-prefetcher` / `create-composite`). Preserve that shape:
    // schema size must not become ordinary transaction work.
    let mut composite_ids = BTreeSet::new();
    for (_, attribute) in touched {
        reader.work.dependency_lookups = reader.work.dependency_lookups.saturating_add(1);
        for composite in reader.base.schema().composites_for_constituent(*attribute) {
            reader.work.dependency_edges = reader.work.dependency_edges.saturating_add(1);
            composite_ids.insert(composite);
        }
    }
    reader.work.composite_candidates = reader
        .work
        .composite_candidates
        .saturating_add(composite_ids.len() as u64);
    for composite_id in composite_ids {
        let composite = reader.base.schema().attribute(composite_id)?.clone();
        let TupleSpec::Composite(constituents) = composite.tuple.as_ref().unwrap() else {
            unreachable!()
        };
        let entities = touched
            .iter()
            .filter(|(_, attribute)| constituents.contains(attribute))
            .map(|(entity, _)| *entity)
            .collect::<BTreeSet<_>>();
        for entity in entities {
            count_work(|work| &mut work.composite_candidates, 1);
            let before_count = logical.len();
            let old = reader.values(entity, composite.id)?.into_iter().next();
            let mut slots = Vec::with_capacity(constituents.len());
            for constituent in constituents {
                slots.push(
                    successor_values(reader, logical, entity, *constituent)?
                        .into_iter()
                        .next(),
                );
            }
            let new = (!slots.iter().all(Option::is_none)).then_some(Value::Tuple(slots));
            if let Some(old) = &old
                && new.as_ref().is_none_or(|new| !old.stored_eq(new))
            {
                logical.push(LogicalDatom {
                    entity,
                    attribute: composite.id,
                    value: old.clone(),
                    added: false,
                });
            }
            // Touching a constituent supplies the resulting composite under
            // this transaction's source even when its value is unchanged.
            // material_changes suppresses an existing contribution from the
            // same source along with the redundant constituent assertion.
            if let Some(new) = new {
                reader.base.schema().validate_value(&composite, &new)?;
                logical.push(LogicalDatom {
                    entity,
                    attribute: composite.id,
                    value: new,
                    added: true,
                });
            }
            count_work(
                |work| &mut work.composite_datoms,
                logical.len() - before_count,
            );
        }
    }
    Ok(())
}

fn validate_delta_successor(
    reader: &mut Reader<'_>,
    successor_schema: &Schema,
    logical: &[LogicalDatom],
) -> Result<(), SemanticError> {
    let mut touched_ea = BTreeMap::<(u64, u32), Vec<&LogicalDatom>>::new();
    let mut unique_indices = Vec::new();
    let mut excision_entities = BTreeSet::new();
    for (index, datom) in logical.iter().enumerate() {
        touched_ea
            .entry((datom.entity, datom.attribute))
            .or_default()
            .push(datom);
        let descriptor = successor_schema.attribute(datom.attribute)?;
        if descriptor.unique.is_some() {
            unique_indices.push(index);
        }
        if matches!(
            u64::from(datom.attribute),
            DB_EXCISE | DB_EXCISE_BEFORE_T | DB_EXCISE_BEFORE
        ) {
            excision_entities.insert(datom.entity);
        }
    }
    for (&(entity, attribute), deltas) in &touched_ea {
        let descriptor = successor_schema.attribute(attribute)?;
        let values = successor_values_from_deltas(reader, entity, attribute, deltas)?;
        if descriptor.cardinality == Cardinality::One && values.len() > 1 {
            return Err(SemanticError::conflict(
                "transaction/cardinality-one-conflict",
                "resulting database has multiple cardinality-one values",
            )
            .detail("entity", entity.to_string())
            .detail("attribute", attribute.to_string()));
        }
    }
    let unique_groups = group_unique_deltas(logical, unique_indices);
    for group in unique_groups {
        count_work(|work| &mut work.uniqueness_checks, 1);
        if group.value.is_nan() {
            return Err(SemanticError::incorrect(
                "transaction/nan-cannot-identify",
                "NaN cannot participate in uniqueness",
            ));
        }
        if successor_owner_count(
            reader,
            group.attribute,
            group.value,
            group.deltas.as_slice(),
        )? > 1
        {
            return Err(SemanticError::conflict(
                "transaction/unique-conflict",
                "resulting database has multiple holders of a unique value",
            ));
        }
    }
    for entity in excision_entities {
        // Cutoff attributes are ordinary information until this entity
        // actually retains an A=15 excision request in the successor. This
        // matches eager `validate_excision_requests` and avoids rejecting an
        // entity which merely edits or retracts request metadata.
        if successor_values_from_deltas(
            reader,
            entity,
            DB_EXCISE as u32,
            touched_ea
                .get(&(entity, DB_EXCISE as u32))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?
        .is_empty()
        {
            continue;
        }
        let before_t = successor_values_from_deltas(
            reader,
            entity,
            DB_EXCISE_BEFORE_T as u32,
            touched_ea
                .get(&(entity, DB_EXCISE_BEFORE_T as u32))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
        let before = successor_values_from_deltas(
            reader,
            entity,
            DB_EXCISE_BEFORE as u32,
            touched_ea
                .get(&(entity, DB_EXCISE_BEFORE as u32))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
        if !before_t.is_empty() && !before.is_empty() {
            return Err(SemanticError::incorrect(
                "transaction/excision-cutoff-conflict",
                "an excision request may use at most one cutoff",
            ));
        }
        if let Some(value) = before_t.first() {
            let Value::Long(value) = value else {
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

struct UniqueDeltaGroup<'a> {
    attribute: u32,
    value: &'a Value,
    first_index: usize,
    deltas: Vec<&'a LogicalDatom>,
}

fn group_unique_deltas(
    logical: &[LogicalDatom],
    mut indices: Vec<usize>,
) -> Vec<UniqueDeltaGroup<'_>> {
    indices.sort_by(|left, right| {
        logical[*left]
            .attribute
            .cmp(&logical[*right].attribute)
            .then_with(|| logical[*left].value.index_cmp(&logical[*right].value))
            .then(left.cmp(right))
    });
    let mut groups = Vec::new();
    for indices in equal_groups(&indices, |left, right| {
        logical[*left].attribute == logical[*right].attribute
            && logical[*left]
                .value
                .index_cmp(&logical[*right].value)
                .is_eq()
    }) {
        groups.push(UniqueDeltaGroup {
            attribute: logical[indices[0]].attribute,
            value: &logical[indices[0]].value,
            first_index: indices[0],
            deltas: indices.iter().map(|index| &logical[*index]).collect(),
        });
    }
    // The former touched-A/V vector validated keys at their first occurrence
    // in canonical logical order. Preserve that failure precedence even
    // though grouping itself uses the source-shaped A/V key order.
    groups.sort_by_key(|group| group.first_index);
    groups
}

fn validate_ensure_attributes(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    ensures: &[EnsureRequirement],
) -> Result<(), SemanticError> {
    for ensure in ensures {
        let mut missing = Vec::new();
        for attribute in &ensure.required {
            if successor_values(reader, logical, ensure.entity, *attribute)?.is_empty() {
                missing.push(*attribute);
            }
        }
        if !missing.is_empty() {
            return Err(SemanticError::incorrect(
                "transaction/entity-spec",
                format!(
                    "entity {} is missing attributes {missing:?} of spec {}",
                    ensure.entity, ensure.spec
                ),
            ));
        }
    }
    Ok(())
}

fn successor_values(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    entity: u64,
    attribute: u32,
) -> Result<Vec<Value>, SemanticError> {
    let deltas = logical
        .iter()
        .filter(|datom| datom.entity == entity && datom.attribute == attribute)
        .collect::<Vec<_>>();
    successor_values_from_deltas(reader, entity, attribute, &deltas)
}

fn successor_values_from_deltas(
    reader: &mut Reader<'_>,
    entity: u64,
    attribute: u32,
    deltas: &[&LogicalDatom],
) -> Result<Vec<Value>, SemanticError> {
    let values = reader.values(entity, attribute)?;
    let mut deltas = deltas.to_vec();
    deltas.sort_by(|left, right| left.value.stored_cmp(&right.value));
    let mut result = Vec::with_capacity(values.len().saturating_add(deltas.len()));
    let (mut value_index, mut delta_index) = (0, 0);
    while value_index < values.len() || delta_index < deltas.len() {
        match (values.get(value_index), deltas.get(delta_index)) {
            (Some(value), Some(delta)) => match value.stored_cmp(&delta.value) {
                Ordering::Less => {
                    result.push(value.clone());
                    value_index += 1;
                }
                Ordering::Equal => {
                    if delta.added {
                        result.push(value.clone());
                    }
                    value_index += 1;
                    delta_index += 1;
                }
                Ordering::Greater => {
                    if delta.added {
                        result.push(delta.value.clone());
                    }
                    delta_index += 1;
                }
            },
            (Some(value), None) => {
                result.push(value.clone());
                value_index += 1;
            }
            (None, Some(delta)) => {
                if delta.added {
                    result.push(delta.value.clone());
                }
                delta_index += 1;
            }
            (None, None) => break,
        }
    }
    Ok(result)
}

fn successor_owner_count(
    reader: &mut Reader<'_>,
    attribute: u32,
    value: &Value,
    deltas: &[&LogicalDatom],
) -> Result<usize, SemanticError> {
    let mut facts = reader.prefix(&IndexPrefix::Avet {
        attribute,
        value: Some(value.clone()),
        entity: None,
    })?;
    facts.sort_by(|left, right| {
        left.entity
            .cmp(&right.entity)
            .then_with(|| left.value.stored_cmp(&right.value))
    });
    facts.dedup_by(|right, left| same_eav(left, right));
    let mut counts = BTreeMap::<u64, usize>::new();
    for fact in &facts {
        *counts.entry(fact.entity).or_default() += 1;
    }
    for delta in deltas {
        let present = facts
            .binary_search_by(|fact| {
                fact.entity
                    .cmp(&delta.entity)
                    .then_with(|| fact.value.stored_cmp(&delta.value))
            })
            .is_ok();
        match (delta.added, present) {
            (true, false) => *counts.entry(delta.entity).or_default() += 1,
            (false, true) => {
                if let Some(count) = counts.get_mut(&delta.entity) {
                    *count -= 1;
                }
            }
            _ => {}
        }
    }
    Ok(counts.values().filter(|count| **count != 0).count())
}

fn material_changes(
    reader: &mut Reader<'_>,
    logical: &[LogicalDatom],
    tx: u64,
    source: u64,
) -> Result<Vec<Datom>, SemanticError> {
    let mut result = Vec::new();
    for datom in logical {
        count_work(|work| &mut work.redundancy_checks, 1);
        let existed = if datom.added {
            reader.contains_source(datom.entity, datom.attribute, &datom.value, source)?
        } else {
            reader.contains(datom.entity, datom.attribute, &datom.value)?
        };
        if existed != datom.added
            || (datom.added && u64::from(datom.attribute) == DB_ALTER_ATTRIBUTE)
        {
            result.push(Datom {
                entity: datom.entity,
                attribute: datom.attribute,
                value: datom.value.clone(),
                tx,
                source,
                added: datom.added,
            });
        } else {
            count_work(|work| &mut work.redundant_datoms, 1);
        }
    }
    Ok(result)
}

fn validate_tx_instant_forms(ops: &[TxOp], selected: i64) -> Result<(), SemanticError> {
    let mut explicit = None;
    for op in ops {
        match op {
            TxOp::Add {
                entity,
                attribute,
                value,
            } if u64::from(*attribute) == DB_TX_INSTANT => {
                if !matches!(entity, EntityRef::Tx) {
                    return Err(SemanticError::incorrect(
                        "transaction/reset-tx-instant",
                        ":db/txInstant may be asserted only on the current transaction",
                    ));
                }
                let TxValue::Scalar(Value::Instant(instant)) = value else {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-tx-instant",
                        ":db/txInstant must be a scalar instant",
                    ));
                };
                if explicit
                    .replace(*instant)
                    .is_some_and(|previous| previous != *instant)
                {
                    return Err(SemanticError::incorrect(
                        "transaction/multiple-tx-instants",
                        ":db/txInstant additions must specify the same instant",
                    ));
                }
            }
            TxOp::Retract { attribute, .. } | TxOp::Cas { attribute, .. }
                if u64::from(*attribute) == DB_TX_INSTANT =>
            {
                return Err(SemanticError::incorrect(
                    "transaction/reset-tx-instant",
                    ":db/txInstant cannot be retracted or changed",
                ));
            }
            _ => {}
        }
    }
    if explicit.is_some_and(|instant| instant != selected) {
        return Err(SemanticError::incorrect(
            "transaction/tx-instant-mismatch",
            "explicit :db/txInstant differs from the transactor-selected instant",
        ));
    }
    Ok(())
}

fn validate_entity_predicate_name(attribute: u32, value: &Value) -> Result<(), SemanticError> {
    if u64::from(attribute) != DB_ENTITY_PREDS {
        return Ok(());
    }
    let Value::Symbol(symbol) = value else {
        return Ok(());
    };
    if symbol
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace.is_empty() || namespace.contains('/'))
        || symbol.name.is_empty()
        || symbol.name.contains('/')
    {
        return Err(SemanticError::incorrect(
            "transaction/unqualified-entity-predicate",
            ":db.entity/preds values must be fully qualified symbols",
        ));
    }
    Ok(())
}

fn is_derived_composite(schema: &Schema, attribute: u32) -> Result<bool, SemanticError> {
    let attribute = schema.attribute(attribute)?;
    // Discontinuation removes maintenance dependencies; the immutable
    // composite definition still makes its value database-managed.
    Ok(matches!(attribute.tuple, Some(TupleSpec::Composite(_))))
}

fn dedupe(datoms: &mut Vec<LogicalDatom>) {
    let before = datoms.len();
    // Recovered `create-deduper` is a transaction-local HashSet. Values need
    // the native port's stricter stored identity here so legal BigDecimal
    // scale variants do not collapse. Sorting by that same canonical identity
    // gives deterministic O(n log n) behavior without a second value wrapper.
    datoms.sort_by(compare_logical);
    datoms.dedup_by(|right, left| same_logical(left, right));
    count_work(|work| &mut work.duplicate_datoms, before - datoms.len());
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SameTransactionConflict {
    Datoms,
    CardinalityOne,
    Unique,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ConflictCandidate {
    left: usize,
    right: usize,
    kind: SameTransactionConflict,
}

fn validate_same_transaction(
    schema: &Schema,
    datoms: &[LogicalDatom],
) -> Result<(), SemanticError> {
    // Three deterministic sorted key views (EAV, EA/op, and AV) validate
    // transaction-local conflicts without requiring Value to implement a
    // single equality relation at both stored and logical-index boundaries.
    let mut descriptor = Vec::with_capacity(datoms.len());
    let mut first_schema_error = None;
    for (index, datom) in datoms.iter().enumerate() {
        match schema.attribute(datom.attribute) {
            Ok(attribute) => descriptor.push(Some(attribute)),
            Err(error) => {
                if first_schema_error.is_none() {
                    first_schema_error = Some((index, error));
                }
                descriptor.push(None);
            }
        }
    }

    let mut conflict = None;

    let mut exact = (0..datoms.len()).collect::<Vec<_>>();
    exact.sort_by(|left_index, right_index| {
        let left = &datoms[*left_index];
        let right = &datoms[*right_index];
        left.entity
            .cmp(&right.entity)
            .then(left.attribute.cmp(&right.attribute))
            .then_with(|| left.value.stored_cmp(&right.value))
            .then(left_index.cmp(right_index))
    });
    for group in equal_groups(&exact, |left, right| {
        let left = &datoms[*left];
        let right = &datoms[*right];
        left.entity == right.entity
            && left.attribute == right.attribute
            && left.value.stored_eq(&right.value)
    }) {
        let first = group[0];
        if let Some(right) = group
            .iter()
            .copied()
            .find(|right| datoms[*right].added != datoms[first].added)
        {
            record_conflict(
                &mut conflict,
                ConflictCandidate {
                    left: first.min(right),
                    right: first.max(right),
                    kind: SameTransactionConflict::Datoms,
                },
            );
        }
    }

    let mut cardinality_one = (0..datoms.len())
        .filter(|index| {
            datoms[*index].added
                && descriptor[*index]
                    .is_some_and(|attribute| attribute.cardinality == Cardinality::One)
        })
        .collect::<Vec<_>>();
    cardinality_one.sort_by_key(|index| {
        let datom = &datoms[*index];
        (datom.entity, datom.attribute, *index)
    });
    for group in equal_groups(&cardinality_one, |left, right| {
        datoms[*left].entity == datoms[*right].entity
            && datoms[*left].attribute == datoms[*right].attribute
    }) {
        let first = group[0];
        if let Some(right) = group
            .iter()
            .copied()
            .find(|right| datoms[first].value.index_cmp(&datoms[*right].value).is_ne())
        {
            record_conflict(
                &mut conflict,
                ConflictCandidate {
                    left: first,
                    right,
                    kind: SameTransactionConflict::CardinalityOne,
                },
            );
        }
    }

    let mut unique = (0..datoms.len())
        .filter(|index| {
            datoms[*index].added
                && descriptor[*index].is_some_and(|attribute| attribute.unique.is_some())
        })
        .collect::<Vec<_>>();
    unique.sort_by(|left, right| {
        let left_datom = &datoms[*left];
        let right_datom = &datoms[*right];
        left_datom
            .attribute
            .cmp(&right_datom.attribute)
            .then_with(|| left_datom.value.index_cmp(&right_datom.value))
            .then(left.cmp(right))
    });
    for group in equal_groups(&unique, |left, right| {
        datoms[*left].attribute == datoms[*right].attribute
            && datoms[*left].value.index_cmp(&datoms[*right].value).is_eq()
    }) {
        let first = group[0];
        if let Some(right) = group
            .iter()
            .copied()
            .find(|right| datoms[first].entity != datoms[*right].entity)
        {
            record_conflict(
                &mut conflict,
                ConflictCandidate {
                    left: first,
                    right,
                    kind: SameTransactionConflict::Unique,
                },
            );
        }
    }

    if let Some((index, error)) = first_schema_error
        && conflict.is_none_or(|conflict| index <= conflict.left)
    {
        return Err(error);
    }
    match conflict.map(|conflict| conflict.kind) {
        Some(SameTransactionConflict::Datoms) => Err(SemanticError::conflict(
            "transaction/datoms-conflict",
            "addition and retraction of the same E/A/V conflict",
        )),
        Some(SameTransactionConflict::CardinalityOne) => Err(SemanticError::conflict(
            "transaction/cardinality-one-conflict",
            "two values for one cardinality-one E/A conflict",
        )),
        Some(SameTransactionConflict::Unique) => Err(SemanticError::conflict(
            "transaction/unique-conflict",
            "two entities assert the same unique A/V",
        )),
        None => Ok(()),
    }
}

fn equal_groups<T>(
    sorted: &[T],
    mut equivalent: impl FnMut(&T, &T) -> bool,
) -> impl Iterator<Item = &[T]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == sorted.len() {
            return None;
        }
        let mut end = start + 1;
        while end < sorted.len() && equivalent(&sorted[start], &sorted[end]) {
            end += 1;
        }
        let group = &sorted[start..end];
        start = end;
        Some(group)
    })
}

fn record_conflict(current: &mut Option<ConflictCandidate>, candidate: ConflictCandidate) {
    if current.is_none_or(|current| candidate < current) {
        *current = Some(candidate);
    }
}

fn same_logical(left: &LogicalDatom, right: &LogicalDatom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.added == right.added
        && left.value.stored_eq(&right.value)
}

fn compare_logical(left: &LogicalDatom, right: &LogicalDatom) -> Ordering {
    left.entity
        .cmp(&right.entity)
        .then(left.attribute.cmp(&right.attribute))
        .then_with(|| left.value.stored_cmp(&right.value))
        .then_with(|| right.added.cmp(&left.added))
}

#[derive(Debug)]
struct UnionFind {
    parent: Vec<usize>,
    next_member: Vec<Option<usize>>,
    last_member: Vec<usize>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self {
            parent: (0..size).collect(),
            next_member: vec![None; size],
            last_member: (0..size).collect(),
        }
    }

    fn root(&mut self, mut index: usize) -> usize {
        // Keyed identity groups can connect roots in descending order and
        // create a long chain even though their keys were sorted efficiently.
        // Compress paths without changing the minimum representative chosen
        // by join: allocation order, partition checks and conflict selection
        // depend on that representative. This is a native cost repair, not a
        // claim that recovered get-ids implements union-find. Without weighted
        // union we do not claim the usual inverse-Ackermann bound.
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        while self.parent[index] != index {
            let parent = self.parent[index];
            self.parent[index] = root;
            index = parent;
        }
        root
    }

    fn join(&mut self, left: usize, right: usize) {
        let left = self.root(left);
        let right = self.root(right);
        if left != right {
            let (first, second) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            self.parent[second] = first;
            self.next_member[self.last_member[first]] = Some(second);
            self.last_member[first] = self.last_member[second];
        }
    }

    fn members(&self, root: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(Some(root), |member| self.next_member[*member])
    }
}

#[cfg(test)]
mod tests;
