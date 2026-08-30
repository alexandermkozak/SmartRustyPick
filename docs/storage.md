# Storage Engine

SmartRustyPick uses a hashed storage layout for table records to ensure that write performance remains consistent even
as tables grow.

## Hashfile Layout

Table records are stored in a directory named `data.hf/` within the table's directory. This layout replaces the previous
flat file format.

### Files

- `data.hf/meta`: A text file containing metadata about the table:
    - `version`: A counter that increments on every flush, used for cross-process freshness detection.
    - `modulus`: The number of groups currently in use.
    - `records`: The total number of records in the table.
  - `checksums`: `1` once every group file carries a checksum trailer.
  - `checksum`: A CRC32C of the other lines, written first so a truncation cannot remove it unnoticed.
- `data.hf/g<8 hex digits>`: Group files containing the actual records. Each file corresponds to a hash group. Empty
  groups do not have a corresponding file.

### Frame Encoding

Both group files and the dictionary file (`dict`) use a simple frame encoding:
`[key_len u64 LE][key][data_len u64 LE][record bytes]`

A group file ends with a fixed 20-byte trailer:
`[magic "SRPHFG01"][record_count u64 LE][crc32c u32 LE]`

The checksum covers every byte before the trailer. Without it a torn tail is indistinguishable from a short group, and a
partial write reads back as "these records simply do not exist". The `meta` file is checksummed the same way, on its
first line, so that a truncation cannot take the checksum with it and leave a plausible-looking older file behind.

### Hashing and Dynamic Modulus

A record's group is determined by:
`fnv1a64(key) % modulus`

The modulus is dynamic and adjusts based on the number of records:

- **Minimum**: 8 groups.
- **Growth**: The modulus (a power of two) doubles when the average number of records per group exceeds the
  `records_per_group` target (default 16).
- **Shrink**: The modulus drops back to the target only when the table falls to a quarter of its capacity (hysteresis)
  to prevent repeated rehashing at boundaries.

Changing the modulus rehashes the whole section. Because the modulus doubles, the amortised cost of rehashing is
constant per inserted record.

### What a Write Costs

Flushing a changed record reads its group file, applies the change and writes the group back. Nothing in that path
scales with the size of the table, so updating one record costs the same on a table of ten records as on one of ten
million. A full rewrite of every group happens only when the modulus changes, when a table is edited in bulk (a change
that cannot be attributed to individual keys), or during migration from the old format.

### Atomic Writes

When a group is flushed, it is written to a `.tmp` sibling file and then renamed to the final group filename, ensuring
atomic updates. A `.tmp` left behind by a crash belongs to no group and is swept the next time the section is loaded.

