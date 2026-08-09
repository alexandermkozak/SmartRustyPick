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
- `data.hf/g<8 hex digits>`: Group files containing the actual records. Each file corresponds to a hash group. Empty
  groups do not have a corresponding file.

### Frame Encoding

Both group files and the dictionary file (`dict`) use a simple frame encoding:
`[key_len u64 LE][key][data_len u64 LE][record bytes]`

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
atomic updates.

### Automatic Migration

SmartRustyPick automatically migrates data from the old flat file format. If a table contains a `data` file, it is read,
converted to the `data.hf/` layout during the first flush, and the old `data` file is deleted. No manual export/import
is required.

## Write Buffering and Durability

Writes and deletes are buffered in memory and flushed in batches to improve throughput and reduce disk I/O.

### Flush Policy

A flush is triggered by any of the following events:

1. `flush_max_pending` (default 256) writes have accumulated.
2. `flush_interval_ms` (default 250) has elapsed since the last flush.
3. A client connection is closed.
4. The server's background ticker fires.
5. Graceful server shutdown (SIGTERM or Ctrl-C).

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

The global `durable_writes = true` still wins: it makes every file durable regardless of its `DIR` entry. Reading the
flag costs nothing on the write path — it is cached per file after the first lookup.

## Configuration

The following optional keys in `config.toml` control the storage engine:

- `records_per_group` (default 16): Target number of records per group. Lower values result in smaller group files and
  faster rewrites but more files.
- `durable_writes` (default false): If true, every write is flushed to disk before being acknowledged.
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

### Performance Constraints

The performance suite enforces the following rules:

- **Write cost stays flat**: The growth ratio of write cost as the file grows must be <= 2.0x.
- **Small write amplification**: The largest group file must be <= 5% of the total table size.
