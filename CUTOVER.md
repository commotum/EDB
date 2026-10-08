# Source cutover

The contract is [NORTHSTAR](docs-2/NORTHSTAR.md), the
[transaction data reference](docs-2/04_transactions/04_transaction_data.md),
and the API and behavior documented in `docs-2/`.

## Working copies and the running application

| Purpose | Path | Branch |
| --- | --- | --- |
| Course_Academy's existing EDB | `/home/jake/Developer/EDB` | `master` |
| Source cutover development | `/home/jake/Developer/EDB-source` | `source-first-class-cutover` |

Keep the existing checkout, release binaries, and library artifacts available
at their current paths. Course_Academy's scripts use that checkout when building
native wrappers and invoking the EDB CLI. Build the cutover in its own worktree
and target directory. Do not direct `CARGO_TARGET_DIR` to the existing checkout
or update Course_Academy's EDB paths, service definitions, sockets, or storage.

Use separate databases, storage, sockets, and service names for cutover tests.
Do not run the new writer against Course_Academy's database. Application
migration is a separate step; the cutover has no backward compatibility layer.

Course_Academy's existing PostgreSQL data is at
`/home/jake/Developer/Course_Academy/.local/edb/postgres`, with PostgreSQL database
`course_academy` and logical EDB database `course-academy-v2`. Its PostgreSQL
socket directory is `/home/jake/Developer/Course_Academy/.local/edb/run`; the EDB
writer socket is `/tmp/course-academy-edb-v2/writer.sock`. These resources belong
to the running application and must remain outside cutover tests.

## Implementation and acceptance

1. Extend the fact, wire, log, index, and persisted-program representations
   consistently to `[e a v t s added?]`. Preserve existing names, transaction
   forms, argument ordering, and return shapes wherever source does not require
   a change. Reject incompatible stored formats explicitly.
2. Require one request-level source, resolve it through the ordinary entity
   reference rules, and attribute every explicit and generated change to it.
   Include source in exact-retry identity. Preserve ordinary timestamp
   generation, explicit import instants, and clock validation.
3. Keep distinct live source contributions to one EAV without multiplying
   values or unique holders. Retraction withdraws that EAV across sources;
   source-filtered reads must apply global visibility before restricting source.
   Verify current, history, as-of, since, query, Pull, CAS, and uniqueness behavior.
4. Maintain EAVT, AEVT, AVET, VAET, SAEVT, and SAVET consistently across recent
   and durable current/history indexes, recovery, backfill, and maintenance.
   AVET and SAVET share eligibility and readiness. Source activity traverses
   SAEVT's `[source :db/txInstant]` range and reads selected transactions from
   the same captured log basis. No separate ST or STEAV structure is needed.
5. Exercise persistence, reopen/recovery, exact retries, backup/restore,
   background indexing, and documented entry points with source-bearing facts.
   Test same-value contributions, cross-source replacement/retraction, generated
   facts, and stable source-activity pagination explicitly. Complete release
   checks against the documented feature set before calling the cutover ready.

Authenticated log entries use kind 14: fixed transaction metadata with links to
the existing body chunks for every transaction, including small ones. This lets
bounded consumers admit encoded payload length before reading body bytes and
lets log endpoint validation read metadata alone. Former kind 11 entries are
incompatible and rejected; acceptance runs use freshly initialized cutover
storage rather than mixing these representations.

## Retired timestamp mechanism

The dedicated `[:db/txInstant S]` operation, `TxOp::TxInstant`,
`Instruction::EmitTxInstant`, `tx_instant_source`, per-operation sources, and
map-level `:db/source` declarations are not part of the contract. Check that
none survive as accepted behavior in code, examples, or generated API
documentation. Regression tests should reject the retired input forms.

Keep ordinary `:db/txInstant` assertions for explicit import values,
`tx_instant_override`, and the bootstrap source entity `:db.source/system`.
The transaction data reference describes the current behavior; the superseded design
remains in Git history.
