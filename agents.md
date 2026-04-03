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

### 2. Testing and Automation

- **Integration Tests:** Developed a Python-based integration suite covering the full CRUD protocol (WRITE, READ, QUERY,
  SELECT LIST, READNEXT, DELETE).
- **Performance Testing:** Created load tests to verify database performance under concurrent-like sequential pressure.
- **Git Hooks:** Automated quality control by setting up a `pre-push` hook that runs `cargo test` to prevent regression.

### 3. Database Core

- **MultiValue Logic:** Implementation of hierarchical data structures (FM, VM, SVM).
- **Dictionary Support:** Logic for field formatting and conversions (Dates, Numbers).
- **Query Engine:** Implementation of `SELECT` and `QUERY` commands for data retrieval.
- **Test Infrastructure:** Added `CREATE.TEST.ACCOUNT` command in the `SYSTEM` account to quickly spin up pre-populated
  accounts for feature verification and regression testing. This command must be maintained and updated as new data
  structures or features are added to the system.
- **Certificate Management:** Implemented `GENERATE.CERT` in the `SYSTEM` account, allowing users to create signed
  client certificates directly from the database CLI for simplified secure remote access setup.

### TLS Troubleshooting

- **UnknownIssuer error (on server logs)**: The client certificate is not signed by a CA the server trusts. Correct by
  ensuring the client certificate is signed by `ca.crt` or by updating the server's CA store.
- **UnknownCA fatal alert (on server logs)**: The client does not trust the server's certificate. Correct by providing
  `ca.crt` to the client's trust store.
- **No client certificate provided**: The server requires client authentication. Ensure the client is sending its
  certificate and key.
- **Unauthorized certificate**: The certificate thumbprint is not in the authorized list. Use
  `AUTHORIZE.CONN <thumbprint>` in the CLI to grant access.

## Lessons Learned

- **Safety First:** Rust's compiler is an excellent partner for AI agents, catching many hallucinations or logic errors
  before they reach runtime.
- **Context is Key:** Providing the agent with clear documentation and a well-structured project allows for more
  accurate and maintainable code generation.
- **Iterative Refinement:** Agents excel at fixing specific errors (like the `ConnectionRefusedError` or TLS EOF issues)
  when provided with exact traceback and logs.
