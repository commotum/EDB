use crate::{DatabaseValue, ErrorCategory, IndexOrder, QueryControl, SemanticError};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// History-datom count for one numeric attribute identity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AttributeStats {
    pub count: u64,
}

/// Counts from one immutable database value's history index.
///
/// Assertions and retractions both contribute; this is not current entity
/// cardinality, PostgreSQL storage size, or deep integrity inspection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabaseStats {
    pub datoms: u64,
    pub attrs: BTreeMap<u32, AttributeStats>,
}

impl DatabaseValue {
    /// Count history datoms and their attributes in this exact immutable view.
    ///
    /// Temporal and custom filters remain in force. This implementation streams
    /// AEVT, retaining only one count per encountered attribute, rather than
    /// materializing the database or inspecting every physical storage object.
    /// It is a history scan, not a constant-time metadata operation. Filtered
    /// and temporal views may read substantially more candidates than they count.
    pub fn db_stats(&self) -> Result<DatabaseStats, SemanticError> {
        self.db_stats_with_control(&QueryControl::default())
    }

    /// A controlled history count using `timeout`, `cancel`, and `max_work`.
    ///
    /// Work counts visible history datoms, not hidden filtered candidates.
    /// Query row limits and planner options do not apply. Cancellation/deadline
    /// are checked before and after every cursor step, including an empty scan;
    /// they do not interrupt an in-flight storage read or user filter callback.
    /// Errors return no partial count and leave the database unchanged.
    pub fn db_stats_with_control(
        &self,
        control: &QueryControl,
    ) -> Result<DatabaseStats, SemanticError> {
        let started = Instant::now();
        check(control, started)?;
        let history = self.clone().history();
        let mut cursor = history.scan_cursor(IndexOrder::Aevt)?;
        let mut stats = DatabaseStats::default();
        let mut work = 0_usize;
        loop {
            check(control, started)?;
            let next = cursor.next();
            check(control, started)?;
            let Some(datom) = next else { return Ok(stats) };
            let datom = datom?;
            work = work.checked_add(1).ok_or_else(work_limit)?;
            if work > control.max_work {
                return Err(work_limit());
            }
            stats.datoms = stats.datoms.checked_add(1).ok_or_else(count_overflow)?;
            let attribute = stats.attrs.entry(datom.attribute).or_default();
            attribute.count = attribute.count.checked_add(1).ok_or_else(count_overflow)?;
        }
    }
}

fn check(control: &QueryControl, started: Instant) -> Result<(), SemanticError> {
    if control.cancel.load(Ordering::Relaxed) {
        return Err(SemanticError::new(
            ErrorCategory::Interrupted,
            "database-stats/canceled",
            "database statistics were canceled",
        ));
    }
    // Compare elapsed duration instead of constructing a potentially overflowing
    // Instant deadline from an arbitrary caller-supplied duration.
    if control
        .timeout
        .is_some_and(|timeout| started.elapsed() >= timeout)
    {
        return Err(SemanticError::new(
            ErrorCategory::Interrupted,
            "database-stats/timeout",
            "database statistics deadline elapsed",
        ));
    }
    Ok(())
}

fn work_limit() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "database-stats/work-limit",
        "database statistics exceeded the visible history-datom work limit",
    )
}

fn count_overflow() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Fault,
        "database-stats/count-overflow",
        "database statistics count exceeds u64",
    )
}
