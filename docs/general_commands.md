### General Use Commands

These commands are used for day-to-day data operations within a specific account.

#### LOGTO

Switch the current context to a different account.

- **Usage**: `LOGTO <account name>`
- **Example**: `LOGTO SALES`
- **Note**: When switching to an account that lacks a `DIR` file, you will be prompted to create and populate it. An
  account created by `CREATE.ACCOUNT` comes with one, so the prompt is only for an account that predates that or has had
  its listing removed.

#### LIST.FILES

List all files in the current account, with the durability of each: `Durable` is `yes` when every write to that file is
flushed before it is acknowledged, which `SET.FILE` changes. This command reads from the `DIR` file.

- **Usage**: `LIST.FILES`
- **Example**: `LIST.FILES`

#### SET

Store a record in the database.

- **Usage**: `SET [DICT] <table> <key> <data>`
- **Example**: `SET USERS 1 Ted^Smith]123-4567`

#### GET

Retrieve a record by its key or via an active SELECT list.

- **Usage**: `GET [DICT] <table> [<key>]`
- **Example**: `GET USERS 1`

#### DELETE

Remove a record from the database.

- **Usage**: `DELETE [DICT] <table> [<key>]`
- **Example**: `DELETE USERS 1`

#### LIST

List tables, keys, or records with formatted fields.

- **Usage**:
  `LIST [DICT] [<table> [<fields>...] [WITH <field> <op> <value> ...] [BY|BY.DSND <field> ...] [BY.EXP <field> [<op> <value>] ...]]`
- **Example**: `LIST USERS First.Name Last.Name`
- **Example**: `LIST PRODUCTS BY PRICE`
- **Example**: `LIST PRODUCTS BY.DSND PRICE`
- **Example**: `LIST USERS WITH Last.Name = "Smith" First.Name Last.Name`
- **Selection**: `LIST` takes the same `WITH` clause `SELECT` does. Column names may sit on either side of it. Without
  one, `LIST` consumes the active select list if there is one for the same file, as it always has.
