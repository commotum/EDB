# Forward and reverse seek

The database-value API provides index access to raw datoms via `seek_cursor`
and its reverse complement `reverse_seek_cursor`.

There are two concepts you need to understand when using these APIs: seek and
consume. You say where to seek and the cursor will return datoms in the
respective direction from that point in the specified index. It will continue
conveying datoms lazily until either you stop consuming them or you reach the end
of the index. Current, history, temporal and filtered database values support
these cursors, as do all four index orders. The selected view determines which
datoms are visible; AVET includes only indexed or unique attributes whose
projection is ready, and VAET includes only reference-valued attributes.

A seek position is not a restriction on the results. Once the cursor passes the
supplied components, it continues into the next entity, value or attribute in
that direction. Use `prefix_cursor` when you want only matching datoms. See
[ordered indexes and the transaction log](00_indexes_and_log.md).

## Forward seek

```rust
let cursor = db.seek_cursor(&boundary)?;
```

Returns datoms forward from the point in the index where the components would
reside. `db` is a `DatabaseValue`; `boundary` is an `IndexBoundary`. Each iterator
item is a `Result<Datom, SemanticError>`:

```rust
for datom in db.seek_cursor(&boundary)?.take(10) {
    let datom = datom?;
    // Consume this datom.
}
```

`collect_seek_datoms(&boundary)` collects the remaining index into a
`Vec<Datom>`. Prefer consuming the cursor incrementally when you only need a
bounded number of datoms. Errors can occur during traversal as well as when the
cursor is opened.

## Reverse seek

```rust
let cursor = db.reverse_seek_cursor(&boundary)?;
```

Returns datoms backward from the point in the index where the components would
reside. `collect_reverse_seek_datoms(&boundary)` is the collecting convenience.
Reverse seek traverses toward the beginning of the index; it does not reverse
the results of a forward seek from the same boundary.

The eager `Database` also provides `seek_datoms(&IndexPrefix)` and
`reverse_seek_datoms(&IndexPrefix)` for its current indexes. The former returns a
borrowed slice and the latter an owned vector. Use the `DatabaseValue` cursor
APIs above for lazy traversal and database views.

## Seek Position

You say where to seek by supplying components of a datom in index-sort order.
Regardless of whether the supplied datom exists, forward seek will start before
the lowest match and reverse seek after the highest. Both directions include
matching datoms: “before” and “after” describe the cursor position, not exclusive
bounds.

| Boundary | Component order |
| --- | --- |
| `IndexBoundary::Eavt` | entity, attribute, value, transaction |
| `IndexBoundary::Aevt` | attribute, entity, value, transaction |
| `IndexBoundary::Avet` | attribute, value, entity, transaction |
| `IndexBoundary::Vaet` | value, attribute, entity, transaction |

`IndexComponents::One` through `Four` supply a contiguous sequence from the left.
`Empty` starts at the beginning of the index for forward seek or the end for
reverse seek. E, A and V sort ascending; T sorts descending. Thus reversing an
E/A/V history group visits its transactions in ascending order. Entity and
attribute order use numeric IDs. See the [index model](01_index_model.md).

Use schema-aware helpers such as `db.avet_boundary(...)` to resolve attribute
idents, validate values and check AVET readiness against that database value:

```rust
use edb_core::{AttributeName, IndexComponents, Keyword, RawIndexValue, Value};

let boundary = db.avet_boundary(IndexComponents::Two(
    AttributeName::Ident(Keyword::new("item", "score")),
    RawIndexValue::Stored(Value::Long(20)),
))?;
let cursor = db.reverse_seek_cursor(&boundary)?;
```

This example assumes an indexed `:item/score` long attribute. A transaction
component uses `IndexTransaction::T(t)` or `IndexTransaction::Tx(tx_entity_id)`;
there is no fifth assertion/retraction component. At a full E/A/V/T boundary,
both directions include all datoms tied on those supplied components.

The following tables illustrate different aspects of seek positioning. Each
shows a slice of AVET. Small entity numbers stand in for allocated IDs, and
transaction components are omitted. Result tables list the first returned
datom unless they explicitly show continued traversal.

### Attribute only

