# Querying on byte array attributes

The equality semantics of byte values in EDB are those of value equality:
two byte values are equal when they contain the same bytes in the same order.
Datalog queries use this equality when matching byte attributes. Separate
allocations do not prevent a match. Consider the following example.

- First, we do some setup, creating a simple schema with just one byte array
  attribute. These Rust examples run in order in a function returning
  `Result<(), Box<dyn std::error::Error>>`. They use local speculative database
  values, so no running database service is required.

  ```rust
  use edb_core::{Database, QueryControl, QuerySourceValue, Value};
  use edb_core::edn::{read_edn, write_edn};
  use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

  let empty = Database::bootstrap()?;
  let schema = empty.with_edn(r#"
      [{:db/ident :some-bytes
        :db/valueType :db.type/bytes
        :db/cardinality :db.cardinality/one}]
  "#, 10)?;
  ```

- Next, a few entities with two byte arrays with equivalent values. Each
  `#edb/bytes` string encodes bytes as hexadecimal pairs; `"010203"` contains
  the three bytes `1`, `2`, and `3`.

  ```rust
  let report = schema.db_after.with_edn(r#"
      [{:db/id "first"  :some-bytes #edb/bytes "010203"}
       {:db/id "second" :some-bytes #edb/bytes "010203"}]
  "#, 20)?;
  let db = report.db_after.database_value();
  ```

- Rust equality on two separately constructed EDB byte values returns true:

  ```rust
  let bytes_1 = Value::Bytes(vec![1, 2, 3]);
  let bytes_2 = Value::Bytes(vec![1, 2, 3]);
  assert_eq!(bytes_1, bytes_2);
  ```

- Therefore running the following query with a newly read byte value returns
  both entities:

  ```rust
  let query = parse_query_edn(
      "[:find ?e
        :in $ ?bytes
        :where [?e :some-bytes ?bytes]]"
  )?;
  let bound = query.bind(&[
      EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
      EdnQueryArgument::Data(read_edn(r#"#edb/bytes "010203""#)?),
  ])?;
  let result = bound.execute(&QueryControl::default(), None)?;
  println!("{}", write_edn(&bound.result_to_edn(&result.result)?)?);
  ```

  For this fresh bootstrap database, the result is:

  ```edn
  #{[#edb/ref 17592186045417] [#edb/ref 17592186045418]}
  ```

  The entity IDs depend on allocation history. The relation is a set of tuples;
  its row order is not significant. Supplying `#edb/bytes "010204"` instead
  returns `#{}`.

- Using the built-in `=` predicate to test for equality also returns both
  entities. Replace the query text above with:

  ```edn
  [:find ?e
   :in $ ?bytes
   :where [?e :some-bytes ?stored]
          [(= ?stored ?bytes)]]
  ```

  Ordinary pattern matching already compares byte contents, so a special
  array-equality function is unnecessary.

See [value and identity rules](../../03_schema/01_identity_and_values.md), the
[EDN data frontend](../../01_tutorials/00_edn_workflow.md), and the
[query reference](../../05_query_and_pull/05_query_reference.md).
