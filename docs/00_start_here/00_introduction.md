# Start with Atomic

Atomic is a native Rust/PostgreSQL database for applications that need declared
attribute types, durable identity, transactional changes and historical reads.
Applications capture immutable database values and evaluate Datalog, Pull and
entity navigation locally. A separate transactor serializes writes; PostgreSQL
stores authenticated immutable objects and guarded references.

Start with the [EDN tutorial](../01_tutorials/00_edn_workflow.md) for file/stdin
commands, or the [separate Rust application](../01_tutorials/01_application_workflow.md)
for explicit installation, a writer process and restart-safe submission.
Neither runtime startup nor a query implicitly provisions storage.

## Guide chapters

| Chapter | What you can do |
| --- | --- |
| [Tutorials](../01_tutorials/00_edn_workflow.md) | Install schema, transact data and read it from a separate application. |
| [Database values](../02_core_concepts/00_database_values.md) | Capture, compose, compare and reopen immutable views. |
| [Schema](../03_schema/00_schema.md) | Declare attributes, references, uniqueness and managed tuples. |
| [Transactions](../04_transactions/00_transactions.md) | Submit logical intent, interpret reports and reconcile unknown outcomes. |
| [Queries and Pull](../05_query_and_pull/00_queries.md) | Join facts and application data, aggregate, navigate and search. |
| [Indexes and logs](../06_indexes/00_indexes_and_log.md) | Traverse ordered datoms and exact transaction history. |
| [Native peer API](../07_peer_api/00_connections.md) | Capture and synchronize reads, use async workers and consume changes. |
| [Operations](../08_operations/00_deployment.md) | Deploy, administer, inspect, back up and restore. |
| [Optional local cache](../09_optional/00_local_cache.md) and [hints](../09_optional/01_transaction_hints.md) | Opt into disposable caching or advisory prefetch. |

## Choose the boundary deliberately

Live peers need authorized PostgreSQL access for opening and cold object reads.
They do not send queries to the transactor. Same-host sockets and verified-TLS
remote routing submit transactions; they are not query gateways. For intentional
offline operation, open a [portable backup](../08_operations/05_backup_reads.md).

Captured values do not register reader pins. Configure collection grace to cover
reads, lagging report consumers and backup capture; the normal recommendation is
30 days. A held value or serialized reference cannot prevent cold data from being
collected after that grace. See [retention](../02_core_concepts/00_database_values.md#captured-value-retention).

This first-release development line uses current native storage, request and
program formats. Unsupported historical formats fail explicitly; installation
never resets data. There is no JVM, Lucene ABI, Datomic wire protocol or automatic
historical-upgrade promise. Cooperative resource controls do not preempt arbitrary
Rust callbacks or blocked storage calls. Use the owning API's limits and the
[transport policy](../08_operations/00_deployment.md#transport-and-failure-policy).
