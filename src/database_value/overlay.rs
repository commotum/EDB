//! Flattened, path-shared speculative successors and selective index merges.
use super::cursor::{
    DatabaseValuePrefixCursor, DatabaseValueScanCursor, DatabaseValueScanCursorInner,
    ReadCursorControl,
};
use super::read_context::{LogicalReadObserver, ReadValueIdentity, TransactionReadContext};
use super::resolve::schema_has_avet;
use super::value::{DatabaseValue, ReadBasis};
use crate::index::compare_prefix;
use crate::index::overlay::{EavSet, EavsSet, OverlayIndexCursor, OverlayIndexes};
use crate::model::identity::validate_frontier;
use crate::{
    DB_IDENT, Datom, IndexBoundary, IndexOrder, IndexPrefix, Schema, SemanticError, Value,
};
use std::cmp::Ordering;
use std::sync::Arc;
use std::vec::IntoIter;

/// One immutable speculative db-after over an exact committed base.
///
/// Path-copied indexes share canonical speculative datoms and ident metadata.
/// Prefix reads merge a selective index range with the committed base; no
/// complete durable index is retained or materialized. Every branch references
/// that same base directly, so cursor/drop depth is bounded by tree height.
#[derive(Clone)]
pub(super) struct TransactionOverlay {
    pub(super) base: DatabaseValue,
    pub(super) indexes: OverlayIndexes,
    pub(super) schema: Arc<Schema>,
    pub(super) newly_enabled_avet: Arc<[u32]>,
    pub(super) basis_t: u64,
    pub(super) eidx_frontier: u64,
    pub(super) last_tx_instant: i64,
    pub(super) reserved_allocation: Option<crate::reserved_allocation::ReservedAllocation>,
}

pub(super) struct TransactionOverlayScanCursor<'a> {
    pub(super) base: TransactionOverlayBaseCursor<'a>,
    pub(super) delta: OverlayDeltaCursor,
    pub(super) removals: EavSet,
    pub(super) schema: Arc<Schema>,
    pub(super) history: bool,
    pub(super) order: IndexOrder,
    pub(super) reverse: bool,
    pub(super) observer: Option<Arc<LogicalReadObserver>>,
    pub(super) base_next: Option<OverlayCursorDatom>,
    pub(super) delta_next: Option<OverlayCursorDatom>,
    pub(super) failed: bool,
}

pub(super) enum TransactionOverlayBaseCursor<'a> {
    Scan(DatabaseValueScanCursor<'a>),
    Prefix(DatabaseValuePrefixCursor<'a>),
    Empty,
}

impl Iterator for TransactionOverlayBaseCursor<'_> {
    type Item = Result<Datom, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Scan(cursor) => cursor.next(),
            Self::Prefix(cursor) => cursor.next(),
            Self::Empty => None,
        }
    }
}

pub(super) struct OverlayCursorDatom {
    pub(super) datom: Datom,
    /// An AVET-enablement fallback must read an AEVT range and reorder it
    /// before it can be merged. Those source datoms are charged while that
    /// explicit broad operation is collected, so the merged cursor must not
    /// charge them a second time.
    pub(super) precharged: bool,
}

/// Merge a lazy resident range with the one exceptional materialized source:
/// an AVET-enablement backfill. Ordinary reads allocate no novelty-sized vector.
pub(super) struct OverlayDeltaCursor {
    pub(super) indexed: OverlayIndexCursor,
    pub(super) indexed_next: Option<Arc<Datom>>,
    pub(super) backfill: std::iter::Peekable<IntoIter<OverlayCursorDatom>>,
    pub(super) backfill_eavs: EavsSet,
    pub(super) schema: Arc<Schema>,
    pub(super) history: bool,
    pub(super) order: IndexOrder,
    pub(super) reverse: bool,
}

impl OverlayDeltaCursor {
    pub(super) fn new(
        indexed: OverlayIndexCursor,
        mut backfill: Vec<OverlayCursorDatom>,
        schema: Arc<Schema>,
        history: bool,
        order: IndexOrder,
        reverse: bool,
        boundary: Option<&crate::index::NormalizedIndexBoundary>,
    ) -> Self {
        let mut backfill_eavs = EavsSet::default();
        if !history {
            for item in &backfill {
                backfill_eavs.insert(Arc::new(item.datom.clone()));
            }
        }
        // Duplicate current facts belong to the base even when its original
        // transaction coordinate lies outside this requested boundary.
        if let Some(boundary) = boundary {
            backfill.retain(|item| {
                let compared = boundary.compare_datom(&item.datom);
                if reverse {
                    !compared.is_gt()
                } else {
                    !compared.is_lt()
                }
            });
        }
        backfill.sort_by(|left, right| left.datom.cmp_in(&right.datom, order));
        if reverse {
            backfill.reverse();
        }
        Self {
            indexed,
            indexed_next: None,
            backfill: backfill.into_iter().peekable(),
            backfill_eavs,
            schema,
            history,
            order,
            reverse,
        }
    }
}

