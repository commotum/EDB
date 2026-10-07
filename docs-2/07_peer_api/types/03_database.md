# DatabaseValue and Database

An immutable, point-in-time database value.

`edb_core::DatabaseValue` is the captured value used for queries, entity
navigation, Pull, temporal views, and index reads. `Connection::db()` captures
one; `Database::bootstrap()?.database_value()` supplies an in-memory value.
`Database` is the eager in-memory representation and supplies additional
slice-returning index methods. The signatures below belong to `DatabaseValue`
unless another owner is specified. Reference types are exported by `edb_core`;
the EDN Pull parser is in `edb_core::edn_pull`.

Deriving a view does not change the original value or refresh its basis.
Methods taking `self` consume the handle; clone it first to retain the original.
Fallible operations return `SemanticError`; absence is represented by `Option`.
See [database values](../../02_core_concepts/00_database_values.md) and the
[native API reference](../04_native_api_reference.md).

## Predicate type

Boolean-valued function for [filtering](#filter) a database:

```rust,ignore
Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static
```

The predicate receives the unfiltered database value and a datom. This is a Rust
closure bound, rather than a nested interface or an untyped function overload.
See [Database Filter Predicate](04_database_predicate.md) for the callback's
temporal context and evaluation rules.

## Index names

Pass an `IndexOrder` variant to APIs that take an index name, such as
[`datoms`](#datoms). Prefixes and boundaries name their index with the
corresponding `IndexPrefix` or `IndexBoundary` variant.

| Variant | Index | Description |
| --- | --- | --- |
| `IndexOrder::Eavt` | EAVT | Entity, attribute, value, transaction. |
| `IndexOrder::Aevt` | AEVT | Attribute, entity, value, transaction. |
| `IndexOrder::Avet` | AVET | Attribute, value, entity, transaction. |
| `IndexOrder::Vaet` | VAET | Value, attribute, entity, transaction; reverse references. |

All four retain the source entity ID after the transaction coordinate.
EAVT and AEVT contain all datoms visible in the selected view. AVET contains
datoms for attributes with indexing or uniqueness enabled and an available
projection. VAET contains datoms for reference attributes. See the
[index model](../../06_indexes/01_index_model.md).

## Method summary

| Operation | Method or equivalent | Result |
| --- | --- | --- |
| Database identity | `snapshot_key`, `SnapshotKey::lineage_id` | Authenticated identity for supported committed values. |
| Basis | `basis_t` | Most recent transaction coordinate of the underlying value. |
| Next transaction coordinate | No public `next_t` method | A speculative successor advances its basis by one. |
| As-of boundary | `as_of_t` | `Option<u64>`. |
| Since boundary | `since_t` | `Option<u64>`. |
| History flag | `is_history` | Whether this is a history value. |
| Local transaction | `with`, `with_forms`, `with_edn` | `Result<SpeculativeTransactionReport, SemanticError>`. |
| As-of view | `as_of`, `as_of_time_point` | Data up to the boundary, inclusive. |
| Since view | `since`, `since_time_point` | Data since the boundary, exclusive. |
| History view | `history` | Retained assertions and retractions across time. |
| Predicate filter | `filter` | Datoms satisfying the predicate. |
| Filter flag | `is_filtered` | Whether custom predicates are set. |
| Entity | `entity` | `Result<Option<Entity>, SemanticError>`. |
| Attribute information | `schema`, `Schema::attribute` | Captured schema and attribute definition. |
| Symbolic identity | `ident` | `Option<&Keyword>`. |
| Entity identifier | `entid`, `resolve_entity_identifier` | Resolve an ident, or an ID/ident/lookup reference. |
| Fabricated entity boundary | `entid_at` | `Result<u64, SemanticError>`. |
| Stored function | `invoke` | `Result<ProgramOutput, SemanticError>`. |
| Matching datoms | `datoms`, `datoms_with_prefix`, `scan_cursor`, `prefix_cursor` | Collected datoms or a fallible cursor. |
| Forward seek | `seek_cursor` | Datoms at or after a boundary. |
| Reverse seek | `reverse_seek_cursor` | Datoms at or before a boundary, in reverse order. |
| AVET range | `Database::avet_range`; bounded seek on `DatabaseValue` | Start inclusive, end exclusive. |
| Pull | `pull` | `Result<QueryValue, SemanticError>`. |
| Index Pull | `index_pull` | `Result<IndexPullCursor<'_>, SemanticError>`. |
| Pull many | `pull_many` | `Result<Vec<QueryValue>, SemanticError>`. |
| Statistics | `db_stats` | `Result<DatabaseStats, SemanticError>`. |

## Method details

### Database identity

```rust,ignore
// DatabaseValue
fn snapshot_key(&self) -> Result<SnapshotKey, SemanticError>;
// SnapshotKey
fn lineage_id(&self) -> &str;
```

`DatabaseValue` has no `id()` string method. For a supported committed value,
`snapshot_key()?.lineage_id()` identifies the authenticated database lineage.
The full key also distinguishes committed coordinates and time views. Eager
fixtures, speculative values, and custom predicate filters have no portable
committed key and return an unsupported error. A catalog name is a routing name,
not this identity. For entity equality, see
[entity identity](../../02_core_concepts/00_database_values.md#entity-identity-is-not-state-equality).

### basis_t

```rust,ignore
fn basis_t(&self) -> u64;
```

Returns the transaction coordinate of the most recent transaction in the
underlying immutable database value. An as-of or since view does not lower this
basis. Use the boundary accessors to inspect the view's selected window.

### Next transaction coordinate

There is no public `next_t()` method. A successful speculative transaction uses
the input basis plus one, subject to coordinate validation. Read the returned
`db_after.basis_t()` for its coordinate. This neither reserves the next durable
transaction number nor predicts the result of a later concurrent submission.
Transaction coordinates and entity-allocation frontiers are distinct.

### as_of_t

```rust,ignore
fn as_of_t(&self) -> Option<u64>;
```

Returns an as-of transaction coordinate, or `None` if none is set.

### since_t

```rust,ignore
fn since_t(&self) -> Option<u64>;
```

Returns a since transaction coordinate, or `None` if none is set.

### is_history

```rust,ignore
fn is_history(&self) -> bool;
```

True for databases created with [`history`](#history).

### with

```rust,ignore
fn with(&self, ops: &[TxOp], tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_forms(&self, forms: &[TxForm], tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_edn(&self, text: &str, tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
```

Returns a database with transaction data applied locally in memory. It is as if
the data were applied in a transaction, but no durable transaction takes place.
The input is primitive operations, transaction forms, or EDN transaction data,
respectively. Supply the transaction instant explicitly as Unix epoch
milliseconds.

Each assertion and retraction explicitly supplies its source through the
transaction form. A metadata-only map
`{:db/id "edb.tx" :db/source S}` attributes the generated `:db/txInstant`;
primitive operations use an explicit sourced `TxOp::Add` for that fact instead.
This declaration does not supply a default source for other forms.

The report contains `db_before`, `db_after`, `tx_data`, and `tempids`. No
connection advances and no entity IDs are reserved. IDs allocated on a
speculative branch are not a promise of IDs allocated by a later submission.
`Database::with` provides the eager counterpart and returns `TxReport`.

History values cannot transact. Read filters, including as-of and since, apply
to the returned view; transaction generation and invariant checking use the
full captured basis. An as-of view therefore does not branch the past.
Callback, allocation-default, and resource-limit variants are described in
[native computation](../02_native_computation.md) and
[speculative transactions](../04_native_api_reference.md#speculative-transactions-databasevaluewith).

### as_of

```rust,ignore
fn as_of(self, t: u64) -> DatabaseValue;
fn as_of_time_point(self, time_point: TimePoint)
    -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database filtered to include data up to `t`, inclusive.
The typed time-point form accepts `TimePoint::T`, `TimePoint::Tx`, or
`TimePoint::Instant` (Unix epoch milliseconds). Repeated calls replace the
previous upper boundary.

The basis, schema, and ident dictionary remain those of the captured value.
An instant exactly matching several transactions selects the earliest matching
transaction. Use a transaction coordinate or transaction entity ID when an exact
boundary matters. See [database filters](../../02_core_concepts/03_database_filters.md).

### since

```rust,ignore
fn since(self, t: u64) -> DatabaseValue;
fn since_time_point(self, time_point: TimePoint)
    -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database filtered to include only data since `t`,
exclusive. Time-point forms have the same meanings as for `as_of`. Repeated
calls replace the previous lower boundary. Since the resolved boundary is
exclusive, an instant shared by multiple transactions can leave later
transactions from that same millisecond visible.

### history

```rust,ignore
fn history(self) -> DatabaseValue;
```

Returns a history database value containing retained assertions and retractions
across time. A history value can be used for raw index access, queries, as-of,
and since. Existing temporal and custom predicate bounds remain in force.
Use a bounded seek for a history AVET range.

A history database cannot be used with APIs that require a single point in
time: entity navigation, Pull (including index Pull), or speculative
transactions. Queries can return additions and retractions, distinguished by
the sixth datom position (`?added`); the fifth is the source entity ID.
Raw reads expose `Datom.source` and `Datom.added`. Each retained assertion and
retraction keeps its own attribution.
No-history retention and excision can remove historical datoms. The
[transaction log](../../06_indexes/00_indexes_and_log.md) is a separate API.

### filter

```rust,ignore
fn filter<F>(self, predicate: F) -> DatabaseValue
where
    F: Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static;
```

Returns a value of the database containing only datoms satisfying the predicate.
The predicate is passed the same temporal/history value with custom predicates
removed, and a datom. Chained calls compose predicates with logical AND.
Predicates run as data is read; deriving the filter does not eagerly scan it.
Keep predicates pure. There is one typed Rust closure API, with no untyped
object overload. See [database filters](../../02_core_concepts/03_database_filters.md).

### is_filtered

```rust,ignore
fn is_filtered(&self) -> bool;
```

Does the database have a custom predicate set with `filter`? An as-of or since
boundary alone does not make this return `true`.

### entity

```rust,ignore
fn entity(&self, identifier: impl Into<EntityIdentifier>)
    -> Result<Option<Entity>, SemanticError>;
```

Returns an entity: a lazy associative view of datoms sharing an entity ID.
The identifier may be an ID, ident, or lookup reference. An unresolved ident or
lookup reference returns `None`; a valid numeric ID can produce an entity view
with no facts. Reference navigation retains the same captured database value.
History values are rejected. See the [Entity type reference](06_entity.md) and
[entities guide](../../02_core_concepts/04_entities.md).

### Attribute information

```rust,ignore
// DatabaseValue
fn schema(&self) -> &Schema;
fn has_avet(&self, attribute: &AttributeName) -> Result<bool, SemanticError>;
// Schema
fn attribute(&self, id: u32) -> Result<&Attribute, SemanticError>;
fn resolve_ident(&self, ident: &Keyword) -> Option<u32>;
```

Returns information about an attribute through the captured schema. Resolve an
attribute ident with `Schema::resolve_ident`, then call `Schema::attribute`.
Attribute IDs are `u32`, distinct from general `u64` entity IDs. `Attribute`
exposes identity, type, cardinality, uniqueness, indexing, retention, component,
fulltext, tuple, and predicate settings. See the
[attribute reference](01_attribute.md).

`has_avet` reports physical availability at this captured value. Schema indexing
or uniqueness flags alone do not establish readiness while durable backfill is
pending. A known unavailable attribute returns `false`; an unknown attribute
returns an error. The check does not wait or refresh the value.

### ident

```rust,ignore
fn ident(&self, entity: u64) -> Option<&Keyword>;
```

Returns the symbolic keyword associated with an ID, or `None` if not found.
The method takes a numeric ID; a keyword already needs no conversion. It reads
the captured ident dictionary, which is not windowed by temporal or custom
datom filters.

### entid

```rust,ignore
fn entid(&self, ident: &Keyword) -> Option<u64>;
fn resolve_entity_identifier(&self, identifier: &EntityIdentifier)
    -> Result<Option<u64>, SemanticError>;
```

`entid` returns the entity ID associated with a symbolic keyword, or `None` if
not found. To accept any supported entity identifier, use
`resolve_entity_identifier`: numeric ID, ident, or lookup reference. Numeric
IDs are validated without requiring facts to exist. Ident resolution uses the
captured dictionary; lookup references resolve through the selected database
view and its unique attribute. See [identity](../../03_schema/01_identity_and_values.md).

### entid_at

```rust,ignore
fn entid_at(&self, partition: &EntityIdentifier, time_point: TimePoint)
    -> Result<u64, SemanticError>;
```

Returns a fabricated entity ID in the supplied named or implicit partition,
placing the resolved transaction coordinate into its entity-index component.
For an instant, the boundary uses the first transaction at or after that instant.

Entity IDs sort by partition, then entity-index component. Ordinary entity
allocation uses an independent frontier, so this fabricated ID is **not a
creation-time cutoff**. A seek starting there can include entities created
before the requested time. Use the log or an explicit indexed creation-time
attribute for new-entity scans. See
[partitions](../../04_transactions/02_partitions.md#new-entity-scans).

### invoke

```rust,ignore
fn invoke(
    &self,
    function: impl Into<EntityIdentifier>,
    arguments: &[RuntimeValue],
    control: InvokeControl<'_>,
) -> Result<ProgramOutput, SemanticError>;
```

Looks up the database function bound through `:db/fn` on the specified entity
and invokes its stored program with the arguments. The binding resolves inside
this exact value. The output retains the program's role: query rows, predicate
value, or transaction forms. Dual predicates require an explicit `InvokeRole`.

Returned transaction forms are inert data; invocation does not transact them
or recursively expand nested transaction calls. An eager value lacking retained
program content cannot fetch it from an ambient connection. The control supplies
execution and read limits. See
[persisted programs](../../04_transactions/01_persisted_programs.md).

### datoms

```rust,ignore
fn datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError>;
fn scan_cursor(&self, order: IndexOrder)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
fn datoms_with_prefix(&self, prefix: &IndexPrefix)
    -> Result<Vec<Datom>, SemanticError>;
fn prefix_cursor(&self, prefix: &IndexPrefix)
    -> Result<DatabaseValuePrefixCursor<'_>, SemanticError>;
```

Implements raw access to matching index data. Supply an index order for a whole
index, or leading components in the index's order to narrow the result. Prefixes
restrict all results to the specified components and support E/A/V components
in the chosen order; use a boundary for a transaction component.

Collecting methods return vectors. Cursors yield
`Result<Datom, SemanticError>` incrementally: both construction and advancement
can fail, and an error terminates the cursor. Handle each result. A [Datom](05_datom.md) has
`entity: u64`, `attribute: u32`, `value: Value`, `tx: u64` (a transaction entity
ID), `source: u64` (a source entity ID), and `added: bool`.

AVET availability depends on the captured value. Unrestricted AVET scans omit
unavailable attributes. Attribute-qualified seeks reject unavailable projections;
eager and durable prefix APIs have different validation/error behavior.
Check `has_avet` when relying on a projection, and see
[index readiness](../../06_indexes/04_index_apis.md#readiness-and-database-views).

### seek_cursor

```rust,ignore
fn seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Raw access to index data, starting at the nearest match to the input. Unlike
an exact prefix:

1. The match need not be exact. Results begin at the point where the supplied
   components would reside, or the next datom after it.
2. There is no prefix termination. Results continue to the end of the index.

Prefer matching-prefix APIs wherever they are adequate. A seek caller must
supply termination logic for an attribute or value range. Schema-aware boundary
helpers resolve attributes, entity identifiers, and values. Boundaries support
up to four leading components, including a transaction coordinate. See
[seek APIs](../../06_indexes/04_index_apis.md#seek-apis).

### reverse_seek_cursor

```rust,ignore
fn reverse_seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Like `seek_cursor`, but iterates the index in reverse, beginning at or before
the point where the supplied components would reside. Missing suffix components
position after the highest match. It terminates only at the start of the index,
so callers must supply their own range termination logic. Per-item errors must
be handled as with forward cursors.

### AVET range

```rust,ignore
// Eager Database
fn avet_range(
    &self,
    attribute: u32,
    start: Option<&Value>,
    end: Option<&Value>,
) -> Result<&[Datom], SemanticError>;
```

Returns a range of AVET-indexed current datoms for an indexed or unique
attribute. `start` is inclusive; `None` means from the beginning. `end` is
exclusive; `None` means through the end of that attribute. Equal bounds return
an empty range; a start greater than the end is rejected.

`DatabaseValue` has no direct `index_range` method. Use `avet_boundary` and
`seek_cursor`, stopping when the attribute changes or the value reaches the
exclusive end. This supports temporal, filtered, and history values. Compare
values with `Value::index_cmp`. See the complete
[index-range examples](../../06_indexes/04_index_apis.md#index-range-api).

### pull

```rust,ignore
fn pull(&self, pattern: &PullPattern, entity: impl Into<EntityIdentifier>)
    -> Result<QueryValue, SemanticError>;
```

Returns a hierarchical selection of attributes for the entity. The pattern is
a typed `PullPattern`; parse an EDN pattern string with
`edb_core::edn_pull::parse_pull_edn` first.
The entity argument accepts an ID, ident, or lookup reference. The result is a
`QueryValue::Map`, with nested selections as specified by the pattern.
`pull_with_control` additionally accepts `&PullControl`.
History values are rejected. See the
[Pull reference](../../05_query_and_pull/06_pull_reference.md), including
defaults, transforms, and unresolved identifiers.

### index_pull

```rust,ignore
fn index_pull(&self, options: IndexPullOptions)
    -> Result<IndexPullCursor<'_>, SemanticError>;
```

Walks an index, pulling entities via E in AVET or reference V in AEVT, using
the selector, returning a lazy iterator of `Result<QueryValue, SemanticError>`.

| `IndexPullOptions` field | Meaning |
| --- | --- |
| `start` | AVET or AEVT `IndexBoundary`; at least the attribute must be specified. |
| `selector` | A `PullPattern`. |
| `reverse` | When true, iterate in reverse order; defaults to false. |
| `offset` | Skip this many datoms before projection; defaults to zero. |
| `limit` | Maximum projected results; defaults to `None`, meaning unlimited. |

Construct options with `IndexPullOptions::new(start, selector)`. Iteration is
limited to datoms matching the starting attribute, rather than the entire
starting prefix. AEVT requires a reference attribute. Results are not
deduplicated. Errors during reading or projection terminate the cursor.
`index_pull_with_control` additionally accepts `&PullControl`. See
[index Pull](../../06_indexes/03_index_pull.md).

### pull_many

```rust,ignore
fn pull_many<I, E>(&self, pattern: &PullPattern, entities: I)
    -> Result<Vec<QueryValue>, SemanticError>
where
    I: IntoIterator<Item = E>,
    E: Into<EntityIdentifier>;
```

Returns hierarchical selections of attributes for the supplied entity
identifiers. The result contains one map for each input identifier, in input
order. Pattern and history validation also apply to an empty input collection.

### db_stats

```rust,ignore
fn db_stats(&self) -> Result<DatabaseStats, SemanticError>;
fn db_stats_with_control(&self, control: &QueryControl)
    -> Result<DatabaseStats, SemanticError>;
```

Queries for database statistics. The result has these fields:

| Field | Meaning |
| --- | --- |
| `datoms: u64` | Total count of retained history datoms, including assertions and retractions. |
| `attrs: BTreeMap<u32, AttributeStats>` | Per-attribute history counts; `AttributeStats.count` is `u64`. |

Temporal and custom filters remain in force. This performs a history scan,
rather than a constant-time metadata lookup. It is not a count of entities or
a storage-size estimate. The controlled form observes cancellation, timeout,
and visible-history-datom work limits. An error returns no partial count.

## Example

This in-memory example applies transaction data, reads a selection, and compares
current and history views without advancing a connection:

```rust
use edb_core::{Database, IndexOrder, Keyword, SemanticError};
use edb_core::edn_pull::parse_pull_edn;

fn example() -> Result<(), SemanticError> {
    let initial = Database::bootstrap()?.database_value();
    let installed = initial.with_edn(r#"[
      {:db/id "edb.tx" :db/source :db.source/system}
      {:db/ident :person/name :db/source :db.source/system
       :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one}
    ]"#, 1_000)?.db_after;
    let first = installed.with_edn(r#"[
      {:db/id "edb.tx" :db/source :db.source/system}
      {:db/id "person" :db/source :db.source/system :person/name "Ada"}
    ]"#, 2_000)?;
    let id = first.tempids["person"];
    let changed = first.db_after.with_edn(
        &format!(r#"[
          {{:db/id "edb.tx" :db/source :db.source/system}}
          [:db/add {id} :person/name "Augusta" :db.source/system]
        ]"#),
        3_000,
    )?.db_after;
    let pattern = parse_pull_edn("[:person/name]")?;
    let earlier = changed.clone().as_of(first.db_after.basis_t());
    assert_eq!(earlier.basis_t(), changed.basis_t());
    assert_eq!(earlier.pull(&pattern, id)?, first.db_after.pull(&pattern, id)?);
    assert_ne!(changed.pull(&pattern, id)?, earlier.pull(&pattern, id)?);

    let name = changed.schema().resolve_ident(&Keyword::new("person", "name"))
        .expect("installed attribute");
    let names = changed.clone().filter(move |_, datom| datom.attribute == name);
    assert!(names.is_filtered());
    assert_eq!(names.datoms(IndexOrder::Eavt)?.len(), 1);
    let history = names.history();
    assert!(history.is_history());
    assert_eq!(history.db_stats()?.datoms, 3); // Two assertions and a retraction.
    Ok(())
}
```
