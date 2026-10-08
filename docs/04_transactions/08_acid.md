# ACID

EDB's durable transaction path provides Atomicity, Consistency, Isolation, and Durability through immutable database values and guarded PostgreSQL publication. This document defines the four components of ACID, explains how EDB works, and explores the implications of EDB's approach. Speculative `with` calls share transaction assessment, but do not commit anything to durable storage.

## Atomicity

Atomicity requires that each transaction is "all or nothing". If one part of a transaction fails validation, the entire transaction fails, and the committed database is left unchanged.

An EDB transaction is made visible by a single atomic publication of the database root. The transaction's log entry, successor metadata, and exact request receipt are stored before publication. The root makes all of them visible together, so there is no possibility of partially committed transaction data. This is not a claim that the whole transaction takes one physical storage write: preparing immutable objects can require multiple writes. Failed preparation can leave unreferenced objects, but cannot publish a partial database.

Inside peer processes, the resident database is an immutable snapshot. Peer advancement replaces the complete snapshot under a lock after validating the new observation. A reader obtains either the previous snapshot or its replacement; already captured database values do not change.

A failed wait is different from a failed transaction. If acknowledgment is lost after publication, the caller may receive an unknown outcome even though the transaction committed. Retry the identical request with the same request key to resolve that outcome. See [Transactions, Reports and Exact Retries](00_transactions.md).

## Consistency

Consistency ensures that any transaction will take the database from one valid state to another. EDB makes the following consistency guarantees:

- Every peer sees completed transactions as of a particular point in time, called a *time basis*.
- The time basis is a total ordering of transactions for a particular database lineage. A captured database incorporates a complete transaction prefix through its basis, with no partially applied transactions or gaps. Physical indexing can lag that basis without omitting committed facts from the database value.

EDB also provides first class support for accessing the time basis of information:

- Every fact in the database carries a transaction reference in the transaction component of its datom. `tx_to_t` converts a transaction entity id to logical transaction time; the entity id is not itself `basis_t`.
- A database value knows its time basis via `DatabaseValue::basis_t()`.
- Connections can synchronize on a time basis via `Connection::sync_to(target, timeout)`.

The logical transaction order is distinct from the wall-clock `:db/txInstant`. Database views and retention also matter: history, as-of and filtered views select information from their underlying basis, while no-history attributes and excision can remove historical information. A complete committed prefix does not promise indefinite availability of every past datom. See [Database Values](../02_core_concepts/00_database_values.md).

To correctly implement domain models, transactions need to be able to derive new facts based on existing facts (e.g. adding to a bank balance), and to enforce domain-specific functional constraints (e.g. a valid account must include name, email, and password hash). EDB's [transaction functions](07_transaction_functions.md) provide transformations and validations of transaction data based on the database value at start-of-transaction (db-before), and [entity predicates](../03_schema/02_schema_reference.md#entity-predicates) provide predicates of the database value at end-of-transaction (db-after).

Entity specs are checked when requested through `:db/ensure`; defining a spec alone does not validate every entity on every transaction. Schema rules, attribute predicates and requested entity constraints must all accept the proposed successor. Native transaction callbacks must honor their purity contract: EDB cannot roll back external effects performed by arbitrary Rust code.

## Isolation

The Isolation property ensures that concurrent transactions result in the same system state that would result if the transactions were executed serially.

For purposes of understanding isolation, EDB operations come in two flavors: reads and writes. A read is an operation which obtains a value of the database, e.g. a call to `Connection::db()`. A write is an operation which alters the committed state of the database, e.g. `Connection::transact(request, timeout)`. Queries and Pull then operate on the captured immutable value.

EDB serializes durable transactions for each database lineage:

