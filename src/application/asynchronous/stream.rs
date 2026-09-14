use crate::SemanticError;
use crate::runtime::executor::{AsyncExecutor, AsyncOperation, Owned, deadline_error};
use futures_core::Stream;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// At most one chunk is computed per demand. Individual values/transactions
/// retain existing query/codec limits; chunk size is a row count, not an RSS cap.
#[derive(Clone, Copy, Debug)]
pub struct AsyncStreamOptions {
    pub chunk_rows: usize,
    pub timeout: Option<Duration>,
}
impl Default for AsyncStreamOptions {
    fn default() -> Self {
        Self {
            chunk_rows: 64,
            timeout: None,
        }
    }
}
type Cursor<T> = Box<dyn Iterator<Item = Result<T, SemanticError>> + Send>;
pub(super) type Factory<T> = Box<dyn FnOnce() -> Result<Cursor<T>, SemanticError> + Send>;
struct State<T> {
    factory: Option<Factory<T>>,
    cursor: Option<Cursor<T>>,
    done: bool,
}
struct Resource<T> {
    stopped: AtomicBool,
    state: Mutex<State<T>>,
}
struct Chunk<T> {
    rows: VecDeque<Result<T, SemanticError>>,
    done: bool,
}

/// Ordered demand-driven results. One error ends the stream; successful rows
/// preceding an error in the same chunk are delivered first. No cursor is
/// advanced or destroyed on the executor thread.
pub struct AsyncStream<T: Send + 'static> {
    executor: AsyncExecutor,
    resource: Option<Owned<Resource<T>>>,
    pending: Option<AsyncOperation<Chunk<T>>>,
    rows: VecDeque<Result<T, SemanticError>>,
    deadline: Option<Instant>,
    chunk_rows: usize,
    done: bool,
    id: u64,
    operation: Option<crate::OperationContext>,
}
impl<T: Send + 'static> Unpin for AsyncStream<T> {}
impl<T: Send + 'static> std::fmt::Debug for AsyncStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncStream")
            .field("buffered_rows", &self.rows.len())
            .field("done", &self.done)
            .finish()
    }
}
impl<T: Send + 'static> AsyncStream<T> {
    pub(crate) fn new(
        executor: &AsyncExecutor,
        options: AsyncStreamOptions,
        deadline: Option<Instant>,
        make_factory: impl FnOnce() -> Factory<T>,
    ) -> Result<Self, SemanticError> {
        if options.chunk_rows == 0 {
            return Err(SemanticError::incorrect(
                "async/chunk-size",
                "stream chunk_rows must be positive",
            ));
        }
        let resource = executor.retain(|| Resource {
            stopped: AtomicBool::new(false),
            state: Mutex::new(State {
                factory: Some(make_factory()),
                cursor: None,
                done: false,
            }),
        })?;
        Ok(Self {
            executor: executor.clone(),
            resource: Some(resource),
            pending: None,
            rows: VecDeque::new(),
            deadline,
            chunk_rows: options.chunk_rows,
            done: false,
            id: executor.stream_id(),
            operation: crate::OperationContext::current(),
        })
    }
    pub async fn next(&mut self) -> Option<Result<T, SemanticError>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }
    pub fn buffered_rows(&self) -> usize {
        self.rows.len()
    }
    pub fn is_terminated(&self) -> bool {
        self.done && self.rows.is_empty()
    }
}
impl<T: Send + 'static> Stream for AsyncStream<T> {
    type Item = Result<T, SemanticError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(row) = self.rows.pop_front() {
            return Poll::Ready(Some(row));
        }
        if self.done {
            self.resource.take();
            return Poll::Ready(None);
        }
        if self.pending.is_none() {
            self.executor.watch_admission(self.id, cx.waker());
            let reservation = match self.executor.reserve() {
                Ok(reservation) => reservation,
                Err(_) => return Poll::Pending,
            };
            self.executor.unwatch_admission(self.id);
            let resource = self
                .resource
                .as_ref()
                .expect("live stream resource")
                .clone();
            let deadline = self.deadline;
            let maximum = self.chunk_rows;
            let operation = self.operation.clone();
            self.pending = Some(reservation.run(deadline, false, move || {
                let _scope = operation.as_ref().map(crate::OperationContext::enter);
                let mut state = resource.state.lock().unwrap_or_else(|e| e.into_inner());
                if resource.stopped.load(Ordering::Acquire) {
                    return Ok(Chunk {
                        rows: VecDeque::new(),
                        done: true,
                    });
                }
                if let Some(factory) = state.factory.take() {
                    state.cursor = Some(factory()?);
                }
                let mut rows = VecDeque::new();
                while rows.len() < maximum && !state.done {
                    if resource.stopped.load(Ordering::Acquire) {
                        state.done = true;
                        break;
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        rows.push_back(Err(deadline_error()));
                        state.done = true;
                        break;
                    }
                    match state
                        .cursor
                        .as_mut()
                        .expect("initialized stream cursor")
                        .next()
                    {
                        Some(Ok(row)) => rows.push_back(Ok(row)),
                        Some(Err(error)) => {
                            rows.push_back(Err(error));
                            state.done = true;
                        }
                        None => state.done = true,
                    }
                }
                if state.done {
                    state.cursor.take();
                }
                Ok(Chunk {
                    rows,
                    done: state.done,
                })
            }));
        }
        match Pin::new(self.pending.as_mut().expect("scheduled chunk")).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending.take();
                match result {
                    Ok(chunk) => {
                        self.rows = chunk.rows;
                        self.done = chunk.done;
                    }
                    Err(error) => {
                        self.rows.push_back(Err(error));
                        self.done = true;
                    }
                }
                Poll::Ready(self.rows.pop_front())
            }
        }
    }
}
impl<T: Send + 'static> futures_core::stream::FusedStream for AsyncStream<T> {
    fn is_terminated(&self) -> bool {
        self.is_terminated()
    }
}
impl<T: Send + 'static> Drop for AsyncStream<T> {
    fn drop(&mut self) {
        self.executor.unwatch_admission(self.id);
        if let Some(resource) = self.resource.take() {
            resource.stopped.store(true, Ordering::Release);
            let rows = std::mem::take(&mut self.rows);
            self.executor.cleanup(move || {
                drop(rows);
                drop(resource);
            });
        }
    }
}
