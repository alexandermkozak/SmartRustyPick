# AI Agents in SmartRustyPick

This project is a proof of concept and an exploration of the effectiveness of AI agents in software development. As
noted in the [README.md](README.md), the developer has minimal experience with Rust and relies on AI agents for
implementation, refactoring, and troubleshooting.

## Agent Philosophy

The development of SmartRustyPick follows a "vibe-coding" approach where:

- The human developer provides high-level intent, architectural goals, and oversight.
- The AI agent performs the heavy lifting: writing boilerplate, implementing logic, fixing bugs, and optimizing
  performance.
- Rust's strong type system and built-in testing provide the safety net needed for an agent-driven workflow.

## The Agent: Junie

The primary agent used in this project is **Junie**, an autonomous programmer developed by JetBrains.

- **Model:** Gemini 3 Flash.
- **Role:** Full-cycle developer (Feature implementation, Bug fixing, Testing, Documentation).

## Key Contributions and Milestones

AI agents have been responsible for several critical improvements and fixes in this project:

### 1. Networking and Security

- **TLS Implementation:** Set up the TCP/SSL server with certificate-based authentication.
- **Connection Optimization:** Transitioned integration and performance tests from per-request handshakes to persistent
  TLS connections, reducing test time from ~3s to ~0.5s.
- **Graceful Shutdowns:** Fixed "Read error" and "peer closed connection" warnings by implementing proper TLS
  `close_notify` sequences in test clients.

### 2. Management Interfaces

- **Web Dashboard:** Built the browser-based management interface that starts with the database server, covering
  connection authorization, certificate issuing and download, live connection and usage monitoring, navigation of
  accounts and their files, per-file durability, the queue and autokey flags with the numbers behind each - a queue's
  depth and in-flight count, an autokey file's next key - creating and dropping accounts and files, creating the
  populated demo account, and maintaining a file's dictionary. It connects to the database as an ordinary remote client, with a certificate reissued and
  re-authorized on every boot, so it can do nothing the documented protocol does not already allow.
- **Protocol Extensions:** Added the management commands the dashboard needed - `LIST.CONNS`, `LIST.ACCOUNTS`,
  `LIST.FILES`, `FILE.STATS`, `SERVER.STATS`, `SET.FILE`, `GENERATE.CERT`, `LIST.DICT`, `SET.DICT` and
  `CREATE.TEST.ACCOUNT` - to the remote
  protocol rather than giving the interface a private path into the engine. The rule the additions follow is that a
  command earns its place by filling a gap rather than by matching a page's shape: dictionary entries are created and
  listed through new commands because `WRITE`/`READ` with `is_dict` label them with the *data* file's field names, but
  they are deleted with the `DELETE` that already did the job correctly.

### 3. Testing and Automation

- **Integration Tests:** Developed a Python-based integration suite covering the full CRUD protocol (WRITE, READ, QUERY,
  SELECT LIST, READNEXT, DELETE).
- **Performance Testing:** Created load tests to verify database performance under concurrent-like sequential pressure.
- **Git Hooks:** Automated quality control by setting up a `pre-push` hook that runs `cargo test` to prevent regression.

### 4. Database Core

