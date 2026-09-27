# Identity and Uniqueness

EDB provides a number of ways to model identity and uniqueness:

- All entities have database-unique *entity ids*.
- *Idents* can be used for programmatic names.
- *Unique identities* allow transactions to work with domain keys instead of entity ids.
- *Unique values* enforce a single holder of an identifying key.
- *Squuids* provide efficient, globally unique identifiers.
- *Lookup refs* represent a lookup on a unique attribute as an attribute, value pair.

Each of these is described below. The [schema reference](02_schema_reference.md)
defines the schema properties; [value and identity rules](01_identity_and_values.md)
cover URI comparison, composite upsert hints, NaN and value limits.

## Entity Identifiers

An *entity identifier* is any one of the three ways that EDB can uniquely
identify an entity: an entity id, ident, or lookup ref. Read APIs such as
`DatabaseValue::entity` and `DatabaseValue::pull` accept `EntityIdentifier`, whose
variants are `Id`, `Ident` and `Lookup`. APIs taking a numeric attribute or entity
id require explicit resolution first.

## Entities

Every datom in EDB includes a database-unique entity id, often abbreviated as
simply *e* in documentation and API names. Entity ids are assigned during
transaction assessment, and never change.

You can request new entity ids by specifying a temporary id (tempid) in
transaction data. EDN tempids are strings; typed Rust uses `EntityRef::Temp`.
For example, after installing `:person/email`, this transaction requests an
entity identified within the transaction as `"joe"`:

```edn
[{:db/id "joe" :person/email "joe@example.com"}]
```

The transaction report's `tempids` map gives the actual id assigned to each
named tempid. Reusing a tempid string in a later transaction does not identify
the same entity. Use the assigned entity id or a unique domain key. An anonymous
entity map can also request an entity without naming a tempid.

Internally, entity ids encode the partition an entity belongs to. An entity's
partition may be useful in some cases, and can be discovered by calling
`partition_eid(entity)`. `eid_to_part(entity)` returns the raw partition bits.
Constructing an integer with `make_eid` does not issue or reserve an entity id.

Transactions and partitions are discussed fully in
[transactions](../04_transactions/00_transactions.md) and
[partitions](../04_transactions/02_partitions.md). Speculative transactions
return resolved tempids too, but reserve no identities in a live database.

Entity ids are unique within one database, not across unrelated databases.
Rust entity handles compare by entity id and database origin, independent of
which time view or lazy cache they hold. `Entity::identity()` supplies a token
with those same identity semantics; include a separate snapshot key when an
application cache must distinguish different times.

## Idents

Idents associate a programmatic name (a keyword) with an entity id, by setting a
value for the `:db/ident` attribute:

```edn
;; Attribute definition elided for brevity.
{:db/ident :person/loves}
```

When an entity has an ident, you can use that ident in place of the numeric
identifier. For example, assuming `:person/loves` is a reference attribute with
id 1007 and `:pizza` names entity 1124, this datom pattern:

```edn
[42 :person/loves :pizza]
```

is equivalent to:

```edn
[42 1007 1124]
```

Idents should be used for two purposes: to name schema entities and to implement
enumerated tags. To support these usages, idents have two special characteristics:

- Idents are designed to be fast and available from the resident ident dictionary
  associated with a captured database value.
- When you navigate the entity API forward through a reference that has an ident,
  the lookup returns the ident as `EntityValue::Scalar(Value::Keyword(...))`,
  not another entity. Reverse reference navigation returns entities.

These characteristics also imply situations where idents should *not* be used:

- Idents should not be used as unique names or ids on ordinary domain entities.
  Such entity names should be implemented with a domain-specific attribute that
  is a unique identity.
- Idents should not be used as names for test data. Your real data will not have
  such names, and you don't want test data to behave differently than the real
  data it simulates.

Idents can be used instead of entity ids in the following API positions:

- As the identifier argument to `entity` and `pull`.
- In the e and a positions, and reference-valued v positions, of assertions and
  retractions in EDN transactions and speculative `with_edn` transactions.
- In the e and a positions, and reference-valued v positions, of database
  patterns in a query. A keyword on a keyword-valued attribute remains a keyword.

