# Releases page review

Assigned page: `docs-2/00_start_here/04_releases.md`.

## Situation, cause and bounded problem

The complete page describes the unpublished `edb-core` source package, its
`edb_core` library and `edb` executable at package version 0.1.0. It explicitly
distinguishes the package version from release history and sends users to local
build requirements and current-storage provisioning. The manifest and CLI
version implementation agree. No concrete gap is observed so far.

Confirm that this acquisition guide reaches the source-aware native engine,
that every local link and build target exists, and that linked incompatible-format
rejection is implemented at actual decoding/open boundaries. Preserve the private
local distribution model and do not announce cutover/release readiness.

## Approaches and selected implementation

Inspect the existing package targets, CLI dispatch, public exports and codec
admission, using recorded successful checks for unchanged behavior. Leave the
page intact unless a specific false statement or missing behavior emerges.
Adding publishing infrastructure, another package, or legacy readers would
introduce independent release-policy work and entangle acquisition with engine
semantics. The current package composes library and executable build targets
without a feature-gated attribution path; changing distribution later need not
change source resolution or immutable fact formats. This follows the supplied
talks' problem-first design, independent concerns, local reasoning and ability
to change rather than designing a release mechanism for an unobserved need.

## Concrete acceptance checks

- Match package/license/publish/features/version/targets to Cargo metadata,
  manifest, CLI version dispatch and public library exports.
- Verify all local Markdown destinations and named anchors.
- Trace the linked storage warning through PostgreSQL provider admission,
  canonical object/log/program/tree/backup codecs and their old-format tests.
- Reuse setup metadata/offline probes, successful application build,
  all-target check, rustdoc and integrated test evidence for unchanged paths.
- Record exact findings; no production resources, commits, release claims or
  unnecessary full build/test repetition.

## Changes and results

Boundary review found a concrete mismatch: both CLI installation paths print
`edb/opaque-storage/1`, whereas `PgBlockStore::install` writes and `connect`
requires `edb/opaque-storage/2`. The active path is `admin::dispatch`; the older
fallback remains in `dispatch::run`. Root assigned both files and a separate
focused integration test to this worker. There is no existing exported format
constant/method. Correcting these two literals preserves the current API rather
than exporting provider internals solely for presentation. An ignored portable
test, requiring explicit disposable `EDB_TEST_POSTGRES_URL`, will run installation
twice and compare its reported format to `PgBlockStore::read_ref("system/format")`.
This verifies actual output and idempotence without resetting shared resources.

Both output literals are corrected to `/2`. The new
`tests/release_distribution_cutover.rs` exercises the active command through
`CARGO_BIN_EXE_edb`, requires explicit isolated configuration, and compares its
complete success line against the provider's actual ordinary reference. It
checks the second installation too. No new public API, compatibility reader,
package metadata or release workflow was introduced.

The releases page remains unchanged: all claims are accurate. Checked all ten
Markdown destinations and both named anchors by resolving the files and their
headings. The manifest and existing locked offline `setup-metadata.json` agree
on `edb-core` 0.1.0, `UNLICENSED`, no publishing, no Cargo features, library
`edb_core`, binary `edb`, and edition 2024. `dispatch::run` obtains version text
directly from `CARGO_PKG_VERSION`. Public exports in `src/lib.rs` provide native
connections/database values, query/Pull, caches and the PostgreSQL runtime from
this same dependency. The ordinary `src/lib.rs` and `src/bin/edb/main.rs` targets
are valid for the documented local dependency and `--bin edb` command.

Inspected actual admission/decoding boundaries rather than just format names:

- `PgBlockStore::install` rejects a nonempty namespace unless both provider
  tables and the exact `/2` marker match. `connect` checks the same marker before
  returning a runtime store; it does no upgrade or DDL.
- `encoding::decode_blob` rejects pre-cutover EDBC versions; `decode_program`
  additionally rejects ABI 12. Native `decode_submission` rejects wire versions
  other than 7 before decoding a request source.
- `Block::decode` authenticates bytes and rejects pre-cutover EDBO versions
  before parsing payload links/data. Tree readers require tree version 6;
  `IndexDescriptor::decode` requires kind 13 and all twelve current/history
  trees, with shared AVET/SAVET readiness.
- Backup point loading/listing uses `Block::decode`, then validates the point
  and selected publication/index coordinates. The existing backup test writes
  an authenticated old-format point and asserts listing fails. No source is
  invented from old information.

Existing `tests-integrated-second.log` records passing old-version rejection
tests for native submission, storage log blocks, all six tree orders, descriptor
kind, backup listing, EDBC/genesis and persisted program ABI, along with fulltext
pre-source format rejection. Its 43 library and 55 integration passes cover
source-bearing query/Pull, attribution and the unchanged native APIs. Reused
`check-all.log`, `rustdoc.log` (one pre-existing URL-format warning),
`application-build.log`, `native-workflow.log` and
`application-local-retry-after-fix.log`; the latter records SOURCE_OK,
APPLICATION_OK, local caching and the native async facade. These are acceptance
evidence for the unchanged guide, not claims that all release checks are done.

Fresh checks: `cargo test --locked --offline --test release_distribution_cutover`
compiled successfully and explicitly ignored its one live test without setup.
The explicit isolated invocation with `--ignored --nocapture` passed 1/1 tests;
`releases-cli-install-live.log` records that result. `git diff --check` passed.
No release build or install into a user's Cargo directory was repeated; previous
successful compilation/metadata establishes unchanged target selection. No
production resources, old checkout files or historical reports were modified,
and no commits were made. There is no remaining gap from this page review;
the parent still owns the complete documentation pass and readiness assessment.
