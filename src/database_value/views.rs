//! Temporal/custom views and their single incremental history window.
use super::cursor::{DatabaseValuePrefixCursor, DatabaseValueScanCursor, ReadCursorControl};
use super::read_context::{ReadValueIdentity, TransactionReadContext};
use super::value::{DatabaseValue, ReadBasis, ReadFilter};
use crate::{
    Datom, EntityIdentifier, ErrorCategory, IndexBoundary, IndexComponents, IndexOrder,
    IndexPrefix, SemanticError, TimePoint, Value, tx_to_t,
};
use std::collections::VecDeque;
use std::sync::Arc;

/// Recovered `windowed` as an incremental state machine.
///
/// The source is retained behind one box to break the recursive cursor shape:
/// a window wraps a raw-history scan/prefix cursor, while the public cursor in
/// turn owns the window. No source datoms are retained beyond the current
/// logical E/A/V group.
pub(super) struct DatabaseValueWindowCursor<'a> {
    pub(super) source: DatabaseValueWindowSource<'a>,
    pub(super) filter_database: DatabaseValue,
    pub(super) filters: Arc<[Arc<ReadFilter>]>,
    pub(super) as_of_t: Option<u64>,
    pub(super) since_t: Option<u64>,
    pub(super) history: bool,
    pub(super) order: IndexOrder,
    pub(super) reverse: bool,
    pub(super) forward_boundary: Option<crate::index::NormalizedIndexBoundary>,
    pub(super) group: Option<WindowGroup>,
    pub(super) retracted_representations: Vec<Value>,
    pub(super) reverse_pending: Option<Datom>,
    pub(super) reverse_output: VecDeque<Datom>,
    pub(super) failed: bool,
}

pub(super) enum DatabaseValueWindowSource<'a> {
    Scan(Box<DatabaseValueScanCursor<'a>>),
    Prefix(Box<DatabaseValuePrefixCursor<'a>>),
}

impl Iterator for DatabaseValueWindowSource<'_> {
    type Item = Result<Datom, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Scan(cursor) => cursor.next(),
            Self::Prefix(cursor) => cursor.next(),
        }
    }
}

impl DatabaseValueWindowSource<'_> {
    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Option<Result<Datom, SemanticError>> {
        match self {
            Self::Scan(cursor) => cursor.next_controlled(control),
            Self::Prefix(cursor) => cursor.next_controlled(control),
        }
    }
}

pub(super) struct WindowGroup {
    pub(super) entity: u64,
    pub(super) attribute: u32,
    pub(super) value: Value,
}

impl WindowGroup {
    pub(super) fn from_datom(datom: &Datom) -> Self {
        Self {
            entity: datom.entity,
            attribute: datom.attribute,
            value: datom.value.clone(),
        }
    }

    pub(super) fn matches(&self, datom: &Datom) -> bool {
        self.entity == datom.entity
            && self.attribute == datom.attribute
            && self.value.index_cmp(&datom.value).is_eq()
    }
}

impl<'a> DatabaseValueWindowCursor<'a> {
    pub(super) fn new(
        database: &DatabaseValue,
        source: DatabaseValueWindowSource<'a>,
        order: IndexOrder,
        reverse: bool,
    ) -> Self {
        Self {
            source,
            filter_database: database.without_filters(),
            filters: Arc::clone(&database.filters),
            as_of_t: database.as_of_t,
            since_t: database.since_t,
            history: database.history,
            order,
            reverse,
            forward_boundary: None,
            group: None,
            retracted_representations: Vec::new(),
            reverse_pending: None,
            reverse_output: VecDeque::new(),
            failed: false,
        }
    }

