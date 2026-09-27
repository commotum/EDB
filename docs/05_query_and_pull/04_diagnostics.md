# Query diagnostics

## Background

EDB schedules query clauses by readiness and a bound-term score. Patterns with
more constants or bound variables receive a higher score; ready predicates and
negations are prioritized over functions, rules and disjunctions. Later clauses
win equal scores. Textual clause order is not a forced execution order, and the
score is a heuristic rather than a measured cardinality estimate.

## Problem

You may not know which clauses are more selective. One measure of clause
selectivity is the count of rows flowing in and out of each clause. Another
measure is the boundness of variables, which helps to limit work done in later
clauses.

## Approach

When you request query diagnostics, EDB monitors your query as it runs,
returning a report with the following:

- Clause ordering and the access path used by each clause.
- Count of rows flowing in and out of each clause.
- Boundness of variables in and out of each clause.
- Operators, including predicates, executed as individual steps.
- Possible concerns, including expansions and scans.

Query diagnostics explain the native optimizer's actual execution. They do not
force textual clause order or change query results. Original clause paths remain
distinct even when several clauses have the same kind, or the same rule executes
many times.

```rust,ignore
use edb_core::{QueryControl, QueryDiagnosticOptions};

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
`edb query --query-stats` option returns the normal result together with query
diagnostics. `query_diagnostics_to_edn` provides the same metadata conversion for
native callers. Without the option, existing query output is unchanged.

The CLI returns the query result under `:edb/ret` and the diagnostic report under
`:edb/query-stats`. In the Rust API, these are `QueryOutcome::result` and
`QueryOutcome::diagnostics`. Enable capture through `QueryControl::diagnostics`;
`:query-stats` is not an accepted EDN query section.

Requesting diagnostics adds some overhead to query processing. Enable it for
investigation and account for that overhead when measuring performance.

For example, this complete program queries a collection without a database:

```rust
use edb_core::{QueryControl, QueryDiagnosticOptions};
use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = parse_query_edn(
        "[:find ?n :in [?n ...] :where [(< ?n 3)] [(> ?n 1)]]",
    )?;
    let bound = query.bind(&[EdnQueryArgument::Data(read_edn("[1 2 3]")?)])?;
    let control = QueryControl {
        diagnostics: Some(QueryDiagnosticOptions::default()),
        ..Default::default()
    };
    let outcome = bound.execute(&control, None)?;
    println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
    for step in &outcome.diagnostics.as_ref().unwrap().steps {
        println!("{} {}: {} -> {:?}; {:?} -> {:?}",
            step.clause_path, step.operator, step.rows_in, step.rows_out,
            step.binds_in, step.binds_out);
    }
    Ok(())
}
```

Output:

```text
#{[2]}
where/1 >: 3 -> Some(2); ["?n"] -> ["?n"]
where/0 <: 2 -> Some(1); ["?n"] -> ["?n"]
```

Both predicates are ready and have equal scores, so the later clause runs
first. Its output relation becomes the earlier clause's input relation.

## The query-stats map

`query_diagnostics_to_edn` produces the following keys. The corresponding Rust
fields use underscores. Detailed step records are in one `:query/steps` vector;
each step refers to its phase by ID.

| Key | Description |
| --- | --- |
| `:query/phases` | Subquery evaluations in the order they were entered. See [Phases](#phases). |
| `:query/steps` | For each scheduled clause, information about row counts, bindings and work. See [Steps](#steps). |
| `:query/truncated` | Whether diagnostic capture stopped before recording all detail. |
| `:query/capture-bytes` | Conservative cumulative annotation-byte accounting. |
| `:query/inspection-work` | Accounted work inspecting query structure and bindings for diagnostics. |

The report does not retain the submitted query. Keep the query separately to
interpret its clause paths. Consumers should tolerate additional report fields.

### Phases

| Key | Description |
| --- | --- |
| `:query/id` | Phase identifier within this report. |
| `:query/parent` | Active caller's phase ID, or `nil`. |
| `:query/parent-step` | Active caller's step ID, or `nil`. |
| `:query/path` | Lexical scope in the submitted query. |
| `:query/kind` | `"Query"`, `"OrBranch"`, `"Negation"` or `"Rule"`. |
| `:query/scheduled-steps` | Number of captured steps scheduled in this phase. |

To recover a phase's schedule, select steps whose `:query/phase` matches its ID
and read them in `:query/schedule-position` order.

### Steps

| Key | Description |
| --- | --- |
| `:query/id` | Execution ordinal in clause start order. |
| `:query/phase` | Phase ID; the Rust field is `phase_id`. |
| `:query/schedule-position` | Zero-based scheduling position within the phase. |
| `:query/clause-path` | Location of the clause in the submitted AST. |
| `:query/original-index` | Zero-based position within the clause's lexical scope. |
| `:query/operator` | Operator name, such as `"pattern"`, `">"` or `"q"`. |
| `:query/source` | Effective named-source label, or `nil`. |
| `:query/access` | Evaluator access-path description; it may be empty when capture stops or evaluation suspends. |
| `:query/rows-in` | Count of rows into the clause. |
| `:query/rows-out` | Count of rows out of the clause after deduplication, or `nil` if no completed output was recorded. A large expansion here could be an indicator of poor selectivity. |
| `:query/binds-in` | Variable names definitely bound coming into the clause. Insufficiently bound clauses can be less selective. |
| `:query/binds-out` | Variable names definitely bound going out of the clause. |
| `:query/bindings-complete` | Whether output binding capture completed. |
| `:query/status` | `"Running"`, `"Complete"`, `"AwaitNegative"`, `"AwaitRules"` or `"Failed"`. Query errors still return errors rather than a successful outcome. |
| `:query/work` | Inclusive evaluator work charged during the step. |
| `:query/datoms-examined` | Inclusive examined-datom count. |
| `:query/index-seeks` | Inclusive logical index-seek count. |
| `:query/join-candidates` | Inclusive join-candidate count. |
| `:query/value-bytes` | Inclusive accounted value allocation; the Rust field is `work.allocated_value_bytes`. |
| `:query/warnings` | Vector of possible concerns. See [Warnings](#warnings). |

The five work counters are grouped under `QueryClauseStep::work` in Rust and
flattened into the step map in EDN. Predicates have their own operator steps;
there is no per-pattern collection of attached predicate clauses. Expansions
are flagged on every captured expanding step, rather than ranked as a top-three
list. Compute their size as `rows_out - rows_in` for completed steps.

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
read. See [I/O stats](../07_peer_api/08_io_stats.md) for that report and its
operation context.

## Warnings

Warnings are observations for investigation, not claims that the optimizer made
a mistake:

- `UnboundPattern`: none of the pattern's variables was already bound. Constants
  or an indexed range can still make it selective.
- `FullScan`: the selected access path scans its source. Check indexing and
  supplied bindings, especially when many candidates produce few result rows.
- `Expansion`: the clause produced more rows than it received. This is often
  intentional; examine large expansions before expensive downstream clauses.

In EDN these warnings are strings, such as `["UnboundPattern" "Expansion"]`.
A large expansion may or may not indicate a problem: sometimes it is part of
what the query needs to accomplish. An unbound pattern can still be selective
because of constants or index ranges, so inspect its actual access path and
counts before changing the query.

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

`QueryControl::force_scan` is a separate diagnostic control that disables hash
joins and grouped probes and evaluates native database patterns through EAVT
scans. It changes the execution strategy; enabling report capture alone does
not. Compare results as well as total work when using it to investigate an
access path.
