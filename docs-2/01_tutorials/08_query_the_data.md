# Query the Data

Previously: you learned [how to transact domain data into the database](07_transact_data.md).
This section expects that you have connected to the database `edn-demo`,
transacted the movie schema, and added three movies. Keep `EDB_POSTGRES_URL`
set to the **peer** connection for that database.

You have transacted three sets of attributes into the database, creating three
movies. What you actually created were three **entities**, each of which has a
unique ID and a collection of associated attributes. The unique IDs were
assigned for you when you transacted the data (for more on entity IDs, see
[identity and uniqueness](../03_schema/05_identity_and_uniqueness.md)).

When you query an EDB database, you may be looking to retrieve attribute values,
collections of attribute values, entities, or any combination thereof. The peer
library offers three mechanisms for retrieving data from your database:

- **Query**: uses Datalog, a declarative query language.
- **Pull**: a declarative way to make hierarchical (and possibly nested)
  selections of information about entities.
- **Entity**: provides a lazy, associative view of information reachable from an
  entity ID.

We'll focus on query in this tutorial. For more on Pull, read the
[Pull reference](../05_query_and_pull/06_pull_reference.md), and for more on
Entity, read [entities](../02_core_concepts/04_entities.md).

## Database Value

A database value is the state of the database at a given point in time.

You must retrieve a database value to issue a query against an EDB database.
You can issue as many of these queries against that database value as you want,
and they will always return the same results.

We'll use the Rust peer library so we can retain that value across queries and
the update in the next tutorial. Each separate `edb query` command opens a
connection and captures a database value for that invocation; it does not retain
a Rust variable between commands.

With the [peer library](../00_start_here/02_peer_library.md) available in your
Rust project, put these imports and two helpers in `src/main.rs`. `q` parses
query data and runs it against a captured database value; `show` prints the
result as EDN:

```rust
use edb_core::{
    Connection, DatabaseValue, QueryControl, QueryResult, QuerySourceValue,
    SemanticError, postgres_config_from_env,
};
use edb_core::edn::write_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::edn_value::query_result_to_edn;

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

Open a connection, retrieve its current database value, and store it in a
variable:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_configured(
        postgres_config_from_env()?, "edn-demo", 64,
    )?;
    let db = conn.db();
```

Keep `main` open until the final snippet on this page. The snippets below are
successive parts of the same program. `db` retains this database value even if
the connection later observes new transactions.

## Query

Once you have the database value, you can issue queries against it. You issue
queries via our `q` helper, which takes two parameters:

- Query data, represented here as EDN text containing a `:find` clause and a
  `:where` clause.
- The database value used as the query's input source.

The helper parses the query, binds its input source, and executes it. The query
data can also contain a variety of other optional components, which you can
learn more about in the [query reference](../05_query_and_pull/05_query_reference.md).

Provide enough of a `:where` clause to limit the results. Selective clauses can
limit the work needed by related clauses. EDB schedules ready clauses according
to their bindings, so their written order does not guarantee execution order.

