# Index APIs

Most EDB programs will not access the [indexes](01_index_model.md) directly,
instead taking advantage of EDB's declarative
[Datalog](../05_query_and_pull/00_queries.md) query engine.

However, raw index access is available for integrations. For example, you might
use the indexes to make EDB data available to a map/reduce framework spanning
multiple data sources of different types.

EDB indexes are covering indexes. This means the index actually contains the
datoms, rather than just a pointer to them. When you (or an EDB query) iterate an
index, you get the full datoms—`[e a v t s added?]`, including the source entity
id—without any additional entity lookup.
Durable cursors can still fetch index nodes as they advance.

## Datoms API

The datoms API lets you specify an EDB index, plus leading components in the
same order as the index. `DatabaseValue::prefix_cursor` returns exactly the
datoms matching an `IndexPrefix`. `datoms_with_prefix` collects the same matches
into a `Vec<Datom>`. With no prefix, `scan_cursor(IndexOrder)` traverses the whole
index; `datoms(IndexOrder)` collects it.

The example below searches the AVET index for datoms whose `a` component is
`:inv/sku`. This small in-memory database supplies the data for the examples:

```rust
use edb_core::{
    AttributeName, Database, IndexComponents, IndexPrefix, Keyword, Value,
};

let installed = Database::bootstrap()?.with_edn(r#"[
  [:db/txInstant :db.source/system]
  {:db/ident :inv/sku
   :db/source :db.source/system
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one
   :db/index true}
]"#, 1)?.db_after;
let memory = installed.with_edn(r#"[
  [:db/txInstant :db.source/system]
  {:db/source :db.source/system :inv/sku "SKU-0"}
  {:db/source :db.source/system :inv/sku "SKU-1"}
  {:db/source :db.source/system :inv/sku "SKU-10"}
  {:db/source :db.source/system :inv/sku "SKU-42"}
  {:db/source :db.source/system :inv/sku "SKU-43"}
  {:db/source :db.source/system :inv/sku "SKU-44"}
]"#, 2)?.db_after;
let db = memory.database_value();
let sku = db.schema().resolve_ident(&Keyword::new("inv", "sku"))
    .expect("installed :inv/sku");

let values = db.prefix_cursor(&IndexPrefix::Avet {
    attribute: sku,
    value: None,
    entity: None,
})?
    .take(3)
    .map(|item| item.map(|datom| datom.value))
    .collect::<Result<Vec<_>, _>>()?;
assert_eq!(values, vec![
    Value::String("SKU-0".into()),
    Value::String("SKU-1".into()),
    Value::String("SKU-10".into()),
]);
```

Values sort in index order: string SKUs sort lexically, so `"SKU-10"` precedes
`"SKU-2"`. Supplying `value: Some(Value::String("SKU-42".into()))` restricts this
prefix to that exact value. It does not include `"SKU-43"`.

`IndexPrefix` contains resolved IDs and values. Its components must be contiguous
from the left: an AVET entity component requires a value component. Prefixes
support up to E/A/V in their index order; they do not have transaction or source
fields.
See [the index model](01_index_model.md) for component order and index coverage.

## Seek APIs

`DatabaseValue::seek_cursor` starts at an `IndexBoundary` and continues through
the rest of the index. Unlike an exact prefix, the supplied components choose
only the starting position. A seek at `[:inv/sku "SKU-42"]` can return
`"SKU-43"`, `"SKU-44"`, and datoms for later attributes. Stop explicitly when
the required range ends.

```rust
let start = db.avet_boundary(IndexComponents::Two(
    AttributeName::Id(sku),
    Value::String("SKU-42".into()).into(),
))?;
let mut values = Vec::new();
for item in db.seek_cursor(&start)? {
    let datom = item?;
    if datom.attribute != sku || values.len() == 2 {
        break;
    }
    values.push(datom.value);
}
assert_eq!(values, vec![
    Value::String("SKU-42".into()),
    Value::String("SKU-43".into()),
]);
```

Use `.take(n)` on a cursor when an overall result count is sufficient. Raw
cursors have no implicit result limit. Taking a limit on a previously collected
`Vec` does not limit the work or memory used to collect it. A loop that checks
the next datom to detect a boundary can consume that datom before stopping.

`reverse_seek_cursor` traverses toward the beginning of the index, starting
after the highest match of the supplied components. Thus a forward seek on an
attribute begins before its first value, while a reverse seek on that attribute
begins after its last value. Both can spill into adjacent attributes. These are
ordered boundary operations, not string or tuple prefix searches.