impl OverlayDeltaCursor {
    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Result<Option<OverlayCursorDatom>, SemanticError> {
        if self.indexed_next.is_none() {
            loop {
                if !control.check(None)? {
                    return Ok(None);
                }
                let Some(datom) = self.indexed.next() else {
                    break;
                };
                if overlay_index_member(&self.schema, &datom, self.order)
                    && (self.history || !self.backfill_eavs.contains(&datom))
                {
                    self.indexed_next = Some(datom);
                    break;
                }
            }
        }
        let take_indexed = match (&self.indexed_next, self.backfill.peek()) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(indexed), Some(backfill)) => {
                let compared = indexed.cmp_in(&backfill.datom, self.order);
                if self.reverse {
                    compared.is_ge()
                } else {
                    compared.is_le()
                }
            }
        };
        Ok(if take_indexed {
            self.indexed_next.take().map(|datom| OverlayCursorDatom {
                datom: (*datom).clone(),
                precharged: false,
            })
        } else {
            self.backfill.next()
        })
    }
}

impl DatabaseValue {
    /// Assessment supplies its final retained cursor, including allocated
    /// tempids which left no datoms. It cannot be derived from schema alone.
    pub(crate) fn transaction_overlay_with_allocation(
        mut base: DatabaseValue,
        tx_data: Arc<[Datom]>,
        schema: Arc<Schema>,
        basis_t: u64,
        eidx_frontier: u64,
        last_tx_instant: i64,
        reserved_allocation: Option<crate::reserved_allocation::ReservedAllocation>,
    ) -> Result<Self, SemanticError> {
        if !base.direct_current() {
            return Err(SemanticError::incorrect(
                "database/overlay-requires-current",
                "a transaction overlay requires an unfiltered point-current db-before",
            ));
        }
        let programs = base.programs.clone();
        let expected_basis = base.basis_t().checked_add(1).ok_or_else(|| {
            SemanticError::incorrect(
                "database/overlay-basis-overflow",
                "a transaction overlay cannot advance the maximum database basis",
            )
        })?;
        if basis_t != expected_basis {
            return Err(SemanticError::incorrect(
                "database/overlay-noncontiguous-basis",
                format!("expected overlay basis {expected_basis}, got {basis_t}"),
            ));
        }
        validate_frontier(eidx_frontier)?;
        if let Some(allocation) = reserved_allocation {
            crate::reserved_allocation::ReservedAllocation::from_frontier(
                allocation.frontier(),
                eidx_frontier,
            )?;
        }
        if eidx_frontier < base.eidx_frontier() {
            return Err(SemanticError::incorrect(
                "database/overlay-frontier-regression",
                "a transaction overlay cannot move the entity issuance frontier backward",
            ));
        }
        let expected_tx = crate::t_to_tx(basis_t)?;
        if tx_data.iter().any(|datom| datom.tx != expected_tx) {
            return Err(SemanticError::incorrect(
                "database/overlay-transaction-mismatch",
                "every overlay datom must name the successor transaction",
            ));
        }
        if tx_data
            .windows(2)
            .any(|pair| !pair[0].cmp_in(&pair[1], IndexOrder::Eavt).is_lt())
        {
            return Err(SemanticError::incorrect(
                "database/overlay-noncanonical-datoms",
                "overlay datoms must be strictly ordered in canonical EAVT order",
            ));
        }
        if base
            .last_tx_instant()
            .is_some_and(|previous| last_tx_instant < previous)
        {
            return Err(SemanticError::incorrect(
                "database/overlay-non-monotonic-instant",
                "overlay transaction instant precedes its db-before",
            ));
        }

        // Recovered ident reconstruction folds assertions only: retractions
        // do not erase aliases, and a later assertion may repurpose one. Keep
        // just this transaction's delta and resolve it before the base cache.
        let mut ident_assertions = Vec::new();
        for datom in tx_data
            .iter()
            .filter(|datom| datom.added && u64::from(datom.attribute) == DB_IDENT)
        {
            let Value::Keyword(ident) = &datom.value else {
                return Err(SemanticError::incorrect(
                    "database/overlay-invalid-ident",
                    ":db/ident overlay assertions must contain keywords",
                ));
            };
            if ident_assertions.iter().any(|(prior_ident, prior_entity)| {
                prior_ident == ident || *prior_entity == datom.entity
            }) {
                return Err(SemanticError::incorrect(
                    "database/overlay-conflicting-ident",
                    "overlay ident assertions must be unique by ident and entity",
                ));
            }
            ident_assertions.push((ident.clone(), datom.entity));
        }

        let read_context = base.read_context.clone();
        let mut indexes = OverlayIndexes::default();
        let mut unchanged_avet = None;
        if let ReadBasis::TransactionOverlay(prior) = &base.basis {
            indexes = prior.indexes.clone();
            if Arc::ptr_eq(&schema, &prior.schema) {
                unchanged_avet = Some(prior.newly_enabled_avet.clone());
            }
            base = prior.base.clone();
        }
        indexes.extend(&tx_data, ident_assertions);
        let base_schema = base.schema_arc();
        let newly_enabled_avet: Arc<[u32]> = if let Some(unchanged) = unchanged_avet {
            unchanged
        } else if Arc::ptr_eq(&schema, &base_schema) {
            Arc::from([])
        } else {
            schema
                .attributes()
                .filter(|attribute| {
                    schema_has_avet(&schema, attribute.id)
                        && !schema_has_avet(&base_schema, attribute.id)
                })
                .map(|attribute| attribute.id)
                .collect::<Vec<_>>()
                .into()
        };

        // The overlay is the logical read boundary. Keeping an observer or
        // memo context on both it and its wrapped base would charge/cache
        // every base datom twice. The outer value inherits the same attempt
        // context under its own immutable-value identity.
        base.read_observer = None;
        base.read_context = None;
        Ok(Self {
            entity_origin: base.entity_origin.clone(),
            basis: ReadBasis::TransactionOverlay(Arc::new(TransactionOverlay {
                base,
                indexes,
                schema,
                newly_enabled_avet,
                basis_t,
                eidx_frontier,
                last_tx_instant,
                reserved_allocation,
            })),
            read_identity: Arc::new(ReadValueIdentity),
            as_of_t: None,
            since_t: None,
            history: false,
            filters: Arc::default(),
            read_observer: read_context.as_ref().map(|context| context.observer()),
            read_context,
            programs,
        })
    }
}

