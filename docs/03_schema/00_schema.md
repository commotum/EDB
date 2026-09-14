# Schema is database data

An attribute declares a value type and cardinality before application facts can
use it. Entities are open collections of facts, not rows constrained to one
table-shaped record type. Reference values connect entities; absence of a fact
is different from a stored null value.

Install this EDN as an ordinary transaction:

```edn
[{:db/ident :person/name
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/ident :person/email
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}]
```

Then submit application data in a later transaction:

```edn
[{:person/name "Alice" :person/email "alice@example.com"}]
```

Use the complete [schema fixture](../../examples/edn/schema.edn) and
[EDN tutorial](../01_tutorials/00_edn_workflow.md) to run this through the CLI.
Typed Rust uses `Schema`, `Attribute`, `ValueType`, `Cardinality` and `Unique`;
the [native workflow](../../examples/native_workflow.rs) shows installation and
ordinary transactions through public APIs.

## Identity and references

`:db.unique/identity` participates in tempid upsert; `:db.unique/value` rejects
conflicting identities without merging them. A lookup reference such as
`[:person/email "alice@example.com"]` resolves against a unique attribute.
An ident names an entity; renaming an ident is distinct from changing that
entity's identity. Keep application request identity separate from fact equality.

Cardinality-one replacement retracts the old fact and asserts the new fact.
Omitting an attribute in a map does not retract it. Cardinality-many input is a
collection; wrap a many-reference lookup in its outer collection.
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

Schema changes are validated against transaction db-before. Do not assume every
flag/type can be toggled after installation. Use [native predicates](../07_peer_api/02_native_computation.md)
for explicitly deployed transaction checks, and the [identity/value guide](01_identity_and_values.md)
for managed composite tuples, URI equality, NaN and numeric/tuple admission limits.
