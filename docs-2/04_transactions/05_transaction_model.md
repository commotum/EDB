# Transaction Model

This page explains EDB’s transaction model and the motivations behind its design. It also attempts to preempt common misconceptions.

There are a number of potential causes for confusion when learning the model. First, its declarative semantics are less common and thus unfamiliar. Second, some EDB constructs superficially resemble constructs of e.g. SQL databases, which are very different in that they rely on imperative manipulation of a mutable database. Third, EDB often names data and functions with verbs which may suggest operations where there are none.

## It’s All About Information

An EDB database is *information*, specifically a set of atomic immutable statements. We call these statements in EDB *datoms*. There are two kinds of datoms, *additions* and *retractions*, named via `:db/add` and `:db/retract` in transaction data. Each datom associates an entity (E) with a value (V) via an attribute (A), similar to the subject/predicate/object of RDF. It also records the transaction (T), source (S), and assertion/retraction flag: `[e a v t s added?]`. The source is an ordinary entity id explicitly supplied for each assertion or retraction; one transaction can contain statements from several sources. In the example below, the entity Stu is related to the value `"pizza"` by the attribute `:person/likes`.

```edn
[{:db/id "edb.tx" :db/source 99}
 [:db/add "Stu" :person/likes "pizza" 99]]
```

Here `"Stu"` is a transaction-local tempid, `99` identifies an existing source entity, and `:person/likes` must already be installed as a string attribute. The metadata-only `"edb.tx"` map explicitly attributes the generated `:db/txInstant`; it does not provide a default source for other forms. To identify an existing Stu, use an entity id, ident or lookup ref instead. See [Transaction Data](04_transaction_data.md).

A transaction is *information*, specifically a set of assertions and retractions written as above. An EDB transaction is declarative. The `transact` APIs atomically accrue a transaction to a database. On their own, the datoms of a transaction ‘do’ nothing. They could instead be considered mere clauses named:

```text
[[:it-is-now-true-that Stu :person/likes "pizza" 99]
 [:it-is-no-longer-true-that Stu :person/likes "ice-cream" 99]]
```

These clause names explain the meaning; they are not EDB transaction syntax.

