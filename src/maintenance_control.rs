//! Cooperative pacing outside durable work batches. No scheduler or I/O owner.
use crate::{ErrorCategory, SemanticError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceStats {
    pub completed_batches: u64,
    pub pauses: u64,
    /// Actual wall time spent in pacing, not source I/O or CPU time.
    pub paused_nanos: u64,
}

#[derive(Default, Debug)]
struct Counters {
    completed_batches: AtomicU64,
    pauses: AtomicU64,
    paused_nanos: AtomicU64,
}

/// Optional delay after each successfully committed/copied maintenance batch.
/// Clones share cancellation and counters, but delays apply to each caller;
/// this is not a process-wide bandwidth limiter. No permit or SQL lock is held
/// by this helper. Callers choose safe boundaries outside their transactions.
#[derive(Clone, Debug)]
pub struct MaintenanceControl {
    pause: Duration,
    cancel: Arc<AtomicBool>,
    counters: Arc<Counters>,
}

impl Default for MaintenanceControl {
    fn default() -> Self {
        Self {
            pause: Duration::ZERO,
            cancel: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
        }
    }
}

impl MaintenanceControl {
    pub fn new(pause: Duration, cancel: Arc<AtomicBool>) -> Result<Self, SemanticError> {
        if Instant::now().checked_add(pause).is_none() {
            return Err(SemanticError::incorrect(
                "maintenance/invalid-pause",
                "maintenance pause exceeds the monotonic clock range",
            ));
        }
        Ok(Self {
            pause,
            cancel,
            counters: Arc::new(Counters::default()),
        })
    }

    pub fn pause(&self) -> Duration {
        self.pause
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<(), SemanticError> {
        if self.cancel.load(Ordering::Acquire) {
            Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "maintenance/canceled",
                "maintenance canceled between work batches; completed work is retained",
            ))
        } else {
            Ok(())
        }
    }

    /// Records the completed batch even if cancellation is now requested.
    /// Checks cancellation between short sleeps. Blocking database/filesystem
    /// calls and CPU work within a batch keep their existing interruption rules.
    pub fn after_batch(&self) -> Result<(), SemanticError> {
        self.counters
            .completed_batches
            .fetch_add(1, Ordering::Relaxed);
        self.check()?;
        if self.pause.is_zero() {
            return Ok(());
        }
        let begin = Instant::now();
        self.counters.pauses.fetch_add(1, Ordering::Relaxed);
        let result = loop {
            if let Err(error) = self.check() {
                break Err(error);
            }
            let remaining = self.pause.saturating_sub(begin.elapsed());
            if remaining.is_zero() {
                break Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        };
        self.counters.paused_nanos.fetch_add(
            begin.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        result
    }

    pub fn stats(&self) -> MaintenanceStats {
        MaintenanceStats {
            completed_batches: self.counters.completed_batches.load(Ordering::Relaxed),
            pauses: self.counters.pauses.load(Ordering::Relaxed),
            paused_nanos: self.counters.paused_nanos.load(Ordering::Relaxed),
        }
    }
}