    pub(super) fn next_filtered(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Result<Option<Datom>, SemanticError> {
        loop {
            let Some(datom) = self.source.next_controlled(control).transpose()? else {
                return Ok(None);
            };
            let t = tx_to_t(datom.tx)?;
            if self.as_of_t.is_some_and(|as_of| t > as_of)
                || self.since_t.is_some_and(|since| t <= since)
                || !self
                    .filters
                    .iter()
                    .all(|predicate| predicate(&self.filter_database, &datom))
            {
                continue;
            }
            return Ok(Some(datom));
        }
    }

    /// Reverse history visits one logical E/A/V group oldest-first. Match
    /// recovered `rseek-datoms`: temporal/custom predicates run first, then
    /// the last visible event determines current membership. Atomic retains
    /// one winner per strict stored V representation, consistent with the
    /// native kernel's representation-distinct top-level BigDecimals.
    pub(super) fn next_reverse_current(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Result<Option<Datom>, SemanticError> {
        loop {
            if let Some(datom) = self.reverse_output.pop_front() {
                return Ok(Some(datom));
            }

            let first = match self.reverse_pending.take() {
                Some(datom) => datom,
                None => {
                    let Some(datom) = self.next_filtered(control)? else {
                        return Ok(None);
                    };
                    datom
                }
            };
            let group = WindowGroup::from_datom(&first);
            let mut winners = Vec::<Datom>::new();

            let retain = |candidate: Datom, winners: &mut Vec<Datom>| match winners
                .binary_search_by(|prior| prior.value.stored_cmp(&candidate.value))
            {
                Ok(position) => winners[position] = candidate,
                Err(position) => winners.insert(position, candidate),
            };
            retain(first, &mut winners);

            while let Some(candidate) = self.next_filtered(control)? {
                if group.matches(&candidate) {
                    retain(candidate, &mut winners);
                } else {
                    self.reverse_pending = Some(candidate);
                    break;
                }
            }

            winners.retain(|datom| datom.added);
            winners.sort_by(|left, right| right.cmp_in(left, self.order));
            self.reverse_output.extend(winners);
        }
    }

    pub(super) fn visible_current(&mut self, datom: &Datom) -> bool {
        if self
            .group
            .as_ref()
            .is_none_or(|group| !group.matches(datom))
        {
            self.group = Some(WindowGroup::from_datom(datom));
            self.retracted_representations.clear();
        }

        let stored = self
            .retracted_representations
            .binary_search_by(|value| value.stored_cmp(&datom.value));
        if datom.added {
            // Recovered `filter-retractions` yields every assertion until a
            // later visible retraction. Atomic deliberately keeps top-level
            // strict-scale BigDecimal facts representation-distinct, so the
            // one-group skip set is keyed by stored equality; tuple members
            // retain their established recursive logical equality.
            stored.is_err()
        } else {
            if let Err(position) = stored {
                self.retracted_representations
                    .insert(position, datom.value.clone());
            }
            false
        }
    }

    pub(super) fn with_forward_boundary(
        mut self,
        boundary: crate::index::NormalizedIndexBoundary,
    ) -> Self {
        self.forward_boundary = Some(boundary);
        self
    }
}

impl DatabaseValueWindowCursor<'_> {
    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Option<Result<Datom, SemanticError>> {
        if self.failed {
            return None;
        }
        if self.reverse && !self.history {
            return match self.next_reverse_current(control) {
                Ok(Some(datom)) => Some(Ok(datom)),
                Ok(None) => None,
                Err(error) => {
                    self.failed = true;
                    Some(Err(error))
                }
            };
        }
        loop {
            let datom = match self.next_filtered(control) {
                Ok(Some(datom)) => datom,
                Ok(None) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            };
            if self.history || self.visible_current(&datom) {
                if self
                    .forward_boundary
                    .as_ref()
                    .is_some_and(|boundary| boundary.compare_datom(&datom).is_lt())
                {
                    continue;
                }
                return Some(Ok(datom));
            }
        }
    }
}

impl DatabaseValue {
    /// Database-filter docs: `with` computes over the full basis, then applies
    /// the supplied view. An as-of view is not a branch of the past.
    pub(crate) fn speculation_base(&self) -> Result<Self, SemanticError> {
        if self.history {
            return Err(SemanticError::incorrect(
                "transaction/history-with",
                "history values cannot be used for speculative transactions",
            ));
        }
        let mut base = self.clone();
        base.read_identity = Arc::new(ReadValueIdentity);
        base.as_of_t = None;
        base.since_t = None;
        base.filters = Arc::default();
        Ok(base)
    }

