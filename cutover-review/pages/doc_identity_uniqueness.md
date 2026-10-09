# Identity and uniqueness review

Situation: the assigned page already states one request-level source, identity and
uniqueness across sources, EAVS redundancy, and db-before lookup resolution. It
agrees with NORTHSTAR; retain its prose unless a demonstrated contract gap needs
an ordinary correction. Existing schema14, source/CAS, Entity, raw-index/query,
and URI/NaN tests cover most behavior.

Bounded question: reference/tuple unique keys and multiple identity claims must
use ordinary upsert and lookup rules even when the resulting entity is also the
request source. Inspect assessor grouping/resolution, holder deduplication,
source resolution, ident/read lookup paths, partition coordinates, and UUID
helpers. Reuse existing evidence for unchanged paths; add focused acceptance for
uncovered identity branches rather than repeat broad suites.

Decision/tradeoffs: source belongs in material redundancy and recorded datoms,
not logical identity comparison or unique-holder keys. Preserve that separation
for independent reasoning and change, using the existing resolver and material
boundary rather than a source-specific adapter. A failing documented scenario
will be reproduced first; coordinate with root before any shared code change.

Acceptance: cross-source reference and tuple-key upsert keeps the original eid;
source tempid/lookup resolves to that same entity and attributes generated time;
multi-key conflicting holders fail atomically; unique-value tempid does not
upsert; lookup cannot resolve a key created in the same transaction. Verify eager
and speculative paths where relevant. Append actual results and exact residual
gaps; no commits or live PostgreSQL needed unless a storage gap is demonstrated.

## Reproduction and selected repair

Reference-key and heterogeneous-tuple-key tests both reproduced a unique-conflict
where the dependent record tempid should upsert to its existing holder, also the
request source. Cause: raw symbolic identity grouping is followed by only one
lookup pass; keys containing TempRef are never reconsidered after their targets
upsert. Root approved assessor and input ownership.

Use the initial keyed union plus a dependency worklist, not repeated full scans:
register ref dependencies once; newly known existing-root bindings wake only their
claims; resolved keys use ordinary db-before Reader::lookup once and the existing IdentityClaimKey comparator for newly equal identity claims,
including symbolic aliases. Union membership uses
linked groups to notify newly bound names once without repeated group copying.
No source-specific identity policy, extra index, or compatibility path. Preserve
new-only cycles/allocation and verify multi-hop order independence/conflicts.


## Actual changes and evidence

The assigned page is aligned and remains unchanged. Reviewed the complete page,
NORTHSTAR, timestamp/source reference, CUTOVER, and supplied talk guidance; used
explicit problem evidence and independent identity/attribution responsibilities.
Inspected checked identity/UUID helpers, IdentIndex history folding, Entity origin
and forward ident navigation, ordinary read/index/query identity resolution,
EDN and tuple/ref lowering, assessor key grouping/allocation and source resolution,
unique-holder successor projection and the EAVS material boundary. Existing
schema14, Entity, query/index and URI/NaN evidence covers unchanged branches.

Implemented the reproduced ref/tuple identity dependency gap in assessor
`resolve_identity_dependencies` and input `UpsertIdentityValue` traversal and
canonicalization. Initial grouping, comparison, deterministic representative and
allocation rules remain in use. A dependency worklist propagates root aliases and
existing bindings, rather than repeatedly scanning all claims. Linked group
membership concatenates without copying. Only changed keys are reconsidered
(counted as actual identity_claims); fully resolved claims do one charged ordinary
lookup. New-only cycles finish without speculative database keys. Source resolves
after final identities and every material datom uses that resolved source.

Also reproduced renamed-ident lookup source failure. `Reader::lookup` now reuses
`DatabaseValue::lookup` for :db/ident, preserving its existing resident dictionary
policy and ordinary counters. IdentIndex deliberately retains historical aliases
after both rename and explicit ident withdrawal; there is no separate retraction
policy to invent. Ordinary domain unique keys still disappear on withdrawal.

Added `tests/identity_uniqueness_source_cutover.rs`, nine portable in-memory tests:
ref and tuple dependent upsert; reversed eager/overlay 64-hop mixed chains with
exactly 66 lookups/matches; new-only ref/tuple alias composition; late existing
binding through key unification; deterministic new-only cycles with 0 lookups;
conflicting keys and unique-value failures with unchanged bases; db-before-only
source lookups and ordinary key withdrawal; retained ident aliases after rename
and explicit withdrawal through direct Ident/native Lookup/deferred EDN sources,
unknown ident rejection, and minimum positive 1-datom/1-byte read capacity.

Before logs `identity-uniqueness-initial.log` and `identity-ident-alias-before.log`
record the reproduced contract failures. Final focused 9/9 and strict Clippy pass:
`identity-uniqueness-focused-final.log`, `identity-uniqueness-clippy-final.log`.
`identity-uniqueness-final.log` records 80 passing checks across library, identity,
URI/NaN, schema, changing-schema and diagnostics suites, with 2 explicit live ignores.
The final focused run includes extended alias/withdrawal/capacity assertions added
after that combined run. Diff whitespace checks pass; assigned page diff is empty.
Two intermediate fixture errors were corrected: chain seed collided with an
already held ref key; same-transaction attribute install/value input violated
db-before schema resolution. Zero read capacity is invalid, so dictionary proof
uses the smallest permitted positive capacity. None was a library regression.

Coordinated shared-file ownership with Transaction Data worker: its timestamp
validation and map-depth admission hunks remain distinct and were not edited here.
Root reviewed the resolver and initial acceptance bodies. No remaining page-specific
behavior gap found; no PostgreSQL test run by this worker. Root owns partition
composition, complete integrated/live validation and integration. No docs beyond
the assigned unchanged page, compatibility paths, commits or production writes.
