I read all three documents in full and traced the original EDB code. **This strengthens the case for keeping source in the datom.** My earlier observation that “transaction determines source” was true, but I gave it too much weight. The more important question is what information every fact must retain, and what guarantees the database provides around that information.

The Datomic design gives us a useful way to answer that.

**The underlying motivation is to represent change without destroying the information needed to understand it.** An entity has a continuing identity, but its state differs over time. A database can preserve those states as stable values. Learning something new then produces a successor value; it does not alter the value that an earlier reader observed. This follows Hickey’s distinction between identity and state: an identity has different state values over time, while each value remains unchanged. [Clojure’s explanation of identity and state](https://clojure.org/about/state).

Consider a catalog entry. Yesterday it said the painting’s year was `1916`. Today a curator corrects it to `1917`.

There are two pieces of information worth retaining:

- The catalog previously recorded `1916`.
- The catalog now records `1917`.

The earlier statement can explain a report, decision, or publication made yesterday. Its usefulness survives the correction. Datomic’s history tutorial makes exactly this practical point: people may have acted on information before it was corrected. [Datomic history tutorial](https://docs.datomic.com/client-tutorial/history.html).

That is the root of the design as I understand it: **preserve information, and give programs stable information to reason about.** Historical queries, auditing, reproducibility, and safe sharing follow from that choice.

An **atomic fact** gives this preservation the right granularity. The painting’s title, year, artist, and location can change independently. Each attribute relationship has its own history. An entity-shaped map is a projection assembled from those relationships. This also lets the same underlying facts support entity lookup, relational queries, and graph navigation. [Your Datomic introduction](/home/jake/Developer/edb-old/ATOMIC/datomic_pro_docs/00_start_here/00_introduction.md:34).

“Atomic fact” and “atomic transaction” serve different purposes: the fact is a small unit of information; the transaction allows a set of related facts to enter the record indivisibly.

**Time gives each assertion or retraction a permanent position in the database’s history.** The logical datom is:

```text
[E A V Tx added?]
```

Using symbolic transaction IDs, the correction becomes:

```text
[painting :painting/year 1916 Tx1 true]

[painting :painting/year 1916 Tx2 false]
[painting :painting/year 1917 Tx2 true]
```

All three statements remain meaningful. At `Tx1`, the current year is `1916`. At `Tx2`, it is `1917`. The two statements in `Tx2` describe one atomic correction.

There are three related concepts that are easy to conflate:

| Concept | Meaning |
|---|---|
| `t` | The transaction’s position in the database’s strictly increasing logical order |
| `Tx` | The entity ID representing that transaction, carried by its datoms |
| `:db/txInstant` | The wall-clock timestamp recorded as an attribute of the transaction entity |

Several transactions can have the same wall-clock timestamp while still having different logical times. The original EDB explicitly converts between `t` and transaction entity IDs. [Datomic time guidance](https://docs.datomic.com/reference/best.html#specify-t-instead-of-txinstant-for-precise-asof-locations), [EDB conversion code](/home/jake/Developer/EDB/src/model/identity.rs:88).

This is **database transaction time**. The painting’s creation year remains domain data. Recording a correction today does not mean the painting was created today.

Why attach `Tx` to **each datom**, when every datom in the transaction shares it? Because datoms subsequently appear outside their original transaction grouping. They are organized by entity, attribute, value, and relationship; returned by queries; and examined individually. Their transaction association must survive all those uses.

That association provides both exact temporal context and a route to additional information about the transaction. Datomic can attach an import file, submitting application, or other metadata to the transaction entity and reach it from any datom. [Reified transactions](https://docs.datomic.com/transactions/transaction-data-reference.html#reified-transactions).

The distinction between the **information model and its physical representation** is important here.

Datomic presents ordered sets of datoms through four indexes:

| Index | Principal access pattern |
|---|---|
| EAVT | Find facts about an entity |
| AEVT | Find entities and values for an attribute |
| AVET | Find entities by an attribute/value combination |
| VAET | Traverse reference relationships backwards |

Within those orders, transaction time sorts descending. The log supplies the complementary access pattern: transactions in time order. These structures let readers approach the same information through different questions. [Datomic indexes](https://docs.datomic.com/indexes/index-model.html), [time-oriented log access](https://docs.datomic.com/reference/best.html#use-the-log-api-if-time-is-your-most-selective-criterion).

The durable indexes use shallow, immutable trees of segments. New versions can share unchanged structure. Recent transaction data is merged with the indexed data during reads, allowing durable indexing to happen in the background. Immutable segments can be cached without their contents becoming stale; acquiring a newer database value is a separate operation. Accumulation is the logical guarantee—Datomic explicitly distinguishes it from an append-only physical layout. [Index implementation](https://docs.datomic.com/indexes/index-model.html#implementation).

The original EDB makes this concrete:

- Its [datom structure](/home/jake/Developer/EDB/src/model/datom.rs:8) includes `tx`.
- Its [durable index leaves](/home/jake/Developer/EDB/src/index/tree/mod.rs:3838) store transaction coordinates alongside the other fields.
- Its [log entries](/home/jake/Developer/EDB/src/storage/log.rs:29) contain transaction basis information and the resulting datoms.
- Its [recent indexes](/home/jake/Developer/EDB/src/index/recent/btset.rs:14) share transaction objects and refer to individual datoms within them.

That last point is instructive: the same logical datom can participate in several access structures without requiring a separate allocation of all its data in each one.

**The transaction model completes the design.** A transaction describes a set of proposed facts. There is a total order between transactions, but the forms inside one transaction do not constitute a sequence of intermediate database mutations.

Conceptually:

```text
with(db_before, transaction_data) → db_after
```

The database assesses the complete proposed change, resolves identities, applies constraints, and computes an immutable successor. Committing adds durable atomic publication. Transaction functions help produce the proposed data while observing `db_before`. The application sees complete database values on either side. [Datomic transaction model](https://docs.datomic.com/transactions/model.html).

The original EDB follows this separation: its [assessor](/home/jake/Developer/EDB/src/transaction/assess/mod.rs:415) computes a successor, while [durable authority](/home/jake/Developer/EDB/src/transactor/authority/mod.rs:653) handles storage and publication.

The EDN document fits here: queries and transactions are ordinary data structures that programs can construct, inspect, combine, and transmit. EDN is their textual representation. The temporal and transactional guarantees come from how the database interprets that data. [Programming with data and EDN](/home/jake/Developer/edb-old/ATOMIC/datomic_pro_docs/02_core_concepts/01_programming_with_data_and_edn.md:9).

Together, these choices simplify several otherwise difficult application problems:

| Problem | What the model provides |
|---|---|
| Several reads must agree while writes continue | Pass the same immutable database value to every read |
| Investigate an earlier decision | Query the retained database state used at that time |
| Explain a correction | Inspect the original assertion and subsequent retraction |
| Preview a proposed change | Compute and query a speculative successor |
| Share data among readers | Share immutable values and cache immutable segments |
| Consume changes incrementally | Read an ordered transaction log containing recorded effects |

These benefits depend on the whole design: explicit time, immutable values, indexed access, and atomic publication. Historical views reuse the ordinary query machinery; speculative EDB values reuse its ordinary read interfaces. [Datomic database views](https://docs.datomic.com/reference/filters.html), [EDB database values](/home/jake/Developer/EDB/src/database_value/value.rs:16).

**For source, the strongest analogy is preserving context that EDB considers essential to the meaning of a statement.**

Transaction granularity determines attribution. Fact granularity determines where that attribution remains available.

Under our model:

```text
[E A V Tx S added?]
```

Every statement has a contributor, including a generated statement or retraction. A consumer receives that attribution through the common datom model, without needing an application-specific convention for transaction annotations.

There is also a substantive difference from merely adding transaction metadata. Ordinary Datomic suppresses a redundant assertion of an already-current EAV, even when submitted in a later transaction. The original EDB does the same. Consequently, adding source metadata to a later transaction does not by itself preserve a second assertion of the unchanged painting year. [Datomic redundancy elimination](https://docs.datomic.com/transactions/transaction-data-reference.html#redundancy-elimination), [original EDB materiality check](/home/jake/Developer/EDB/src/transaction/assess/mod.rs:2448).

EDB’s source model deliberately changes that rule:

```text
[painting :painting/year 1916 Tx1 llm    true]
[painting :painting/year 1916 Tx2 museum true]
```

The museum’s contribution remains information even though the year is unchanged. **That preservation rule, together with mandatory attribution, justifies source’s place in the logical datom.**

Time and source have different jobs: time orders changes; source identifies their contributor. Neither the shared transaction boundary nor a possible alternative storage encoding diminishes the usefulness of making source universally available. I recommend keeping the six-slot model and evaluating additional indexes after cutover, as you suggest.

For the remaining current-state question, I think we can use a compact rule:

> **Determine whether a fact is current across all sources, then select its surviving contributions by source.**

For each EAV:

1. An assertion from a new source adds a distinct contribution.
2. Reasserting it from the same source while that contribution remains current is redundant.
3. A retraction withdraws all current contributions to that EAV.
4. Assertions in later transactions can establish it again.

For example:

| Transaction | Recorded change | Current value | Current contributors |
|---|---|---|---|
| Tx1 | LLM asserts `1916` | `1916` | LLM |
| Tx2 | Museum asserts `1916` | `1916` | LLM, museum |
| Tx3 | Curator retracts `1916` and asserts `1917` | `1917` | Curator |

Cardinality counts distinct values. Uniqueness counts distinct entity holders. CAS checks the current value. Pull projects distinct values. History preserves all four datoms in this example.

A current query for museum contributions returns no year after `Tx3`; a historical query still finds the museum’s `1916` assertion. Filtering out the curator’s events **before** resolving current state would answer a different question and could revive withdrawn information. That operation must remain distinct from ordinary source selection.

The original EDB already rejects asserting and retracting the same EAV in one transaction, so we can preserve its unordered transaction semantics. [Existing conflict rule](/home/jake/Developer/EDB/src/transaction/assess/mod.rs:2704). Conceptually, current contributions are the assertions after the latest EAV retraction; indexes and current-state projections can implement that without replaying the entire history for every read.

That gives us a small semantic rule to implement consistently across the engine—and a direct set of examples against which to prove it.
