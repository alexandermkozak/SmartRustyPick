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
  accounts and their files, per-file durability, creating and dropping accounts and files, creating the populated demo
  account, and maintaining a file's dictionary. It connects to the database as an ordinary remote client, with a certificate reissued and
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
  [Association groups](#7-association-groups-correlated-multivalues-for-free) below.
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
  both tiers of an association, and the ordering primitive as well as the record ones.
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

### 7. Association groups: correlated multivalues for free

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
