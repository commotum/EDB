# Setup page review

Assigned page: `docs-2/00_start_here/01_setup.md`.

## Situation and cause

The setup page documents the local source package, executable build/install,
PostgreSQL provisioning boundaries, separated administrative/writer/peer roles,
private local transport, readiness and shutdown, and library integration.
The source cutover extends transaction intent, not these setup operations.
The page contains no transaction or speculation call that omits source. Its
EDN and Rust tutorial links lead to explicit source-aware submissions.

## Bounded problem

Confirm that every documented setup command and claim remains true for the
six-field, mandatory-source implementation, including initial schema/source
availability. Repair concrete divergence without expanding setup into another
transaction tutorial or touching application resources.

## Approaches and selected implementation

A repeated source paragraph here would enlarge an already source-aware linked
workflow without repairing a failing instruction. Reusing the documented
setup/transaction separation preserves independent concerns and local reasoning:
installation creates provider tables/grants, creation materializes canonical
genesis, runtime starts a writer, and applications supply attributed transaction
intent. Inspect those existing mechanisms and change only demonstrated gaps.
No code or documentation changes are selected unless verification exposes one.
This follows the talks' problem-before-solution discipline, structural simplicity,
composable boundaries, and ability to change attribution independently of setup.

## Acceptance checks

- Manifest confirms package `edb-core`, version 0.1.0, library `edb_core`,
  executable `edb`, Rust edition 2024, unpublished/unlicensed local dependency.
- Locked offline Cargo metadata validates build/install package/target selection.
- Offline executable version/help validate command surfaces; rejected missing
  source must happen before storage access.
- Trace install/create/runtime to provider format, native genesis/system source,
  six current/history index orders, no implicit provisioning, and private socket
  permission/readiness/signal/drain behavior.
- Reuse passing local, restart, remote TLS and source-bootstrap application logs
  and root's integrated suite for unchanged semantics.
- Do not use any real application database, endpoint, process, target, or checkout.

## Changes and results

No source, test, or documentation files changed. The complete assigned page
remains aligned with NORTHSTAR and the timestamp/source contract.

Inspected the complete manifest; `src/bin/edb/{main,arguments,dispatch,runtime}.rs`;
setup-relevant paths in `data.rs`; PostgreSQL environment parsing and provider
installation/grants; canonical creation in `storage/catalog/database.rs`;
`Database::bootstrap`/`new`; system-source vocabulary; and local endpoint
ownership and server shutdown. Creation uses `Schema::new()` and native
system-attributed t=0 genesis. It builds EAVT/AEVT/AVET/VAET/SAEVT/SAVET for both
current and history, without an application-source default. The linked EDN
workflow explicitly supplies `:db.source/system`; the linked Rust workflow
establishes its own ordinary source through a same-transaction tempid and then
uses its ident. No source lookup or compatibility adapter was added.

`setup-metadata.json` confirms `edb-core` 0.1.0, edition 2024, UNLICENSED,
`publish=[]`, library `edb_core`, binary `edb`, and target directory wholly
within the isolated implementation. `setup-offline-checks.log` records seven
successful checks against the existing isolated debug executable: version/help
without PostgreSQL; missing-source rejection for transact/with before storage;
and install/create/transactor documented argument shapes accepted before the
explicit missing configuration error. The production environment was removed
from each probe, so no PostgreSQL or application socket could be accessed.
The release build/install commands were checked against Cargo metadata and
existing successful compilation; a new release build/Cargo install was not
run for this unchanged page.

`tests-integrated-second.log` already records passing CLI version/help/data-only
query, mandatory source admission, native genesis attribution, and ordinary
source tempid/ident/lookup resolution tests. `application-local-retry-after-fix.log`
records SOURCE_OK and APPLICATION_OK after the historical first-run query issue
was repaired. `application-local-restart.log`, `application-tls-first.log`,
`application-tls-restart.log`, and `application-standby-takeover.log` establish
successful schema/source bootstrap, exact retries, native current/history,
local restart, verified remote transport and takeover. `process-workflow.log`
records concurrent independent peers, complete reports and writer-offline reopen.
No whole suite or PostgreSQL setup was repeated for these unchanged statements.

All page claims are covered; no unresolved setup dependency or real gap remains.
