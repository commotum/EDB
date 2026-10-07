# Database Filters

Filters take a database value and return a new database value that exposes only
datoms that satisfy a predicate. This makes it possible to have a single set of
queries and index traversals that can be used without change against different
filtered views of your data.

EDB databases can be filtered with the time-based predicates `as_of` and `since`.
In addition, you can get a view of retained assertions and retractions via
`history`, and apply domain-specific predicates with `filter`. Each of these
APIs is described below. They operate on an immutable `DatabaseValue`; clone a
value before deriving a view when you also need the original.

## Example Database

All the inventory examples below use an entity that tracks the quantity of
dilithium crystals on hand at a given point in time. The subset of the data
relevant to the examples is shown below. `item` is the inventory entity, and
`tx1` through `tx5` are the five transaction entity IDs. This setup explicitly
attributes each statement to the built-in source entity `:db.source/system`,
abbreviated `system` in the table. All dates are UTC.

| e | a | v | tx | s | added |
| --- | --- | --- | --- | --- | --- |
| tx1 | :db/txInstant | 2013-12-01T00:00:00Z | tx1 | system | true |
| item | :item/id | DLC-042 | tx1 | system | true |
| item | :item/description | Dilithium Crystals | tx1 | system | true |
| item | :item/count | 100 | tx1 | system | true |
| tx2 | :db/txInstant | 2013-12-20T00:00:00Z | tx2 | system | true |
| item | :item/count | 100 | tx2 | system | false |
| item | :item/count | 250 | tx2 | system | true |
| tx3 | :db/txInstant | 2014-02-28T00:00:00Z | tx3 | system | true |
| item | :item/count | 250 | tx3 | system | false |
| item | :item/count | 50 | tx3 | system | true |
| tx4 | :db/txInstant | 2014-04-01T00:00:00Z | tx4 | system | true |
| tx4 | :tx/error | true | tx4 | system | true |
| item | :item/count | 50 | tx4 | system | false |
| item | :item/count | 9999 | tx4 | system | true |
| tx5 | :db/txInstant | 2014-05-15T00:00:00Z | tx5 | system | true |
| item | :item/count | 9999 | tx5 | system | false |
| item | :item/count | 100 | tx5 | system | true |

Here is an in-memory setup. `with_edn` returns a speculative transaction report;
these examples require no server and commit nothing to storage. In an
application, capture `db` from your connection instead. See
[transactions](../04_transactions/00_transactions.md) and the
[EDN frontend](../01_tutorials/00_edn_workflow.md).

```rust
use edb_core::{
    AttributeName, Database, DatabaseValue, EntityIdentifier, Keyword,
    QueryControl, QuerySourceValue, TimePoint, Value,
};
use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_pull::parse_pull_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::edn_value::query_value_to_edn;

let schema = r#"[
 {:db/id "edb.tx" :db/source :db.source/system}
 {:db/source :db.source/system
  :db/ident :item/id :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
 {:db/source :db.source/system
  :db/ident :item/description :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/source :db.source/system
  :db/ident :item/count :db/valueType :db.type/long
  :db/cardinality :db.cardinality/one}
 {:db/source :db.source/system
  :db/ident :tx/error :db/valueType :db.type/boolean
  :db/cardinality :db.cardinality/one}
]"#;
let installed = Database::bootstrap()?.with_edn(schema, 0)?.db_after;
let first = installed.with_edn(
    r#"[{:db/id "edb.tx" :db/source :db.source/system}
        {:db/id "crystals" :db/source :db.source/system :item/id "DLC-042"
         :item/description "Dilithium Crystals" :item/count 100}]"#,
    1385856000000,
)?;
let second = first.db_after.with_edn(
    r#"[{:db/id "edb.tx" :db/source :db.source/system}
        {:db/id [:item/id "DLC-042"] :db/source :db.source/system :item/count 250}]"#,
    1387497600000,
)?;
let third = second.db_after.with_edn(
    r#"[{:db/id "edb.tx" :db/source :db.source/system}
        {:db/id [:item/id "DLC-042"] :db/source :db.source/system :item/count 50}]"#,
    1393545600000,
)?;
let fourth = third.db_after.with_edn(
    r#"[{:db/id "edb.tx" :db/source :db.source/system}
        {:db/id [:item/id "DLC-042"] :db/source :db.source/system :item/count 9999}
        {:db/id "edb.tx" :db/source :db.source/system :tx/error true}]"#,
    1396310400000,
)?;
let bad_tx = fourth.tx_data[0].tx;
let fifth = fourth.db_after.with_edn(
    r#"[{:db/id "edb.tx" :db/source :db.source/system}
        {:db/id [:item/id "DLC-042"] :db/source :db.source/system :item/count 100}]"#,
    1400112000000,
)?;
let db = fifth.db_after.database_value();
let item = EntityIdentifier::Lookup {
    attribute: AttributeName::Ident(Keyword::new("item", "id")),
    value: Value::String("DLC-042".into()),
};
let wildcard = parse_pull_edn("[*]")?;
```

