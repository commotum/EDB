//! Lazy cursor dispatch and cooperative controls over one shared value engine.
use super::overlay::TransactionOverlayScanCursor;
use super::read_context::{LogicalReadObserver, PrefixMemoSource, TransactionReadContext};
use super::value::{DatabaseValue, ReadBasis};
use super::views::{DatabaseValueWindowCursor, DatabaseValueWindowSource};
use crate::{Datom, IndexBoundary, IndexComponents, IndexOrder, IndexPrefix, SemanticError, Value};
use std::iter::{Cloned, Rev};
use std::slice::Iter;
use std::sync::Arc;

/// A fallible forward scan of one exact immutable database value.
///
/// Native scans retain the lazy persistent-tree/recent-tier merge; an
/// assessment overlay adds only its bounded transaction delta; temporal and
/// custom windows consume and collapse history incrementally. Every yielded
/// datom is owned, so advancing the connection or evicting a tree node cannot
/// mutate an already observed result.
pub struct DatabaseValueScanCursor<'a> {
    pub(super) inner: DatabaseValueScanCursorInner<'a>,
    pub(super) operation: Option<crate::sql_io::OperationContext>,
    pub(super) observer: Option<Arc<LogicalReadObserver>>,
    pub(super) physical_context: Option<Arc<TransactionReadContext>>,
    pub(super) physical_recorded: bool,
    pub(super) failed: bool,
}

