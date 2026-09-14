# Transactions, reports and exact retries

A transaction is logical information to apply atomically, not a script of
imperative updates. Typed `TxOp`/`TxForm` and EDN entity maps or primitive forms
use the same assessment rules. The writer evaluates fresh intent against one
immutable db-before, validates it, then publishes a new ordered transaction.
Unrelated omitted facts remain unchanged.

`TransactionRequest` pairs that intent with a caller-owned request key. Keep the
complete request, including explicit basis/time options and generated UUIDs,
until its outcome is known. An application outbox can preserve it across process
failure. A transaction callback must compute from supplied data, not perform
external side effects that a retry could duplicate.

## Interpret the outcome

A successful report carries exact db-before/db-after values, assertion and
retraction datoms, resolved tempids and transaction identity. `basis_t` is logical
transaction order; the transaction entity and `:db/txInstant` are separate
representations. Earlier captured values remain unchanged.

After ambiguous delivery, retry identical content under the same key. Receipt
resolution precedes current schema/default/function assessment. Reusing a key
with different intent conflicts; it is not a second transaction. Keep the original
typed or EDN representation: equivalent representations are not promised to have
the same request digest.

A confirmed `CommittedTransaction` can have `report: Err` if opening its local
read values fails. The commit is still confirmed. EDN output can similarly carry
`:atomic/report-error`; output failure does not prove rollback. Never generate a
new key just to recover a reply. Excision deliberately replaces old receipts with
occupied-key tombstones; see [its retry policy](../08_operations/00_deployment.md#excision).

## Preview and commit intent

`DatabaseValue::with`, `with_forms` and EDN preview produce a speculative
db-after without storage publication, durable receipts or identity reservation.
Pass the result through ordinary queries/Pull to compare alternatives.
Submit the original logical forms, not preview IDs or a datom diff.
An optional basis guard rejects a plan whose required live basis changed;
recompute intent from a new value before submitting a newly chosen plan.

Filtered speculation assesses the full basis and preserves the filter on its
result. A temporal view is not permission to rewrite the past, and raw history
cannot transact. Use matching writer [allocation defaults](02_partitions.md)
when a preview needs to predict placement.

Follow the [EDN workflow](../01_tutorials/00_edn_workflow.md) or
[separate application](../01_tutorials/01_application_workflow.md) for executable
submission/restart examples. [Stored programs](01_persisted_programs.md) and
[compiled callbacks](../07_peer_api/02_native_computation.md) are explicit native
deployment choices. React to commits with [change consumers](../07_peer_api/03_change_consumers.md),
not side effects inside assessment; inspect [diagnostics](03_diagnostics.md) when
you need operation costs.