There are some situations where EDB cannot know that a keyword is an ident.
For example, EDB does not know the semantics of database functions written by
you. If you need to convert between idents and entity ids in your own code,
use `DatabaseValue::ident(entity)` and `DatabaseValue::entid(&keyword)`.
These return `Option` values. To resolve any supported read identifier, use
`resolve_entity_identifier`.

Renaming an ident does not change the entity id. Previously assigned names remain
available as aliases; see [changing schema](03_changing_schema.md) for renaming
and reusing idents. Use namespace-qualified names for application schema and
enumerations; `db` and `db.*` namespaces are reserved.

## Unique Identities

Unique identity is specified through an attribute with `:db/unique` set to
`:db.unique/identity`. For example, this schema asserts that an `:inv/sku` is
unique:

```edn
[{:db/ident :inv/sku
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}]
```

Unique identity is appropriate whenever you want to assert a database-wide unique
identifier for an entity. Common examples include email, account name, product
code and UUIDs (but see Squuids, below). Unique identities have the following
properties:

- A unique identity attribute is indexed by value, in order to support uniqueness
  checks. Specifying `:db/index true` on a new unique attribute is redundant.
- Uniqueness checks are per-attribute, and do not prevent you from using the same
  value with a different attribute elsewhere.
- If a transaction specifies a unique identity for a temporary id, and that
  unique identity already exists in the database, then that temporary id resolves
  to the *existing* entity in the system. This *upsert* behavior makes it possible
  for transactions to work with domain identities, without ever having to specify
  EDB entity ids.

It is legal for a single entity to have multiple different unique attributes,
e.g. you might decide that people have both unique emails and unique government
identifiers. However, this creates the possibility of conflict, which causes a
transaction to fail with `transaction/upsert-conflict`. Conflict occurs if a
transaction tries to upsert a tempid into two *different* existing entities. For
example, if entity 42 has the unique email `"johndoe@example.com"`, and entity 43
has the unique account number 1007, then a transaction cannot claim that a new
tempid is both John Doe and account 1007.

Uniqueness can be declared on reference attributes (`:db.type/ref`) as well as
scalar and tuple attributes. Bytes attributes cannot be unique. Only
`:db.cardinality/one` attributes can be unique. NaN cannot be used as a unique
key; see [value and identity rules](01_identity_and_values.md).

