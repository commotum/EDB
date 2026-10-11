//! Fixed blocking workers behind runtime-independent wakeable completions.
use crate::{ErrorCategory, OperationContext, SemanticError};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send + 'static>;
type Callback<T> = Box<dyn FnOnce(Result<T, SemanticError>) + Send + 'static>;

/// Operational limits, not database/query semantic limits. Completed but
/// unconsumed operations count against `max_operations`. Clients and streams
/// reserve resources until their worker-side cleanup finishes.
#[derive(Clone, Copy, Debug)]
pub struct AsyncConfig {
    pub workers: usize,
    pub max_operations: usize,
    pub max_resources: usize,
}
impl Default for AsyncConfig {
    fn default() -> Self {
        Self {
            workers: 4,
            max_operations: 64,
            max_resources: 64,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AsyncStats {
    pub operations: usize,
    pub resources: usize,
    pub queued: usize,
    pub running: usize,
    pub peak_operations: usize,
    pub peak_running: usize,
    pub completed: u64,
    pub rejected: u64,
    pub cleaned: u64,
    pub worker_panics: u64,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    cleanup: VecDeque<Job>,
    closed: bool,
}
struct Shared {
    config: AsyncConfig,
    queue: Mutex<Queue>,
    ready: Condvar,
    admission: Mutex<BTreeMap<u64, Waker>>,
    next_stream: AtomicU64,
    operations: AtomicUsize,
    resources: AtomicUsize,
    queued: AtomicUsize,
    running: AtomicUsize,
    peak_operations: AtomicUsize,
    peak_running: AtomicUsize,
    completed: AtomicU64,
    rejected: AtomicU64,
    cleaned: AtomicU64,
    panics: AtomicU64,
}
struct Lifetime(Arc<Shared>);
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed = true;
        self.0.ready.notify_all();
        // Workers drain admitted jobs/cleanup and exit themselves. Never join
        // a SQL-blocked worker from an executor's Drop implementation.
    }
}

/// Cloneable bounded executor. It does not require or enter a Tokio runtime.
/// Construct it during application setup; each instance creates exactly the
/// configured number of blocking threads, never a thread per operation.
#[derive(Clone)]
pub struct AsyncExecutor {
    shared: Arc<Shared>,
    _lifetime: Arc<Lifetime>,
}
impl fmt::Debug for AsyncExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncExecutor")
            .field("config", &self.shared.config)
            .field("stats", &self.stats())
            .finish()
    }
}
impl AsyncExecutor {
    pub fn new(config: AsyncConfig) -> Result<Self, SemanticError> {
        if config.workers == 0 || config.max_operations == 0 || config.max_resources == 0 {
            return Err(SemanticError::incorrect(
                "async/config",
                "worker, operation and resource limits must be positive",
            ));
        }
        let shared = Arc::new(Shared {
            config,
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            admission: Mutex::new(BTreeMap::new()),
            next_stream: AtomicU64::new(1),
            operations: AtomicUsize::new(0),
            resources: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            peak_operations: AtomicUsize::new(0),
            peak_running: AtomicUsize::new(0),
            completed: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            cleaned: AtomicU64::new(0),
            panics: AtomicU64::new(0),
        });
        let executor = Self {
            shared: shared.clone(),
            _lifetime: Arc::new(Lifetime(shared.clone())),
        };
        for index in 0..config.workers {
            let worker = shared.clone();
            std::thread::Builder::new()
                .name(format!("edb-async-{index}"))
                .spawn(move || worker_loop(worker))
                .map_err(|error| {
                    SemanticError::new(
                        ErrorCategory::Unavailable,
                        "async/worker-start",
                        error.to_string(),
                    )
                })?;
        }
        Ok(executor)
    }