Renaming only makes the replacement *atomic*, not *durable*: the rename can be visible while the data blocks are not.
See [Sync Policy](#sync-policy) for what closes that gap.

### Write Ordering

A flush writes every group first and only then rewrites `meta`. `meta.version` therefore never advertises data that is
not yet on disk. The opposite order would let a surviving `meta` name a modulus the group files do not implement, so
records would hash into groups that never received them - silent data loss on read rather than a load error.

### Corruption Policy

A damaged section is refused, not patched over:

- A group whose checksum or record count does not match its trailer fails the load with an error naming the file.
- A group with no trailer at all fails too, once `meta` records that the section is checksummed.
- A truncated, unparsable or mis-checksummed `meta` fails the load.
- Nothing is published into the in-memory table until the group it came from has been verified, so a rejected group
  cannot leave a half-applied table behind.

Sections written before checksums existed have no trailer and no `checksums` flag in `meta`; they are read leniently and
gain their trailers on the first full rewrite, so an upgrade never declares an intact database corrupt.

### Automatic Migration

SmartRustyPick automatically migrates data from the old flat file format. If a table contains a `data` file, it is read,
converted to the `data.hf/` layout during the first flush, and the old `data` file is deleted. No manual export/import
is required.

## Write Buffering and Durability

Writes and deletes are buffered in memory and flushed in batches to improve throughput and reduce disk I/O.

### Flush Policy

A flush is triggered by any of the following events:

1. `flush_max_pending` (default 256) writes to a file have accumulated.
2. `flush_interval_ms` (default 250) has elapsed since that file was last flushed.
3. A client connection is closed.
4. The server's background ticker fires.
5. Graceful server shutdown (SIGTERM or Ctrl-C).

The first two are counted **per file**: a burst of writes to one file flushes that file, and takes no other file's lock
on the way. The last three flush everything that is buffered, which is what bounds how long any change can sit in
memory. A write to a file marked durable also flushes the whole database, because a promise about the disk is not worth
much if it leaves the rest of the database behind in memory.

### Durability Trade-off

By default, acknowledged writes may remain in memory for up to `flush_interval_ms` (250 ms). In the event of an abrupt
process kill (SIGKILL) or power loss, these buffered writes may be lost.

To restore flush-on-every-write behavior, set `durable_writes = true` in `config.toml`. Note that this will
significantly reduce write throughput.

### Per-file Durability

Durability can also be chosen per file, so a mission critical file is never at risk while the rest of the database keeps
the throughput of buffered writes.

The flag lives in the account's `DIR` file, as attribute 2 (`DURABLE`) of the file's entry: `Y` means every write to
that file is flushed before it is acknowledged, an empty value means the file follows the global buffering policy. The
flag is metadata, so setting it is itself flushed at once, and it is preserved when the `DIR` listing is rebuilt.

Set it when creating the file:

- CLI: `CREATE.FILE LEDGER DURABLE`
- Remote protocol: `CREATE.FILE` with `"durable": true` (an account without a `DIR` file gets one, since that is where
  the flag is stored)

Or change it afterwards, which is the usual case — a file is rarely known to be mission critical before it has any data
in it:

- CLI: `SET.FILE LEDGER DURABLE`, and `SET.FILE LEDGER BUFFERED` to go back
- Remote protocol: `SET.FILE` with `"durable": true` or `false` (admin only, like `CREATE.FILE`)
- Web dashboard: the Accounts tab, beside the file's statistics

Promoting a file is safe for the data already in it: the flag is written and the file is flushed as part of the same
change, so anything it had buffered reaches the disk under the durability being turned on rather than after it.
Demoting only relaxes what a later write has to do, and needs no such care. Either way the file keeps its records — that
is the point of setting the flag rather than recreating the file.

`DIR` itself cannot be marked durable: it holds the flags rather than carrying one, and its own writes are flushed as
soon as they are made.

Reading the flag back:

- CLI: `LIST.FILES` shows it per file
- Remote protocol: `LIST.FILES` pairs each name with `{"durable": …}` in `results`, and `FILE.STATS` reports it for one
  file
- Web dashboard: durable files are tagged in the file list

The global `durable_writes = true` still wins: it makes every file durable regardless of its `DIR` entry, and the
listings say so file by file rather than reporting `DIR` entries that no longer describe what a write does. Reading the
flag costs nothing on the write path — it is cached per file after the first lookup.

### Sync Policy

`fsync` decides how much of a flush is forced all the way to the disk:

| Value             | Group files                  | Directory and `meta` | Survives power loss                   |
|-------------------|------------------------------|----------------------|---------------------------------------|
| `never` (default) | page cache                   | page cache           | No                                    |
| `meta`            | page cache                   | `fsync`ed            | The namespace and `meta`, not records |
| `always`          | `sync_all` before the rename | `fsync`ed            | Yes                                   |

A file marked `DURABLE` (and a database running with `durable_writes = true`) uses `always`, because "flushed before the
write is acknowledged" has to mean on disk and not merely in the page cache. Setting `fsync` explicitly overrides that
too, for an operator who knowingly trades the guarantee for throughput.

## Concurrency and Lock Ordering

Every loaded file carries its own read/write lock, and the database's own state - the account registry, each account's
file listing, the client authorizations, the flush accounting, the cached durability flags - sits behind locks of its
own. Ordinary record work therefore needs nothing but a shared borrow of the database:

- `READ`, `WRITE`, `DELETE` and `QUERY` take the shared database lock and then lock only the one file they name. Two
  connections writing to two different files never wait for each other.
- A flush locks each dirty file in turn, so writing out one large file excludes work on that file and nothing else.
- Creating and dropping files and accounts, changing authorizations and the stateful select lists still take the
  database exclusively. None of it is on the hot path.

Locks are acquired in this order, and never the other way round:

1. the outer lock the server wraps the database in;
2. the account registry;
3. the per-account file listings;
4. the map of loaded files;
5. the eviction order;
6. a single file;
7. the durability, client and flush-accounting caches.

A thread holds **at most one file lock at a time**. Nothing in the engine needs two, and a command that ever does must
take them in `(account, file)` order. In particular, a full flush must not be started while a file is locked: it locks
each dirty file in turn, and would deadlock on the one already held. The same rule is why a report renders from the file
its caller has already locked rather than looking the dictionary up again per column.

### Cache Eviction

Once more than `max_loaded_tables` files are in memory, the coldest are written out and dropped. A file another
connection is still working on is skipped rather than evicted - dropping it would let a third connection load a second
copy from disk, and the two would overwrite each other - and a victim is flushed with the map still locked, so nothing
can reload it between it leaving the cache and its changes reaching the disk.

Two connections may load the same cold file at the same time. The second to finish finds the first one's entry already
in the map and discards its own copy. That costs a duplicate read of a file nobody had written to yet, and is what keeps
the map's lock off the disk.

## Configuration

The following optional keys in `config.toml` control the storage engine:

- `records_per_group` (default 16): Target number of records per group. Lower values result in smaller group files and
  faster rewrites but more files.
- `max_loaded_tables` (default 64): How many files may be held in memory at once. A file is only as large in memory as
  the records that have been read into it, and eviction is what makes two connections working on different files
  interfere with each other, so this is worth raising rather than lowering on a database with many active files.
- `durable_writes` (default false): If true, every write is flushed to disk before being acknowledged.
- `fsync` (default `never`): How much of a flush is forced to the disk — `never`, `meta` or `always`. Durable files use
  `always` unless this is set explicitly.
- `flush_interval_ms` (default 250): Maximum time a change stays in memory before being flushed.
- `flush_max_pending` (default 256): Maximum number of pending writes before a flush is triggered.

## Performance Measurements

The following results were measured with 10,000 records over the TLS protocol on a standard workstation. These are
measurements, not guarantees.

| Metric                                     | Old (Flat File) | New (Hashfile) |
|--------------------------------------------|-----------------|----------------|
| p50 Write Latency (0 - 2,500 records)      | 1.4 ms          | 0.08 ms        |
| p50 Write Latency (2,500 - 10,000 records) | 7.1 ms          | 0.08 ms        |
| Bulk Load (10,000 records)                 | ~56 s           | 1.1 s          |
| Concurrent Writers (8 clients)             | 933 ops/s       | 12,267 ops/s   |
| p99 Latency (Contention)                   | 100 ms          | ~3 ms          |
| Write Amplification (10,000 records)       | 100%            | ~0.17%         |

Measured directly on the engine (`cargo bench -p smart-rusty-pick-core --bench storage -- incremental_write`), one
record updated and flushed: 63 µs on a 1,000-record table and 61 µs on a 10,000-record one — the cost of a write does
not depend on the size of the table.

### The Cost of Syncing

Same benchmark, one record updated and flushed, with `SRP_BENCH_FSYNC` selecting the policy. On a tmpfs (`/tmp`) an
`fsync` is close to free, so these were taken on an ext4 volume on an NVMe SSD, which is what the guarantee actually
costs:

| `fsync`  | 1,000 records | 10,000 records |
|----------|---------------|----------------|
| `never`  | 0.15 ms       | 0.16 ms        |
| `meta`   | 15.5 ms       | 15.2 ms        |
| `always` | 23.6 ms       | 20.6 ms        |

Two things to read out of this. First, durability costs about two orders of magnitude per flush — which is why `never`
remains the default for the buffered path, where a flush covers a whole batch rather than a single write, and why
syncing is reserved for files that asked for it. Second, the cost still does not depend on the size of the table: what
is paid for is the sync itself, not the amount of data rewritten.

### Performance Constraints

The performance suite enforces the following rules:

- **Write cost stays flat**: The growth ratio of write cost as the file grows must be <= 2.0x.
- **Small write amplification**: The largest group file must be <= 5% of the total table size.
