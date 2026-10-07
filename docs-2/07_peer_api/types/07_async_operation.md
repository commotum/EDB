# AsyncOperation and AsyncTransaction

A future that supports a completion callback.

```rust,ignore
pub struct AsyncOperation<T: Send + 'static> { /* private fields */ }

impl<T: Send + 'static> Future for AsyncOperation<T> {
    type Output = Result<T, SemanticError>;
    // ...
}
```

`edb_core::AsyncOperation<T>` represents one result from work admitted to an
`AsyncExecutor`. Work is queued when the API returns the operation; it does
not wait for the first poll. Awaiting the operation yields `Ok(T)` or
`Err(SemanticError)`. API admission can itself fail before returning a future,
so calls commonly use `client.db()?.await?` to handle both stages.

Polling only registers a waker or transfers an already-completed result.
It does not execute the database operation or require a Tokio runtime.
For executor setup, admission limits, deadlines, and transport behavior, see
[Async native clients](../01_async_client.md).

## Method Details

### on_complete

```rust,ignore
pub fn on_complete(
    self,
    callback: impl FnOnce(Result<T, SemanticError>) + Send + 'static,
)
```

Register a callback to run on the operation's blocking worker pool. The
callback will run once and only once, if and when the operation's work is
complete, receiving either its value or its error. If the operation has
completed already, the callback is queued to run on a worker; it is not
invoked synchronously on the registering thread. There is no separate
executor argument. Ordering of callbacks across operations is not guaranteed.

`on_complete` consumes the operation instead of awaiting it. The callback
receives ownership of the result. Captured data must satisfy `Send + 'static`;
move owned values into the closure when needed. Callback panics are contained
by the worker pool. Callback work occupies a worker until it returns.

```rust
use edb_core::{AsyncOperation, SemanticError};

fn observe<T: Send + 'static>(operation: AsyncOperation<T>) {
    operation.on_complete(|result| match result {
        Ok(value) => drop(value),
        Err(error) => eprintln!("operation failed: {error}"),
    });
}

async fn receive<T: Send + 'static>(
    operation: AsyncOperation<T>,
) -> Result<T, SemanticError> {
    operation.await
}
```

## Ownership and completion

An operation has one consumer. It is not cloneable and does not maintain a
list of listeners. Choose either awaiting or `on_complete`; after yielding
`Poll::Ready`, polling the operation again panics.

A completed but unconsumed result continues to occupy an operation slot.
Dropping an unconsumed operation abandons the result and arranges worker-side
disposal. Dropping an ordinary read future can skip a read that has not
started; it does not interrupt an already-running database call.

Once the future or callback transfers a value to application code, that value
retains its native synchronous methods and destructor contract. See the
[ownership and cancellation rules](../01_async_client.md#admission-ownership-and-cancellation)
before using native results on an async runtime thread.

## AsyncTransaction

`edb_core::AsyncTransaction` is the future returned by `AsyncClient::transact`.
Its `Future::Output` is `Result<ServiceTransactionReport, SemanticError>`.
It provides the same consuming `on_complete` method with a callback accepting
that result, and exposes `request_key(&self) -> &str` for the submitted request
key. Copy the key before consuming the future if it is needed later.

Receiving an `AsyncTransaction` means the worker pool admitted the work; it
does not yet establish a durable commit. Dropping it abandons the result,
not the queued transaction. The worker still attempts the transaction unless
its deadline expired before submission. Retain the exact request key and
request data for any explicit retry after an unknown outcome.