impl TransactionOverlay {
    pub(super) fn scan_cursor(
        &self,
        history: bool,
        order: IndexOrder,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<TransactionOverlayScanCursor<'_>, SemanticError> {
        self.scan_cursor_from(history, order, observer, physical_context, None)
    }

    pub(super) fn scan_cursor_from(
        &self,
        history: bool,
        order: IndexOrder,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
        seek: Option<(&IndexBoundary, bool, bool)>,
    ) -> Result<TransactionOverlayScanCursor<'_>, SemanticError> {
        let reverse = seek.is_some_and(|(_, reverse, _)| reverse);
        let base = TransactionOverlayBaseCursor::Scan(match (&self.base.basis, seek) {
            (ReadBasis::Block(snapshot), Some((boundary, reverse, after)))
                if boundary.avet_attribute().is_some_and(|attribute| {
                    self.newly_enabled_avet.binary_search(&attribute).is_ok()
                }) =>
            {
                DatabaseValueScanCursor {
                    inner: DatabaseValueScanCursorInner::Block(Box::new(
                        snapshot.normalized_cursor(
                            history,
                            if after {
                                boundary.normalized()?.after_prefix()
                            } else {
                                boundary.normalized()?
                            },
                            reverse,
                        ),
                    )),
                    observer: None,
                    physical_context: physical_context.clone(),
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    failed: false,
                }
            }
            (_, Some((boundary, true, _))) => self.base.basis_reverse_boundary_cursor(
                history,
                boundary,
                None,
                physical_context.clone(),
            )?,
            (_, Some((boundary, false, after))) => self.base.basis_seek_boundary_cursor_biased(
                history,
                boundary,
                after,
                None,
                physical_context.clone(),
            )?,
            (_, None) => {
                self.base
                    .basis_scan_cursor(history, order, None, physical_context.clone())?
            }
        });
        let removals = self.indexes.removals.clone();
        let mut delta = Vec::<OverlayCursorDatom>::new();

