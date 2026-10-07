# Writing a Problem Report

EDB is a database, so problem reports about EDB tend to be about putting data
in, getting data out, or both. This guide will help you turn unexpected EDB
behavior into a reproducible problem report.

## Anatomy of a Report

A reproducible problem report (hereafter just "report") contains the following
elements:

- **Environment**: the context in which the problem occurred. This needs to
  include everything necessary for a third party to reconstruct the environment.
- **History**: the steps to go from a starting environment to the moment a
  problem occurs.
- **Expectation**: what you expected to happen.
- **Actual**: what actually happened.
- **Evidence**: how do you know what happened.
- **Impact**: what are the effects and severity of the problem?

A report is also noticeable for what it does **not** contain:

- Analysis
- Attempts to assign causes and effects
- Recommended courses of action
- Judgment

This is the correct pre-analytic posture, and it will also improve the mood of
your colleagues. Analysis is important too, but when you are reporting try to
stay in "just the facts" mode. As Sherlock Holmes says, *"It is a capital
mistake to theorize before one has data. Insensibly one begins to twist facts
to suit theories, instead of theories to suit facts".*

> Note that we are carefully avoiding the use of the word "bug". We save "bug"
> for problems where we have a hypothesis, a reproducing test, and a fix.

For EDB, make the environment and evidence concrete:

- Identify the `edb-core` package version, source revision and any local changes,
  Rust toolchain, operating system, architecture, and build profile. `edb
  --version` prints the package version; it does not identify the source revision
  or build configuration. Include `rustc --version` and the exact build command.
- For a durable database, include PostgreSQL version and relevant configuration,
  database identity/lineage, and the roles and transport used by each process.
  Record relevant EDB configuration values, replacing credentials with explicit
  placeholders. Keep enough configuration to reproduce the behavior without
  sharing passwords, tokens, or private keys.
- Include complete schema, transaction, query, Pull, rule, and input data needed
  by the reproducer. Record transaction order, explicit transaction times when
  relevant, database basis T, and any `as_of`, `since`, or history views. A
  database name alone does not specify a database value.
- Preserve command lines, stdout, stderr, and exit status. Native failures carry
  `SemanticError` fields `category`, `code`, `message`, `details`, and optional
  `anomaly`. Its `Display` output includes only the code and message. The CLI
  deliberately prints reduced error information: category, code, valid numeric
  input locations when present, and messages only for `cli/` or `config/` errors.
  Include relevant native diagnostic fields when available; see
  [error handling](../07_peer_api/07_error_handling.md).
- For performance problems, include the operation, data sizes, elapsed times,
  concurrency, resource limits, and whether caches were warm. Attach relevant
  [query diagnostics](../05_query_and_pull/04_diagnostics.md),
  [I/O stats](../07_peer_api/08_io_stats.md), and
  [monitoring observations](../08_operations/03_observability.md), with units,
  timestamps, and the process that produced them. `query --query-stats` wraps
  the returned value with query diagnostics; `--io-context :report/query` enables
  operation I/O reporting. Capture host and PostgreSQL measurements separately
  when they matter.

For example, with `edb` on your `PATH`, capture a query's observations using the
same database, files, and limits as the failing operation:

```sh
edb --version
rustc --version
edb status --database repro
edb query --database repro --file query.edn --inputs inputs.edn \
  --query-stats --io-context :report/query > actual.edn 2> error.txt
query_status=$?
printf '%s\n' "$query_status" > exit-status.txt
```

Supply `query.edn` and `inputs.edn` with the report; omit `--inputs` if the query
has no non-source arguments. `status` records catalog state, including basis T
and generation. It does not prove a live transactor or fix the basis used by a
later query. Record the database value actually used by the reproducer, and
describe whether enabling diagnostics changes the observed behavior.

## Writing a Good Report

When you first encounter a problem, you are typically in the middle of something
bigger. To write a good report, you must winnow that bigger thing down to
something that is both concise and precise. Concision here is about the number
of steps: if your problem history contains N > 1 steps, search for a problem
history with fewer steps until you have the smallest number of steps that still
reproduce.

