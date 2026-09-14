# Partitions and time-local UUIDs

Partitions organize entity IDs for index locality. They are not shards, separate
PostgreSQL databases, tenant authorization boundaries, or independent transaction
streams. Cross-partition references, queries, history and atomic transactions use
the same database. Choose a grouping that matches actual reads and measure its
effect; assigning partitions alone does not establish a performance improvement.

Native authoring uses Rust enums and EDN transaction data.

## Identity and partition installation

`eid_to_part(entity)` returns the raw partition bits. `partition_eid(entity)`
returns the partition's entity ID; these differ for implicit partitions.
`implicit_part(number)` returns an implicit partition entity ID without a
transaction; `implicit_part_id(partition)` decodes it. Numbers are in
`0..524_288`. The upper half of the 20-bit partition field is implicit; named
partitions occupy installed codes in the lower half.

The existing 42-bit entity-index field and its sign-bit restriction remain
unchanged. Explicit IDs still need a valid partition and an issued entity index;
constructing an integer with `make_eid` does not reserve or issue it. The ordinary
default is `:db.part/user`, with an explicit execution-default option described
below. Schema/partition installation has system affinity to `:db.part/db`;
transaction time uses `:db.part/tx`. Native reserved-allocation checkpoints
separate new schema/named-partition placement from growth of ordinary entities;
they do not provide a separate counter for every application partition.
Named partition entities must still fit the lower half of the partition field,
and implicit partitions do not require installation.

Install a named partition as ordinary data, then use it in a later transaction
(policy resolves against db-before):

```rust
use atomic_core::*;

let install = TxForm::EntityMap(EntityMap {
    id: Some(EntityRef::Temp("partition".into())),
    attributes: vec![
        (AttributeRef::Ident(Keyword::new("db", "ident")),
         MapValue::Value(Value::Keyword(Keyword::new("app.part", "orders")).into())),
        (AttributeRef::ReverseIdent(Keyword::new("db.install", "partition")),
         MapValue::Value(Value::Keyword(Keyword::new("db.part", "db")).into())),
    ],
});
```

`AttributeRef::Ident(Keyword::new("db.install", "_partition"))` is also accepted
for this reverse installation spelling. It lowers to ordinary installation
facts, not a hidden schema descriptor. `db.schema().partitions()` lists installed
names. An installed partition cannot be removed while retaining its allocated
identity space. No UUID or user entity is silently migrated to another partition.