    pub(crate) fn with_speculation_view(mut self, original: &Self) -> Self {
        self.read_identity = Arc::new(ReadValueIdentity);
        self.as_of_t = original.as_of_t;
        self.since_t = original.since_t;
        self.filters = Arc::clone(&original.filters);
        self
    }

    pub fn as_of_t(&self) -> Option<u64> {
        self.as_of_t
    }

    pub fn since_t(&self) -> Option<u64> {
        self.since_t
    }

    /// Resolve a documented time point to the boundary used by `as-of` and
    /// `since`.
    ///
    /// T and Tx are exact.  Instant resolution mirrors recovered 1.0.7705
    /// `as-of-t`: one AVET lower-bound seek finds the first transaction at or
    /// after the millisecond.  An exact duplicate-millisecond match therefore
    /// selects the earliest matching transaction; `since` remains exclusive
    /// of that resolved T and can expose later transactions from the same
    /// millisecond.  This is the documented imprecision of instant points.
    pub fn resolve_time_point(&self, time_point: TimePoint) -> Result<u64, SemanticError> {
        match time_point {
            TimePoint::T(t) => {
                crate::t_to_tx(t)?;
                Ok(t)
            }
            TimePoint::Tx(tx) => crate::tx_to_t(tx),
            TimePoint::Instant(instant) => self.t_at_or_before_instant(instant),
        }
    }

    /// Resolve and install an inclusive as-of boundary without changing the
    /// immutable basis, schema, or ident dictionary.
    pub fn as_of_time_point(mut self, time_point: TimePoint) -> Result<Self, SemanticError> {
        let t = self.resolve_time_point(time_point)?;
        self.read_identity = Arc::new(ReadValueIdentity);
        self.as_of_t = Some(t);
        Ok(self)
    }

    /// Resolve and install an exclusive since boundary without changing the
    /// immutable basis, schema, or ident dictionary.
    pub fn since_time_point(mut self, time_point: TimePoint) -> Result<Self, SemanticError> {
        let t = self.resolve_time_point(time_point)?;
        self.read_identity = Arc::new(ReadValueIdentity);
        self.since_t = Some(t);
        Ok(self)
    }

    /// Fabricate the EAVT entity boundary for a named or implicit partition.
    ///
    /// Recovered `entid-at` uses a different instant rule from `as-of`: it
    /// selects the first transaction at or after the instant. Recovered
    /// `partbits` accepts either a partition-zero entity (whose entity-index
    /// names the partition) or the base eid of any nonzero partition.
    pub fn entid_at(
        &self,
        partition: &EntityIdentifier,
        time_point: TimePoint,
    ) -> Result<u64, SemanticError> {
        let partition_entity = self.resolve_entity_identifier(partition)?.ok_or_else(|| {
            SemanticError::incorrect(
                "database/unknown-partition",
                "partition entity does not resolve in this database value",
            )
        })?;
        let entity_partition = crate::eid_to_part(partition_entity)?;
        let entity_index = crate::eid_to_eidx(partition_entity)?;
        let partition_bits = if entity_partition == crate::DB_PARTITION {
            u32::try_from(entity_index)
                .ok()
                .filter(|partition| *partition <= crate::MAX_PARTITION)
                .ok_or_else(|| {
                    SemanticError::incorrect(
                        "database/not-a-partition",
                        format!(
                            "partition-zero entity {partition_entity} has out-of-range partition bits {entity_index}"
                        ),
                    )
                })?
        } else if entity_index == 0 {
            entity_partition
        } else {
            return Err(SemanticError::incorrect(
                "database/not-a-partition",
                format!(
                    "entity {partition_entity} is not partition-zero or a nonzero partition base"
                ),
            ));
        };
        let t = match time_point {
            TimePoint::T(t) => {
                crate::t_to_tx(t)?;
                t
            }
            TimePoint::Tx(tx) => crate::tx_to_t(tx)?,
            TimePoint::Instant(instant) => self.t_at_or_after_instant(instant)?,
        };
        crate::make_eid(partition_bits, t)
    }

