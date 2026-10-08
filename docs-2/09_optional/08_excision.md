# Excision

Excision is the complete removal of a set of datoms matching a predicate. Excision should be a very infrequent operation and is designed to support the following two scenarios:

- Removing data for privacy reasons
- Removing data older than some domain-defined retention period

Excision should never be used to correct erroneous data and is unsuitable for that task as it does not restore any previous view of the facts. Consider using ordinary retraction to correct errors without removing history.

## Motivation

EDB makes the history of your data first class. Given a database value, you can ask questions about what you know right now, and/or questions about what you knew at points in the past.

Legitimate motivations for removing data are *very rare*. A common desire is to be able to erase mistakes, but this is almost always a questionable idea. If a past version of a database contained incorrect information, decisions may have been made based on that incorrect information. It is usually much more valuable to be able to recover the basis for those decisions, even when that basis is known to be incorrect than to pretend that mistakes never happened. (Imagine a source control system that removed the history of code defects once they were fixed). In any case, excision is unsuitable for the error-correcting task, as it does not restore any prior view of the facts.

There are, however, legitimate reasons to 'forget' data. Systems are sometimes interested only in data that happened within a certain time window, or keeping older data can be a source of liability. Or, you may discover after recording data that you do not have the legal right to store the data. Privacy laws might require you to excise data from ex-customers, etc.

These scenarios motivate excision, literally cutting data out of a database.

## Backup Before Excising

Excision is irrevocable. You are strongly encouraged to [back up](../08_operations/11_backup_and_restore.md) a database before excising data.

A backup retained for recovery also retains the information you intend to remove. Apply your retention decision to backups and independent copies as well as the live database.

## Implementation

You can request excision of data by transacting a new entity with the following attributes:

- *:db/excise* is required, and refers to a target entity or attribute to be excised. Thus there are two scenarios — excise all or part of an entity, or excise some or all of the values of a particular attribute.
- *:db.excise/attrs* is an optional, cardinality-many, reference attribute that limits an excision to a set of attributes, useful only when the target of excise is an entity. If *:db.excise/attrs* are not specified, then all matching attributes will be excised.
- *:db.excise/beforeT* is an optional, long-valued attribute that limits an excision to only datoms whose t is before the specified *beforeT*, which may be a non-negative t or transaction entity ID. This can be used with entity or attribute targets.
- *:db.excise/before* is an optional, instant-valued attribute that limits an excision to only datoms whose transaction time is before the specified *before*. This can be used with entity or attribute targets.

Use at most one of *:db.excise/before* or *:db.excise/beforeT*. Supplying both rejects the transaction with `transaction/excision-cutoff-conflict`.

The cutoff is strict: datoms at the boundary are retained. Every request is also bounded by its own assertion transaction. Without an explicit cutoff, only datoms before that transaction are eligible; a future cutoff does not remove later writes. A request is a one-time removal predicate, not an ongoing retention rule.

> Excision is a special operation that happens outside the timeline of EDB history, removing matching data across history in the successor generation. While the excise request itself is transactional, the removal occurs asynchronously. Committing the request does not establish that removal has completed.

