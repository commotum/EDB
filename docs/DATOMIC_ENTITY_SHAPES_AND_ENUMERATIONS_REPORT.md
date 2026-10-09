# Datomic, EDB, entity shapes, and enumerations

Investigation date: 2026-10-08. EDB baseline: `source-first-class-cutover`, commit `9ab251ed7a8058257b5d559e5700a8f5b33920c3`.

**EDB already has the representation and much of the transaction machinery needed for named shapes and enum domains. What it does not yet have is a supported interpretation of those descriptions.** The difficult work is defining what a description means, which database state it describes, and what a requested check promises. Nested syntax is a useful authoring interface to that work; it is not the whole feature.

I read all six files in [the schema documentation](/home/jake/Developer/EDB-source/docs/03_schema) in full, along with [NORTHSTAR](/home/jake/Developer/EDB-source/docs/NORTHSTAR.md), the earlier [time-and-source report](/home/jake/Developer/EDB-source/docs/DATOMIC_TIME_AND_SOURCE_REPORT.md), and the [nested question example](/home/jake/Developer/Course_Academy/schema/nested-question.edn). I traced the relevant Rust implementation and regression tests, checked Datomic's public documentation, and ran focused EDB tests and small investigative probes. Datomic statements below describe its published contract; I did not run a Datomic binary or inspect its proprietary implementation. Recommendations and design consequences are identified as such.

**The underlying requirement is to preserve schema knowledge that people currently carry outside the database.** A question designer knows that an answer-field reference leads to an answer field, that its choices lead to answers, and that difficulty draws from a particular vocabulary. The current database can preserve the relationships and their values without preserving those intended descriptions as recognized schema concepts.

We need to distinguish six things:

| Concept | Meaning |
|---|---|
| Attribute definition | The meaning and admission rules of one independently usable attribute. |
| Entity shape | A named description of attributes and relationships considered together. |
| Enum domain | An explicitly declared set of member identities or values. |
| Reference | A connection to an entity; currently it does not specify a target shape or domain. |
| Component relationship | Navigation and lifecycle behavior associated with a parent–child reference. |
| Conformance policy | Which checks run, against which state, and whether failure prevents a transaction. |

These concepts can be native EDB facilities while their definitions remain ordinary database facts. They do not all need to become primitive value kinds.

