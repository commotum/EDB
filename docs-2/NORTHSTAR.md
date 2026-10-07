# EDB North Star

**EDB exists to preserve knowledge with its source intact. Source belongs with the fact, first-class alongside time, and survives through change.**

Datomic made time fundamental to the data model. Each assertion and retraction has a place in history. A later change can alter the current view while preserving the earlier statements. Keeping that history recognizes that what we knew before still matters after our knowledge changes. EDB extends that commitment to source: where information came from matters, and it should remain available as the database grows.

We build knowledge together. A single record can contain an observation from Julia, a fact from a museum catalog, an estimate from an LLM, and a correction from a curator. Those contributions have independent origins even when they describe the same subject or arrive in the same transaction. The database should preserve those distinctions directly.

**What Datomic did for time, EDB will do for source: make it an explicit dimension of the fact and preserve it through change.** This is the commitment that guides our design and implementation decisions.

[Model](#the-data-model) · [Glossary](#glossary) · [Examples](#worked-examples) · [Design principles](#design-principles) · [Foundations](#foundations)

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
| Source (`s`) | Who? | The source behind the assertion or retraction, the agent to whom it is attributed. |
| Added? (`±`) | How? | A boolean: assertion (`true`) or retraction (`false`). |

The model carries these commitments:

- **Explicit source.** The application supplies a source entity id for each assertion or retraction.
- **Immutable attribution.** Julia's assertion remains Julia's when Jake retracts it. Jake's retraction carries Jake's attribution. Later contributions add statements to history.
- **Independent sources.** Facts from different sources can belong to the same entity and commit together in one atomic transaction.
- **Source is information.** Even when two sources supply the same value, the difference in source is information worth preserving.
- **History remains available.** A current view can select what applies now while history retains how that knowledge developed and who contributed it.
- **One uniform mechanism.** Every domain attribute participates in the same source model. Adding an attribute gives us another attributable fact without requiring another authorship scheme.

## Glossary

**Source** — The entity to whom an assertion or retraction is attributed. The application explicitly supplies its id. A source can represent a person, an institution, a publication, an LLM, or another origin of information. Its description consists of ordinary database facts.

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

### Museum catalog: several sources in one transaction

An illustrative painting's catalog entry is assembled from several sources:

| Attribute | Value | Source |
| --- | --- | --- |
| Artist | Claude Monet | Museum catalog |
| Title | Water Lilies | Museum catalog |
| Date | 1916 | Scholarly publication |
| Description | "A luminous…" | LLM |
| Location | Gallery 4 | Julia |

These can all arrive in one atomic transaction. Each fact carries its own source, independently of transaction batching.

Later, a curator corrects the date and writes a new description. The new date and description assertions carry the curator's attribution; the other facts retain their sources. The earlier assertions remain in history with their original attribution, alongside the attributed retractions.

We can ask, "Which descriptions are generated?", "Which dates came from this publication?", or "What did the curator contribute?" using the same source dimension.

One entity can contain knowledge from many sources, and each fact can follow its own history. No bespoke `:date-author`, `:description-author`, or `:location-author` machinery.

### Same fact, different source

A different source is meaningful even when the entity, attribute, and value are unchanged. Assume `:painting/year` has cardinality one.

```clojure
;; An LLM supplies the painting's year.
[painting :painting/year 1916 T1 llm            true]

;; The museum later supplies the same year.
[painting :painting/year 1916 T2 museum-catalog true]
```

Both assertions remain in history with their respective sources. The current year is still `1916`, and cardinality-one still holds.

The second assertion adds knowledge about the source. Matching EAV does not make that contribution redundant.

### Shared travel itinerary: a retraction before a replacement

| Attribute | Value | Source |
| --- | --- | --- |
| Departure time | 09:30 | Ferry operator |
| Meeting point | North entrance | Julia |
| Journey time | 45 minutes | LLM estimate |
| Reservation | ABC123 | Booking confirmation |

Jake discovers that the north entrance is closed. He retracts Julia's meeting point before the group has chosen another one. Later, Jason supplies a new meeting point.

The itinerary can show what is confirmed, estimated, or contributed by a traveling companion. Its history also records who withdrew a plan, even during the interval when there is no replacement value. Julia's original assertion retains her attribution; Jake's retraction carries his attribution; Jason's later assertion carries his.

## Design principles

**Make independent things independent.** Rich Hickey asks us to notice "interconnections between things that could be independent." This is the entanglement we want to remove: attribution should be independent of transaction batching and the particular domain attributes an application defines. The decision to commit facts together should leave their individual sources intact. — [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md).

**Apply one model across domains.** "Good design is about taking things apart." Giving source its own coordinate separates concerns that applications otherwise have to manage together. The museum catalog, the fruit stand, and the shared itinerary should all benefit from the same mechanism. Each can develop its own domain model while relying on a common way to retain attribution. — [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md).

**Judge simplicity by structure.** "Simplicity is not about counting." Giving source its own coordinate removes dependencies and repeated special cases. It makes the database and its applications easier to understand, reason about, and extend by making attribution independent of transaction batching and domain-specific authorship schemes. Source is a uniform, durable part of the information model. Our implementation decisions must preserve and strengthen that independence. — [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md).

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

Julia is the entity being described in the last datom; Jake is the declared author of that descriptive fact. In the peaches datoms, Julia occupies the source position instead. It is the same entity id in either position.

The universal schema means any entity can have any installed attribute, subject to its constraints. A source entity can represent a person, Math Academy, an LLM, or whatever source the application needs to describe. Its description is database data, with its own history and attribution. The source position is a built-in part of the datom; the attributes describing its referenced entity use the ordinary schema. Scalar values still remain scalar values, and reference values point to entities.

The database can use the same tools to describe, query, and connect sources that it uses for subjects, attributes, and transactions.

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
