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

### Reading a file's health

`FILE.STATS` answers "is this file healthy, and will it stay that way", not only "how big is it". Everything below is
derived from the section metadata and the group **trailers**, so answering it reads no record — the property the
[web dashboard](web_dashboard.md) promises about that whole view, and one a test asserts by checking the file is still
not loaded afterwards.

**Records per group.** Each group's 20-byte trailer holds `[magic][record_count][crc32c]`, so the true distribution
costs one seek per group and loads nothing. `group_records` reports the min, median, mean and max, a bucketed shape for
drawing, and counts of the empty, the overweight and the unreadable.

It is computed over the **modulus**, not over the group files. A group holding nothing has no file at all — an empty
bucket is deleted rather than written — so averaging the files that exist would report a file whose records had piled
into four groups out of thirty-two as perfectly even. That is precisely the case the measure exists to catch.

**Skew.** A hash file's failure mode is not size, it is imbalance: a group holding a large share of the records makes
every write to it rewrite that whole group, which is the one cost this format exists to avoid. `skew` is max over mean,
which is scale-free and so reads the same on a file of any size; `largest_group_share` is that extreme against the whole
file; `overweight` counts the groups above twice the mean, which says something one extreme cannot — that the hash
itself is not spreading, rather than that one group was unlucky. None of it is judged below four records per group,
because on four records over eight groups the largest group is three times the mean and that is simply what small
numbers look like.

**Headroom.** `load_factor` is `records / (modulus × records_per_group)`. `records_until_growth` says how many more
records the file takes before the modulus doubles, and `records_until_shrink` how many fewer before it halves (`null`
at the floor). A modulus change rehashes every group, so "3,000 records from a rehash" is worth seeing before it
happens rather than after.

**Bytes.** `group_bytes` is the record groups, `index_bytes` the index sections, and `disk_bytes` the whole directory —
the remainder is the dictionary and the small metadata files. There is no "wasted space" figure because there is no
waste to report: a group is rewritten whole from its live entries, so a deleted record leaves nothing behind inside one.

**Integrity.** `legacy` and `checksums: false` both mean the file is not yet protected by the current format's
guarantees, and both are reported as something to act on with the remedy attached ("converted on the next flush")
rather than as neutral rows reading "no".

Each of these carries a verdict from `db::health` — `good`, `watch` or `act` — with the threshold that produced it, and
a file's verdict absorbs its indexes'. The cheaper form of the same question, for a *listing*, reads metadata and index
`state` files only and appears on `LIST.FILES` and `LIST.ACCOUNTS`: enough to say which file is worth opening, which is
all a listing should cost.

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