        // `add-avet` copies the attribute's existing AEVT working set into
        // the transaction-local AVET. Do the same bounded, attribute-local
        // backfill when this transaction first enables AVET; the durable base
        // remains streamed and untouched.
        if matches!(order, IndexOrder::Avet | IndexOrder::Savet) {
            for attribute in self.newly_enabled_avet.iter().copied() {
                for datom in self.base.basis_prefix_cursor(
                    history,
                    &IndexPrefix::Aevt {
                        attribute,
                        entity: None,
                        value: None,
                    },
                    None,
                    physical_context.clone(),
                )? {
                    let datom = datom?;
                    if let Some(observer) = &observer {
                        observer.charge_datom(&datom)?;
                    }
                    if history || !removals.contains(&datom) {
                        delta.push(OverlayCursorDatom {
                            datom,
                            precharged: true,
                        });
                    }
                }
            }
        }

        let normalized = seek
            .map(|(boundary, _, after)| {
                boundary.normalized().map(|normalized| {
                    if after {
                        normalized.after_prefix()
                    } else {
                        normalized
                    }
                })
            })
            .transpose()?;
        let indexed = self.indexes.cursor(
            history,
            order,
            |datom| {
                normalized
                    .as_ref()
                    .map_or(Ordering::Equal, |boundary| boundary.compare_datom(datom))
            },
            reverse,
            None,
        );
        Ok(TransactionOverlayScanCursor {
            base,
            delta: OverlayDeltaCursor::new(
                indexed,
                delta,
                self.schema.clone(),
                history,
                order,
                reverse,
                normalized.as_ref(),
            ),
            removals,
            schema: Arc::clone(&self.schema),
            history,
            order,
            reverse,
            observer,
            base_next: None,
            delta_next: None,
            failed: false,
        })
    }

    pub(super) fn prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<TransactionOverlayScanCursor<'_>, SemanticError> {
        let source_prefix = self.source_prefix(prefix);
        let removals = self.indexes.removals.clone();
        let mut delta = Vec::<OverlayCursorDatom>::new();

        // A newly enabled AVET range is physically absent from db-before and
        // must be sourced from AEVT, whose order cannot be merged directly
        // into AVET. This is the one explicit broad prefix operation. Charge
        // each source datom before retaining it so a small transaction cap
        // stops the reorder before it can allocate the complete range.
        let base = if source_prefix.order() == prefix.order() {
            TransactionOverlayBaseCursor::Prefix(self.base.basis_prefix_cursor(
                history,
                &source_prefix,
                None,
                physical_context.clone(),
            )?)
        } else {
            for datom in self.base.basis_prefix_cursor(
                history,
                &source_prefix,
                None,
                physical_context.clone(),
            )? {
                let datom = datom?;
                if let Some(observer) = &observer {
                    observer.charge_datom(&datom)?;
                }
                if overlay_index_member(&self.schema, &datom, prefix.order())
                    && compare_prefix(&datom, prefix).is_eq()
                    && (history || !removals.contains(&datom))
                {
                    delta.push(OverlayCursorDatom {
                        datom,
                        precharged: true,
                    });
                }
            }
            TransactionOverlayBaseCursor::Empty
        };

        // A source-only SAVET range spans every eligible attribute. Keep
        // its ready base range and add only the attributes enabled locally.
        if matches!(
            prefix,
            IndexPrefix::Savet {
                attribute: None,
                ..
            }
        ) {
            for attribute in self.newly_enabled_avet.iter().copied() {
                for datom in self.base.basis_prefix_cursor(
                    history,
                    &IndexPrefix::Aevt {
                        attribute,
                        entity: None,
                        value: None,
                    },
                    None,
                    physical_context.clone(),
                )? {
                    let datom = datom?;
                    if let Some(observer) = &observer {
                        observer.charge_datom(&datom)?;
                    }
                    if compare_prefix(&datom, prefix).is_eq()
                        && (history || !removals.contains(&datom))
                    {
                        delta.push(OverlayCursorDatom {
                            datom,
                            precharged: true,
                        });
                    }
                }
            }
        }

        let indexed = self.indexes.cursor(
            history,
            prefix.order(),
            |datom| compare_prefix(datom, prefix),
            false,
            Some(prefix.clone()),
        );

        Ok(TransactionOverlayScanCursor {
            base,
            delta: OverlayDeltaCursor::new(
                indexed,
                delta,
                self.schema.clone(),
                history,
                prefix.order(),
                false,
                None,
            ),
            removals,
            schema: Arc::clone(&self.schema),
            history,
            order: prefix.order(),
            reverse: false,
            observer,
            base_next: None,
            delta_next: None,
            failed: false,
        })
    }

    pub(super) fn source_prefix(&self, prefix: &IndexPrefix) -> IndexPrefix {
        match prefix {
            // Only AVET enablement needs the documented linear attribute
            // backfill. Steady indexed reads retain the caller's selective
            // AVET seek, while disabling can filter the old AVET range away.
            IndexPrefix::Avet { attribute, .. }
            | IndexPrefix::Savet {
                attribute: Some(attribute),
                ..
            } if self.newly_enabled_avet.binary_search(attribute).is_ok() => IndexPrefix::Aevt {
                attribute: *attribute,
                entity: None,
                value: None,
            },
            _ => prefix.clone(),
        }
    }
}