- Fresh writes are fully serialized. Every successful fresh transaction is assessed against its immediate predecessor, and publication compares the captured root and writer authority before replacing the root. A competing publication or lost writer authority cannot silently commit a successor of stale state.
- Reads on a live peer advance monotonically in their observed basis. Catch-up rejects a regressing basis, and adoption of a transaction report requires a contiguous successor or an already observed matching transaction. Explicitly held older database values remain usable as older values, subject to storage retention.
- A successful transaction result and local peer observation are separate events. `Connection::transact` and connection ticket waits attempt to wait for local observation within the remaining timeout. If that observation times out after a successful result arrives, the connection returns the successful report and records `observation_error`. Therefore a subsequent `Connection::db()` is not an unconditional read-after-write guarantee. Use the report's exact `db_after`, or successfully call `sync_to(report.basis_t, timeout)` before capturing a new value.
- Reads across multiple peers can observe different prefixes. For instance, if peer P1 sees a write complete before peer P2 begins a read, P2 may not observe P1's write. Operations that interact with multiple peers can explicitly ensure the required basis using `sync_to`.

One fresh transaction proceeds through assessment, upload and publication at a time. Index and excision preparation, prefetch and application reads can proceed independently, but fresh assessments do not overlap another transaction's log I/O. Each transaction publishes its own receipt and root. The service reconciles an ambiguous outcome before consuming another fresh request or maintenance result. Upload batching reduces per-object coordination; it does not combine several transactions into one publication or establish a throughput guarantee.

`Connection::db()` captures the resident value without I/O; it does not establish that the peer has the latest published root. `sync` observes storage progress, and `sync_to` waits for at least the requested basis. Neither changes a value already held by an application. See [Connections and Captured Reads](../07_peer_api/00_connections.md).