The schema-aware helpers `eavt_boundary`, `aevt_boundary`, `avet_boundary`, and
`vaet_boundary` resolve idents and lookup refs against the captured database.
`IndexComponents::Empty`, `One`, `Two`, `Three`, and `Four` express zero through
four contiguous components in index order. The fourth is
`IndexTransaction::T(t)` or `IndexTransaction::Tx(tx_entity)`; transaction order
is descending, followed by source ascending and assertions before retractions.
Source and assertion/retraction are not boundary components.
`collect_seek_datoms` and `collect_reverse_seek_datoms` explicitly materialize
their respective cursors.

### Partial tuple boundaries

For a tuple-valued attribute, a boundary can contain only the leading tuple
slots, including no slots. For example, given an indexed `:inv/pair` with
`:db/tupleTypes [:db.type/string :db.type/long]`:

```rust
use edb_core::{DatabaseValue, IndexBoundary, RawIndexValue, SemanticError};

fn pair_start(db: &DatabaseValue) -> Result<IndexBoundary, SemanticError> {
    db.avet_boundary(IndexComponents::Two(
        AttributeName::Ident(Keyword::new("inv", "pair")),
        RawIndexValue::Tuple(vec![Some(
            Value::String("SKU-42".into()).into(),
        )]),
    ))
}
```

The virtual value `["SKU-42"]` sorts before `["SKU-42" nil]` and
`["SKU-42" 1]`. Forward traversal starts before those longer values; reverse
traversal from that same short boundary does not include them. A short tuple is
not a wildcard, and traversal does not stop when its leading slots change.
Overlong boundaries and incorrectly typed slots are rejected. `None` is a tuple
nil slot; reference slots can use `RawIndexValue::Entity` to resolve an ident or
lookup ref. Transactions still require valid stored tuple arity. See
[partial tuple boundaries](../02_core_concepts/00_database_values.md#partial-tuple-boundaries).

## Index-range API

The index-range operation lets you specify an attribute, and returns datoms
sorted by that attribute's value, optionally limited by start and end values.
The example below returns all the SKUs between `"SKU-42"` inclusive and
`"SKU-44"` exclusive.

The in-memory `Database` exposes `avet_range(attribute, start, end)`:

```rust
let start = Value::String("SKU-42".into());
let end = Value::String("SKU-44".into());
let values: Vec<_> = memory.avet_range(sku, Some(&start), Some(&end))?
    .iter()
    .map(|datom| datom.value.clone())
    .collect();
assert_eq!(values, vec![
    Value::String("SKU-42".into()),
    Value::String("SKU-43".into()),
]);
```

Omitting either bound with `None` leaves that side open within the attribute.
Equal bounds return an empty range; a start greater than the end is rejected.
This method reads the in-memory database's current index.

For a captured `DatabaseValue`, including a durable or temporal value, use a
seek and stop at the attribute or exclusive upper bound:

```rust
let boundary = db.avet_boundary(IndexComponents::Two(
    AttributeName::Id(sku), start.clone().into(),
))?;
let mut values = Vec::new();
for item in db.seek_cursor(&boundary)? {
    let datom = item?;
    if datom.attribute != sku || !datom.value.index_cmp(&end).is_lt() {
        break;
    }
    values.push(datom.value);
}
assert_eq!(values, vec![
    Value::String("SKU-42".into()),
    Value::String("SKU-43".into()),
]);
```

For an open lower bound use `IndexComponents::One(AttributeName::Id(sku))`.
For an open upper bound stop only when the attribute changes. Use
`Value::index_cmp` for value ordering. To project entities from an ordered
attribute range, see [index Pull](03_index_pull.md).

## Readiness and database views

An attribute needs `:db/index true` or `:db/unique` for AVET access. Durable
background projection must also be ready in the exact captured value:

```rust
assert!(db.has_avet(&AttributeName::Id(sku))?);
```

For a known unindexed or pending attribute, `has_avet` returns `false`; unknown
attributes return an error. Attribute-qualified AVET boundary helpers and seek
cursors reject unindexed attributes with `index/attribute-not-in-avet` and
pending projections with `index/avet-not-ready`. The lower-level prefix API
does not perform the same schema validation: an in-memory prefix for an
unindexed attribute returns no matches, while a durable prefix rejects it with
`peer/avet-attribute-not-indexed` and rejects a pending projection with
`peer/avet-not-ready`. Check `has_avet` before relying on AVET
prefix results. Unrestricted `DatabaseValue` AVET scans omit unavailable attributes.
The check neither waits for indexing nor refreshes the value. See
[background indexing](02_background_indexing.md) for requesting publication and
acquiring a value that observes it.

Raw cursors honor the database value's time window and filters. Current views
return visible assertions; history can include both assertions and retractions.
Cursor construction and advancement are fallible; handle each yielded `Result`.
An error terminates that cursor. An immutable value does not gain later commits
or index publications while you traverse it.

Entity IDs are not creation timestamps. In particular, `entid_at` does not give
a reliable creation-time cutoff for EAVT scans under native allocation; see
[new entity scans](../04_transactions/02_partitions.md#new-entity-scans).