A *database value* represents the database at a particular transaction basis, with current facts and retained history. Ordinary transactions accrue new information (like a log or ledger). *A database is not a set of places that get updated*. The word value emphasizes that databases, like datoms, are immutable. For brevity, database value is often shorthanded as *db value* or even *db*. Current-state reads omit retracted facts; history exposes additions and retractions. Retention policies such as [noHistory](../03_schema/02_schema_reference.md#dbnohistory) and [excision](../08_operations/00_deployment.md#excision) mean that retaining every fact forever is not an unconditional promise.

An assertion of the same entity, attribute and value by a different source is not redundant: both assertions retain their attribution, while ordinary current-value reads still return one distinct value. Redundant assertions are compared by EAVS. A retraction removes the current EAV fact regardless of its original source; the retraction carries its own source and leaves the earlier assertions' attribution unchanged.

## with and transact

EDB is a *functional* database: databases are immutable values, and reads and speculative transactions operate on those values. `DatabaseValue::with`, `with_forms` and `with_edn` take a database value and a set of information held to be true at a point in time, and return a report containing a new database value that includes that information. They enforce the transaction’s semantic guarantees: checking the information set for internal consistency, validating it against data already in the database, unifying entity ids, and assigning the next logical transaction basis. The caller supplies the speculative transaction instant.

The declarative, set-oriented nature of `with` radically simplifies reasoning about data. The externally visible states are complete database values that have passed the applicable constraints. You never have to worry about the order things happened inside a transaction, because there is no application-visible sequence of intermediate database states. The assessor validates the proposed information set as a whole. This can only be done with the full set in hand and no presumption that one input form sees another form’s changes.

A transaction, as a noun, is the information set accrued to a database. Transact, as a verb, is the logical operation of atomically accruing an information set to a database. EDB’s speculative and durable paths share expansion, assessment and validation. Expansion turns maps and function calls into primitive transaction data against one db-before; assessment resolves identities and constraints and constructs a complete proposed db-after. Durable submission then publishes that successor and its receipt atomically under the active writer’s authority. A connection obtains immutable values of the durable database; a captured value does not change when another transaction commits.

For durable transactions, these boundaries provide the ACID properties:

- **Atomicity:** the complete transaction is published, or none of its facts become committed.
- **Consistency:** schema rules and requested application constraints must accept the proposed successor.
- **Isolation:** the writer serializes fresh transactions, each assessed against its exact predecessor. Readers retain complete immutable values.
- **Durability:** publication follows durable storage of the transaction and its receipt. A speculative `with` result has no such publication or receipt.

At the application level, a transaction is a way to record in the database something that happened in your organization—a purchase, registration, event etc.—by recording the ‘facts’ of the event. EDB is like a ledger. Ordinary transactions append assertions and retractions rather than exposing read/writeable ‘places’.

Any particular transaction successor is the product of the immediate predecessor value and the application, in toto, of a single transaction. The only database effect supported by `transact` is the acceptance of a complete set of transaction data. There are no smaller externally visible effects or operations within that transaction.

Speculation does not reserve entity ids or guarantee that a later submission will have the same db-before. Submit the original logical forms, not speculative ids or a datom diff. Use `TransactionRequest::comparing_basis` if a plan requires the live basis to remain unchanged. Filtered and temporal views assess against the full underlying basis and preserve their view on the result; history views cannot transact. Match the writer’s allocation defaults and native deployment when those affect the preview.

Durable requests also carry a caller-owned request key. After an ambiguous outcome, retry the identical request, including its explicit sources, with the same key. Receipt resolution precedes fresh assessment, so an accepted request returns its original outcome rather than applying the information a second time. Reusing the key for different intent conflicts. A confirmed commit can still have a local report-opening error; see [Transactions, Reports and Exact Retries](00_transactions.md).

## Application Correctness

Programs may wish to enforce business-rules validations at transaction boundaries. EDB supports such validations with [entity specs](../03_schema/02_schema_reference.md#entity-specs), whose entity predicates have access to db-after, the proposed end-of-transaction database value. An entity spec is checked when requested through `:db/ensure`; defining it does not automatically validate every entity in every transaction. Attribute predicates validate asserted values. Application correctness depends on the rules the application actually installs and requests.

Programs may wish to generate, transform or validate input data with reference to data already in the database. EDB supports such transformations and validations via [transaction functions](04_transaction_data.md#transaction-functions), which follow the pure-function model of functional programming with values. A transaction function is passed the value of db-before for this transaction, and its result is transaction data added to the other data in the transaction, but not incorporated in the db until transaction completion. Its returned assertions and retractions carry explicit sources. Thus deterministic transaction functions produce the same result regardless of the order in which they appear, and preserve the declarative nature of EDB transactions. They are not stored procedures manipulating intermediate states.

One way to think about transaction functions is that they are like the helper functions one might run in the application when creating the initial transaction data. Like those helpers, they only create data; they are not operations affecting a db. Unlike helpers run before submission, transaction functions run during assessment and are supplied with db-before, the value of the database immediately preceding the transaction in which they are invoked. This allows you to functionally increment values, confirm prerequisites etc. that you could not safely decide from an earlier application read, since you cannot know beforehand which db value your transaction will create the successor of.

Thus an EDB transaction function, however named (`compare-and-swap`, `create-customer` etc.), is not an operation modifying a database, nor a grouping of such operations, and does not ‘do’ anything to the durable database by itself. Verb-like names indicate the purpose of the data but do not imply effects.

The built-in `:db/cas` checks a cardinality-one attribute against db-before; `nil` as the expected value means absence. Its proposed new value participates in the same whole-transaction assessment as every other assertion. Its supplied source attributes both the assertion and any resulting retraction. Two different asserted values for one cardinality-one attribute fail rather than implementing “last one wins”; multiple sources asserting the same value still satisfy cardinality-one. Unique values cannot have multiple holders in the successor, while unique identity assertions may unify tempids with an existing entity. See [Value and Identity Rules](../03_schema/01_identity_and_values.md).

EDB supports [persisted programs](01_persisted_programs.md) and explicitly deployed [native Rust callbacks](../07_peer_api/02_native_computation.md). Rust callbacks are ordinary application code, so purity is a requirement on that code, not a guarantee that the language prevents external effects. Keep their results dependent on supplied arguments and database values. Send mail, charge cards and perform other external effects only through a separate workflow reacting to committed transactions.

## Integrity and Composition

Transactions are atomic and take effect at a monotonic logical point in time; therefore, they have integrity, in the sense of “being complete or undivided.” The information set of a transaction cannot be split or combined with another transaction while assuming its meaning remains the same. Splitting a transaction would incorrectly assert two different time points where there is only one, while combining two transactions would have the opposite problem. Such splits and combinations might violate any number of other domain constraints as well.

The indivisibility of transactions is a powerful guarantee, directly analogous to real-world contracts. When parties agree to a contract, they agree to all provisions of the contract at once, and would not in general be willing to agree to only an arbitrary sub- or superset of the provisions. The fact that I am willing to *sell* you my house does not mean that I am willing to *give* you my house. Like EDB transactions, real-world contract provisions must be written in some order but that order is incidental: contracts take effect all at once.

SQL DML describes an ordered batch of read-write operations, and concatenating such batches yields `DOabc + DOdef = DOabcdef`. EDB transactions are sets of assertions at a point in time and cannot be composed with the same interpretation: `info-true-at-Tj + info-true-at-Tk = info-true-at-T???`. Nor can they be decomposed into sub-time: there is no application-visible time within an EDB transaction; it represents a point. Concatenating arbitrary operations does not by itself guarantee that the resulting database states will be domain-correct.

Transaction data can be large and complex. You cannot split transactions into smaller transactions without risk of violating their semantics. And you cannot decompose them into a script of database mutations, because transaction forms do not expose intermediate effects. So what answer does EDB provide for managing complex transaction data? Declarative and functional programming.

EDB transaction functions and entity specs bring the power of declarative and functional programming to the challenge of assembling and validating transaction data. Rather than a limited, special-purpose DML, you have Rust application helpers and persisted programs. Further, you have declarative Datalog on complete database values. Composition is of functions and declarative queries *inside* a transaction function or entity predicate. Across transaction functions, returned forms are collected into the same transaction; one function does not read another function’s proposed successor. Entity predicates inspect the complete proposed db-after and accept or reject it, rather than passing a modified database to the next predicate.

## Why Do Things This Way?

EDB is designed to facilitate creating correct applications and reasoning about the information they produce. `transact` and speculative `with` produce transaction successors. Reads can also derive temporal or filtered views without creating transactions. Given a complete EDB database value, you know that:

- It represents current facts and retained history at a captured basis. Current, history and temporal reads expose different portions of that information.
- Each datom is associated with a transaction and an explicitly supplied source entity. Logical `basis_t` orders transactions; the transaction entity id and wall-clock `:db/txInstant` are distinct representations, and adjacent instants may be equal.
- A transaction successor is the product of its immediate predecessor and the atomic addition of a single transaction, in toto, representing a set of facts at a single indivisible point in logical time.
- Transactions and the log of their assertions are reified. You can make assertions about a transaction and use a datom’s transaction reference or the transaction log to associate facts with the transaction that asserted them, subject to retention. Each statement's source remains independent of transaction annotations.

EDB’s transaction APIs, including transaction functions and entity specs, use complete database values at their respective before/after boundaries. There is no smaller imperative DML supporting sub-transaction mutations, stored procedures grouping those mutations, mid-transaction database states, mid-transaction concurrency semantics or sub-transaction time. Exposing such states would compromise the properties above.

Nor are you constrained to constructing an ordered script when composing transaction data. You can use ordinary Rust functions to develop logic that operates on an immutable EDB db and returns data. The engine need not understand all of your application code to enforce its schema and transaction invariants. Persisted programs run within their supported instruction set and resource limits; native callbacks must honor their purity contract.

Since EDB programmers see database values incorporating complete transactions, they can work with data that has passed the schema and application checks required at those boundaries. This is stronger than merely preventing partially visible writes, although it does not supply business rules the application has not expressed. It makes working with and reasoning about EDB more straightforward than reasoning about intermediate mutations and their interactions.

The tradeoffs and benefits of EDB’s approach largely align with those of declarative and functional versus procedural programming. EDB prioritizes building simple, robust systems about which you can reason more readily.
