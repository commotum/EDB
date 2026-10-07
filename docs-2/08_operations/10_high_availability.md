# High Availability (HA)

High Availability (HA) keeps an EDB transactor available in the event of a
single transactor machine failure by running another process that can take over
writer authority. This requires available shared PostgreSQL storage; transactor
HA does not provide PostgreSQL replication or storage failover.

## Enabling HA

To enable HA, run two transactor processes on separate machines, configured to
use the same PostgreSQL database, schema and logical EDB database. Run both with
`--mode auto`, and use matching transaction defaults, resource policies and
transport settings. Each process needs its own listener address and credentials
appropriate to that machine.

For example, after installing storage and creating `orders`, run the following
on each machine, substituting that machine's reachable advertised address and
certificate name:

```sh
target/debug/edb transactor --database orders --mode auto \
  --listen 0.0.0.0:7443 --advertise 192.0.2.10:7443 \
  --tls-server-name writer-a.example.com \
  --lease-ms 5000 --renew-ms 1000 --standby-poll-ms 250
```

Set `EDB_POSTGRES_URL` to the writer connection and provision the remote TLS
certificate, private key and shared token as described in the
[transactor reference](07_transactor_reference.md#tls-certificates-and-trust-roots).
The example address and name are placeholders. Remote clients need PostgreSQL
read credentials, the shared token and trust for both servers' certificates.

The first process to acquire the guarded writer lease becomes active. The other
waits in standby until the lease is released or becomes eligible for takeover.
The default `--mode active` instead fails startup on contention; two processes
started with that default do not form an automatically waiting pair.

Writer authority belongs to one logical database. Observing an expired lease
allows a process to attempt takeover; only a successful conditional storage
update grants authority. That update publishes the new writer epoch and lease
together, provided the captured database, lease and collection guards still
hold. Publishing or discovering a listener endpoint does not itself grant
permission to append transactions. If the claim acknowledgment is lost, EDB
recognizes ownership only by finding the exact lease it proposed.

Provide a supervisor that tries to keep both machines running the transactor
process at all times. Once active, a stock process that loses service or listener
availability exits unsuccessfully for the supervisor to restart. It does not
return to standby within the same process. For embedded services,
`TransactionStandby` transfers one activated `TransactionService` to its caller,
which then owns service and listener management.

## Peer Recovery Time

The time for an application to recover transaction submission from a transactor
failure consists of several components:

- The time until the former writer's lease is released or eligible for takeover.
- The time for a standby to claim authority and complete activation.
- The time to start and publish the new listener and for the application to
  discover it and reconnect.

The stock defaults are a 5,000 ms lease, a 1,000 ms renewal interval and a
250 ms standby polling interval. Renewal must be positive and less than the
lease duration; polling must be positive. These intervals do not establish a
worst-case recovery time. Storage calls, recovery work, network conditions,
process scheduling and client retry policy also contribute.

You can choose shorter intervals after carefully considering two risks:

- Network anomalies can trigger unnecessary takeover if they could have
  resolved themselves within a longer lease window.
- Long process pauses, CPU starvation or blocked storage operations can delay
  renewal even when the process has not failed.

Test EDB in your environment before changing the intervals. Lease expiry uses
wall-clock time, so clock behavior also affects takeover eligibility. A successor
claims a new writer epoch; guarded root and lease revisions fence publications
even under clock skew. A stale writer cannot commit merely because its work
began before takeover. Lease duration is not an execution deadline for arbitrary
transaction callbacks.

Remote discovery authenticates the advertised endpoint against the database
identity and current writer authority. `Connection::remote_writer` can
rediscover a failed cached route and try its successor once when the request
has not been transmitted. There is no fixed once-per-second discovery loop.
Local socket clients must select the socket of the process that became active.
See [connecting to the transactor](07_transactor_reference.md#connecting-to-the-transactor).

## Reads Remain Available

Peers can continue to read and query during a transactor failover. In particular,
`Connection::db()` returns its most recently adopted immutable database value
without PostgreSQL I/O or transactor coordination; it does not wait for a new
writer. A held database value does not change when writer authority changes.

Queries that need uncached objects still require available storage, and those
objects must remain within storage retention. Capturing a newer durable head
also requires storage. Writer HA does not make reads independent of PostgreSQL
or turn an old database value into the latest one. See
[database values](../02_core_concepts/00_database_values.md) and
[synchronization](../04_transactions/09_synchronization.md).

## Transaction Latency After HA Recovery

A standby does not continuously maintain a ready transaction service. After
claiming the lease, activation captures the current immutable publication and
loads its indexed state and admitted recent log tail. This happens before the
stock process starts serving transactions and prints `READY`.

Activation does not replay the entire database history to rebuild current
indexes. Damaged current indexes cause startup to fail; explicit recovery uses
`edb consolidate`, as described in [index recovery](00_deployment.md#explicit-index-recovery).
Canonical or retained-history damage requires separate recovery.

Explicit index recovery rebuilds the current derived index from the canonical
log and validates the allocation and transaction-time checkpoint. It preserves
exact receipts and retained read authorization; it does not repair damaged
historical retained indexes. Preparing an index does not grant writer authority.
Before adopting prepared output, EDB checks the current writer epoch, database
generation, source-index lineage and canonical log endpoint. Adoption preserves
the current log and receipts, including transactions newer than the prepared
index.

After activation, cold caches and background indexing can affect transaction
latency; indexing back-pressure can delay fresh work. Measure activation time
and post-failover latency with your database and workload. EDB provides no
fixed several-second catch-up or latency bound. `STANDBY` is not write readiness;
`READY` and the optional `/ready` probe reflect completed activation and listener
startup. See [health checks](07_transactor_reference.md#health-check-endpoint).

## Application-Level Retry

EDB does not automatically resubmit a transaction with an ambiguous outcome
after transactor failure. Applications, aware of their own semantic needs,
implement an appropriate retry policy. A timeout or lost connection after
attempted transmission can mean `UnknownOutcome`, even if the transaction
committed.

Retry with the **same request key and identical transaction content**. EDB's
retained exact receipts return the original committed result without repeating
the transaction. Receipt lookup precedes fresh-transaction admission, so a
committed retry does not need to pass the current capacity limits or basis
precondition again. Do not use a new key to resolve an unknown result. A confirmed
commit remains committed even if opening the caller's local report fails.
Applications also have access to recent transaction history through the
[Log API](../07_peer_api/06_log_api.md). See
[error handling](../07_peer_api/07_error_handling.md) for outcome distinctions.

## Switching to a Different Transactor Version

For versions whose storage, transaction and transport formats are compatible,
normal HA takeover can support a controlled replacement:

1. Start a new transactor pair with the intended version in `--mode auto`.
2. Perform the operational checks appropriate to your environment: verify
   process health, configuration, certificates and standby status. A healthy
   standby is not proof that activation or application transactions will succeed.
3. Stop the original standby and prevent its supervisor from restarting it.
4. Stop the original active process and prevent its supervisor from restarting
   it. Confirm a replacement reaches `READY`, then exercise application reads,
   writes and exact retries.

The two original processes may be stopped together. Keep one replacement
waiting as standby after the other becomes active.

EDB does not promise rolling compatibility between arbitrary versions. Check
the [release notices](../00_start_here/06_release_notices.md) and test the intended
versions before using this procedure. Unsupported storage and backup formats
are rejected, not automatically upgraded. Earlier development data may require
a fresh installation and regenerated data; HA is not a format migration tool.

## Moving Across Data Centers / Regions

You may want to move an EDB system from one data center or region to another,
for example to recover from the catastrophic failure of a data center. Perform
the following steps, in this order:

1. Shut down all EDB-related processes, including transactors and peers, in the
   data center you are leaving. Disable automatic restarts. If that site is
   unreachable, isolate it so it cannot resume accepting application writes.
2. Restore a **consistent** copy of the EDB data into the new data center.
3. Configure storage access, certificates and advertised endpoints for the new
   location, start its transactors and open new application connections. Verify
   current and historical reads, writes and exact retries before routing traffic.

Two independently restored PostgreSQL stores do not share a writer lease.
Writer fencing within one store cannot prevent both sites from accepting
divergent writes. The selected recovery point also determines which recent
transactions are available after a disaster.

### Use EDB Backup For Consistent Copy

EDB's [backup and restore](11_backup_and_restore.md) makes a
consistent copy of one immutable database publication, including its indexes,
log tail and retained exact receipts. A backup point becomes visible only after
its reachable objects are durably copied. Storage retention must cover the
copy duration. Thus a completed EDB backup is suitable for use in a
disaster-recovery plan.

Keep backups independently of the failed site, verify them and test restoration.
Successful copying authenticates the object graph; use deep verification for
semantic agreement between indexes, receipts and canonical history. Install
the destination storage and stop its writer before applying restore. Retry an
interrupted restore with the same point and destination. Open new clients after
restoration; old handles do not automatically retarget. Backups can also be
[queried directly](05_backup_reads.md) without PostgreSQL or a transactor.

### Other Consistent Copy Options

You can also copy EDB data using PostgreSQL's backup or replication facilities,
an approach called a *storage copy*. Storage copy may be preferable to EDB
backup for disaster recovery for reasons including:

- Operators are already familiar and comfortable with the tools involved.
- The disaster-recovery strategy needs to protect data placed in storage by
  other applications besides EDB.
- Storage facilities provide replication that can reduce recovery time.

A storage copy must comprise a complete, consistent set of writes through the
chosen recovery point. Preserve both `edb_objects` and `edb_refs` coherently,
including catalog and authority references. An independently copied table or
an arbitrary mixture of references and objects is not sufficient.

A PostgreSQL backup and WAL recovery procedure can provide that consistent
point; configure and test it as part of your storage deployment. Asynchronous
replication may leave the recovered point behind the failed site's last
acknowledged writes. EDB's transactor lease neither supplies replication nor
guarantees zero data loss across storage promotion. See
[storage services](06_storage_services.md) for the storage contract.