EDB provides a mechanism to declare composite uniqueness constraints via
[composite tuples](02_schema_reference.md#composite-tuples). A fresh tempid that
asserts only composite constituents does not automatically upsert to an existing
entity. Supply an explicit composite identity hint, a lookup ref or the entity id;
see [composite identity](01_identity_and_values.md#composite-identity-derive-values-supply-identity-when-upserting).

Adding uniqueness to an existing attribute requires current values to be unique.
If the attribute has historical values, its AVET index must already be physically
ready. Set `:db/index true`, complete index synchronization, and inspect a fresh
value with `DatabaseValue::has_avet` before adding uniqueness. Schema flags alone
do not prove readiness. Adding uniqueness does not rewrite historical databases;
see [changing schema](03_changing_schema.md).

## Unique Values

Unique value is specified through an attribute with `:db/unique` set to
`:db.unique/value`. A unique value represents a database-wide value that can have
only one current holder for that attribute.

Unique values have the same uniqueness constraint as unique identities, with one
critical difference: attempts to assert a new tempid with a unique value already
held by another entity cause `transaction/unique-value-conflict`, rather than resolving
the tempid to that entity. An assertion against the existing entity is allowed;
an exact reassertion is redundant. Uniqueness applies to the resulting current
facts, not to every assertion ever retained in history.

## Squuids

It is often important to have a globally unique identifier for an entity. Where
such identifiers do not already exist in the domain, you can use a unique
identity attribute with a value type of `:db.type/uuid`.

In systems that maintain a value-sorted index, checking for the existence or
uniqueness of random (v4) UUIDs has poor locality, as reads scatter across the
UUIDs. To address this, EDB includes a semi-sequential UUID generator, `squuid`.
Squuids are valid UUIDs, but unlike purely random UUIDs, they include a leading
time component, which helps align read patterns with recency. Applications may
also choose to use `uuid_v7` for the same rationale.

You can retrieve the time component of a squuid with `squuid_time_millis`:

```rust
let id = edb_core::squuid()?;
let created_millis = edb_core::squuid_time_millis(id);
let value = edb_core::Value::Uuid(id);
```

`squuid()` returns `Result<u128, SemanticError>`. The leading 32 bits contain
Unix seconds; `squuid_time_millis` expresses that second boundary in milliseconds.
It does not establish that an arbitrary UUID was generated as a squuid.
`uuid_v7()` uses a millisecond timestamp, and `uuid_v7_time_millis` checks the
version and variant before decoding. The `_at` generator variants accept an
explicit Unix-millisecond clock.

These generators use operating-system randomness and do not guarantee strict
ordering within the same clock tick or after clock regression. Keep generated
UUIDs with the original transaction intent when retrying; generating another UUID
changes that intent.

If the ability to discover the time that an identifier was created leaks
sensitive information, then time-bearing UUIDs may not be appropriate. Otherwise,
consider Squuids or v7 UUIDs when your ids may also be indexed in other systems.
See [partitions and time-local UUIDs](../04_transactions/02_partitions.md) for the
native generation and retry rules.

## Lookup Refs

In many databases, entities have unique identifiers from the problem domain like
an email address or an order number. Applications often need to find entities
based on these external keys. You can do this with query, but it's easier to use
a *lookup ref*. A lookup ref is a two-element EDN vector containing an attribute
and a value. It identifies the entity with the given unique attribute value.
For example, this lookup ref:

```edn
[:person/email "joe@example.com"]
```

identifies the entity with the `:person/email` value `"joe@example.com"`.

Read-side Rust uses `EntityIdentifier::Lookup { attribute, value }`. You can use
lookup refs to retrieve entities with `entity` or `pull`. For raw indexes, resolve
an `EntityIdentifier` through the `eavt_boundary`, `aevt_boundary`, `avet_boundary`
or `vaet_boundary` helpers in the corresponding entity position before opening
an index cursor. Ref-valued index positions accept `RawIndexValue::Entity`.

You can also use lookup refs to refer to existing entities in transactions,
avoiding extra lookup code:

```edn
[{:db/id [:person/email "joe@example.com"]
  :person/loves :pizza}]
```

This transaction asserts that the entity with the value `"joe@example.com"` for
the `:person/email` attribute also loves pizza. It assumes `:person/loves` is an
installed reference attribute and `:pizza` is an existing ident.

Lookup refs have the following restrictions and resolution rules:

- The specified attribute must be defined as either `:db.unique/value` or
  `:db.unique/identity`.
- When used in a transaction, the lookup ref is evaluated against the database
  as it exists before the transaction is processed, so you cannot use a lookup
  ref to look up an entity being defined in the same transaction. Use a shared
  tempid for transaction-local references instead.
- Transactions accept lookup refs in entity positions and reference-valued
  positions. A cardinality-many reference in a map needs an outer collection,
  e.g. `:person/friends [[:person/email "joe@example.com"]]`.
- Database query patterns resolve a lookup ref in the **entity position**, either
  as a literal or through a parameterized input. Attribute positions require an
  attribute id or ident. Query reference-value positions do not resolve lookup
  refs; use a separate clause to bind the referenced entity id.

For example, this query resolves the email lookup in the entity position:

```edn
[:find ?loves
 :where [[:person/email "joe@example.com"] :person/loves ?loves]]
```

A parameterized form accepts the same lookup ref as the `?person` input:

```edn
[:find ?loves
 :in $ ?person
 :where [?person :person/loves ?loves]]
```

Lookup refs used in a transaction resolve during transaction assessment against
db-before. Lookup refs used in reads resolve using the database value provided;
an unresolved read lookup returns no entity or no matching query rows. Raw index
boundary construction requires a resolvable identity and reports an error when
it cannot resolve one. Ordinary unique lookups require ready AVET; `:db/ident`
uses the resident ident dictionary.

## Joins Should Use Entity IDs

Entity ids are native to EDB, have a compact numeric representation, and are
stored as part of every datom.

Queries against a single database can look up entity ids via other kinds of
identifiers, but for efficiency should join by entity id:

```edn
[:find ?person
 :where [?joe :person/email "joe@example.com"]
        [?joe :person/loves ?food]
        [?person :person/loves ?food]]
```

The shared `?food` variable joins reference values by entity id. This also avoids
repeating domain-key lookups at every relationship in a query.
