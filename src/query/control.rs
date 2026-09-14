//! One invocation owns cancellation, deadlines, row limits and cumulative value admission.
use super::*;

#[derive(Clone, Debug)]
pub struct QueryControl {
    pub timeout: Option<Duration>,
    pub max_work: usize,
    pub max_intermediate_rows: usize,
    pub max_result_rows: usize,
    pub cancel: Arc<AtomicBool>,
    /// Diagnostic scan mode: disable hash joins/grouped probes and evaluate
    /// native database patterns through EAVT scans.
    pub force_scan: bool,
    /// Auxiliary hash tables and native probe groups are chunked using this
    /// retention allowance. One ordinary native probe is always admitted even
    /// when its key exceeds the allowance; valid data is not rejected. Zero
    /// disables hash joins and groups native probes one row at a time.
    pub max_join_bytes: usize,
    /// Maximum estimated temporary/output allocation of an exact numeric
    /// operation. Compact decimal exponents do not themselves consume bytes.
    /// Program queries additionally obey their shared remaining value budget.
    pub max_numeric_bytes: usize,
    /// Cumulative admitted query-value and general-key scratch allocation.
    /// Inputs/sources remain borrowed until selected; nested/program queries
    /// share the remaining allowance rather than resetting it.
    pub max_value_bytes: usize,
    /// Independent bounded metadata capture. Does not affect semantic budgets.
    pub diagnostics: Option<QueryDiagnosticOptions>,
}

impl Default for QueryControl {
    fn default() -> Self {
        Self {
            timeout: None,
            max_work: usize::MAX,
            max_intermediate_rows: usize::MAX,
            max_result_rows: usize::MAX,
            cancel: Arc::new(AtomicBool::new(false)),
            force_scan: false,
            max_join_bytes: 4 * 1024 * 1024,
            max_numeric_bytes: 16 * 1024 * 1024,
            max_value_bytes: usize::MAX,
            diagnostics: None,
        }
    }
}

pub(super) struct State<'a> {
    pub(super) sources: BTreeMap<&'a str, SourceRef<'a>>,
    pub(super) control: &'a QueryControl,
    pub(super) deadline: Option<Instant>,
    pub(super) work: usize,
    pub(super) stats: QueryStats,
    pub(super) plan: Vec<PlanStep>,
    pub(super) extensions: Option<&'a QueryExtensions>,
    pub(super) rule_memo: BTreeMap<RuleInvocationKey, Vec<Vec<BoundValue>>>,
    pub(super) negative_memo: BTreeMap<dependencies::NegativeInvocation, bool>,
    pub(super) solving_rules: bool,
    pub(super) rule_memo_complete: bool,
    pub(super) defer_rules: bool,
    pub(super) borrowed_cancel: Option<&'a AtomicBool>,
    pub(super) max_value_bytes: usize,
    pub(super) diagnostics: Option<diagnostics::Trace>,
}

pub(super) fn resource(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Busy, code, message)
}

pub(crate) fn query_value_allocation_bytes(value: &QueryValue) -> usize {
    join::query_value_bytes(value)
}
pub(super) fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}

impl State<'_> {
    pub(super) fn compare_keys(
        &mut self,
        left: QueryValueRef<'_>,
        right: QueryValueRef<'_>,
    ) -> Result<std::cmp::Ordering, SemanticError> {
        if !matches!(left, QueryValueRef::Query(_)) && !matches!(right, QueryValueRef::Query(_)) {
            return Ok(left.logical_cmp(right));
        }
        self.prepare_key(left)?;
        self.prepare_key(right)?;
        left.logical_cmp_with(right, &mut |work| self.check(work))
    }
    pub(super) fn validate_general(&mut self, value: &QueryValue) -> Result<(), SemanticError> {
        let size: QueryValueSize = value.measure_with(&mut |work| self.check(work))?;
        if size.retained_bytes
            > self
                .max_value_bytes
                .saturating_sub(self.stats.allocated_value_bytes)
        {
            return Err(resource(
                "query/value-byte-limit",
                "general query value exceeds remaining byte allowance",
            ));
        }
        self.charge_value_bytes(size.canonical_bytes)?;
        value.validate_with(&mut |work| self.check(work))
    }

    pub(super) fn prepare_key(&mut self, value: QueryValueRef<'_>) -> Result<(), SemanticError> {
        if let QueryValueRef::Query(value) = value {
            self.validate_general(value)?;
            // Validation and the subsequent comparison/hash each build their
            // own bounded canonical arena; no borrowed corpus bytes are copied.
            let size = value.measure_with(&mut |work| self.check(work))?;
            self.charge_value_bytes(size.canonical_bytes)?;
        }
        Ok(())
    }

    pub(super) fn admit_ref(&mut self, value: QueryValueRef<'_>) -> Result<(), SemanticError> {
        match value {
            QueryValueRef::Nil => self.charge_value_bytes(std::mem::size_of::<BoundValue>()),
            QueryValueRef::Stored(value) => self.charge_value_bytes(
                std::mem::size_of::<BoundValue>()
                    .saturating_add(value.retained_heap_bytes() as usize),
            ),
            QueryValueRef::Query(value) => {
                self.validate_general(value)?;
                self.charge_value_bytes(value.retained_bytes())
            }
        }
    }
    pub(super) fn check(&mut self, amount: usize) -> Result<(), SemanticError> {
        self.work = self.work.saturating_add(amount);
        if self.control.cancel.load(AtomicOrdering::Relaxed)
            || self
                .borrowed_cancel
                .is_some_and(|cancel| cancel.load(AtomicOrdering::Relaxed))
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
        if self.work > self.control.max_work {
            return Err(resource(
                "query/work-limit",
                "query exceeded its work limit",
            ));
        }
        Ok(())
    }

    pub(super) fn check_row_count(&self, count: usize) -> Result<(), SemanticError> {
        if count > self.control.max_intermediate_rows {
            return Err(resource(
                "query/intermediate-limit",
                "query relation exceeded its row limit",
            ));
        }
        Ok(())
    }

    pub(super) fn charge_value_bytes(&mut self, bytes: usize) -> Result<(), SemanticError> {
        self.stats.allocated_value_bytes = self.stats.allocated_value_bytes.saturating_add(bytes);
        if self.stats.allocated_value_bytes > self.max_value_bytes {
            return Err(resource(
                "query/value-byte-limit",
                "query exceeded its shared value allocation allowance",
            ));
        }
        Ok(())
    }

    pub(super) fn push_row(&mut self, rows: &mut Vec<Row>, row: Row) -> Result<(), SemanticError> {
        if rows.len() >= self.control.max_intermediate_rows {
            *rows = dedupe_rows(std::mem::take(rows), self)?;
            if rows.iter().any(|existing| existing == &row) {
                return Ok(());
            }
        }
        self.check_row_count(rows.len().saturating_add(1))?;
        self.charge_value_bytes(join::row_bytes(&row))?;
        rows.push(row);
        Ok(())
    }
}
