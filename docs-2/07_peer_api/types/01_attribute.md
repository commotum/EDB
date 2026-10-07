# Attribute

`edb_core::Attribute` is the programmatic representation of a
[schema attribute](../../03_schema/02_schema_reference.md). Attribute information
resides in the captured database value's in-memory schema projection. Inspecting
this projection requires no query or storage read.

An `Attribute` exposes public Rust fields. `Cardinality`, `Unique`, `ValueType`,
`TupleSpec`, and `Schema` are also exported by `edb_core`. Attribute descriptors
are derived from schema datoms; changing a descriptor you own does not change an
existing database value. Install or alter application schema through
[transactions](../../03_schema/03_changing_schema.md).

## Field summary

| Field | Type | Description |
| --- | --- | --- |
| `id` | `u32` | The attribute's numeric identity. |
| `ident` | `Keyword` | The attribute's ident (programmatic name). |
| `value_type` | `ValueType` | The attribute's value type. |
| `cardinality` | `Cardinality` | The attribute's cardinality. |
| `unique` | `Option<Unique>` | Type of the attribute's uniqueness constraint, if any. |
| `indexed` | `bool` | Whether `:db/index` is true for this attribute. |
| `component` | `bool` | Whether this is a component attribute. |
| `no_history` | `bool` | Whether this is a no-history attribute. |
| `fulltext` | `bool` | Whether fulltext indexing is enabled. |
| `tuple` | `Option<TupleSpec>` | The tuple definition, if any. |
| `tuple_discontinued` | `bool` | Whether automatic composite tuple maintenance has been discontinued. |
| `predicates` | `Vec<String>` | Names of deterministic attribute value predicates. |

## Cardinality and uniqueness values

| Rust value | Schema value | Meaning |
| --- | --- | --- |
| `Cardinality::Many` | `:db.cardinality/many` | An entity may have multiple values for this attribute. |
| `Cardinality::One` | `:db.cardinality/one` | An entity may have at most one current value for this attribute. |
| `Some(Unique::Identity)` | `:db.unique/identity` | Values identify entities and support identity upsert. |
| `Some(Unique::Value)` | `:db.unique/value` | Values are unique across entities without identity upsert. |
| `None` | No `:db/unique` assertion | No uniqueness constraint. |

Unique attributes require cardinality one and cannot have the bytes value type.
See [identity and uniqueness](../../03_schema/05_identity_and_uniqueness.md).

## Value types

| Rust value | Schema value | Description |
| --- | --- | --- |
| `ValueType::BigDec` | `:db.type/bigdec` | Arbitrary precision decimal. |
| `ValueType::BigInt` | `:db.type/bigint` | Arbitrary precision integer. |
| `ValueType::Boolean` | `:db.type/boolean` | Boolean. |
| `ValueType::Bytes` | `:db.type/bytes` | Binary data. |
| `ValueType::Double` | `:db.type/double` | 64-bit floating point. |
| `ValueType::Float` | `:db.type/float` | 32-bit floating point. |
| `ValueType::Function` | `:db.type/fn` | Immutable stored-program content hash. |
| `ValueType::Instant` | `:db.type/instant` | Unix epoch milliseconds. |
| `ValueType::Keyword` | `:db.type/keyword` | Keyword. |
| `ValueType::Long` | `:db.type/long` | Signed 64-bit integer. |
| `ValueType::Ref` | `:db.type/ref` | Reference to another entity. |
| `ValueType::String` | `:db.type/string` | Unicode string. |
| `ValueType::Symbol` | `:db.type/symbol` | Symbol. |
| `ValueType::Tuple` | `:db.type/tuple` | Tuple of scalar values. |
| `ValueType::Uri` | `:db.type/uri` | URI with component-based identity. |
| `ValueType::Uuid` | `:db.type/uuid` | 128-bit UUID. |

