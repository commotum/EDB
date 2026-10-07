# Transaction Functions

This page describes transaction functions, which allow arbitrary validations and
transformation of transaction data.

## Transaction Function Semantics

A transaction function lets you build transactions that are flexible based on the
value of the database at the start of the transaction (db-before). Rather than
determining all the values a transaction needs prior to submitting the transaction,
you can use transaction functions to calculate values based on the current state
of the database (db-before).

A transaction function is a pure function `[db-before, args] -> tx-data`, i.e. a
transaction function takes db-before plus args you provide and produces tx-data
for inclusion in the transaction. Every generated assertion or retraction carries
an explicit source entity reference. A function can receive sources through its
ordinary arguments; its binding or deployment does not supply them implicitly.

Transaction functions support the operation of
[`DatabaseValue::with_forms`](05_transaction_model.md), which computes a
speculative transaction report without committing it. The transaction processor
calls transaction functions to augment tx-data, producing more tx-data:

```text
tx-data + (tx-fn db-before args) -> tx-data'
```

Transaction functions are tightly focused: they do not see the entire tx-data,
only the args passed to them. They also do not see each other's return value.
This tight focus is semantically critical: if the processor piped the return
value from one transaction function into the next, transactions would have order
semantics and no longer be declarative!

All function calls, including nested calls returned by other functions, read the
same immutable db-before. Database function bindings also resolve in db-before:
installing or replacing a function and invoking it in the same transaction does
not make the new binding available to that invocation. Entity predicates are a
different consistency mechanism: an explicitly ensured entity predicate reads
the complete proposed db-after.

## When to Use Transaction Functions

Consistency refers to the property that a database transaction takes the database
from one valid state to another. EDB has a number of built-in consistency checks
that you can augment by writing custom entity predicates and transaction
functions. Prefer built-in features over writing custom code where they fit your
use case. Generally speaking, work your way down the table below, preferring the
approaches listed earlier if they are sufficient for your needs.