Local commit hints use bounded, nonblocking delivery. A full hint slot may drop a hint because peer catch-up uses the durable log; a stalled peer does not stop the authoritative writer through that delivery path. These hints are distinct from the unbounded in-memory transaction-report queue described in [Monitoring Transactions](06_processing_transactions.md#monitoring-transactions).

## Durability

Durability means that once a transaction has been committed, it has been recorded in durable storage. EDB awaits acknowledgment from PostgreSQL for the immutable objects and the guarded root publication before reporting a fresh transaction as complete. Its exact receipt is part of that publication, enabling a matching retry to recover the original outcome without executing the transaction again.

The durability of PostgreSQL's acknowledgment depends on the deployed PostgreSQL storage and durability configuration. PostgreSQL replication, WAL retention, backup and failover policies are separate deployment responsibilities; the EDB writer lease does not implement them. See [PostgreSQL Operations](../08_operations/00_deployment.md).

Lost acknowledgment can produce `UnknownOutcome`; it does not establish rollback. Likewise, a known commit remains committed if opening or observing its local report subsequently fails. Resolve these boundaries with the original request identity, rather than submitting the same intent under a new key.

## How It Works

EDB uses PostgreSQL to store immutable content-addressed objects, not individual application datoms as relational rows. EDB keeps two principal kinds of datom structures in object storage:

- The *indexes* are updated separately from transaction publication and contain datoms sorted in various orders.
- The *log* is updated as part of every transaction and contains datoms grouped by transaction and ordered by logical time.

Both are stored as immutable objects. A database value combines its indexed base with the committed log tail beyond that base, so a transaction does not have to wait for background indexing to become visible. Exact receipts, metadata and read authorization are additional immutable structures reachable from the publication. See [Indexes and the Transaction Log](../06_indexes/00_indexes_and_log.md).

The log uses immutable pages of at most 64 transaction-entry IDs, with authenticated backward skip links. Transaction bodies use payload chunks separately from their authenticated entry metadata. A range cursor retains navigation between pages instead of seeking again from the captured endpoint at every boundary. It fetches O(Q + log P) pages over Q visited pages in a log of P pages, retaining O(log P) navigation pages. This counts navigation pages, not transaction payload reads or constant bytes: skip arrays grow with log size. A read or validation error ends the cursor.

Pointers to published roots are stored in mutable references. These refs require stronger update semantics than the immutable objects: EDB changes them with compare-and-exchange operations against expected reference revisions. PostgreSQL implements the guarded reference batch in a SQL transaction, locking guard keys, checking revisions, and committing the reference changes together. Writer-lease and storage-ownership guards participate in publication.

Reference revisions increase even across deletion and recreation. Deletion leaves a revisioned tombstone, so restoring earlier bytes does not permit a stale observer to update the reference. Guarded PostgreSQL transactions use READ COMMITTED and acquire namespace-qualified advisory locks in sorted order, including locks for absent keys, before reading guarded rows. Every changed reference needs a guard. Direct administrative SQL can bypass this protocol.

The current live storage provider is PostgreSQL. Immutable objects are prepared and authenticated before a root can expose them. Object uploads can be batched, but the number of physical writes depends on the transaction and storage work; there is no fixed one-write-per-transaction guarantee.

### Preparation and publication

The durable transaction path follows this order:

1. Look up the exact request receipt. A matching receipt returns the original outcome before fresh admission, basis, capacity and writer-authority checks, without reassessing the transaction.
2. Assess an absent request against the captured predecessor, then prepare its immutable log, metadata, before/after value descriptors, receipt, request-index paths and publication root.
3. Finish all guarded uploads and authenticate existing content before validating the successor snapshot.
4. Atomically publish the root, renewed lease and storage-ownership changes under the captured guards, then report the result.

An exact receipt preserves the request digest, transaction identity, before/after value identities and original tempid names. The publication indexes the receipt by both caller request key and transaction basis. The separate database-value descriptors omit receipts and writer authority, avoiding a content-addressed cycle through the transaction's own receipt.

Uploads buffer whole objects into groups of at most 128 objects and 64 MiB of canonical bytes, with a 4 MiB target for pending groups. One larger admitted object is held alone. A transaction can require multiple groups; these bounds describe pending object bytes, not total transaction memory, serializer copies or process RSS. Every group uses the same captured source, lease and collection guards and protection epoch. Final publication checks the guards again.

Reused objects still require authentication and renewed protection against older collection epochs. Finding an object by its hash does not prove that a failed guarded upload refreshed its protection. If a later upload fails, earlier successful groups may remain as unreferenced objects, but no transaction is published. A lost upload acknowledgment cannot be resolved from cached bytes or object presence alone; a lost publication acknowledgment is resolved through the exact receipt path.

## Implications

EDB separates the immutable data describing a database from the small mutable references that select its published state. The semantics of immutable values are simple: a value is present, or it is not present. Once authenticated under its content identity, it does not acquire a different meaning through an in-place update.

Only guarded publication makes prepared objects part of the committed database:

- Conditional publication of the database root guarantees atomic visibility of the transaction log successor and its exact receipt.
- Conditional publication of a replacement root guarantees atomic adoption of new indexes, while preserving the canonical log and receipts. A prepared index must cover the authenticated log prefix it claims to cover.

An index candidate does not acquire publication authority merely by being prepared. Adoption checks its index lineage and log endpoint against the current capture; a stale capture is rejected. Retrying from a fresh capture can preserve transactions newer than the candidate's indexed basis, along with the latest receipts and metadata.

This distinction explains why failed staging can leave orphan objects without leaving half a transaction in the database. It also explains why background index work can prepare a new index while peers keep reading previously captured values.

Another way to understand this is to consider a storage failure after a peer obtains a valid root pointer. If a required object cannot be read or authenticated, the operation fails rather than returning a partial database as a complete result. Cached data may remain readable, while cold reads require available storage. This is a consistency boundary, not an eventual-availability promise: missing or corrupt PostgreSQL objects can require operational recovery, and retention or excision can intentionally make older reads unavailable.

Held database values create no storage retention pins. Choose retention to cover active reads and copies, and use explicit synchronization when an application requires a particular committed basis. These operational boundaries preserve the distinction between a coherent immutable value and the continued availability of the storage needed to read it.