The stock transactor automatically schedules committed requests through its background maintenance worker, coordinating with indexing. More than one excision can be processed in a batch. Avoid repeatedly requesting indexing in an attempt to make excision feel synchronous. If you need to coordinate with a database that is guaranteed to reflect completed excision requests, use [`Connection::sync_excise`](../07_peer_api/types/02_connection.md#sync_excise):

```rust,ignore
let db = connection.sync_excise(request_basis_t, std::time::Duration::from_secs(60))?;
```

Use the excision transaction's committed basis as `request_basis_t`. The returned database must have reached that basis and carry completion metadata for requests asserted at or before it. This wait does not initiate excision or garbage collection. A timeout does not cancel a committed request. See [synchronization](../04_transactions/09_synchronization.md).

EDB reconstructs the request attributes as of the transaction asserting `:db/excise`, then persists the material predicate for resumable processing. Later ordinary edits do not redefine an already queued request. Use a new request entity for each operation.

Schema datoms and protected bootstrap datoms cannot be excised. This includes the schema-defining attributes and the excision attributes themselves. In addition, datoms whose entity is in the `:db.part/db` partition are protected. Datoms recording past excision predicates are protected. Requests matching protected datoms are recorded but do not remove those datoms.

When the target of excision is an entity, and a selected reference attribute has *:db/isComponent* true, component entities are included recursively. Their attributes are not restricted to the parent's *:db.excise/attrs* selection. The same cutoff and protected-datom rules still apply. See [component attributes](../03_schema/02_schema_reference.md#dbiscomponent).

## Excising Specific Entities

The examples assume source entity `99` identifies the contributor requesting excision. Supply it once for each transaction through the request or `edb transact --source 99`. EDB automatically records the transaction instant with that source.

To excise a specific entity, manufacture a new entity with a *:db/excise* attribute pointing to that entity's id. For example, if user `17592186044458` requests that their personal data be removed from the system, the transaction data would be:

```edn
[{:db/excise 17592186044458}]
```

Since *:db.excise/attrs* is not specified in the transaction above, *all* eligible datoms about the entity will be excised. The example ID is in EDB's user partition; substitute the entity ID returned by your application's transaction or query. Small ID `42` belongs to the protected database partition and would not serve this example.

These are ordinary [transaction data](../04_transactions/04_transaction_data.md) forms. Install the application attributes before using them, submit through your connection or the `edb transact` command, and retain the committed request basis for synchronization. Local `with_edn` previews only record the request in a speculative database; they do not run storage excision.

## Excising Specific Attributes of an Entity

To excise only specific attributes of an entity, create a transaction as in the previous step, but also specify a collection of *:db.excise/attrs*. For example, you might excise attributes that contain personally identifying information, such as usernames and email addresses, while leaving untouched other attributes that might be valuable for aggregate queries. Submit this transaction with source `99`.

```edn
[{:db/excise 17592186044458
  :db.excise/attrs [:person/name :person/email]}]
```

## Excising Old Values of an Attribute

To excise old values of a particular attribute, you can create an excision for the attribute you want to eliminate, and then limit the excision using either *before* or *beforeT*. Imagine tracking application events that have users, categories, and details. Each attribute requires its own excise request. There are lots of events, and you don't need them after a certain time window, so you can get rid of the attribute datoms transacted before 2012. Submit the following transaction with source `99`:

```edn
[{:db/excise :event/user
  :db.excise/before #inst "2012-01-01T00:00:00Z"}
 {:db/excise :event/category
  :db.excise/before #inst "2012-01-01T00:00:00Z"}
 {:db/excise :event/description
  :db.excise/before #inst "2012-01-01T00:00:00Z"}]
```

The cutoff uses transaction time, not an event's application timestamp. This is removal of matching attribute datoms; it does not select whole events by their creation date.

The use of *beforeT* is similar, but specifies the excision boundary in terms of database t. The example below will excise eligible event-related datoms before t 1100, provided the excision requests are transacted at or after that boundary. Submit this transaction with source `99`:

```edn
[{:db/excise :event/user
  :db.excise/beforeT 1100}
 {:db/excise :event/category
  :db.excise/beforeT 1100}
 {:db/excise :event/description
  :db.excise/beforeT 1100}]
```

## Excising All Values of an Attribute

**Excising by attribute is very dangerous**. If you leave out the *before* limitations from the previous examples, then you will remove **all** eligible values of the various *:event* attributes before the excision request transactions, effectively removing those event facts from the database. Submit this transaction with source `99`:

```edn
[{:db/excise :event/user}
 {:db/excise :event/category}
 {:db/excise :event/description}]
```

Attribute excision removes values, not the attribute's schema definition. Writes at or after the request transaction remain eligible to use that attribute.

## Excision and Reference Attributes

When excising an entire entity, component entities are also included, as are inbound references to the excised entity. When selecting particular attributes of an excised entity, both in- and outbound values of those attributes involving the excised entity are included, and, if a component attribute, the component entity's attributes are included in their entirety. Inbound references to included components are included as well. Cutoffs and protected-datom rules apply to each datom independently.

Component traversal follows historical component references through the request transaction, including relationships later retracted before that request. Selection limits the first hop from the target; traversal through descendants follows their component attributes. The selected component extent is persisted with the rewrite predicate.

When using the excise-values-of-attribute form, it is the responsibility of the application to manage inbound references and components. That form matches the datom's attribute and does not recursively traverse component relationships.

## Tracking Excisions

Excision is lossy. Given a datom in the rewritten database, you cannot ask "Was this datom in the database before an excision?" If that database retained the removed fact to answer that question, then the datom would still be present in some sense, which defeats the purpose of excision.

However, you can query the *predicates* used for excision, which makes it possible to answer the more limited question "Has an excision been requested that might have affected this entity?" For example:

```edn
[:find ?e
 :in $ ?e
 :where [_ :db/excise ?e]]
```

Bind `$` to your database and `?e` to `17592186044458`, using the [EDN query API](../01_tutorials/00_edn_workflow.md#native-library) or a CLI `--inputs` file containing `[17592186044458]`. Use a history database when checking predicates that may subsequently have been retracted. This query finds direct target requests; it does not discover every attribute-wide predicate or component cascade that could affect an entity, nor establish completion. Use `sync_excise` for completion.

> The excision attributes themselves are protected from excision, so an excision request leaves a retained audit record. Ordinary retractions can change its current visibility but do not excise that historical record. This helps preserve a fundamental value proposition of EDB — it is a database that facilitates your knowing why it is in the state it is in, and how it got there. Thus excision strikes a delicate balance between forgetting and remembering that you forgot.

## How it Works

After the excision request, the background worker builds an inactive successor generation. It rewrites the transaction log without matching datoms, preserving each surviving assertion and retraction with its original transaction and source. It prepares all twelve current/history indexes and fulltext search, and stages sanitized receipts. The completed root and excision completion metadata become visible together. No partially completed successor is activated. Bounded persisted checkpoints allow interrupted work to resume.

Activation is one guarded publication of the replacement log, indexes, metadata and receipts. The writer verifies that the source is still current, the candidate is exactly the next generation, and the database basis, allocation coordinates and last transaction time are unchanged. Activation creates no additional history transaction. The same publication clears the excision work checkpoint and invalidates restore checkpoints and completed restore receipts that could otherwise retain or return the old generation.

Resuming interrupted work checks the database identity, saved predicate and source generation. If ordinary transactions have extended the source during an interruption, the worker verifies that the current log extends the saved source in the same generation before continuing. It rejects a replaced generation or a log that is not a continuation of that source. During each admitted rewrite attempt, the source is frozen; the worker does not follow new live roots as it scans.

Thus, **excision is permanent in the successor generation**, short of restoring a backup. Backing up before significant excisions is highly recommended, subject to the same retention obligations as other copies.

An already-held database or log value remains an immutable view of its captured generation and can retain access to pre-excision information within storage retention. Discard old values and synchronize controlled clients before claiming removal. New serialized openings of an older generation are rejected. Current and history fulltext searches on the successor exclude excised facts, while authorized older values follow the same retention policy as other old views.

Generation handoffs retain original outcomes for lagging transaction-report queues during the configured GC grace period. They are independent retained owners, not part of ordinary snapshot lookup. Expired handoffs must be pruned and their released ownership processed by GC before reclamation. See [excision operations](../08_operations/00_deployment.md#excision) and [change consumers](../07_peer_api/03_change_consumers.md).

All pre-excision idempotency keys remain occupied by tombstones, including keys for transactions that did not touch selected facts. Retrying one returns `postgres/idempotency-predates-excision` without rerunning transaction functions. Tombstones contain no original request digest, tempid names, transaction payload or old value-root links. New requests retain ordinary exact-retry behavior.

`PostgresOperator::process_excision_requests` is an administrative driver for the same resumable worker. It takes the catalog's stable `database_id` and claims the normal writer lease. If a running service owns that lease, let its automatic worker finish or stop it before administrative processing; the operator does not bypass writer authority.

## Performance

Excision puts a substantial burden on background maintenance. Large excisions can require work proportional to the size of the entire database, reducing write availability. Request discovery and initial admission checks run on the maintenance worker before the writer pauses fresh transactions. After successful writer admission, the service parks fresh transactions through log rewriting, index preparation, receipt rewriting and activation; lease renewal, receipt resolution and independent reads continue. The pause covers the admitted work, not just the final publication. Limit each request to the data you intend to remove and plan significant excisions as maintenance work. Small removal counts do not establish a small rewrite cost.

Log rewriting scans the canonical source log forward in bounded batches. An oversized encoded entry is rejected before fetching its transaction chunks or decoding its datoms. These bounds limit admitted work per checkpoint; they do not bound the total duration of the fresh-write pause. Index preparation and receipt rewriting add work beyond the source-log scan.

`ServiceOptions::excision` controls automatic-worker admission. Defaults are enabled processing, 512 MiB of accounted maintenance bytes, and at most 256 log transactions per step. The stock flags are `--excision-max-bytes` and `--excision-log-batch`; the latter accepts 1–4096. These are policy accounts, not measured RSS or guaranteed time limits. Predicate discovery, transaction loading, replay batches and index preparation have admission checks; fulltext has separate build limits. See [configuration](../08_operations/14_configuration_reference.md) and [maintenance controls](../08_operations/04_maintenance_controls.md).

Admission failure leaves the committed request pending and allows ordinary writes. Observe `background_indexing_stats().excision` and `excision_failure`; the stock process reports `TRANSACTOR_EXCISION_FAILURE`. Increase the allowance deliberately and restart to resume, or use authorized administrative maintenance. Progress includes rewrite counts and `fresh_write_pause_nanos`, the latest automatic activation attempt's measured fresh-write pause, not a persisted lifetime total.

After excision is complete, storage no longer owned by retained roots can be recovered through [garbage collection](../08_operations/00_deployment.md#inspection-retention-and-reclamation). Collection is a separate operation governed by published ownership and protection epochs; an excision predicate does not authorize deleting an object still owned by another retained root. Completion alone does not mean immediate physical deletion. Retention, generation handoffs and GC grace still apply. The datoms recording excisions remain in storage.

## Limitations

Excision operates on durable storage. An in-memory or speculative database can contain excision requests, but it does not run the storage rewrite.

Fulltext attributes are supported: search is rebuilt from the successor before activation. Search copies in old values and backups are not erased by that rebuild. See [fulltext retention](../05_query_and_pull/03_fulltext.md#retention-repair-and-excision).

`:db/noHistory` is a separate index-retention policy, not an erasure guarantee. Excision filters matching datoms from its source log regardless of that flag, but neither operation erases independently retained generations or copies. See [no-history semantics](../03_schema/02_schema_reference.md#dbnohistory).

Excision does not erase application exports, application logs, process memory, replicas, PostgreSQL WAL, portable or physical backups, or existing local cache files. Apply the retention decision to these separately. [`Connection::purge_ssd_generation`](../08_operations/12_memory_and_caching.md#access-and-retention) purges recognized entries in the configured cache namespace; it can return `false` on busy/error paths and is not secure erasure or a purge of other namespaces. Old cache roots must also be retired under your retention policy.
