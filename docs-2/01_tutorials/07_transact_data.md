# Transact Data

You have learned [how to transact a schema into the database](06_transact_schema.md).
This section expects that you have already connected to the database `edn-demo`
and transacted `movie-schema.edn`. Leave the transactor running, and keep
`EDB_POSTGRES_URL` set to your **peer** connection and `EDB_TUTORIAL_ENDPOINT`
set to the transactor's private socket path in your application terminal.
Run the shell commands from the root of your EDB checkout.

In the previous step, you learned how to transact schema data into the database.
As was mentioned there, schema is just data, like any other data. So you will be
unsurprised to learn that creating domain data is a very similar operation.

Putting domain data into your database also takes advantage of the executable's
`transact` command. It opens an active peer connection and submits a collection
of [transaction data](../04_transactions/04_transaction_data.md) to the running
transactor.

- Define your data in a file called `first-movies.edn`:

  ```sh
  cat > first-movies.edn <<'EDN'
  [{:db/source :db.source/system
    :movie/title "The Goonies"
    :movie/genre "action/adventure"
    :movie/release-year 1985}
   {:db/source :db.source/system
    :movie/title "Commando"
    :movie/genre "action/adventure"
    :movie/release-year 1985}
   {:db/source :db.source/system
    :movie/title "Repo Man"
    :movie/genre "punk dystopia"
    :movie/release-year 1984}
   {:db/id "edb.tx" :db/source :db.source/system}]
  EDN
  ```

Each movie map explicitly uses the built-in `:db.source/system` entity as its
source. The final map supplies the source for the generated transaction timestamp.

This is an example of the power of working directly with data—anything you might
do to assemble a collection of movie maps could be substituted for the file
`first-movies.edn`. You can also pass that data to intermediate functions for
cleaning/pre-processing before transacting.

- Now that you have the movies ready to go, transact them into the database:

  ```sh
  target/release/edb transact --database edn-demo \
    --endpoint "$EDB_TUTORIAL_ENDPOINT" \
    --request-key tutorial-first-movies --file first-movies.edn
  ```

The executable prints a commit receipt with `:edb/committed true`,
`:edb/basis-t`, `:edb/tx-hash`, and `:edb/replayed`, followed by the report fields
below. This illustrative report assumes the "Hello world" and schema
transactions were the first two transactions; IDs, time and basis depend on your
database. Here `source-id` stands for the entity ID of `:db.source/system`:

```text
{:edb/db-before-t 2
 :edb/db-after-t 3
 :edb/tx-data
 [[13194139533315 50 #inst "2023-11-14T22:13:22.000Z" 13194139533315 source-id true]
  [17592186045419 1000 "action/adventure" 13194139533315 source-id true]
  [17592186045419 1001 "Commando" 13194139533315 source-id true]
  [17592186045419 1002 1985 13194139533315 source-id true]
  [17592186045420 1000 "action/adventure" 13194139533315 source-id true]
  [17592186045420 1001 "The Goonies" 13194139533315 source-id true]
  [17592186045420 1002 1985 13194139533315 source-id true]
  [17592186045421 1000 "punk dystopia" 13194139533315 source-id true]
  [17592186045421 1001 "Repo Man" 13194139533315 source-id true]
  [17592186045421 1002 1984 13194139533315 source-id true]]
 :edb/tempids {"__map/00000000000000000000" 17592186045419
              "__map/00000000000000000001" 17592186045420
              "__map/00000000000000000002" 17592186045421}}
```

Once again, the response you get back contains a `:edb/db-before-t` and a
`:edb/db-after-t`, identifying the immutable database versions before and after
the transaction. Note that it lists out all the actual data that was added to
the database in the `:edb/tx-data` key. Each datom is
`[entity attribute value transaction source added?]`.

The three movie maps omitted `:db/id`, so EDB generated temporary IDs and returned
their entity IDs in `:edb/tempids`. Generated names and allocation order are
implementation details; do not assume they match the order of your input maps.

Keep `tutorial-first-movies` and the same file contents when retrying this
transaction after a lost reply. Use a new key for a new transaction. Submitting
these maps again with a new key creates three more movies: none of these
attributes declares a unique identity. A confirmed commit remains committed
even if its local report cannot be opened or rendered, in which case the receipt
includes `:edb/report-error`. See
[transactions and exact retries](00_edn_workflow.md#transactions-and-exact-retries).

Now you have three movies in the database. The next step is to learn how to query
for them; continue with [querying your data](08_query_the_data.md).
