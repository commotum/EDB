# Query diagnostics

Query diagnostics explain the native optimizer's actual execution. They do not
force textual clause order or change query results. Original clause paths remain
distinct even when several clauses have the same kind, or the same rule executes
many times.

```rust,ignore
use atomic_core::{QueryControl, QueryDiagnosticOptions};

let control = QueryControl {
    diagnostics: Some(QueryDiagnosticOptions::default()),
    ..Default::default()
};
let outcome = database.query(&query, &inputs, &control)?;
for step in &outcome.diagnostics.as_ref().unwrap().steps {
    println!("{} phase={} scheduled={} {} -> {:?} via {}",
        step.clause_path, step.phase_id, step.schedule_position,
        step.rows_in, step.rows_out, step.access);
}
```

For EDN queries, pass the same control to `BoundEdnQuery::execute`. The stock
`atomic query --query-stats` option returns the normal result together with query
diagnostics. `query_diagnostics_to_edn` provides the same metadata conversion for
native callers. Without the option, existing query output is unchanged.

## Reading the report

`steps` is in **start order**. Its `id` is the corresponding execution ordinal;
`schedule_position` is local to a phase. The older `outcome.plan` remains in
completion order and links to a detailed step through `diagnostic_step`.

Clause paths use zero-based positions in the submitted AST:

- `where/1`: the second top-level clause.
- `where/1/or/0/2`: the third clause in that disjunction's first branch.
- `rules/0/0/not/0`: the first negated clause in the first rule definition.
- `where/2/query/where/0`: the first clause of that nested query.

Paths correlate with the current query, not a cross-release identifier format.
Prepared queries retain the same lexical meaning without retaining inputs or
results in their preparation cache. Each execution starts a fresh report.

Phases distinguish disjunction branches, rule evaluations, negative completion
tasks and nested queries. `parent` and `parent_step` describe an active caller
when one exists. Deferred negation runs on the existing explicit completion-task
stack after its caller suspends; its lexical path identifies the owner, rather
than inventing an active parent frame. `AwaitNegative` and `AwaitRules` steps are
real attempted work, not completed relations. They have no `rows_out` value.

`binds_in` and `binds_out` are variable names definitely bound across the actual
input/output rows. Empty output has no definite bindings. `bindings_complete`
is false when output binding capture was truncated or the clause suspended.
No bound values are included. `source` is the effective named-source label; it
does not assert that a source-free function performed database I/O.

Each step reports rows and evaluator work, examined datoms, index seeks, join
candidates and accounted value allocation. Parent counters **include child
work**, so do not sum a parent with its descendants. Query-wide `outcome.stats`
remains authoritative for total evaluator work. Physical SQL/cache attribution
is a separate operation-level report; an index seek is not necessarily a SQL
read.

Warnings are observations for investigation, not claims that the optimizer made
a mistake:

- `UnboundPattern`: none of the pattern's variables was already bound. Constants
  or an indexed range can still make it selective.
- `FullScan`: the selected access path scans its source. Check indexing and
  supplied bindings, especially when many candidates produce few result rows.
- `Expansion`: the clause produced more rows than it received. This is often
  intentional; examine large expansions before expensive downstream clauses.

The report records operator names, variable/source identifiers, paths and
counts—not whole clauses, constants, argument/result values or callback
payloads. Identifiers are still application-defined; choose them accordingly
when exporting diagnostics.

## Cost and limits

Detailed capture is disabled by default. Its default allowance is 4,096 steps
and 1 MiB of conservative cumulative annotation bytes. The byte allowance also
bounds annotation-only inspections, preventing an enormous relation or variable
name from causing an unbounded diagnostic scan. These are capture policy units,
not allocator/RSS measurements.

On exhaustion, `truncated` becomes true and further detail stops. Existing
steps retain their outcome counters where available. No query work/value budget
is reset, consumed by metadata accounting or converted into a capture error.
Instrumentation takes wall-clock time, so the existing query timeout still
applies to the complete call. Use diagnostics for investigation and compare
complete calls when assessing its overhead.

`QuerySequence::diagnostics()` describes relational preparation. Its later lazy
Pull work updates `stats()` during consumption; Pull is not mislabeled as a
scheduled `where` clause. Async query methods accept the same controls and keep
reports local to their captured execution.

Atomic schedules clauses by readiness and selectivity; textual clause order is
not a forced execution order.
