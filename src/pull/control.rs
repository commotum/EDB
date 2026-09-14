use super::*;

#[derive(Clone, Debug)]
pub struct PullControl {
    /// Explicit traversal depth policy, with the root at zero. The default
    /// imposes no practical cap; selector recursion limits remain independent.
    pub max_depth: usize,
    /// Explicit number of expanded entity occurrences, counting repeated
    /// branches independently. The default imposes no practical cap.
    pub max_entities: usize,
    pub cancel: Arc<AtomicBool>,
}

impl Default for PullControl {
    fn default() -> Self {
        Self {
            max_depth: usize::MAX,
            max_entities: usize::MAX,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub(super) struct PullState<'a, 'cancel> {
    pub(super) control: &'a PullControl,
    pub(super) query_budget: Option<&'a mut QueryPullBudget<'cancel>>,
    /// Cycle and depth state belongs to one lexical recursive selector. An
    /// ordinary nested pull, or a different recursive selector in the same
    /// pattern, must not consume this state.
    pub(super) recursions: BTreeMap<RecursionKey, RecursionState>,
    pub(super) entities: usize,
}

/// Shared execution budget used when pull is a query result transformation.
///
/// Datomic's query timeout covers pull work too. Keeping this state separate
/// from the public `PullControl` lets every pull expression in one query share
/// the already-consumed query work and the original absolute deadline.
pub(crate) struct QueryPullBudget<'a> {
    pub(super) cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
    max_work: usize,
    work: usize,
    borrowed_cancel: Option<&'a AtomicBool>,
    value_bytes: usize,
    max_value_bytes: usize,
}

impl<'a> QueryPullBudget<'a> {
    pub(crate) fn new(
        cancel: Arc<AtomicBool>,
        deadline: Option<Instant>,
        max_work: usize,
        work: usize,
    ) -> Self {
        Self {
            cancel,
            deadline,
            max_work,
            work,
            borrowed_cancel: None,
            value_bytes: 0,
            max_value_bytes: usize::MAX,
        }
    }

    pub(crate) fn with_borrowed_cancel(mut self, cancel: Option<&'a AtomicBool>) -> Self {
        self.borrowed_cancel = cancel;
        self
    }

    pub(crate) fn with_value_budget(mut self, used: usize, max: usize) -> Self {
        self.value_bytes = used;
        self.max_value_bytes = max;
        self
    }

    pub(crate) fn value_bytes(&self) -> usize {
        self.value_bytes
    }

    pub(crate) fn remaining_value_bytes(&self) -> usize {
        self.max_value_bytes.saturating_sub(self.value_bytes)
    }

    pub(crate) fn charge_value_bytes(&mut self, bytes: usize) -> Result<(), SemanticError> {
        self.value_bytes = self.value_bytes.saturating_add(bytes);
        if self.value_bytes > self.max_value_bytes {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "query/value-byte-limit",
                "query projection exceeded its shared value allocation allowance",
            ));
        }
        Ok(())
    }

    pub(crate) fn work(&self) -> usize {
        self.work
    }

    pub(crate) fn check(&mut self, amount: usize) -> Result<(), SemanticError> {
        self.work = self.work.saturating_add(amount);
        if self.cancel.load(Ordering::Relaxed)
            || self
                .borrowed_cancel
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "query/canceled",
                "query was canceled",
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "query/timeout",
                "query deadline elapsed",
            ));
        }
        if self.work > self.max_work {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "query/work-limit",
                "query exceeded its work limit",
            ));
        }
        Ok(())
    }
}

impl PullState<'_, '_> {
    pub(super) fn read_candidate(&mut self, datom: Option<&Datom>) -> Result<bool, SemanticError> {
        // Polls are traversal work too: an overlay may skip many removed base
        // facts before yielding a candidate. Charge those advances, not only
        // datoms that happen to survive its merge/window.
        self.check(1)?;
        if let Some(datom) = datom {
            // Cursor items are owned. Charge their actual inline/heap payload
            // before view filtering and before retaining values or tasks. A
            // filtered-out candidate still consumed work and an owned item.
            self.charge_value_bytes(usize::try_from(datom.retained_bytes()).unwrap_or(usize::MAX))?;
        }
        Ok(true)
    }

    pub(super) fn charge_value_bytes(&mut self, bytes: usize) -> Result<(), SemanticError> {
        if let Some(budget) = self.query_budget.as_deref_mut() {
            budget.charge_value_bytes(bytes)?;
        }
        Ok(())
    }
    pub(super) fn check(&mut self, amount: usize) -> Result<(), SemanticError> {
        if let Some(budget) = self.query_budget.as_deref_mut() {
            return budget.check(amount);
        }
        if self.control.cancel.load(Ordering::Relaxed) {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "pull/canceled",
                "pull was canceled",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub(super) struct RecursionState {
    pub(super) depth: usize,
    pub(super) seen: BTreeSet<u64>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RecursionKey {
    Selector(Vec<usize>),
    Component(Vec<usize>),
}
