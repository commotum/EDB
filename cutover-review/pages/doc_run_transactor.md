# Run a Transactor review

Reviewed `docs-2/01_tutorials/04_run_a_transactor.md` in full, NORTHSTAR,
TIMESTAMP_SOURCE_DESIGN, CUTOVER, and all three supplied Hickey transcripts.

The page teaches install/create, role selection, a private local endpoint,
READY, separate PostgreSQL reads/socket writes, and graceful stop/restart.
These deployment operations do not themselves submit application facts. Source
belongs in each transaction request, independently of process configuration.
The existing command shapes therefore remain appropriate under NORTHSTAR.

The sole wording gap is endpoint permissions: "allow access only to you" is
less precise than the implementation's exact owned mode-0700 requirement.
`mktemp -d` supplies the required mode. Clarify that requirement in place,
preserving the complete tutorial and its command argument order.

Approaches considered: leave the prose (working recipe, ambiguous manual
setup); specify mode 0700 (matches the existing mechanism); add process-level
source configuration (would entangle deployment with transaction attribution
and contradict the one-source-per-request contract). Select the minimal
permission clarification. No adapter or compatibility path is needed.

Implementation inspected: CLI arguments/dispatch/runtime and signal handlers;
PgBlockStore install/connect/runtime grants; catalog genesis and name resolution;
TransactionService activation/shutdown; local endpoint ownership and server
request forwarding; PostgreSQL configuration/environment and verified TLS.
Install and grants commit separately, runtime resolves an existing catalog
entry, READY uses service storage identity and lineage, the socket is private,
and SIGINT/SIGTERM share the same stop handler. Catalog genesis constructs all
six index orders with existing system-source vocabulary. The local server
forwards the canonical source-bearing request unchanged to the writer.

Acceptance checks: preserve install/create/start flags; verify READY shape from
runtime; verify exact mode 0700; check local Markdown links/fragment; validate
shell examples syntactically. Reuse already executed live evidence for unchanged
writer lifecycle and source transport instead of running a broad suite again.

Evidence inspected: `edn-workflow.log` records both tests passing, including
actual stock CLI install/create/private endpoint startup, six-slot source-bearing
reports, writer-offline reads, graceful stop/restart, exact retries after source
ident withdrawal, and rejection of retired syntax. The test code was inspected
against these claims. `process-workflow.log` records independent writer/peers,
complete reports, and writer-offline reopen passing. `releases-cli-install-live.log`
records repeated installation format verification passing. The first application
log contains a historical mixed-source query failure; the subsequent
`application-local-retry-after-fix.log`, `application-local-restart.log`, and
`application-tls-first.log` contain successful source/application checks. The
failed run is not counted as acceptance evidence.

Independent concerns remain separated: administrative provisioning,
authenticated storage connections, endpoint ownership, writer authority, and
transaction attribution compose through existing data boundaries. Exact
permissions can be reasoned about locally in EndpointOwner::bind; attribution
does not gain a new process policy. This keeps future transport changes separate
from the fact model.

Actual change: one prose line now states exact endpoint directory permissions
`0700`. All commands, API shapes, linked tutorials, and lifecycle instructions
are preserved. No implementation gap identified for this page; no shared code
changes or new tests were required.

Focused checks passed: Python checked all five local links (including both
fragments), `bash -n` checked all three shell blocks, and source assertions
verified the documented READY shape/storage identity and common SIGINT/SIGTERM
handler. `git diff --check -- docs-2/01_tutorials/04_run_a_transactor.md` passed.
No live suite was rerun. Reused successful live logs described above cover the
unchanged behavior. Unresolved page gaps: none.