**Datomic chooses globally defined attributes and open entities.** Its data model allows any entity to have any installed attribute. Attribute namespaces suggest useful groupings without establishing exclusive record types. An entity is assembled from its facts rather than stored as a fixed record containing all its fields. A program can use one attribute without adopting an aggregate description of the entity. [Datomic data model](https://docs.datomic.com/data-model.html), [Datomic schema best practices](https://docs.datomic.com/reference/best.html).

The tradeoff, as I interpret these documented choices, is that reusable attribute meanings and generic access take precedence over automatically discovering complete application records. A namespace alone cannot tell a form generator which fields to display or tell a validator which target structure a ref intends. That missing information is the opportunity for this feature; it is not evidence that the atomic fact model is deficient.

**Schema is data, but selected schema facts have native consequences.** In Datomic, an attribute is an entity described by `:db/ident`, `:db/valueType`, and `:db/cardinality`, with optional constraints and annotations. Schema entities can carry other installed attributes. An enum member or entity spec is also an entity, but naming one does not make it an attribute. [Datomic Schema Reference](https://docs.datomic.com/schema/schema-reference.html).

EDB makes the distinction explicit in code. Its [`ValueType`](/home/jake/Developer/EDB-source/src/model/schema.rs:34) is a fixed enumeration. Its [`Attribute`](/home/jake/Developer/EDB-source/src/model/schema.rs:61) descriptor contains the recognized type, cardinality, uniqueness, index, component, history, fulltext, tuple, and predicate properties. Its [`Schema`](/home/jake/Developer/EDB-source/src/model/schema.rs:130) collects those descriptors and derived name/dependency maps. An arbitrary new ident used as `:db/valueType` is rejected with `schema/unknown-value-type`.

This is already an example of native meaning over ordinary data. The engine does not treat every map that happens to have a name as a new value type. The decoding of value-type identities is deliberately restricted in [`value_type_for_entity`](/home/jake/Developer/EDB-source/src/model/vocabulary.rs:133).

For example, these three entities play different roles:

```edn
;; An attribute: its values must be entity references.
{:db/ident :question/difficulty
 :db/valueType :db.type/ref
 :db/cardinality :db.cardinality/one}

;; A named entity used as an enum member.
{:db/ident :question.difficulty/easy}

;; A named entity containing requirements for an explicitly requested check.
{:db/ident :question/validate
 :db.entity/attrs [:question/id :question/problem]}
```

The first has native attribute semantics. The second has an identity and a name. The third supplies requirements to `:db/ensure`. None declares a fundamental question entity type. The example assumes the required question attributes are installed before the spec is used.

**There are two layers of schema in EDB: recorded information and derived working structures.** The recorded information is authoritative. The working structures make interpretation and checking efficient.

| Layer | Current implementation | Consequence for the proposal |
|---|---|---|
| Schema facts | Ordinary attributed datoms, including application-defined metadata | Shape membership and target associations can use the existing information model. |
| Attribute projection | `Schema::derive_from_information` reconstructs native attribute descriptors | New metadata does not automatically enter this projection. |
| Name projection | Historical ident assertions produce the resident ident dictionary | Renaming and alias behavior must be understood when identifying domains and shapes. |
| Durable metadata | Index metadata retains and reconstructs a selected schema working set | Native descriptor caches need deliberate reconstruction/invalidation or an ordinary-data reader. |
| Physical indexes | Schema intent controls eligibility; physical readiness is tracked separately | A new schema fact does not magically make every desired access path ready. |

The relevant code is [`derive_from_information`](/home/jake/Developer/EDB-source/src/model/schema.rs:575), [`IdentIndex`](/home/jake/Developer/EDB-source/src/model/idents.rs:23), and [durable metadata reconstruction](/home/jake/Developer/EDB-source/src/index/metadata.rs:435). [`datom_may_change_schema`](/home/jake/Developer/EDB-source/src/model/schema.rs:174) deliberately recognizes only selected operational properties. Arbitrary annotations, documentation, and entity-spec requirements are not all loaded into `Attribute`.

Therefore, a native shape facility could read definitions from ordinary facts when requested, or maintain a separate immutable projection for frequent inspection and checking. Either can be legitimate. Calling the definitions “schema data” does not settle cache invalidation, recovery, read cost, or admission limits. Conversely, keeping definitions outside the always-resident attribute descriptor does not prevent the engine from understanding them: existing entity specs are read when an ensure is resolved.

**A ref guarantees considerably less than an application object type.** In EDB, reference input resolves through an entity ID, ident, lookup ref, or transaction-local tempid. An unknown ident fails. A lookup ref requires a unique attribute and an existing match in db-before. Numeric IDs undergo partition and issued-coordinate validation, but that is not a check that the target currently has facts or satisfies any domain description. See [entity resolution](/home/jake/Developer/EDB-source/src/transaction/assess/mod.rs:2059) and [numeric ID validation](/home/jake/Developer/EDB-source/src/transaction/assess/mod.rs:2110).

Thus `:question/difficulty` can currently refer to `:answer.type/math` if that ident exists. It is a valid entity reference even though it is the wrong domain choice. This is directly covered by [`db_before_binding_and_representation_choice_remain_ordinary`](/home/jake/Developer/EDB-source/tests/data_modeling_cutover.rs:248). It is also reproduced by the investigative probe after loading the demo's descriptive metadata.

Cardinality-one means at most one distinct current value, not mandatory presence. A ref does not require populated target attributes. A component ref does not require exclusive ownership or a particular target shape. These are distinct contracts, and the proposed feature should not silently collapse them.

**Datomic's enum convention names members, not the collection containing them.** Its documented approach uses ident-bearing entities as members and ref-valued attributes as links to those members. An unknown member ident fails to resolve; the ref declaration does not itself supply a named finite domain. [Datomic enum modeling](https://docs.datomic.com/schema/schema-modeling.html).

That choice keeps each member independently identifiable and available for ordinary relationships. It also permits metadata on a member, such as a label. The tradeoff is that a consumer cannot derive the intended choice set from the ref type alone. Treating a keyword namespace as membership would be an application convention, not the declared relationship the proposed facility needs.

Datomic also makes an explicit memory tradeoff for names: all idents are resident in compute nodes, supporting fast resolution. It recommends them for schema and enum vocabulary, while ordinary business identities use domain-specific unique attributes. We should distinguish a small named choice set from an unbounded catalog before requiring every possible member to have an ident. [Datomic ident guidance](https://docs.datomic.com/schema/schema-reference.html#db-ident).

In EDB, using a keyword-valued attribute instead would store the keyword directly, without requiring an ident entity or providing reference navigation. A string would be another choice with different identity and navigation behavior. The existing [modeling guide](/home/jake/Developer/EDB-source/docs/03_schema/04_data_modeling.md:105) explains those alternatives. Preserving refs for the proposed enum domains therefore preserves meaningful identity and query behavior, not merely a byte encoding.

**Names and membership must remain distinct.** Datomic documents stable entity IDs, programmatic idents, and unique domain keys as different tools. Unique identity supports tempid upsert; unique value checks occupancy without merging a new tempid. Lookup refs resolve against db-before. [Datomic identity and uniqueness](https://docs.datomic.com/schema/identity.html).

EDB's ident dictionary folds historical ident assertions. Renaming preserves the old name as an alias; retracting the current ident does not erase all names by which the entity can resolve. Later reuse of an alias can redirect that name. A current ident fact, an alias lookup, and enum membership therefore answer different questions. See [`IdentIndex::apply_assertion`](/home/jake/Developer/EDB-source/src/model/idents.rs:55) and [the alias regression](/home/jake/Developer/EDB-source/tests/identity_uniqueness_source_cutover.rs:482).

My recommendation is that domain membership and target associations use entity identity. Retiring a member should change explicit membership or availability facts, not rely on deleting its name. An enum member could belong to several domains unless we explicitly require otherwise. Domain membership alone need not make that same entity compare unequal depending on the attribute through which it was reached.

There is also a concrete API implication: current lazy entity navigation renders an ident-bearing forward target as a keyword, while Pull can explicitly request its attributes. A native inspection API should return a well-defined description rather than accidentally inherit whichever convenience representation a caller happened to use. [EDB identity API](/home/jake/Developer/EDB-source/docs/03_schema/05_identity_and_uniqueness.md:90).

**Datomic already separates several forms of consistency checking.** Transaction functions inspect db-before and contribute transaction data. Required attributes and entity predicates check the proposed result when requested. Entity predicates receive db-after. The checks participate in the atomic transaction boundary. [Datomic transaction model](https://docs.datomic.com/transactions/model.html), [Datomic ACID](https://docs.datomic.com/transactions/acid.html).

Datomic's `:db/ensure` is virtual: it requests a check rather than storing an enduring conformance assertion. Changing a spec does not retroactively validate existing entities. [Datomic entity specs](https://docs.datomic.com/schema/schema-reference.html#entity-specs).

One sentence in Datomic's Schema Reference mentions db-before while discussing entity specs. Its grammar says db-after, and the ACID, transaction-function, and transaction-composition documentation consistently describe db-after for entity predicates. I treat the isolated sentence as a documentation inconsistency, not an alternate validation phase. [Consistency mechanisms](https://docs.datomic.com/transactions/transaction-functions.html), [Composing transactions](https://docs.datomic.com/tech-notes/composing-transactions-by-example.html).

EDB's implementation resolves that distinction precisely:

| Mechanism | Definition/binding used | Data examined | Activation |
|---|---|---|---|
| Attribute type and transaction interpretation | db-before schema | Submitted values and resolved operations | Ordinary transaction processing |
| Schema transition checks | Proposed successor schema | Relevant resulting facts | A schema change invokes applicable transition checks |
| Attribute predicates | db-before attribute/predicate binding | Each material added datom's value | Applies to relevant assertions; not a whole-entity audit |
| Required attributes | Spec resolved from db-before | Proposed successor's current values | Explicit ensure request |
| Entity predicates | Spec/binding resolved from db-before | Complete proposed db-after and target ID | Explicit ensure request |

The distinction between interpretation and successor validation is essential. A transaction that changes one-to-many cardinality still uses the old replacement behavior while interpreting its data. A transaction that changes component status still expands retraction under the old flag. The successor must then satisfy applicable new invariants. Tests exercise both behaviors in [schema evolution](/home/jake/Developer/EDB-source/tests/changing_schema_source_cutover.rs:186).

**The EDB transaction pipeline already provides a common place for requested conformance.** Speculative and durable paths share the selective assessor rather than implementing different schema semantics.

```text
EDN/native forms, interpreted using db-before
    ↓
map and transaction-function expansion
    ↓
identity/upsert resolution and one transaction source
    ↓
logical changes, replacement/cascade effects, tuple derivation, timestamp
    ↓
successor schema and applicable invariant checks
    ↓
immutable proposed db-after and exact predicate checks
    ↓
speculative result or durable publication
```

This is a conceptual sequence, not a promise that every check runs only once. Required attributes are checked during assessment and again at the exact predicate boundary. The code is in [the shared assessor](/home/jake/Developer/EDB-source/src/transaction/assess/mod.rs:448), [`validate_exact`](/home/jake/Developer/EDB-source/src/transaction/assess/mod.rs:119), and [transaction execution](/home/jake/Developer/EDB-source/src/transaction/pipeline.rs:354).

An entity spec can use existing facts to satisfy a requirement; the required value need not appear in the submitted map. A retraction can make a requirement fail. A derived composite tuple can satisfy it. Requiredness for a many-valued attribute means at least one remaining value. It does not mean a nonempty string, a correct mathematical expression, or a valid target entity. Predicate results must be literal true; missing code and execution faults are errors. These behaviors are tested in [the spec and predicate suite](/home/jake/Developer/EDB-source/tests/schema_source_cutover.rs:585).

Attribute predicates are not a drop-in generic enum-domain checker. Their standard input is a value, without a database argument. An application can hard-code a set, but then the database's mutable membership facts and the deployed set become separate definitions. A native membership checker needs the relevant definition and database context.

EDB also does not currently require a special spec-kind marker. Ensuring an ident-only entity with no requirements succeeds vacuously; an investigative test confirmed this. That is understandable for the current permissive spec model, but new named descriptions need a decision about distinguishing a deliberately empty shape from an unknown, incomplete, or wrong-kind definition. A misspelled descriptor must not accidentally become successful validation.

**The present nested demo is closer to ordinary transaction data than our earlier discussion implied.** Datomic treats nested maps as an abbreviation for facts about separate entities plus connecting refs. EDB's expander follows the same general approach. Nesting on a ref is allowed when the child has suitable identity or is a component; it is not a new scalar map value. [Datomic transaction data](https://docs.datomic.com/transactions/transaction-data-reference.html), [EDB nested expansion](/home/jake/Developer/EDB-source/src/transaction/expand.rs:426).

I tested the actual question demo against the current source:

| Probe | Observed result |
|---|---|
| Submit the demo, wrapped in an ordinary transaction vector, before installing its metadata attributes | Rejected with `schema/unknown-attribute`. |
| First install `:schema/attributes`, `:schema/target`, `:schema/enum`, and `:schema/specs` as ordinary ref attributes, then submit it | Rejected with `transaction/unknown-ident` for the document-local named reference. |
| Give the inline answer shape a tempid and use that same tempid for `:answer-field/correct`'s target association | Existing speculative assessment accepts the whole nested graph. Both target links reach the same named shape; enum membership is retained as ordinary facts in db-after. |
| Assign `:answer.type/math` as a question's difficulty afterward | Accepted: the metadata does not yet activate domain checking. |
| Use `:answer/schema` as a new attribute's `:db/valueType` | Rejected with `schema/unknown-value-type`. |

The second failure is specifically about `:schema/target :answer/schema`: that name is first introduced inside the same document. Ordinary idents resolve in db-before, regardless of which map appears first. Tempids already express this connection correctly. A convenient schema document reader could collect local definitions and lower local references to shared tempids. This does not require changing ordinary transaction name resolution.

The metadata edges in this experiment were noncomponent refs. Named definitions already have unique identity through `:db/ident`; nesting does not require giving their containing declarations ownership. Marking these edges as components would be an unrelated and dangerous semantic choice for reusable definitions: retracting one description could cascade into shared definitions.

The remaining loader questions are concrete: how do local names interact with existing idents, what happens with conflicting duplicate definitions, how are forward/cyclic references resolved, and what exact input does the retry key identify? Equivalent nested and flat documents should produce equivalent information, but that need not imply identical request digests or allocated IDs. EDB's existing exact-retry contract must remain explicit.

**A vector in the document is not necessarily an ordered list or a replacement operation.** With cardinality-many metadata, member and attribute lists become sets. Reasserting a shorter list does not remove omitted members. Updating `[easy moderate hard]` to `[easy hard]` through ordinary map assertions leaves `moderate` in the database unless it is explicitly retracted.

A replacement-style schema loader would consequently be another operation: it must calculate retractions against a basis and define how it handles concurrent changes. A display order likewise requires explicit information or a specified presentation rule; it does not survive merely because members were written in a vector. These are authoring semantics we should settle before promising that a file is the complete current definition.

**Time exposes the most important qualification to our earlier phrase “snapshot-specific conformance.”** Datomic states that time travel does not rewind a database value's working schema. Its explanation is that one working schema accompanies the basis and historical supporting index infrastructure may no longer exist. Cardinality changes can therefore change entity navigation of old facts. [Datomic changing-schema rules](https://docs.datomic.com/schema/schema-change.html).

EDB deliberately follows that distinction. [`DatabaseValue::schema`](/home/jake/Developer/EDB-source/src/database_value/value.rs:293) returns the captured basis's schema. Its ident dictionary also belongs to that basis. [`as_of`](/home/jake/Developer/EDB-source/src/database_value/views.rs:347) changes a fact-visibility boundary; it does not rebuild either structure. A regression demonstrates old multiple values through raw reads while the same view's entity projection follows newer cardinality-one schema. [Temporal cardinality test](/home/jake/Developer/EDB-source/tests/changing_schema_source_cutover.rs:109).

These are different questions:

```text
Retained database value D1:
    D1 facts, D1 working schema, D1 name-resolution context.

Later value D2 viewed as_of D1's time:
    historical fact visibility, D2 working schema and name-resolution context.

Proposed conformance question:
    Which definition revision should describe which visible entity state?
```

If shape definitions are read as ordinary facts through the historical view, we can see historical shape membership alongside a newer operative attribute schema. If shape definitions are instead read from a resident basis projection, we may check old data against the latest definition at that basis. Neither behavior follows merely from the word “snapshot.” We must specify definition selection, data visibility, and identifier interpretation. An explicit request to compare old data against a newer contract could also be useful, but it should be identifiable as that operation.

There is another trap: EDB's speculative `with` removes temporal/custom read filters before assessment and reattaches the original view afterward. It is not a branch of past state. Therefore a read-only conformance API cannot simply issue a speculative ensure and claim it checked the caller's historical or source-filtered view. See [`speculation_base`](/home/jake/Developer/EDB-source/src/database_value/views.rs:215).

**Schema evolution already accepts deliberate limits.** Datomic recommends additive schema growth, stable meanings, and new names where meanings change. It also encourages annotation of schema entities. This lets consumers evolve without requiring one universal record definition to change in lockstep. [Datomic production schema guidance](https://docs.datomic.com/reference/best.html).

In EDB, existing value types, fulltext flags, and tuple definitions are immutable after installation. Installed attributes cannot simply be removed. Many-to-one and uniqueness transitions must validate the resulting facts; uniqueness over an existing population also has index-readiness requirements. AVET and SAVET share readiness. Composite installation can be followed by paced backfill; it does not instantly derive values for every old entity. See [schema transition code](/home/jake/Developer/EDB-source/src/transaction/assess/mod.rs:1005) and [the complete evolution guide](/home/jake/Developer/EDB-source/docs/03_schema/03_changing_schema.md).

These choices illustrate different accepted costs: immutable type meaning instead of reinterpretation in place, explicit migration instead of hidden conversion, and staged physical work instead of assuming that a schema declaration instantly prepares all access paths. A shape or enum facility needs equally explicit evolution rules. Adding a required field narrows conformance. Adding an enum member widens membership, but may still break an application that assumed exhaustive choices. Additive database facts do not imply compatibility for every consumer.

Tuples are a useful contrast. They really are values in V, with their own equality, ordering, arity, nil-slot, and scalar-element rules. A profile assembled from independently changing facts is not the same kind of thing. Substituting a tuple or an opaque structured value would change the granularity of updates and attribution. The current proposal does not require that change. [EDB tuple and value semantics](/home/jake/Developer/EDB-source/docs/03_schema/01_identity_and_values.md:137).

**Datomic has another relevant precedent: Analytics metaschema.** It describes table membership, columns, and joins over ordinary Datomic entities. A membership attribute selects entities, and join declarations connect reference attributes to destination tables. This is an existing consumer-specific structural description, not an entity-type system or a reference-target conformance guarantee. It demonstrates that useful aggregate descriptions can coexist with the underlying open model. [Datomic Analytics metaschema](https://docs.datomic.com/analytics/analytics-metaschema.html).

The proposed EDB facility would differ by giving shapes and domains a general, supported meaning for inspection and checking. We should not claim Datomic lacks every form of structural metadata, or that its choices prove all richer shape features are philosophically wrong. Its documented facilities establish precedents and boundaries; they do not supply our complete design.

**Our question example identifies the checks that a shape must not pretend to solve automatically.**

| Requirement | Present mechanism or remaining responsibility |
|---|---|
| An answer has an ID, type, and value | Explicit `:answer/validate` required-attribute spec. |
| An answer's type belongs to the declared answer-type domain | Currently an application rule; a native enum checker could supply it. |
| A field's `correct` ref reaches an answer-shaped entity | Proposed target-shape check. |
| That answer belongs to this particular field's choices | Cross-relationship rule; shape conformance alone is insufficient. |
| Field keys are unique within a question | Contextual uniqueness rule; global attribute uniqueness would mean something else. |
| A blank answer preserves malformed learner input | Storage contract: string presence is intentionally weaker than mathematical validity. |
| Retracting the question retracts its owned fields and choices | Existing component lifecycle behavior. |

A useful native facility should remove repeated generic checking without pretending that domain-specific relationships disappear. The current question files already distinguish storage requirements, automatic-grading requirements, and application validation. Preserving those distinctions matters more than making every rule look like a type declaration.

**Before implementation, the following decisions need executable answers.** These are design questions exposed by the investigation, not existing promises.

| Question | Why it changes the design | Recommended starting position |
|---|---|---|
| What identifies a shape or enum definition? | Missing properties currently can produce an empty spec; a native API needs reliable kind/error recognition. | Recognize valid definitions explicitly; distinguish unknown, malformed, and intentionally empty descriptions. |
| Is a shape also a spec, or does it refer to specs? | The demo's `:schema/specs` currently says checks are available, not which must run. | Reuse existing requiredness machinery; define exactly which checks an explicit shape check invokes. |
| Does listing a field require it? | An optional-field catalog and a required-key contract serve different purposes. | Keep presence requirements explicit. |
| Is a target annotation global or specific to one shape's use of an attribute? | A fact on the attribute applies everywhere. Different per-shape targets require a separate field-use description. | Begin with global meanings; reject conflicting redefinitions rather than silently overriding them. |
| Are enum members refs only? | Literal scalar domains introduce value-equality questions, including EDB decimal and cross-numeric semantics. | Begin with existing enum entity identities; keep a member's identity independent of its domain. |
| Can a member belong to several domains, and can a domain be empty? | Membership, exclusivity, and definition validity are separate rules. | Permit shared membership unless the domain requires otherwise; specify empty-domain behavior explicitly. |
| What does loading a revised document mean? | Ordinary assertions accumulate members; omission is not deletion. | Preserve transaction semantics; make any reconciliation/replacement operation explicit. |
| Which definition is checked against which data state? | Temporal views retain working schema and names from their basis. | Publish a precise basis/view contract; follow db-before definition activation for transaction checks unless deliberately changed. |
| Is checking shallow or recursive? | Indentation is not an execution policy; graph traversal can encounter shared children and cycles. | Define separate direct and graph obligations rather than infer recursion from nesting. |
| Which database views are admitted? | History is an event relation; source/custom/since views can omit required evidence. | Specify supported views and how incomplete information is reported; never claim a partial-view result proves full-record conformance. |
| How do definitions evolve? | Requiredness, membership, targets, and attached predicates can change answers for old data. | Keep revision provenance and distinguish audit/recheck from automatic migration. |
| What does a failed check return? | Nonconformance differs from unreadable storage, missing code, unknown definitions, and exhausted budgets. | Return structured, bounded diagnostics; do not collapse operational failure into false. |
| How are definitions accessed and cached? | Always-resident projections, demand reads, and bounded caches have different costs and recovery obligations. | Use measured access patterns; key any cache by immutable definition/data context, not only a mutable name. |
| Is conformance continuously maintained? | Target-only changes and definition changes can violate a contract without touching its referring attribute. | Treat ongoing enforcement as a separately selected guarantee. |

Recursive checking requires more than a depth limit. A traversal visiting the same entity under two shapes must not mistake the second check for a duplicate. A useful visited key includes the entity, shape, and relevant definition/state context. Cycles may be rejected, handled by a defined fixed-point rule, or supported only for a restricted language; arbitrary predicates make a blanket “already visited means valid” rule unsafe. Shared subgraphs should not trigger unbounded repeated work.

These costs matter on the writer. Datomic explicitly cautions that transaction functions and entity predicates run in the serialized transaction pipeline. EDB also provides cooperative work, read, and cancellation controls through its native execution machinery. Generic shape checking needs to participate in those controls and produce deterministic, bounded error paths. [Datomic transaction functions](https://docs.datomic.com/transactions/transaction-functions.html), [EDB native execution](/home/jake/Developer/EDB-source/src/transaction/native.rs:201), [predicate binding and execution](/home/jake/Developer/EDB-source/src/program_bindings.rs:581).

**Checking once and maintaining a guarantee remain different responsibilities.** Consider an answer shape requiring `:answer/value`:

```text
T1: A field's correct ref reaches an answer with a value. A requested check passes.
T2: Someone retracts the answer's value without changing the field's correct ref.
```

The existing explicit-check model accepts T2 unless a relevant check is requested. An always-conforming typed-reference contract must reject T2, or accept an atomic change that also removes or repairs the obligation. A shape-definition edit or enum-membership retraction can have the same effect across many untouched entities.

Incoming ref indexes can help find dependents, but they do not by themselves encode which contracts apply, which definition revisions matter, or all dependencies of arbitrary predicates. Enabling such a contract over existing data would also need an audit and publication rule. This is feasible design work if required; checking only when a ref is first asserted would not fulfill the promise.

**Source participates through the existing shared-record semantics.** Schema descriptions and enum membership facts can retain who supplied them. If two sources contribute the same membership edge, there is still one member in the logical set. A later retraction withdraws that edge globally, preserving both the earlier contributions and the retraction's source in history. Requiredness and membership must operate on distinct current values, not raw datom counts. [`DatabaseValue::values`](/home/jake/Developer/EDB-source/src/database_value/resolve.rs:67) already provides that distinction.

A shape check should therefore not accidentally become an independent-belief check for each source. A source-filtered view may omit a required fact supplied by someone else. Whether a caller deliberately wants to check that partial view is a separate API decision. Current truth must still be resolved before attribution filtering, as required by [NORTHSTAR](/home/jake/Developer/EDB-source/docs/NORTHSTAR.md:220).

Conformance evidence also has its own context. A result should identify the entity state and definition used; if it invokes native predicates, reproducibility also depends on the deployed predicate implementation. Persisting an optional audit result would record that a check occurred under that context, not grant the entity permanent validity.

**The implementation boundary I favor is a native schema facility over the current model.** It would preserve named definitions and associations, provide consistent inspection, and share one conformance evaluator between read-only checks and explicitly requested transaction checks, with the context differences made explicit. It would preserve ordinary refs, independent attributes, open entities, component semantics, and existing query access.

Clojure's spec rationale is relevant supporting evidence: it separates attribute specifications from aggregate keysets and advocates reusable, named specifications with general checking operations. That supports this decomposition; it does not settle EDB's temporal, transactional, or graph semantics. [Clojure spec rationale](https://clojure.org/about/spec).

I would not start by adding `Value::Enum` or `Value::ShapedEntity`. Such variants would require a distinct semantic reason, including decisions about equality, hashing, ordering, lookup, joins, Pull, and reference traversal, followed by codec and format work. The requirement identified here is satisfied by adding supported interpretation of existing information. That is a substantive engine feature, not merely documentation or pretty syntax.

Keeping the existing representation still leaves operational obligations: bootstrap the supported vocabulary, define descriptor admission and activation, preserve exact retries, handle renamed/reused names, make eager/block/overlay reads agree, and recover definitions correctly after restart or restore. It avoids changing the value model; it does not eliminate engineering work.

**The next design artifact should be a small set of precise contracts and examples.** We should be able to specify the expected result for each of these before writing the feature:

- The same shape declared inline or separately, with a forward reference and with a conflicting duplicate definition.
- One entity satisfying two shapes while carrying additional attributes.
- An absent optional ref versus a present ref to an empty entity; explicit requiredness versus an optional field listing.
- An enum member renamed, shared by two domains, removed from one domain, and contributed by two sources.
- A shape definition changed in the same transaction that requests checking.
- Historical data checked under a specified older or newer definition, and a partial source view checked with explicit limitations.
- A graph containing a cycle and a shared child, with a bounded diagnostic for failure.
- A revised file omitting an earlier member, with unambiguous additive or replacement behavior.
- A successful explicit check followed by an unchecked target edit, contrasted with any separately promised ongoing contract.

Those examples would decide the semantics that syntax alone cannot express. They also supply acceptance cases for implementation without replacing design with an expansive feature checklist.

**Evidence and limits.** All six schema documents were read completely: [schema introduction](/home/jake/Developer/EDB-source/docs/03_schema/00_schema.md), [identity and values](/home/jake/Developer/EDB-source/docs/03_schema/01_identity_and_values.md), [schema reference](/home/jake/Developer/EDB-source/docs/03_schema/02_schema_reference.md), [changing schema](/home/jake/Developer/EDB-source/docs/03_schema/03_changing_schema.md), [data modeling](/home/jake/Developer/EDB-source/docs/03_schema/04_data_modeling.md), and [identity and uniqueness](/home/jake/Developer/EDB-source/docs/03_schema/05_identity_and_uniqueness.md). Together they contain 1,846 lines at the inspected commit.

The focused runs passed 24 distinct existing tests: 14 in `schema_source_cutover`, four in `changing_schema_source_cutover`, three in `data_modeling_cutover`, and three in `transaction_data_source_cutover`. One live PostgreSQL test in the transaction-data suite remained ignored. Four additional temporary investigative tests passed, covering the nested demo's actual admission, retention as ordinary metadata in the speculative result, lack of implicit domain/type semantics, and empty-spec behavior. Existing tests cover both source-aware constraints and schema timing; the probes establish the limited behavior stated above, not readiness of an unimplemented shape feature.

The temporary probe source is [schema_report_probes.rs](/tmp/edb-schema-study-20261008/tests/schema_report_probes.rs); the principal logs are [schema suites](/tmp/edb-schema-investigation-tests.log), [transaction-data suite](/tmp/edb-schema-investigation-transaction-data.log), and [probe results](/tmp/edb-schema-investigation-probes.log). Builds and probes used temporary artifacts and speculative databases. No database service, production data, Course_Academy runtime, or EDB implementation was changed for this investigation.
