# In-Memory Database Getting Started

This page contains a short guide that explores the most common operations of a
database using the native Rust library and a local in-memory database. You will
create a database, transact schema and data, query it, and explore its history.

## Creating a Database

For this example, a local in-memory database will be used, which does not require
PostgreSQL or a transactor to run. `Database::bootstrap()` creates an immutable
value containing EDB's built-in schema. Each local transaction returns a new
database value, which you retain for subsequent operations. These values live in
your process; dropping them does not leave a persistent database to reconnect to.

For steps on using a durable database and running a transactor, see the
[application workflow](01_application_workflow.md).

Throughout this tutorial, Rust snippets are successive parts of one program.
Create a Rust application with `cargo new hello-edb`, and add the local EDB
checkout to its `Cargo.toml` dependencies, adjusting the path:

```toml
[dependencies]
edb-core = { path = "/path/to/EDB" }
```

Put these imports and two small helpers in `src/main.rs`. `q` parses query data
and runs it against a captured database value; `show` prints its result as EDN.
The tutorial will explain the queries as we go.

```rust
use edb_core::{
    Database, DatabaseValue, QueryControl, QueryResult, QuerySourceValue,
    QueryValue, SemanticError, Value,
};
use edb_core::edn::write_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::edn_value::{query_result_to_edn, transaction_report_to_edn};

fn q(text: &str, db: &DatabaseValue) -> Result<QueryResult, SemanticError> {
    let query = parse_query_edn(text)?;
    let bound = query.bind(&[EdnQueryArgument::Source(
        QuerySourceValue::Database(db.clone()),
    )])?;
    Ok(bound.execute(&QueryControl::default(), None)?.result)
}

fn show(result: &QueryResult) -> Result<(), SemanticError> {
    println!("{}", write_edn(&query_result_to_edn(result)?)?);
    Ok(())
}
```

Start `main` and create the database:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let empty = Database::bootstrap()?;
    assert_eq!(empty.basis_t(), 0);
```

Keep `main` open until the final snippet in the tutorial. You can now use `empty`
as an input to future operations. The next step will be to define some schema
for your new database.

## Transacting Schema

This section expects that you have already created the database value `empty`.

One of the great things about EDB is that everything is data, including the
definition of the schema for your database. Because it is just data, it can be
created and manipulated just like any other data you put into the database. To
add new schema, you issue a transaction that carries the new schema definition.

We'll create a basic model for movies: a movie can have a title, a release year,
and a genre. Title and genre are represented as strings, while release year is
an integer. In a relational database, each of those (title, release year, and
genre) would be considered "fields" in a "table".

In EDB, each of those is an attribute, which can be associated with an entity.
To start adding movie data to our database, we'll have to tell it about those
three attributes.

Since schema is just data, you describe your custom attributes using EDB's
built-in attributes. Every attribute definition has three required attributes:

- `:db/ident` specifies a unique name for your attribute.
- `:db/valueType` specifies the type of data that can be stored in the attribute.
- `:db/cardinality` specifies whether the attribute stores a single value or many
  values.

Supply also the optional `:db/doc` attribute, which stores a docstring of the
attribute you can query later.

Attribute definitions are defined as maps of data. Our first attribute,
"title", would be defined as:

```edn
{:db/ident :movie/title
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one
 :db/doc "The title of the movie"}
```

Note that `:db.type/string` and `:db.cardinality/one` are built-in idents used as
values when defining attributes. Learn more in the
[schema reference](../03_schema/02_schema_reference.md).

Similarly, "genre" would be:

```edn
{:db/ident :movie/genre
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one
 :db/doc "The genre of the movie"}
```

Since "year" will be stored as a number, we'll have to change the `:db/valueType`
for it:

```edn
{:db/ident :movie/release-year
 :db/valueType :db.type/long
 :db/cardinality :db.cardinality/one
 :db/doc "The year the movie was released in theaters"}
```

You could choose to send each of these attributes to the database in individual
transactions, but we'll batch them up into a single transaction. To do that,
combine the three maps into a single vector of maps. Let's bind the EDN schema
text to a variable called `movie_schema`:

```rust
    let movie_schema = r#"[
        {:db/ident :movie/title
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
         :db/doc "The year the movie was released in theaters"}
    ]"#;
