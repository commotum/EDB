# Atomic

Atomic is a native Rust/PostgreSQL database inspired by Datomic. Applications use
immutable database values, declarative serialized transactions, strong identity,
schema as data, history/time views, and peer-local Datalog, Pull and navigation.

Rust owns the database engine: persistent log/index structures, receipts,
publication, writer fencing, retention and maintenance. PostgreSQL stores opaque
immutable objects and revisioned references. It supplies durable atomic storage
primitives, not transaction or maintenance policy. There is one production
storage path and no JVM/wire compatibility target.

This is a first-release development product. Fresh databases may be required
after format changes; earlier development databases and backups are not supported
upgrade inputs. Installation never resets existing data.

## Start here

Start with the [chapter guide](docs/00_start_here/00_introduction.md), the
[EDN file/stdin tutorial](docs/01_tutorials/00_edn_workflow.md), or the
[separate Rust application](docs/01_tutorials/01_application_workflow.md).
The guide covers schema, transactions, immutable values, query/Pull/fulltext,
indexes/logs, native APIs, operations and genuinely optional caching/prefetch.

For deployment and recovery use [operations](docs/08_operations/00_deployment.md)
and [administration](docs/08_operations/01_administration.md).

## Build and exercise

```sh
cargo build --locked --release --bin atomic --examples
```

Run the public Rust application against the disposable database:

```sh
ATOMIC_POSTGRES_URL='host=/path/to/socket port=5432 user=atomic_test dbname=atomic_test' \
  cargo run --offline --example native_workflow
```

It creates a unique logical database and exercises schema, program deployment,
transactions, queries, Pull, history, held values and reopen. The
`process_workflow` example exercises an independent writer and submitting peers.
The stock `application_workflow` example uses the CLI-managed local or remote
transactor; follow the [application guide](docs/01_tutorials/01_application_workflow.md).

## Application model

Capture `Connection::db()` or `Peer::db()` once and pass the resulting
`DatabaseValue` through computations. Capturing a resident value does no I/O;
cold reads authenticate immutable storage objects, while warm reads can be
entirely local. Queries and navigation do not run in the transactor.

`DatabaseValue::with` and `with_forms` return speculative reports without
publishing anything. Branches share their base and own their new information.
Maps, primitive forms, symbolic references and controlled persisted calls use
the same transaction semantics as committed requests. Transactions on filtered
values use the full basis and preserve filters on the result; `as_of` is not
a writable branch of the past. History values cannot transact.

A successful committed report contains the exact before/after values, transaction
data and resolved tempids. After an ambiguous delivery outcome, retry the same
request key and content. Receipt-first resolution preserves the committed result
instead of running the transaction again. A committed response can separately
report failure to open its read values without changing the durable outcome.

Connections observe newer roots in the background using coalesced notifications
and durable catch-up. Change consumers expose explicit checkpoints and bounded
replay. Read handles and backup captures create no reader pins. Configure GC grace
(normally 30 days) to cover reads, consumer lag and copy durations; holding a
Rust value does not override it. A serialized snapshot reference is neither a
credential nor a retention pin.
Excision cannot erase bytes or memories already exported to another process.

Read limits, deadlines and cancellation are cooperative resource policies, not
hard allocator or SQL preemption guarantees. Local Rust callbacks are trusted.
Fulltext merges indexed and recent facts with supplied-view validation; an
absent search hit is not an identity or uniqueness constraint. Advisory hints
never become transaction meaning or durable request identity.

Benchmark throughput and recovery with an actual application workload and an
explicitly disposable PostgreSQL instance.
