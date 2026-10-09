# docs-2/07_peer_api/types/08_log.md

Reviewed the complete assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER,
worker rules and supplied talks. The page already describes an immutable captured
log with typed `t`/`data` fields and source activity composed from SAEVT timestamp
facts and same-basis log reads. No model or API mismatch was found. The bounded
problem was to establish parity after kind 14 replaced inline log bodies; changing
an aligned page would add churn without solving an unmet objective. Selected
approach: retain the page and implementation, trace their concrete mechanisms,
and reuse executed tests for unchanged behavior. No adapters, extra timeline
structure or implementation-status prose are needed.

## Implementation parity

- `src/database_value/log.rs` (complete): cloneable snapshot capture, basis and
  physical generation, validated T/Tx bounds, earliest matching instant via
  captured AVET, genesis exclusion, clipping/reversed ranges, lazy authenticated
  iteration, one-transaction buffering and fused errors/exhaustion. `tx_ids`
  uses the same authenticated cursor; `tx_data` distinguishes valid out-of-range
  coordinates from missing committed content.
- `src/storage/log.rs` (complete): kind 14 fixed metadata and kind 12 linked
  chunks for every nonempty body; kind 11 rejection; payload-free endpoint opens;
  authenticated skip-page traversal with logarithmic forward anchors; coordinate,
  chunk-shape, canonical-body and uniform-source validation. The codec in
  `src/encoding/mod.rs` carries E/A/V/Tx/source/bool and rejects mixed sources.
- `src/storage/snapshot.rs` captured log reads preserve snapshot authority and
  generation-scoped authenticated I/O without mutable-head lookup.
  `Connection::log`, `Peer::log`, `DatabaseValue::log_value` and
  `BackupConnection::log` supply the captured snapshot directly.
- `src/storage/excision.rs` rewrites the retained log into a successor generation;
  original snapshots reference their original graph. Offline repository snapshots
  supply the same log implementation. The page accurately conditions cold live
  reads on referenced objects surviving the configured GC grace period.

Independent concerns remain separate: source activity selects immutable timestamp
facts in the ordinary index; the log authenticates complete transaction data;
capture/retention determines read authority. The existing shared traversal keeps
ordering and authentication locally reasoned without a source-specific log path.

## Acceptance evidence inspected

- `change-consumers-admission-unit.log`: **3 passed**, including small-body
  metadata-only open/admission, former-kind rejection, malformed chunks, and empty
  retained entries.
- `change-consumers-uniform-live.log`: **2 consumer + 1 recovery + 2 log passed**
  with kind 14. Read `tests/log_source_cutover.rs` in full: exact T/Tx/instant
  bounds, tied instants, invalid/future/genesis coordinates, initial zero cursor
  stats, fused exhaustion, complete six-slot data, cross-source withdrawals,
  empty/redundant timestamp activity and reverse pagination stable after later
  transactions. Inspected consumer/recovery assertions for noHistory data,
  attribution, timestamp and generation preservation.
- `backup-log14-live.log`: **1 passed**. Inspected backup assertions comparing
  offline and restored full log data across pending, partial-current,
  partial-history and complete publications, with uniform sources and one
  timestamp per transaction.

Result: **no source/doc edits; no redundant test execution**. No remaining gap
identified for this page. These inspected per-feature proofs establish its current
behavior; they are not a declaration that all unrelated release checks are done.
