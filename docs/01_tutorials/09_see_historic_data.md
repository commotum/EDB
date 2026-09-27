# See Historic Data

Previously: you learned [how to query domain data in the database](08_query_the_data.md).
This tutorial expects that you have already [connected to](05_connect_to_a_database.md)
the database `edn-demo`, [installed the movie schema](06_transact_schema.md), and
[added three movies](07_transact_data.md). Continue the Rust program from the
query tutorial, keeping its `conn`, `db`, `all_data_from_1985`, and `q`/`show`
helpers. Add the snippets below inside `main`, before its final `Ok(())`.
Keep the transactor running, `EDB_POSTGRES_URL` set to your peer connection,
and `EDB_TUTORIAL_ENDPOINT` set to its private socket path.

The output below assumes your first pass through this update. On a later run,
the captured `db` may already contain the new genre, and the same request key
replays the original transaction receipt.

You've done all the basics—created, defined, and populated a database, and
learned how to ask it some questions. Now, we will explore the chronological
side of EDB.

You've created a database with three movies in it. One of them, "Commando", is
assigned the "action/adventure" genre. Now, imagine the MPAA has updated its
approved list of genres, and added "future governor", for movies starring a
future governor. Obviously, Commando needs to be updated.

- To change the value of the `:movie/genre` attribute, you'll first need to find
  the entity ID of Commando:

  ```rust
  show(&q(r#"[:find ?e :where [?e :movie/title "Commando"]]"#, &db)?)?;
  ```

  ```edn
  #{[#edb/ref 17592186045419]}
  ```

Entity IDs, transaction IDs, and times in the outputs are illustrative; your
database may allocate different IDs. Query relations have no row-order guarantee.

- Bind the found entity ID to a local variable so you can use it in your
  subsequent transactions. The scalar find form, `:find ?e .`, returns the
  value directly:

  ```rust
  use edb_core::{QueryValue, TransactionRequest, Value};
  use edb_core::edn_value::transaction_report_to_edn;
  use std::time::Duration;

  let commando = q(r#"[:find ?e . :where [?e :movie/title "Commando"]]"#, &db)?;
  let QueryResult::Scalar(Some(QueryValue::Scalar(Value::Ref(commando_id)))) =
      commando
  else {
      return Err("expected Commando's entity ID".into());
  };
  ```

- Next, issue a transaction telling EDB about the new value for `:movie/genre`.

Remember back to the [transact data](07_transact_data.md) tutorial that a
transaction requires an active connection and a collection of transaction data.
In this case, we will specify one very special key, `:db/id`, to which we will
bind the entity ID we just retrieved. The transaction looks like this:

```rust
let update = format!(
    r#"[{{:db/id {commando_id} :movie/genre "future governor"}}]"#
);
let endpoint = std::env::var("EDB_TUTORIAL_ENDPOINT")?;
let request = TransactionRequest::from_edn("tutorial-commando-genre", &update)?;
let committed = conn.transact_socket(&endpoint, request, Duration::from_secs(30))?;
println!("Committed at basis {}, replayed={}", committed.basis_t, committed.replayed);
let report = committed.report?;
println!("{}", write_edn(&transaction_report_to_edn(
    report.db_before.basis_t(),
    report.db_after.basis_t(),
    &report.tx_data,
    &report.tempids,
)?)?);
```

```edn
{:edb/db-before-t 3
 :edb/db-after-t 4
 :edb/tx-data
 [[13194139533316 50 #inst "2023-11-14T22:13:23.000Z" 13194139533316 true]
  [17592186045419 1000 "action/adventure" 13194139533316 false]
  [17592186045419 1000 "future governor" 13194139533316 true]]
 :edb/tempids {}}
```

The report contains an assertion of `"future governor"` and a retraction of
`"action/adventure"`. Because `:movie/genre` has cardinality one, asserting the
new genre replaces the old genre in the current view.

