# Persisted native queries

`QueryTemplate::new` lowers compact conjunctive authoring forms to the shared
Rust query AST, with canonically ordered result rows. Use
`QueryTemplate::native(query, input_arguments, sources)` to embed the ordinary
Rust `Query` AST with its ordinary result ordering. Both constructors execute
through the same query engine and use one current program format.
The same program has deterministic canonical bytes and identity. Earlier
development formats require fresh databases.

For example, this query program returns entity/value rows above a threshold;
the attribute itself is a program argument, not a fixed schema assumption:

```rust
use edb_core::*;

fn selector() -> Result<Program, SemanticError> {
    let mut query = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable("entity".into()),
            FindElement::Variable("value".into()),
        ]),
        vec![
            Clause::Pattern(Box::new(DataPattern::new(
                Term::var("entity"), Term::var("attribute"), Term::var("value"),
            ))),
            Clause::Predicate {
                predicate: Predicate::GreaterOrEqual,
                source: "$".into(),
                args: vec![Term::var("value"), Term::var("minimum")],
            },
        ],
    );
    query.inputs = vec![
        InputSpec::Scalar("attribute".into()),
        InputSpec::Scalar("minimum".into()),
    ];
    Ok(Program {
        kind: ProgramKind::Query,
        arity: 2,
        instructions: vec![
            Instruction::Query(QueryTemplate::native(query, vec![0, 1], vec![])?),
            Instruction::ForEach {
                body: vec![Instruction::Unpack(2), Instruction::EmitRow(2)],
            },
            Instruction::Return,
        ],
    })
}
```

Invoke it with `ProgramRuntime::execute_query` and one captured `&DatabaseValue`,
passing an attribute ref/ident and the threshold. Deploying the same artifact
can use `PostgresOperator::install_program(&client, request_key, ident, &program,
&dependencies, source, timeout)`. This stages the complete immutable closure and
submits ordinary `:db/ident` / `:db/fn` assertions. The `source: EntityRef`
attributes every assertion and retraction in the transaction, including its
automatically generated `:db/txInstant` assertion. Dependencies may be supplied
in any order; an exact retry, including the same source, resolves its durable
receipt before inspecting them.

For applications composing their own binding transaction,
`PostgresOperator::deploy_program(&program)` returns a `ProgramHash`. Fixed
dependencies must already be present. The binding transaction supplies source
once through its request; its ordinary assertions and automatic timestamp carry
that source, as described in [transaction data](04_transaction_data.md).
Staged code is protected for the GC grace period; no application-owned deployment
handle or release call is necessary.
Committed database roots retain bound code. Collection removes expired unbound
staging roots and their otherwise-unreferenced code. Missing dependencies fail
rather than publishing broken code. Transaction-kind programs can consume
the query result using ordinary VM instructions and emit transaction forms;
they still run against the transaction's immutable db-before. Staging alone does
not install a function binding; `install_program` performs both steps.

Each emitted assertion or retraction receives the enclosing transaction's source.
The write instructions consume these stack operands, listed bottom to top:

| Instruction | Operands |
| --- | --- |
| `EmitAdd(attribute)` / `EmitRetract(attribute)` | `[entity, value]` |
| `EmitRetractAll(attribute)` | `[entity]` |
| `EmitCas(attribute)` | `[entity, old, new]` |
| `EmitRetractEntity` | `[entity]` |
| `EmitEntityMap` | `[map]` |

Programs emit ordinary transaction forms without attribution arguments.
`EmitEntityMap` emits an ordinary entity map, including any nested maps; all
resulting facts receive the enclosing transaction's source. `EmitCall` passes
the called program's ordinary application arguments. The function binding's
source describes who installed the binding; the enclosing transaction's source
attributes its emitted writes. That transaction source participates in canonical
request identity. EDB generates the transaction timestamp automatically after
selecting its instant, without reading the clock during program expansion.
Ensure and partition instructions retain their existing operands because they
do not emit datoms.

Native templates support the shared evaluator's predicates, functions, recursive
rules, `not`/`not-join`, `or`/`or-join`, dynamic attributes, aggregates and Pull.
Input mappings are positional program-argument indexes; tuple, collection and
relation inputs use runtime vectors. All named database sources derive from the
invocation's exact value, including its existing filters:

- Empty sources mean `QueryTemplateSource::current("$")`.
- `QueryTemplateSource::history("$history")` exposes assertions and retractions.
- `.as_of(QueryTemplateTime::Argument(2))` and `.since(...)` accept a nonnegative
  Long T, a transaction Ref, or an Instant argument. `Literal(TimePoint::...)`
  embeds an explicit bound. Bounds compose with the captured value's view.
- `QueryTemplateSource::log("$log")` supplies its actual captured committed log
  to `Function::TxIds` / `TxData`. Database time filters do not truncate that
  log; use explicit log-function ranges. Eager fixtures, speculative values and
  opaque predicate filters cannot supply a committed log and return Unsupported.
  Database history is never substituted for a log, including with `noHistory`.

The evaluator shares the enclosing program's fuel, deadline, cancellation,
row/collection and retained-value allocation limits. Failed query work is not
refunded when a caller reuses a `ProgramBudget`. Durable ASTs also have bounded
depth, node counts and the existing 4 MiB program payload limit. These are
cooperative limits, not hard allocator or PostgreSQL preemption guarantees.

This is native serializable code, not closure/JVM serialization: callback
extensions, custom aggregate callbacks, callback Pull transforms and random/sample
aggregates are rejected. Compiled transaction callbacks are a separate explicit
[native deployment](../07_peer_api/02_native_computation.md), not program serialization.
Ordinary query containers use VM vectors/maps where those preserve their shape.
`RuntimeValue::Query` carries general data losslessly, including arbitrary map
keys, sets, characters and inert tags. The existing scalar-only result variant
remains `ProgramOutput::Query`; non-stored cells use `GeneralQuery`. These query
carriers are not additional stored transaction-value types. Use
`execute_query_general` for general arguments and `QueryTemplateSource::relation`
for named raw relation arguments. See [general query data](../05_query_and_pull/01_application_data.md).

Release validation should combine recursive selection and native execution,
speculative and committed execution, retained prior bindings, exact retries
before and after restart, and canonical program round trips on a disposable
isolated database.
