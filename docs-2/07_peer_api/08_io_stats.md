# I/O Stats

This reference describes operation-local I/O statistics: how to request them,
what their fields mean, and how cache and index state affect their interpretation.

## Background

Any application or task has a working set of data. Where that data is located
or cached is an important part of architecting a system; a well-architected
system has to plan for moving through tiers of cache. Otherwise, applications
will encounter unexpected and dramatically different performance when data
accesses spill over to the next tier. In EDB, this can happen when there is too
much reliance on increasing the in-process decoded-node cache to handle the
entire working set.

## Problem

To plan for moving through tiers of cache, developers need to understand the
capacity and latency of each tier. Then, for a given API call, they need to know
where data is coming from, what part of the application called the API, and why
the API needs that particular data.

The transactor uses [indexes](../06_indexes/01_index_model.md) to enforce
invariants, including unique-value lookups and comparisons with existing entity
facts. The query engine chooses a suitable access path for each relational
clause. These operations nest: transaction functions can query the database,
and queries can call other queries. Without operation-local I/O information,
developers would have to reason backward from downstream effects, for example
inferring that a query is spilling into the next cache tier when it performs
very differently from other superficially similar queries.

## Approach: I/O Stats

I/O statistics summarize the input and output associated with a native API
operation. They allow you to analyze where data is coming from, providing counts
and durations for cache and storage accesses. An `OperationContext` identifies
the operation kind and supports a caller-provided qualified keyword to
distinguish different domain operations. Index-order counts and separate
[query diagnostics](../05_query_and_pull/04_diagnostics.md) provide help with
why the operation needed that data.

