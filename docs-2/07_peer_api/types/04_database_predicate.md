# Database Filter Predicate

Boolean-valued function for [filtering](03_database.md#filter) a database.
Pass a Rust function or closure to `DatabaseValue::filter`:

```rust,ignore
fn filter<F>(self, predicate: F) -> DatabaseValue
where
    F: Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static;
```

The datom type is `edb_core::Datom`. The callback uses Rust's `Fn` trait;
there is no separate public database predicate interface to implement.

## Callback Summary

| Return type | Callback | Description |
| --- | --- | --- |
| `bool` | `predicate(db: &DatabaseValue, val: &Datom)` | Database-filtering predicate. |

## Callback Details

Database-filtering predicate.

Parameters:

- `db`: a database value with the same captured basis, `as_of`, `since`, and
  history settings as the value being read, with **all custom predicates
  removed**. Database reads through this argument do not recursively invoke
  this predicate or any earlier chained predicate.
- `val`: a datom. Its fields are `entity: u64`, `attribute: u32`, `value: Value`,
  `tx: u64` (a transaction entity ID), `source: u64` (a source entity ID), and
  `added: bool`. The callback can see assertions and retractions, including when
  filtering a point-in-time value.

Returns `true` if the datom matches the predicate; `false` excludes it. Returning
`true` does not guarantee that the datom appears in the read result: other
chained predicates and point-in-time retraction collapse still apply.

The callback returns a Boolean, not a `Result`. If it performs fallible database
reads, the application must decide how to handle their errors. To propagate
errors before constructing the filter, precompute the accepted IDs with a
fallible read and capture that set in the closure.

## Evaluation

`filter` consumes a database value and returns a derived value; clone the input
first when both views are needed. It stores the predicate without scanning the
database. Predicates run as index data is read, including reads performed by
queries and entity navigation. Repeated `filter` calls combine predicates by
logical conjunction, short-circuiting when a predicate returns `false`.

Keep predicates pure and deterministic. Reads can evaluate a predicate multiple
times, and callers should not depend on invocation counts or a particular
traversal order. Captured data must satisfy `Send + Sync + 'static`; move owned
data into the closure when it must outlive the call to `filter`.

Temporal bounds are checked before custom predicates: only datoms with
`since < t <= as_of`, where those bounds are present, reach the callback.
Custom predicates run before point-in-time retraction collapse. Rejecting a
retraction can therefore expose an older assertion; arbitrary filters can even
leave several values visible for a cardinality-one attribute. A history value
returns accepted assertions and retractions without that collapse.

The callback database has the temporal/history settings of the value **being
read**, including view operations applied after `filter`. Entity navigation
and Pull reject a history callback database. A `since` boundary may hide
transaction metadata needed by a predicate; use an appropriate separately
captured value or precompute accepted transaction IDs when necessary. The
captured schema and ident dictionary are not filtered datoms. See
[database filters](../../02_core_concepts/03_database_filters.md) for transaction
metadata predicates, temporal examples, and security considerations.

## Example

This predicate accepts only datoms about the installed `:person/name` attribute:

```rust
use edb_core::{Database, IndexOrder, Keyword, SemanticError};

fn example() -> Result<(), SemanticError> {
    let installed = Database::bootstrap()?.with_edn(r#"[
      {:db/id "edb.tx" :db/source :db.source/system}
      {:db/ident :person/name :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one :db/source :db.source/system}
    ]"#, 1_000)?.db_after;
    let populated = installed.with_edn(r#"[
      {:db/id "edb.tx" :db/source :db.source/system}
      {:db/id "person" :person/name "Ada" :db/source :db.source/system}
    ]"#, 2_000)?.db_after;
    let db = populated.database_value();
    let name = db.schema().resolve_ident(&Keyword::new("person", "name"))
        .expect("installed attribute");

    let names = db.clone().filter(move |context, val| {
        // The callback context has no custom predicates.
        assert!(!context.is_filtered());
        val.attribute == name
    });
    assert!(names.is_filtered());
    let datoms = names.datoms(IndexOrder::Eavt)?;
    assert_eq!(datoms.len(), 1);
    assert_eq!(datoms[0].attribute, name);
    Ok(())
}
```
