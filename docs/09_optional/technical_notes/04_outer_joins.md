# Outer Joins

In the relational database world, outer joins allow you to return relations even when data is missing from one side of a join. For example, you might want all of the artists and their start years, including artists who do not even have a start year.

In EDB, you can find the entities you want with [Datalog](../../05_query_and_pull/05_query_reference.md), and then make an independent decision about which details you want to [pull](../../05_query_and_pull/06_pull_reference.md). The following example uses a query to find the artists, and then plugs in a pull pattern to get both the artist name and start year:

```rust
use edb_core::{QueryControl, QuerySourceValue};
use edb_core::edn::read_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

// db is a captured DatabaseValue with the artist attributes installed.
let find_expr = parse_query_edn(
    r#"[:find (pull ?e details-expr)
        :in $ details-expr
        :where [?e :artist/name]]"#,
)?;
let details_expr = read_edn("[:artist/name :artist/startYear]")?;
let bound = find_expr.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
    EdnQueryArgument::Data(details_expr),
])?;
let outcome = bound.execute(&QueryControl::default(), None)?;
let result = bound.result_to_edn(&outcome.result)?;
```

The query/pull separation also makes it easy to reuse query and pull logic independently. Each matching artist remains in the result; a missing start year is omitted from that artist's pulled map. For example, with one artist whose start year is known and one whose start year is absent, the EDN result could be:

```edn
#{[{:artist/name "Known Artist" :artist/startYear 1968}]
  [{:artist/name "Unknown Artist"}]}
```

EDB also includes the [get-else query function](../../05_query_and_pull/05_query_reference.md#get-else), which is closer to a literal outer join in that you can reference a possibly-missing attribute directly in the Datalog, specifying an alternate value when the attribute is missing. The example below replaces a missing start year with "Unknown":

```edn
[:find ?e ?name ?year
 :where
 [?e :artist/name ?name]
 [(get-else $ ?e :artist/startYear "Unknown") ?year]]
```

The omitted `:in` defaults to `$`; bind the database as the source input as in the first example. `get-else` requires a cardinality-one attribute and a non-nil default. The string default above is allowed even when the stored start year is a long. The `get-else` example requires `:artist/name` and `:artist/startYear` in the schema; a missing value is different from an unknown attribute.

For related operations, [get-some](../../05_query_and_pull/05_query_reference.md#get-some) tries cardinality-one attributes in order, returning the first available attribute id and value. If none is present, it produces no tuple, so it does not preserve that artist's row. The [missing? predicate](../../05_query_and_pull/05_query_reference.md#missing) instead selects artists whose start year is absent:

```edn
[:find ?name
 :where
 [?e :artist/name ?name]
 [(missing? $ ?e :artist/startYear)]]
```
