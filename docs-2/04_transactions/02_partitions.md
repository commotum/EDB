# Partitions and time-local UUIDs

Every EDB entity is associated with a particular partition. These partitions act
as a coarse-grained grouping mechanism for entities, much as file cabinets act as
a coarse-grained grouping mechanism for paper files.

The partition is encoded via high bits in the entity ID. Therefore, entities in
the same partition are sorted together in EAVT and, within each attribute, AEVT.
If the partitions align with usage patterns, this can improve locality. For
example, consider a system with inventory, customers, and order partitions. A
query for a particular item in inventory can bring an index segment into the
peer's cache alongside other inventory-related facts. If inventory queries tend
to be followed by other inventory queries, this locality can reduce the number
of segments a peer needs to cache. Conversely, if your application tends to
access information about individual customers, it can be useful to put all of
each particular customer's data in one partition and spread customers across
thousands of [implicit partitions](#implicit-partitions). Measure the effect for
your workload; assigning partitions alone does not establish a performance
improvement.

EDB supports two types of partitions: implicit partitions and named partitions.
Both can be used in the same database. Partitions are a locality optimization.
You can ignore them completely; if you do, all your domain entities will be in
the same default partition.

Partitions are not separate PostgreSQL databases, tenant authorization boundaries,
or independent transaction streams. Cross-partition references, queries, history
and indivisible transactions use the same database. Applications can use
partitions for [read routing](#partition-sharding); [new entity scans](#new-entity-scans)
require care about the difference between allocation order and transaction time.

Native authoring uses Rust enums and EDN transaction data.

## Implicit partitions

Each EDB database includes 524,288 implicit partitions, accessed by calling
`implicit_part`. Unlike named partitions, implicit partitions require no
transaction to create. Like named partitions, each implicit partition has a
partition entity ID.

Applications can employ algorithmic schemes to group related entities in the
same partition. For example, an order processing system could assign a customer
to an implicit partition via a stable hash, and then put all of a customer's order
and line item entities in that partition. This groups the customer's data for
queries about their orders; multiple customers may share a partition.

```rust
use edb_core::{implicit_part, implicit_part_id};

let partition = implicit_part(343)?;
assert_eq!(implicit_part_id(partition)?, Some(343));
```

## Named partitions

Named partitions are useful when your domain has a modest number of named
categories for which you would like locality. In addition, named partitions are
used internally by EDB. Every database comes with three named partitions:

| Partition | Purpose |
| --- | --- |
| `:db.part/db` | System partition, used for schema and named partition entities |
| `:db.part/tx` | Transaction partition |
| `:db.part/user` | User partition, for application entities |

Schema entities are automatically placed in `:db.part/db`.
[Transaction entities](04_transaction_data.md) are automatically placed in
`:db.part/tx`. The `:db.part/user` partition is the default whenever no other
placement policy applies. You can override it with an explicit
[execution default](#default-placement-for-new-entities).

### Installing named partitions

The transaction examples below use `99` as an existing source entity id. Each
submitted transaction includes a metadata declaration such as
`{:db/id "edb.tx" :db/source 99}` to attribute its generated `:db/txInstant`;
this does not supply a default source for other forms.

To install a new named partition, transact an entity with an ident and reference
that new entity as a `:db.install/partition` value of the system entity
`:db.part/db`. For example, this transaction uses reverse-reference notation to
install the partition `:app.part/orders`:

```clojure
[{:db/id "edb.tx" :db/source 99}
 {:db/source 99
  :db/ident :app.part/orders
  :db.install/_partition :db.part/db}]
```

Use it in a later transaction: partition policy resolves against db-before.

## Identity and partition installation

`eid_to_part(entity)` returns the raw partition bits. `partition_eid(entity)`
returns the partition's entity ID; these differ for implicit partitions.
`implicit_part(number)` returns an implicit partition entity ID without a
transaction; `implicit_part_id(partition)` decodes it. Numbers are in
`0..524_288`. The upper half of the 20-bit partition field is implicit; named
partitions occupy installed codes in the lower half.

The entity-index field has 42 bits, with its high bit reserved for the sign;
permanent entity indexes must be nonnegative. Explicit IDs need a valid
partition and an issued entity index;
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
use edb_core::*;

let install = TxForm::EntityMap(EntityMap {
    id: Some(EntityRef::Temp("partition".into())),
    source: Some(EntityRef::Id(99)),
    attributes: vec![
        (AttributeRef::Ident(Keyword::new("db", "ident")),
         MapValue::Value(Value::Keyword(Keyword::new("app.part", "orders")).into())),
        (AttributeRef::ReverseIdent(Keyword::new("db.install", "partition")),
         MapValue::Value(Value::Keyword(Keyword::new("db.part", "db")).into())),
    ],
});
```

Submit `install` alongside an `EntityMap` with `id: Some(EntityRef::Tx)`,
`source: Some(EntityRef::Id(99))`, and empty attributes for transaction metadata.

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

## Partition assignment

To control partition assignment for tempids, transaction maps can include the
`:db/force-partition` or `:db/match-partition` directives. These directives request
placement for new entity IDs created by a transaction.

Because partition directives can be separate maps, they encourage keeping
partition policy separate from entity data, and make it easier to write supporting
code that manages partitions across many transactions. Both directives can also
appear in one policy map.

The following examples assume installed attributes: `:order/lineItems` is a
cardinality-many ref, `:lineItem/product` and `:order/id` are cardinality-one
strings, `:lineItem/quantity` is a cardinality-one long, and `:customer/order` is a
cardinality-many ref. See [schema](../03_schema/02_schema_reference.md) and
[transaction data](04_transaction_data.md) for their definitions and submission.

### :db/force-partition

The value of `:db/force-partition` is a map from tempid strings to desired
partitions. Named partitions are specified by their installed keyword:

```clojure
[{:db/id "edb.tx" :db/source 99}
 {:db/id "order" :db/source 99
  :order/lineItems ["item1" "item2"]}
 {:db/id "item1" :db/source 99
  :lineItem/product "chocolate"
  :lineItem/quantity 1}
 {:db/id "item2" :db/source 99
  :lineItem/product "whisky"
  :lineItem/quantity 2}
 {:db/force-partition {"order" :app.part/orders
                       "item1" :app.part/orders
                       "item2" :app.part/orders}}]
```

Implicit partitions are specified by the partition entity ID returned by
`implicit_part`, or obtained from an existing entity with `partition_eid`.
EDN does not evaluate Rust function calls; construct the transaction text in Rust,
or use `TxOp::ForcePartition`. This example places both entities in implicit
partition 343:

```rust
let partition = implicit_part(343)?;
let text = format!(r#"
[{{:db/id "edb.tx" :db/source 99}}
 {{:db/id "customer" :db/source 99 :customer/order "order"}}
 {{:db/id "order" :db/source 99 :order/id "55555"}}
 {{:db/force-partition {{"customer" {partition} "order" {partition}}}}}]
"#);
let report = database.with_edn(&text, 1_000)?;
assert_eq!(partition_eid(report.tempids["customer"])?, partition);
assert_eq!(partition_eid(report.tempids["order"])?, partition);
```

`with_edn` is pure speculation; submitting the same logical transaction through a
connection commits it. The instant supplied to speculation must be valid for
that database's transaction clock.

### :db/match-partition

Sometimes it is more convenient to request a partition via an entity in the
same partition, rather than the partition ID. The value of `:db/match-partition`
is a map from tempid strings to entities in desired partitions.

The following example places line items in the same implicit partition, 500,
as their containing order:

```rust
let partition = implicit_part(500)?;
let text = format!(r#"
[{{:db/id "edb.tx" :db/source 99}}
 {{:db/id "order" :db/source 99 :order/lineItems ["item1" "item2"]}}
 {{:db/id "item1" :db/source 99 :lineItem/product "chocolate" :lineItem/quantity 1}}
 {{:db/id "item2" :db/source 99 :lineItem/product "whisky" :lineItem/quantity 2}}
 {{:db/force-partition {{"order" {partition}}}
   :db/match-partition {{"item1" "order" "item2" "order"}}}}]
"#);
let report = database.with_edn(&text, 1_000)?;
for tempid in ["order", "item1", "item2"] {
    assert_eq!(partition_eid(report.tempids[tempid])?, partition);
}
```

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

The stock CLI accepts `--default-partition :part/orders` on `edb transactor`
and local `edb with`. A committing client uses the writer's policy; defaults
are not a per-request override or part of the durable request encoding.

With the usual explicit PostgreSQL environment configured, start the writer and
preview the same EDN intent in another terminal:

```sh
mkdir -m 700 /tmp/edb-orders
edb transactor --database orders --endpoint /tmp/edb-orders/writer.sock \
  --default-partition :part/orders
edb with --database orders --file order.edn \
  --default-partition :part/orders
```

### Install the partition before fresh use

Using the ordinary default, install a named partition:

```clojure
[{:db/id "edb.tx" :db/source 99}
 {:db/source 99 :db/ident :part/orders :db.install/_partition :db.part/db}]
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

## Partition sharding

Partition-based sharding is a technique for distributing entity-scoped read load
to application servers. When dividing entities mechanically into N groups at
creation time, group together entities that are likely to be accessed together
and assign each group a partition. Then, at query time, direct entity-related
queries to application servers based on their partition. This can cause the
servers' peer caches to retain different subsets of the total index, increasing
the fraction of that index their combined memory can hold.

This is an application routing strategy, not automatic database sharding. Any
peer can still reach entities in any partition, and cross-partition reads can
overlap the servers' cached subsets. Partition assignment does not distribute
writes among independent writers or remove serialized transaction processing.
See [connections and peers](../07_peer_api/00_connections.md).

## New entity scans

Because EDB stores information about time, it is possible to ask questions that
apply only to recent transactions. For example, a batch job over everything that
changed today can use the captured [log](../06_indexes/00_indexes_and_log.md),
calling `log.tx_range(Some(TimePoint::Instant(midnight_millis)), None)`. Such a
scan considers all of the relevant transactions' datoms, filtering out any that
are irrelevant to the task at hand. Updates to an old entity are changes, too;
they do not establish that the entity was newly created.

Partitions can group the entities of an event category. A partition-oriented
scan starts at a fabricated entity boundary, walks EAVT, and stops when the
partition changes. For example, this scans current facts for implicit partition
500 using a database value named `db`:

```rust
let partition = implicit_part(500)?;
let boundary = IndexBoundary::Eavt(IndexComponents::One(partition));
for result in db.seek_cursor(&boundary)? {
    let datom = result?;
    if partition_eid(datom.entity)? != partition {
        break;
    }
    // Process this partition's current facts.
}
```

`DatabaseValue::entid_at(&partition, time_point)` also fabricates an EAVT boundary;
its partition argument is an `EntityIdentifier`. It puts the resolved transaction
`t` into the low entity-index field. An instant selects the first transaction at
or after that instant. **This is not a creation-time cutoff for native entity
allocations:** ordinary entity indexes advance from a separate issued frontier,
not directly from transaction `t`. An EAVT scan beginning at `entid_at` can
therefore include entities allocated before the requested transaction. Explicit
IDs and the independent reserved allocator also prevent treating general entity
ID order as a creation-time index.

For a time-based new-event scan, use the log with an application creation marker,
or store an indexed creation-time attribute and query its range. Do not infer
entity creation time from the numeric ID alone. A current-index scan also omits
facts that have since been retracted; use the log or history when those matter.

## Tempids and partitions

Use string tempids with the directives above, or native `EntityRef::Temp` and
`TxOp::ForcePartition` / `TxOp::MatchPartition`. Reusing a tempid string in one
transaction refers to the same entity; its name has no identity across
transactions. The native API does not provide JVM structural tempids with a
partition slot, and the EDN transaction reader does not accept `#db/id` tagged
tempids or negative numeric entity IDs. Keep partition policy in the supported
directives.

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