Native statistics are reported as a `SqlIoReport`, [described below](#the-io-stats-map).
Despite the name, a diagnostic report includes cache/index and semantic
transaction counters as well as SQL activity. `io_report_to_edn` converts it to
an EDN map. These are actual attributed observations, not estimates obtained by
subtracting process-wide counters before and after a call.

`OperationContext::named(kind, keyword)` enables detailed diagnostics.
`OperationContext::diagnostic(kind)` enables the same detail without a name.
`OperationContext::new(kind)` collects SQL statistics but leaves detailed cache,
index and semantic transaction counters disabled. Names require nonempty
namespace and name components whose combined length is at most 256 bytes.
Choose operational labels such as `:artist/by-name`; keep customer identifiers,
query values, request keys and secrets out of them.

## Transaction Logging

Configured service telemetry emits an `edb.transaction` event with the
transaction's `basis_t`, `replayed` status and operation statistics. It is
optional, rather than an automatic log record for every transaction. Enable it
through `ServiceOptions::telemetry`, or use the stock transactor's
`--telemetry-ms` option. See [operational events](../08_operations/03_observability.md)
for publisher configuration and delivery limits.

The following is an illustrative excerpt from an event; other envelope and
metric fields are omitted:

```json
{
  "event": "edb.transaction",
  "basis_t": 1008,
  "replayed": false,
  "operation": "Transaction",
  "context": ":billing/update-account",
  "operation_elapsed_nanos": 5000000,
  "metrics": {
    "transaction.assessments": 1,
    "transaction.produced_datoms": 2,
    "cache.DecodedNode": {
      "accesses": 2,
      "hits": 2,
      "misses": 0
    }
  },
  "nested": [],
  "nested_truncated": false
}
```

Filter JSON event lines by `event == "edb.transaction"` in your log collector.
The publisher uses a bounded queue; disabled publication, a full queue or sink
failure does not change the transaction outcome. These events are best-effort
observations, not a durable transaction log. Given the database identity and
transaction basis, use the [log API](06_log_api.md) to query the datoms produced
by the transaction.

## Transaction API I/O Stats

Enter a diagnostic context around submission to request statistics on the local
service report:

```rust
use edb_core::{
    Keyword, OperationContext, OperationKind, SemanticError,
    ServiceTransactionReport, TransactionClient, TransactionRequest,
};
use std::time::Duration;

fn transact_observed(
    client: &TransactionClient,
    request: TransactionRequest,
) -> Result<ServiceTransactionReport, SemanticError> {
    let context = OperationContext::named(
        OperationKind::Transaction,
        Keyword::new("billing", "update-account"),
    )?;
    let receipt = {
        let _scope = context.enter();
        client.transact(request, Duration::from_secs(30))?
    };
    let diagnostics = receipt.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics.basis_t, receipt.basis_t);
    println!("{:?}", diagnostics.report.stats);
    Ok(receipt)
}
```

Submission carries the context across the service queue, even if the caller's
scope ends before waiting on its ticket. The receipt's `TransactionDiagnostics`
contains `basis_t`, `replayed` and `report`. Configured service telemetry also
enables these detailed per-submission statistics.

Diagnostics are ephemeral: they are not stored in durable receipt bytes or
reconstructed remote/historical reports. An exact receipt replay does not
repeat semantic assessment merely to recreate its counters; a separately
submitted retry can report zero fresh semantic work while still performing
reads. See [transaction diagnostics](../04_transactions/03_diagnostics.md).

For speculative `Database::with` or `DatabaseValue::with`, enter a context and
read its `report()`, or use `measure()`. There is no committed service receipt
for a speculative operation.

## Query API I/O Stats

Use an operation context around query execution, query-sequence consumption,
Pull or Pull-many. `measure` returns the original operation result together with
its I/O report. It preserves errors as well as successful results; it does not
change the native API's result type into an EDN wrapper.

For example, this query counts artists with a supplied name in a database that
has an `:artist/name` attribute:

```rust
use edb_core::{
    DatabaseValue, Keyword, OperationContext, OperationKind, QueryControl,
    QuerySourceValue, SemanticError, io_report_to_edn,
};
use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

fn artists_by_name(db: &DatabaseValue) -> Result<(), SemanticError> {
    let query = parse_query_edn(
        "[:find (count ?a) :in $ ?aname :where [?a :artist/name ?aname]]",
    )?;
    let bound = query.bind(&[
        EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
        EdnQueryArgument::Data(read_edn(r#""The Beatles""#)?),
    ])?;
    let context = OperationContext::named(
        OperationKind::Query, Keyword::new("artist", "by-name"),
    )?;
    let (result, report) = context.measure(|| {
        bound.execute(&QueryControl::default(), None)
    });
    println!("{}", write_edn(&io_report_to_edn(&report))?);
    let outcome = result?;
    println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
    Ok(())
}
```

With one matching artist, the result is `#{[1]}`. The report's counts and elapsed
time depend on the captured database and cache state. An in-memory database can
return that result with no SQL calls or durable cache accesses.

The stock CLI exposes `--io-context :artist/by-name` on `edb query` and `edb pull`.
Its output is a map containing `:edb/ret` and `:edb/io-stats`; query execution
with `--query-stats` also includes `:edb/query-stats`. These are CLI output
conventions, not extra sections in the EDN query language.

For lazy operations, measure the actual consumption. A report taken just after
creating a cursor cannot include future reads. Native database/index cursors
capture their operation context for later work; take another `report()` after
consumption. A context is clonable, but its entered scope is thread-bound.
Explicitly enter a cloned context in application-created worker threads.

## Nested I/O Stats

The statistics for a top-level operation include the total I/O for its child
operations. Often, you will also want the breakdown for nested calls. For
example, when tuning a transaction function that issues two queries, you could
focus your efforts if nested statistics showed that one query dominates reads.

Create named descendants with `parent.child_named(kind, keyword)` and enter the
child around its work. Merely constructing another independent named context
inside an active scope does not attach it to that parent. Unnamed children and
phases preserve their nearest named ancestor's label in their report.

Suppose an operation named `:transfer/funds` creates two named child query
contexts for `:transfer/check-balance` and one for `:transfer/check-auth`. The
parent's EDN report has the following shape, with metric maps abbreviated:

```edn
{:operation "Transaction"
 :context :transfer/funds
 :operation-elapsed-nanos 5000000
 :metrics {} ; inclusive totals for the entire operation
 :nested {:transfer/check-balance {} ; contributions from both balance calls
          :transfer/check-auth {}}  ; contributions from the authorization call
 :nested-truncated false}
```

Each observation contributes once to its operation and each ancestor, never to
siblings. Named descendant contributions are grouped by distinct keyword.
Groups can themselves contain nested work and therefore overlap: do not add
nested maps together to reconstruct the total. An ancestor retains at most 128
distinct nested labels. Overflow sets `nested_truncated` and drops detail only;
total I/O and operation results remain unaffected.

## The I/O Stats Map

The native `SqlIoReport` and its EDN conversion have these top-level fields:

| Native field | EDN key | Value |
| --- | --- | --- |
| `kind` | `:operation` | Operation kind; EDN uses a string such as `"Query"` or `"Transaction"`. |
| `context` | `:context` | Optional caller-provided qualified keyword; EDN uses `nil` when absent. |
| `operation_elapsed_nanos` | `:operation-elapsed-nanos` | Wall time from context construction to this report snapshot, in nanoseconds. |
| `stats` | `:metrics` | Inclusive SQL, cache, index, phase and transaction work statistics. |
| `nested` | `:nested` | Map from named descendant keyword to its aggregated statistics. |
| `nested_truncated` | `:nested-truncated` | Whether the nested-label bound omitted detail. |

Reports are snapshots, not a timer stopped by `measure`: a later report from the
same context has a later elapsed time and includes any subsequent work. Create
one context per operation being compared. Treat metric names as an open set and
ignore unrecognized fields when consuming events.

### Read Statistics

Native `stats.reads` contains `cache` and `indexes` maps. The EDN conversion puts
them under `:metrics` as `:cache.DecodedNode`, `:cache.LocalDisk`,
`:cache.PostgresBlock`, and index keys such as `:index.Avet`.

For durable tree reads, the usual lookup path is decoded node, optional local
disk cache, then PostgreSQL immutable block storage:

| Cache tier | Meaning |
| --- | --- |
| `DecodedNode` | Access to the in-process cache of decoded native tree nodes. A hit already has its node in memory. |
| `LocalDisk` | Access to the optional local disk cache; also used for object reads from an offline backup repository. |
| `PostgresBlock` | Immutable block lookup through the PostgreSQL storage backend. A successful lookup is a hit at this tier. |
| `Inflight` | A defined tier variant; the current tree read path does not emit observations for this tier. |

Each present tier reports:

| Field | Meaning |
| --- | --- |
| `accesses`, `hits`, `misses`, `errors` | Observed accesses and outcomes; an error can also count as a miss. |
| `physical_bytes` | Payload bytes read at that tier, not protocol traffic or physical filesystem block I/O. |
| `canonical_bytes` | Canonical payload bytes associated with those reads. |
| `elapsed_nanos` | Sum of measured wall durations at the tier, including its waits. |
| `min_nanos`, `max_nanos` | Minimum and maximum observed access durations. |

There are several important things to know when interpreting these maps:

- The first, decoded-node tier is special. Hits are already in memory and do
  not require storage I/O; their payload-byte counters are zero.
- Access counts include misses. A decoded miss can read through to local disk
  or PostgreSQL. It is not itself a PostgreSQL read.
- The maps contain observed tiers, not every possible tier. Disabled local
  caching produces no local-cache access entry. No entry can also mean that no
  access was needed, or that detailed diagnostics were disabled.
- Tier counters describe instrumented native accesses, not a universal count of
  all I/O in the process. Roots already retained by a snapshot, recent datoms,
  metadata reads and other paths do not necessarily traverse identical tiers.
  Do not require a strict decreasing count across every tier in every report.
- Duration sums can overlap or include concurrent work. They may exceed the
  operation's wall time and must not be added to reconstruct latency.

For each observed `IndexOrder` (`Eavt`, `Aevt`, `Avet`, `Vaet`), index statistics
report `cursors`, `node_accesses` and `decoded_misses`. Node attribution records
traversed directory/leaf loads. A decoded miss can be satisfied by a lower cache;
it does not mean a PostgreSQL block had to be read. These counts are also distinct
from logical query `index_seeks` and examined-datom counts.

### SQL, Phase and Transaction Statistics

Native `SqlIoStats` counts driver API calls, not SQL statements, packets or
protocol round trips. It includes started/completed calls, errors, row counts,
call durations, connection/control-call counts, and breakdowns by call kind and
operation kind. Stream polls and consumption durations are tracked separately
from starting the SQL request.

`result_cell_bytes` measures returned binary row-field bytes; known payload
read/write bytes are separately recorded application payload sizes. Neither is
a complete network, disk or allocation measure. The EDN map exposes selected
scalar metrics such as `:sql.calls`, `:sql.errors`, `:sql.rows`,
`:sql.result_cell_bytes`, `:sql.elapsed_nanos`, `:sql.payload_read_bytes` and
`:sql.payload_write_bytes`; it does not serialize every native field.

Phase maps such as `:phase.TransactionAssessment` contain `:invocations` and
`:elapsed_nanos`. Transaction counters such as `:transaction.uniqueness_checks`
and `:transaction.redundant_datoms` describe actual semantic work, including
attempts and nested work, rather than durable writes. See
[transaction diagnostics](../04_transactions/03_diagnostics.md) for the complete
counter meanings. Query clause statistics and physical I/O answer different
questions; inspect both when investigating an expensive query.

## Interpreting I/O Stats

To get the most benefit from I/O statistics, look at them under realistic load,
either from production events or simulated load in a test environment.
Realistic load is important because statistics depend not just on the API and
database but also on the current state of the caches and indexes.

### I/O Stats Depend on the Current State of the Caches

EDB populates caches as data is used. The most trivial example is that running
the same query twice on an otherwise unloaded system can produce very different
I/O statistics. Run `artists_by_name` above twice against the same captured
durable database value, creating a fresh context for each call.

If the first call needs uncached tree nodes, its report can contain decoded
misses and accesses to lower tiers. If the second call finds all needed nodes
in the decoded cache, it can show decoded hits with no PostgreSQL block reads.
That is evidence that those needed nodes were retained for the second call. It
does not establish that the entire production working set fits in memory.
Real-world queries are likely to land between these extremes, finding some but
not all of the data they need in each cache tier.

Other queries, eviction, concurrent activity, disk-cache configuration and
which snapshot objects are reused can all change the result. Compare the counts
with the [cache configuration](../09_optional/00_local_cache.md) and cumulative
`cache_stats()`/`ssd_cache_stats()` when investigating capacity. Cumulative peer
or process counters are useful context, but differences between concurrent
snapshots are not reliable attribution to one operation.

### I/O Stats Depend on the Current State of the Indexes

EDB merges a recent in-memory transaction tier with published immutable indexes.
Queries satisfied by recent data can show very little durable-node I/O. This can
dominate results in tiny databases created for experiments. A purely in-memory
database likewise does not exercise PostgreSQL or the durable cache hierarchy.

[Background indexing](../06_indexes/02_background_indexing.md) changes how much
data is in the recent tier and in immutable trees. Attribute-index readiness
also affects which physical access paths are available. Data used for performance
testing should ideally resemble production in size, distribution, index readiness
and recent-versus-indexed state, as well as cache pressure.

## Relating I/O Stats to Transaction Time

Semantic transaction processing is only one component of the total transaction
time experienced by a caller, which can include:

- Transport time when requesting and transacting processes are separate.
- Time queued or waiting for admission/maintenance on the transacting process.
- Transaction expansion and assessment, including invariant checks.
- Encoding, durable storage writes and report construction.
- Delivery back to the caller.

To measure total transaction time, time the complete call to `transact` from the
application. With separate submission and ticket waiting, include both. For a
remote client, measure at that client to include transport and remote report
reconstruction.

The local service receipt's `diagnostics.report.operation_elapsed_nanos` starts
when its submission context is created. It includes queue residence and service
processing through the diagnostic snapshot, including durable work; it stops
before delivery to the caller. A separately created outer application context
can have a wider lifetime. The report's elapsed time is not an isolated
transaction-assessment timer.

Expansion, assessment, encoding, commit and report phases provide more focused
measurements in `stats.phases`. Each phase measures wall time, including CPU and
waits. Phases can nest or overlap; they are not a disjoint decomposition of total
latency. Reducing work in the writer's serial transaction path can also reduce
the wait for transactions queued behind it.

| Measurement | How to measure | What it includes |
| --- | --- | --- |
| Total transaction time | Application timer around the complete transaction call | Caller-visible submission, waiting, processing and delivery; transport when present. |
| Local service submission time | Receipt report's `operation_elapsed_nanos` | Context creation through receipt diagnostic capture, including queue residence and durable processing. |
| Expansion/assessment time | Corresponding `stats.phases` entry | The measured processing phase, including its internal waits; not queue residence before that phase. |
| SQL time | `stats.elapsed_nanos`, with native `by_kind`/`by_operation` breakdowns | Sum of observed driver-call durations; not caller latency or a sum of all transaction phases. |

A `DatabaseValue::with` call is speculative, so its measured time does not include
a durable commit. Do not compare it with full transaction latency as though the
two measurements covered the same work.
