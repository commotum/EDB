# Entities

An EDB entity provides a lazy, associative view of all the information that can
be reached from an EDB entity ID.

The Rust `Entity` interface provides associative access to:

- All the attribute/value pairs associated with an entity ID E.
- All other entities reachable as values V from E.
- All other entities that can reach E through their values V.

Navigation to other entities is recursive, so with thoroughly connected data such
as a social network graph, it may be possible to navigate an entire dataset
starting with a single entity. The entity interface is generic and can navigate
any point-in-time EDB data without an application-specific entity class.

## Basics

The examples below refer to Jane Doe, her address in North Carolina, and John,
who likes Jane. At the datom level, these facts look like this (entity and
transaction IDs are shown symbolically, and attributes by their idents):

| E | A | V | Tx | S | Added |
| --- | --- | --- | --- | --- | --- |
| Jane | `:person/firstName` | `"Jane"` | T1 | `:db.source/system` | `true` |
| Jane | `:person/lastName` | `"Doe"` | T1 | `:db.source/system` | `true` |
| Jane | `:person/address` | Address | T1 | `:db.source/system` | `true` |
| Address | `:address/state` | `"NC"` | T1 | `:db.source/system` | `true` |
| John | `:person/firstName` | `"John"` | T2 | `:db.source/system` | `true` |
| John | `:person/likes` | Jane | T2 | `:db.source/system` | `true` |

Each row is a fact about the entity in E. The reference values in V connect
Jane to her address and John to Jane. Tx identifies the transaction that
asserted the fact; S identifies the source, shown here by its ident, and
`Added = true` marks an assertion. Reading Jane's attributes
gathers the rows with Jane in E. Following `:person/address` reaches the rows
about Address; navigating backward through `:person/likes` reaches John.

```mermaid
flowchart LR
    John["John"] -->|person/likes| Jane["Jane Doe"]
    Jane -->|person/address| Address["123 Main Street, NC"]
```

This setup creates those facts, together with a street and email address, in an
in-memory database. It groups all the data assertions into one transaction
instead of the two illustrated above. The database allocates entity IDs; the
examples retain Jane's ID as `jane_id`. Each map explicitly attributes its facts
to `:db.source/system`. The metadata-only `"edb.tx"` maps attribute the generated
transaction instants.

```rust
use edb_core::{
    AttributeName, Database, EntityIdentifier, EntityValue, Keyword,
    PullDirection, Value,
};

let installed = Database::bootstrap()?.with_edn(r#"[
  {:db/id "edb.tx" :db/source :db.source/system}
  {:db/source :db.source/system :db/ident :person/firstName :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
  {:db/source :db.source/system :db/ident :person/lastName :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
  {:db/source :db.source/system :db/ident :person/email :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
  {:db/source :db.source/system :db/ident :person/address :db/valueType :db.type/ref
   :db/cardinality :db.cardinality/one}
  {:db/source :db.source/system :db/ident :person/likes :db/valueType :db.type/ref
   :db/cardinality :db.cardinality/many}
  {:db/source :db.source/system :db/ident :address/street :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
  {:db/source :db.source/system :db/ident :address/state :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
]"#, 0)?.db_after;

let report = installed.with_edn(r#"[
  {:db/id "edb.tx" :db/source :db.source/system}
  {:db/id "address" :db/source :db.source/system
   :address/street "123 Main Street" :address/state "NC"}
  {:db/id "jane" :db/source :db.source/system
   :person/firstName "Jane" :person/lastName "Doe"
   :person/email "jane@example.com" :person/address "address"}
  {:db/id "john" :db/source :db.source/system
   :person/firstName "John" :person/likes ["jane"]}
]"#, 1)?;
let jane_id = report.tempids["jane"];
let john_id = report.tempids["john"];
let db = report.db_after.database_value();
let attr = |namespace: &str, name: &str|
    AttributeName::Ident(Keyword::new(namespace, name));
```

The `entity` method returns an optional entity, given a database value and entity
ID, ident, or lookup reference:

