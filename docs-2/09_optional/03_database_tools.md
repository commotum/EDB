# Database tools

EDB's command-line and native Rust interfaces provide tools for exploring schema,
building and executing queries, examining transactions, and reading indexes.
There is no graphical console or browser server. This page describes how to use
the tools together to explore a database.

## Starting the tools

Build the executable and display its commands:

```sh
cargo build --release --bin edb
target/release/edb --help
```

The examples below assume `edb` is on your `PATH`. For live databases, configure
`EDB_POSTGRES_URL` and the appropriate PostgreSQL credentials as described in
[the application workflow](../01_tutorials/01_application_workflow.md). Reads
execute in the command process; a transactor need not be running to query or
Pull existing data. Installation, database creation, and writer startup are
separate operations.

For a small database to explore, follow the
[EDN workflow](../01_tutorials/00_edn_workflow.md) to create `edn-demo` and transact
the supplied person schema and Alice/Bob data. The examples here use those
attributes. Query and Pull files contain EDN, so the same text can be used in
application code through the native EDN adapters.

## Specifying a database

Use the configured PostgreSQL storage and `--database` to specify a database to
work with. List its logical database names and inspect the selected basis:

```sh
edb list-databases --limit 100
edb status --database edn-demo
```

Listing is paginated; use `--after NAME` to continue after the last name in a
full page. `status` reports catalog state, not proof of a live transactor.
For an offline backup, select `--repository PATH` in place of `--database NAME`;
see [backup reads](../08_operations/05_backup_reads.md). Neither offline reads nor
queries over literal data require PostgreSQL configuration.

You can specify `--as-of T` or `--since T` to filter a database by time. The
as-of bound is inclusive; the since bound is exclusive. These command options
accept nonnegative logical transaction coordinates, not transaction entity IDs
or calendar dates. In Rust, `as_of_time_point` and `since_time_point` also accept
`TimePoint::Tx(id)` and `TimePoint::Instant(epoch_millis)`.

```sh
edb query --database edn-demo --as-of 2 --file names.edn
edb query --database edn-demo --since 1 --file names.edn
edb query --database edn-demo --history --file history.edn
```

The query files are defined below. Use `--history` to query retained assertions
and retractions. Pull accepts `--as-of` and `--since`, but not `--history`.
Temporal views retain the captured schema and ident dictionary; a since view
is a filtered value, not a complete account of all changes. See
[database filters](../02_core_concepts/03_database_filters.md) for composition
and history-retention details.

## Exploring schema

The attributes defined in the current database are themselves entities. Query
their names, value types, and cardinalities:

```sh
cat > schema.edn <<'EDN'
[:find ?ident ?type ?cardinality
 :where [?a :db/ident ?ident]
        [?a :db/valueType ?type-id]
        [?type-id :db/ident ?type]
        [?a :db/cardinality ?cardinality-id]
        [?cardinality-id :db/ident ?cardinality]]
EDN
edb query --database edn-demo --file schema.edn
```

Each result row contains an attribute keyword and its two properties. The
keyword groups an attribute name under its namespace: for example,
`:person/name` belongs to `person`. Results include built-in attributes as well
as application attributes. To examine all properties of one attribute, Pull its
entity by ident:

```sh
cat > all.edn <<'EDN'
[*]
EDN
edb pull --database edn-demo --file all.edn --entity ':person/email'
```

This also reveals properties such as uniqueness and indexing when present.
Reference-valued schema properties may be returned as entity references; use
the schema query above to resolve their names. See the
[schema reference](../03_schema/02_schema_reference.md) for property meanings.

For example, a music schema might group attributes under `abstractRelease` and
`artist`. Within `artist`, names such as `country`, `endDay`, `endMonth`,
`endYear`, `gender`, `gid`, `name`, and `sortName` are distinct attributes with
fully qualified idents such as `:artist/country` and `:artist/name`. Group the
schema query's returned keywords by namespace in your application to browse
them this way. These namespaces and attributes are illustrative; the supplied
`edn-demo` database does not install a music schema.

An attribute's documentation describes its use: for example, `:artist/country`
could have the documentation string "The artist's country of origin", and
`:artist/name` could have "The artist's name". The documentation belongs to the
attribute entity, independently of the values stored for individual artists.
To inspect a name attribute's properties, including readable names for its
reference-valued type and cardinality, use this explicit Pull pattern against
a database with that attribute installed:

```sh
cat > attribute.edn <<'EDN'
[:db/ident :db/doc :db/fulltext :db/index
 {:db/cardinality [:db/ident]}
 {:db/valueType [:db/ident]}]
EDN
edb pull --database music --file attribute.edn --entity ':artist/name'
```

For an attribute defined with these properties, the selected values are:

| Property | Illustrative value |
| --- | --- |
| `:db/doc` | `"The artist's name"` |
| `:db/fulltext` | `true` |
| `:db/index` | `true` |
| `:db/cardinality` | `{:db/ident :db.cardinality/one}` |
| `:db/valueType` | `{:db/ident :db.type/string}` |
| `:db/ident` | `:artist/name` |

This describes a single string value per artist, with fulltext search enabled
and AVET indexing requested. The two nested maps are Pull's representation of
the referenced schema entities. Pull selects stored properties; it does not
add absent optional properties with their schema defaults. Index intent also
does not establish physical index readiness; see the schema reference's
[index property](../03_schema/02_schema_reference.md#dbindex).

To collect stored documentation strings into a two-column data set, query
`:db/doc`:

```sh
cat > documentation.edn <<'EDN'
[:find ?e ?doc
 :in $
 :where [?e :db/doc ?doc]]
EDN
edb query --database edn-demo --file documentation.edn > documentation-result.edn
```

The `:in $` declaration binds the selected database to the default source used
by `[?e :db/doc ?doc]`. Omitting `:in` has the same effect as `:in $` for this
query. Here `--database edn-demo` supplies that source; you can
also bind `$` through `--sources`, as shown under [data sources](#data-sources).
There are no non-source arguments, so this query needs no `--inputs` file.

Each row pairs an entity reference with its documentation string. Only entities
with an asserted `:db/doc` appear; the supplied person schema does not assert
documentation strings. The row count and text depend on your database. Read
`documentation-result.edn` to inspect the saved relation, use a returned entity
ID with Pull, or reuse the relation as described under [data sources](#data-sources).

## Exploring data

### Query

Use `query` to build and execute queries. Edit the EDN query directly and save
it to a file:

```sh
cat > names.edn <<'EDN'
[:find ?e ?name
 :where [?e :person/name ?name]]
EDN
edb query --database edn-demo --file names.edn
```

- Use `:find` to specify the values or aggregates to return.
- Use `:with` to retain additional variables when determining the basis for
  aggregation.
- Use `:where` to specify one or more clauses. A datom pattern needs only as
  many components as the clause requires, starting on the left.
- Use `:in` to specify input bindings. Supply non-source values, in order, in
  the vector read by `--inputs`. Supply database or tuple sources with
  `--sources`, as described under [data sources](#data-sources).

`$` refers to the selected database by default. Here a scalar input selects
people by name:

```sh
cat > by-name.edn <<'EDN'
[:find ?e :in $ ?name :where [?e :person/name ?name]]
EDN
cat > name-input.edn <<'EDN'
["Alice"]
EDN
edb query --database edn-demo --file by-name.edn --inputs name-input.edn
```

The result is printed as EDN on stdout. Relation results have the shape
`#{[value ...] ...}`; tuple, scalar, and collection find specifications have
their corresponding result shapes. Relations have set semantics, with no
promised row order. Count rows or sort them in the consuming application; there
is no separate table caption or interactive sort control. Entity references in
results use `#edb/ref`; pass the corresponding ID to Pull to inspect the entity.

Save a result with shell redirection:

```sh
edb query --database edn-demo --file names.edn > names-result.edn
```

Saving the query file lets you reload or copy it into application code. Saving
the result records that execution's data; it does not refresh when the database
changes. `--query-stats` wraps results with diagnostics, so omit it when saving
a bare relation for later input. See
[query execution](../05_query_and_pull/07_executing_queries.md) for result shapes
and resource controls.

### Entities

Use `pull` to examine a particular entity. `--entity` accepts an entity ID,
`:db/ident` keyword, or lookup reference:

```sh
edb pull --database edn-demo --file all.edn \
  --entity '[:person/email "alice@example.com"]'
```

The entity's attributes and their values are returned in a map. If an attribute
refers to another entity, a nested Pull pattern can examine that entity. If the
entity is the value of a reference on another entity, a reverse attribute can
examine the referring entity. `[*]` includes `:db/id` and the entity's asserted
forward attributes; add reverse attributes explicitly to inspect incoming
references:

```sh
cat > graph.edn <<'EDN'
[:db/id :person/name
 {:person/friends [:db/id :person/name]}
 {:person/_friends [:db/id :person/name]}]
EDN
edb pull --database edn-demo --file graph.edn \
  --entity '[:person/email "bob@example.com"]'
```

In the supplied data Alice refers to Bob through `:person/friends`; Bob's reverse
`:person/_friends` result therefore includes Alice. This makes it possible to
traverse a graph independently of which side of a reference you start on. Extend
the pattern to choose the depth and attributes to inspect, or issue another Pull
using a returned ID. The [Pull reference](../05_query_and_pull/06_pull_reference.md)
describes recursion and limits; the native
[entity API](../07_peer_api/types/06_entity.md) also provides lazy access through
`Entity::get_named` and `Entity::get_direction`, with related entities tied to
the same database value.

For a music database with the following attributes installed, an artist such as
Foo Fighters can be inspected with this pattern. This is an illustrative schema;
the `edn-demo` tutorial does not install music attributes or data.

```edn
[:db/id :artist/gid :artist/name :artist/sortName :artist/startYear
 {:artist/country [:db/id :db/ident]}
 {:artist/type [:db/id :db/ident]}
 {:abstractRelease/_artists [:db/id]}
 {:release/_artists [:db/id]}
 {:track/_artists [:db/id]}]
```

Save the pattern to a file and pass the artist's entity ID from a query as
`--entity`. The scalar properties might include the name and sort name
`"Foo Fighters"`, a UUID for `:artist/gid`, and the start year `1994`. Here
`:artist/country` and `:artist/type` are references: the nested `:db/ident`
selection exposes identifiers such as `:country/US` and `:artist.type/group`
alongside their entity IDs. Ordinary non-component references without a nested
selection return ID maps, rather than automatically substituting an ident.

The three reverse selections find abstract releases, releases, and tracks that
refer to this artist through their respective `artists` attributes. For
non-component references these results are collections, even when only one
entity refers to the artist. Add names or other properties to a nested pattern
to inspect those entities. Pull returns at most 1,000 values by default for each
such collection; its length is therefore not necessarily the total number of
references. Use `[:track/_artists :limit nil]` in place of `:track/_artists`
as the map key to request all matching tracks, subject to execution resource
limits, or query with `(count ?track)` to obtain the count directly.

### Transactions

Use a history query to examine assertions and retractions of an attribute:

```sh
cat > history.edn <<'EDN'
[:find ?e ?name ?tx ?added
 :where [?e :person/name ?name ?tx _ ?added]]
EDN
edb query --database edn-demo --history --file history.edn
```

The transaction reference and boolean assertion flag distinguish when a datom
was asserted (`true`) or retracted (`false`). The `_` in the fifth position
skips the datom's source entity. Replace it with `?source` and add `?source` to
`:find` to return who asserted or retracted the fact.

To query retained transaction data in a time range, supply the log as a query
input source:

```sh
cat > log-source.edn <<'EDN'
{$log {:database "edn-demo" :log true}}
EDN
cat > transaction-window.edn <<'EDN'
[1 3]
EDN
cat > transactions.edn <<'EDN'
[:find ?tx ?e ?a ?v ?added
 :in $log ?start ?end
 :where [(tx-ids $log ?start ?end) [?tx ...]]
        [(tx-data $log ?tx) [[?e ?a ?v _ _ ?added]]]]
EDN
edb query --file transactions.edn --sources log-source.edn \
  --inputs transaction-window.edn
```

This selects T=1 and T=2: start is inclusive and end is exclusive. Replace the
two inputs with `#inst` values to select a day, hour, minute, or second in UTC,
for example `[#inst "2026-09-26T00:00:00Z" #inst "2026-09-27T00:00:00Z"]`.
An empty time window produces no rows. Narrow the bounds to examine a smaller
interval or move them forward/backward to examine adjacent intervals.

To count transactions in the window, use `:find (count ?tx)` with the `tx-ids`
clause alone. To count their datoms, bind the fifth `tx-data` column to `?source`
and use `:find ?tx (count ?e) :with ?a ?v ?source ?added` with both clauses.
These values can be plotted in an application; the CLI does not provide a
timeline graph. For an hour's transaction timeline, divide the
hour into adjacent one-minute windows and count transactions in each window.
Each bar then represents transactions during that minute, not the number of
datoms in those transactions. Convert local clock times to UTC when supplying
the `#inst` bounds. Widen the bounds to the containing hour or day to examine
a broader period; narrow them to a selected minute or second for more detail.

To inspect one transaction, use `tx-data` with its
T or tagged transaction reference. The native `LogValue::tx_range` API offers
incremental traversal. See the [log API](../07_peer_api/06_log_api.md) for bound
types and retained log data, which can differ from indexed history.

### Indexes

Use the native index APIs to look up specific data or ranges directly from an
index. There is no dedicated index CLI command. Given a captured `DatabaseValue`
named `db`, this function reads a range of email values from AVET:

```rust
use edb_core::{AttributeName, DatabaseValue, IndexComponents, Keyword,
               SemanticError, Value};

fn print_email_range(db: &DatabaseValue) -> Result<(), SemanticError> {
    let email = db.schema().resolve_ident(&Keyword::new("person", "email"))
        .expect("installed person schema");
    let start = db.avet_boundary(IndexComponents::Two(
        AttributeName::Id(email), Value::String("a".into()).into(),
    ))?;
    let end = Value::String("c".into());
    for item in db.seek_cursor(&start)? {
        let datom = item?;
        if datom.attribute != email || !datom.value.index_cmp(&end).is_lt() {
            break;
        }
        println!("{datom:?}");
    }
    Ok(())
}
```

The unique `:person/email` attribute participates in AVET. The range includes
`"a"` and excludes `"c"`; it covers Alice's and Bob's example addresses. A seek
continues through the rest of the index, so the loop stops at the attribute or
value boundary. For an exact match use `prefix_cursor` with an `IndexPrefix`.
Index results are datoms in index order, suitable for displaying or consuming
directly. See [index APIs](../06_indexes/04_index_apis.md) for EAVT, AEVT, AVET,
VAET, SAEVT, SAVET, prefixes, reverse seeks, and cursor limits.

For the illustrative music schema above, select AVET and supply the leading
components A = `:artist/name` and V = `"Foo Fighters"` to retrieve matching
datoms. The attribute must participate in AVET through `:db/index true` or
uniqueness, and its index must be ready:

```rust
use edb_core::{DatabaseValue, IndexPrefix, Keyword, SemanticError, Value};

fn print_artist_datoms(db: &DatabaseValue) -> Result<(), SemanticError> {
    let name = db.schema().resolve_ident(&Keyword::new("artist", "name"))
        .expect("installed music schema");
    let prefix = IndexPrefix::Avet {
        attribute: name,
        value: Some(Value::String("Foo Fighters".into())),
        entity: None,
    };
    for item in db.prefix_cursor(&prefix)? {
        println!("{:?}", item?);
    }
    Ok(())
}
```

Leaving the entity unspecified includes every entity with that exact name; it
does not require names to be unique. Each returned `Datom` has `entity`,
`attribute`, `value`, `tx`, `source`, and `added` fields. The attribute is a numeric
ID, and the value is the string `"Foo Fighters"`; the remaining values depend on
the database. A prefix does not have a transaction component. To choose a
starting position including T, use a four-component AVET boundary and a seek,
which continues beyond matching components as described above.

## Data sources

A data source is a named value, either a database specification or a data set.
Keep source specifications in an EDN map and pass its file with `--sources`.
Add or remove map entries to manage the sources used by a query; unused source
entries are rejected. Ordinary scalar, tuple, collection, and relation inputs
instead go in the `--inputs` vector in their declared order.

If the data source is a database, specify it as the value of an input bound to
`$`, `$1`, `$2`, etc. First, bind `edn-demo` to `$`, the implicit source used by
ordinary `:where` patterns:

```sh
cat > default-source.edn <<'EDN'
{$ {:database "edn-demo"}}
EDN
edb query --file names.edn --sources default-source.edn
```

Next, bind the same database to `$1`, which must be specified in the pattern:

```sh
cat > named-source.edn <<'EDN'
{$1 {:database "edn-demo"}}
EDN
cat > named-query.edn <<'EDN'
[:find ?e ?name :in $1 :where [$1 ?e :person/name ?name]]
EDN
edb query --file named-query.edn --sources named-source.edn
```

The documentation query can use the same named source:

```sh
cat > named-documentation.edn <<'EDN'
[:find ?e ?doc
 :in $1
 :where [$1 ?e :db/doc ?doc]]
EDN
edb query --file named-documentation.edn --sources named-source.edn
```

Here `:in $1` declares the source, and `$1` at the beginning of the pattern
selects it. The source map binds that name to `edn-demo`; `$1` is a query-local
name, not a database name. The result has the same entity/documentation pairs
as the default-source documentation query above. No `--inputs` file is needed
because the only declared input is supplied by `--sources`.

Use multiple names to query across databases or database views. The following
compares names at an earlier T with the currently opened value:

```sh
cat > views.edn <<'EDN'
{$then {:database "edn-demo" :as-of 2}
 $now  {:database "edn-demo"}}
EDN
cat > changed-names.edn <<'EDN'
[:find ?e ?before ?after
 :in $then $now
 :where [$then ?e :person/name ?before]
        [$now ?e :person/name ?after]
        [(not= ?before ?after)]]
EDN
edb query --file changed-names.edn --sources views.edn
```

The result is empty until a name differs between those views. Source descriptors
also accept `:since` and `:history`; log descriptors use `:log true` and cannot
combine that flag with database-view options. Descriptor views control their own
source; primary command-line view flags apply to the default primary `$` source,
not to every named source. Independently opened sources do not promise a shared
capture instant. For a consistent comparison within one database in Rust, derive
both views from the same captured `DatabaseValue`. For different databases, join
on shared application keys such as email, rather than comparing entity IDs.

If a data source is a data set, bind it to a relation matching its row shape.
This standalone example filters two-column input data using a predicate:

```sh
cat > scores-input.edn <<'EDN'
[[["Alice" 42] ["Bob" 19]]]
EDN
cat > scores-query.edn <<'EDN'
[:find ?name ?score
 :in [[?name ?score]]
 :where [(>= ?score 21)]]
EDN
edb query --file scores-query.edn --inputs scores-input.edn
# #{["Alice" 42]}
```

The outer vector holds one input argument: a relation containing two tuples.
Predicates operate on the bound columns without requiring a database. EDB also
supports raw tuple sources, where a source-qualified pattern matches columns
directly:

```sh
cat > scores-source.edn <<'EDN'
{$scores [["Alice" 42] ["Bob" 19]]}
EDN
cat > scores-source-query.edn <<'EDN'
[:find ?name ?score
 :in $scores
 :where [$scores ?name ?score] [(>= ?score 21)]]
EDN
edb query --file scores-source-query.edn --sources scores-source.edn
```

To reuse a saved relation result, place the entire result set inside the inputs
vector for one relation argument, or use it as a value in a source map. A bare
result file is not itself an `--inputs` vector or `--sources` map.

For example, filter a data set of documentation strings by ID. The following
rows illustrate documentation for a music schema; they are literal input data,
so this command does not require that schema or a database:

```sh
cat > documentation-input.edn <<'EDN'
[[[81 "The artist's name"]
  [82 "The artist's sort name"]]
 81]
EDN
cat > documentation-by-id.edn <<'EDN'
[:find ?e ?doc
 :in [[?e ?doc]] ?id
 :where [(= ?e ?id)]]
EDN
edb query --file documentation-by-id.edn --inputs documentation-input.edn
# #{[81 "The artist's name"]}
```

The two inputs are the relation of entity/documentation pairs and the scalar
`?id`, supplied in that order. `[[?e ?doc]]` binds each two-column tuple in the
relation; `[?e ?doc]` would instead declare a single tuple input. The equality
predicate retains only rows whose `?e` equals `?id`. No `$` input or datom
pattern is needed, since both columns come from the supplied data set.

To filter `documentation-result.edn` from the schema exploration above, replace
the example relation with the saved set and supply one of its entity references
as the second input. Preserve the `#edb/ref` tag on both the saved IDs and the
selected ID; the plain integers above are illustrative data values.

Rerun a command to reload its input files and reopen database sources. A database
descriptor is a recipe for obtaining a value; a saved data set remains fixed
until replaced. Native code can retain immutable database values and bound
queries for repeated use. See
[application data queries](../05_query_and_pull/01_application_data.md) for
relation bindings and reusable native sources.
