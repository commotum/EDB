# Best practices

## EDB Schema Best Practices

- [Plan for accretion](#plan-for-accretion)
- [Model relationships in one direction only](#model-relationships-in-one-direction-only)
- [Use idents for enumerated types](#use-idents-for-enumerated-types)
- [Use unique identities for external keys](#use-unique-identities-for-external-keys)
- [Use noHistory for high-churn attributes](#use-nohistory-for-high-churn-attributes)
- [Grow schema and never break it](#grow-schema-and-never-break-it)
- [Never remove or reuse names](#never-remove-or-reuse-names)
- [Use aliases](#use-aliases)
- [Annotate schema](#annotate-schema)

## EDB Transaction Best Practices

- [Add facts to the transaction entity](#add-facts-about-the-transaction-entity)
- [Use lookup refs to specify entities](#use-lookup-refs-to-specify-existing-entities)
- [Use CAS for optimistic concurrency](#use-cas-for-optimistic-concurrency)
- [Use db-after to see the result of a transaction](#use-dbafter-to-see-the-result-of-a-transaction)
- [Set txInstant on imports](#set-txinstant-on-imports)
- [Pipeline transactions for higher throughput](#pipeline-transactions-for-higher-throughput)

## EDB Query Best Practices

- [Put the most selective clause first](#put-the-most-selective-clause-first-in-query)
- [Join along](#join-along)
- [Prefer query over raw index access](#prefer-query-over-raw-index-access)
- [Pass collections as inputs](#pass-collections-as-inputs)
- [Use Pull to retrieve attribute values](#use-pull-to-retrieve-attribute-values)
- [Put blanks in data patterns](#put-blanks-in-data-patterns)
- [Parameterize queries and reuse preparation](#use-query-inputs-to-parameterize-queries-and-leverage-caching)
- [Work with data structures, not strings](#work-with-data-structures-not-strings)

## EDB Time Best Practices

- [Use a consistent db value for a unit of work](#use-a-consistent-db-value-for-a-unit-of-work)
- [Specify T for precise as-of locations](#specify-t-instead-of-txinstant-for-precise-asof-locations)
- [Use history for audit trails](#use-the-history-filter-for-audit-trail-queries)
- [Pass multiple points in time to a query](#pass-multiple-points-in-time-to-a-single-query)
- [Use the log when time is most selective](#use-the-log-api-if-time-is-your-most-selective-criterion)

Examples using artists, countries, accounts and purchases assume the named
attributes have already been installed with appropriate types and cardinalities.
Music-corpus results below illustrate result shapes; row order is unspecified.
See the [schema reference](../03_schema/02_schema_reference.md),
[transaction data](../04_transactions/04_transaction_data.md), and
[query reference](../05_query_and_pull/05_query_reference.md) for complete syntax.

## Plan for Accretion

Programs should not assume that an entity will be limited to the set of attributes that it has at a given point in time. The [schema growth](#grow-schema-and-never-break-it) principle provides a means for entities to develop over time, and tying programmatic usage to a fixed set of attributes can lead to *breakage* in the face of *growth*.

For example, usage should be structured to handle a given set of attributes for an entity, not apply arbitrary side effects to all possible attributes. Request the keys your code understands with an explicit Pull pattern:

```rust
use edb_core::edn_pull::parse_pull_edn;

let understood = parse_pull_edn("[:user/id :user/name]")?;
let user = db.pull(&understood, user_id)?;
// Perform application work using these understood attributes.
```

A wildcard Pull is useful for exploration, but discovering a new attribute should not automatically cause an old program to act on it.

## Model Relationships in One Direction Only

When you create a reference type, EDB automatically indexes the reference in both directions: from entity to value and from value to entity. EDB maintains EAVT and the reference-only VAET index, so it is redundant to manage relationships in both directions. The best access path and its cost depend on the supplied bindings and workload.

For example, a music-catalog schema connects artists and releases via the `:release/artists` attribute. There is no need for a separate `:artist/releases` attribute. The data clause below can find `?artist` from `?release` and/or `?release` from `?artist`:

```edn
[?release :release/artists ?artist]
```

## Use Idents for Enumerated Types

Enumerated types should be represented by a reference pointing to an entity that has an ident. Application datoms store the numeric entity reference rather than repeating the ident keyword in every reference value.

A music-catalog schema demonstrates this with `:artist/country`. Note that transactions can use an ident directly as a reference value, eliding the indirection. The transaction data below creates a reference between an artist identified by a unique `:artist/name` and the entity whose ident is `:country/GB`:

```edn
[[:db/add [:artist/name "Example Artist"] :artist/country :country/GB]]
```

## Use Unique Identities for External Keys

If an attribute is used as an external key, set that attribute to be `:db/unique` `:db.unique/identity`. Unique identities can be domain-specific identifiers, such as an account number or an email address.

For example, you could represent an ISO 3166-1 compliant country code external key with the following attribute definition:

```edn
{:db/ident :country/code
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one
 :db/unique :db.unique/identity
 :db/doc "ISO 3166-1 alpha-3 country code"}
```

## Use NoHistory for High-Churn Attributes

For high churn attributes, such as a counter or version incrementer, the cost of storing history is frequently not worth the impact on database size or indexing performance. If you have a high-churn attribute that you don't expect to use in historical queries, you can set `:db/noHistory` to `true`. This permits future indexing to omit eligible historical assertion/retraction pairs; it is not immediate deletion and does not erase the transaction log, receipts or backups. See [changing noHistory](../03_schema/03_changing_schema.md#changing-dbnohistory).

## Grow Schema and Never Break It

In a production system, one or more codebases depend on your data. In schema terms, *growth* is providing more schema while *breakage* is removing schema or changing the meaning of existing schema.

Growth migrations are suitable for production, and breakage migrations are, at best, a dev-only convenience.

Further details on *growth* vs. *breakage* can be found in Rich Hickey's 2016 Clojure/conj keynote, [Spec-ulation](https://www.youtube.com/watch?v=oyLBGkS5ICk).

EDB provides lightweight, flexible schema that can be *grown* as necessary to support new information about your system or domain. *Growth* is always additive, i.e. adding new attributes, adding new 'types', adding relationships between 'types':

```edn
;; Given some initial schema
{:db/ident :user/id
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one}

;; Grow system by adding schema:
{:db/ident :user/name
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one}
```

Although this recommendation may seem difficult, the following three sections provide approaches to facilitate *growth* without *breakage*.

## Never Remove or Reuse Names

The meaning of a name is established when the name is first introduced. Removing a named schema component at any level is a breaking change for programs that depend on that name. Never remove a name. Reusing that name to mean something substantially different breaks programs that depend on that meaning. This can be even worse than removing the name, as the breakage may not be as immediately obvious.

## Use Aliases

Instead of removing or reusing names, use aliases to allow multiple names to refer to a single schema element. EDB retains old ident names as aliases when an entity is renamed. The current `:db/ident` fact is cardinality-one; both old and new names resolve to the same entity ID. See [changing idents](../03_schema/03_changing_schema.md#changing-dbident).

For example, to create an alias, `:user/primary-email` that refers to the same schema element as an existing ident (`:user/id`), transact the following assertion:

```edn
[[:db/add :user/id :db/ident :user/primary-email]]
```

This new alias allows new programs to use the new `:user/primary-email` name, while adhering to the [prior section](#never-remove-or-reuse-names) ensures that old programs that require `:user/id` will continue to function.

## Annotate Schema

Because EDB schema is stored as data, you can and should annotate your schema elements with useful information that can:

- Help users/readers understand the system
- Document how the schema has grown over time

For example, in the case of a [preferred newer schema option](#grow-schema-and-never-break-it), you could add a `:schema/see-instead` reference and a `:db/doc` on the older schema element to point users at the new convention:

First install `:schema/see-instead` as a cardinality-one reference attribute and
ensure both email attributes exist. Then annotate the old schema entity:

```edn
[{:db/id :user/email
  :schema/see-instead :user2/email
  :db/doc "prefer the user2 namespace for new development"}]
```

## Add Facts About the Transaction Entity

Most entities in a system model the "what" of your domain. Transactions provide a place to model "when", "who", "where", and "why".

As a part of every transaction, EDB creates an entity to represent the transaction itself. This *reified transaction* automatically includes a "when" datom, `:db/txInstant`, which records the wall clock time that the transaction was recorded.

Transactions are ordinary entities: you can create attributes that are about transactions and query them just like any other datoms in EDB.

For example, install `:data/src` as a reference and `:source/url` as a string. The following transaction forms use `:data/src` to link the transaction to a source URL from an external system.

```edn
[[:db/add "edb.tx" :data/src "import-file"]
 [:db/add "import-file" :source/url "http://example.com/catalog-2_29_2012.xml"]]
```

## Use Lookup Refs to Specify Existing Entities

Database updates often have a two-step structure:

- Query for database ids using an externally unique identifier
- Use those database ids as part of a transaction

Lookup refs flatten this into a single step. With a lookup ref, you can specify an entity directly via an external identifier.

So instead of:

```edn
;; query for e
[:find ?e
 :where [?e :user/email "jdoe@example.com"]]

;; tx-data using e
[[:db/add e :db/doc "doc about John"]]
```

Here `e` denotes the returned entity ID; substitute that ID when constructing
the transaction data. EDN does not evaluate an application variable named `e`.

You can simply:

```edn
;; tx-data with a lookup ref, no query needed
[[:db/add
  [:user/email "jdoe@example.com"]
  :db/doc
  "doc about John"]]
```

## Use CAS for Optimistic Concurrency

Transactional systems can use optimistic or pessimistic concurrency controls to deal with read-for-update scenarios. EDB's built-in compare-and-swap enables generic optimistic approaches.

The following example uses `TxOp::Cas` (EDN `:db/cas`) to add a deposit to an account. It retries with a newly read balance **only** when assessment returns `transaction/cas-failed`. Amounts are integer cents, and addition is checked. `account` is an existing entity ID and `balance_attribute` is the installed cardinality-one long attribute ID.

```rust
use edb_core::{Connection, EntityRef, SemanticError, ServiceTransactionReport,
               TransactionRequest, TxOp, Value};
use std::time::Duration;

#[derive(Debug)]
struct DepositFailure {
    error: SemanticError,
    // Keep this request, especially when the outcome is unknown.
    request: Option<TransactionRequest>,
}

fn make_deposit(
    conn: &Connection,
    account: u64,
    balance_attribute: u32,
    cents: i64,
    operation_key: &str,
) -> Result<ServiceTransactionReport, DepositFailure> {
    for attempt in 0..5 {
        let prepare = || -> Result<TransactionRequest, SemanticError> {
            let db = conn.db();
            let values = db.values(account, balance_attribute)?;
            let [Value::Long(balance)] = values.as_slice() else {
                return Err(SemanticError::incorrect(
                    "app/missing-balance", "account must have one long balance"));
            };
            let next = balance.checked_add(cents).ok_or_else(||
                SemanticError::incorrect("app/overflow", "balance overflow"))?;
            Ok(TransactionRequest::new(
                format!("{operation_key}/cas-{attempt}"),
                vec![TxOp::Cas {
                    entity: EntityRef::Id(account),
                    attribute: balance_attribute,
                    old: Some(Value::Long(*balance).into()),
                    new: Value::Long(next).into(),
                }],
            ))
        };
        let request = prepare().map_err(|error| DepositFailure {
            error, request: None,
        })?;
        // Persist the exact request in an application outbox here if recovery
        // must survive process failure, before submitting it.
        match conn.transact(request.clone(), Duration::from_secs(30)) {
            Ok(report) => return Ok(report),
            Err(error) if error.code == "transaction/cas-failed" && attempt < 4 => {
                // Confirmed noncommit: obtain a fresh value before recomputing.
                conn.sync().map_err(|error| DepositFailure {
                    error, request: None,
                })?;
            }
            Err(error) => return Err(DepositFailure {
                error, request: Some(request),
            }),
        }
    }
    unreachable!()
}
```

Choose `operation_key` once for this logical deposit and make it unique to that operation. This function must not be restarted from the beginning after a timeout or lost reply. An `UnknownOutcome` does not mean the CAS failed: the deposit may already have committed. Retain the returned request and resolve it by resubmitting `conn.transact(request.clone(), timeout)` with the **same key and complete content**. Recomputing against a later balance under a new key could deposit twice. Stop retrying after a known commit; a remote committed response whose local report could not be opened is still a commit.

The retry limit above applies to confirmed CAS conflicts. Other errors are returned with the attempted request for the caller to classify. See [exact retries and outcomes](../04_transactions/00_transactions.md).

## Use DbAfter to see the Result of a Transaction

The `Connection::transact` function returns a report whose `db_after` field holds the value of the database immediately after the transaction is applied. Using `db_after` ensures that you see the result of your transaction without including changes made later.

Because `db_after` is also returned by `DatabaseValue::with`, `with_forms` and `with_edn`, you can examine the results of a prospective or real transaction identically. Speculation does not commit data or reserve durable request identities.

The following example uses `:db/cas` and `db_after` in conjunction to verify that a deposit increases the account by the correct amount. The existing balance is 10000 cents and the deposit is 85000 cents:

```rust
use edb_core::{EntityIdentifier, Keyword, TransactionRequest};
use edb_core::edn_pull::parse_pull_edn;
use std::time::Duration;

let request = TransactionRequest::from_edn(
    "deposit-123-example",
    r#"[[:db/cas [:account/number "123"] :account/balance 10000 95000]]"#,
)?;
// Retain request for exact retry if the outcome is unknown.
let report = conn.transact(request.clone(), Duration::from_secs(30))?;
let account = EntityIdentifier::Lookup {
    attribute: edb_core::AttributeName::Ident(Keyword::new("account", "number")),
    value: edb_core::Value::String("123".into()),
};
let balance_after = report.db_after.pull(
    &parse_pull_edn("[:account/balance]")?, account,
)?;
// {:account/balance 95000}
```

The [transaction model](../04_transactions/05_transaction_model.md) describes db-before and db-after in more detail.

## Set txInstant on Imports

When importing values (e.g. from another datastore), you can override EDB's default timestamping behavior by setting `:db/txInstant` explicitly. In the typical case, you use the original transaction time logged by the database from which you are importing.

EDB's `:db/txInstant` values must be nondecreasing; multiple transactions can share the same millisecond. When performing an import:

- Choose an instant not older than the last transaction already in the database.
- Choose an instant no newer than the transactor's clock time. The durable writer rejects future instants.
- If the source time predates the destination's existing transactions, preserve it in an application attribute instead of backdating `:db/txInstant`.

To set `:db/txInstant`, add a map with the special `:db/id` value `"edb.tx"` and the supplied instant:

```edn
[{:db/id "edb.tx"
  :db/txInstant #inst "2014-02-28T00:00:00Z"}]
```

This example requires a destination whose last instant is at or before that date. Native requests may instead use `TransactionRequest::with_tx_instant(milliseconds)`. If both forms supply a timestamp, their values must agree. See [transaction data](../04_transactions/04_transaction_data.md).

## Pipeline Transactions for Higher Throughput

Data imports can overlap transaction submission and waiting by maintaining several transactions in flight. EDB provides bounded worker execution through `AsyncClient`; choose a concurrency bound using measurements of your actual workload. Concurrent submission does not make one transaction's db-after available to another before it commits, and submission order is not a dependency mechanism.

The following Rust helper submits independent requests in bounded batches, awaits every admitted result, and retains each request alongside its outcome:

```rust
use edb_core::{AsyncClient, SemanticError, ServiceTransactionReport,
               TransactionRequest};
use std::time::Duration;

async fn tx_pipeline(
    client: &AsyncClient,
    requests: Vec<TransactionRequest>,
    concurrency: usize,
) -> Vec<(TransactionRequest, Result<ServiceTransactionReport, SemanticError>)> {
    assert!(concurrency > 0);
    let mut input = requests.into_iter();
    let mut results = Vec::new();
    while input.len() > 0 {
        let mut pending = Vec::new();
        for request in input.by_ref().take(concurrency) {
            match client.transact(request.clone(), Duration::from_secs(30)) {
                Ok(operation) => pending.push((request, operation)),
                Err(error) => results.push((request, Err(error))),
            }
        }
        for (request, operation) in pending {
            results.push((request, operation.await));
        }
    }
    results
}
```

Create an `AsyncExecutor` and `AsyncClient` outside the async executor task as described in [async native clients](../07_peer_api/01_async_client.md). Capacity errors at admission mean that request was not submitted; an admitted operation can have an unknown outcome. Keep exact request keys and content for recovery. Dropping an admitted transaction future abandons its result, not the queued transaction.

For a large import, consume requests and reports incrementally rather than retaining the entire input and result vectors used by this small example. Apply backpressure to preparation as well as submission. If one batch depends on schema or entities from another, wait for the prerequisite commit before submitting it.

## Put the Most Selective Clause First in Query

To minimize work, restrictive patterns and already-bound values should guide query design. A useful reading order places a restrictive clause before a less restrictive clause:

```edn
:where [?e :only/matches ?few]
       [?e :will/match ?many]
```

EDB does **not** promise to execute `:where` clauses in written order. Its native evaluator selects ready clauses using a bound-term score; later clauses win score ties. This is not a cardinality estimator. Writing the selective clause first alone does not force it to run first. Supply selective inputs, connect patterns through shared variables, and inspect the actual plan and intermediate row counts with [query diagnostics](../05_query_and_pull/04_diagnostics.md).

## Join Along

A second rule of thumb is that each clause should join along at least one variable that has been bound in the preceding clauses.

For example, imagine a database with 1000 people, each of whom has one pet. In the written order below, the second clause does not join along an existing variable. If evaluated in that order, the intermediate result would be a cross product of a million (1000x1000) tuples, which the third clause would then reduce back to 1000 results.

```edn
;; likely a bad ordering!
[?person :person/name ?name]
[?pet :pet/name ?pet-name]    ;; matches no previous variables!
[?person :person/pet ?pet]
```

In the improved query below, each clause after the first joins along a variable that is already bound. Evaluating in this order keeps the intermediate result to at most 1000 tuples, saving memory and query work. EDB may reorder either example, so use diagnostics to establish the actual schedule.

```edn
;; better ordering
[?person :person/name ?name]
[?person :person/pet ?pet]    ;; join along: ?person bound
[?pet :pet/name ?pet-name]    ;; join along: ?pet bound
```

While neither of the preceding two rules of thumb is absolute, they often make it possible to quickly select a decent clause ordering.

## Prefer Query Over Raw Index Access

EDB's datalog query is simple and declarative. Queries written in Datalog are evident and logic-based, and allow the engine to choose access paths. As such, EDB query decouples logical specification from lookup implementation.

Datalog's simple and declarative nature allows the decomposition of queries. Evaluate a smaller set of clauses to understand which bindings they produce, then add the remaining joins. Use [query diagnostics](../05_query_and_pull/04_diagnostics.md) to connect this reasoning to observed work.

Conversely, [raw index access](../06_indexes/00_indexes_and_log.md) is preferable when all you want is an ordered datom range with no logic or joins. Common scenarios are data import/export and integration with external query systems.

## Pass Collections as Inputs

You can use a collection binding in a query to match several values specified in a collection. This behaves as a logical *or*, that is, it returns a union of the results for each item in the collection. When you want all results that match any of a given collection of entities or values, you should prefer a collection binding in a parameterized query over running multiple queries.

The following query uses a collection binding to return all names for artists based in Japan or Canada:

```edn
;; query
[:find (pull ?a [:artist/name])
 :in $ [?c ...]
 :where [?a :artist/country ?country]
 [?country :country/name ?c]]

;; Bind $ to the database and [?c ...] to ["Canada" "Japan"].
;; Illustrative result subset:
#{[{:artist/name "The Guess Who"}]
  [{:artist/name "Whiskey Howl"}]
  [{:artist/name "Yoko Ono"}]
  [{:artist/name "Asakawa Maki"}]}
```

## Use Pull to Retrieve Attribute Values

`pull` is usually the simplest way to retrieve a value for a specified attribute for a given entity. Using an [entity identifier](../05_query_and_pull/06_pull_reference.md), calling Pull is straightforward. For example, retrieve the documentation on an application attribute:

```rust
use edb_core::{EntityIdentifier, Keyword};
use edb_core::edn_pull::parse_pull_edn;

let documentation = db.pull(
    &parse_pull_edn("[:db/doc]")?,
    EntityIdentifier::Ident(Keyword::new("country", "code")),
)?;
```

If the attribute was installed with the earlier definition, the returned native map renders as:

```edn
{:db/doc "ISO 3166-1 alpha-3 country code"}
```

Use the `:where` clauses to *identify* entities of interest, combined with a Pull expression to *navigate* to attribute values for those entities:

```edn
[:find (pull ?e [:artist/name])
 :where [?e :artist/country :country/JP]]
```

This finds all artists from Japan then uses Pull to list their names. The results are self-documenting. An illustrative subset is:

```edn
#{[{:artist/name "Flower Travellin' Band"}]
  [{:artist/name "The Tigers"}]
  [{:artist/name "Jacks"}]}
```

Pull does not unify on attributes. While a `:where` clause omits an entity that lacks a specified attribute, Pull simply omits requested attributes that are missing:

```edn
[:find (pull ?e [:artist/name :artist/gender])
 :where [?e :artist/country :country/JP]]
```

This returns maps for artists that have no gender listed. If `:artist/gender` is a noncomponent reference, it is represented by a nested `:db/id` map unless you request deeper navigation:

```edn
#{[{:artist/name "Yoko Ono" :artist/gender {:db/id #edb/ref 17592186045591}}]
  [{:artist/name "Far Out"}]
  [{:artist/name "Asakawa Maki" :artist/gender {:db/id #edb/ref 17592186045591}}]}
```

The entity ID above is illustrative. To retrieve the enum's ident too, request `{:artist/gender [:db/ident]}` in the Pull pattern. See the [Pull reference](../05_query_and_pull/06_pull_reference.md).

## Put Blanks in Data Patterns

[Blanks](../05_query_and_pull/05_query_reference.md) can be used as placeholders that match anything but do not bind or unify. This music-catalog example finds all countries that have an artist in a music-catalog database, using a blank to match any artist:

```edn
[:find ?c
 :where [_ :artist/country ?c]]
```

Using a variable rather than the blank when you do not care about the value is an anti-pattern because:

- Blanks allow the query engine to avoid extra work tracking binding and unification for a dummy variable.
- The `_` makes clear your (lack of) intent for these values.

## Use Query Inputs to Parameterize Queries and Leverage Caching

EDB's native query engine can reuse schema-independent preparation using a bounded structural cache, and callers can retain a `PreparedQuery`. Sources, bound input values, callbacks and results are not cached there. Reusing a parameterized query avoids embedding each application's values into a new query structure. Equivalent native query structures can share cache entries when admitted; this does not guarantee a performance improvement for every workload.

The EDN adapter's `EdnQuery::bind` compiles a binding and creates a prepared query; it does not automatically reuse the native engine's global preparation cache. Parse a fixed EDN template once, pass input data separately, and retain a bound query when executing the same binding repeatedly. For explicit native preparation reuse and cache limits, see [query sources and reuse](../05_query_and_pull/00_queries.md).

The following query finds the names of all bands starting with `"B"`, of type *group*, with a start year of `1970`:

```edn
[:find ?name
 :where [?a :artist/type :artist.type/group]
        [?a :artist/name ?name]
        [?a :artist/startYear 1970]
        [(starts-with? ?name "B")]]
```

A common anti-pattern is to make repeated adjustments to the query's data structure to change what facts it will match, e.g.:

```edn
[:find ?name
 :where [?a :artist/type :artist.type/group]
        [?a :artist/name ?name]
        [?a :artist/startYear 1971]
        [(starts-with? ?name "C")]]
```

Instead, you should parameterize the query so that you can call it multiple times with different inputs:

```edn
[:find ?name
 :in $ ?year ?letter
 :where [?a :artist/type :artist.type/group]
        [?a :artist/name ?name]
        [?a :artist/startYear ?year]
        [(starts-with? ?name ?letter)]]

;; Bind $ to the database, ?year to 1971, and ?letter to "C".
```

## Work with Data Structures, Not Strings

Two features of Datalog queries help avoid SQL-injection-style mistakes:

- Queries are composed of data structures. Native `Query` values or `EdnValue` structures avoid the need to interpolate user text into query syntax.
- The query API is parameterized with data sources and ordinary values. In many cases, pass user data as an input instead of including it in the query itself.

Avoid building queries by reading a string constructed with concatenation or interpolation. Doing so gives up the security and simplicity of working with data structures. EDB also accepts EDN text, so keep that text fixed and pass user input separately:

```rust
use edb_core::edn::EdnValue;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{QueryControl, QuerySourceValue};

let query = parse_query_edn(
    "[:find ?e :in $ ?name :where [?e :artist/name ?name]]",
)?;
let user_name = "The Beatles";
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
    EdnQueryArgument::Data(EdnValue::String(user_name.into())),
])?;
let result = bound.execute(&QueryControl::default(), None)?;
```

Here `user_name` remains a string value even if it contains quotes or query-looking text. Do not substitute it into the EDN query with `format!`. `query_from_edn` is available when assembling a query directly from `EdnValue` structures. See [programming with data and EDN](02_programming_with_data_and_edn.md).

## Use a Consistent Db Value for a Unit of Work

A database value is immutable and multiple reads of database facts through `query` or `pull` on the same database value have a consistent basis. Use deterministic application callbacks when you also need reproducible computed results. The result of calling `Connection::db` will progress over time, and therefore calling the function repeatedly in sequence can retrieve facts that do not share a common time basis.

You should use a single database value for a unit of work in order to maintain consistency and reap the resulting benefits for testing, predictability, and composability.

## Specify t Instead of txInstant for Precise asOf Locations

EDB's logical `T` value exactly orders transactions in monotonically ascending order, with each transaction receiving its own `t` value. By contrast, wall clock times specified by `:db/txInstant` are imprecise as more than one transaction can be recorded in the same millisecond.

For filtering databases by time, establishing the relative order of events in a historical database, or any other time-based operation requiring exact precision, you should always use the `t` (or related `tx`) value.

For example, if you are interested in the wall clock time in which The Rolling Stones were added to a music-catalog database, you might use a query like this:

```edn
[:find ?txInstant
  :where [?a :artist/name "The Rolling Stones" ?tx]
         [?tx :db/txInstant ?txInstant]]
```

But if you want to use the log API to inspect the other data transacted along with this fact, you should get the transaction ID itself:

```edn
[:find ?tx
 :where [?a :artist/name "The Rolling Stones" ?tx]]
```

This transaction ID can be converted to a logical T with `edb_core::tx_to_t`, or passed directly to the log's exact-transaction API:

```rust
use edb_core::IndexTransaction;

let log = conn.log();
let datoms = log.tx_data(IndexTransaction::Tx(tx_id))?;
```

For an exact database time boundary use `db.clone().as_of(t)` or `as_of_time_point(TimePoint::Tx(tx_id))`. A wall-clock boundary can encompass several transactions with the same instant. See [database filters](03_database_filters.md).

## Use the History Filter for Audit Trail Queries

With the history filter, your database will have a view that spans time and sees retained historical datoms, including facts that have since been retracted. `noHistory` omission and excision can limit what remains available. Use the [history filter](03_database_filters.md) to answer audit trail questions such as:

- "What did we know about this customer when we confirmed this purchase?"
- "In which of these four-time ranges was this customer active?"

The following example uses a history database to retrieve retained purchase events for a user as well as the time the purchase was logged by the system. Since it uses a history database, it includes facts that were once asserted but have been later retracted:

```edn
;; query
[:find ?p ?inst ?added
 :in $ ?userid
 :where [?u :user/purchase ?p ?tx ?added]
 [?u :user/id ?userid]
 [?tx :db/txInstant ?inst]]

;; Bind $ to db.clone().history() and ?userid to the user identifier.
```

This will bind `?added` to `true` if the datom is an assertion and `false` if the datom is a retraction, returning results like:

```edn
#{["Bread" #inst "2014-11-18T03:41:59.301Z" false]
  ["Bread" #inst "2014-11-18T03:41:37.031Z" true]
  ["Soda" #inst "2014-11-18T03:41:39.617Z" true]
  ["French Fries" #inst "2014-11-18T03:41:56.115Z" true]}
```

You can see a canceled (retracted) order and explain why you sent a loaf of bread to shipping at 3:41:43 even though there was no longer a bread order in the shipping queue when the loaf arrived at 3:43:15.

## Pass Multiple Points-in-time to a Single Query

When using filters, you will often need to use multiple points in time for a database. A common mistake encountered with using the `since` filter, for example, is to omit the unfiltered (or differently filtered) database that is the basis for the facts about the identity of the entity for which the query is being made.

For example, use two points in time in the same query:

```edn
[:find ?count
 :in $ $since ?id
 :where [$ ?e :item/id ?id]
        [$since ?e :item/count ?count]]
```

Bind `$` to `db.clone()` and `$since` to `db.clone().since(t)`, with `?id` supplied separately. This query requires two databases as input. The item's id was transacted before the time range was defined by the `since` filter, so you look up the entity id from its domain id using the unfiltered `$` database. We then join that to the `:item/count` from the filtered `$since` database.

## Use the Log API If Time Is Your Most Selective Criterion

If you are most interested in retrieving values by or at specific transaction times, the [log API](../06_indexes/00_indexes_and_log.md) is the appropriate starting point. The log is ordered by logical transaction time and supports bounded traversal without expressing the time restriction as an arbitrary historical join. Measure actual performance for your workload.

`Connection::log()` captures an immutable log endpoint. `LogValue::tx_range(start, end)` returns a fallible cursor. Each `LogTransaction` has `t` and `data` fields containing its logical T and assertion/retraction datoms. Later commits do not extend the captured cursor.

For example, retrieve every datom from a particular logical transaction T:

```rust
use edb_core::TimePoint;

let log = conn.log();
let end_t = t.checked_add(1).expect("transaction boundary fits u64");
for transaction in log.tx_range(Some(TimePoint::T(t)), Some(TimePoint::T(end_t)))? {
    let transaction = transaction?;
    let datoms = transaction.data;
    // Process additions and retractions in this transaction.
}
```

The start is inclusive and the end is exclusive: `[t, t)` is empty, while `[t, t + 1)` selects that transaction if present. If you already have a transaction entity ID, `log.tx_data(IndexTransaction::Tx(tx_id))` is simpler and returns `None` when that transaction is absent from the captured endpoint. See [transaction logs and provenance](../05_query_and_pull/00_queries.md#transaction-logs-and-provenance) for joining log data inside a query.