For the inventory in the [next example](#attribute-and-value), seeking to
`[:inv/sku]` starts forward at entity 600 with `"SKU-42"`, or reverse at entity
500 with `"SKU-47"`. Forward traversal includes every SKU and continues into
following attributes; reverse traversal includes every SKU and continues into
preceding attributes.

Suppose `:item/score` has these datoms, in forward index order:

| A | V | E |
| --- | --- | --- |
| `:item/score` | 10 | 101 |
| `:item/score` | 20 | 102 |
| `:item/score` | 20 | 103 |
| `:item/score` | 30 | 104 |

| Supplied components | First forward result | First reverse result |
| --- | --- | --- |
| `[:item/score]` | value 10, entity 101 | value 30, entity 104 |

### Attribute and value

Each additional component specifies a more precise position in the index.

For example, seek to attribute `:inv/sku` and value `"SKU-44"`. The table below
shows datoms in forward AVET order. Assume these attributes are indexed and
ready, and their numeric attribute IDs place `:a/foo` before `:inv/sku` and
`:z/bar` after it.

| E | A | V | Forward from `[:inv/sku "SKU-44"]` | Reverse from `[:inv/sku "SKU-44"]` |
| --- | --- | --- | --- | --- |
| 100 | `:a/foo` | 1 | | continued traversal |
| … | … | … | | continued traversal |
| 600 | `:inv/sku` | `"SKU-42"` | | third |
| 540 | `:inv/sku` | `"SKU-43"` | | second |
| 700 | `:inv/sku` | `"SKU-44"` | first | first |
| 312 | `:inv/sku` | `"SKU-46"` | second | |
| 500 | `:inv/sku` | `"SKU-47"` | third | |
| … | … | … | continued traversal | |
| 500 | `:z/bar` | 42 | continued traversal | |

Both directions include `"SKU-44"`. Forward seek proceeds toward `"SKU-46"`
and `"SKU-47"`; reverse seek proceeds toward `"SKU-43"` and `"SKU-42"`.
Consumption can continue beyond `:inv/sku` into the following or preceding
attributes.

Using the numeric score example above, supplying an entity component makes the
position still more precise:

| Supplied components | First forward result | First reverse result |
| --- | --- | --- |
| `[:item/score 20]` | value 20, entity 102 | value 20, entity 103 |
| `[:item/score 20 102]` | value 20, entity 102 | value 20, entity 102 |

The bracket notation in these tables shows index components; use the typed Rust
boundary helpers to construct them.

### Tuple value

A tuple is one value component, compared lexicographically by its slots. Seek
to attribute `:inv/region+sku` and tuple value `["US" "SKU-44"]`. Suppose this
indexed tuple attribute has `:db/tupleTypes [:db.type/string :db.type/string]`.
As above, assume the numeric attribute IDs place `:a/foo` before it and `:z/bar`
after it, and all three attributes are indexed and ready.

| E | A | V | Forward from `[:inv/region+sku ["US" "SKU-44"]]` | Reverse from `[:inv/region+sku ["US" "SKU-44"]]` |
| --- | --- | --- | --- | --- |
| 100 | `:a/foo` | 1 | | continued traversal |
| … | … | … | | continued traversal |
| 600 | `:inv/region+sku` | `["BR" "SKU-44"]` | | third |
| 540 | `:inv/region+sku` | `["BR" "SKU-50"]` | | second |
| 700 | `:inv/region+sku` | `["US" "SKU-44"]` | first | first |
| 312 | `:inv/region+sku` | `["US" "SKU-46"]` | second | |
| 500 | `:inv/region+sku` | `["US" "SKU-47"]` | third | |
| … | … | … | continued traversal | |
| 500 | `:z/bar` | 42 | continued traversal | |

Both directions include `["US" "SKU-44"]`. Forward seek proceeds through
`["US" "SKU-46"]` and `["US" "SKU-47"]`; reverse seek proceeds through
`["BR" "SKU-50"]` and `["BR" "SKU-44"]`. The region is compared before the SKU,
so `["BR" "SKU-50"]` sorts before `["US" "SKU-44"]`. Neither direction is
restricted to a region or to the tuple attribute: consumption can continue into
the following or preceding attributes.

For the additional examples below, suppose an indexed numeric tuple attribute
with `:db/tupleTypes [:db.type/long :db.type/long]` has these datoms:

| A | V | E |
| --- | --- | --- |
| `:item/pair` | `[1 5]` | 101 |
| `:item/pair` | `[2 nil]` | 102 |
| `:item/pair` | `[2 3]` | 103 |
| `:item/pair` | `[2 7]` | 104 |
| `:item/pair` | `[3 1]` | 105 |

| Supplied components | First forward result | First reverse result |
| --- | --- | --- |
| `[:item/pair [2 3]]` | `[2 3]`, entity 103 | `[2 3]`, entity 103 |

### Value not in the index

When the supplied component does not exist in the index, both APIs start at the
same point. They consume in opposite directions from that gap, so the first
returned datoms differ.

For example, seek to `[:inv/sku "SKU-45"]` in the inventory above. There is no
`"SKU-45"` datom: its virtual position falls between `"SKU-44"` and `"SKU-46"`.

| E | A | V | Forward from `[:inv/sku "SKU-45"]` | Reverse from `[:inv/sku "SKU-45"]` |
| --- | --- | --- | --- | --- |
| 100 | `:a/foo` | 1 | | continued traversal |
| … | … | … | | continued traversal |
| 600 | `:inv/sku` | `"SKU-42"` | | third |
| 540 | `:inv/sku` | `"SKU-43"` | | second |
| 700 | `:inv/sku` | `"SKU-44"` | | first |
| 312 | `:inv/sku` | `"SKU-46"` | first | |
| 500 | `:inv/sku` | `"SKU-47"` | second | |
| … | … | … | continued traversal | |
| 500 | `:z/bar` | 42 | continued traversal | |

Forward seek returns `"SKU-46"` first; reverse seek returns `"SKU-44"` first.
Both can continue beyond the inventory attribute. The virtual gap does not
change the inclusive boundary rule: forward seek includes datoms comparing
equal to or greater than the supplied components, and reverse seek includes
datoms comparing equal to or less than them. Here there are no equal datoms.

The numeric examples also show the first results on either side of a missing
value:

| Supplied components | First forward result | First reverse result |
| --- | --- | --- |
| `[:item/score 25]` | value 30, entity 104 | value 20, entity 103 |
| `[:item/pair [2 5]]` | `[2 7]`, entity 104 | `[2 3]`, entity 103 |

### Partial tuple

A partial tuple supplies a more precise virtual position without supplying all
the tuple's slots. For example, seek to `[:inv/region+sku ["US"]]` in the tuple
inventory above. The virtual position falls after the `"BR"` tuples and before
the `"US"` tuples:

| E | A | V | Forward from `[:inv/region+sku ["US"]]` | Reverse from `[:inv/region+sku ["US"]]` |
| --- | --- | --- | --- | --- |
| 100 | `:a/foo` | 1 | | continued traversal |
| … | … | … | | continued traversal |
| 600 | `:inv/region+sku` | `["BR" "SKU-44"]` | | second |
| 540 | `:inv/region+sku` | `["BR" "SKU-50"]` | | first |
| 700 | `:inv/region+sku` | `["US" "SKU-44"]` | first | |
| 312 | `:inv/region+sku` | `["US" "SKU-46"]` | second | |
| 500 | `:inv/region+sku` | `["US" "SKU-47"]` | third | |
| … | … | … | continued traversal | |
| 500 | `:z/bar` | 42 | continued traversal | |

Forward seek begins with `["US" "SKU-44"]`; reverse seek begins with
`["BR" "SKU-50"]`. Neither direction is restricted to the supplied region or
attribute. The partial tuple is not a wildcard: `["US"]` sorts before all
tuples beginning with `"US"`, so reverse seek excludes those tuples.

A shorter tuple sorts before its longer extensions, including extensions with
`nil` slots. Using the numeric tuple example above:

| Supplied components | First forward result | First reverse result |
| --- | --- | --- |
| `[:item/pair [2]]` | `[2 nil]`, entity 102 | `[1 5]`, entity 101 |
| `[:item/pair [2 nil]]` | `[2 nil]`, entity 102 | `[2 nil]`, entity 102 |

In particular, reverse seek from `[2]` does not include the tuples beginning
with `2`. To construct this virtual value, pass
`RawIndexValue::Tuple(vec![Some(RawIndexValue::Stored(Value::Long(2)))])` to
`avet_boundary`. `None` denotes an explicit `nil` slot. An empty tuple boundary
sorts before every stored tuple, so reverse seek from it starts in a preceding
attribute, or returns no datoms if none precedes it. A partial tuple boundary
does not make partial stored assertions legal. See
[partial tuple boundaries](../02_core_concepts/00_database_values.md#partial-tuple-boundaries).

### Multiple entities with the same value

When multiple entities possess the same value, forward seek starts before the
lowest one and reverse seek after the highest. Consider this variation of the
inventory: entities 312 and 700 both possess `"SKU-44"`. Here `:inv/sku` is
indexed but not unique, so the duplicate value is allowed. The same numeric
attribute ordering as above applies.

| E | A | V | Forward from `[:inv/sku "SKU-44"]` | Reverse from `[:inv/sku "SKU-44"]` |
| --- | --- | --- | --- | --- |
| 100 | `:a/foo` | 1 | | continued traversal |
| … | … | … | | continued traversal |
| 600 | `:inv/sku` | `"SKU-42"` | | fourth |
| 540 | `:inv/sku` | `"SKU-43"` | | third |
| 312 | `:inv/sku` | `"SKU-44"` | first | second |
| 700 | `:inv/sku` | `"SKU-44"` | second | first |
| 500 | `:inv/sku` | `"SKU-47"` | third | |
| … | … | … | continued traversal | |
| 500 | `:z/bar` | 42 | continued traversal | |

Forward seek visits entity 312 then entity 700 at `"SKU-44"`, and continues to
`"SKU-47"`. Reverse seek visits entity 700 then entity 312 at `"SKU-44"`, and
continues to `"SKU-43"` and `"SKU-42"`. Both include every entity tied on the
supplied value and can continue beyond `:inv/sku` into neighboring attributes.

With the score datoms above,
forward seek from `[:item/score 20]` visits entities 102 then 103 before
continuing to value 30. Reverse seek visits entities 103 then 102 before
continuing to value 10. Neither direction drops an entity tied on the supplied
value, and neither stops at the end of those ties.
