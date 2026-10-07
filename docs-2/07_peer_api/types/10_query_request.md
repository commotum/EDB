# Query requests and controls

Parameters for query execution are a query, its inputs and execution controls.
The native API accepts these as `Query`, `QueryInput` values and `QueryControl`.
For EDN, `BoundEdnQuery` contains the prepared query and arguments produced by
`EdnQuery::bind`; pass the execution controls to its `execute` method.

See [Executing Queries](../../05_query_and_pull/07_executing_queries.md) for
database acquisition and execution, and the
[Query Reference](../../05_query_and_pull/05_query_reference.md) for query syntax.

## Field summary

| Type | Field or accessor | Description |
| --- | --- | --- |
| `Query` | `find`, `with`, `inputs`, `clauses`, `rules` | The data structure describing the query. |
| `BoundEdnQuery` | `query()` | A reference to the compiled native `Query`. |
| `BoundEdnQuery` | `sources: Vec<QueryDataSource>` | Named database, relation or log sources. |
| `BoundEdnQuery` | `inputs: Vec<QueryInput>` | Ordinary inputs bound to the query's input specifications. |
| `BoundEdnQuery` | `return_keys: Option<Vec<Value>>` | Optional keys used when rendering return maps. |
| `QueryControl` | `timeout: Option<Duration>` | The duration after which a query may be stopped; defaults to `None`. |
| `QueryControl` | `diagnostics: Option<QueryDiagnosticOptions>` | Optional bounded capture of query execution details; defaults to `None`. |

`QueryControl` also supplies cancellation, work, intermediate-row, result-row,
join-memory, numeric-allocation and cumulative-value controls. See
[Query controls](../../05_query_and_pull/00_queries.md) for their semantics.
Execution controls are Rust fields, not EDN query sections such as `:timeout`
or `:io-stats`.

## Method summary

| Function or method | Returns | Description |
| --- | --- | --- |
| `Query::new(find, clauses)` | `Query` | Creates native query data with empty `with`, `inputs` and `rules`. |
| `parse_query_edn(text)` | `Result<EdnQuery, SemanticError>` | Reads an EDN query template. |
| `query_from_edn(value)` | `Result<EdnQuery, SemanticError>` | Creates a template from an existing `EdnValue`. |
| `EdnQuery::bind(arguments)` | `Result<BoundEdnQuery, SemanticError>` | Binds the arguments and prepares the query. |
| `PreparedQuery::new(query)` | `Result<PreparedQuery, SemanticError>` | Prepares reusable native query structure. |
| `PreparedQuery::execute(sources, inputs, control)` | `Result<QueryOutcome, SemanticError>` | Executes prepared structure with supplied sources, inputs and controls. |
| `BoundEdnQuery::execute(control, extensions)` | `Result<QueryOutcome, SemanticError>` | Executes the bound query; use `None` when no local extensions are needed. |
| `BoundEdnQuery::result_to_edn(result)` | `Result<EdnValue, SemanticError>` | Renders a result, honoring any return-map keys. |

## Creating a bound query

`EdnQuery::bind` creates a bound query that can be passed execution controls.
`query` and arguments take the forms described in
[Query Execution](../../05_query_and_pull/07_executing_queries.md#query-execution).
Arguments are inputs bound to the names in the `:in` section of the query,
in declaration order. They include source, rule and Pull-pattern arguments as
well as ordinary data bindings.

This complete example uses a scalar input and requires no database:

```rust
use edb_core::{QueryControl, QueryDiagnosticOptions};
use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use std::time::Duration;

fn main() -> Result<(), edb_core::SemanticError> {
    let query = parse_query_edn("[:find ?name . :in ?name]")?;
    let bound = query.bind(&[
        EdnQueryArgument::Data(read_edn(r#""The Beatles""#)?),
    ])?;
    let control = QueryControl {
        timeout: Some(Duration::from_millis(1_000)),
        diagnostics: Some(QueryDiagnosticOptions::default()),
        ..Default::default()
    };
    let outcome = bound.execute(&control, None)?;
    println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
    Ok(())
}
```

Output:

```edn
"The Beatles"
```

For native queries, `DatabaseValue::query(&query, &inputs, &control)` supplies
the default `$` database source. `QueryEngine::execute_sources` and
`PreparedQuery::execute` accept explicit source arrays. Their `inputs` contain
only ordinary data bindings; native rules belong to `Query::rules`.
Prepared query structure can be reused with different sources, inputs and
controls. It does not retain results. See
[Prepared queries](../../05_query_and_pull/00_queries.md#prepared-queries).

## Timeout

`QueryControl::timeout` specifies the duration after which a query may be
stopped. To specify milliseconds, use `Some(Duration::from_millis(timeout_msec))`.
Set the field when constructing the control, or update it before execution.

Timeout is approximate: it is meant to protect against long-running queries,
but is not guaranteed to stop after precisely the duration specified. Checks
are cooperative, and application callbacks must cooperate during expensive
work. An expired deadline returns `SemanticError` with category `Interrupted`
and code `query/timeout`. EDN parsing and binding precede the execution timeout.
The native default is no timeout; the CLI sets its own default of 30 seconds.

## Results and diagnostics

Execution returns `QueryOutcome`, containing:

- `result`: a `QueryResult` relation, collection, optional tuple or optional
  scalar, according to the find specification.
- `stats`: query-wide evaluator counters, including work, examined datoms and
  index seeks.
- `plan`: completed execution steps.
- `diagnostics`: an optional detailed report enabled by
  `QueryControl::diagnostics`.

Detailed query diagnostics are returned separately from the result. Their
default capture limits are 4,096 steps and 1 MiB of annotation bytes; exhaustion
truncates metadata rather than failing the query. See
[Query diagnostics](../../05_query_and_pull/04_diagnostics.md).

I/O statistics use an `OperationContext`, independently of `QueryControl`.
Create a named diagnostic context, enter its scope around execution, and read
its `SqlIoReport`:

```rust,ignore
use edb_core::{Keyword, OperationContext, OperationKind};

let context = OperationContext::named(
    OperationKind::Application,
    Keyword::new("app", "artist-query"),
)?;
let outcome = {
    let _scope = context.enter();
    bound.execute(&control, None)?
};
let io_report = context.report();
```

The report contains operation-local SQL, cache and index attribution.
`outcome.stats.index_seeks` counts evaluator activity; an index seek does not
necessarily cause a SQL read. Use `io_report_to_edn(&io_report)` to render the
I/O report. See [Observability](../../08_operations/03_observability.md).

The CLI exposes these controls as `--query-stats` and
`--io-context :app/artist-query`. With both enabled, its output is a map with
`:edb/ret`, `:edb/query-stats` and `:edb/io-stats`. With only I/O diagnostics,
it returns `:edb/ret` and `:edb/io-stats`; with neither, it renders the result
directly. These wrappers are CLI output conventions; native execution continues
to return `QueryOutcome`, and the operation context provides the I/O report.

## Inspecting query data

Use `BoundEdnQuery::query()` or `PreparedQuery::query()` to inspect the native
query data. The types implement Rust `Debug` for inspection; there is no
`asData()` request-map conversion or `toString()` query serializer. Keep the
original `EdnValue` if the application needs to retain and render the submitted
query form. `result_to_edn` renders results, not query requests.