```

Now that we have our attribute definitions ready to go, issue a local transaction
using `with_edn`. This method takes two arguments:

- Transaction data as EDN text, formatted in
  [list or map form](../04_transactions/04_transaction_data.md).
- An explicit transaction instant in milliseconds since the Unix epoch.

The receiver is the immutable database value to which the transaction applies.
We use increasing example instants throughout this tutorial so that the history
is reproducible. Transact the schema:

```rust
    let schema_report = empty.with_edn(movie_schema, 1_000)?;
    println!("{}", write_edn(&transaction_report_to_edn(
        schema_report.db_before.basis_t(),
        schema_report.db_after.basis_t(),
        &schema_report.tx_data,
        &schema_report.tempids,
    )?)?);
    assert_eq!(schema_report.db_after.basis_t(), 1);
```

The returned `TxReport` contains:

| Field | Meaning |
| --- | --- |
| `db_before` | The database value before the transaction |
| `db_after` | The database value including the transaction |
| `tx_data` | The assertions and retractions made by the transaction |
| `tempids` | Temporary ID names mapped to assigned entity IDs |

The printed EDN report represents these as `:edb/db-before-t`,
`:edb/db-after-t`, `:edb/tx-data`, and `:edb/tempids`. The first two are basis
numbers, here `0` and `1`; the Rust report retains the database values themselves.
Each datom in the printed transaction data has five components:
`[entity attribute value transaction added?]`.

The presence of `db_before` and `db_after` shows the local state transition.
`with_edn` does not persist this information to storage. Retain `db_after` to
continue from it. Refer to [transactions](../04_transactions/00_transactions.md)
for more about local speculation and durable transaction reports.

Next, we'll start adding movies into the database.

## Transacting Data

This section expects that you have already created the database and transacted
the movie schema.

As was mentioned previously, schema is just data, like any other data. So you
will be unsurprised to learn that creating domain data is a very similar
operation to the creation and transaction of schema.

Once again, use `with_edn`, this time on the database value that includes the
schema. Start by defining your data, again storing it in a variable first:

```rust
    let first_movies = r#"[
        {:movie/title "The Goonies"
         :movie/genre "action/adventure"
         :movie/release-year 1985}
        {:movie/title "Commando"
         :movie/genre "action/adventure"
         :movie/release-year 1985}
        {:movie/title "Repo Man"
         :movie/genre "punk dystopia"
         :movie/release-year 1984}
    ]"#;
```

This is an example of the power of working directly with data — anything you
might do to assemble a map of movie data could be substituted for
`first_movies`. You can also pass that data to intermediate functions for
cleaning or preprocessing before transacting. EDN is the text representation
used here; the [native transaction APIs](../04_transactions/04_transaction_data.md)
also accept typed data assembled directly in Rust.

Now that you have the movies ready to go, transact them into the database:

```rust
    let movies_report = schema_report.db_after.with_edn(first_movies, 2_000)?;
    println!("{}", write_edn(&transaction_report_to_edn(
        movies_report.db_before.basis_t(),
        movies_report.db_after.basis_t(),
        &movies_report.tx_data,
        &movies_report.tempids,
    )?)?);
    let movies_t = movies_report.db_after.basis_t();
    assert_eq!(movies_t, 2);
```

Once again, the response contains `db_before` and `db_after`. The `tx_data`
field lists the actual data added to the database, including the transaction's
own `:db/txInstant` assertion and the title, genre, and year for each movie.
The database assigns identities to the three anonymous movie maps.

So, now you have three movies in the database. Next you will learn how to query
for them.

## Query Data

This section expects that you have already transacted the movie schema and
three movies.

Previously, you transacted three sets of attributes into the database, creating
three movies. What you actually created were three **entities**, each of which
has a unique ID and a collection of associated attributes. The
[unique IDs](../03_schema/05_identity_and_uniqueness.md) were assigned for you
when you transacted the data. When you query an EDB database, you may be looking
to retrieve attribute values, collections of attribute values, entities, or any
combination thereof. The library offers three mechanisms for retrieving data
from your database:

- **Query** uses Datalog, a declarative query language.
- **Pull** is a declarative way to make hierarchical (and possibly nested)
  selections of information about entities.
- **Entity** allows you, given an entity ID, to obtain a lazy, associative view
  of the information reachable from that entity ID.

We'll focus on query in this tutorial. For more on Pull, read the
[Pull reference](../05_query_and_pull/06_pull_reference.md), and for more on
Entity, read [entities](../02_core_concepts/04_entities.md).

First, capture the database value to query. A database value is the state of the
database at a given point in time. You can issue as many queries against that
database value as you want; they will always run against the database as of the
same point in time.

```rust
    let db = movies_report.db_after.database_value();
