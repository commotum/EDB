# Schema is database data

An attribute declares a value type and cardinality before application facts can
use it. Entities are open collections of facts, not rows constrained to one
table-shaped record type. Reference values connect entities; absence of a fact
is different from a stored null value.

Install this EDN as an ordinary transaction:

```edn
[{:db/id "edb.tx" :db/source :db.source/system}
 {:db/ident :person/name
  :db/source :db.source/system
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/ident :person/email
  :db/source :db.source/system
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}]
```

Each map explicitly attributes its facts to the ordinary built-in source entity
`:db.source/system`. `:db/source` is map metadata, not a stored attribute. The
metadata-only `edb.tx` map attributes the generated `:db/txInstant`; it does not
supply a default source for other maps.

Then submit application data in a later transaction:

```edn
[{:db/id "edb.tx" :db/source :db.source/system}
 {:db/source :db.source/system
  :person/name "Alice" :person/email "alice@example.com"}]
```

Use the complete [schema fixture](../../examples/edn/schema.edn) and
[EDN tutorial](../01_tutorials/00_edn_workflow.md) to run this through the CLI.
Typed Rust uses `Schema`, `Attribute`, `ValueType`, `Cardinality` and `Unique`;
the [native workflow](../../examples/native_workflow.rs) shows installation and
ordinary transactions through public APIs.

For the complete syntax, types, defaults and validation rules, use the
[schema reference](02_schema_reference.md). See [changing schema](03_changing_schema.md)
for modifying installed attributes and [data modeling](04_data_modeling.md) for
choosing attributes, identities and relationships.

## Identity and references

`:db.unique/identity` participates in tempid upsert; `:db.unique/value` rejects
conflicting identities without merging them. A lookup reference such as
`[:person/email "alice@example.com"]` resolves against a unique attribute.
An ident names an entity; renaming an ident is distinct from changing that
entity's identity. Keep application request identity separate from fact equality.
See [identity and uniqueness](05_identity_and_uniqueness.md) for entity IDs,
idents, domain keys, UUIDs and lookup references across transactions and reads.

Cardinality-one replacement retracts the old fact and asserts the new fact.
Both statements carry the replacement's source; the earlier assertion retains
its original source in history. Cardinality counts distinct values, so two
sources asserting the same value still satisfy cardinality one.
Omitting an attribute in a map does not retract it. A cardinality-many attribute
accepts a single value or a collection of values; wrap a many-reference lookup
in its outer collection.
Nested map admission, explicit child IDs and component behavior are described in
the [transaction tutorial](../01_tutorials/00_edn_workflow.md#transactions-and-exact-retries).

## Indexing, history and validation

`:db/index` requests AVET access; unique attributes also need indexed lookup.
Changing schema intent does not make background backfill immediately ready:
check `DatabaseValue::has_avet`, or synchronize schema/index work before requiring
the new access path. The supplied immutable value does not change in place.

`:db/noHistory` permits older facts to be omitted from retained indexes; it is not
an erasure mechanism for logs, receipts, backups or copied data. Excision is a
separate [controlled operation](../08_operations/00_deployment.md#excision).
`:db/isComponent` selects ownership-style navigation and transaction behavior;
it is not an authorization boundary. `:db/fulltext` is an immutable flag on string
attributes; see [native fulltext](../05_query_and_pull/03_fulltext.md).

Attribute resolution uses transaction db-before; schema changes also validate
the proposed schema and resulting facts. Not every flag or type can be changed
after installation; see the [schema change rules](03_changing_schema.md).
Use [native predicates](../07_peer_api/02_native_computation.md)
for explicitly deployed transaction checks, and the [identity/value guide](01_identity_and_values.md)
for managed composite tuples, URI equality, NaN and numeric/tuple admission limits.
