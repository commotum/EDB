# Peer library dependency and entry points

Read the full assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER, WORKER_RULES and the three supplied talks. This page teaches how an application acquires the native library and constructs its async facade; it contains no transaction forms or datom shape requiring a source extension.

## Situation, diagnosis and bounded problem

`Cargo.toml` declares unpublished package `edb-core`, Rust edition 2024, and no package features. The normal `src/lib.rs` and `src/bin/edb/main.rs` targets supply the library and executable. `src/lib.rs` exports all four named types. The example's `Connection::db(&self) -> DatabaseValue` matches `src/application/connection.rs`. `AsyncClient::new(&Connection, &AsyncExecutor)` in `src/application/asynchronous/client.rs` matches the documented construction expression. There is no observed documentation or implementation gap. The bounded question is whether a caller following this dependency/construction guide reaches the same source-aware native engine without an alternate package or attribution mechanism.

## Approaches and decision

Leaving the guide intact preserves its focused dependency scope. Adding a transaction example would duplicate the linked tutorials and introduce transaction/schema setup into a package acquisition page. A separate async/source package or compatibility facade would entangle dependency selection with database semantics. Select the existing package, exports and constructors with no edits or adapters.

Dependency acquisition, immutable database capture, worker scheduling and transaction attribution remain independent. The async facade reserves bounded executor resources and forwards ordinary native requests; it does not resolve or invent source. `TransactionRequest.source` remains mandatory, and `AsyncClient::transact` passes that complete request to `Connection::transact`. Changing worker limits does not change transaction semantics. The standard library target keeps dependency naming locally understandable.

## Acceptance evidence and result

- Inspected `Cargo.toml`, `src/lib.rs`, native connection construction/capture/submission, `src/database_value/mod.rs`, `src/transactor/request.rs`, and the complete async client, stream and executor implementations. The exported future and stream wrappers use Rust `Future` and `futures_core::Stream`, backed by the documented native connection and bounded workers.
- Checked every Markdown link destination exists. Package name, inferred library name, path-dependency example, toolchain edition, absence of Cargo features and constructor argument order agree with the implementation.
- Reused existing unchanged-feature evidence as WORKER_RULES requests: `application-build.log` successfully built the application example and CLI; `native-workflow.log` completed native schema/transact/query/pull/log/snapshot workflow; `tests-interfaces.log` passed all 6 interface tests.
- `tests-integrated-second.log` records 43 library and 55 integration tests passing, with explicitly ignored durable tests. The separate `application-local-retry-after-fix.log` records `ASYNC_READ_OK single_thread=true old_basis=true chunk_rows=1`, four completed operations, resources/operations 0, and worker_panics 0; `doc_application_workflow.md` describes the isolated live setup and checks.

No page or source changes, new tests, builds, database access or commits were necessary. The historical report and running application resources were untouched. No remaining gap or dependency is known from this page review.