    pub fn stats(&self) -> AsyncStats {
        let s = &self.shared;
        AsyncStats {
            operations: s.operations.load(Ordering::Acquire),
            resources: s.resources.load(Ordering::Acquire),
            queued: s.queued.load(Ordering::Acquire),
            running: s.running.load(Ordering::Acquire),
            peak_operations: s.peak_operations.load(Ordering::Relaxed),
            peak_running: s.peak_running.load(Ordering::Relaxed),
            completed: s.completed.load(Ordering::Relaxed),
            rejected: s.rejected.load(Ordering::Relaxed),
            cleaned: s.cleaned.load(Ordering::Relaxed),
            worker_panics: s.panics.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn reserve(&self) -> Result<Reservation, SemanticError> {
        let count = &self.shared.operations;
        #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
        let previous = count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < self.shared.config.max_operations).then_some(value + 1)
        });
        let Ok(previous) = previous else {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "async/operation-capacity",
                "all async operation slots are in use",
            ));
        };
        self.shared
            .peak_operations
            .fetch_max(previous + 1, Ordering::Relaxed);
        Ok(Reservation {
            executor: self.clone(),
            permit: Permit {
                shared: self.shared.clone(),
                resource: false,
            },
        })
    }

    pub(crate) fn retain<T: Send + Sync + 'static>(
        &self,
        make: impl FnOnce() -> T,
    ) -> Result<Owned<T>, SemanticError> {
        #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
        self.shared
            .resources
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < self.shared.config.max_resources).then_some(value + 1)
            })
            .map_err(|_| {
                SemanticError::new(
                    ErrorCategory::Busy,
                    "async/resource-capacity",
                    "all async retained-resource slots are in use",
                )
            })?;
        let permit = Permit {
            shared: self.shared.clone(),
            resource: true,
        };
        Ok(Owned(Arc::new(Deferred {
            value: Some(make()),
            permit: Some(permit),
            executor: self.clone(),
        })))
    }

    pub(crate) fn cleanup(&self, job: impl FnOnce() + Send + 'static) {
        // Every call is backed by a still-live operation/resource reservation.
        // No arbitrary public producer can append cleanup jobs. Thus cleanup
        // cannot grow with an unbounded number of abandoned unadmitted jobs.
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cleanup
            .push_back(Box::new(job));
        self.shared.ready.notify_one();
    }
    pub(crate) fn stream_id(&self) -> u64 {
        self.shared.next_stream.fetch_add(1, Ordering::Relaxed)
    }
    pub(crate) fn watch_admission(&self, id: u64, waker: &Waker) {
        self.shared
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, waker.clone());
    }
    pub(crate) fn unwatch_admission(&self, id: u64) {
        self.shared
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let (job, cleanup) = {
            let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(job) = queue.cleanup.pop_front() {
                    break (job, true);
                }
                if let Some(job) = queue.jobs.pop_front() {
                    shared.queued.fetch_sub(1, Ordering::AcqRel);
                    break (job, false);
                }
                if queue.closed {
                    return;
                }
                queue = shared.ready.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        let running = shared.running.fetch_add(1, Ordering::AcqRel) + 1;
        shared.peak_running.fetch_max(running, Ordering::Relaxed);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            shared.panics.fetch_add(1, Ordering::Relaxed);
        }
        shared.running.fetch_sub(1, Ordering::AcqRel);
        if cleanup {
            shared.cleaned.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Permit {
    shared: Arc<Shared>,
    resource: bool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        if self.resource {
            self.shared.resources.fetch_sub(1, Ordering::AcqRel);
        } else {
            self.shared.operations.fetch_sub(1, Ordering::AcqRel);
            let wakers = std::mem::take(
                &mut *self
                    .shared
                    .admission
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            );
            for (_, waker) in wakers {
                waker.wake();
            }
        }
    }
}

pub(crate) struct Owned<T: Send + Sync + 'static>(Arc<Deferred<T>>);
impl<T: Send + Sync + 'static> Clone for Owned<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Send + Sync + 'static> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.value.as_ref().expect("live retained async value")
    }
}
struct Deferred<T: Send + Sync + 'static> {
    value: Option<T>,
    permit: Option<Permit>,
    executor: AsyncExecutor,
}
impl<T: Send + Sync + 'static> Drop for Deferred<T> {
    fn drop(&mut self) {
        let value = self.value.take();
        let permit = self.permit.take();
        self.executor.cleanup(move || {
            drop(value);
            drop(permit);
        });
    }
}