`value_type` describes the stored value contract. It is immutable after
installation. For the corresponding `Value` variants and EDN representations,
see [value types](../../03_schema/02_schema_reference.md#dbvaluetype).

## Inspecting attributes

```rust,ignore
// DatabaseValue
fn schema(&self) -> &Schema;
fn has_avet(&self, attribute: &AttributeName) -> Result<bool, SemanticError>;

// Schema
fn attribute(&self, id: u32) -> Result<&Attribute, SemanticError>;
fn resolve_ident(&self, ident: &Keyword) -> Option<u32>;
fn attributes(&self) -> impl Iterator<Item = &Attribute>;
```

Capture a database value, then use its schema to resolve an attribute's ident
and inspect the descriptor. `resolve_ident` returns `None` for an unknown
attribute ident and recognizes retained aliases. `attribute` returns
`schema/unknown-attribute` for an unknown ID. `attributes` enumerates the
installed descriptors in numeric attribute-ID order.

This complete in-memory example installs an identity attribute and inspects the
resulting captured value:

```rust
use edb_core::{
    AttributeName, Cardinality, Database, Keyword, SemanticError, Unique, ValueType,
};

fn example() -> Result<(), SemanticError> {
    let initial = Database::bootstrap()?.database_value();
    let report = initial.with_edn(r#"[
      {:db/id "edb.tx" :db/source :db.source/system}
      {:db/ident :person/email
       :db/source :db.source/system
       :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one
       :db/unique :db.unique/identity}
    ]"#, 1_000)?;
    let db = report.db_after;
    let name = Keyword::new("person", "email");
    let id = db.schema().resolve_ident(&name).expect("installed attribute");
    let attribute = db.schema().attribute(id)?;

    assert_eq!(attribute.ident, name);
    assert_eq!(attribute.value_type, ValueType::String);
    assert_eq!(attribute.cardinality, Cardinality::One);
    assert_eq!(attribute.unique, Some(Unique::Identity));
    assert!(!attribute.indexed); // No explicit :db/index assertion.
    assert!(attribute.indexed || attribute.unique.is_some());
    assert!(db.has_avet(&AttributeName::Id(id))?);
    assert!(initial.schema().resolve_ident(&name).is_none());
    Ok(())
}
```

A durable application obtains the captured value from `Connection::db`.
Attribute inspection continues to describe that value even after the connection
advances. As-of, since, history, and filtered views retain the captured basis's
schema; an as-of view does not reconstruct an earlier attribute definition.

## Field details

### `id` and `ident`

The attribute's numeric identity and ident (programmatic name). Attribute IDs
are compact `u32` values; general entity IDs are `u64`. Use the exported
`schema_eid_to_attr_id` conversion to validate a schema entity ID before using
it as an attribute ID. The ident is a `Keyword`, such as `:person/email`.
Renaming an attribute changes its canonical ident while retaining its numeric
identity. See [identity and values](../../03_schema/01_identity_and_values.md).

### `cardinality`, `value_type`, and `unique`

The attribute's cardinality, value type, and type of uniqueness constraint, if
any. `cardinality` is either `Cardinality::Many` or `Cardinality::One`;
`unique` is `Some(Unique::Identity)`, `Some(Unique::Value)`, or `None`.
These are fields to read, not getter methods.

### `component`

Is this a [component](../../03_schema/02_schema_reference.md#dbiscomponent)
attribute? `true` means `:db/isComponent` is true for this attribute. Component
attributes must have `ValueType::Ref`. Component references participate in
recursive entity retraction and component navigation.

### `indexed` and `DatabaseValue::has_avet`

Is this attribute configured for an AVET index? The schema intent is:

```rust,ignore
attribute.indexed || attribute.unique.is_some()
```

`indexed` alone is the explicit `:db/index` flag. A unique attribute is configured
for AVET even if that field is false.

Does this attribute *currently* have a usable AVET index? Call
`db.has_avet(&AttributeName::Id(attribute.id))`, or supply
`AttributeName::Ident(attribute.ident.clone())`. When you
[alter an existing schema](../../03_schema/03_changing_schema.md), durable AVET
backfill can continue in the background after indexing is enabled. `has_avet`
reports physical readiness at this exact captured value. It returns `false`
for a known attribute without AVET or with pending backfill, and returns
`database/unknown-attribute` for an unknown ID or ident.

The check does not wait for an index build or refresh the immutable value.
After coordinating with `Connection::sync_schema`, inspect the returned value.
An eager in-memory value has AVET ready whenever schema enables it. See
[index readiness](../../06_indexes/04_index_apis.md#readiness-and-database-views)
and [synchronization](../../04_transactions/09_synchronization.md).

### `no_history`

Is this a [no-history](../../03_schema/02_schema_reference.md#dbnohistory)
attribute? `true` means `:db/noHistory` is true. This is a retention setting for
historical indexes, not proof that old information has already been removed.
Logs, captured values, receipts, and backups have separate retention.

### `fulltext`

Is fulltext indexing enabled for this attribute? `true` means `:db/fulltext` is
true. This requires `ValueType::String` and is immutable after installation.
It describes the analyzed search configuration, not a physical completion
watermark or the AVET setting. See
[fulltext search](../../05_query_and_pull/03_fulltext.md).

### `tuple`, `tuple_discontinued`, and `predicates`

`tuple` is `None` for non-tuple attributes. Tuple attributes use
`TupleSpec::Homogeneous(ValueType)`, `TupleSpec::Heterogeneous(Vec<ValueType>)`,
or `TupleSpec::Composite(Vec<u32>)`. Composite entries are constituent attribute
IDs. `tuple_discontinued` applies only to composites and stops their automatic
maintenance; discontinuation is irreversible. See
[tuples](../../03_schema/02_schema_reference.md#tuples).

`predicates` contains fully qualified predicate names such as `app/valid-email`.
The transaction execution context resolves their implementations through
registered native callbacks or stored predicate programs. See
[attribute predicates](../../03_schema/02_schema_reference.md#attribute-predicates)
and [native computation](../02_native_computation.md).

## Construction methods

```rust,ignore
impl Attribute {
    fn new(id: u32, ident: Keyword, value_type: ValueType, cardinality: Cardinality) -> Self;
    fn unique(self, unique: Unique) -> Self;
    fn component(self) -> Self;
    fn fulltext(self) -> Self;
    fn tuple(self, tuple: TupleSpec) -> Self;
    fn predicate(self, name: impl Into<String>) -> Self;
}
```

`new` creates a descriptor with the supplied identity, type, and cardinality.
`unique` and `tuple` default to `None`; all Boolean flags default to `false`;
`predicates` starts empty. Builder methods consume and return the descriptor.
`unique` and `tuple` set their corresponding fields; `component` and `fulltext`
set their flags to true. `predicate` appends a name, sorts the names, and removes
duplicates. These methods construct descriptions; they do not submit transactions
or establish that a description passes schema validation.

For an owned in-memory schema, `Schema::new()` creates an empty schema and
`Schema::install(attribute)` validates and installs a descriptor, returning
`Result<(), SemanticError>`. `Schema::rename(id, ident)` renames an installed
attribute and retains the old ident as an alias. Neither method writes to an
existing database or mutates the `&Schema` borrowed from a captured value.
See the [native API reference](../04_native_api_reference.md#attribute-information)
for the surrounding database API.