```rust
let jane = db.entity(jane_id)?.expect("numeric entity ID");
let by_email = db.entity(EntityIdentifier::Lookup {
    attribute: attr("person", "email"),
    value: Value::String("jane@example.com".into()),
})?.expect("Jane's email resolves");
assert_eq!(jane, by_email);
```

Given an entity, you can then navigate to any of that entity's attributes:

```rust
assert!(matches!(jane.get_named(&attr("person", "firstName"))?,
    Some(EntityValue::Scalar(Value::String(name))) if name == "Jane"));
```

The special attribute `:db/id` provides access to the entity's own ID. The Rust
`id()` method provides the same ID directly:

```rust
assert_eq!(jane.id(), jane_id);
assert!(matches!(jane.get_named(&attr("db", "id"))?,
    Some(EntityValue::Scalar(Value::Ref(id))) if id == jane_id));
```

If an attribute points to another entity through a cardinality-one attribute,
getting it returns another entity instance:

```rust
let Some(EntityValue::Entity(address)) =
    jane.get_named(&attr("person", "address"))? else {
        panic!("Jane has an address");
    };
assert!(matches!(address.get_named(&attr("address", "street"))?,
    Some(EntityValue::Scalar(Value::String(street))) if street == "123 Main Street"));
```

Forward references to entities with a `:db/ident` return
`EntityValue::Scalar(Value::Keyword(...))` instead. You can pass that keyword to
`db.entity(ident)` to obtain the identified entity. Reverse references always
return entity instances, including references from identified entities.

If an attribute points to other entities through a cardinality-many attribute,
getting it returns `EntityValue::Collection(Vec<EntityValue>)`, with distinct
values. It retains collection shape even when only one value is present. Missing
attributes return `None`, rather than an empty collection. The following example
asks for all the entities that Jane likes:

```rust
// None, given the example data: John likes Jane, but Jane has no likes.
assert!(jane.get_named(&attr("person", "likes"))?.is_none());
```

If you precede an attribute's local name with an underscore (`_`),
`PullDirection::SchemaResolved` navigates backward, returning the entities that
point to the current entity. The following example returns everyone who likes
Jane:

```rust
let incoming = jane.get_direction(&PullDirection::SchemaResolved(
    Keyword::new("person", "_likes"),
))?;
let Some(EntityValue::Collection(people)) = incoming else {
    panic!("John likes Jane");
};
assert_eq!(people.len(), 1);
assert!(matches!(&people[0], EntityValue::Entity(person) if person.id() == john_id));
```

An exact installed underscore-leading attribute takes precedence over this
shorthand. To request reverse navigation unambiguously, use
`PullDirection::Reverse(attr("person", "likes"))`. `get_named` always requests a
forward attribute; it does not interpret the underscore shorthand. Reversing a
non-reference attribute is an error.

Reverse noncomponent attributes return collections regardless of their forward
cardinality. A reverse component attribute returns a single parent entity.
`entity.reverse(attribute_id)` is a convenience that returns a `Vec<Entity>` in
either case, or an empty vector when there are no incoming references.

Entities do not *exist* in any particular place; they are merely an associative
view of all the datoms about some E at a point in time. For a valid numeric ID
with no available facts, `db.entity(id)` returns an entity whose only key is
`:db/id`. An unresolved ident or lookup reference instead returns `None`.
Unknown keyword attributes behave like missing keys; invalid numeric attribute
IDs produce errors.

## Laziness and Caching

Entity attributes are accessed lazily as you request them. Once you access a
particular attribute direction, the result, including absence, is cached for the
lifetime of that entity instance. Cloning an entity shares its cache; constructing
another entity for the same ID creates a separate cache. Explicit numeric and
keyword selectors are separate cache keys.

`entity.keys()` returns the available forward keys, including `:db/id`, without
caching their values. It does not enumerate reverse attributes.