pub(crate) struct Reservation {
    executor: AsyncExecutor,
    permit: Permit,
}
impl Reservation {
    pub(crate) fn run<T: Send + 'static>(
        self,
        deadline: Option<Instant>,
        transaction: bool,
        run: impl FnOnce() -> Result<T, SemanticError> + Send + 'static,
    ) -> AsyncOperation<T> {
        let executor = self.executor.clone();
        let cell = Arc::new(Completion {
            state: Mutex::new(CompletionState {
                result: None,
                waker: None,
                callback: None,
            }),
            abandoned: AtomicBool::new(false),
            executor: self.executor,
            _permit: self.permit,
        });
        let worker = cell.clone();
        let context = OperationContext::current();
        let job = Box::new(move || {
            let _scope = context.as_ref().map(OperationContext::enter);
            if !transaction && worker.abandoned.load(Ordering::Acquire) {
                return;
            }
            let result = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                Err(deadline_error())
            } else {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
                    worker
                        .executor
                        .shared
                        .panics
                        .fetch_add(1, Ordering::Relaxed);
                    Err(SemanticError::new(
                        if transaction {
                            ErrorCategory::UnknownOutcome
                        } else {
                            ErrorCategory::Fault
                        },
                        "async/worker-panic",
                        "async operation panicked; transaction outcome, if attempted, is unknown",
                    ))
                })
            };
            worker
                .executor
                .shared
                .completed
                .fetch_add(1, Ordering::Relaxed);
            worker.finish(result);
        });
        executor.shared.queued.fetch_add(1, Ordering::AcqRel);
        executor
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jobs
            .push_back(job);
        executor.shared.ready.notify_one();
        AsyncOperation { cell: Some(cell) }
    }
}

struct CompletionState<T> {
    result: Option<Result<T, SemanticError>>,
    waker: Option<Waker>,
    callback: Option<Callback<T>>,
}
struct Completion<T> {
    state: Mutex<CompletionState<T>>,
    abandoned: AtomicBool,
    executor: AsyncExecutor,
    _permit: Permit,
}
impl<T: Send + 'static> Completion<T> {
    fn finish(&self, result: Result<T, SemanticError>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.abandoned.load(Ordering::Acquire) {
            drop(state);
            drop(result);
            return;
        }
        if let Some(callback) = state.callback.take() {
            drop(state);
            callback(result);
            return;
        }
        state.result = Some(result);
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// One result. Polling only transfers an already-completed result or registers
/// a waker. Dropping an unconsumed result schedules worker-side disposal, even
/// when that result owns native I/O resources. Once yielded, `T` retains
/// its ordinary native methods and destructor contract.
pub struct AsyncOperation<T: Send + 'static> {
    cell: Option<Arc<Completion<T>>>,
}
impl<T: Send + 'static> Unpin for AsyncOperation<T> {}
impl<T: Send + 'static> fmt::Debug for AsyncOperation<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncOperation")
            .field("consumed", &self.cell.is_none())
            .finish()
    }
}
impl<T: Send + 'static> AsyncOperation<T> {
    /// Consume this operation through a once-only callback instead of awaiting
    /// it. The callback runs on an async blocking worker, including when the
    /// operation completed before registration. Callback panics are contained.
    pub fn on_complete(mut self, callback: impl FnOnce(Result<T, SemanticError>) + Send + 'static) {
        let cell = self.cell.take().expect("unconsumed operation");
        let executor = cell.executor.clone();
        executor.cleanup(move || {
            let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(result) = state.result.take() {
                drop(state);
                callback(result);
            } else {
                state.callback = Some(Box::new(callback));
            }
        });
    }
}
impl<T: Send + 'static> Future for AsyncOperation<T> {
    type Output = Result<T, SemanticError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let cell = self
            .cell
            .as_ref()
            .expect("completed operation polled again");
        let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = state.result.take() {
            drop(state);
            self.cell.take();
            Poll::Ready(result)
        } else {
            if !state
                .waker
                .as_ref()
                .is_some_and(|waker| waker.will_wake(cx.waker()))
            {
                state.waker = Some(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}
impl<T: Send + 'static> Drop for AsyncOperation<T> {
    fn drop(&mut self) {
        if let Some(cell) = self.cell.take() {
            cell.abandoned.store(true, Ordering::Release);
            let executor = cell.executor.clone();
            executor.cleanup(move || drop(cell));
        }
    }
}

pub(crate) fn deadline(timeout: Option<Duration>) -> Result<Option<Instant>, SemanticError> {
    timeout
        .map(|timeout| {
            Instant::now().checked_add(timeout).ok_or_else(|| {
                SemanticError::incorrect("async/timeout", "timeout is not representable")
            })
        })
        .transpose()
}
pub(crate) fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
}
pub(crate) fn deadline_error() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Interrupted,
        "async/deadline",
        "async operation deadline elapsed before the next operation step",
    )
}