Precision is how completely you specify each step. Ideally, you will provide the
exact data for each step. If you attempt to describe the data (instead of just
enumerating it) it is very easy to introduce presumptions that generalize the
problem in ways not (yet) supported by the evidence.

Resist this.

## EDB and Data Make Reporting Easy

EDB is particularly well suited to problem reporting:

- Database values are **immutable**, and queries and transactions have direct
  data representations. Retaining the database value and the operation's input
  data avoids complex and error-prone recipes for getting the system back into
  the state where the problem occurs. Retain the sequence that constructs the
  value so another process can reproduce it.
- EDB's [EDN frontend](../01_tutorials/00_edn_workflow.md) represents domain
  entities as **generic** data: plain maps, lists, strings, etc. This eliminates
  the need for complex and error-prone "domain object" or "model" code. It is
  easy to simplify EDB problem reports to a small number of operations with
  generic data because that is what EDN transactions and queries already look
  like. Native Rust callers can likewise supply the exact typed input values.
- The CLI can read query, transaction, and Pull data from files or stdin. When
  you are homing in on a problem, edit those inputs and rerun the operation,
  carving out the parts of the application that are irrelevant to the problem
  at hand. A small Rust program serves the same purpose for native API behavior.

## Getting Concise

Reports should include only the steps necessary to trigger the problem. In
particular, they typically will **not** include:

- Test frameworks
- (In)convenience wrapper libraries
- Build automation beyond what is needed to build and run the reproducer
- Dev tools

The "reduce the problem steps idea" should be applied not just to steps, but
also to dependencies. If you don't think a library is part of the problem, it
should not be in the report. Taking this from the opposite direction, most EDB
semantic problem reports should, after iteration, include an application that
depends only on `edb-core`. Keep PostgreSQL, transport, concurrency, or other
dependencies when removing them removes the problem.

Below are some specific tips for how to make a Rust or EDB repro more concise
and precise.

## Rust Tips

- Functions should depend on their arguments, not on data in globals or hidden
  process state.
- Try to reduce or eliminate control flow. If the repro branches, can you
  eliminate some branches entirely?
- Try to minimize the number of modules and crates your repro depends on.
  Include the small program's `Cargo.toml`, dependency resolution information,
  and exact run command so another person can build it.

## EDB Tips

- The state preceding a problem should generally be represented as a single
  database value, or the transaction data to build a value.
- Try to call `Connection::db()` only once in the steps of a problem report.
  It returns the connection's most recently adopted immutable value without
  contacting PostgreSQL or the transactor. Record its `basis_t()`; retaining
  this value prevents later connection advancement from changing the read basis.
  If advancement itself is the problem, retain the separate captures and the
  exact refresh or synchronization steps.
- Give every function in a problem report a Rust documentation comment that
  explains the function's purpose, inputs, and outputs.
- Naming the nouns is just as important as naming the verbs. Put query and
  transaction data in variables or files, and give them good names.
- Every EDB API call in a repro should either contribute to the problem or be
  removed from the repro. This is the earlier point about reducing the number
  of steps applied mechanically to each call.
- EDB deployments can involve many processes: transactors, native peers,
  application clients, and PostgreSQL. When you describe the environment and
  steps for a problem, be specific about which process(es) you are describing.
  Distinguish embedded, Unix socket, and remote TLS transaction submission.
- If your repro involves more than one process, try to shrink it down to a
  single process. For semantic problems, the native `Database::new(Schema::new())`
  and `Database::with` APIs provide immutable in-memory database values without
  PostgreSQL. An existing database can also be read and speculatively changed
  through [offline backup reads](../08_operations/05_backup_reads.md). Neither
  substitutes for the durable writer when the problem concerns storage,
  idempotency, fencing, or transport.
- For transaction problems, record the request key, exact submitted data,
  basis precondition, time override, and the result observed at each boundary.
  A timeout does not establish that a transaction failed to commit. See
  [processing transactions](../04_transactions/06_processing_transactions.md)
  for unknown outcomes and exact retries.

## Additional Resources

- [Why Programs Fail](https://www.elsevier.com/books/why-programs-fail/zeller/978-0-08-092300-0)
  is the book to read if you are debugging software.
- [Debugging with the scientific method](https://www.youtube.com/watch?v=FihU5JxmnBg)
