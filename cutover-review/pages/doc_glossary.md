# Glossary review

The full page already uses EDB names, six-field facts, six indexes, one
request-level source, generated timestamp attribution, and shared cardinality
and retraction semantics. NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER and the
three supplied talks were read. No misleading definition or implementation
parity gap was found. Keep the aligned file unchanged: optional elaboration on
source resolution or retries would duplicate linked references without fixing
a defect. This preserves precise terms and independent responsibilities.

Acceptance: preserve the complete page, verify its source contract against
implementation and existing concrete acceptance logs, and check link targets.
Source remains independent of domain authorship and clock selection, while
ordinary references and one assessment boundary compose attribution without
new adapters. No code change is indicated by the inspected behavior.

Evidence inspected: `src/model/datom.rs` retains E/A/V/T/S/bool and the six
index orders; `src/transaction/assess/mod.rs` resolves source ordinarily,
generates one timestamp, attributes all material changes, suppresses only
same-source assertions and validates distinct shared values;
`src/index/visibility.rs::current_eav_group` applies global withdrawal before
source restriction; `src/database_value/resolve.rs::values` deduplicates stored
values; `src/encoding/mod.rs::canonical_submission_request` hashes the supplied
source, data, basis and explicit instant; `src/transactor/authority/mod.rs`
checks the exact receipt before fresh resolution/clock selection.

Inspected `tests-integrated-second.log`: 43 library and 55 integration checks
passed, with live cases explicitly ignored. Relevant passing checks cover
same-EAVS redundancy, cross-source material assertions/CAS/uniqueness, generated
source inheritance, six-tree order/eligibility and source activity. Inspected
`tests-durable.log`: both durable transaction-model tests passed, including
receipt-first replay after source ident/lookup facts were removed, changed-source
request-key conflict, cross-source replacement/withdrawal and reopen. No tests
were rerun because behavior and code are unchanged. No remaining glossary gap.
Ran a local Markdown link/heading check: all 117 links resolve to existing
files and headings. `git diff` confirms the assigned documentation is unchanged.