The `entity.touch()` method is a convenience that accesses and caches all the
ordinary attributes of an entity. In addition, touch accesses and caches the
attributes of other entities reached as entities through an attribute whose
schema definition includes `:db/isComponent true`. Forward identified references
remain keyword values, so touch does not descend through them.

As an example, consider a database of orders, line items, and products. A line
item is a component of an order. A product is related to an order but is not a
component of the order. In such a database, touching an order will access the
order's attributes, and recursively access all the order's line items. Touching
an order will not access the line items' products' attributes.

```rust
let touched = jane.touch()?;
assert_eq!(touched, jane);
```

`touch()` returns a clone sharing the populated cache. It follows forward
component edges, stops when a component path revisits an entity, and does not
traverse reverse edges. It is a convenience, suitable for interactive
exploration, or for when you know that you need all of an entity's information.
When this is not the case, it can be more efficient to be explicit and ask for
precisely what you need. [Pull](../05_query_and_pull/06_pull_reference.md) provides
a declarative way to select that information.

Entities are lazy values backed by a database value in the application process.
The entity API does not refresh the connection or ask the transactor to execute
a query. On a storage-backed value, a cache miss can still fetch immutable index
nodes from storage. Local entity caching is not a guarantee of zero I/O or a fixed
number of storage reads; the [Pull and entity navigation guide](../05_query_and_pull/02_pull_and_entities.md)
describes node loading and cache behavior.

## Entities and Time

Entities know the database value on which they are based. Given an entity, you
can access that database and its basis transaction:

```rust
let entity_db = jane.database();
assert_eq!(entity_db.basis_t(), db.basis_t());
```

From the database you can then inspect its datoms and the transactions that
contributed to the entity. `basis_t()` remains the basis of the underlying
captured database, even for a narrower time view; `as_of_t()` and `since_t()`
report that view's boundaries.

Entities represent only a point in time, not the database across time. Notice
that the *across time* aspect of the database vanishes when making entities.
An entity is an associative view of the facts visible in its captured value,
not an attribute history. It projects distinct values, so contributions of the
same value from different sources do not change scalar or collection shapes.

| Datom | Entity |
| --- | --- |
| E | special `:db/id` key |
| A | keys |
| V | values |
| Tx | n/a |
| S | n/a |
| Added | n/a |

The transaction, source, and added/retracted fields of a datom do not become
fields of the entity view. To inspect them, use the captured database's datoms or
history.

The point-in-time nature of entities means that entities can be derived from
point-in-time database views: a captured database, `as_of(t)`, `since(t)`, a
filtered value, or the speculative `db_after` returned by `with`/`with_edn`.
A `since` entity includes only facts visible through that filter; it need not
contain every attribute the entity has in the unfiltered database. See
[database filters](03_database_filters.md).

Entities cannot be derived from a time-spanning `history()` value. Attempting to
do so returns `database/history-not-point-in-time`; use queries or index access
to inspect history.

## Usage Considerations

Entities are consistent with their captured database value.

Entities are lazy, logically immutable values. They are thread-safe and do not
require application coordination with other concurrent uses of the same
information. Their internal caches may fill, but their database facts do not
change.

Entities are based on a point-in-time view of the database. Navigation from an
entity will always and only reach other entities with that same time basis and
filters, allowing extended calculations to be performed without coordinating
with concurrent transactions. Storage-backed values retain the normal
[captured-value retention](00_database_values.md#captured-value-retention)
requirements for cold reads.

Entity equality and hashing use database origin and entity ID, not attribute
state or time basis. Two entities from different time views of the same origin
can compare equal even when their attributes differ. Use `entity.identity()` for
a lightweight identity key; compare selected Pull/query results when comparing
state. See [entity identity and state equality](00_database_values.md#entity-identity-is-not-state-equality).

Entities are not a mapping layer between databases and application code. They
are a direct, mechanical translation from database information to associative
application access.

Numeric entity handles are not suitable for existence tests, which should
typically be performed via lookup of a
[unique identity](../03_schema/05_identity_and_uniqueness.md). Resolving a numeric
ID validates its representation; it does not establish that facts about that
entity exist.
