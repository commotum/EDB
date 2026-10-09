# EDB — Extensible Database

EDB is a general-purpose temporal database inspired by
[Datomic](https://docs.datomic.com/datomic-overview.html) and written entirely in
Rust. It stores immutable facts with explicit time and source attribution, for
applications whose records, relationships, and history matter.

Datomic made time part of the data model: a change preserves the earlier
statement and records what replaced or withdrew it. EDB extends that model to
source. Each fact records who supplied it, and each retraction records who
withdrew it. Attribution stays with the statement as the database changes.

**What Datomic did for time, EDB does for source: makes it explicit.**

## From EDN to EDB

[EDN, Extensible Data Notation](https://github.com/edn-format/edn), is a
language-neutral format based on
[Clojure's data syntax](https://clojure.org/reference/reader#_extensible_data_notation_edn)
and used by Datomic for data interchange. It represents values through numbers,
strings, keywords, collections, and tagged elements. Collections compose those
values; tags give new values a defined interpretation using the same notation.
Readers in different languages can exchange the data without a Clojure runtime.

EDB takes that approach from values to recorded knowledge. Facts connect
entities, attributes, and values; schema describes those attributes through
ordinary facts. Transactions and sources are entities too. The vocabulary can
grow through the same model used to describe the application itself.

That is the meaning of **Extensible Database**: new knowledge can bring new
attributes and relationships without requiring a different kind of database
structure. EDB uses EDN to express schema, transactions, queries, and Pull
patterns as data, alongside its typed Rust API.

## Facts with time and source

The unit of information is an immutable fact called a *datom*:

```text
[Entity  Attribute  Value  Transaction  Source  Added?]
```

| Dimension | Meaning |
| --- | --- |
| Entity | The subject the statement describes. |
| Attribute | The property or relationship being described. |
| Value | The information supplied. |
| Transaction | The change that asserted or retracted the fact. |
| Source | The contributor to whom that change is attributed. |
| Added? | `true` for an assertion; `false` for a retraction. |

An application supplies one source for each transaction. Every assertion and
retraction it produces carries that source, including generated changes. A
source can describe a person, an institution, an importer, or an LLM through
ordinary database attributes.

Consider a museum catalog. An importer supplies a painting's year, an LLM adds
a description, and Julia later corrects the year. These illustrative datoms use
symbolic entity and transaction IDs; assume `:painting/year` has cardinality one:

```edn
[painting :painting/year        1916             T1 catalog-importer true]
[painting :painting/description "Water lilies."  T2 llm              true]
[painting :painting/year        1916             T3 julia            false]
[painting :painting/year        1917             T3 julia            true]
```

Julia's retraction and replacement occur atomically. The current record has
year `1917` and the LLM's description. Retained history contains the importer's
original assertion and Julia's correction, each with its own attribution. The
same model answers “What did we know then?” and “Who supplied or withdrew it?”

Contributors maintain one shared record. If two sources assert the same value,
EDB preserves both contributions while exposing one value in the entity view.
A retraction withdraws that value across sources. Cardinality and uniqueness
apply to the shared record, and source-filtered current reads still honor
retractions by other contributors.

Source identifies who supplied the database statement. A painting's artist or a
book's publisher is described through domain attributes; the importer who
records those relationships is their source.

## The Datomic foundations

EDB carries forward Datomic's approach to data and application reads:

- **Immutable database values.** Capture a consistent view and pass it through
  application code. Later transactions do not change that value. As-of, since,
  and history views expose retained changes through the same query engine.
- **Schema as data.** Attributes declare types, cardinality, uniqueness, and
  other constraints. Their definitions are queryable entities, and any entity
  can have any installed attribute subject to those constraints.
- **Datalog and Pull.** Join facts with reusable rules, query relationships,
  and project nested results. Entity references connect records directly.
- **Atomic transactions.** One active transactor validates and serializes
  changes. Transaction functions and predicates can compute and check changes
  against the database; local `with` operations let applications inspect
  speculative results before submission.
- **Indexed facts and an accessible log.** EAVT, AEVT, AVET, and VAET support
  entity, attribute, value, and reverse-reference access. EDB adds SAEVT and
  SAVET for source access. Every index retains the complete datom; the log
  exposes transactions and their changes in chronological order.

## A native Rust engine

Datalog, Pull, entity navigation, transaction assessment, and indexing run in
the Rust engine. Application peers execute reads locally and cache immutable
index objects. Additional peers can serve independent read workloads, while
one active transactor orders writes for each database.

PostgreSQL provides durable storage for immutable objects and the references
that publish database state. Queries run in EDB; cold reads load the required
objects from storage. Immutable indexes share unchanged structure between
versions, and background indexing incorporates changes separately from each
commit. This architecture supports efficient reads through local execution,
working-set caches, and reuse of stored structure.

The native library, async adapters, and EDN command-line interface share the
same model and engine. In-memory databases support local exploration; portable
backups support offline reads. See the
[introduction](docs/00_start_here/00_introduction.md) for architecture and
deployment choices.

## Start here

EDB is in first-release development. Storage format changes may require fresh
databases; unsupported older formats are rejected explicitly.

- [Setup](docs/00_start_here/01_setup.md) and [peer library](docs/00_start_here/02_peer_library.md)
- [In-memory tutorial](docs/01_tutorials/02_in_memory.md)
- [EDN tutorial](docs/01_tutorials/00_edn_workflow.md)
- [Rust application tutorial](docs/01_tutorials/01_application_workflow.md)
- [Documentation reference](docs/README.md)
- [Deployment](docs/08_operations/00_deployment.md) and [administration](docs/08_operations/01_administration.md)