Each metadata-only `edb.tx` map supplies the source for the generated
`:db/txInstant`; the other maps supply sources for their own assertions and
any resulting retractions.

## as-of

An `as_of` filter returns a database "as of" a particular point in time, ignoring
any transactions after that point. `DatabaseValue::as_of(t)` accepts a logical
transaction coordinate. `as_of_time_point` accepts any of the explicit
`TimePoint` forms:

- `TimePoint::Tx(tx)`: a transaction entity ID. Use this when you want a database
  as of a specific transaction.
- `TimePoint::T(t)`: a logical transaction coordinate. Use, for example, the
  `basis_t()` of a transaction report's `db_after`.
- `TimePoint::Instant(millis)`: Unix epoch milliseconds. Use an instant when you
  have a wall clock time for the database you want, but do not have a transaction
  ID or T value. An instant is not as precise as T or a transaction entity ID.

The upper boundary is inclusive. When multiple transactions share exactly the
requested millisecond, instant resolution selects the earliest matching
transaction. Use T or Tx when that distinction matters.

`resolve_time_point` exposes the boundary calculation. Between transaction
instants, it returns one less than the next transaction's T; after the last
instant, it returns `basis_t() + 1`. A boundary therefore need not name an
assigned transaction. Resolution uses the captured underlying basis, including
when the value already has filters. By contrast, `entid_at` constructs an entity
index boundary using the first transaction **at or after** an instant; it does
not use the `as_of` predecessor rule. See
[partitions](../04_transactions/02_partitions.md).

Because the `:item/count` of dilithium crystals changes over time, different
`as_of` views show different counts. Pull and entity navigation project distinct
values: two sources asserting the same count still produce one count value,
while history retains the separate contributions.

```rust
let current = db.pull(&wildcard, item.clone())?;
println!("{}", write_edn(&query_value_to_edn(&current)?)?);
```

```edn
{:db/id #edb/ref 17592186045420
 :item/id "DLC-042"
 :item/description "Dilithium Crystals"
 :item/count 100}
```

```rust
// 2014-01-01T00:00:00Z: after tx2 and before tx3.
let as_of_eoy_2013 = db.clone()
    .as_of_time_point(TimePoint::Instant(1388534400000))?;
let earlier = as_of_eoy_2013.pull(&wildcard, item.clone())?;
println!("{}", write_edn(&query_value_to_edn(&earlier)?)?);
```

```edn
{:db/id #edb/ref 17592186045420
 :item/id "DLC-042"
 :item/description "Dilithium Crystals"
 :item/count 250}
```

Entity IDs above are those of this fresh in-memory setup. Derive them from
tempids or lookup references in application code rather than hard-coding them.
The view retains the underlying value's basis, schema and ident dictionary;
`basis_t()` does not become the `as_of` boundary. Inspect `as_of_t()` for that
boundary. An old view cannot reveal transactions beyond its captured basis.
Similarly, `last_tx_instant()` reports the most recent transaction instant at
the underlying basis, not the most recent datom visible through the filter.

## since

A `since` filter is the opposite of `as_of`. Taking the same point-in-time
arguments, `since` returns a value of the database that includes only datoms
from transactions after that point in time. The lower boundary is exclusive.
A point-in-time view still suppresses retracted assertions; it is not a list of
all changes since the boundary. Use history or the log for additions and
retractions. Because an exact instant selects the earliest transaction at that
millisecond, `since` can include later transactions in the same millisecond.

