# EDB North Star

**EDB exists to preserve knowledge with its source intact. Source belongs with the fact, first-class alongside time, and survives through change.**

Datomic made time fundamental to the data model. Each assertion and retraction has a place in history. A later change can alter the current view while preserving the earlier statements. Keeping that history recognizes that what we knew before still matters after our knowledge changes. EDB extends that commitment to source: where information came from matters, and it should remain available as the database grows.

EDB is a database of record for collaboratively maintained catalogs and applications. Contributors update one shared current record under the same schema and cardinality rules. History preserves earlier assertions and the changes that replaced or withdrew them. EDB does not maintain a separate current account of the world for each source.

We build knowledge together. A single record can contain an observation from Julia, facts supplied by a catalog importer, an estimate from an LLM, and a correction from a curator. Each transaction has one source. Contributions from different sources arrive in separate transactions, and their attribution remains with the facts as the record grows.

**What Datomic did for time, EDB will do for source: make it an explicit dimension of the fact and preserve it through change.** This is the commitment that guides our design and implementation decisions.

[Model](#the-data-model) · [Glossary](#glossary) · [Examples](#examples) · [Design principles](#design-principles) · [Foundations](#foundations)

## The data model

A datom expresses six distinct dimensions:

```text
[e a v t s ±]
[entity attribute value transaction source added?]
```

The final slot is a boolean: `true` for assertion and `false` for retraction. `±` is only shorthand for that slot.

| Slot | Question | Meaning |
| --- | --- | --- |
| Entity (`e`) | Where? | Where the information belongs: the subject it describes. |
| Attribute (`a`) | Why? | The role the information plays, its purpose in the model. |
| Value (`v`) | What? | The information being supplied. |
| Transaction (`t`) | When? | When the statement enters the database's history. |
| Source (`s`) | Who? | The source responsible for the transaction that asserted or retracted the fact. |
| Added? (`±`) | How? | A boolean: assertion (`true`) or retraction (`false`). |

Why attach `S` to **each datom**, when every datom in the transaction shares it? Because datoms subsequently appear outside their original transaction grouping. They are organized by entity, attribute, value, and relationship; returned by queries; and examined individually. Their source attribution must survive all those uses.

The model carries these commitments:

- **One explicit source per transaction.** The application supplies one source entity id for the transaction. Every assertion and retraction it produces carries that source, including facts produced by transaction functions and database expansion.
- **Immutable attribution.** Julia's assertion remains Julia's when Jake retracts it. Jake's retraction carries Jake's attribution. Later contributions add statements to history.
- **Shared records.** Facts from different sources can belong to the same entity through separate transactions. Each transaction applies its changes atomically to the shared database of record.
- **Source is information.** Even when two sources supply the same value, the difference in source is information worth preserving.
- **History remains available.** The current view contains the current record; history retains how that record developed and who contributed each change.
- **One uniform mechanism.** Every domain attribute participates in the same source model. Relationships distinct from the contribution itself, such as artistic credits or a book's publisher, remain ordinary domain attributes.
- **Automatic transaction time.** EDB selects and records the transaction instant using its existing clock rules. The timestamp assertion carries the transaction's source, just like every other fact it produces. An explicit timestamp value for an import remains subject to the existing time bounds.

## Glossary

**Source** — The entity to whom a transaction and all of its assertions and retractions are attributed. The application explicitly supplies its id once for the transaction. A source can represent a person, an importing agent, a function, an LLM, or another contributor to the database. Its description consists of ordinary database facts. Source identifies the contributor of the database statements. Distinct relationships within their subject matter, such as a book's author and publisher, are described with domain attributes.

**Simplicity** — "One fold." A component concerns a distinct responsibility, concept, or dimension without entangling independent concerns. Rich: "What matters for simplicity is that there is no interleaving, not that there's only one thing, and that's very important." Simplicity is objective: we can examine the structure to identify entanglement. — [Rich Hickey, Simple Made Easy, 00:03:19](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md).

**Ease** — "Near, at hand"; "Near to our understanding / skill set"; "Near our capabilities." Something is available to us, familiar to us, or within our ability to use and understand. "Easy is relative": it depends on the person, their tools, their experience, and their capabilities. — [Rich Hickey, Simplicity Matters, 00:03:34](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md).

## Examples

### Peaches: history and attributed retractions

Assume `:stock-status` has cardinality one. Entity and transaction names below are symbolic ids. T0 is a starting point in a much larger history of transactions; earlier history is omitted.

| Transaction | What happens |
| --- | --- |
| T0 | Jason last asserted that peaches are in stock. |
| T1 | Julia sells the last peach and records that peaches are out of stock. |
| T2 | Jake accepts a delivery the next morning and records that peaches are in stock. |

**Without a source slot:** `[entity attribute value transaction added?]`.

```clojure
;; T0
[peaches :stock-status :in-stock     T0 true]

;; T1 — Julia's change
[peaches :stock-status :in-stock     T1 false]
[peaches :stock-status :out-of-stock T1 true]

;; T2 — Jake's change
[peaches :stock-status :out-of-stock T2 false]
[peaches :stock-status :in-stock     T2 true]
```

The names in the comments aren't recorded in these datoms.

**With source:** `[entity attribute value transaction source added?]`.

```clojure
;; T0 — assume Jason last asserted that peaches are in stock; earlier history omitted
[peaches :stock-status :in-stock     T0 jason true]

;; T1 — Julia's change
[peaches :stock-status :in-stock     T1 julia false]
[peaches :stock-status :out-of-stock T1 julia true]

;; T2 — Jake's change
[peaches :stock-status :out-of-stock T2 jake  false]
[peaches :stock-status :in-stock     T2 jake  true]
```

Each transaction's retraction and assertion happen atomically. All five datoms shown remain in history; the current value progresses from `:in-stock` to `:out-of-stock` to `:in-stock`. Jake's retraction records Jake's attribution without changing Julia's earlier assertion.

### Museum catalog: contributions to one record

An illustrative painting's catalog entry is assembled through several transactions:

| Transaction | Source | Facts supplied |
| --- | --- | --- |
| T1 | Catalog importer | Artist: Claude Monet; title: Water Lilies; date: 1916 |
| T2 | LLM | Description: "A luminous…" |
| T3 | Julia | Location: Gallery 4 |

The importer supplies the artist, title, and date together in one atomic transaction. All three assertions carry the importer's source id. The description and location arrive in later transactions with their own sources. The current painting record contains all five facts.

Later, a curator corrects the date and writes a new description in one transaction. Its new assertions and retractions carry the curator's source; the other current facts retain their sources. The earlier assertions remain in history with their original attribution.

We can ask, "Which descriptions were supplied by the LLM?", "Which facts did this importer supply?", or "What did the curator contribute?" using the same source dimension. A citation identifying the publication used by an importer is an ordinary domain relationship.

One entity can contain knowledge from many sources, and each fact can follow its own history. Attribution of database changes uses the same mechanism across attributes.

### Music and books: authorship and source

A music catalog records who wrote, performed, and produced a recording through attributes such as `:song/writer`, `:recording/artist`, and `:recording/producer`. Those credits describe the music. Source identifies who supplied the database statements about it. An application can also use source as authorship when the database facts are themselves the work being authored, or when that single attribution is sufficient.

For an illustrative recording, a library importer supplies these facts in transaction T1. The names below are symbolic entity ids:

```clojure
[song      :song/writer        songwriter T1 library-importer true]
[recording :recording/song     song       T1 library-importer true]
[recording :recording/artist   performer  T1 library-importer true]
[recording :recording/producer producer   T1 library-importer true]
```

The importer did not write, perform, or produce the music. It supplied the catalog statements. The credited people remain values of their respective attributes; the importer is the source of every assertion in T1.

If Julia later corrects the producer credit, her transaction retracts the old credit and asserts the replacement. Both changes carry Julia's source id. The original assertion remains in history with the importer's attribution. Julia does not become the recording's producer by correcting its record.

The same distinction applies to literature. A catalog can record a book's author and publisher with `:book/author` and `:book/publisher` attributes. If Jake adds those facts, Jake is their source; the author and publisher remain the entities identified by the attribute values. These relationships describe the book; source attributes the catalog statements without attempting to describe the complete provenance of the work.

### Same fact, different source

A different source is meaningful even when the entity, attribute, and value are unchanged. Assume `:painting/year` has cardinality one.

```clojure
;; An LLM supplies the painting's year.
[painting :painting/year 1916 T1 llm            true]

;; The museum later supplies the same year.
[painting :painting/year 1916 T2 museum-catalog true]
```

Both assertions remain in history with their respective sources. The current year is still `1916`, and cardinality-one still holds.

The second transaction adds knowledge about the source. Matching EAV does not make that contribution redundant. Both transactions contribute to the same current record; cardinality-one does not create a separate current year for each source.

### Shared travel itinerary: a retraction before a replacement

| Attribute | Value | Source |
| --- | --- | --- |
| Departure time | 09:30 | Ferry operator |
| Meeting point | North entrance | Julia |
| Journey time | 45 minutes | LLM |
| Reservation | ABC123 | Booking importer |

Each row's source supplies its contribution in a separate transaction. A transaction may supply several itinerary facts, all attributed to its one source.

Jake discovers that the north entrance is closed. He retracts Julia's meeting point before the group has chosen another one. Later, Jason supplies a new meeting point.

The itinerary can show what is confirmed, estimated, or contributed by a traveling companion. Its history also records who withdrew a plan, even during the interval when there is no replacement value. Julia's original assertion retains her attribution; Jake's retraction carries his attribution; Jason's later assertion carries his.

## Design principles

**Make independent things independent.** Rich Hickey asks us to notice "interconnections between things that could be independent." Attribution of database changes and authorship within the application domain are distinct concerns. A transaction names its source once; domain attributes describe the work, people, and relationships the application records. — [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md).

**Apply one model across domains.** "Good design is about taking things apart." Giving source its own coordinate separates concerns that applications otherwise have to manage together. The museum catalog, the fruit stand, and the shared itinerary should all benefit from the same mechanism. Each can develop its own domain model while relying on a common way to retain attribution. — [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md).

**Judge simplicity by structure.** "Simplicity is not about counting." A transaction's source applies uniformly to its explicit and generated facts. Applications do not repeat attribution on each operation or coordinate separate sources for automatic changes. Source remains a durable part of each fact, available through the same model in current and historical views. — [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md).

**Preserve the ability to change.** "Simplicity enables change." We want applications to be able to ask where information came from, distinguish contributions, and examine corrections using the facts the database already preserves. — [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md).

## Foundations

### Source is ordinary database data

Source uses the same entity model already used for attributes, transactions, and reference values. An attribute is itself an entity, described by facts such as its `:db/ident`, `:db/valueType`, and `:db/cardinality`. A transaction is also an entity, described by facts such as its `:db/txInstant`. The source position likewise points to an ordinary entity, described by ordinary facts.

For example, defining `:person/name` and then describing Julia looks like this in the source-aware model. The ids are symbolic; assume Jake supplied these facts.

```clojure
;; Install the attribute through facts in an earlier transaction.
[person-name-attribute :db/ident       :person/name        T-schema jake true]
[person-name-attribute :db/valueType   :db.type/string     T-schema jake true]
[person-name-attribute :db/cardinality :db.cardinality/one T-schema jake true]

;; Describe the same Julia entity used in the source position in the peaches example.
[julia :person/name "Julia" T-person jake true]
```

Julia is the entity being described in the last datom; Jake is the source of the transaction that supplies that descriptive fact. In the peaches datoms, Julia occupies the source position instead. It is the same entity id in either position.

The universal schema means any entity can have any installed attribute, subject to its constraints. A source entity can represent a person, Math Academy, an LLM, or whatever source the application needs to describe. Its description is database data, with its own history and attribution. The source position is a built-in part of the datom; the attributes describing its referenced entity use the ordinary schema. Scalar values still remain scalar values, and reference values point to entities.

The database can use the same tools to describe, query, and connect sources that it uses for subjects, attributes, and transactions.

### Indexes and the transaction log

The information model defines what EDB preserves; indexes arrange that information for different questions. Every index retains the complete datom, `[e a v t s added?]`, so its transaction and source remain available through every access path.

EDB maintains six datom index orders:

| Index | Principal access pattern |
| --- | --- |
| EAVT | Find facts about an entity, across all sources. |
| AEVT | Find entities and values for an attribute, across all sources. |
| AVET | Find entities by an attribute/value combination or value range, across all sources. |
| VAET | Traverse reference relationships backwards, across all sources. |
| SAEVT | Find a source's contributions, optionally restricted to an attribute. |
| SAVET | Find a source's contributions matching an attribute/value combination or value range. |

Index names describe the leading sort components. Entity, attribute, value, and source sort ascending; transaction sorts descending within each order. The four orders beginning with entity, attribute, or value let applications query the shared catalog without knowing who supplied its facts. The source-first orders let them select a contributor directly.

EAVT, AEVT, and SAEVT cover all attributes. AVET and SAVET cover attributes marked `:db/index true` or `:db/unique`; VAET covers reference attributes. Source changes the available access paths while cardinality, uniqueness, and identity continue to apply across the shared database.

All current indexes describe the same current record. When a fact has contributions from several sources, a retraction withdraws that value across all of them. Restricting a query to one source must still honor retractions by other sources. History retains the original assertions and the retracting transaction's attribution.

#### Source activity

The transaction log provides chronological access to complete transactions, including their assertions and retractions. A source's activity timeline uses SAEVT to locate its transactions and the log to retrieve their contents.

Every submitted transaction has one immutable timestamp assertion, carrying the transaction's source:

```text
[tx :db/txInstant instant tx source true]
```

Within SAEVT, fixing source and attribute to `[source :db/txInstant]` leaves entries ordered by entity. Those entities are transaction entities, whose IDs preserve logical transaction order. The range therefore contains one timestamp entry per transaction from that source. Reverse traversal gives the most recent activity first, and each entry identifies the transaction to retrieve from the log.

This supplies source-and-time access through the six datom orders and the log. Applications access source activity without depending on this physical access path. Index traversal and log reads use the same captured database basis. Pagination follows transaction order; timestamps supply the displayed dates. Earlier contributions remain part of the timeline after their facts are replaced or retracted in the current record.

#### Immutable storage

Durable indexes use shallow trees of immutable segments. New versions share unchanged structure, and reads merge recent transaction data with the indexed data so durable indexing can proceed in the background. Immutable segments can be cached without their contents becoming stale; acquiring a newer database value is a separate operation.

The log records transaction data, durable index leaves carry datom coordinates, and recent indexes can share transaction objects while referring to individual datoms within them. A fact can participate in several access structures without a separate allocation of its complete data in each one.

Accumulation is the information guarantee. Physical index maintenance builds new projections of that information, sharing or replacing storage structures as needed. Removing an entry from a new current index does not rewrite the assertion's attribution or manufacture a historical retraction; the transaction's recorded changes determine the history.

### Existing EDB foundations

- [Universal Schema](02_core_concepts/01_data_model.md#universal-schema): "Any entity can then have any attribute. Because all datoms are part of a single relation, this is called a *universal schema*."
- [Defining Schema](03_schema/02_schema_reference.md#defining-schema): "Attributes are defined using the same data model used for application data. That is, attributes are themselves defined by entities with associated attributes and are added to the database through an ordinary transaction."
- [Time Model](02_core_concepts/01_data_model.md#time-model): "Transaction ids are themselves entity ids, so transactions can have datoms that describe them."
- [Grammar Notes](03_schema/02_schema_reference.md#grammar-notes): "Just like all other entities in EDB, schema entities are open and can have any attributes you define added to them."

### The role of Rich Hickey's talks

| Talk | Role in our work |
| --- | --- |
| [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md) | Defines simplicity and gives us a criterion for judging whether we have removed entanglement. |
| [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md) | Explains the payoff: a system we can understand, change, and extend. |
| [Design in Practice](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/DesignInPractice.md) | Guides the reasoning process through which we turn the North Star into design and implementation decisions. |