```

Once you have the database value, you can issue queries against it. Our `q`
helper takes two arguments:

- Query data as EDN text, containing a `:find` clause and a `:where` clause in
  these examples.
- The database value used as the query's input source.

The helper parses the query, binds the database source, and executes it with
default query controls. Query data can also contain a variety of other optional
components, which you can learn more about in the
[query reference](../05_query_and_pull/05_query_reference.md).

When crafting your Datalog query, provide enough of a `:where` clause to limit
the results. So the question is, how do we ask for all the movies in our
database? In our schema, a movie is anything that has an associated
`:movie/title` attribute (or `:movie/release-year` or `:movie/genre`, but we'll
just use `:movie/title` for now).

In this section, use the vector form of the query data. Let's look at a minimal
query:

```rust
    let all_movies_q = "[:find ?e :where [?e :movie/title]]";
```

Here we define a variable, `all_movies_q`, that holds our query, which later
will be passed to `q`. Look at the two clauses:

- `:find` specifies what you want returned from the query. In this case, `?e`
  is a logic variable that will be bound within the `:where` clause.
- `:where` contains data patterns. In our case, we are only passing one pattern,
  `[?e :movie/title]`, which translates to "bind the ID of each entity that has
  an attribute called `:movie/title` to the logic variable named `?e`".

So, the whole query reads as "find me the IDs of all entities which have an
attribute called `:movie/title`".

To actually issue the query, call `q` and pass the query and the database value:

```rust
    show(&q(all_movies_q, &db)?)?;