There is an important subtlety to consider when using `since`. Typically, the
identifying information used to look up an entity is established in the first
transaction about that entity, so a `since` filter may not be able to see the
information needed for lookup. Our example has exactly this problem trying to
find the entity in question by its `:item/id`:

```rust
let since_2014 = db.clone()
    .since_time_point(TimePoint::Instant(1388534400000))?;
let missing = since_2014.pull(&wildcard, item.clone())?;
println!("{}", write_edn(&query_value_to_edn(&missing)?)?);
```

```edn
{:db/id nil}
```

Idents are an exception: `entid` and lookup refs using `:db/ident` resolve through
the underlying value's ident dictionary. Filtering datoms does not reconstruct
an earlier dictionary or hide its entries. Ordinary unique attributes such as
`:item/id` resolve through the filtered index.

Most callers of `since` will refer to the database twice: one reference to the
default "now" db to find entities, and a second reference to the `since` db to
shave off the past. With Pull, this looks like:

```rust
let eid = db.resolve_entity_identifier(&item)?.expect("inventory item exists");
let recent = since_2014.pull(&wildcard, eid)?;
println!("{}", write_edn(&query_value_to_edn(&recent)?)?);
```

```edn
{:db/id #edb/ref 17592186045420 :item/count 100}
```

Notice that the "now" `db` is used to resolve the lookup ref, and the
`since_2014` db is then used to pull the associated attributes. In a query, this
can take the form of passing two different views of the same db as separate inputs:

```rust
let query = parse_query_edn(r#"
    [:find ?count
     :in $ $since ?id
     :where [$ ?e :item/id ?id]
            [$since ?e :item/count ?count]]
"#)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
    EdnQueryArgument::Source(QuerySourceValue::Database(since_2014.clone())),
    EdnQueryArgument::Data(read_edn(r#""DLC-042""#)?),
])?;
let outcome = bound.execute(&QueryControl::default(), None)?;
println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
// #{[100]}
```

Here, the `db` argument is named `$` inside the query and used to resolve the
`:item/id`. Then `since_2014` is named `$since` inside the query, and used to
find the `:item/count`.

## history

The history view of a plain database includes the present and the retained past,
including retractions. This makes the history view useful for querying the
history of an entity, or group of entities. `history()` disables
assertion/retraction collapse; it does not remove an existing `as_of`, `since`
or custom predicate. Start with the plain value to inspect its unfiltered history.

Below is a query for all retained assertions about dilithium crystals:

```rust
let history = db.clone().history();
let query = parse_query_edn(r#"
    [:find ?aname ?v ?inst
     :in $ ?e
     :where [?e ?a ?v ?tx _ true]
            [?tx :db/txInstant ?inst]
            [?a :db/valueType _]
            [?a :db/ident ?aname]]
"#)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(history.clone())),
    EdnQueryArgument::Data(read_edn(&format!("#edb/ref {eid}"))?),
])?;
let outcome = bound.execute(&QueryControl::default(), None)?;
println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
```

The query joins through `:db/txInstant` to return wall clock time, restricts `?a`
to installed attributes with `:db/valueType`, and joins through `:db/ident` to
return human-readable attribute names. The count assertions are 100 on December 1,
250 on December 20, 50 on February 28, 9999 on April 1, and 100 on May 15. Query
relations have no chronological order guarantee; sort the result by `?inst` in
the caller when presenting a timeline. Replace `true` with `?added` and return
that variable to include retractions as well. To distinguish sources, replace
the `_` source slot with `?source` and add it to `:find`. History retains distinct
contributions of the same value. A retraction retains the retracting source
without changing the source of the earlier assertion.

Simplified views of data that do not account for time are incompatible with
history, since a history database can see multiple different values for the
"same" fact at different times. In particular, an entity is a point-in-time
view and cannot be created from a history database. Pull and entity navigation
reject history values with `database/history-not-point-in-time`. Queries and
index traversal remain available.

