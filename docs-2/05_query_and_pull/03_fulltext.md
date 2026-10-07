# Native fulltext search

Fulltext is an optional derived search index over string facts. Background
indexing can lag transactions; each captured value uses one immutable index/search
attachment, and refreshing the peer produces a new value rather than mutating it.
It complements structured query; it does not replace identity, schema validation,
or the immutable database value supplied to a query. PostgreSQL stores opaque immutable objects and conditional references; backup
repositories can supply the same authenticated read graph without PostgreSQL. The analyzer, expression evaluator, scoring and queries run in
Rust; Lucene/JVM interoperability and identical Lucene scores are not promised.

## Declare and search

Install a `ValueType::String` attribute with `Attribute::fulltext()`:

```rust
Attribute::new(1000, Keyword::new("article", "text"),
    ValueType::String, Cardinality::One).fulltext()
```

The flag is false by default, is queryable as `:db/fulltext` schema data, and
cannot be toggled after installation. Both single- and multi-valued string
attributes are supported. New databases contain the vocabulary in their canonical genesis. Installing an
attribute is an ordinary transaction; opening a database does not rewrite schema.
See [the schema reference](../03_schema/02_schema_reference.md#dbfulltext)
and [changing schema](../03_schema/03_changing_schema.md).

```rust
let report = database.fulltext(1000, "\"blue river\" OR mountain*",
    &FulltextOptions::default())?;
for hit in report.hits {
    // entity: u64, value: original String, tx: assertion transaction, score: f64
}
```

The direct API returns descending scores, with deterministic entity/value/tx
tie-breaks within that search. Datalog's `Function::Fulltext` takes an attribute
keyword or numeric reference and a search string; relation binding produces
`[entity, value, tx, score]`. Join those entities with ordinary structured facts
to enforce status, ownership or other selection rules. See the working syntax
in [queries.md](00_queries.md#fulltext-and-structured-facts).

Each hit identifies an entity, value and assertion transaction. The value is the
original string, including its capitalization and punctuation. Several strings asserted
for a many-valued attribute on the same entity in one transaction remain distinct
hits. Retracting and later reasserting the same string produces distinct
assertion transactions in a retained history view. The query form can select an
explicit database source; both candidate validation and the returned transaction
refer to that source's view.

## Analyzer and expression rules

The versioned native analyzer lowercases Unicode alphanumeric words, strips
English possessive `'s`, and removes straight/curly apostrophes. Accents remain;
there is no stemming or arbitrary substring matching. Thus `JANE'S` matches
`Jane`, `café` does not become `cafe`, and `cat` does not match `category`.
Lowercasing is Unicode lowercase, not full Unicode case folding. Other
punctuation separates words: `blue-river` contributes `blue` and `river`.

The documented English stop words are omitted:

```text
a an and are as at be but by for if in into is it no not of on or such that
the their then there these they this to was will with
```

Queries support terms, quoted phrases, parentheses, uppercase `AND`/`OR`/`NOT`,
and a trailing `*` on a nonempty word prefix. Adjacent terms default to OR;
AND binds more tightly than OR. Phrases retain stop-word position gaps, so
`"red blue"` does not match `red and blue`. A stop-word-only search is empty.
Phrase matching compares the relative positions of the remaining words; it does
not compare omitted stop words. For example, `"red and blue"` can match
`red or blue`, since both place `blue` two positions after `red`. An unquoted
`blue-river` search uses the same default OR as `blue river`; use `"blue river"`
when the words must be adjacent.

Every OR branch must have a positive anchor: `river AND NOT mountain` is valid;
`NOT mountain` and `river OR NOT mountain` are rejected rather than scanning a
negative universe. Leading/infix wildcards, fuzzy terms, field syntax, boosts
and range expressions are rejected with `fulltext/query-syntax`. This is an
explicit native grammar, not a permissive Lucene parser.

Scores use native BM25 term-frequency, document-length and inverse-frequency
weighting. Treat them as relative relevance for the available index, not a
probability, application invariant or cross-version/Lucene-compatible number.
Datalog relation ordering is unspecified even when the direct search is ranked.
The corpus is the retained historical assertions in the selected search
publication plus committed recent and speculative assertions, not just entities
visible through the final current/time/filter view. Repeated assertions at
different transactions are distinct documents. Final hit validation enforces
the supplied view, but does not recalculate a view-specific corpus. Consequently
historical facts and filtered-out documents can influence a visible hit's score.
Filters are not a promise that ranking statistics reveal nothing about the
underlying corpus; use an appropriate independent database/access boundary for
that requirement.

## Views, indexing and limits

Candidates are checked against the supplied value's visible facts. Retained,
`as_of`, `since`, history, filtered and speculative views must not leak a fact
hidden by that view. The `tx` column is the visible assertion transaction, not
the indexing time. An old assertion can still appear in history after its
current value is retracted. An attachment is fixed for its captured value.
Indexing can change retained corpus statistics (for example, by physically
removing no-history assertions) without changing the logical snapshot key.
That key omits physical index placement and is not a fulltext-result cache key.

Persisted search reads the captured source-bound immutable attachment; it does not
fall back to materializing the whole native database when the projection is
missing. Missing/unusable required search data is an explicit error. Searches
also seek the requested attribute's bounded committed recent tail and local
speculative assertions, so new matches are visible before indexing. An attribute
installed after the captured index basis needs no attachment until that basis
includes it. `FulltextStats::index_basis_t` reports persisted indexing progress,
not a missing-match frontier. Indexing reduces tail analysis work; required
search data is built in the same coherent job as canonical indexes. Use
structured equality and uniqueness constraints for exact-value requirements,
not analyzed or top-k-limited search results.

`FulltextOptions` defaults to at most 10,000 returned hits, 10 million work
units, 64 MiB cumulative logical allocation/read admission, a 64 KiB query,
4,096 lexical query tokens and 64 levels of grouping/negation. Nesting is also
clamped to a hard maximum of 128, even if a caller requests a larger allowance;
flat Boolean expressions are balanced rather than recursively chained. These are configurable operation
budgets, not datom/storage limits, process RSS or network-traffic measurements.
Cancellation and an optional deadline are checked cooperatively, including
inside exact-view validation when filters reject candidate facts. Rejected
facts still debit the operation's work allowance. `limit` is
explicit top-k truncation (`stats.truncated`); work/byte/parser budgets fail with
an error instead of silently returning partial results. Limit zero returns no
hits and reports truncation when matches exist.
Visible hits are deduplicated by entity, original value and assertion transaction
before ranking and truncation, even when several sources supplied the same fact
in that transaction. Datom/history queries expose their distinct attribution.
Hidden candidates therefore do not consume the
result limit. The limit bounds returned hits, not candidate discovery or
validation work. Cooperative cancellation cannot interrupt an application filter
callback that never returns.

Queries debit search work and allocated bytes from their enclosing query or
stored-program budget, including attempted work on failure. A query row limit
remains an error, not an implicit search top-k. `QueryStats` reports search,
lagging-index and truncation counts; nested query counters are included. The
`fulltext_lagging_searches` counter means the persisted index was behind the
value, even though the same search included its unindexed assertions.
`FulltextStats::read_bytes` counts authenticated search objects loaded through
the backing source (PostgreSQL or repository; zero for eager fixtures and decoded
page-cache hits). `admitted_bytes` is the separate cumulative logical allowance.
Visits to decoded search pages debit that allowance even when the pages are
already cached; a warm cache does not remove operation budgets.
Search-page bytes exclude other metadata, protocol overhead and structured
candidate-validation reads. Use the operation's SQL/cache diagnostics for the
broader I/O picture.

## Stored programs and operations

The current program codec supports fulltext-bearing native templates, including
nested queries and rules; author them with the typed query API and current encoder. Fulltext is also allowed in transaction
programs: the writer re-executes the program and commits its grounded facts.
Ranking can depend on physical corpus statistics, so do not describe a
score-dependent program as deterministic from its logical db-before key alone.
Use structured indexes for exact-value correctness constraints.

Search data is derived, not authoritative history. Backup preserves the
canonical publication and supplies an exact read graph with a coherent search
attachment; restore imports that authenticated graph. Explicit administrative
rebuild can reconstruct search from canonical index facts. Cache hits must not
bypass access/generation rules or turn corrupted search pages into empty
results. Operational benchmarks should report dataset/index coverage, cold
versus warmed reads, measured SQL/payload I/O and real memory separately from
logical work counters.

The first build and explicit reconstruction stream fulltext history through
bounded external sorted runs. Ordinary later publications compare authenticated
canonical history trees, skip matching subtrees, and update only changed search
records and their routing paths. An unrelated change reuses the search root;
normal retractions retain historical assertion documents, while physical
`noHistory` omissions remove the corresponding documents and postings. Corpus
statistics are updated for touched attributes. Unchanged facts in repacked leaves
cancel before tokenization. The supplied-view checks described above still apply.
An authenticated predecessor containing only zero-valued corpus statistics can
use the bulk builder for its first text load. It merges the admitted mutations
with unchanged statistics without charging those old records as new input.
An empty-string document is still a document and prevents this shortcut;
non-text changes to an empty corpus still reuse its root without rebuilding.

Search pages are immutable content-addressed objects with explicit child links.
A source-bound attachment links its exact canonical index descriptor and search
root. Successors reuse unchanged pages directly, not a predecessor lookup chain
or a SQL namespace copy. Missing predecessors, generation changes and explicit
repair can require whole-projection work.

Incremental does not mean constant cost: roots/directories are inspected,
changed leaves decoded, affected search paths read/written, and source/GC guards
checked before adoption. A small corpus can fit inside one changed canonical
leaf. Root/directory metadata grows with tree width. `FulltextBuildStats`
distinguishes source work, tokenization, search-page I/O, spills and unchanged-root
reuse; it does not report fictitious SQL edges or trigger operations.

`ServiceOptions.fulltext_build_limits` and
`PostgresOperator::with_fulltext_build_limits(limits)` configure sort memory,
target page size, record/key capacities, input count, cumulative spill writes
and work directory. Service settings survive standby takeover and apply to
background indexing and automatic excision; operator settings apply to
consolidation, current-index recovery, search rebuild and excision. Invalid
numeric settings reject before work admission. Raising a valid limit can resume
an interrupted job without changing canonical request identity.

The lower-level `IndexInput::with_fulltext_build_limits` and `storage::fulltext`
builders expose the same policy. Defaults remain 8 MiB sort admission,
64 KiB target pages, at most 10 million input
records per operation and 4 GiB cumulative spill writes. Incremental input counts
mutations, not unchanged reused records; this is not a maximum database size.
One admitted record/page may exceed the target sort/page size, subject to hard
format limits. Spill counts include merge rewrites, not live disk occupancy.

`FulltextBuildStats::blocks`/`encoded_bytes` count page upload attempts,
including intermediate and deduplicated pages; `FulltextProjection` totals
describe the final reachable tree. `peak_buffer_bytes` tracks sort/editor
workspace, not process RSS or all source caches and retained values.
Measure complete operation time and foreground/worker I/O separately.

`NativeFulltextReader::cache_stats` exposes cache-owned decoded bytes, entries
and configured bounds. This positive header/page cache has a separate allowance
equal to the configured native tree-cache bounds; it is not part of tree-cache
occupancy. Its decoded entries are not persisted; underlying immutable objects
can use the shared SSD cache on a live snapshot. Active cursors may retain
additional pages outside cache ownership. A fully warmed search can require no
SQL. New live captures establish current
authority; subsequent cold source reads remain subject to storage retention.
A cache hit is not an authorization token.

The block background service prepares canonical indexes and their search
attachment as one coherent candidate. `BackgroundIndexingStats.fulltext`
observes this combined job's attempts, failures and successfully adopted checked
basis; a preparation error is not necessarily a fulltext-specific error. There
is no separate idle search worker. Candidate preparation failures remain
visible and retry through the actual indexing
scheduler. A candidate with required fulltext data is not published without its
authenticated search attachment. Ordinary transactions can continue while a
background candidate is prepared; an admitted excision deliberately parks fresh
transactions until its complete successor, including search, is activated.

## Retention, repair and excision

`PostgresOperator::rebuild_fulltext(database_id, expected_index)` prepares a
replacement attachment and adopts it conditionally. The stable route ID comes
from the catalog; the optional expected descriptor guards against replacing a
different concurrent source. The CLI's `fulltext-rebuild --discard-manifest`
spelling supplies that expected digest, not permission to delete pages under
readers. Reopen/synchronize to observe the replacement; existing held values
continue using their old immutable graph.

Search objects share canonical indexes' authenticated graph ownership. Current publications,
exact receipts, authorized retained indexes and protected work all
participate in the same GC protocol. Dropping an old root does not delete pages
reachable through a successor or another owner. GC advances bounded persisted
work and honors retention; no feature-specific SQL page frontier, sidecar import
or physical discard procedure is involved. Search repair is not secure erasure.
Holding a database value or search cursor does not by itself promise indefinite
retention of every backing object. Active readers may keep decoded pages alive
after cache eviction; later cold reads still depend on storage retention.

Excision builds search from the rewritten successor before activating its new
generation. New current/history views exclude excised facts, while already-held
authorized old values can read their pre-excision graph within storage retention. New serialized openings
of the old generation are denied. GC waits for remaining owners and retention;
independent backups, SSD copies and already delivered application data are
separate retention responsibilities. See [operations](../08_operations/00_deployment.md#excision).

## Operational boundaries

The separate application follows the
[application guide](../01_tutorials/01_application_workflow.md). Exercise
structured joins, exact views, incremental updates, captured and speculative
views, backup restoration, corruption handling and cancellation against a
disposable PostgreSQL instance before deployment.
No local fixture establishes Lucene parity or a universal search throughput
claim.