A damaged *index* section is the one exception, and deliberately so: an index is derived data, so it is rebuilt from the
records rather than failing the load of a file whose records are perfectly intact. See
[Secondary Indexes](#secondary-indexes).

### Automatic Migration

SmartRustyPick automatically migrates data from the old flat file format. If a table contains a `data` file, it is read,
converted to the `data.hf/` layout during the first flush, and the old `data` file is deleted. No manual export/import
is required.

## Secondary Indexes

A keyed read is O(1): the key is hashed and one group is read. Every other retrieval used to be a full scan of a fully
resident file, so `WITH CITY = "York"` cost the same whether it matched one record or a million. A secondary index
closes that gap for the case it can be closed for cheaply — equality on a field the dictionary already describes.

### Layout

An index is a section beside the records, in exactly the format they use:

```text
db_storage/<account>/<file>/data.hf/          the records
db_storage/<account>/<file>/index.CITY.hf/    an index on the CITY dictionary field
```

The section reuses the hashfile machinery whole — the same framing, the same per-group checksums, the same `.tmp` +
rename, the same dynamic modulus — so an index inherits the crash-safety story of the data rather than inventing a
second one. The key of an entry is an indexed *value*; the record it maps to holds the keys of every record carrying
that value, one per value mark.

Beside the section's own `meta`, an index carries a `state` file, checksummed the same way:

- `field`: the dictionary field indexed.
- `attribute`: the Pick attribute number that field resolved to when the index was written.
- `data_version`: the `meta.version` of the data section the index was last written against.

Which indexes a file has is read off its directory. There is no manifest, so there is no second list that can disagree
with the sections after a crash or a manual removal.

### What is indexed

Every sub-value of the attribute, trimmed — which is exactly what a comparison looks at — plus the empty string for a
record whose attribute is absent or empty, because `WITH FIELD = ""` matches such a record. A multivalued field
therefore indexes each of its values, and a sub-valued one each sub-value.

A value longer than 512 bytes is indexed under a truncated key, since a group file refuses to read back a key longer
than its own limit. Several long values then share one entry, which costs a few extra candidates and never a missing
one.

`ID` cannot be indexed: it is the record key, already found in one hash lookup. Nor can a field whose name would not
make a directory component — the name set is letters, digits and `. _ - $ # %`.

### How a query uses one

`query_in` gains one step in front of the scan. It walks the query tree and works out which keys an index can narrow it
to:

- An equality (`=` / `EQ`) on an indexed field resolves to that value's posting list, with the field's input conversion
  applied first, so the index is looked up with the value that is actually stored.
- `AND` intersects what its sides know, and keeps one side's answer when the other has none — narrowing by half a
  condition is still narrowing.
- `OR` needs both sides, since a side it cannot resolve could match anything at all.
- Wildcards (`[value`, `value]`, `[value]`), inequalities and `ID` conditions resolve to nothing, and the scan behind
  this handles them exactly as before.

What comes back is a *superset* of the matches, never the answer. Every candidate is then put through the same
evaluation a scan would have put it through, so an indexed query returns byte-identical results to the same query
without the index — the index changes what is looked at, never what is decided. The candidate keys are walked in key
order, which is the order the scan sorted its matches into, and a caller-supplied key list keeps its own order and is
merely filtered by the candidate set.

### Maintenance and staleness

An index is held in memory beside the records and updated on the write path, in `Table::insert_record` and
`Table::remove_record` — the one moment the record being replaced is still available, which is what a withdrawal of the
old value needs. Only the values that actually moved are touched, so a write that leaves the indexed attribute alone
costs a comparison of two short lists and no index work at all.

A flush writes the records first, then each index, then that index's `state`. `state.data_version` therefore never
names a data version that is not already on disk, and a crash anywhere in between leaves an index whose recorded
version is behind the data's. That mismatch is the staleness signal. The same check catches an index whose field has
been moved to a different attribute by a dictionary edit.

Three more things mark an index stale rather than trusting it: a bulk change that names no keys (`touch_all`), a
section that cannot be read at all, and an explicit `REBUILD.INDEX`. In every case the index is rebuilt from the records
when the file is loaded, or at the start of the next flush, and **nothing consults an index in that state** — the query
falls back to the scan it always had.

An index is therefore never silently wrong. It is either consistent with the data, or detectably stale and rebuilt.

### What an index costs

Building one is a single pass over the file, which is the only cost an index has that grows with the file. After that a
flush rewrites, per index, the groups holding the values the write moved — at most two — plus a small `state` file. The
extra work is per index, not per record in the file.

The one place the file's size does show through is the size of a posting list: an index on a field with ten values holds
a tenth of the file's keys in each entry, and rewriting that entry writes them all. An index is worth most on a field
with many values, which is also the field where it saves most.

### Excluded values

An index may be told not to hold particular values.

The motivating shape is a field where 90% of records carry one value and the remaining 10% are spread over hundreds.
That field is *excellent* to index — for the 10%. Indexing the dominant value buys nothing, because the lookup hands
the scan behind it most of the file and the scan was going to do that work anyway, and it costs the most, because it is
the longest posting list and so the entry rewritten most expensively on every write that touches it. `"index everything
except the empty string"` is the other common spelling: a sparse field most records simply do not carry.

The exclusions live in the index section's `state` file, beside `field`, `attribute` and `data_version`. They are part
of what the index *is*, so:

- they survive a restart, and
- changing them marks the index for rebuild, exactly as moving its field to another attribute does. Adding an exclusion
  has to drop a posting list; removing one has to derive a list that was never kept, and that needs every record anyway.

**The planner is the whole risk of the feature.** `FileIndex::candidates` returns `Option<&BTreeSet<String>>`, and the
difference between `None` and an empty set is the contract: an empty set means "no record carries this value", `None`
means "I cannot help, scan for it". An excluded value holds no posting list precisely because it was not worth one, so
returning that empty list would answer "no records" to a query that matches most of the file. It returns `None`
instead — and the same applies to the `AND`/`OR` composition above: an excluded side is an *unknown* side, not an empty
one, which is exactly how the existing code already treats a condition no index can resolve.

That is sound because "I do not know" was already an answer the planner handled. The invariant is the one this whole
feature rests on: the index only ever narrows, and the evaluation behind it decides. A query for an excluded value is
therefore byte-identical to the same query with no index at all, and the tests assert that by running both.

There is deliberately **no automatic variant** that skips any value covering more than some share of the file. It would
make an index's contents depend on the data distribution at build time, so the same command would produce different
indexes on different days, and a value could silently cross the threshold as the file grew. The exclusions stay
explicit, and the diagnostics below suggest them.

### Knowing whether one is earning its keep

Three questions, in the order an operator meets them.

**Which index should I be looking at?** `LIST.INDEXES` with no file names every index in the account, and every file
and account listing carries a health verdict, so a badly shaped or stale index surfaces without anyone walking file by
file. The dashboard's Overview tab shows the same roll-up.

**Why is this one bad?** `INDEX.STATS <file> <field>` returns the values holding the most keys, largest first. That is
what turns "this index is skewed" into "`STATUS = ACTIVE` is 91% of it", and the value it names is the one to exclude.
Reading it costs one pass over the index section — values and record keys, never record bodies — which is why it is its
own command rather than part of the per-file listing that is read on every navigation.

**Is anything even using it?** `IndexStats.usage` counts what the read path has asked of each index **since the server
started** — never persisted, because the question is "is anything querying this now" and a count carried over from a
previous run answers it wrongly. Relaxed atomics, since nothing is decided by their exact interleaving.

- `lookups` and `candidates` — how many lookups it answered and how many record keys those handed to the filter behind
  them. A hit rate of zero is the clearest possible signal that an index is pure cost: it is maintained on every write
  to its field whether or not anything queries it.
- `matched` over `candidates` is how selective the index is *for the queries actually being run*, which is not the same
  as how selective it is over the data. It is attributed only for a query one index resolved on its own
  (`measured_lookups` counts those): once an `AND` intersects two indexes there is no honest way to say which of them a
  surviving record is owed to.
- `excluded_lookups` — lookups that fell back to a scan because the value asked for is excluded. A high count is not an
  argument against the exclusion; the scan behind it was going to do that work either way.

Every one of these is turned into a verdict — `good`, `watch` or `act` — by `db::health`, with the threshold that
produced it. The rule lives there rather than in each interface: the CLI, the remote protocol and the web dashboard
describe the same index, and three copies of "a quarter of the file is the line" is three chances to disagree.

### Managing them

- CLI: `CREATE.INDEX <file> <field> [EXCLUDE <value>...]`, `LIST.INDEXES [<file>]`, `INDEX.STATS <file> <field> [<n>]`,
  `SET.INDEX.EXCLUDE <file> <field> [<value>...]`, `REBUILD.INDEX <file> <field>`, `DELETE.INDEX <file> <field>` — see
  [General Use Commands](general_commands.md).
- Remote protocol: the same six commands; the ones that change something are admin-only, like `CREATE.FILE`. See
  [Remote Connection Protocol](protocol.md).
- Web dashboard: the Accounts tab, under the file's dictionary — create, rebuild, drop, read one index's values and
  exclude one of them from the row it is shown on. See [Web Management Dashboard](web_dashboard.md).

`FILE.STATS` reports a file's indexes alongside its layout, and `LIST.INDEXES` reports them on their own. Both carry the
same counts: `values` (distinct values held), `postings` (total value-to-key pairs) and `largest_postings` (the
biggest posting list), plus `excluded`, `usage` and `health`. `values` against the file's record count is how selective
the field is, `postings` is what maintaining the index costs per write, and `largest_postings` is the skew the average
hides — an index whose biggest value covers half the file saves nothing on that value.

### Measurements

Measured with `cargo bench -p smart-rusty-pick-core`, on a tmpfs so the numbers are about the engine rather than about
the disk. These are measurements, not guarantees.

Finding one record by a non-key value (`index/{scan,indexed}`), on a field distinct per record:

| Plan                   | 1,000 records | 10,000 records |
|------------------------|---------------|----------------|
| Full scan              | 58 µs         | 1,350 µs       |
| Through the index      | 6.7 µs        | 7.0 µs         |

Ten times the file is twenty-three times the scan, and the same lookup through the index. That is the point of the
feature: the cost of finding a record by a non-key value stops growing with the size of the file.

One record updated and flushed (`storage/indexed_write`, against `storage/incremental_write` as the control):

| Index on the file                  | 1,000 records | 10,000 records |
|------------------------------------|---------------|----------------|
| None                               | 49 µs         | 48 µs          |
| One, on a field distinct per record | 77 µs         | 62 µs          |
| One, on a field with ten values     | 63 µs         | 58 µs          |

So an index costs roughly ten to thirty microseconds a write here — a fifth to a half again on top of the flush — and
that cost does not grow with the file. The higher figure in the small-file column is the index's own modulus doubling
during the run, amortised into the average; it is a property of a section that is still growing, not of the file size.

What excluding a dominant value is worth (`storage/excluded_write`). `STATUS` is `ACTIVE` on nine records in ten and
one of five hundred rare values on the tenth; one record's status is flipped and flushed, which is the only kind of
write an index pays for at all — re-storing a record with the value it already had compares two short lists and stops.

| Index on `STATUS`             | 1,000 records | 10,000 records |
|-------------------------------|---------------|----------------|
| Every value                   | 1,856 µs      | 4,238 µs       |
| Excluding `ACTIVE`            | 218 µs        | 2,244 µs       |

The entry for `ACTIVE` is one record holding nine tenths of the file's keys, and it is rewritten whenever a record
enters or leaves it. Excluding it also shrinks the section itself: on 10,000 records the index goes from about 84 KB to
12 KB, and on 50,000 from 404 KB to 44 KB. Both are the same fact from two directions — the dominant value's entry is
most of the index, and it is the part that was buying nothing.

The two rows converge in the right-hand column because the flush of the *records* starts to dominate; the gap is
widest exactly where the index was the problem.

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

A `DIR` entry carries five attributes in all — the entry type, this flag, and the three that describe a
[queue file](#queue-files). A rebuild of the listing reconstructs it from the filesystem, which knows none of them, so
every attribute of the old entry is carried across in one piece.

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

## Queue Files

A hash file has no order to walk: a record's key decides its group, so "the oldest record" is not a question the layout
can answer. A queue file adds the two things that are missing from that for work several consumers divide between them
— an order, and a claim only one of them can hold.

A file is made a queue by its `DIR` entry, exactly as it is made durable: attribute 3 (`QUEUE`) is `Y`, attribute 4
(`QUEUE.TIMEOUT`) is how many seconds a claim is held, and attribute 5 (`QUEUE.RETRIES`) is how many times a record is
delivered before it is dead-lettered. Both numbers are written out even when they are the defaults (60 and 5), because
the entry is what an administrator reads to find out what the queue will do, and "blank, which means sixty" is a worse
answer than "60". The policy is per file rather than per server: a queue of thirty-second jobs and a queue of hour-long
ones need different answers.

Nothing else about the file changes. Its records are ordinary records in the ordinary hashed layout, its dictionary
describes them the usual way, and `READ`, `LIST`, `SELECT` and the index commands all work on it unchanged.

### Sequence keys

The engine mints the key of every enqueued record, as twenty decimal digits:

```text
 01764950412345 000001
 ^ milliseconds ^ counter within that millisecond
```

`milliseconds-since-the-epoch * 1000000 + counter`, zero padded, so the keys sort into arrival order both as text and as
numbers. Claiming the oldest record is then the first entry of an ordered set, not a scan.

The clock is deliberately *in* the key. That is what lets the oldest unacknowledged age be read off the smallest live
key rather than from a timestamp stored per record — and that is the difference between persistent queue state the size
of the in-flight set and state the size of the queue's depth. Two consequences follow, and are worth stating plainly:

- The sequence is forced upwards, so a clock that steps backwards still yields keys in arrival order — but the time
  those keys carry is behind the wall clock until it catches up, and a reported age is off by that much in the meantime.
- A millisecond holds a million keys. Enqueueing faster than that borrows from the next millisecond rather than
  colliding.

A record put into a queue file by hand — `WRITE` under a key of your own — is not refused. It joins the order in key
order like any other, and a record deleted out from under a claim is forgotten rather than handed out with nothing
behind it. A queue that cannot be repaired by hand would be worse than one that can.

### What is on disk, and what is not

Beside the records, a queue file carries one small `queue` file:

```text
<account>/JOBS/data.hf/        the records
<account>/JOBS/dict            the dictionary
<account>/JOBS/queue           the next sequence number and the delivery counts
```

It holds two things: the next sequence number, and the delivery count of each record that has been delivered at least
once. Written checksum-first through a temporary file and a rename, like an index's `state`, and written *after* the
records for the same reason — it names records, so it must never get ahead of them. A count naming a record the data
section has not got is dropped on the next load; a record with no count merely starts its retries again.

**Claims are not persisted at all.** A claim belongs to a connection, and a server that has restarted has none, so
every claim is released on load and its record becomes available again with its delivery count intact. This is why the
`queue` file stays small in a queue that is being drained normally: it is the size of the trouble, not the size of the
backlog.

A file that stops being a queue loses the `queue` file with it, so nothing is left on disk describing an order the file
no longer has. Its records are untouched; promoting it again rebuilds the order from them and starts the delivery counts
over.

A `queue` file that does not check out — a bad checksum, a truncation, anything unreadable — is treated as absent rather
than as an error. The records are the queue; this only says where the sequence had got to and how often each record had
been delivered, so losing it costs a few redeliveries and a sequence recovered from the largest key already present.
Refusing to open the queue would cost the queue.

What that buys, against a hard kill of the server:

- A record that was acknowledged does not come back. `ACK` removes it from the records, and the records are flushed
  before the acknowledgement returns — a queue is durable by default precisely so this holds.
- A record that was claimed and not acknowledged does not disappear. It never left the records; only the claim did, and
  the claim was in memory.
- A record redelivered after a restart carries the deliveries it had already used, so a poison record still reaches the
  dead-letter file rather than looping forever.

### Dead letters

A record delivered `QUEUE.RETRIES` times without being acknowledged moves to `<name>.DEAD`, which is created on the
first one and is a queue file itself. The record keeps its sequence key and its delivery count, so what failed and how
often is readable with `PEEK`, and a fixed consumer drains the file with the same commands it drains the live queue
with.

A dead-letter file is the end of the line: its own records are never dead-lettered again. A `NACK` or a lapsed claim
there returns the record to the file it is already on, with its delivery count still rising, rather than creating a
`<name>.DEAD.DEAD` — draining a dead-letter file is how an operator finds out what went wrong, and burying a record one
level deeper each time they look at it would defeat that. The file's `DIR` entry still carries the retry limit its
records were given before they arrived, and that is what `FILE.STATS` reports.

The move crosses two files, and a thread holds one file's lock at a time (see [Concurrency and Lock
Ordering](#concurrency-and-lock-ordering)). So the records are taken out of the queue under its own lock, carried in
hand, and written to the dead-letter file — which is flushed *before* the queue is. A crash in between therefore
duplicates a dead letter rather than losing a record, and because the copy keeps its original sequence key, the retry
that follows overwrites it rather than adding a second one.

### What the statistics cost

`FILE.STATS` on a queue reads its depth, in-flight count, oldest unacknowledged age and
dead-letter count, and it does so under that file's own lock - the one every consumer of the queue is waiting on. So
none of it may grow with the backlog.

The depth and the in-flight count are counters. The oldest age is read from the *front* of the available set, which is
already ordered, rather than by taking a minimum over every key: a sequence key is twenty ASCII digits, so among the
keys that are sequence keys, sorting by text is sorting by number and the first one found is the smallest. A key written
by hand carries no arrival time and is stepped over rather than ending the search. The sweep of lapsed claims is over
the in-flight set, which is the number of consumers rather than the depth.

What is left is the cost `FILE.STATS` has on any file: one seek per group trailer, which grows with the group count and
not with this. `test/performance/test_concurrency.py` measures a queue's statistics against an ordinary file holding the
same records for exactly that reason - the shared cost is on both sides, and what the ratio reports is the queue's own.

### Why a claim is atomic

Everything that decides who gets a record happens inside one write lock on that file: the lapsed claims are swept back
into the order, the oldest available key is taken out of it, and the claim is recorded — without the lock being
released in between. A second consumer arriving at any point either has not got the lock yet or is looking at a queue
the record has already left. Two consumers therefore cannot come away with the same key.

`PEEK` is the exception, and deliberately so: it claims nothing and counts no delivery, so it takes a *shared* lock
unless a claim has actually lapsed - which is a cheap question, asked over the in-flight set rather than the backlog.
Polling a queue to see what is on it therefore does not hold up the consumers draining it. A peek that does find a
lapsed claim puts it back before answering, and that part takes the file exclusively.

That lock is the *file's*, not the database's, so consumers of one queue contend only with each other. The queue
commands take the same shared path as `READ` and `WRITE` for exactly this reason: a queue is the most contended file in
any system that has one, and taking the database exclusively to claim from it would serialise every consumer against
every other connection in the server.

Delivery is therefore **at least once, not exactly once**. A consumer that finishes its work and dies before
acknowledging leaves a claim that lapses and a record that is handed out again; the delivery count on every claim is
what a consumer should be idempotent against.

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

That last rule is checked, not merely written down. A debug build counts the file locks each thread holds and panics
where a flush starts if any is outstanding. Breaking it is otherwise a hang, which is the one failure that arrives with
nothing to read — no assertion, no stack, just a run that never finishes. The counting compiles out of a release build.

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
