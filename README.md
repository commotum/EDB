# EDB — Extensible Database

AI development is hampered by having to predefine structures and evaluations
before we know what we are looking for. We need to capture observations, explore
relationships, and revise our questions as we learn. Our database should make
room for that discovery.

**EDB is an extensible database: a counterpart to EDN, Extensible Data Notation.**
EDN provides a language of values. EDB builds on that foundation with a language
of facts, their descriptions, their sources, and their changes through time. New knowledge
should fit without requiring a new kind of database structure.

Our design starts with three requirements: **small, self-descriptive, and
composable.**

## 1. Small

The fundamental unit is a fact:

```text
[Entity  Attribute  Value  Transaction  Source  Added?]
```

Each transaction has one explicitly supplied source. Every assertion and
retraction it produces retains that source alongside its transaction, with
`true` for an assertion and `false` for a retraction.

An entity's identity answers “What thing are we talking about?” Its facts answer
“What do we know about it?” Learning something new adds facts without redefining
the thing or prescribing every attribute it might ever have.

**The vocabulary can grow without continually enlarging the rules for
constructing it.** Small describes the common foundation, not a limit on what
we can express.

## 2. Self-descriptive

The database contains descriptions of its own attributes. Names, types,
cardinalities, and documentation are data, expressed through the same model and
transaction mechanism as application facts.

An attribute is itself an entity with facts describing it. Programs can inspect
those definitions as the vocabulary grows. Schema becomes part of the knowledge
we can query and extend.

**New information arrives with a way to describe it.**

## 3. Composable

Facts combine without losing their identities or meanings. One person can have
customer and employee attributes; adding either does not change what
`:person/name` means. References connect that person to an organization whose
own facts and relationships remain accessible.

**A whole can participate as a part, while its own parts remain understandable.**
New combinations use the same described relationships. Datalog joins and reusable
rules carry that principle into asking questions.

## Knowledge grows through time and contribution

Changes are assertions and retractions. Source identifies who supplied those
statements, while domain attributes describe distinct roles such as authorship
and publishing. Transactions and sources are entities too, described with
ordinary facts. Immutable database values let us ask questions against a
consistent point in time; retained history preserves earlier statements and
their attribution.

Contributors maintain one shared record. Two sources supplying the same value
remain distinct contributions without multiplying the value. A later retraction
withdraws that value across sources and records who withdrew it.

Extensibility still requires discipline: attributes have constraints, and changes
must preserve the model's invariants. The aim is a foundation that supports
learning without requiring us to anticipate everything we will learn.

**The database can know more without requiring a new way of saying what it knows.**

## Start here

EDB is implemented in Rust, with PostgreSQL providing durable storage. It is in
first-release development; format changes may require fresh databases.

- [Guide](docs-2/00_start_here/00_introduction.md)
- [Setup](docs-2/00_start_here/01_setup.md) and [peer library](docs-2/00_start_here/02_peer_library.md)
- [Documentation reference](docs-2/README.md)
- [In-memory tutorial](docs-2/01_tutorials/02_in_memory.md) and [durable peer tutorial](docs-2/01_tutorials/03_peer_tutorial.md)
- [EDN tutorial](docs-2/01_tutorials/00_edn_workflow.md)
- [Rust application tutorial](docs-2/01_tutorials/01_application_workflow.md)
- [Deployment](docs-2/08_operations/00_deployment.md) and [administration](docs-2/08_operations/01_administration.md)
