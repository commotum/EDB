# Optional local SSD cache

Create a dedicated private cache directory (mode `0700`) and opt in with
`ATOMIC_SSD_CACHE_DIR`. Optional positive `ATOMIC_SSD_CACHE_ENTRIES` and
`ATOMIC_SSD_CACHE_BYTES` bound its shared directory; defaults are 4096 entries
and 1 GiB of physical file lengths, including framing, not filesystem allocation.
All processes sharing that directory must use the same limits. Without the
directory setting SSD caching is disabled. Unsafe paths or conflicting limits
fail opening clearly; runtime contention, corrupt entries or disk errors fall
back to PostgreSQL. The library equivalent is
`PostgresConnectionConfig::with_ssd_cache(SsdCacheConfig { directory, limits })`.
Concurrent initialization retries its directory lock for one second before
returning `cache/busy`. A crash before publishing the ownership record can leave
an unclaimed initialization file: opening then reports `cache/unsafe-config`
and preserves it. Inspect that directory or choose a new empty private root;
these uncertain files are not automatically deleted.
Malformed owned files at recognized `.block` names do not prevent reopening:
the cache reports corruption and `inventory_complete=false`, preserves those
files, and bypasses writes/purge while reads fall back to PostgreSQL. Reported
occupancy includes their physical sizes. Unknown files, unsafe permissions or
links still reject opening; malformed framing is not permission to delete them.

New peers still authorize and authenticate their root through PostgreSQL. Cache
names separate connection/trust configuration, database lineage, excision
generation and format; they are not authorization tokens. An SSD hit supplies
hash-authenticated canonical bytes under the captured reader's protection.
A new live capture still needs PostgreSQL authority; SSD reuse does not promise
disconnected startup or indefinitely available historical data. Fully resident
RAM reads avoid foreground SQL within the stated retention policy.

`Peer`/`Connection::ssd_cache_stats()` distinguishes hits, misses, corruption,
busy/error bypasses, eviction and physical/canonical byte counts. Files are
private but not encrypted. They may contain personal or sensitive data. Excision
does not automatically remove existing local copies, backups or RAM values.
`purge_ssd_generation(generation)` removes recognized entries of that generation
from this cache root, returning false on a busy/error path; it is not secure
erasure and does not purge other processes' differently configured namespaces.
Retire obsolete cache roots explicitly under your data-retention policy.