- **Sorting**: See [Sorting](#sorting) below.
- **Multivalue**: See [Exploding multivalues](#exploding-multivalues) below.

#### SELECT

Create or refine an active select list based on field criteria.

- **Usage**:
  `SELECT [DICT] <table> [WITH <field> <op> <value> [AND/OR <field> <op> <value> ...]] [BY|BY.DSND <field> ...]`
- **Operators**: `=`, `#` (not equal), `<`, `>`, `<=`, `>=`, `[` (ends with), `]` (starts with), `[]` (contains)
- **Logical Operators**: `AND`, `OR`
- **Example**: `SELECT USERS WITH First.Name = "Ted" AND Last.Name = "Smith"`
- **Sorting**: See [Sorting](#sorting) below.
- **Multivalue**: See [Exploding multivalues](#exploding-multivalues) below.

#### Exploding multivalues

A field can hold several values, and by default `LIST` shows all of them in one cell, separated by `]`. `BY.EXP` gives
each value its own row instead, so a multivalued field reads like a list rather than like one long string.

- `BY.EXP <field>` - one row per value of that field
- `BY.EXP <field> <op> <value>` - the same, filtered: only the values that satisfy the criterion get a row

The second form is shorthand. `BY.EXP ACCOUNTS = "TEST"` and `BY.EXP ACCOUNTS WITH ACCOUNTS = "TEST"` ask the same
question. The operator set is closed, so a bare column name after the field is still read as a column:
`LIST USERS BY.EXP ROLES NAME` explodes `ROLES` and shows the `NAME` column.

Given a `$CLIENTS` record `WEB` whose `ACCOUNTS` field holds `TEST]PAYROLL]TESTLAB`:

```
LIST $CLIENTS BY.EXP ACCOUNTS = "[TEST]" ACCOUNTS

ID         ACCOUNTS
---------- --------------------
WEB        TEST
WEB        TESTLAB
```

The select list remembers the positions, so the two-step form works too - the `LIST` that follows shows the same rows:

```
SELECT $CLIENTS BY.EXP ACCOUNTS = "TEST"
LIST $CLIENTS ACCOUNTS
```

`SAVE-LIST` and `GET-LIST` carry the positions with the list, so a saved exploded list restores as one.

The rules in detail:

- Only the named field explodes. Every other column repeats the record's whole field down the rows.
- Which records appear is still the query's decision. A record kept by a condition on some other field appears once,
  unexploded. A record whose exploded field is empty does too.
- With a criterion, the unit is the deepest thing that matched: a value, or a sub-value when the field has them. Two
  conditions on the same field union their positions, so `WITH ROLES = "DEV" OR ROLES = "ADMIN"` gives a row for each.
  A field in an association group is the exception - see below.
- `BY.EXP` does not sort. Rows come out in record-key order, and within a record in value order. Add `BY <field>` to
  order them - and when that field is the exploded one, it sorts on each row's own value rather than on the whole
  joined field.
- Only one field may be given, unless the fields are members of one association group - see below.

Commands that act on records rather than on values - `GET`, `DELETE`, `CT` - take each record from an exploded list
once, not once per matching value.

#### Exploding an association group

Where the dictionary records that several multivalued fields belong together - PICK's correlated multivalued
attributes, described under [Association groups](data_structures.md#association-groups) - `BY.EXP` explodes the whole
group in lockstep rather than one field of it. Row *n* is value *n* of every member, so the values that belong together
appear on one line.

Given a `$CLIENTS` record `WEB` whose `ACCOUNTS` holds `TEST]PAYROLL]LAB`, whose `ACCT.CODES` holds `T-1]P-7` and whose
`ACCT.NOTES` - a second-tier member - holds `one]two\three`:

```
LIST $CLIENTS BY.EXP ACCOUNTS ACCOUNTS ACCT.CODES ACCT.NOTES

ID         ACCOUNTS     CODE     NOTE
---------- ------------ -------- ----------
WEB        TEST         T-1      one
WEB        PAYROLL      P-7      two
WEB        PAYROLL      P-7      three
WEB        LAB
```

The rules the group adds:

- **Naming any member names the group.** `BY.EXP ACCOUNTS` and `BY.EXP ACCT.CODES` ask the same question, so a report
  need not know which field is the controller.
- **Several members may be named at once.** `BY.EXP ACCOUNTS BY.EXP ACCT.CODES` is the same clause again. Two fields
  that are *not* associated are still refused: without an association there is no defined pairing between their values,
  which is the whole reason the restriction exists.
- **Ragged groups keep every value.** Three accounts beside two codes give three rows, the third showing an empty code.
  A value is never dropped because a sibling ran out of them.
- **The second tier nests inside the first.** A value whose second-tier members have sub-values becomes one row per
  sub-value, and the value-tier columns repeat down those rows - `PAYROLL` and `P-7` above. A value with nothing below
  it stays one row.
- **A criterion on any member positions the whole group.** `BY.EXP ACCOUNTS WITH ACCT.CODES = "P-7"` gives the rows
  where the *code* matched, carrying each row's account and note with it. Criteria on two members union their positions,
  the same rule two criteria on one field already follow. A member no criterion names adds no rows of its own; it fills
  in its column on the rows the criteria chose.
- **`BY` on any member sorts on that row's own value**, not on the whole joined field, exactly as it does for a lone
  exploded field.
- **A member's tier decides how deep a criterion reaches.** A lone `BY.EXP` field gives a row for the deepest thing
  that matched, down to a sub-value. Inside a group a `V` member gives a row for the whole *value* that matched, even
  when a sub-value of it is what satisfied the criterion, because that is the tier its siblings are paired against.
  Declaring an association on a sub-valued field therefore changes what `BY.EXP` on it returns - deliberately: it is
  the price of the values lining up. A field that should still explode by sub-value inside the group is an `S` member.

Everything else is as it is for a lone field: which records appear is the query's decision, a record the group cannot
expand stays as one unexploded row, and `GET`, `DELETE` and `CT` still take each record once.

Columns outside the group are untouched: they repeat the record's whole field down the rows, as they do for a lone
`BY.EXP` field.

#### Sorting

`LIST` and `SELECT` accept any number of sort operators. Sorts are applied from left to right, so the first operator is
the primary sort key, the second breaks ties, and so on. Records that compare equal on every key fall back to record ID
order. Sort operators and column names are order-agnostic and may be freely interleaved.

- `BY <field>` - ascending sort
- `BY.DSND <field>` - descending sort

The special field name `ID` sorts on the record key. Values that are numeric on both sides are compared numerically,
otherwise they are compared as text.

- **Example**: `LIST PRODUCTS BY PRICE` - list products by ascending price
- **Example**: `LIST PRODUCTS BY.DSND PRICE` - list products by descending price
- **Example**: `SELECT PRODUCTS WITH DESC = "[new]" BY PRICE BY.DSND CREATE.DATE` - select products whose description
  contains "new", ordered by ascending price, then by descending create date
- **Example**: `LIST PRODUCTS BY.DSND DESC DESC PRICE` and `LIST PRODUCTS DESC PRICE BY.DSND DESC` are equivalent - both
  show the `DESC` and `PRICE` columns sorted by descending `DESC`
- **Example**: `LIST USERS BY.EXP ROLES ROLES BY ROLES` - one row per role, ordered by the role on that row

#### EDIT

Edit a record using an external editor.

- **Usage**: `EDIT [DICT] <table> <key>`
- **Example**: `EDIT USERS 1`
- **Configuration**: The editor can be configured in `config.toml` in the application root (e.g., `editor = "nano"`). If
  not set, it defaults to the `$EDITOR` environment variable, then to `nano`.

#### CT (Copy-To-Terminal)

Print record contents with numbered fields.

- **Usage**: `CT [DICT] <table> [<key>]`
- **Example**: `CT DICT USERS D1`

#### SAVE

Flush all changes from memory to the disk.

- **Usage**: `SAVE`

#### SAVE-LIST

Save the current active SELECT list.

- **Usage**: `SAVE-LIST <name>`
- **Example**: `SAVE-LIST TED_LIST`

#### GET-LIST

Retrieve a previously saved SELECT list.

- **Usage**: `GET-LIST <name>`
- **Example**: `GET-LIST TED_LIST`

#### CREATE.FILE

Create a new table (both data and dictionary sections).

The optional `DURABLE` flag marks the file as mission critical: every write to it is flushed to disk before it is
acknowledged, while the rest of the database keeps buffering writes. The flag is stored as the `DURABLE` attribute of
the file's `DIR`
entry - see [Storage Engine](storage.md).

`QUEUE` makes it a [queue file](#queue-files): records keep their arrival order and are handed out one at a time. A
queue is `DURABLE` unless `BUFFERED` says otherwise. `TIMEOUT` sets how long a claim on it is held, in seconds
(default 60), and `RETRIES` how many times a record is delivered before it moves to the dead-letter file (default 5);
naming either implies `QUEUE`.

- **Usage**: `CREATE.FILE <name> [DURABLE] [QUEUE [TIMEOUT <seconds>] [RETRIES <n>]]`
- **Example**: `CREATE.FILE ORDERS`
- **Example**: `CREATE.FILE LEDGER DURABLE`
- **Example**: `CREATE.FILE JOBS QUEUE`
- **Example**: `CREATE.FILE JOBS QUEUE TIMEOUT 300 RETRIES 3`

#### SET.FILE

Change what a file already is, keeping the records it holds: its durability, whether it is a queue, and that queue's
claim policy.

Only the flags you name change, so a `SET.FILE JOBS DURABLE` cannot quietly stop `JOBS` being a queue. Promoting a
file flushes what it still had buffered as part of the change, so the flag never gets ahead of the data it protects.
`BUFFERED` returns the file to the database's ordinary flush policy, and `NOQUEUE` returns a queue to an ordinary
file without touching a record. The one exception to "only what you name": a file becoming a queue becomes `DURABLE`
with it unless `BUFFERED` says otherwise, for the reason a queue is created durable. `DIR` carries the attributes for the other files and cannot be set itself. See
[Storage Engine](storage.md).

- **Usage**: `SET.FILE <name> [DURABLE | BUFFERED] [QUEUE | NOQUEUE] [TIMEOUT <seconds>] [RETRIES <n>]`
- **Example**: `SET.FILE LEDGER DURABLE`
- **Example**: `SET.FILE LEDGER BUFFERED`
- **Example**: `SET.FILE OUTBOX QUEUE TIMEOUT 120`
- **Note**: Admin clients can do the same over the [remote protocol](protocol.md) with `SET.FILE`, and from the
  [web dashboard](web_dashboard.md).

#### Queue files

A `SELECT` is a snapshot, so two clients that select the same pending work both get all of it. A queue file is the
other thing: an order, and a claim only one consumer can hold at a time.

`ENQUEUE` appends a record and the engine mints its key - twenty digits carrying the millisecond it arrived, so the
keys sort into arrival order. `DEQUEUE` claims the oldest unclaimed record for this session, `ACK` consumes it and
`NACK` gives it straight back. A claim that is not settled within the queue's `TIMEOUT` lapses on its own, and the
record is delivered again with its retry count one higher - so a consumer that dies mid-job costs a redelivery rather
than a lost record. A record that has used up its `RETRIES` moves to `<name>.DEAD`, which is a queue itself, keeping
its key and its failure count.

Everything else still works on the file: `LIST`, `SELECT`, `READ` and the dictionary commands treat a queue as the
ordinary file it also is. `FILE.STATS` adds the queue's depth, in-flight count, oldest unacknowledged age and
dead-letter count. See [Storage Engine](storage.md#queue-files) for what is on disk and what survives a crash.

#### ENQUEUE

Append a record to a queue. The key is minted by the engine and printed back.

- **Usage**: `ENQUEUE <queue> <data>`
- **Example**: `ENQUEUE JOBS invoice^4471`

#### DEQUEUE

Claim the oldest unclaimed record, printing its key, its delivery count and its contents. An optional number of
seconds overrides the queue's own visibility timeout for this one claim.

- **Usage**: `DEQUEUE <queue> [<visibility seconds>]`
- **Example**: `DEQUEUE JOBS`
- **Example**: `DEQUEUE JOBS 300`

#### ACK

The work succeeded: consume the claimed record, which leaves the queue for good. Only the session holding the claim
may acknowledge it, and only while the claim stands.

- **Usage**: `ACK <queue> <key>`
- **Example**: `ACK JOBS 01764950412345000001`

#### NACK

The work failed: give the record back now rather than waiting for the claim to lapse. The delivery already counted
stands, so returning a record for the last time it is allowed dead-letters it.

- **Usage**: `NACK <queue> <key>`
- **Example**: `NACK JOBS 01764950412345000001`

#### PEEK

Read a record without claiming it: the head of the queue, or the one under a named key. Peeking counts no delivery,
and shows who is holding a record when somebody is - which is how a stuck consumer is found.

- **Usage**: `PEEK <queue> [<key>]`
- **Example**: `PEEK JOBS`
- **Example**: `PEEK JOBS.DEAD 01764950412345000001`

#### DELETE.FILE

Delete a table (both data and dictionary sections).

- **Usage**: `DELETE.FILE <name>`
- **Example**: `DELETE.FILE OLD_DATA`

#### FILE.STATS

Describe one file: what it is made of, whether that is healthy, and what to do about anything that is not.

The layout half is the record and dictionary counts, the hash modulus and how the records are spread over its groups.
The health half is a verdict per measure — `ok`, `watch` or `ACT` — with the rule that produced it, covering the storage
format, the per-group checksums, the skew of the group distribution, how close the file is to the full rewrite a
modulus change is, and the worst verdict among its indexes.

The per-group record counts come from each group's trailer, so this reads no record - unless the file is a queue,
whose depth and in-flight count live in the order held in memory and so cost a load.

A [queue file](#queue-files) adds a line of its own: how many records are waiting, how many are claimed and not yet
acknowledged, how old the oldest one still in the queue is, and how many have been dead-lettered - followed by the
claim policy those numbers are under.

- **Usage**: `FILE.STATS <file>`
- **Example**: `FILE.STATS ORDERS`
- **Note**: The same measures reach the [remote protocol](protocol.md) as `FILE.STATS` and the
  [web dashboard](web_dashboard.md)'s file panel, decided in one place so all three say the same thing.

#### CREATE.INDEX

Index a dictionary field, so `WITH <field> = <value>` resolves through the index instead of reading every record. The
cost of finding a record by that field then stops growing with the size of the file — see
[Storage Engine](storage.md#secondary-indexes).

Building it reads the file once. After that the index is maintained on every write, and a query that cannot use it (a
wildcard, an inequality, a field with no index) falls back to the scan it always had.

- **Usage**: `CREATE.INDEX <file> <field> [EXCLUDE <value>...]`
- **Example**: `CREATE.INDEX ORDERS CUSTOMER`
- **Example**: `CREATE.INDEX ORDERS STATUS EXCLUDE ACTIVE ""` — index every status except the one nine records in ten
  carry, and the empty one. See [SET.INDEX.EXCLUDE](#setindexexclude).
- **Note**: The field has to be one the file's dictionary defines. `ID` cannot be indexed — it is the record key, which
  is already found without a scan.

#### LIST.INDEXES

List a file's indexes with the counts each one is judged on: how many distinct values it holds, how many record keys it
indexes in total, the size of its largest posting list, how many records an average lookup narrows the file to, how many
lookups it has served since the server started, and the verdict the database puts on all of that.

An index over many values turns a scan into a lookup of a handful of records. One over two or three values turns it into
a scan of a third of the file, and still costs a write every time the field changes.

With no file, every index in the account — so a database with forty files can be asked which of its indexes is worth
your attention, rather than requiring you to remember to look at each file in turn.

- **Usage**: `LIST.INDEXES [<file>]`
- **Example**: `LIST.INDEXES ORDERS`
- **Example**: `LIST.INDEXES` — every index in the current account, grouped by file.

#### INDEX.STATS

One index in full: its counts, the verdicts on them with the threshold behind each, and the values holding the most
record keys.

The value histogram is what turns "this index is skewed" into "`STATUS = ACTIVE` is 91% of it" — and the value it names
is the one to hand to `SET.INDEX.EXCLUDE`. Reading it costs one pass over the index section, which holds values and
record keys and never record bodies.

- **Usage**: `INDEX.STATS <file> <field> [<how_many_values>]`
- **Example**: `INDEX.STATS ORDERS STATUS`
- **Example**: `INDEX.STATS ORDERS STATUS 25` — the twenty-five commonest values rather than the default ten.
- **Note**: A stale index reports no values. Its postings do not describe the records, so listing them would be listing
  fiction; rebuild it first.

#### SET.INDEX.EXCLUDE

Replace the values one index deliberately does not hold, and rebuild it.

The remedy between leaving an index alone and dropping it. Take the shape `INDEX.STATS` usually finds: a field where
90% of records carry one value and the remaining 10% are spread over hundreds. That field is *excellent* to index — for
the 10%. Indexing the dominant value buys nothing, because the lookup hands the scan behind it most of the file and the
scan does that work anyway, and it costs the most, because it is the longest posting list and so the entry rewritten
most expensively on every write that touches it.

**A query for an excluded value answers exactly what it answered before.** The planner is told "I cannot help, scan for
it" rather than being handed an empty posting list it would read as "no records", so nothing about the results changes —
only the cost of maintaining the index. See [Excluded values](storage.md#excluded-values).

- **Usage**: `SET.INDEX.EXCLUDE <file> <field> [<value>...]`
- **Example**: `SET.INDEX.EXCLUDE ORDERS STATUS ACTIVE` — stop indexing the dominant status.
- **Example**: `SET.INDEX.EXCLUDE ORDERS NOTES ""` — index only the records that carry the field at all.
- **Example**: `SET.INDEX.EXCLUDE ORDERS STATUS` — with no values, the index goes back to holding everything.
- **Note**: The list *replaces* what was there, so an exclusion to be kept has to be named again. Quoted values are
  unwrapped, which is how the empty value is spelled; the comparison trims, so `ACTIVE` and `" ACTIVE "` are the same
  exclusion. Changing the set rebuilds the index, exactly as moving its field to another attribute does.

#### REBUILD.INDEX

Derive an index from the records again. The repair for an index reported as stale, and the way to bring one back after
its section has been damaged or removed underneath the server.

An index is normally rebuilt on its own — a stale one is detected and rebuilt when the file is loaded, and never
consulted before it has been — so this is for the case where the file is already in memory and the rebuild should happen
now.

- **Usage**: `REBUILD.INDEX <file> <field>`
- **Example**: `REBUILD.INDEX ORDERS CUSTOMER`

#### DELETE.INDEX

Drop an index and remove its section. The records are untouched; queries that were using it go back to scanning.

- **Usage**: `DELETE.INDEX <file> <field>`
- **Example**: `DELETE.INDEX ORDERS CUSTOMER`
- **Note**: Admin clients can do all of these over the [remote protocol](protocol.md), and from the
  [web dashboard](web_dashboard.md).

#### HELP

Show the help message.

- **Usage**: `HELP`

#### EXIT / QUIT

Exit the SmartRustyPick CLI.

- **Usage**: `EXIT` or `QUIT`
