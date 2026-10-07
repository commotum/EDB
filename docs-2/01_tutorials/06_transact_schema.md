# Transact Schema

Previously: you learned how to [connect to a database](05_connect_to_a_database.md).
This tutorial expects that you have a running transactor for `edn-demo` and an
application terminal with `EDB_POSTGRES_URL` set to your **peer** connection and
`EDB_TUTORIAL_ENDPOINT` set to the transactor's private socket path. Run the shell
commands from the root of your EDB checkout.

One of the great things about EDB is that the definition of the schema for your
database is itself data. Because it is just data, it can be created and
manipulated just like any other data you put into the database. To add a new
schema, you issue a transaction that carries the new schema definition.

This section will create a basic model for movies: a movie can have a title, a
release year, and a genre. Title and genre are represented as *strings*, while
release year is an *integer*. In a relational database, each of those (title,
release year, and genre) would be considered "fields" in a "table".

In EDB, each of those is an attribute, which can be associated with an entity.
To start adding movie data to our database, we'll have to tell it about those
three attributes. And, since schema is just data, you describe your custom
attributes using EDB's [built-in attributes](../03_schema/02_schema_reference.md).

Every attribute definition has [three required attributes](../03_schema/02_schema_reference.md#defining-schema):
`:db/ident`, `:db/valueType`, and `:db/cardinality`.

You can also supply the optional `:db/doc` attribute, which stores a docstring
of the attribute which you can query for later.

Each transaction supplies one source for all of its assertions and retractions.
For this tutorial, we'll use the built-in source entity `:db.source/system`
when submitting the schema.

- Our first attribute, "title", will be defined as:

  ```edn
  {:db/ident :movie/title
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one
   :db/doc "The title of the movie"}
  ```

- Similarly, "genre" will be:

  ```edn
  {:db/ident :movie/genre
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one
   :db/doc "The genre of the movie"}
  ```

- Since "year" will be stored as a number, we'll need to use `:db.type/long`
  for its `:db/valueType`:

  ```edn
  {:db/ident :movie/release-year
   :db/valueType :db.type/long
   :db/cardinality :db.cardinality/one
   :db/doc "The year the movie was released in theaters"}
  ```

You could choose to send each of these attributes to the database in individual
transactions. We'll batch them up into a single transaction and combine the
three maps into a single vector. EDB automatically records the transaction
instant with the source supplied when we submit the transaction. Let's save our
schema vector in a file called `movie-schema.edn`:

```sh
cat > movie-schema.edn <<'EDN'
[{:db/ident :movie/title
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/doc "The title of the movie"}

 {:db/ident :movie/genre
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/doc "The genre of the movie"}

 {:db/ident :movie/release-year
  :db/valueType :db.type/long
  :db/cardinality :db.cardinality/one
  :db/doc "The year the movie was released in theaters"}]
EDN
```

Now that we have our attribute definitions ready to go, we have to issue a
transaction. The executable's `transact` command uses the peer library to open a
connection and submit the transaction to the running transactor. It takes:

- The logical database name and transaction endpoint.
- A collection of transaction data, formatted in [list or map form](../04_transactions/04_transaction_data.md), supplied through a file.
- A request key identifying this transaction for exact retries.
- One source entity reference supplied with `--source`.

Go ahead and transact the schema:

```sh
target/release/edb transact --database edn-demo \
  --endpoint "$EDB_TUTORIAL_ENDPOINT" \
  --request-key tutorial-movie-schema --source ':db.source/system' \
  --file movie-schema.edn
```

The executable prints EDN with `:edb/committed true`, `:edb/basis-t`,
`:edb/tx-hash`, and `:edb/replayed`. The report fields look like this; IDs, time
and basis depend on your database. This example assumes the "Hello world"
transaction from the previous section was the first transaction.

In this schematic report, `source-id` denotes the resolved entity ID of
`:db.source/system`.

```text
{:edb/db-before-t 1
 :edb/db-after-t 2
 :edb/tx-data
 [[0 13 #edb/ref 1000 13194139533314 source-id true]
  [0 13 #edb/ref 1001 13194139533314 source-id true]
  [0 13 #edb/ref 1002 13194139533314 source-id true]
  [1000 10 :movie/genre 13194139533314 source-id true]
  [1000 40 #edb/ref 23 13194139533314 source-id true]
  [1000 41 #edb/ref 35 13194139533314 source-id true]
  [1000 62 "The genre of the movie" 13194139533314 source-id true]
  [1001 10 :movie/title 13194139533314 source-id true]
  [1001 40 #edb/ref 23 13194139533314 source-id true]
  [1001 41 #edb/ref 35 13194139533314 source-id true]
  [1001 62 "The title of the movie" 13194139533314 source-id true]
  [1002 10 :movie/release-year 13194139533314 source-id true]
  [1002 40 #edb/ref 22 13194139533314 source-id true]
  [1002 41 #edb/ref 35 13194139533314 source-id true]
  [1002 62 "The year the movie was released in theaters" 13194139533314 source-id true]
  [13194139533314 50 #inst "2023-11-14T22:13:21.000Z" 13194139533314 source-id true]]
 :edb/tempids {"__map/00000000000000000000" 1000
              "__map/00000000000000000001" 1001
              "__map/00000000000000000002" 1002}}
```

Each datom is `[entity attribute value transaction source added?]`. The report
contains the three attribute definitions, the automatically generated schema
installation facts, and the automatic transaction time, all with the supplied
source. `#edb/ref` marks an entity reference.
Because we omitted `:db/id` from the attribute maps, EDB generated temporary IDs;
their names and allocation order are implementation details.

`:edb/committed true` confirms that the transaction was persisted. The
`:edb/db-before-t` and `:edb/db-after-t` values identify the database versions
before and after the transaction. The Rust report exposes the corresponding
immutable values as `db_before` and `db_after`. A confirmed commit remains
committed even if opening or rendering its local report fails, in which case
the executable returns `:edb/report-error` with the receipt fields.

Keep `tutorial-movie-schema`, the same source, and the same file contents when
retrying this transaction after a lost reply. Use a new request key for a new
transaction. You can learn more about the return value and retries in the
[transaction tutorial](00_edn_workflow.md#transactions-and-exact-retries).

The schema is now ready for [adding movies into the database](07_transact_data.md). Submit movie data
in a subsequent transaction: attribute names resolve against the database value
before each transaction, so newly installed attributes must already be present
when the movie transaction begins.
