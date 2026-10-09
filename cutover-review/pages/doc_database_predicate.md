# Database predicate review

Observed: the assigned predicate page still says point-in-time callbacks see
retractions, filtering precedes current collapse, and rejecting a retraction can
restore an old value. `views.rs` now resolves complete EAV history globally,
then applies since/custom selectors. Its callback database removes every custom
predicate while preserving the basis and final temporal/history settings.

Cause: the page retains the earlier event-filter model; NORTHSTAR requires one
shared current record and cross-source withdrawals. Existing cutover tests cover
global visibility, six indexes, boundaries, and overlays, but do not directly
exercise callback context after later view operations or chained predicates.

Bounded problem: make this page describe the settled filter contract precisely,
and verify the unfiltered callback context and conjunction without changing the
API or reimplementing visibility. Review query/Pull/entity consumers and error
and cancellation controls for feature parity.

Approaches and tradeoffs: changing predicates back to event selection would
conflict with shared-record semantics; classifying source predicates would bind
visibility to opaque user code. Reusing the common global visibility window
keeps attribution, visibility, and user selection independent. A focused
callback regression test supplies missing evidence without repeating existing
withdrawal tests or altering the engine.

Selected: naturally revise callback/evaluation prose, preserve names and
arguments, and add one callback-context/conjunction test. Use existing Pull
cancellation and query/Pull/filter tests as evidence where already sufficient.
No production resources or code outside this copy are involved.

Acceptance: callbacks in current/as-of reads receive surviving assertions only;
history callbacks receive bounded raw events. Later as_of/since/history operations
are reflected in callback db, all custom predicates are removed, nested reads
do not recurse, and chained predicates short-circuit. Run the new focused test
and doc diff checks. Reuse root's five filter, nine query, and seven Pull passing
tests; root performs full integration validation.

## Changes and results

Revised the assigned page's callback and evaluation descriptions: point-in-time
callbacks see globally surviving assertions, event selection belongs to history,
source/transaction/custom selection cannot restore withdrawals, and cardinality
counts values. Removed the later-collapse claim and documented cooperative
callback cancellation. The example retains ordinary transaction forms and its
single request-level system source. No engine code changes were necessary.

Appended one regression to the existing filter test file, reusing its fixture:
`callback_context_tracks_later_views_and_removes_every_chained_predicate`. It
checks final basis/as_of/since/history context, recursively safe nested prefix
reads, history entity rejection, short-circuiting across every chained filter,
and immutable original view metadata. The focused invocation passed: one passed,
zero failed, five filtered out. This adds callback evidence to the root-reported
five passing visibility tests without repeating their scenarios as new tests.

Read-engine errors remain fallible cursor results; the callback remains Boolean
and application-defined nested error handling is accurately documented. Reviewed
Query's post-callback `State::check`, Pull's post-callback checks, and shared
cursor polls on rejected candidates and complete-EAV probes. Existing
`cancellation_still_checks_duplicate_source_candidates` covers accepting a
callback that cancels Pull, and existing Pull history tests reject history
before evaluating the callback. No cancellation gap or extra test was warranted.

`git diff --check` on the doc and test paths passed. No PostgreSQL access or
global formatting was needed. No commits were made. Full integration checks
remain the parent's responsibility; there are no known remaining gaps for this
page's contract.