pub(super) enum DatabaseValueScanCursorInner<'a> {
    Block(Box<crate::storage::BlockIndexCursor>),
    Overlay(Box<TransactionOverlayScanCursor<'a>>),
    Eager(Cloned<Iter<'a, Datom>>),
    EagerReverse(Cloned<Rev<Iter<'a, Datom>>>),
    Window(Box<DatabaseValueWindowCursor<'a>>),
}

/// Cooperative controls belong below a temporal/custom window: one request
/// for a visible datom may otherwise examine an unbounded rejected prefix.
/// A stopped range is not a completed prefix and must never enter its memo.
pub(super) struct ReadCursorControl<'a> {
    pub(super) check: &'a mut dyn FnMut(Option<&Datom>) -> Result<bool, SemanticError>,
    pub(super) stopped: bool,
}

impl ReadCursorControl<'_> {
    pub(super) fn check(&mut self, datom: Option<&Datom>) -> Result<bool, SemanticError> {
        if self.stopped {
            return Ok(false);
        }
        let proceed = (self.check)(datom)?;
        self.stopped = !proceed;
        Ok(proceed)
    }

    pub(super) fn accept(
        &mut self,
        item: Option<Result<Datom, SemanticError>>,
    ) -> Option<Result<Datom, SemanticError>> {
        match item {
            Some(Ok(datom)) => match self.check(Some(&datom)) {
                Ok(true) => Some(Ok(datom)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
            item => item,
        }
    }
}

impl DatabaseValueScanCursor<'_> {
    /// Poll before source advancement (`None`) and inspect each ordered source
    /// candidate (`Some`) before view filters/collapse. False ends this cursor
    /// without claiming that its underlying source prefix was exhausted.
    pub(crate) fn next_with_control(
        &mut self,
        check: &mut dyn FnMut(Option<&Datom>) -> Result<bool, SemanticError>,
    ) -> Option<Result<Datom, SemanticError>> {
        self.next_controlled(&mut ReadCursorControl {
            check,
            stopped: false,
        })
    }

    pub(super) fn record_physical_work(&mut self) -> Result<(), SemanticError> {
        if self.physical_recorded {
            return Ok(());
        }
        self.physical_recorded = true;
        if let (Some(context), DatabaseValueScanCursorInner::Block(cursor)) =
            (&self.physical_context, &self.inner)
        {
            context.record_native_cursor(cursor.stats())?;
        }
        Ok(())
    }

    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Option<Result<Datom, SemanticError>> {
        let operation = self.operation.clone();
        let _scope = operation
            .as_ref()
            .map(crate::sql_io::OperationContext::enter);
        if self.failed {
            return None;
        }
        match control.check(None) {
            Ok(true) => {}
            Ok(false) => {
                self.failed = true;
                let _ = self.record_physical_work();
                return None;
            }
            Err(error) => {
                self.failed = true;
                let _ = self.record_physical_work();
                return Some(Err(error));
            }
        }
        let window = matches!(&self.inner, DatabaseValueScanCursorInner::Window(_));
        let item = match &mut self.inner {
            DatabaseValueScanCursorInner::Block(cursor) => {
                cursor.next_with_poll(&mut || control.check(None))
            }
            DatabaseValueScanCursorInner::Overlay(cursor) => cursor.next_controlled(control),
            DatabaseValueScanCursorInner::Eager(cursor) => cursor.next().map(Ok),
            DatabaseValueScanCursorInner::EagerReverse(cursor) => cursor.next().map(Ok),
            DatabaseValueScanCursorInner::Window(cursor) => cursor.next_controlled(control),
        };
        let item = if window { item } else { control.accept(item) };
        self.failed |= control.stopped;
        let Some(item) = item else {
            if let Err(error) = self.record_physical_work() {
                self.failed = true;
                return Some(Err(error));
            }
            return None;
        };
        match item {
            Ok(datom) => {
                if let Some(observer) = &self.observer
                    && let Err(error) = observer.charge_datom(&datom)
                {
                    let _ = self.record_physical_work();
                    self.failed = true;
                    return Some(Err(error));
                }
                Some(Ok(datom))
            }
            Err(error) => {
                let _ = self.record_physical_work();
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl Iterator for DatabaseValueScanCursor<'_> {
    type Item = Result<Datom, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_control(&mut |_| Ok(true))
    }
}

impl Drop for DatabaseValueScanCursor<'_> {
    fn drop(&mut self) {
        let _ = self.record_physical_work();
    }
}

/// Lazy current-index cursor over one exact point-in-time database value.
///
/// Eager values borrow their immutable index slice. Native values own a
/// immutable peer cursor which lower-bound seeks both the durable tree and
/// its recent tier. Every yielded item is owned so callers cannot retain a
/// cache or tree-node borrow across cursor advancement.
pub struct DatabaseValuePrefixCursor<'a> {
    pub(super) inner: DatabaseValuePrefixCursorInner<'a>,
    pub(super) operation: Option<crate::sql_io::OperationContext>,
    pub(super) observer: Option<Arc<LogicalReadObserver>>,
    pub(super) physical_context: Option<Arc<TransactionReadContext>>,
    pub(super) physical_recorded: bool,
    pub(super) memo_source: Option<PrefixMemoSource>,
    pub(super) memo_hit: bool,
    pub(super) failed: bool,
}

pub(super) enum DatabaseValuePrefixCursorInner<'a> {
    Eager(Cloned<Iter<'a, Datom>>),
    Block(Box<crate::storage::BlockIndexCursor>),
    Overlay(Box<TransactionOverlayScanCursor<'a>>),
    Window(Box<DatabaseValueWindowCursor<'a>>),
    Memoized { datoms: Arc<[Datom]>, next: usize },
}

impl DatabaseValuePrefixCursor<'_> {
    pub(crate) fn next_with_control(
        &mut self,
        check: &mut dyn FnMut(Option<&Datom>) -> Result<bool, SemanticError>,
    ) -> Option<Result<Datom, SemanticError>> {
        self.next_controlled(&mut ReadCursorControl {
            check,
            stopped: false,
        })
    }

    pub(crate) fn is_memo_hit(&self) -> bool {
        self.memo_hit
    }

    pub(super) fn record_physical_work(&mut self) -> Result<(), SemanticError> {
        if self.physical_recorded {
            return Ok(());
        }
        self.physical_recorded = true;
        if let (Some(context), DatabaseValuePrefixCursorInner::Block(cursor)) =
            (&self.physical_context, &self.inner)
        {
            context.record_native_cursor(cursor.stats())?;
        }
        Ok(())
    }
}

impl Iterator for DatabaseValuePrefixCursor<'_> {
    type Item = Result<Datom, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_control(&mut |_| Ok(true))
    }
}

impl DatabaseValuePrefixCursor<'_> {
    pub(super) fn next_controlled(
        &mut self,
        control: &mut ReadCursorControl<'_>,
    ) -> Option<Result<Datom, SemanticError>> {
        let operation = self.operation.clone();
        let _scope = operation
            .as_ref()
            .map(crate::sql_io::OperationContext::enter);
        if self.failed {
            return None;
        }
        match control.check(None) {
            Ok(true) => {}
            Ok(false) => {
                self.failed = true;
                let _ = self.record_physical_work();
                return None;
            }
            Err(error) => {
                self.failed = true;
                let _ = self.record_physical_work();
                return Some(Err(error));
            }
        }
        let window = matches!(&self.inner, DatabaseValuePrefixCursorInner::Window(_));
        let item = match &mut self.inner {
            DatabaseValuePrefixCursorInner::Eager(cursor) => cursor.next().map(Ok),
            DatabaseValuePrefixCursorInner::Block(cursor) => {
                cursor.next_with_poll(&mut || control.check(None))
            }
            DatabaseValuePrefixCursorInner::Overlay(cursor) => cursor.next_controlled(control),
            DatabaseValuePrefixCursorInner::Window(cursor) => cursor.next_controlled(control),
            DatabaseValuePrefixCursorInner::Memoized { datoms, next } => {
                let datom = datoms.get(*next).cloned();
                *next = next.saturating_add(1);
                datom.map(Ok)
            }
        };
        let item = if window { item } else { control.accept(item) };
        self.failed |= control.stopped;
        let Some(item) = item else {
            if let Err(error) = self.record_physical_work() {
                self.failed = true;
                return Some(Err(error));
            }
            if let Some(source) = &mut self.memo_source
                && !source.completed
            {
                source.completed = true;
                let result = if source.cacheable && !control.stopped {
                    source.context.admit(
                        source
                            .key
                            .take()
                            .expect("an incomplete memo source retains its key"),
                        std::mem::take(&mut source.datoms),
                        source.retained_bytes,
                    )
                } else {
                    source.key.take();
                    source.context.reject()
                };
                if let Err(error) = result {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
            return None;
        };
        match item {
            Ok(datom) => {
                if let Some(source) = &mut self.memo_source
                    && let Err(error) = source.context.record_source_datom(&datom)
                {
                    source.key.take();
                    source.completed = true;
                    let _ = self.record_physical_work();
                    self.failed = true;
                    return Some(Err(error));
                }
                if let Some(observer) = &self.observer
                    && let Err(error) = observer.charge_datom(&datom)
                {
                    if let Some(source) = &mut self.memo_source {
                        let _ = source.context.reject();
                        source.key.take();
                        source.completed = true;
                    }
                    let _ = self.record_physical_work();
                    self.failed = true;
                    return Some(Err(error));
                }
                if let Some(source) = &mut self.memo_source
                    && source.cacheable
                {
                    let retained_bytes = datom.retained_bytes();
                    let next_bytes = source.retained_bytes.checked_add(retained_bytes);
                    let next_datoms = u64::try_from(source.datoms.len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(1);
                    if next_bytes.is_none_or(|bytes| bytes > source.context.max_retained_bytes)
                        || next_datoms > source.context.max_entry_datoms
                    {
                        source.cacheable = false;
                        source.datoms.clear();
                    } else {
                        source.retained_bytes = next_bytes.expect("checked above");
                        source.datoms.push(datom.clone());
                    }
                }
                Some(Ok(datom))
            }
            Err(error) => {
                if let Some(source) = &mut self.memo_source {
                    let _ = source.context.reject();
                    source.key.take();
                    source.completed = true;
                }
                let _ = self.record_physical_work();
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl Drop for DatabaseValuePrefixCursor<'_> {
    fn drop(&mut self) {
        let _ = self.record_physical_work();
        if let Some(source) = &mut self.memo_source
            && !source.completed
        {
            let _ = source.context.reject();
            source.key.take();
            source.completed = true;
        }
    }
}

impl DatabaseValue {
    /// Open a lazy forward cursor over one complete logical index.
    ///
    /// Construction does not visit a tree child or scan eager history.
    /// Temporal predicates and custom filters are applied before incremental
    /// current retraction collapse, matching recovered `windowed`.
    pub fn scan_cursor(
        &self,
        order: IndexOrder,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        crate::transaction_hints::record_broad_read();
        if self.direct_current() {
            return self.basis_scan_cursor(
                false,
                order,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        if self.direct_history() {
            return self.basis_scan_cursor(
                true,
                order,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        let source = self.basis_scan_cursor(true, order, None, self.read_context.clone())?;
        Ok(DatabaseValueScanCursor {
            inner: DatabaseValueScanCursorInner::Window(Box::new(DatabaseValueWindowCursor::new(
                self,
                DatabaseValueWindowSource::Scan(Box::new(source)),
                order,
                false,
            ))),
            observer: self.read_observer.clone(),
            physical_context: None,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            failed: false,
        })
    }

    /// Open a lazy forward raw-index cursor at a typed virtual boundary.
    ///
    /// The supplied components choose only the starting position; iteration
    /// continues through the rest of the index. Missing suffix components
    /// position before the lowest match, including every operation/stored
    /// representation tied at a full E/A/V/T boundary.
    pub fn seek_cursor(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        crate::transaction_hints::record_broad_read();
        self.validate_raw_boundary_access(boundary)?;
        if self.direct_current() {
            return self.basis_seek_boundary_cursor(
                false,
                boundary,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        if self.direct_history() {
            return self.basis_seek_boundary_cursor(
                true,
                boundary,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        let order = boundary.order();
        let source_boundary = if self.history {
            boundary.clone()
        } else {
            boundary.current_group_start()
        };
        let source = self.basis_seek_boundary_cursor(
            true,
            &source_boundary,
            None,
            self.read_context.clone(),
        )?;
        let window = DatabaseValueWindowCursor::new(
            self,
            DatabaseValueWindowSource::Scan(Box::new(source)),
            order,
            false,
        );
        let window = if self.history {
            window
        } else {
            window.with_forward_boundary(boundary.normalized()?)
        };
        Ok(DatabaseValueScanCursor {
            inner: DatabaseValueScanCursorInner::Window(Box::new(window)),
            observer: self.read_observer.clone(),
            physical_context: None,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            failed: false,
        })
    }

    /// Explicit collecting convenience over [`Self::seek_cursor`].
    pub fn collect_seek_datoms(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<Vec<Datom>, SemanticError> {
        self.seek_cursor(boundary)?.collect()
    }

    /// Open the lazy reverse complement of [`Self::seek_cursor`].
    ///
    /// Traversal starts after the highest match of the virtual components and
    /// proceeds toward the beginning of the index. Temporal/custom predicates
    /// filter raw history before current retraction collapse, matching
    /// recovered `rseek-datoms` rather than reversing a collected forward
    /// result.
    pub fn reverse_seek_cursor(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        crate::transaction_hints::record_broad_read();
        self.validate_raw_boundary_access(boundary)?;
        if self.direct_current() {
            return self.basis_reverse_boundary_cursor(
                false,
                boundary,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        if self.direct_history() {
            return self.basis_reverse_boundary_cursor(
                true,
                boundary,
                self.read_observer.clone(),
                self.read_context.clone(),
            );
        }
        let order = boundary.order();
        let source =
            self.basis_reverse_boundary_cursor(true, boundary, None, self.read_context.clone())?;
        Ok(DatabaseValueScanCursor {
            inner: DatabaseValueScanCursorInner::Window(Box::new(DatabaseValueWindowCursor::new(
                self,
                DatabaseValueWindowSource::Scan(Box::new(source)),
                order,
                true,
            ))),
            observer: self.read_observer.clone(),
            physical_context: None,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            failed: false,
        })
    }

    /// Explicit collecting convenience over [`Self::reverse_seek_cursor`].
    pub fn collect_reverse_seek_datoms(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<Vec<Datom>, SemanticError> {
        self.reverse_seek_cursor(boundary)?.collect()
    }

    /// Explicit collecting convenience over [`Self::scan_cursor`].
    pub fn collect_datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError> {
        self.scan_cursor(order)?.collect()
    }

    /// Collecting convenience. Streaming callers can
    /// prefer [`Self::scan_cursor`] or name collection explicitly with
    /// [`Self::collect_datoms`].
    pub fn datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError> {
        self.collect_datoms(order)
    }

    /// Query evaluation uses the same public lazy scan semantics.
    pub(crate) fn query_scan_cursor(
        &self,
        order: IndexOrder,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        self.scan_cursor(order)
    }

    /// Query-only AVET lower bound. A strict bound advances past every logical
    /// A/V tie, without inventing a greatest entity/transaction sentinel.
    pub(crate) fn query_avet_start_cursor(
        &self,
        attribute: u32,
        lower: Option<(&Value, bool)>,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        let boundary = IndexBoundary::Avet(match lower {
            Some((value, _)) => IndexComponents::Two(attribute, value.clone()),
            None => IndexComponents::One(attribute),
        });
        self.validate_raw_boundary_access(&boundary)?;
        let after = lower.is_some_and(|(_, inclusive)| !inclusive);
        let source = self.basis_seek_boundary_cursor_biased(
            !self.direct_current(),
            &boundary,
            after,
            if self.direct_current() || self.direct_history() {
                self.read_observer.clone()
            } else {
                None
            },
            self.read_context.clone(),
        )?;
        if self.direct_current() || self.direct_history() {
            return Ok(source);
        }
        Ok(DatabaseValueScanCursor {
            inner: DatabaseValueScanCursorInner::Window(Box::new(DatabaseValueWindowCursor::new(
                self,
                DatabaseValueWindowSource::Scan(Box::new(source)),
                IndexOrder::Avet,
                false,
            ))),
            observer: self.read_observer.clone(),
            physical_context: None,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            failed: false,
        })
    }

    /// Open a lazy cursor over one left-contiguous logical index prefix.
    pub fn prefix_cursor(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        prefix.validate()?;
        if self.direct_current() {
            return self.memoized_prefix_cursor(false, prefix);
        }
        if self.direct_history() {
            return self.memoized_prefix_cursor(true, prefix);
        }
        let source = self.basis_prefix_cursor(true, prefix, None, self.read_context.clone())?;
        Ok(DatabaseValuePrefixCursor {
            inner: DatabaseValuePrefixCursorInner::Window(Box::new(
                DatabaseValueWindowCursor::new(
                    self,
                    DatabaseValueWindowSource::Prefix(Box::new(source)),
                    prefix.order(),
                    false,
                ),
            )),
            observer: self.read_observer.clone(),
            physical_context: None,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            memo_source: None,
            memo_hit: false,
            failed: false,
        })
    }

    /// Explicit collecting convenience over [`Self::prefix_cursor`].
    pub fn collect_datoms_with_prefix(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<Vec<Datom>, SemanticError> {
        self.prefix_cursor(prefix)?.collect()
    }

    /// Collecting convenience. Streaming callers can
    /// prefer [`Self::prefix_cursor`] or [`Self::collect_datoms_with_prefix`].
    pub fn datoms_with_prefix(&self, prefix: &IndexPrefix) -> Result<Vec<Datom>, SemanticError> {
        self.collect_datoms_with_prefix(prefix)
    }

    /// Query and persisted-program execution use the same public lazy prefix
    /// semantics.
    pub(crate) fn query_prefix_cursor(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        self.prefix_cursor(prefix)
    }

    /// Lazily read one left-contiguous prefix of an unfiltered current value.
    ///
    /// Transaction processing requires exactly this point-current capability.
    /// Temporal, history, and custom-filter values use [`Self::prefix_cursor`]
    /// so this stricter transaction-only entry point remains unambiguous.
    pub fn current_prefix_cursor(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        prefix.validate()?;
        if !self.direct_current() {
            return Err(SemanticError::incorrect(
                "database/prefix-cursor-requires-current",
                "lazy transaction prefix access requires an unfiltered current database value",
            ));
        }
        self.memoized_prefix_cursor(false, prefix)
    }

    /// Lazily probe historical data. A caller that consumes only the first
    /// item deliberately does not populate the complete-prefix memo.
    pub(crate) fn history_prefix_cursor(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        prefix.validate()?;
        self.memoized_prefix_cursor(true, prefix)
    }

    pub(super) fn basis_seek_boundary_cursor(
        &self,
        history: bool,
        boundary: &IndexBoundary,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        let normalized = boundary.normalized()?;
        let order = normalized.order();
        match &self.basis {
            ReadBasis::Eager(database) => {
                let datoms = database.index_datoms(history, order);
                let start = datoms.partition_point(|datom| normalized.compare_datom(datom).is_lt());
                Ok(DatabaseValueScanCursor {
                    inner: DatabaseValueScanCursorInner::Eager(datoms[start..].iter().cloned()),
                    observer,
                    physical_context: None,
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    failed: false,
                })
            }
            ReadBasis::Block(snapshot) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Block(Box::new(
                    snapshot.boundary_cursor(history, boundary, false)?,
                )),
                observer,
                physical_context,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
            ReadBasis::TransactionOverlay(overlay) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Overlay(Box::new(overlay.scan_cursor_from(
                    history,
                    order,
                    observer,
                    physical_context,
                    Some((boundary, false, false)),
                )?)),
                observer: None,
                physical_context: None,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
        }
    }

    pub(super) fn basis_seek_boundary_cursor_biased(
        &self,
        history: bool,
        boundary: &IndexBoundary,
        after: bool,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        if !after {
            return self.basis_seek_boundary_cursor(history, boundary, observer, physical_context);
        }
        let normalized = boundary.normalized()?.after_prefix();
        let order = normalized.order();
        let inner = match &self.basis {
            ReadBasis::Eager(database) => {
                let datoms = database.index_datoms(history, order);
                let start = datoms.partition_point(|datom| normalized.compare_datom(datom).is_lt());
                DatabaseValueScanCursorInner::Eager(datoms[start..].iter().cloned())
            }
            ReadBasis::Block(snapshot) => DatabaseValueScanCursorInner::Block(Box::new(
                snapshot.normalized_cursor(history, normalized, false),
            )),
            ReadBasis::TransactionOverlay(overlay) => {
                return Ok(DatabaseValueScanCursor {
                    inner: DatabaseValueScanCursorInner::Overlay(Box::new(
                        overlay.scan_cursor_from(
                            history,
                            order,
                            observer,
                            physical_context,
                            Some((boundary, false, true)),
                        )?,
                    )),
                    observer: None,
                    physical_context: None,
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    failed: false,
                });
            }
        };
        Ok(DatabaseValueScanCursor {
            inner,
            observer,
            physical_context,
            physical_recorded: false,
            operation: crate::sql_io::OperationContext::current(),
            failed: false,
        })
    }

    pub(super) fn basis_reverse_boundary_cursor(
        &self,
        history: bool,
        boundary: &IndexBoundary,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        let normalized = boundary.normalized()?;
        let order = normalized.order();
        match &self.basis {
            ReadBasis::Eager(database) => {
                let datoms = database.index_datoms(history, order);
                let end = datoms.partition_point(|datom| !normalized.compare_datom(datom).is_gt());
                Ok(DatabaseValueScanCursor {
                    inner: DatabaseValueScanCursorInner::EagerReverse(
                        datoms[..end].iter().rev().cloned(),
                    ),
                    observer,
                    physical_context: None,
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    failed: false,
                })
            }
            ReadBasis::Block(snapshot) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Block(Box::new(
                    snapshot.boundary_cursor(history, boundary, true)?,
                )),
                observer,
                physical_context,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
            ReadBasis::TransactionOverlay(overlay) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Overlay(Box::new(overlay.scan_cursor_from(
                    history,
                    order,
                    observer,
                    physical_context,
                    Some((boundary, true, false)),
                )?)),
                observer: None,
                physical_context: None,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
        }
    }

    pub(super) fn basis_scan_cursor(
        &self,
        history: bool,
        order: IndexOrder,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<DatabaseValueScanCursor<'_>, SemanticError> {
        match &self.basis {
            ReadBasis::Eager(database) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Eager(
                    database.index_datoms(history, order).iter().cloned(),
                ),
                observer,
                physical_context: None,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
            ReadBasis::Block(snapshot) => Ok(DatabaseValueScanCursor {
                inner: DatabaseValueScanCursorInner::Block(Box::new(
                    snapshot.cursor(history, order),
                )),
                observer,
                physical_context,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
            ReadBasis::TransactionOverlay(overlay) => Ok(DatabaseValueScanCursor {
                // The overlay owns observation so an AVET-enablement source
                // range can be charged while it is reordered, without later
                // double-charging those precharged datoms.
                inner: DatabaseValueScanCursorInner::Overlay(Box::new(overlay.scan_cursor(
                    history,
                    order,
                    observer,
                    physical_context,
                )?)),
                observer: None,
                physical_context: None,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                failed: false,
            }),
        }
    }

    pub(super) fn basis_prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
        observer: Option<Arc<LogicalReadObserver>>,
        physical_context: Option<Arc<TransactionReadContext>>,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        match &self.basis {
            ReadBasis::Eager(database) => {
                let datoms = if history {
                    database.history_with_prefix(prefix)?
                } else {
                    database.datoms_with_prefix(prefix)?
                };
                Ok(DatabaseValuePrefixCursor {
                    inner: DatabaseValuePrefixCursorInner::Eager(datoms.iter().cloned()),
                    observer,
                    physical_context: None,
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    memo_source: None,
                    memo_hit: false,
                    failed: false,
                })
            }
            ReadBasis::Block(snapshot) => Ok(DatabaseValuePrefixCursor {
                inner: DatabaseValuePrefixCursorInner::Block(Box::new(
                    snapshot.prefix_cursor(history, prefix)?,
                )),
                observer,
                physical_context,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                memo_source: None,
                memo_hit: false,
                failed: false,
            }),
            ReadBasis::TransactionOverlay(overlay) => {
                Ok(DatabaseValuePrefixCursor {
                    // As with full scans, the overlay owns observation so its one
                    // necessarily reordered AVET backfill is bounded at source.
                    inner: DatabaseValuePrefixCursorInner::Overlay(Box::new(
                        overlay.prefix_cursor(history, prefix, observer, physical_context)?,
                    )),
                    observer: None,
                    physical_context: None,
                    physical_recorded: false,
                    operation: crate::sql_io::OperationContext::current(),
                    memo_source: None,
                    memo_hit: false,
                    failed: false,
                })
            }
        }
    }
}