- **MultiValue Logic:** Implementation of hierarchical data structures (FM, VM, SVM), and the `BY.EXP` clause that
  gives each value of a multivalued field its own `LIST` row, carrying the matched position through select lists,
  `SAVE-LIST`/`GET-LIST` and the remote protocol. Fields that belong together explode together - see
  [Association groups](#9-association-groups-correlated-multivalues-for-free) below.
- **Dictionary Support:** Logic for field formatting and conversions (Dates, Numbers).
- **Query Engine:** Implementation of `SELECT` and `QUERY` commands for data retrieval.
- **Queue Files:** An ordering primitive beside the hashed one. A file created `QUEUE` mints a sequence key per
  enqueued record - twenty digits carrying the millisecond it arrived, so arrival order is recoverable without a sort
  and the oldest unacknowledged age is readable off the smallest live key. `ENQUEUE`, `DEQUEUE`, `ACK`, `NACK` and
  `PEEK` divide work between consumers: a claim is taken inside the file's own write lock, so two consumers cannot come
  away with the same record, and one that lapses is redelivered rather than lost with the process that took it. A
  record that uses up its deliveries moves to `<name>.DEAD`, itself a queue, with its failure count intact.
- **Test Infrastructure:** Added `CREATE.TEST.ACCOUNT` command in the `SYSTEM` account to quickly spin up pre-populated
  accounts for feature verification and regression testing. It is reachable over the remote protocol and from the web
  dashboard as well, so the fixture is one command away from any interface. This command must be maintained and updated as new data
  structures or features are added to the system - the `USERS` file now carries a multivalued `ROLES` field, one of
  whose values is sub-valued, `PRODUCTS` carries an association group whose members are deliberately ragged, and a
  `JOBS` queue file arrives with three records already enqueued, so the fixture reaches every level of the hierarchy,
  both tiers of an association, and the ordering primitive as well as the record ones. An `EVENTS` autokey file arrives
  with two records appended with no key at all, so the keys in it are real minted ones and a range read over them is in
  write order. An `ATTACHMENTS` directory file
  arrives with two records, one of which holds all three mark bytes and an embedded NUL - content an ordinary record
  cannot carry, so anything that round-trips it has demonstrated what the third file type is for.
- **Directory Files:** A third file type, and the answer to "how does a record hold a scanned invoice". The marks
  `FM`, `VM` and `SVM` *are* a record's structure, so a PNG or a `.wasm` module cannot be one - the first `0xFE` in it
  is indistinguishable from the separator it is. A file created `DIRECTORY` is a pointer to a real directory on the
  host, PICK's own answer: the key is a file name, the record is that file's bytes, and nothing frames them so nothing
  in them can be read as structure. `STORE` and `EXTRACT` stream a host file in and out, so a gigabyte is bounded by
  `max_directory_record_bytes` and not by the 1 MiB request line.
- **The property that decided the shape:** a directory file has **no table**, and therefore no table lock. Reading a
  forty megabyte record must not block every writer to that file for the length of the read, and it cannot when there
  is nothing to lock - `the_hot_paths_lock_a_file_a_fixed_number_of_times` gives every directory-file command a budget
  of **zero**, beside the two an ordinary `READ` takes. It also means nothing is cached: `hashfile::load` reads every
  group of a section into the table's map on the first touch, so one read of one photograph would otherwise make every
  photograph resident.
- **Refusals over empty answers.** A directory file has no fields, so a dictionary, an index and a `WITH` clause each
  have nothing to work on, and every one of them is refused with a message saying what to use instead. A query that
  quietly matches nothing is a wrong answer sent with `status: "OK"` - the same failure `INVALID_QUERY` was introduced
  for. A transaction naming one is refused for a different reason: its `rename` commits a record on its own and cannot
  be held back until the rest of the set is ready, so the refusal is `TRANSACTION_SCOPE` and nothing is applied.
- **A key is checked, never repaired.** No separators, no leading dot, no control bytes, at most 255 bytes. A name
  quietly sanitised is a write that succeeds and reads back under a key nobody asked for, and with `..` it is that plus
  somebody else's directory. Same rule as the error codes: the refusal is the interface.
- **The type is fixed at creation, and the `DIR` entry says which.** Attribute 1 is `D` rather than `F`, and it decides
  how the rest of the entry reads - a directory file has no buffered writes to make durable and no order to claim from,
  so attributes 2 to 5 read as empty on one whatever a hand-edited entry says. `SET.FILE` will not convert in either
  direction: an ordinary file's records are inside a hashed section and a directory file's are host files, so flipping
  the flag alone would leave a file whose entry says one thing and whose records are somewhere else.
- **Raw byte transfers:** `PUT.BYTES` and `GET.BYTES`, the one place the protocol is not a line of JSON. A directory
  file holds 64 MiB, and the ordinary path could carry 768 KiB of it: base64 inflates by 4/3 inside a 1 MiB request
  line. So a length is announced on the line and the record's bytes follow it raw. The body is *announced* rather than
  delimited because a record contains newlines like any other byte - which is also what lets an oversized transfer be
  refused before a byte of it is read.
- **The bug this was written to avoid.** By the time the request line has been parsed, the first bytes of the body are
  already inside the connection's `BufReader` - they arrived in the same segment. Reading the body from the underlying
  stream would drop exactly those, on exactly the transfers whose body straddles the buffer, and the record would be
  stored looking almost right. Every read goes through the caller's reader, and the test asserts its own precondition
  (the buffer is non-empty when the body is asked for) so it cannot quietly stop testing anything.
- **A body is always accounted for, or the connection closes.** The rule the line reader already had for an over-long
  request - *"unread bytes may still be sitting on the socket, so the only safe response is to close"* - becomes
  routine once a body exists, so it is a return type rather than a comment: `Outcome` says whether the socket is still
  at a request boundary. A refusal within the record limit drains the body and keeps the connection; one over the limit
  closes, because draining ten gigabytes to report that ten gigabytes is too many is the denial of service the limit
  exists to prevent.
- **A short body is refused, not stored.** Fewer bytes than announced means a truncated record under a key the caller
  would then trust, which is the corruption directory files exist to rule out. The staging is removed and the transfer
  fails.
- **A stalled transfer needed a bound of its own.** `idle_timeout_ms` watches a connection with nothing in flight; a
  client that announces 64 MiB and sends a byte a second is neither idle nor finished. `transfer_stall_timeout_ms`
  bounds *no progress* rather than total duration, so a slow link moving a large record is untouched, and it is on by
  default because once a body is announced there is no line-reader bound left to fall back on.
- **Transactions:** `TRANSACT` applies a set of writes and deletes across any number of files of one account so that
  either all of it is visible or none of it is. Nothing used to span records: a caller with two records that had to
  change together wrote one, then the other, and hoped. The set is written to an **intent** - tmp-then-rename, CRC32C
  trailer, fsynced - *before* anything is applied, and the intent is removed only once every file it names is fsynced;
  `Database::new` replays whatever it finds. Recovery is forward rather than backward, which works because every change
  is idempotent by construction, and it is the only shape available when several file renames cannot be made one atomic
  act. A set outside the scope - a queue file, more than a thousand changes - is refused with its own
  `TRANSACTION_SCOPE` code and writes nothing, because **a caller can handle a refusal and cannot handle a guarantee
  that quietly does not hold**. The property is asserted the only way it can be: a test re-executes the test binary and
  has the child SIGKILL itself between the two halves of a set, then checks that one file has its record, the other does
  not, and that opening the database makes it whole.
- **The one place two file locks are held:** a transaction takes every file its set touches, in file-name order, and
  keeps them across the write-out. It is the single exception to *at most one file lock at a time*, and the ordering is
  the whole deadlock argument - nothing else in the engine holds two, so nothing else can be the second party to a
  cycle. Releasing the locks before flushing looked simpler and was wrong: a ticker flush slipping into the gap would
  write one of the files under that *file's* sync policy, and the intent would then be retired over bytes that were only
  in the page cache. That is why `flush_locked` exists beside `flush_handle`.
- **Conditional writes:** `WRITE` overwrote, always, which is two lost-record bugs in one line. Two clients that both
  intend to *create* a record write the same key and the second silently wins; two that each read, change and write
  back lose one of the changes. Both writes were valid, so nothing was reported. A write may now carry `if_absent` or
  `if_match`, and `DELETE` takes `if_match` too - deleting a record somebody has since changed is the same bug wearing
  a different hat. **No new synchronisation was needed**: a write already holds the file's own lock and the record it
  is about to replace is in hand there, so the comparison and the write happen inside one guard with no release in
  between. That is the whole concurrency argument.
- **The refusal is the feature.** `PRECONDITION_FAILED` is its own code, because the entire value is in telling a
  collision - read again and retry - apart from a failure, where retrying changes nothing. Reusing a generic write
  error would have left a client exactly where it started. Same rule the error codes went in under: the code is the
  interface, the message is for a person.
- **The token is a digest, not a counter.** A counter is a field, and the record section has no room for one: adding it
  would be a change to the frame encoding and a migration of every file, to hold a number the record's own bytes
  already determine. `Record::version` hashes the bytes that are already in hand, costs nothing on disk, and is
  documented as **opaque** - which is what leaves the choice reversible. Per record rather than per file, because per
  file would turn every concurrent write to a busy file into a conflict, which is exactly where the feature is needed.
- **Server-minted keys:** `WRITE` with no key on a file created `AUTOKEY` mints one and returns it, the way `ENQUEUE`
  already did. The machinery existed - a queue mints under the file's lock and keeps arrival order - so an autokey file
  uses the same counter, moved into `db::sequence` and shared, rather than growing a second answer to "what does a
  minted key look like". Twenty zero-padded digits carrying the millisecond, so **a range read comes back in write
  order by sorting the keys as text**; the width is part of the interface, because a width chosen too small is a
  migration.
- **Two sources for the counter, and both matter.** The `autokey` file beside the records is authoritative when it is
  there, because a key it has handed out may since have been *deleted* and must not come round again. The keys already
  in the file are the backstop for when it is not - lost, restored, or the flag turned on by hand. Between them a
  minted key collides only if both are wrong at once, and because the clock is in the key even a counter starting from
  nothing moves forward past everything minted before it. The restart property is asserted the only way it can be: a
  test re-executes the test binary, has the child delete its highest key and SIGKILL itself, then checks that the key
  does not come back.
- **What the dashboard shows, and what it will not.** A file's next key is on the page, because "where has the counter
  got to" is the question an autokey file raises and none of the other numbers answers it. The counter *behind* the key
  is not: it is a `u64` near 1.7e18, past the integer JavaScript holds exactly, so the browser has already rounded it
  by the time the page sees it. The key is the string the server formatted and says the same thing without the lie.
  Neither costs a load - the counter comes from memory when the file is open and from its own small file when it is
  not - because describing a file must not be what pulls it into the cache.
- **Opt-in, and exclusive.** A file that does not mint keys refuses a keyless write, naming the flag, rather than
  inventing behaviour - and `AUTOKEY` is refused alongside `QUEUE` and `DIRECTORY`, because a queue already mints every
  key it stores and a directory file's keys are the names of host files. Same principle as the directory-file refusals:
  a command asking for two different files has asked a question only the operator can answer.
- **The test that would have caught the original bug.** Eight threads racing to create one key: exactly one succeeds
  and seven are told they collided. Eight more doing a read-modify-write with no retry: the count on disk equals the
  number of writes that were *acknowledged*, which before the condition existed was eight acknowledgements over a count
  of anything from one upwards. Writing those found a **pre-existing** race, since fixed - see below.
- **Certificate Management:** Implemented `GENERATE.CERT` in the `SYSTEM` account, allowing users to create signed
  client certificates and PKCS#12 (.pfx) files directly from the database CLI for simplified secure remote access setup.
- **Typed errors:** The engine reports a `DbError` variant - `FileNotFound`, `AccountExists`, `IndexNotFound`, `Io` and
  the rest - rather than an `io::Error` carrying English prose, and every protocol error reply carries a stable `code`
  beside its `message`. The rule that keeps it honest: **the code is the interface, the message is for a person**. A
  client branches on `FILE_NOT_FOUND`; nothing branches on wording, so a refusal can be reworded without breaking a
  test or a caller. `docs/protocol.md` lists every code, a documentation test fails when one is added and not written
  up, and a handler test fires a refusal at every command and fails on any that answers without a code. It paid for
  itself immediately: a `query_string` the parser could not read used to come back as *the whole file* with
  `status: "OK"`, because "not a query" and "no query" were the same `None`. It is now `INVALID_QUERY`.

### 5. Health: turning a number into advice

- **Verdicts, not raw figures:** `FILE.STATS` used to answer "how big is this file" and `LIST.INDEXES` reported three
  counts. Neither answered the question an administrator actually has, which is *is this healthy, and will it stay that
  way*. Every derived measure now carries a verdict - `good` / `watch` / `act` - and the threshold that produced it.
- **The rule that keeps it honest:** the verdict is decided **on the server**, in `db::health`, and every threshold in
  the system sits in one module. The CLI, the remote protocol and the browser describe the same file, and three copies
  of "5% is the line" is three chances to disagree. A client branches on `id` and `verdict`; `label`, `threshold` and
  `detail` are prose for a person, exactly as an error code is the interface and its message is not. The dashboard's
  `shared/health.ts` holds no threshold at all - only presentation.
- **A measure that cannot be judged says so.** Skew over four records, usage on a server that started a second ago:
  both report `good` and explain why in their detail. Inventing a verdict from too little data is how a dashboard
  teaches people to ignore it.
- **Records per group, without reading a record.** The count is in each group's 20-byte trailer, so the true
  distribution costs one seek per group. A test asserts the file is still not loaded afterwards, because
  `docs/web_dashboard.md` promises that and the easy implementation would have broken it.
- **The bug the distribution found:** it was first computed over the group *files*. An empty group has no file, so a
  file whose records had piled into four groups out of thirty-two averaged out as perfectly even - the exact case skew
  exists to catch. It is over the modulus now, with absent groups counted as the zeroes they are.
- **Excluded index values.** An index can skip nominated values: the shape it exists for is a field where 90% of
  records carry one value, which is excellent to index *for the other 10%*. The whole risk is the planner, and the
  contract is that a lookup on an excluded value returns `None` - "I cannot help, scan for it" - and never an empty
  posting list, which would read as "no records". That is sound because "I do not know" was already an answer the
  planner handled. The tests run the same queries with and without the index and assert the answers are identical,
  the excluded value included. Excluding the dominant value of a skewed field is 8x off the write path on a thousand
  records, and takes the index section from 84 KB to 12 KB on ten thousand.
- **A benchmark that measured nothing.** The first version of `storage/excluded_write` re-stored records with the
  status they already had, and an index charges nothing for that - it compares two short lists and stops. Two rounds of
  "the numbers are suspiciously equal" before the write was made to actually *move* the value. Worth remembering: a
  benchmark agreeing with the null hypothesis is a claim about the benchmark first.
- **Usage counters, honestly attributed.** Survivors are credited to an index only when one index resolved the whole
  query; once an `AND` intersects two there is no honest way to say which of them a surviving record is owed to, so
  such a query counts its lookups and leaves the precision alone.

### 6. Concurrency

- **Per-file locking:** The database-wide write lock is gone. Each loaded file carries its own lock, and every other
  piece of shared state - the account registry, the file listings, the client authorizations, the flush accounting -
  has one of its own, so `READ`, `WRITE`, `DELETE` and `QUERY` need only a shared borrow of the database and lock the
  one file they name. Writers to different files no longer exclude each other, and a flush excludes the file being
  flushed rather than every writer in the system.
- **The rule to keep:** locks go **outer database lock → account registry → file listings → table map → eviction order
  → one file → the small caches**, and a thread holds **at most one file lock at a time**. A command that ever needs
  two takes them in `(account, file)` order. Never start a full flush while holding a file's lock: the flush locks each
  dirty file in turn and will deadlock on the one already held. `docs/storage.md` and the module documentation on
  `Database` say the same thing at more length; the tests learned it the hard way, by hanging.
- **The rule is checked, not just written down:** a debug build counts the file locks each thread holds and panics where
  a flush starts if any is outstanding, so a violation fails with a message naming the rule rather than stopping the
  process silently. It earned its place on the first run, catching a flush under a held lock that had survived only
  because the file happened to be clean at that moment. The counting compiles out of a release build.
- **Batching per file:** an ordinary buffered write flushes the file it touched, not the whole database, so a burst on
  one file no longer drags every other file through a flush with it. The connection-close, ticker and shutdown paths
  still flush everything, which is what bounds how long any change can stay in memory.

### 7. The bug the concurrency tests found

- **A durable `WRITE` could fail for no reason a caller could act on.** Not often - perhaps one run in six with eight
  threads on one file - and never with a record lost, which is why it had gone unnoticed: it surfaced only once there
  were tests that wrote to one file from several threads at once.
- **Two copies of one file.** A thread that found a cached file stale dropped it from the map *even though another
  thread was still holding a handle to it*. Dropping the entry does not drop the file - it only lets the next caller
  load a second copy from disk beside the one still in use. The eviction path had always known this and skipped a held
  file, saying so in its own comment; the invalidation path a few lines above did the same thing without the check.
  One rule, written down once and implemented twice, is how that happens.
- **The symptom was two directories away from the cause.** The second copy's load swept the section's `.tmp` files on
  its way past, and one of them was not crash debris - it was the temporary the *first* copy's flush was between
  `create` and `rename` of. The rename then failed with `NotFound` and the write came back `IO_ERROR`.
- **The sweep moved to where the question can be answered.** "Is any flush of this section in progress" is the file
  lock's guarantee, and a load is precisely the thing that runs before there is a file to lock, so it could never
  answer it. It happens on a loaded file's first flush now, under the lock that flush already holds - still one
  directory scan per load rather than per write. Leaving the debris until then costs only space: a `.tmp` is never
  read, by anything.
- **Both halves are pinned.** `concurrent_writers_to_one_durable_file_all_succeed` is the original failure, and
  `a_table_somebody_is_holding_is_not_invalidated_out_from_under_them` is the rule that prevents it - the second fails
  deterministically when the check is removed, which the first, at one run in six, could never be trusted to do.

### 8. Upgrading over a mounted volume

- **The question nobody could answer.** The server is deployed as an image with `db_storage/` on a volume, and the
  volume outlives the container by design - so swapping the tag *is* the upgrade. Nothing recorded which build wrote
  the data and nothing checked, which made every one of those upgrades an untested assumption.
- **The failure worth preventing is not a server that will not start.** It is a newer binary opening an older
  directory, misreading a structure whose layout changed, and either answering wrongly or writing something the older
  binary can no longer read - by which point the rollback is broken too. A database that refuses to start is a bad
  morning; one that starts and is subtly wrong is a bad quarter. `db_storage/.format` exists to turn the second into
  the first.
- **Two numbers, and four outcomes.** A build declares the version it writes and the oldest it opens. A directory at
  the current version starts silently; one in between is migrated in place and restamped; one outside the range is
  **refused**, naming the version found and the range supported, before a byte is read. `SERVER.STATS` reports both
  numbers, so "can this volume move to that image" is a question you ask the running server rather than a file inside
  a container.
- **In place, not a runbook.** Migrating on open was the deliberate choice over refusing until an operator runs a
  command: an upgrade should be "pull the new tag and start it". The price is that the rollback then fails, and the
  price is paid openly - the start prints *an older build can no longer open it*, and `docs/deployment.md` says to
  take the backup first, because a migration is the one step in an upgrade that changing the tag back does not undo.
- **The one assumption, made once.** Every directory that exists today has no stamp. There has only ever been one
  lineage of this format, so an unstamped directory is version 1 - less a guess than the only thing it can be. It is
  adopted, announced on the start that does it, and recorded, so it is never assumed again. The alternative - refusing
  until a human passes a flag - would have broken the start-up of every existing deployment to avoid an assumption
  that is not in doubt.
- **"I cannot read this" is not "this never said anything".** A damaged stamp is refused, because the version it
  carried is exactly what cannot be recovered - and treating it as absent would adopt a directory of *any* version as
  version 1, which is the corruption the whole mechanism exists to prevent. That is why `statefile::read` returns
  `Missing` and `Unreadable` as different answers rather than an `Option`, and why the queue's book - which genuinely
  does not care, since starting over costs a few redeliveries - is the caller that collapses them.
- **A ladder that cannot lose a rung.** The migrations are a list keyed by the version they start from, and a unit
  test walks the supported range and fails if one is missing. Raise the current version without adding its step and
  the build fails at `cargo test`; reach that state anyway and the server refuses to open the directory rather than
  stamping a version nothing converted it to.
- **Not every change is a version.** The constant rises only for a change an older build would *misread*. A new `DIR`
  attribute it ignores is not one. A number that goes up for changes that did not need it makes every upgrade a
  migration and teaches operators to stop reading it.

### 9. Association groups: correlated multivalues for free

- **The gap.** `BY.EXP` accepted exactly one field, and refused two with *Only one BY.EXP field may be given*. That
  refusal was honest rather than lazy: three accounts beside three dates could mean three rows or nine, and nothing in
  the data says which. Lifting the restriction was never a parser change - it needed somewhere to record that the two
  fields belong together.
- **Where it is recorded:** attribute 5 of the **dependent**, naming its controller, with attribute 6 saying which tier
  it pairs on. The alternative - a multivalued list of dependents on the controller - makes `BY.EXP CONTROLLER` a
  single lookup, and buys that with a list that can name a field that no longer exists, or that another controller also
  claims. On the dependent, a field is in at most one group *by construction*, and there is nothing to keep in step.
  The cost is a scan of the dictionary to resolve a group; it is a small map held in memory, scanned once per query.
- **Two tiers, because PICK has two.** A `V` member pairs value for value; an `S` member pairs sub-value for sub-value
  *inside* the controlling value, and a value the second tier reaches becomes one row per sub-value with the `V`
  members repeating down them. Retrofitting a depth onto the format later would have been the expensive kind of change,
  so the tier went in with the attribute rather than after it.
- **The one thing a group changes that surprises people.** A lone `BY.EXP` field gives a row for the deepest thing that
  matched, down to a sub-value. A `V` member gives a row for the whole *value*, even when a sub-value satisfied the
  criterion - because that is the tier its siblings are lined up against. Declaring an association therefore changes
  what `BY.EXP` on that field returns. It is documented as the price of the values lining up, and a field that should
  still explode by sub-value is an `S` member.
- **What did not have to change.** `SelectEntry` already carried one `ValuePosition`, and one position is enough for a
  whole group: "value 2" now means value 2 of every member. So the saved-list encoding, the wire's `positions` and
  `Request.explode` - already a `Vec` - all kept their shape. What broadened is only what a position is resolved
  *against*, which is one `Narrowing` per column, decided once from the dictionary rather than per cell.
- **Ragged is normal, not an error.** Three accounts beside two dates give three rows, the third showing an empty date.
  Nothing rejects a write that leaves a group uneven, and rows come from the longest member: dropping a value because a
  sibling ran out of them would hide data that is really there.
- **A criterion has to narrow.** The first version unioned every member's positions unconditionally, so
  `WITH ACCT.CODES = "P-7"` came back with every row - the two members carrying no criterion drowned out the one that
  did. The rule that works: the members a criterion names select the rows, and only when *no* member is named does
  every member contribute. Non-selecting `S` members are still read afterwards, to say how deep the chosen rows go.
- **The bug the loop found.** `Database::and_condition(node, None)` returned `None` - discarding the node - because it
  was spelled `QueryNode::Condition(condition?)` and had only ever been called with a `Some`. Folding a clause of
  several `BY.EXP` specs in calls it once per spec, so the specs carrying no criterion threw away the `WITH` clause of
  the ones that did. Every unit test passed; it took driving the real CLI to see four rows where two belonged. The
  function is total now, and says in its own documentation why the obvious spelling is wrong.

### TLS Troubleshooting

- **UnknownIssuer error (on server logs)**: The client certificate is not signed by a CA the server trusts. Correct by
  ensuring the client certificate is signed by `ca.crt` or by updating the server's CA store.
- **UnknownCA fatal alert (on server logs)**: The client does not trust the server's certificate. Correct by providing
  `ca.crt` to the client's trust store.
- **No client certificate provided**: The server requires client authentication. Ensure the client is sending its
  certificate and key.
- **Unauthorized certificate**: The certificate thumbprint is not in the authorized list. Use
  `AUTHORIZE.CONN <thumbprint> <name> <ADMIN | accounts>` in the CLI to grant access.
- **Access denied for account**: The client is authorized but trying to access an account not in its allowed list.
  Use `ADD.CLIENT.ACCOUNT <name> <accounts>` to grant access to additional accounts.

## Lessons Learned

- **Safety First:** Rust's compiler is an excellent partner for AI agents, catching many hallucinations or logic errors
  before they reach runtime.
- **Context is Key:** Providing the agent with clear documentation and a well-structured project allows for more
  accurate and maintainable code generation.
- **Iterative Refinement:** Agents excel at fixing specific errors (like the `ConnectionRefusedError` or TLS EOF issues)
  when provided with exact traceback and logs.
