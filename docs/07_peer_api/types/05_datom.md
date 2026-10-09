# Datom

`edb_core::Datom` represents a point-in-time fact:
`[entity, attribute, value, transaction, source, added]`.

Facts in a database value are immutable. The Rust representation is an owned
struct with public fields; changing a datom you own does not change the database.
Use [transactions](../../04_transactions/04_transaction_data.md) to assert or
retract facts. `Datom`, `Value`, and `IndexOrder` are exported by `edb_core`.

## Field summary

| Field | Type | Description |
| --- | --- | --- |
| `entity` | `u64` | This datom's entity ID (E). |
| `attribute` | `u32` | This datom's attribute ID (A). |
| `value` | `Value` | This datom's value (V). |
| `tx` | `u64` | This datom's transaction entity ID (Tx). |
| `source` | `u64` | The source entity ID for this assertion or retraction (S). |
| `added` | `bool` | Whether this datom was added or retracted. |

## Field details

### `entity`

This datom's [entity ID](../../03_schema/01_identity_and_values.md).
The ID identifies the entity about which the fact is asserted or retracted.

### `attribute`

This datom's [attribute](../../03_schema/02_schema_reference.md) ID.
It is the numeric schema identity, not the attribute's keyword ident. Attribute
IDs use `u32`; general entity IDs use `u64`. Given a database value `db`,
`db.schema().attribute(datom.attribute)?` retrieves the attribute descriptor.
See [Attribute](01_attribute.md).

### `value`

This datom's value, represented by the `Value` enum. For example,
`Value::String(String)` owns a string, `Value::Long(i64)` holds an integer, and
`Value::Ref(u64)` holds an entity reference. Borrow `&datom.value` to inspect the
value without moving it out of the datom, or clone it when another owned value
is needed. See [value types](../../03_schema/02_schema_reference.md#dbvaluetype)
for the supported representations.

### `tx`

This datom's [transaction entity ID](../../04_transactions/05_transaction_model.md).
It identifies the transaction that asserted or retracted this fact.

The transaction entity ID and logical transaction time T have different
representations. Use `tx_to_t(datom.tx)?` to recover T, and `t_to_tx(t)?` to
construct a transaction entity ID from T. Both functions return
`Result<u64, SemanticError>`. A database's `basis_t()` returns logical T; compare
it with the converted value rather than the raw `tx` field. The transaction's
wall-clock time is a separate `:db/txInstant` fact, automatically asserted with
the transaction's source.

### `source`

The source entity ID resolved from the reference explicitly supplied by the
application once for the transaction. The source is an ordinary entity described
by ordinary facts. All assertions and retractions produced by one transaction
carry that source, including generated changes and its automatic timestamp.

A retraction carries the retracting source's ID; it does not change the source
of the earlier assertion. Assertions from different transactions with the same
entity, attribute, and value but different sources retain their distinct
attribution.

### `added`

Is this datom added or retracted? `true` indicates an assertion; `false`
indicates a retraction.

When datoms come from `DatabaseValue::history()`, this field distinguishes
additions from retractions. A current database view exposes facts still
asserted in that view. History exposes retained assertions and retractions;
retention settings can remove historical information. See
[database filters](../../02_core_concepts/03_database_filters.md).

## Access and ownership

Read fields directly: `datom.entity`, `datom.attribute`, `datom.tx`,
`datom.source`, and `datom.added`. The conceptual tuple positions are E, A, V,
Tx, S, and added, in that order. `Datom` has no positional `get(index)` method,
indexing operator, or collection interface. Use field access or Rust struct
destructuring.

`Datom` implements `Clone`, `Debug`, `Eq`, and `PartialEq`. Cloning produces an
owned datom, including its owned value. Equality compares all six fields;
value equality uses `Value::index_cmp`, so numerically equivalent values can
compare equal across numeric representations.

## Index comparison

```rust,ignore
fn cmp_in(&self, other: &Self, index: IndexOrder) -> std::cmp::Ordering;
```

Compare datoms in the specified index order. `Datom` does not implement `Ord`
or `PartialOrd`; choose the desired order explicitly with `cmp_in`.

| `IndexOrder` | Leading components, ascending |
| --- | --- |
| `Eavt` | Entity, attribute, value |
| `Aevt` | Attribute, entity, value |
| `Avet` | Attribute, value, entity |
| `Vaet` | Value, attribute, entity |
| `Saevt` | Source, attribute, entity, value |
| `Savet` | Source, attribute, value, entity |

Values use `Value::index_cmp`. After the leading components compare equal,
transactions sort in descending order. The four original orders then compare
sources in ascending order; the source-first orders already compared source.
Assertions (`added == true`) sort before retractions. Finally, `Value::stored_cmp`
breaks remaining ties; it distinguishes the scale of top-level decimal values.
Consequently, two datoms can be equal under `==` yet compare differently under
`cmp_in` when their top-level decimal scales differ.

Choosing an order for comparison does not determine which datoms belong in an
index. EAVT, AEVT and SAEVT cover all attributes. AVET and SAVET include the
same eligible attributes with ready projections; VAET includes
reference attributes. See the [index model](../../06_indexes/01_index_model.md)
and [index APIs](../../06_indexes/04_index_apis.md).

This complete example constructs owned datoms, reads their fields, converts
transaction coordinates, and sorts an assertion and its later retraction:

```rust
use edb_core::{Datom, IndexOrder, SemanticError, Value, t_to_tx, tx_to_t};

fn example() -> Result<(), SemanticError> {
    let assertion = Datom {
        entity: 42,
        attribute: 100,
        value: Value::String("Ada".into()),
        tx: t_to_tx(7)?,
        source: 300,
        added: true,
    };
    assert_eq!(assertion.entity, 42);
    assert_eq!(assertion.attribute, 100);
    assert_eq!(&assertion.value, &Value::String("Ada".into()));
    assert_eq!(tx_to_t(assertion.tx)?, 7);
    assert_eq!(assertion.source, 300);
    assert!(assertion.added);

    let retraction = Datom {
        tx: t_to_tx(8)?,
        source: 301,
        added: false,
        ..assertion.clone()
    };
    let mut datoms = vec![assertion, retraction];
    datoms.sort_by(|left, right| left.cmp_in(right, IndexOrder::Eavt));
    assert_eq!(tx_to_t(datoms[0].tx)?, 8);
    assert_eq!(datoms[0].source, 301);
    assert!(!datoms[0].added);
    Ok(())
}
```

Constructing these Rust values does not submit a transaction or install the
illustrative entity and attribute IDs in a database.