Keep `tutorial-commando-genre` and the same transaction data when retrying after
a lost reply. A confirmed commit remains committed even if opening or rendering
its report fails. See [transactions and exact retries](00_edn_workflow.md#transactions-and-exact-retries).

- The transaction succeeded. Let's verify if Commando has been updated:

  ```rust
  show(&q(all_data_from_1985, &db)?)?;
  ```

  ```edn
  #{["Commando" 1985 "action/adventure"]
    ["The Goonies" 1985 "action/adventure"]}
  ```

WAIT? What happened? You saw that your transaction succeeded, but when you asked
the database, it still shows "action/adventure" for Commando. Is something wrong?

Absolutely not. Remember this from the "querying the data" tutorial:

> A database value is the state of the database at a given point in time. You
> can issue as many of these queries against that database value as you want,
> and they will always return the same results.

We issued our transaction against a *connection*, but we are issuing queries
against a *database value*, which is a snapshot as of a point in time. And we
captured that database value in a variable called `db`, which we pass into the
query as the final argument. It doesn't matter that our transaction succeeded—we
aren't seeing the new data because we are querying against the old database value.

- Get the database value from the successful transaction, then issue your query
  again:

  ```rust
  let new_db = report.db_after.clone();
  show(&q(all_data_from_1985, &new_db)?)?;
  ```

  ```edn
  #{["Commando" 1985 "future governor"]
    ["The Goonies" 1985 "action/adventure"]}
  ```

`report.db_after` gives us the exact version produced by this transaction.
`conn.db()` captures the connection's most recently adopted value, which may
already include later transactions; `conn.sync()?` refreshes from the durable
head visible at that call. None of these operations changes the old `db`.

Great, now we see "Commando" **has** been updated to the latest imaginary MPAA
specification. What you have seen here is half of the immutability story. As long
as you are holding onto a database value and issuing queries against it, you
will see the data as of a single point in time. To see newer values, capture a
newer database value.

- But what if you are at the latest value, and want to see where you came from?
  You have to get another view of the database value, at a time before your last
  transaction. To do this, you have to know the "time basis" you want. EDB can use
  a transaction entity ID, a logical transaction number `t`, or an instant to
  establish a time boundary. Remember the results from issuing the transaction
  where you transacted the first movies:

  ```edn
  {:edb/db-before-t 2
   :edb/db-after-t 3
   :edb/tx-data
   [[13194139533315 50 #inst "2023-11-14T22:13:22.000Z" 13194139533315 true]
    [17592186045419 1000 "action/adventure" 13194139533315 true]
    [17592186045419 1001 "Commando" 13194139533315 true]
    [17592186045419 1002 1985 13194139533315 true]
    [17592186045420 1000 "action/adventure" 13194139533315 true]
    [17592186045420 1001 "The Goonies" 13194139533315 true]
    [17592186045420 1002 1985 13194139533315 true]
    [17592186045421 1000 "punk dystopia" 13194139533315 true]
    [17592186045421 1001 "Repo Man" 13194139533315 true]
    [17592186045421 1002 1984 13194139533315 true]]
   :edb/tempids {"__map/00000000000000000000" 17592186045419
                "__map/00000000000000000001" 17592186045420
                "__map/00000000000000000002" 17592186045421}}
  ```

In the `:edb/tx-data` collection, the transaction's own `:db/txInstant` datom has
the transaction entity ID as its first number (`13194139533315` here). Every
datom also carries that transaction ID in its fourth position. The
`:edb/db-after-t` field identifies the same transaction's logical number, `3`.
These are distinct representations of the point we'd like to look at—the one
before we updated the genre of "Commando".

We kept `db` from before the update, so we can obtain its actual logical basis
without copying an illustrative number from this page:

```rust
let movies_t = db.basis_t();
```

- To grab a view of a different time basis, use `as_of`, which takes a logical
  transaction number:

  ```rust
  let old_db = new_db.clone().as_of(movies_t);
  show(&q(all_data_from_1985, &old_db)?)?;
  ```

  ```edn
  #{["Commando" 1985 "action/adventure"]
    ["The Goonies" 1985 "action/adventure"]}
  ```

Now we have a database view from the past, including only changes up until that
point in time. When issuing the same query against it, the value of
`:movie/genre` from before our transaction is displayed. The boundary is
inclusive. The view retains its underlying basis and schema; inspect
`old_db.as_of_t()` for the read boundary rather than `old_db.basis_t()`.

For a transaction entity ID, use
`as_of_time_point(edb_core::TimePoint::Tx(tx_id))?`. For a date, use
`as_of_time_point(edb_core::TimePoint::Instant(millis))?`, with Unix epoch
milliseconds. Logical `t` and transaction IDs are exact; instants may not
distinguish transactions within the same millisecond.

You can also use `since(t)` instead of `as_of(t)`, which restricts visible facts
to assertions from transactions after that point in time. Its boundary is
exclusive. Older identifying facts may then be absent from joins; use history
or the log when you need both additions and retractions.

- Finally, if you want to see all the values that a given attribute has held
  over time, access a special view on the database value, called *history*.

To get it, call `history` on your newer database value:

```rust
let hdb = new_db.clone().history();
```

- Now pass that into your query instead of `db`, and voila:

  ```rust
  show(&q(r#"[:find ?genre
              :where [?e :movie/title "Commando"]
                     [?e :movie/genre ?genre]]"#, &hdb)?)?;
  ```

  ```edn
  #{["action/adventure"] ["future governor"]}
  ```

You can see that the `:movie/genre` attribute of "Commando" has held two
different values over time, "action/adventure" and "future governor". History
includes retained assertions and retractions; this query projects only genre
values, so each distinct value appears once. History remains bounded by the
captured database value. There is much more you can do with `as_of`, `since`, and
`history`, including their retention limits, which you can learn more about in
[the filters reference](../02_core_concepts/03_database_filters.md).

Run the assembled program with `cargo run`. Congratulations, you've finished a
whirlwind tour of using EDB.