New databases include the partition vocabulary in their canonical genesis.
No separate vocabulary upgrade or generation-zero conversion is required.
Storage installation and logical database creation remain explicit
administrative actions; see [operations](../08_operations/00_deployment.md#provisioning-and-current-storage).

## Reserved allocation

Schema and named-partition allocation uses a retained, monotone system-partition
cursor, independent of ordinary entity growth. The ordinary issued frontier stays
at least as high as this cursor so explicit-ID range checks remain valid. This
is not a separate counter for each application partition, and does not move or
renumber existing entities.

The current immutable log records ordinary and reserved allocation checkpoints;
captured metadata binds the same endpoint. Receipt-only allocations and typed
references also reserve identities, so scanning current schema alone cannot
establish which IDs are safe to issue. Recovery and exact retry preserve these
checkpoints rather than reapplying today's allocation defaults. Excision carries
the retained frontier into its successor even when it removes the original facts.

The ranges are finite: named-partition entities must be below 524,288, and schema
attribute IDs must be at most 1,048,576. An explicit high system ID can advance
the retained cursor past unused lower slots. The allocator does not search holes,
clamp that observation, or reuse a possibly acknowledged identity. Exhaustion
fails explicitly.

## Transaction allocation policy

Include directives alongside actual entity assertions:

```rust
let placement = vec![
    TxOp::ForcePartition {
        tempid: "order".into(),
        partition: EntityRef::Ident(Keyword::new("app.part", "orders")),
    },
    TxOp::MatchPartition {
        tempid: "line".into(),
        entity: EntityRef::Temp("order".into()),
    },
];
```

These directives are transaction policy, never stored datoms. They do not create
entities by themselves: their target tempids need ordinary entity-position facts.
Force accepts an installed partition identity or an implicit partition ID.
Match accepts an existing entity or another transaction-local tempid. Force wins
over affinity; system installation affinity remains sticky. Duplicate equivalent
directives coalesce; contradictory directives within one policy kind reject rather than pick a winner
from input order. Unanchored affinity cycles reject.

Nested component maps automatically emit child-to-parent affinity, including
anonymous child tempids. A primitive component-reference assertion alone does not
add this map-authoring policy. An explicit match conflicting with component
affinity also rejects. Direct matching to an existing reserved db/tx entity does
not relocate ordinary data into those reserved partitions; temporary targets
carry their effective transaction-local policy. Uniqueness/upsert may resolve a
tempid to an existing entity: that identity and its partition remain unchanged.
Matching another tempid follows that tempid's original allocation policy, even
if it upserts to an existing entity elsewhere. Matching that existing entity via
its ID or lookup ref instead follows the existing partition.
When matching another tempid, its requested allocation policy is followed; an
upsert does not retroactively turn that policy into a relocation operation.

Speculation applies the same policy but reserves nothing. Submit replayable
logical intent, not speculative entity IDs. Named partitions do not remove the
database's finite ID-space and serialized-write constraints.

Persisted transaction programs can use `Instruction::EmitForcePartition` and
`EmitMatchPartition`, consuming `[tempid, partition/entity]` from the stack. A
tempid can be a native Temp reference or nonempty string; qualified keywords can
name partitions. These instructions obey the shared fuel, emitted-form and byte
budgets. The program and request codecs encode these directives explicitly; decoding
and execution apply the same allocation rules as typed transaction forms.
Use the current encoder rather than constructing protocol versions by hand.

## Default placement for new entities

Partitions group entity IDs for locality; they are not databases, access controls
or transaction boundaries. Existing APIs continue placing ordinary fresh entities
in `:db.part/user` unless transaction data supplies a stronger directive.

Configure a different default explicitly:

```rust
let defaults = TransactionDefaults::default()
    .with_default_partition(Keyword::new("part", "orders"));
let preview = database.with_edn_with_defaults(text, instant, &defaults)?;
let writer = TransactionService::start_with_defaults(config, defaults)?;
```

`Database` and `DatabaseValue` both provide `with_defaults`,
`with_forms_with_defaults`, and `with_edn_with_defaults`. The memory forms method
accepts `TxFunctions`; `DatabaseValue::with_functions_and_defaults` accepts the
same callbacks for any exact snapshot. Callbacks receive `DatabaseValue` and
use the shared selective transaction assessor. Native speculation additionally
provides `with_forms_with_limits_and_defaults` to configure allocation and resource
limits independently. The complete service constructor is
`start_with_indexing_and_defaults(config, indexing, defaults)`;
the connection keeps its explicit PostgreSQL/TLS policy.

The stock CLI accepts `--default-partition :part/orders` on `atomic transactor`
and local `atomic with`. A committing client uses the writer's policy; defaults
are not a per-request override or part of the durable request encoding.

With the usual explicit PostgreSQL environment configured, start the writer and
preview the same EDN intent in another terminal:

```sh
mkdir -m 700 /tmp/atomic-orders
atomic transactor --database orders --endpoint /tmp/atomic-orders/writer.sock \
  --default-partition :part/orders
atomic with --database orders --file order.edn \
  --default-partition :part/orders
```

### Install the partition before fresh use

Using the ordinary default, install a named partition:

```clojure
[{:db/ident :part/orders :db.install/_partition :db.part/db}]
```

Then select its keyword as the execution default. A configured name is resolved
from the exact transaction's resident db-before metadata, not from a global
environment variable. Missing names fail with
`transaction/default-partition-not-found`; nonpartition names and reserved
`:db.part/db` / `:db.part/tx` defaults fail with
`transaction/invalid-default-partition`. There is no silent user-partition fallback
for an invalid explicit configuration.

Resolution happens for fresh transactions, not writer startup or replay. A writer
can therefore return saved receipts even if its default was changed to a missing
name or the old partition alias was repurposed to a nonpartition. A rename alone
retains the old name as an alias, so that name remains usable. Install the intended
name before sending fresh work; startup alone does not validate that it exists.
After a bad default is selected, restart
with a valid default to create or repair the intended named partition.

### Allocation precedence and retry behavior

- Existing identities/upserts keep their entity IDs and partitions.
- Schema entities retain automatic system placement; transaction entities remain
  in the transaction partition.
- Explicit force directives override affinity; match directives select their
  target partition. A match to a reserved system/transaction target falls back to
  the configured application default.
- Only otherwise unassigned fresh domain entities use the default, including
  anonymous maps, nested component maps and generated transaction data.

The fallback is not implemented as a force directive for every tempid. If an
unforced tempid unifies with an explicitly forced tempid through a unique identity,
the explicit placement wins without an artificial policy conflict.

Changing defaults affects only future fresh allocations. Stored retry receipts
are checked before default resolution, and retain their original IDs, datoms,
hashes and before/after values. Recovery replays committed datoms; it does not
reevaluate current defaults. Speculation must receive the same defaults as the
writer to predict that writer's placement, and never reserves IDs.

## UUID helpers

`squuid()` and `uuid_v7()` return `Result<u128, SemanticError>`; store the result
as `Value::Uuid`. Their `_at(unix_millis)` variants accept an explicit clock but
still obtain fresh OS randomness. There is no deterministic name-to-UUID function
or request-key lookup hidden inside these generators.

Squuids have 32 leading bits of Unix seconds, UUID version 4, the standard variant,
and 90 random bits. `squuid_time_millis` reads that field at second precision; it
cannot prove that an arbitrary UUID was generated as a squuid. Times past the
32-bit seconds range reject rather than wrap.

UUIDv7 uses 48 leading Unix-millisecond bits, version 7, the standard variant and
74 random bits, following [RFC 9562 section 5.7](https://www.rfc-editor.org/rfc/rfc9562.html#section-5.7).
`uuid_v7_time_millis` checks the version and variant; out-of-range clocks reject.
The native generator has no monotonic counter: same-tick values have random
order, and clock regression can produce earlier timestamps.

Both expose creation-time information and are not credentials or authenticated
provenance. They are not transaction order, entity allocation order, or substitutes
for transaction `t`. The UUID index comparator compares signed 64-bit halves. Time-local prefixes help
group nearby times, but unsigned byte order and native index order differ at a
high-half sign boundary. No universal latency or throughput improvement is claimed.

Generate an intent once and retain its UUIDs across every retry, preferably in an
application outbox. The owned [application example](../../examples/application_workflow.rs)
stores both UUID kinds under one stable tenant key and, on restart, reconstructs
the same request from those committed values. It rejects missing committed UUID
fields instead of regenerating them. This demonstration assumes those fixture
facts are not independently removed or rewritten; it is not a general outbox.

## Verification

Validate allocation, identity, bounded locality, stored-program speculation and
commit, exact receipt replay, restart recovery and canonical encoding round trips.
Use restricted PostgreSQL roles and a separate native transactor process.