| Desired Consistency | EDB Feature |
| --- | --- |
| value type | attribute [value type](../03_schema/02_schema_reference.md#dbvaluetype) |
| uniqueness | attribute [uniqueness constraint](../03_schema/02_schema_reference.md#dbunique) |
| single / multi value attribute | attribute [cardinality](../03_schema/02_schema_reference.md#dbcardinality) |
| optimistic concurrency at the level of a single attribute value | [`:db/cas`](#dbcas) |
| attribute predicate | attribute spec [`:db.attr/preds`](../03_schema/02_schema_reference.md#attribute-predicates) |
| entity required attributes | entity spec [`:db.entity/attrs`](../03_schema/02_schema_reference.md#required-attributes) |
| entity predicate against db-after | entity spec [`:db.entity/preds`](../03_schema/02_schema_reference.md#entity-predicates) |
| predicates and transformations of transaction data, given db-before | [custom transaction function](#types-of-transaction-functions) |
| work coordinating multiple committed transactions or external effects | application coordination using [synchronization](../07_peer_api/00_connections.md) and [change consumers](../07_peer_api/03_change_consumers.md) |

## Performance and Security

By their nature, transaction functions and entity predicates run inside the
serialized pipeline of transactions for a database. A slow transaction function
or entity predicate will impact not only the current transaction, but transaction
requests queued behind it. Do the minimal amount of work possible, and do only
work that requires access to the in-transaction value of the database.

Native transaction functions and entity predicates are trusted Rust code and
should be safeguarded in the same ways you would safeguard any other mechanism
for deploying code into production. Stored programs use a validated, bounded
instruction set, but installing or changing their database bindings still changes
application behavior. Control who can submit transactions that change those
bindings and schema predicates.

Native controls are cooperative. Call `NativeCallContext::check(work)` in loops
and `reserve(bytes)` before substantial temporary allocation. The engine also
checks callback inputs and outputs and contains unwinding callback panics as
errors. It cannot preempt a callback that loops or allocates without cooperation.
Do not send mail, charge cards, read changing external state, or perform other
external effects inside a transaction function; react to a committed transaction
separately.

## Types of Transaction Functions

EDB supports two deployment approaches: stored database programs and compiled
native functions. Both produce transaction data from db-before and arguments,
but their authoring environments and deployment requirements differ.

1. You can bind a **database function** to an entity's `:db/fn` attribute. Its
   validated program and fixed dependencies are stored as immutable,
   content-addressed data. Stored programs accept up to 10 explicit arguments;
   db-before is supplied separately by the runtime.
2. **Native functions** are ordinary Rust callbacks registered by the application
   hosting the transaction service. They are available only in processes supplied
   with that registry.

| | Database Function | Native Function |
| --- | --- | --- |
| invoke | a transaction list/vector starts with a keyword naming the function, followed by arguments | a transaction list/vector starts with a fully qualified symbol naming the deployment, followed by arguments |
| develop | construct a `Program` with `ProgramKind::Transaction` and VM instructions | write Rust code and register it using `NativeRegistry::builder().transaction(...)` |
| test | run `ProgramRuntime::execute_runtime` on a fixture, or speculate against a captured database with its installed binding | test Rust helpers and use `with_forms_with_execution_options` with the registry |
| deploy | stage immutable program content and transact its `:db/fn` binding with `PostgresOperator::install_program` | build and deploy a host that supplies `TransactionExecutionOptions::native` |
| resolve | look up the entity in db-before and retrieve its `:db/fn` program hash | look up the symbol in the host's explicit native registry |
| version control | historical database bindings identify immutable program content | retain source and build artifacts externally; optionally use versioned deployment symbols |
| execution | interpreted, validated instruction set with shared resource limits | trusted Rust code with cooperative resource controls |

Native functions receive `&DatabaseValue`, `&[RuntimeValue]`, and
`&mut NativeCallContext`, and return `Result<Vec<TxForm>, SemanticError>`.
There is no fixed ten-argument API restriction for native callbacks; each callback
validates its own argument shape and the transaction's resource limits still apply.

`TxFunctions` is also available as a local convenience for Rust callbacks taking
`TxValue` arguments. Its `TxForm::Call` values cannot cross the authoritative
service boundary. Use a `NativeRegistry` deployment for functions invoked through
the transaction service. See [native application computation](../07_peer_api/02_native_computation.md).

## Invoking Transaction Functions

The transaction excerpts below assume that entity `99` identifies the contributor.
When submitting them, include `[:db/txInstant 99]` to assert the transaction's
selected instant with that source, unless another form already asserts a sourced
`:db/txInstant`. This operation does not supply a default source for the other
forms.

EDB recognizes built-in operations in transaction list/vector forms and treats
other callable heads as function references. For example, the following
transaction data includes a call to the built-in `:db/retractEntity` operation:

```edn
[[:db/retractEntity [:person/email "jdoe@example.com"] 99]]
```

Custom transaction functions can abort a transaction by returning an error, or
expand to possibly empty data that will be included in the transaction.

The following Rust function constructs a trivial database program:

```rust
use edb_core::*;

fn add_doc_program() -> Program {
    Program {
        kind: ProgramKind::Transaction,
        arity: 3,
        instructions: vec![
            Instruction::PushArgument(0),
            Instruction::PushArgument(1),
            Instruction::PushArgument(2),
            Instruction::EmitAdd(DB_DOC as u32),
            Instruction::Return,
        ],
    }
}
```

After [installing](#deploying-database-functions) this program as `:app/add-doc`,
invoke it in a later transaction:

```edn
[[:app/add-doc #edb/ref 17592186062233 "this is foo's doc" #edb/ref 99]]
```

Replace the illustrative number with an existing entity's ID. The `#edb/ref`
tag makes the argument a runtime entity reference and accepts a numeric ID,
not a tempid string. An untagged string passed to a function remains a string;
the VM's `EmitAdd` instruction consumes `[entity, value, source]` stack operands
(bottom to top), with entity references in the entity and source positions.
Typed Rust callers can also supply a `RuntimeValue::Entity(EntityRef::Temp(...))`
when creating an entity.

An equivalent native deployment can accept the original string tempid directly:

```rust
use edb_core::*;

fn doc_registry() -> Result<NativeRegistry, SemanticError> {
    let mut builder = NativeRegistry::builder();
    builder.transaction(Symbol::new("my.fns", "add-doc"), |_db, args, control| {
        let [RuntimeValue::Scalar(Value::String(e)),
             RuntimeValue::Scalar(Value::String(doc)),
             RuntimeValue::Entity(source)] = args else {
            return Err(SemanticError::incorrect(
                "app/arguments", "add-doc requires a tempid, doc string, and source reference",
            ));
        };
        control.reserve(e.len().saturating_add(doc.len()))?;
        Ok(vec![TxForm::Op(TxOp::Add {
            entity: EntityRef::Temp(e.clone()),
            attribute: DB_DOC as u32,
            value: Value::String(doc.clone()).into(),
            source: source.clone(),
        })])
    })?;
    Ok(builder.build())
}
```

Supply this registry to the host and submit:

```edn
[[my.fns/add-doc "foo" "this is foo's doc" #edb/ref 99]]
```

The EDN reader never loads or evaluates Rust code. A symbol names an already
registered callback. Database callable references can also be entity IDs or
lookup refs; the typed `CallableRef::ExactHash` and EDN `#edb/function` form can
pin an exact stored program. A missing database function or missing native
deployment fails instead of silently switching between deployment mechanisms.

## Built-In Transaction Functions

The following operations are automatically available in EDB.

### :db/retractEntity

The `:db/retractEntity` operation takes an entity ID and a source entity ID. It
retracts all the attribute values where the given entity ID is either the entity or a
reference value, effectively retracting the entity's own data and any references
to the entity as well. Entities that are
[components](../03_schema/02_schema_reference.md#dbiscomponent) of the given
entity are also recursively retracted. Every resulting retraction carries the
supplied source. Historical assertions retain their original attribution.

The following examples specify an entity by ID or by a
[lookup ref](04_transaction_data.md). The numeric IDs are illustrative; use
actual allocated IDs from your database.

```edn
[[:db/retractEntity 17592186062233 99]]
;; or with a lookup ref
[[:db/retractEntity [:person/email "jdoe@example.com"] 99]]

;; example of what :db/retractEntity might expand to
;; attributes shown by name for readability
[[:db/retract 17592186062233 :person/email "jdoe@example.com" 99]
 [:db/retract 17592186062233 :person/name "Jane Doe" 99]
 [:db/retract 17592186062232 :team/members 17592186062233 99]]
```

Expansion reads db-before and follows reference-typed incoming attributes;
ordinary numeric values equal to the entity ID are not references. Component
cycles are visited only once. Transaction entities cannot be retracted. The
spelling `:db.fn/retractEntity` is also accepted.

### :db/cas

The `:db/cas` (compare-and-swap) operation takes five arguments: an entity ID, an
attribute, an expected current value, a new value, and a source entity ID. The
attribute must be `:db.cardinality/one`. If the entity has the expected value for the given
attribute in db-before, `:db/cas` asserts the new value. Otherwise, the transaction
aborts with a `Conflict` error whose code is `transaction/cas-failed`.

You can use `nil` for the old value to specify that the new value should be
asserted only if no value currently exists. The new value must be a valid value
for the attribute; `nil` is not a replacement-value deletion operation.

The following example asserts an account's balance to be 110 if and only if it
is 100 at the time the transaction executes. Replace the illustrative entity ID
with the account's actual ID:

```edn
[[:db/cas 17592186062233 :account/balance 100 110 99]]

;; the resulting change, if the old balance is 100
[[:db/retract 17592186062233 :account/balance 100 99]
 [:db/add     17592186062233 :account/balance 110 99]]
```

The old-value check observes db-before, not preceding forms in the transaction.
Cardinality-one replacement supplies the retraction of the previous value.
Both the retraction and the new assertion carry the supplied source, independently
of the source that asserted the old value.
The spelling `:db.fn/cas` is also accepted.

### :db/force-partition

The `:db/force-partition` directive takes a map of tempids to desired partitions.
In EDN it is a transaction map directive, not a function-call vector:

```edn
[{:db/force-partition {"person" :db.part/user}}
 {:db/id "person" :db/source 99 :person/name "Jane Doe"}]
```

### :db/match-partition

The `:db/match-partition` directive takes a map of tempids to entities that are
in desired partitions:

```edn
[{:db/match-partition {"person" [:person/email "jdoe@example.com"]}}
 {:db/id "person" :db/source 99 :person/name "John Doe"}]
```

These directives control new allocation; they do not move existing or upserted
entities. See [partitions](02_partitions.md) for the native forms and allocation
rules. Programs can emit the same policies with `EmitForcePartition` and
`EmitMatchPartition`.

## Writing Transaction Functions

If you have a consistency requirement not covered by a built-in feature, you can
write a custom transaction function, adhering to the following rules:

1. It must be pure, free of side effects. Rust's callback type does not enforce
   purity; it is an application responsibility.
2. It must use the supplied db-before and data arguments. Native callbacks take
   the database as their first argument; stored programs read it through VM
   instructions and address explicit arguments with `PushArgument`.
3. On success, it must return valid transaction data with explicit sources on
   assertions and retractions, and can include more transaction functions.
   Generated data goes through ordinary transaction validation; returning it
   does not bypass schema or consistency checks.
4. To abort, return a `SemanticError` from native code, or reject through a stored
   program's `Require` or `RequireAnomaly` instruction.
5. Use EDB's native argument representations. EDN lists and vectors become
   `RuntimeValue::Vector`, maps become runtime maps with stored-value keys, `nil`
   becomes `RuntimeValue::Null`, and `#edb/ref` carries an entity reference.
   There is no JVM collection or closure serialization boundary.

Stored programs can emit a timestamp assertion with `Instruction::EmitTxInstant`,
which consumes `[source]` from the stack and emits `TxOp::TxInstant { source }`.
Native callbacks can return that operation in a `TxForm::Op`. The operation uses
the transaction's selected instant; neither callback nor VM expansion reads the
clock to produce it.

Stored programs have at most 10 arguments, 4,096 instructions, and 32 levels of
instruction-block nesting. Nested transaction-function expansion has a separate
32-level depth check. Sibling and nested calls share execution limits rather than
receiving fresh quotas. `ProgramLimits` controls fuel, stack size, cumulative
value allocation, collection size, emitted forms, output bytes, and call count;
the enclosing transaction also has admission and assessment limits. Native
callbacks use the same budget through their call context. See
[persisted programs](01_persisted_programs.md) and
[transaction diagnostics](03_diagnostics.md).

## Canceling a Transaction

A native function cancels the transaction by returning `Err(SemanticError)`.
The error returns to the caller and no generated transaction data is committed.
Choose `ErrorCategory::Incorrect` for invalid input or `ErrorCategory::Conflict`
for a precondition that conflicts with the database state.

For structured cancellation in stored programs, `Instruction::RequireAnomaly`
consumes a boolean condition followed by an anomaly map. Literal `true`
continues; literal `false` cancels. The map requires
`:cognitect.anomalies/category` with one of these values:

- `:cognitect.anomalies/incorrect`
- `:cognitect.anomalies/conflict`

When `:cognitect.anomalies/message` is provided, it must be a string and becomes
the error message. All map keys must be namespace-qualified keywords. Values must
be representable as runtime data. Cancellation returns `program/rejected`,
preserves the typed anomaly, and adds `:edb/cancelled true`. The simpler `Require`
instruction supplies a category and message without constructing an anomaly map.

The following native function accepts a user map and a source reference, and
ensures that the map has both `:name` and `:email` strings. Install `:user/name`
and `:user/email` as string attributes before invoking it.

```rust
use edb_core::*;

fn user_registry() -> Result<NativeRegistry, SemanticError> {
    let mut builder = NativeRegistry::builder();
    builder.transaction(Symbol::new("my.fns", "add-user"), |_db, args, control| {
        let invalid = || SemanticError::incorrect(
            "app/invalid-user", "User map must contain :email and :name strings",
        );
        let [RuntimeValue::Map(entries), RuntimeValue::Entity(source)] = args else {
            return Err(invalid());
        };
        control.check(entries.len() as u64)?;
        let field = |name: &str| entries.iter().find_map(|(key, value)| {
            if key != &Value::Keyword(Keyword::unqualified(name)) { return None; }
            match value {
                RuntimeValue::Scalar(Value::String(s)) => Some(s),
                _ => None,
            }
        });
        let name = field("name").ok_or_else(invalid)?;
        let email = field("email").ok_or_else(invalid)?;
        control.reserve(name.len().saturating_add(email.len()))?;
        Ok(vec![TxForm::EntityMap(EntityMap {
            id: None,
            source: Some(source.clone()),
            attributes: vec![
                (AttributeRef::Ident(Keyword::new("user", "name")),
                 MapValue::Value(Value::String(name.clone()).into())),
                (AttributeRef::Ident(Keyword::new("user", "email")),
                 MapValue::Value(Value::String(email.clone()).into())),
            ],
        })])
    })?;
    Ok(builder.build())
}
```

Submit each of the following separately through a host with that registry. The
first transaction succeeds; the second is canceled because it has `:address`
instead of `:email`:

```edn
[[my.fns/add-user {:name "Marshall" :email "test@test.com"} #edb/ref 99]]

[[my.fns/add-user {:name "Marshall" :address "test@test.com"} #edb/ref 99]]
;; Incorrect, app/invalid-user:
;; User map must contain :email and :name strings
```

Cancellation flags and deadline/resource-limit failures are separate from these
application rejections. Direct program execution can observe a cancellation
flag through `ProgramControl`; transaction execution and native contexts enforce
their enclosing controls cooperatively.

## Testing Transaction Functions

Native transaction functions are ordinary code and can be developed and tested
in whatever environment or IDE you use for Rust. Test the computation directly
where useful, and test its emitted forms through the transaction processor to
catch schema, reference resolution, and consistency failures.

For example, the `doc_registry` above can be exercised without a live service:

```rust
use edb_core::edn_transaction::read_edn_transaction;

let db = DatabaseValue::from(Database::new(Schema::new())?);
let source = db.entid(&Keyword::new("db.source", "system")).expect("built-in source");
let options = TransactionExecutionOptions {
    native: doc_registry()?,
    ..Default::default()
};
let forms = read_edn_transaction(
    &format!(r#"[[:db/txInstant {source}]
                 [my.fns/add-doc "foo" "this is foo's doc" #edb/ref {source}]]"#),
)?;
let report = db.with_forms_with_execution_options(
    &forms, 1, SpeculationLimits::default(), &options,
)?;
assert!(report.tempids.contains_key("foo"));
```

Stored program tests can call `ProgramRuntime::execute_runtime` against an
in-memory `Database`, using `RuntimeValue::Entity(EntityRef::Temp(...))` for
entity arguments. This returns generated forms; submit those forms to the
transaction processor to test the complete transaction. To test temporal
bindings and dependency resolution, speculate on a captured database value
containing the installed function.

## Deploying Transaction Functions

Database functions and native functions are deployed differently.

### Deploying Database Functions

Deploy a database function by binding it to an entity's `:db/fn` attribute, whose
value type is `:db.type/fn`. Normally you also add a `:db/ident` to serve as its
name, and optionally a `:db/doc` string. The function value is a `Value::Function`
containing a program hash; retrieving it does not return a compiled Rust closure.

`PostgresOperator::install_program` stages the complete immutable program closure
and submits ordinary `:db/ident` and `:db/fn` assertions together with a
`TxOp::TxInstant` operation. Given a configured operator and running service,
install the earlier program with contributor `99` as the source of the binding
assertions and the transaction's selected `:db/txInstant`:

```rust
operator.install_program(
    &service.client(),
    "install-add-doc",
    Keyword::new("app", "add-doc"),
    &add_doc_program(),
    &[],
    EntityRef::Id(99),
    std::time::Duration::from_secs(10),
)?;
```

The dependency slice supplies any fixed program dependencies. Dependencies must
be available before a binding can become usable. `deploy_program` is the lower
level staging API for applications composing their own binding transaction;
staging alone does not install a function. Bound code is retained by database
roots; unbound staged code has a collection grace period. See
[persisted programs](01_persisted_programs.md).

Bindings are temporal database data. Replacing `:db/fn` changes subsequent calls
against newer database values while retained prior values resolve their own
bindings. An exact accepted request retry resolves its durable receipt before
code lookup; rebinding or removing a deployment does not cause that accepted
request to execute again.

### Deploying Native Functions

Committed transactions execute on the transaction service, so register functions
in every writer or standby host that may execute them. Speculative transactions
execute wherever you call `with_forms_with_execution_options`; those processes
also need the appropriate registry.

Build an immutable registry and pass it in `TransactionExecutionOptions`:

```rust
let options = TransactionExecutionOptions {
    native: doc_registry()?,
    ..Default::default()
};
let service = TransactionService::start_with_execution_options(config, options)?;
```

The stock transactor has an empty native registry. Compile an application host
that installs the needed callbacks, and deploy matching code to each relevant
process. Use ordinary Rust build and distribution tools; there is no classpath,
runtime library loading, or database storage of native machine code. Names such
as `my.fns.v1/add-doc` can distinguish versions explicitly.

The [native transaction host](../../examples/native_transaction_host.rs) and
[its deployment instructions](../07_peer_api/02_native_computation.md#runnable-host-and-ordinary-edn-client)
provide a complete compiled-host and ordinary-EDN-client example. Native schema
predicates use separately registered roles and may have temporal database aliases;
see the same guide for `:edb.native/deployment` bindings.
