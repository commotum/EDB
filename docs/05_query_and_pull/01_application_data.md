# Query application data

Not everything an application wants to query belongs in its durable schema.
Named raw relations can have any positive number of columns, including nested
maps, sequences, sets, nil, characters and inert tagged data. Join them to a
captured `DatabaseValue`, to other relations, or query them without a database.
The same engine, rule evaluation and prepared-query APIs handle all three cases.

## Native Rust values and relations

`Value` remains the stored attribute-value domain. `QueryValue` additionally
represents general query data; using a character or map in a query does not make
it a new stored attribute type. `QueryInput::General(value)` supplies a scalar,
tuple, collection or relation according to the corresponding `InputSpec`.
Existing typed inputs and `QueryDataSource::tuples` remain available.

Use `QueryDataSource::relation(name, Vec<Vec<QueryValue>>)` for general rows and
`Clause::RelationPattern(Box::new(RelationPattern { source, terms }))` for patterns
of arbitrary width. Existing `DataPattern` remains the convenient, optimized
datom-shaped interface and also matches short raw rows. `Term::QueryConstant`
introduces a general literal; existing stored literals keep `Term::Constant`.
Rules, repeated variables, named sources and prepared reuse work with both.

`QueryValue::Set` has unordered, duplicate-insensitive equality. Maps may have
general keys, but duplicate keys under native query equality are rejected.
`Tuple` and `Collection` retain their existing ordered meanings; a set can supply
a collection input but cannot stand in for a positional tuple or relation row.
EDN lists and vectors adapt to native sequential data. This is data equivalence,
not a reimplementation of Clojure's runtime classes.

Exact numbers are not stringified or narrowed. Cross-numeric equality follows
the existing query engine. A map with two EDN keys that collapse under that
equality is rejected instead of losing an entry.

## Pure application functions

Register a native function without inventing a database source:

```rust
let mut extensions = atomic_core::QueryExtensions::new();
extensions.register_pure("app/wrap", |arguments, _control| {
    let [value] = arguments else {
        return Err(atomic_core::SemanticError::incorrect("app/arity", "expected one argument"));
    };
    Ok(atomic_core::QueryValue::Tuple(vec![value.clone()]))
});
```

The result is one `QueryValue`; the clause's scalar/tuple/collection/relation
binding determines how it is destructured. EDN can call this registry entry as
`[(app/wrap ?input) ?wrapped]`. It does not evaluate code from the input text.
The existing database-aware `register_local` callback ABI is unchanged.

Callbacks receive the query controls and must cooperate with cancellation and
deadlines during expensive work. Atomic validates returned data and resumes
budget checks, but cannot forcibly interrupt arbitrary Rust code or recover an
allocation the callback already made. Keep callbacks deterministic when stable
database values must produce reproducible application results.

`QueryControl::max_value_bytes` bounds cumulative accounted value allocations and
conversion scratch; `max_work`, row limits and the existing join-memory control
remain separate. Native library defaults preserve unbounded broad reads; set an
explicit allowance for untrusted queries. The CLI uses 16 MiB and accepts
`--max-value-bytes` to change it. These are logical cost controls, not
process RSS or live-retention limits. A
prepared query retains its structure, not input values or an extension registry;
nil and general inputs can reuse it without rewriting its clauses.

## EDN and the application command

A source file, `planning.edn`, can contain:

```edn
{$rows [["Alice" nil \A #app/state :ready {:priority 1} #{:blue :green} 7]]}
```

With this query in `query.edn`:

```edn
[:find ?name ?details ?state
 :in $rows
 :where [$rows ?name nil _ ?state ?details _ 7]]
```

Run `atomic query --file query.edn --sources planning.edn`. No `--database` or
PostgreSQL configuration is needed for data-only queries. Either file may be
stdin (`-`), but not both. The stock executable provides built-in functions;
application callbacks require a Rust host that registers them.

To mix durable and application-owned data, add a source descriptor such as
`$customers {:database "customers" :as-of 42}` and patterns naming `$customers`.
That source uses the ordinary authorized PostgreSQL peer path. General sources
still remain local data, and every query uses the database value it captured.
`--inputs` supplies non-source arguments, including nil and nested values.
Results are ordinary readable EDN, with custom tags remaining inert.

## Persisted native queries

General literals and wide relation patterns use the same current program format
as other persisted queries. `QueryTemplateSource::relation(name, argument)`
binds a caller-supplied general relation to a persisted query's named source.
`ProgramRuntime::execute_query_general` accepts general arguments, and
`ProgramOutput::GeneralQuery` preserves non-stored result cells. Scalar-only
outputs keep the existing `ProgramOutput::Query` shape.

These are query interfaces, not a new transaction transport. General query-only
carriers cannot be submitted as stored transaction values. See
[native programs](../04_transactions/01_persisted_programs.md), [EDN](../01_tutorials/00_edn_workflow.md), and the end-to-end
[application example](../../examples/application_workflow.rs).