impl TransactionOverlayScanCursor<'_> {
    pub(super) fn fill_base(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Result<(), SemanticError> {
        while self.base_next.is_none() {
            // Removed/disabled base facts can form a long prefix. Poll even
            // when none of them survives the overlay merge to reach a window.
            // Range-stop predicates still see only ordered merged datoms, not
            // out-of-order base/delta peeks.
            if !control.check(None)? {
                break;
            }
            let Some(candidate) = self.base.next().transpose()? else {
                break;
            };
            if !overlay_index_member(&self.schema, &candidate, self.order) {
                continue;
            }
            if !self.history && self.removals.contains(&candidate) {
                continue;
            }
            self.base_next = Some(OverlayCursorDatom {
                datom: candidate,
                precharged: false,
            });
        }
        Ok(())
    }

    pub(super) fn next_result(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Result<Option<OverlayCursorDatom>, SemanticError> {
        self.fill_base(control)?;
        if control.stopped {
            return Ok(None);
        }
        if self.delta_next.is_none() {
            self.delta_next = self.delta.next_controlled(control)?;
        }
        if control.stopped {
            return Ok(None);
        }
        match (&self.base_next, &self.delta_next) {
            (None, None) => Ok(None),
            (Some(_), None) => Ok(self.base_next.take()),
            (None, Some(_)) => Ok(self.delta_next.take()),
            (Some(base), Some(delta))
                if !self.history
                    && same_stored_eav(&base.datom, &delta.datom)
                    && base.datom.source == delta.datom.source =>
            {
                if u64::from(delta.datom.attribute) == crate::DB_ALTER_ATTRIBUTE {
                    // Repeated alter hooks are distinct immutable history
                    // events, and the newest one is the current coordinate.
                    self.base_next = None;
                    Ok(self.delta_next.take())
                } else {
                    self.delta_next = None;
                    Ok(self.base_next.take())
                }
            }
            (Some(base), Some(delta)) => match if self.reverse {
                delta.datom.cmp_in(&base.datom, self.order)
            } else {
                base.datom.cmp_in(&delta.datom, self.order)
            } {
                std::cmp::Ordering::Less => Ok(self.base_next.take()),
                std::cmp::Ordering::Greater => Ok(self.delta_next.take()),
                std::cmp::Ordering::Equal => {
                    self.delta_next = None;
                    Ok(self.base_next.take())
                }
            },
        }
    }
}

impl TransactionOverlayScanCursor<'_> {
    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Option<Result<Datom, SemanticError>> {
        if self.failed {
            return None;
        }
        match self.next_result(control) {
            Ok(Some(item)) => {
                if !item.precharged
                    && let Some(observer) = &self.observer
                    && let Err(error) = observer.charge_datom(&item.datom)
                {
                    self.failed = true;
                    return Some(Err(error));
                }
                Some(Ok(item.datom))
            }
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

pub(super) fn overlay_index_member(schema: &Schema, datom: &Datom, order: IndexOrder) -> bool {
    match order {
        IndexOrder::Eavt | IndexOrder::Aevt | IndexOrder::Saevt => true,
        IndexOrder::Avet | IndexOrder::Savet => schema
            .attribute(datom.attribute)
            .is_ok_and(|attribute| attribute.indexed || attribute.unique.is_some()),
        IndexOrder::Vaet => schema
            .attribute(datom.attribute)
            .is_ok_and(|attribute| attribute.value_type == crate::ValueType::Ref),
    }
}

pub(super) fn same_stored_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.stored_eq(&right.value)
}