Right now we want to find every movie in the database. A movie is anything that
has an associated `:movie/title` attribute (or `:movie/release-year` or
`:movie/genre`, but we'll just use `:movie/title` for now).

You will use the vector form of the query data. Let's look at a minimal query:

```rust
    let all_movies_q = "[:find ?e :where [?e :movie/title]]";
```

Here we define a variable, `all_movies_q`, that holds our query definition,
which we will later pass to `q`. Look at the two clauses:

- `:find` specifies what you want to be returned from the query. In this case,
  `?e` is a logic variable that will be bound within the `:where` clause.
- `:where` consists of data patterns. In our case, we are only passing one
  pattern, `[?e :movie/title]`, which translates to "bind the ID of each entity
  that has an attribute called `:movie/title` to the logic variable named `?e`".

The whole query reads as "find me the IDs of all entities which have an
attribute called `:movie/title`".

To issue the query, call `q` and pass the query and the database value you
captured in the first part of this tutorial:

```rust
    show(&q(all_movies_q, &db)?)?;
```

The result looks like this; the assigned IDs depend on your database:

```edn
#{[#edb/ref 17592186045419]
  [#edb/ref 17592186045420]
  [#edb/ref 17592186045421]}
```

Note that there were three entities in the database with a `:movie/title`,
which maps to the three movies you added in the last tutorial. What you receive
back is a collection of return values, each of which in this case is just the
entity ID. EDB renders entity references as `#edb/ref` followed by the numeric
ID. A relation is an unordered set of tuples; printed row order is not part of
the result.

Instead of finding the entity IDs, perhaps you just want to find all the titles
of all the movies in the database.

In that case, your query would specify a `:find` clause with a named value
(`?movie-title`) that you bind to an attribute (`:movie/title`). That would look
like this:

```rust
    let all_titles_q =
        "[:find ?movie-title :where [_ :movie/title ?movie-title]]";
```

Note the underscore at the beginning of the pattern where `?e` used to be.
That indicates that we are not interested in the entity ID itself. We are only
interested in the existence (and value) of the `:movie/title` attribute. The
query reads as "find all movie titles from any entity that has an attribute
`:movie/title` and assign the title to a logic variable called `?movie-title`".

To execute the query, pass it and the database value in as before:

```rust
    show(&q(all_titles_q, &db)?)?;
```

```edn
#{["Commando"] ["The Goonies"] ["Repo Man"]}
```

You were able to query the attributes from a collection of all the movies in
the database. You received a collection of return values, and each contains the
string title of the movie.

What if you only want the titles of movies released in 1985? To query for
titles of movies released in 1985, your `:where` statement needs to have two
patterns: one to bind the `:movie/title` attribute (as before), and one to filter
by `:movie/release-year`.

Those two patterns have to be **joined**.

The `:where` clause in our last example contained an `_` where we first saw
`?e`, because we were not interested in the entity itself. We were only
interested in the value of the `:movie/title` attribute.

For that reason, we will reinsert `?e` into both patterns, and that will serve
as the join point. Datalog joins are created by the presence of the same logic
variable in multiple clauses. The new query looks like this:

```rust
    let titles_from_1985 = "[:find ?title
                            :where [?e :movie/title ?title]
                                   [?e :movie/release-year 1985]]";
```

This query reads: "find the title of any entity that has a `:movie/title`
attribute and whose `:movie/release-year` is 1985". When you issue the new query:

```rust
    show(&q(titles_from_1985, &db)?)?;
```

```edn
#{["Commando"] ["The Goonies"]}
```

What if you want to return the title, release year, and genre of all movies
that have values for all three attributes and a release year of 1985?

You need a pattern for each attribute you want to return and for the filter on
release year, all of which should be joined through `?e`. That query looks like
this:

```rust
    let all_data_from_1985 = "[:find ?title ?year ?genre
                              :where [?e :movie/title ?title]
                                     [?e :movie/release-year ?year]
                                     [?e :movie/genre ?genre]
                                     [?e :movie/release-year 1985]]";
```

And when you run it:

```rust
    show(&q(all_data_from_1985, &db)?)?;
```

```edn
#{["The Goonies" 1985 "action/adventure"]
  ["Commando" 1985 "action/adventure"]}
```

Your return value contains multiple tuples — one per movie found. The shape of
the relation returned is defined by your `:find` specification. More information
can be found in the [query reference](../05_query_and_pull/05_query_reference.md).
There is much more to learn about Datalog and query, from nested queries to
additional inputs and more.

Close `main` to run the program with `cargo run`:

```rust
    // Add the next tutorial's snippets here when continuing to history.
    Ok(())
}
```

In the final step of this tutorial, you will learn about the
[history of values in the database](09_see_historic_data.md). Add its snippets
before `Ok(())` so the queries and the update use the same connection and retain
the original `db` value in one run.
