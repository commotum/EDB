# Error handling

This page covers error handling in the [native Rust APIs](04_native_api_reference.md),
including [asynchronous clients](01_async_client.md) and transaction transports.

Error information should be generic and extensible. EDB accomplishes this by
representing ordinary API failures as `SemanticError`. Synchronous operations
return `Result<T, SemanticError>`. An asynchronous operation completes with the
same result type; an `AsyncStream<T>` yields `Result<T, SemanticError>` items.
One error ends the stream: successful rows preceding that error are delivered
first, followed by the error, then `None`.

Error information should be actionable. EDB divides errors into categories
using `ErrorCategory`, with a separate operation-specific code and diagnostic
data. Inspect these fields instead of parsing an error's display text:

| Field | Meaning |
| --- | --- |
| `category` | Top-level failure class, such as `Incorrect`, `Conflict`, or `Unavailable`. |
| `code` | A qualified string identifying the failure, such as `async/operation-capacity`. |
| `message` | Human-readable explanation. |
| `details` | A `BTreeMap<String, String>` of additional diagnostics. Keys and values are strings, not necessarily namespaced keywords. |
| `anomaly` | Optional boxed `RuntimeValue` retaining the structured map from program cancellation. |

`SemanticError` implements `std::error::Error`, `Display`, `Clone`, and equality.
Its display is `code: message`; displaying it does not include all diagnostic
fields. For example, this handles an invalid EDN input without converting the
error to a string first:

```rust
use edb_core::{ErrorCategory, edn::read_edn};

let error = read_edn("[").unwrap_err();
assert_eq!(error.category, ErrorCategory::Incorrect);
eprintln!("{}: {}", error.code, error.message);
for (key, value) in &error.details {
    eprintln!("  {key}: {value}");
}
```

While these basic mechanisms are simple, there are several points of complexity
to consider, explained in more detail below.

## Categories and retries

Categories guide recovery, but do not by themselves establish that a transaction
was never submitted or that repeating it is safe.

| Category | Approach to recovery |
| --- | --- |
| `Incorrect` | Correct the input or request. Repeating unchanged invalid data will not help. |
| `Forbidden` | Resolve the permission or authentication failure. |
| `Unsupported` | Change the requested operation or configuration to one the API supports. |
| `NotFound` | Check the requested identity or resource. |
| `Conflict` | Inspect the code and current state. A changed basis or failed precondition may require a new application decision; an idempotency-key conflict requires preserving the original request. |
| `Busy` | Reduce pressure or wait before retrying, subject to the operation's submission contract. |
| `Unavailable` | Restore connectivity or service availability, then retry according to the operation's contract. |
| `Interrupted` | Inspect the cancellation or deadline condition before attempting the operation again. |
| `Fault` | Investigate the diagnostic information; blind retries are not a general recovery policy. |
| `UnknownOutcome` | The transaction may have committed. Reconcile with the same request key and exact request data. |

A `TransactionTicket::wait` timeout or worker disconnection after admission
returns `service/unknown-outcome`. Transport failures once a request write has
been attempted can likewise leave its outcome unknown. A timeout does not
mean rollback.

Keep the same database identity, request key, transaction forms, basis
precondition, and transaction-time override when explicitly retrying an unknown
outcome. A matching retained receipt returns the original committed result with
`replayed: true`. Reusing that key with different data returns
`postgres/idempotency-key-reused` (`Conflict`). Excision can deliberately erase an
old receipt; its occupied key then returns `postgres/idempotency-predates-excision`
instead of replaying the transaction. See [processing transactions](../04_transactions/06_processing_transactions.md).

The routed remote writer may rediscover and retry a changed endpoint only under
its pre-submission policy. It does not automatically replay an ambiguous
submission. A socket or remote `Ok(CommittedTransaction)` is a confirmed commit
even when its nested `report` is `Err`: opening the local database values failed,
not the commit. Preserve its `basis_t` and `tx_hash`; do not treat this as a
rejected transaction.

## Asynchronous errors

Async calls have two error boundaries. Admission can fail immediately, and an
admitted operation can later complete with an error:

```rust,ignore
// Within an async function returning Result<_, SemanticError>:
let operation = client.query(query, inputs, control)?; // admission
let outcome = operation.await?;                       // execution
```

For example, `async/operation-capacity` (`Busy`) rejects fresh admission before
transaction submission. Dropping an admitted `AsyncTransaction` abandons its
result, not its queued transaction. A worker still attempts it unless its
deadline expires before submission. Dropping an ordinary read operation can
skip a read that has not started; running work remains subject to its cooperative
cancellation and I/O policies. See [async admission and cancellation](01_async_client.md#admission-ownership-and-cancellation).

## Program cancellation anomalies

Transaction-function cancellation uses a map with namespace-qualified keyword
keys. `:cognitect.anomalies/category` is required and accepts
`:cognitect.anomalies/incorrect` or `:cognitect.anomalies/conflict`;
`:cognitect.anomalies/message`, when present, must be a string. Other qualified
keys can carry application data.

A valid cancellation produces `program/rejected` with the corresponding
category. EDB retains the typed map in `error.anomaly`, sets
`:edb/cancelled` to `true`, and supplies textual diagnostics in `details`.
Read `anomaly` when scalar types or nested data matter. Malformed cancellation
maps produce their own validation errors. See
[transaction functions](../04_transactions/07_transaction_functions.md).

## Library errors and panics

EDB relies on third-party libraries, each with its own error conventions.
The native PostgreSQL boundary classifies driver errors into `SemanticError`.
Server diagnostics can include `postgres_sqlstate` and sanitized identifiers;
transport failures use a generic message and `postgres_transport: "true"`.
The original driver error is not retained as a cause.

Application libraries and executor integrations may still introduce their own
error types. Rust panics are also distinct from returned errors. The async
worker boundary catches unwinding operation panics as `async/worker-panic`,
using `Fault` for reads and `UnknownOutcome` for transactions. This is not a
promise that every panic anywhere in an application becomes `SemanticError`.

## Wrapped errors

Rust errors can wrap underlying causes through `std::error::Error::source`.
Error handlers should search that chain semantically, rather than assuming
that the third error is the interesting one. For example, if an application
wrapper exposes its cause, find a `SemanticError` by type:

```rust
use edb_core::SemanticError;
use std::error::Error;

fn semantic_error<'a>(mut error: &'a (dyn Error + 'static))
    -> Option<&'a SemanticError>
{
    loop {
        if let Some(semantic) = error.downcast_ref::<SemanticError>() {
            return Some(semantic);
        }
        error = error.source()?;
    }
}
```

`SemanticError` itself has no source chain. Native futures return it directly;
awaiting an EDB operation does not add an execution-exception wrapper.

Transaction transport decoding introduces a different kind of wrapper:
server errors arrive with `code == "transport/remote-error"`, preserving the
server's category, message, details, and structured anomaly. The original
server code is in `details["remote_code"]`. Inspect that field when matching a
specific server rejection; walking `source()` will not recover it. This applies
to both Unix socket and TLS transaction submissions.
