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
  `SAVE-LIST`/`GET-LIST` and the remote protocol.
- **Dictionary Support:** Logic for field formatting and conversions (Dates, Numbers).
- **Query Engine:** Implementation of `SELECT` and `QUERY` commands for data retrieval.
- **Test Infrastructure:** Added `CREATE.TEST.ACCOUNT` command in the `SYSTEM` account to quickly spin up pre-populated
  accounts for feature verification and regression testing. It is reachable over the remote protocol and from the web
  dashboard as well, so the fixture is one command away from any interface. This command must be maintained and updated as new data
  structures or features are added to the system - the `USERS` file now carries a multivalued `ROLES` field, one of
  whose values is sub-valued, so the fixture reaches every level of the hierarchy.
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

### 5. Concurrency

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