```

For this fresh database, the result is:

```edn
#{[#edb/ref 17592186045419]
  [#edb/ref 17592186045420]
  [#edb/ref 17592186045421]}
```

You receive a set of three one-element tuples, each containing an entity ID
rendered as `#edb/ref` followed by its numeric ID. The exact IDs are assigned by
the database; the examples below obtain them by query instead of hard-coding
numbers. Relations are unordered, so the order of printed rows is not part of
the result.

Notice that there were three entities in the database with a `:movie/title`,
corresponding to the three movies you added in the last section. What you
received back is a collection of return values, each of which in this case is
just the entity ID. Once you have entity IDs, use them to find out other data
about the given [entity](../02_core_concepts/04_entities.md).

Instead of finding the entity IDs, perhaps you just want to find all the titles
of all the movies in the database. In that case, your query would specify a
`:find` clause with a named value, `?movie-title`, that you bind to an attribute,
`:movie/title`. It looks like this:

```rust
    let all_titles_q =
        "[:find ?movie-title :where [_ :movie/title ?movie-title]]";
    show(&q(all_titles_q, &db)?)?;
```

```edn
#{["Commando"] ["The Goonies"] ["Repo Man"]}
```

Notice the underscore at the beginning of the `:where` pattern where `?e` used
to be. That indicates that we are not interested in the entity ID itself, just
the existence (and value) of the `:movie/title` attribute. The query reads "find
all movie titles from any entity that has an attribute `:movie/title` and
assign the title to a logic variable called `?movie-title`".

You were able to query for the attributes from a collection of all the movies
in the database. Again, you received a collection of return values, each of
which this time is the string title of the movie.

But what if you only want the titles of movies released in 1985? Your `:where`
clause will need two patterns, one to bind the `:movie/title` attribute (as
before), and one to filter by `:movie/release-year`. Those two patterns have to
be **joined**.

Remember in the last example, our pattern contained an `_` where we first saw
`?e`, because we were not interested in the entity ID itself, only the value of
the `:movie/title` attribute. Now, reinsert `?e` into both patterns, and that
will serve as the join point. In Datalog, joins are created implicitly by the
presence of the same logic variable in multiple clauses.

The new query looks like this:

```rust
    let titles_from_1985 = "[:find ?title
                            :where [?e :movie/title ?title]
                                   [?e :movie/release-year 1985]]";
    show(&q(titles_from_1985, &db)?)?;
```

```edn
#{["Commando"] ["The Goonies"]}
```

That reads: "find the title of any entity that has a `:movie/title` attribute
and whose `:movie/release-year` is 1985".

Finally, what if you want to return all three movie attributes for each movie
released in 1985? You need a pattern for each attribute you want to return and
for the filter on release year, all of which should be joined through `?e`.
That query looks like:

```rust
    let all_data_from_1985 = "[:find ?e ?title ?year ?genre
                              :where [?e :movie/title ?title]
                                     [?e :movie/release-year ?year]
                                     [?e :movie/genre ?genre]
                                     [?e :movie/release-year 1985]]";
    show(&q(all_data_from_1985, &db)?)?;
```

```edn
#{[#edb/ref 17592186045419 "Commando" 1985 "action/adventure"]
  [#edb/ref 17592186045420 "The Goonies" 1985 "action/adventure"]}
```

There are two tuples: one containing Commando's ID, `"Commando"`, `1985`, and
`"action/adventure"`; the other containing The Goonies' ID, `"The Goonies"`,
`1985`, and `"action/adventure"`.

Notice that this time, each returned tuple contains multiple values — one per
logic variable in the `:find` specification. The shape of the relations returned
is defined by your `:find` specification. There is much more to learn about
Datalog, from nested queries to secondary inputs and more. You can find out
about it in the [query reference](../05_query_and_pull/05_query_reference.md).

Next you will learn about the history of values in the database.

## History of Values in the Database

This section expects that you have already transacted the movie schema and
three movies and captured their database value in `db`.

You've done all the basics — created, defined and populated a database, and
learned how to ask it some questions. Now, we will explore the chronological
side of EDB.

You've created a database with three movies in it. One of them, "Commando", is
assigned the "action/adventure" genre. Now, imagine the MPAA has updated its
approved list of genres, and added "future governor", for movies starring a
future governor. Obviously, Commando needs to be updated.

To change the value of the `:movie/genre` attribute, first find the entity ID of
Commando:

```rust
    show(&q(r#"[:find ?e :where [?e :movie/title "Commando"]]"#, &db)?)?;
```

```edn
#{[#edb/ref 17592186045419]}
```

Bind the found entity ID to a local variable so you can use it in subsequent
transactions. A scalar `:find ?e .` returns the single value directly; matching
the native result also checks that the query returned an entity reference:

```rust
    let commando = q(r#"[:find ?e . :where [?e :movie/title "Commando"]]"#, &db)?;
    let QueryResult::Scalar(Some(QueryValue::Scalar(Value::Ref(commando_id)))) =
        commando
    else {
        return Err("expected Commando's entity ID".into());
    };
```

Next, issue a transaction telling EDB about the new value for `:movie/genre`.
Remember from the transacting data section that a local transaction takes a
database value, transaction data, and a transaction instant. In this case,
specify one special key, `:db/id`, whose value is the entity ID we just
retrieved. The transaction looks like this:

```rust
    let update = format!(
        r#"[{{:db/id {commando_id} :movie/genre "future governor"}}]"#
    );
    let update_report = movies_report.db_after.with_edn(&update, 3_000)?;
    println!("{}", write_edn(&transaction_report_to_edn(
        update_report.db_before.basis_t(),
        update_report.db_after.basis_t(),
        &update_report.tx_data,
        &update_report.tempids,
    )?)?);
```

```edn
{:edb/db-before-t 2
 :edb/db-after-t 3
 :edb/tx-data
 [[13194139533315 50 #inst "1970-01-01T00:00:03.000Z" 13194139533315 true]
  [17592186045419 1000 "action/adventure" 13194139533315 false]
  [17592186045419 1000 "future governor" 13194139533315 true]]
 :edb/tempids {}}
```

The report includes an assertion of `"future governor"` and a retraction of
`"action/adventure"` for the same entity and attribute, along with the
transaction instant. Because `:movie/genre` has cardinality one, asserting the
new genre replaces the old genre in the current view.

The transaction succeeded. Let's verify whether Commando has been updated:

```rust
    show(&q(all_data_from_1985, &db)?)?;
```

```edn
#{[#edb/ref 17592186045419 "Commando" 1985 "action/adventure"]
  [#edb/ref 17592186045420 "The Goonies" 1985 "action/adventure"]}
```

This still shows `"action/adventure"` for both movies. What happened? You saw
that your transaction succeeded, but when you asked the database, it still
shows `"action/adventure"` for Commando. Is something wrong?

Absolutely not. Remember this from the query section:

> A database value is the state of the database at a given point in time. You
> can issue as many queries against that database value as you want; they will
> always run against the database as of the same point in time.

Our transaction returned a **new database value**, but we are issuing queries
against the **old database value**. We captured that value in a variable called
`db` and pass it into the query as the final argument. It doesn't matter that
our transaction succeeded — we aren't seeing the new data because we are
querying against the old database value.

Capture the value returned by the latest transaction and issue your query again:

```rust
    let db = update_report.db_after.database_value();
    show(&q(all_data_from_1985, &db)?)?;
```

```edn
#{[#edb/ref 17592186045419 "Commando" 1985 "future governor"]
  [#edb/ref 17592186045420 "The Goonies" 1985 "action/adventure"]}
```

Now Commando's tuple contains `"future governor"`; The Goonies still has
`"action/adventure"`. What you have seen here is half of the immutability story.
As long as you are holding onto a database value and issuing queries against
it, you will always see the data as of a single point in time. To see the latest
values, use the newer database value.

But what if you are at the latest value, and want to see where you came from?
You have to get another view of the database value, at a time before your last
transaction. To do this, you have to know the time boundary you want. EDB can
use a logical transaction number `t`, a transaction entity ID, or an instant in
milliseconds to establish a boundary.

Remember the report from transacting the first movies. Its `db_after.basis_t()`
was `2`, which we saved in `movies_t`. Its `tx_data` also contains the
transaction ID in each datom and the transaction's `:db/txInstant` assertion.
A logical `t` and a transaction entity ID are distinct representations; use
`as_of(t)` for the former or `as_of_time_point(TimePoint::Tx(tx_id))` for the
latter. `TimePoint::Instant(millis)` supplies an instant boundary.

In our case, `movies_t` represents the boundary we'd like to look at — the one
right before our current state, which was created when we updated the genre of
Commando. To grab a view as of that point, use `as_of`:

```rust
    let old_db = db.clone().as_of(movies_t);
    show(&q(all_data_from_1985, &old_db)?)?;
```

```edn
#{[#edb/ref 17592186045419 "Commando" 1985 "action/adventure"]
  [#edb/ref 17592186045420 "The Goonies" 1985 "action/adventure"]}
```

Now we have a database view from the past, including only changes up until that
point in time. Issuing the same query against it, we see the value of
`:movie/genre` from before our update: `"action/adventure"` for both movies.
The view retains the captured basis and schema; `as_of` changes its read
boundary, not the database from which it was derived.

You can also use `since(t)` instead of `as_of(t)`. A since view restricts visible
facts to assertions after the specified point in time. It is not a complete
snapshot of everything that existed then, so older facts needed for a join may
be absent. See [database filters](../02_core_concepts/03_database_filters.md).

To see all the values that a given attribute has held over time, access a
special view on the database value, called **history**. To get it, call
`history` on your existing database value:

```rust
    let hdb = db.clone().history();
```

Now pass that in to your query instead of `db`:

```rust
    let genres = "[:find ?genre
                   :where [?e :movie/title \"Commando\"]
                          [?e :movie/genre ?genre]]";
    show(&q(genres, &hdb)?)?;
    Ok(())
}
```

```edn
#{["action/adventure"] ["future governor"]}
```

Notice that the `:movie/genre` attribute of Commando has held two different
values over time, `"action/adventure"` and `"future governor"`. History includes
assertions and retractions; this query projects only genre values, so duplicate
values appear once. There is much more you can do with `as_of`, `since`, and
`history`, which you can learn about in the
[filters reference](../02_core_concepts/03_database_filters.md).

Run the assembled program with `cargo run`. Congratulations, you've finished a
whirlwind tour of using EDB. To continue with a durable database, explore the
[application workflow](01_application_workflow.md).