    pub fn is_history(&self) -> bool {
        self.history
    }

    pub fn is_filtered(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Derive a value with an inclusive upper transaction boundary.
    /// Repeating `as_of` replaces the prior upper boundary, matching the
    /// recovered immutable `Db` record update.
    pub fn as_of(mut self, t: u64) -> Self {
        self.read_identity = Arc::new(ReadValueIdentity);
        self.as_of_t = Some(t);
        self
    }

    /// Derive a value with an exclusive lower transaction boundary.
    /// Repeating `since` replaces the prior lower boundary.
    pub fn since(mut self, t: u64) -> Self {
        self.read_identity = Arc::new(ReadValueIdentity);
        self.since_t = Some(t);
        self
    }

    pub(super) fn t_at_or_before_instant(&self, instant: i64) -> Result<u64, SemanticError> {
        let Some(datom) = self.first_tx_instant_at_or_after(instant)? else {
            return self.next_t();
        };
        let (candidate_instant, candidate_t) = self.decode_tx_instant_candidate(&datom, instant)?;
        if candidate_instant == instant {
            Ok(candidate_t)
        } else {
            candidate_t.checked_sub(1).ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "database/invalid-tx-instant-boundary",
                    "positive transaction instant has no predecessor T boundary",
                )
            })
        }
    }

    pub(super) fn t_at_or_after_instant(&self, instant: i64) -> Result<u64, SemanticError> {
        let Some(datom) = self.first_tx_instant_at_or_after(instant)? else {
            return self.next_t();
        };
        self.decode_tx_instant_candidate(&datom, instant)
            .map(|(_, t)| t)
    }

    pub(super) fn next_t(&self) -> Result<u64, SemanticError> {
        self.basis_t().checked_add(1).ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Fault,
                "database/time-boundary-overflow",
                "database basis has no representable successor time boundary",
            )
        })
    }

    pub(super) fn first_tx_instant_at_or_after(
        &self,
        instant: i64,
    ) -> Result<Option<Datom>, SemanticError> {
        let candidate =
            self.first_tx_instant_at_or_after_unobserved(instant, self.read_context.as_deref())?;
        if let (Some(observer), Some(datom)) = (&self.read_observer, &candidate) {
            observer.charge_datom(datom)?;
        }
        Ok(candidate.filter(|datom| datom.attribute == crate::DB_TX_INSTANT as u32))
    }

    pub(super) fn first_tx_instant_at_or_after_unobserved(
        &self,
        instant: i64,
        physical_context: Option<&TransactionReadContext>,
    ) -> Result<Option<Datom>, SemanticError> {
        let prefix = IndexPrefix::Avet {
            attribute: crate::DB_TX_INSTANT as u32,
            value: Some(Value::Instant(instant)),
            entity: None,
        };
        match &self.basis {
            ReadBasis::Eager(database) => Ok(database.seek_datoms(&prefix)?.first().cloned()),
            ReadBasis::Block(snapshot) => {
                let mut cursor = snapshot.boundary_cursor(
                    false,
                    &IndexBoundary::Avet(IndexComponents::Two(
                        crate::DB_TX_INSTANT as u32,
                        Value::Instant(instant),
                    )),
                    false,
                )?;
                let candidate = cursor.next().transpose()?;
                if let Some(context) = physical_context {
                    context.record_native_cursor(cursor.stats())?;
                }
                Ok(candidate)
            }
            ReadBasis::TransactionOverlay(overlay) => {
                let candidate = overlay
                    .base
                    .first_tx_instant_at_or_after_unobserved(instant, physical_context)?;
                if candidate
                    .as_ref()
                    .is_some_and(|datom| datom.attribute == crate::DB_TX_INSTANT as u32)
                {
                    return Ok(candidate);
                }
                if overlay.last_tx_instant < instant {
                    return Ok(candidate);
                }
                overlay
                    .indexes
                    .cursor(
                        false,
                        IndexOrder::Avet,
                        |datom| {
                            datom
                                .attribute
                                .cmp(&(crate::DB_TX_INSTANT as u32))
                                .then_with(|| datom.value.index_cmp(&Value::Instant(instant)))
                        },
                        false,
                        Some(IndexPrefix::Avet {
                            attribute: crate::DB_TX_INSTANT as u32,
                            value: None,
                            entity: None,
                        }),
                    )
                    .find(|datom| {
                        datom.entity == datom.tx
                            && datom.attribute == crate::DB_TX_INSTANT as u32
                            && datom.added
                            && matches!(datom.value, Value::Instant(value) if value >= instant)
                    })
                    .map(|datom| Some((*datom).clone()))
                    .ok_or_else(|| {
                        SemanticError::new(
                            ErrorCategory::Fault,
                            "database/invalid-overlay-tx-instant",
                            "transaction overlay has no own transaction instant",
                        )
                    })
            }
        }
    }

    pub(super) fn decode_tx_instant_candidate(
        &self,
        datom: &Datom,
        requested: i64,
    ) -> Result<(i64, u64), SemanticError> {
        let Value::Instant(candidate) = &datom.value else {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "database/invalid-tx-instant-index",
                ":db/txInstant AVET contains a non-instant value",
            ));
        };
        let t = tx_to_t(datom.tx).map_err(|error| {
            SemanticError::new(
                ErrorCategory::Fault,
                "database/invalid-tx-instant-index",
                format!(":db/txInstant AVET has an invalid transaction id: {error}"),
            )
        })?;
        if datom.entity != datom.tx
            || !datom.added
            || *candidate < requested
            || t == 0
            || t > self.basis_t()
        {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "database/invalid-tx-instant-index",
                ":db/txInstant AVET candidate is not a valid current transaction instant",
            ));
        }
        Ok((*candidate, t))
    }

    /// Derive a raw history value. Temporal and custom predicates remain in
    /// force; only assertion/retraction collapse is disabled.
    pub fn history(mut self) -> Self {
        self.history = true;
        self
    }

    /// Compose an additional custom datom predicate by logical conjunction.
    /// During evaluation the predicate receives the same temporal/history
    /// value with custom predicates removed, preventing recursive filtering.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static,
    {
        self.read_identity = Arc::new(ReadValueIdentity);
        let mut filters = self.filters.iter().cloned().collect::<Vec<_>>();
        filters.push(Arc::new(predicate));
        self.filters = filters.into();
        self
    }

    /// Reject operations such as entity navigation and pull that require one
    /// point in time. History remains valid for index access and queries.
    pub fn require_point_in_time(&self, operation: &'static str) -> Result<(), SemanticError> {
        if self.history {
            return Err(SemanticError::new(
                ErrorCategory::Incorrect,
                "database/history-not-point-in-time",
                format!("{operation} requires a point-in-time database value"),
            ));
        }
        Ok(())
    }

    pub(super) fn direct_current(&self) -> bool {
        !self.history && self.as_of_t.is_none() && self.since_t.is_none() && self.filters.is_empty()
    }

    pub(super) fn direct_history(&self) -> bool {
        self.history && self.as_of_t.is_none() && self.since_t.is_none() && self.filters.is_empty()
    }

    pub(super) fn without_filters(&self) -> Self {
        let mut database = self.clone();
        database.read_identity = Arc::new(ReadValueIdentity);
        database.filters = Arc::default();
        database
    }
}