History is the retained indexed history, not a promise to retain every past fact
forever. `:db/noHistory` consolidation can remove superseded facts from indexes;
excision can remove facts from the active generation. The
[transaction log](../06_indexes/00_indexes_and_log.md#the-log-is-not-an-indexed-history-view)
retains transaction events omitted by `noHistory`, but is also subject to
excision. A held value is not a durable storage-retention pin; see
[captured-value retention](00_database_values.md#captured-value-retention).

## filter

### Filtering Errors

A filter view allows you to produce values of the database filtered by your own
domain-specific predicates. A filter can be used, for example, to remove datoms
that are incorrect, not applicable at a point in time, or not available for
security reasons.

For example, suppose that the 9999 value for `:item/count` in the example database
above turns out to be a mistake. The transaction that made this assertion can be
identified and used to filter out the incorrect value:

```rust
let error_txes = std::collections::BTreeSet::from([bad_tx]);
let corrected = history.clone()
    .filter(move |_, datom| !error_txes.contains(&datom.tx));
```

Now, the `corrected` database value can be passed to existing history queries,
which will never see or consider datoms from the incorrect transaction. It
remains a history value; it cannot be passed to Pull or entity navigation.

Filtering a transaction excludes both its assertions and its retractions.
Predicates run **before** current-value retraction collapse. Consequently,
excluding `tx4` from a point-in-time view leaves the old assertion of 50 visible,
as well as the later assertion of 100: the retraction of 50 was part of `tx4`.
Even a cardinality-one attribute can therefore have multiple visible values
under an arbitrary custom filter. Filtering does not repair the transaction
history or transact a correction. A later retraction of 9999 in `tx5` also remains
visible in the corrected history; it belongs to a different transaction.

Retraction collapse matches entity, attribute and stored value, regardless of
the original asserting source. In particular, top-level decimal values with
equal magnitudes but different scales remain distinct:
retracting `1.0M` does not retract `1.00M`. This applies to both forward and
reverse filtered index traversal. See
[decimal equality and stored scale](../03_schema/01_identity_and_values.md#decimal-equality-and-stored-scale).

### Filtering For Security

Imagine that you want to exclude all values of an attribute entirely from
consideration. With `:user/passwordHash` installed in `plain_db`, this code
creates a filter that rejects all datoms about that attribute:

```rust
let password_hash_id = plain_db
    .entid(&Keyword::new("user", "passwordHash"))
    .expect("password hash attribute is installed") as u32;
let filtered_db = plain_db.clone()
    .filter(move |_, datom| datom.attribute != password_hash_id);
```

This is an application read view for trusted code, not a credential or sandbox
boundary. The caller can retain the plain value; view metadata and the schema
are not redacted, and speculative transactions assess the full underlying basis.
Do not give untrusted code access to the underlying database on the assumption
that a Rust predicate enforces authorization. A filter does not erase stored
data or already captured results.

### Joining Different Filters of the Same Database

It is idiomatic to join different filters of the same database. One motivating
case is performance: since filtering is not free, it makes sense to filter only
the parts of the query that need filtering. For example, this query uses an
unfiltered database to find entities, then a filtered version of the same database
to limit the attributes visible through Pull:

```edn
[:find (pull $filtered ?e [*])
 :in $plain $filtered ?email
 :where [$plain ?e :user/email ?email]]
```

Bind `plain_db`, `filtered_db` and the email string in that order, using the
same `EdnQueryArgument` forms as the `since` example. The application chooses
which clauses receive the plain source; this is part of its trusted read policy.

#### Filtering on Transaction Attributes

Filters can do more than look at single datoms. They can also consider datoms in
the context of the database. For example, imagine that you mark the transactions
in your system with a `:source/confidence` field that indicates your confidence
in that transaction's contributions, on a scale from 0 to 100. This is transaction
metadata; the individual datoms retain their own sources. You could then filter
a point-in-time database as follows:

```rust
fn filter_by_confidence(db: DatabaseValue, conf: i64) -> DatabaseValue {
    let attribute = AttributeName::Ident(Keyword::new("source", "confidence"));
    db.filter(move |context, datom| {
        let confidence = context.entity(datom.tx).and_then(|entity| {
            match entity {
                Some(entity) => entity.get_named(&attribute),
                None => Ok(None),
            }
        });
        matches!(confidence,
            Ok(Some(edb_core::EntityValue::Scalar(Value::Long(score))))
                if score > conf)
    })
}
```

The callback receives the same temporal/history view with **all custom
predicates removed**, preventing recursive filter evaluation. Repeated `filter`
calls combine predicates by logical conjunction. Callbacks must be
`Send + Sync + 'static`, and should be deterministic for repeatable reads.

This example rejects missing confidence values and read errors. The predicate
returns a Boolean, not a `Result`; when read errors must propagate, resolve
eligible transaction IDs with a fallible query first, then filter by that set.
Entity navigation in this callback requires a point-in-time context. A `since`
boundary may also hide confidence metadata. For a history filter, use index/query
reads or precompute eligible IDs from an appropriate plain value instead.

Queries using this filter can focus on finding data of interest, without worrying
about the cross-cutting concern of how trusted the data is. The following query,
bound to `filter_by_confidence(db, 90)`, finds only the stories whose titles were
added by transactions with a confidence score higher than 90:

```edn
[:find ?title
 :where [_ :story/title ?title]]
```

Install `:source/confidence` as a cardinality-one Long and `:story/title` as a
cardinality-one String. A transaction can assert both a story title and
`{:db/id "edb.tx" :db/source :db.source/system :source/confidence 95}`,
with an explicit source on the story assertion and the metadata-only `edb.tx`
map for `:db/txInstant`. The threshold check does not itself enforce the
application's 0–100 range.

## Usage Considerations

### as-of Is Not a Branch

Filters are views of an underlying immutable database value. In particular, the
combination of `with_edn` and `as_of` means "with followed by as-of", regardless
of which API call you make first. Speculation removes temporal and custom filters
for transaction assessment, then reapplies the original view to the result.
`with_edn` plus `as_of` lets you see a speculative db with recent datoms filtered
out, but it does **not** let you branch the past. History values reject
speculative transactions.

You can combine `as_of` and `since` to expose the window `since < t <= as_of`.
Repeating `as_of` replaces its prior upper boundary; repeating `since` replaces
its prior lower boundary. These operations do not advance the captured basis.
Custom Rust predicates are opaque: their filtered values have no portable
committed snapshot reference.

### The Present Pays No History-Filtering Penalty

The original database value returned from a connection does no view filtering.
It sees only the present, without having to filter the past. EDB uses current
index access for this plain current value; temporal and custom-filter views may
need to scan historical candidates and collapse retractions. This avoids a
history scan for ordinary current reads, but is not a claim that storage,
indexing, caching or maintenance cost is independent of retained history.

Filters can require examining additional candidates to find enough visible
results, and proving absence can require exhausting a range. Native callbacks
are trusted cooperative code; query cancellation cannot forcibly preempt a
running Rust predicate.

### Filter or Log?

Filters are applied to database indexes, filtering out data that does not match
a predicate. They allow you to separate some aspect of a query, reusing query
logic across different filtered views.

Consider the following two queries:

- "What happened between 8:00 and 9:00 this morning?"
- "What did entity 42 look like at 8:00 this morning?"

The first query is limited **only** by time. Such a query can be answered directly
from the log, which is ordered by transaction time. Use
`LogValue::tx_range(start, end)` with inclusive start and exclusive end to obtain
transaction events. A point-in-time `as_of`/`since` window is not an event stream;
requesting history makes retractions visible, but traversing the indexes without
an entity, attribute or value restriction can require a broad history scan.

The second query is limited by both entity and time. Thus it makes sense to
augment indexes with a filter, calling a query on an `as_of` view of the database.
An entity-restricted EAVT traversal seeks to that entity's datoms, then filters
them to exclude the most recent datoms. Because all the time-related work is
done by `as_of`, the query can be time-agnostic and work with different
point-in-time views. You can use the same approach in your own queries, writing
queries that are agnostic about time or some domain value, and using filters to
allow reuse across different views. See [indexes and the log](../06_indexes/00_indexes_and_log.md).
