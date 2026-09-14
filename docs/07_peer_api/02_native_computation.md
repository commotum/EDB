# Native application computation

Queries and transactions accept ordinary Rust application logic without a
Clojure runtime. Keep application functions pure: their result must depend only
on supplied data and captured database values. Do not send mail, charge cards or
perform other external effects inside them; react to a committed transaction
separately.

## Grouped custom aggregates

Register a callback with `QueryExtensions::register_aggregate`. A custom
`FindElement::CustomAggregate(AggregateCall)` takes ordered `AggregateArg`
variables, constants and explicit named sources. EDN uses the same shape:

```edn
[:find ?team (app/weighted ?hours ?weight 10)
 :with ?ticket
 :in $planning
 :where [$planning ?team ?hours ?weight ?ticket]]
```

The callback receives an `AggregateGroup` and remaining `QueryControl`. Access
aligned cells with `value(row, argument)`, a column with `column(argument)`,
constants with `constant(argument)` and captured sources with `source(argument)`.
Cells and sources are borrowed; no whole-database or whole-group copy is needed.
Call `group.check(work)` in loops to debit shared work and observe cancellation
and deadlines. Return one `QueryValue`, which may itself be a map or collection.

Non-aggregate find variables group the relation. `:with` retains distinctions
that would otherwise collapse before aggregation. A global aggregate sees one
empty group when no rows match; a grouped aggregate sees no groups. Existing
built-in aggregates keep their behavior. Prepared queries do not capture a
callback registry: each invocation supplies its own registry and exact sources.
The [application example](../../examples/application_workflow.rs) demonstrates a
weighted planning calculation against old and new immutable values.

Custom aggregate and row callbacks are process-local code. They are not serialized
into stored programs; attempting that returns `program/nonportable-query`.

## Portable data functions

The typed `Function` variants and EDN names work without a database:

| EDN name | Meaning |
| --- | --- |
| `count` | Nil is zero; count strings and supported collections/maps/sets. |
| `quot` | Numeric quotient truncated toward zero, with checked exact arithmetic. |
| `subs` | String slice from start, optionally to exclusive end. |
| `str` | Concatenate readable native values; top-level nil contributes nothing. |
| `starts-with?`, `ends-with?`, `includes?` | String tests, usable as predicates or value-producing functions. |

String `count` and `subs` use Unicode scalar positions, not UTF-8 byte offsets,
UTF-16 code units or grapheme clusters. For `"a🦀界"`, count is 3 and `(subs s 1 2)`
is `"🦀"`. Existing stored-program `Length` retains its original UTF-16 behavior.
Bad types/bounds, zero divisors, nonfinite quotients and exhausted controls return
errors. Exact decimals retain exactness; rendering uses compact numeric notation
when expanded decimal text would be unreasonable.

The corresponding `clojure.core/` aliases are accepted for count/quot/subs/str;
`clojure.string/` aliases are accepted for the three string predicates. This is
explicit name mapping, not JVM lookup. Other qualified names remain explicit
extensions. All persisted programs use the same current canonical format,
including portable helpers, fulltext, and general query data.

## Compiled Rust transaction deployments

Build a `NativeRegistry` using its builder, then supply it in
`TransactionExecutionOptions` to `TransactionService::start_with_execution_options`
or the configured/indexing equivalent. Register transaction functions, attribute
predicates and entity predicates separately. The same immutable registry works
with `DatabaseValue::with_forms_with_execution_options` for exact speculation.
`TxFunctions` remains a local callback convenience using `TxValue` arguments.
Its callbacks receive `DatabaseValue`; in-memory `with_forms` and
`DatabaseValue::with_functions` use the same selective transaction assessor.
Content-addressed stored programs remain available.

Deployment symbols are ordinary fully qualified names, for example
`demo.people/add-person`. Application deployment policy controls code versions;
versioned names such as `demo.people.v1/add-person` are also accepted.
Deploy the same code to every writer/standby and speculative application that
needs it. The database does not archive native machine code; retain source/build
artifacts externally. Do not silently reuse a version for incompatible code.

Functions receive exact db-before, `RuntimeValue` arguments and a
`NativeCallContext`, and return transaction forms. Returned calls expand under the
same transaction controls. Attribute predicates receive values; ensured entity
predicates receive the complete proposed db-after. Only literal true accepts a
predicate; rejection data/errors and panic containment follow the native boundary.
The receipt check happens first: removing or changing a deployment cannot change
an already accepted request's exact retry. A new request needing missing code fails.

Native schema predicates can name a registered callback directly. After
registering `demo.people/valid-name?` as an attribute predicate, attach it to
an existing attribute:

```edn
[[:db/add :person/name :db.attr/preds demo.people/valid-name?]]
```

Entity predicate symbols work the same way through `:db.entity/preds`.
An optional temporal alias can bind a stable database ident to a native name.
To use aliases, install `native_deployment_attribute(id)`, then transact:

```edn
[{:db/ident :checks/valid-person
  :atomic.native/deployment demo.people/valid-person}]
```

Reference `checks/valid-person` from `:db.attr/preds` or `:db.entity/preds` as
appropriate. A binding has either `:db/fn` content or an explicit native deployment,
never both. A missing/corrupt stored-program binding is not a native fallback.
The marker is historical Symbol/cardinality-one data; backup/restore preserves it
without executing the callback or storing a native closure.

Controls are cooperative. `NativeCallContext::check` accounts work and
`reserve` admits temporary allocation; callback inputs/outputs are checked by the
engine too. A callback that loops or allocates without cooperation cannot be
preempted by these controls. Only load trusted application code.

## Runnable host and ordinary EDN client

[native_transaction_host.rs](../../examples/native_transaction_host.rs) is a small
compiled transactor extension that normalizes person names using ordinary Rust
Unicode/string operations. Provision a database and `:person/name` first using
the [standard setup](../01_tutorials/01_application_workflow.md). Build and start the host in a terminal:

```sh
cargo build --bin atomic --example native_transaction_host
target/debug/examples/native_transaction_host people
```

The host uses the same explicit PostgreSQL environment configuration as `atomic`,
prints `READY endpoint=...`, and stops on newline/EOF. Its optional second argument
is a stable socket path in an owned mode-0700 directory. In another terminal,
submit this file through `atomic transact --database people --endpoint SOCKET
--request-key person-1 --file person.edn`:

```edn
[[demo.people.v1/add-person "person" "  ALICE   Smith  "]]
```

The result stores `"alice smith"` under `:person/name`. The stock client does not
load code; it sends ordinary data to the explicitly extended host. The stock
transactor has an empty native registry. Restart the custom host with the same
deployment for new calls; accepted retries retain their original results.

This first release uses the current storage and program formats. Create a fresh
database when development formats change; historical migrations and mixed-version
writers are not supported. See [operations](../08_operations/00_deployment.md) for current installation
and recovery procedures.
