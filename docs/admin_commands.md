### Administration Commands

These commands are used for managing the SmartRustyPick system, including accounts, server security, and diagnostics.
Many of these commands are restricted to the `SYSTEM` account.

#### CREATE.ACCOUNT

Create a new account.

- **Usage**: `CREATE.ACCOUNT <account name> [<directory>]`
- **Example**: `CREATE.ACCOUNT MYAPP /path/to/myapp`
- **Default**: If no directory is provided, it defaults to a folder named `<account name>` in the root directory.

#### DELETE.ACCOUNT

Delete an account and all its contained data files.

- **Usage**: `DELETE.ACCOUNT <account name>`
- **Example**: `DELETE.ACCOUNT OLDAPP`

#### CREATE.TEST.ACCOUNT

Create a new test account with the specified name and populate it with sample tables (`USERS`, `PRODUCTS` and a `JOBS`
[queue file](general_commands.md#queue-files)) and dictionary definitions. This command is restricted to the `SYSTEM`
account.

- **Usage**: `CREATE.TEST.ACCOUNT <account name>`
- **Example**: `CREATE.TEST.ACCOUNT TESTDB`
- Also reachable over the [remote protocol](protocol.md#createtestaccount--admin) and from the
  [web dashboard](web_dashboard.md#creating-and-dropping), where an admin certificate replaces the `SYSTEM` restriction.

#### SET.FILE

Turn per-file durable writes on or off for a file that already exists, keeping its data. Over the
[remote protocol](protocol.md) this is authorized against the client's allowed accounts, like `CREATE.FILE` — a file
lives in one account and affects nothing outside it; in the CLI it applies to the current account.

- **Usage**: `SET.FILE <name> DURABLE | BUFFERED`
- **Example**: `SET.FILE LEDGER DURABLE`
- **Note**:
  - Promoting a file flushes what it still had buffered, so the flag never gets ahead of the data it protects.
  - The flag is stored as attribute 2 (`DURABLE`) of the file's `DIR` entry; an account without a `DIR` file gets one.
  - `DIR` itself cannot be set: it carries the flags rather than one of its own, and its writes are always flushed.
  - The current setting shows in `LIST.FILES`, in `FILE.STATS` and in the [web dashboard](web_dashboard.md). See
    [Storage Engine](storage.md).

#### EXPORT.FILE / EXPORT.ACCOUNT / EXPORT.ALL

Write an [archive](storage.md#archives-backup-and-restore) of a file, an account or the whole database to a path. This
is the backup: copying `db_storage/` from underneath a running server is not one, because writes are buffered and a
flush rewrites groups and `meta` separately.

- **Usage**: `EXPORT.FILE <file> TO <path>`, `EXPORT.ACCOUNT <account> TO <path>`, `EXPORT.ALL TO <path>`
- **Example**: `EXPORT.ACCOUNT SALES TO /backups/sales-2026-09-12.srp`
- **Note**:
  - `TO` is required. An export names a file an operator will reach for in an emergency, and a default name is how a
    backup ends up somewhere nobody looks.
  - `EXPORT.FILE` exports a file of the **current** account.
  - The archive is written tmp-then-rename and fsynced, so the path never names a half-written backup. A failed export
    leaves nothing behind, staging file included.
  - **What is held still**: the export flushes and then holds every file it names for as long as it is reading them.
    Readers are unaffected; writers to those files wait. `EXPORT.ACCOUNT` gives you an account consistent across its
    files and is the one worth running routinely. `EXPORT.ALL` blocks writes to the whole database for its duration —
    a maintenance-window operation, not a nightly one.
  - `EXPORT.ALL` leaves `SYSTEM` out. It holds the account registry, the logs, and `$CLIENTS` — the authorized client
    certificate thumbprints — and an archive carrying that last one would grant the source machine's authorizations
    wherever it was restored. `EXPORT.ACCOUNT SYSTEM` still works, which makes taking it a deliberate act.
  - `DIR` is the account's listing of its own files rather than data, so it is never exported.
  - Over the [remote protocol](protocol.md#backup-and-restore) this is admin only, and `EXPORT.BYTES` sends the archive
    over the connection for an admin with no filesystem access to the server.

#### IMPORT

Restore an archive — to the same server or a different one.

- **Usage**: `IMPORT <path> [AS <account>] [OVERWRITE] [VERIFY]`
- **Example**: `IMPORT /backups/sales-2026-09-12.srp AS SALES.COPY`
- **Note**:
  - **`VERIFY` first.** It reads the archive through, checks it against what is already there, and prints what a real
    import would do without writing anything. This is the mode to reach for by default.
  - `AS <account>` restores into a different account, which is how a production file is brought up beside the original
    for inspection. An archive holding more than one account has no single account to be renamed onto and is refused.
  - **`OVERWRITE` is required to replace a file that already exists**, and without it the whole import is refused with
    every colliding file named — nothing is written. With it, the file is **replaced, not merged**: a restore restores,
    so records the live file gained since the archive was taken are gone.
  - Nothing is applied until the archive has decoded whole and every file in it has been resolved. A truncated or
    altered archive is refused with nothing written — not even the accounts it would have created.
  - Indexes are rebuilt from the records that actually arrived rather than carried; a field that can no longer be
    indexed is logged to `$LOGS` and skipped rather than failing the restore.
  - A restored [queue file](general_commands.md#queue-files) keeps its records, their order and its policy, but their
    **delivery counts start again** — a record that had used four of its five attempts gets all five back.
  - A restored [directory file](general_commands.md#directory-files) gets the default place inside its own file
    directory. The path in the archive belongs to the machine that exported it, and honouring it elsewhere would either
    fail or succeed against somebody else's directory.

#### AUTHORIZE.CONN

Authorize a client certificate SHA-256 thumbprint with a name and access restrictions. This command is restricted to the
`SYSTEM` account.

- **Usage**: `AUTHORIZE.CONN <thumbprint> <name> <ADMIN | accounts | capabilities>`
- **Example (Admin)**: `AUTHORIZE.CONN ef9d7b4d5... my-laptop ADMIN`
- **Example (Restricted)**: `AUTHORIZE.CONN ef9d7b4d5... my-laptop MYAPP,TESTDB`
- **Example (Provisioning)**: `AUTHORIZE.CONN ef9d7b4d5... deploy-bot accounts:manage`
- **Example (Mixed)**: `AUTHORIZE.CONN ef9d7b4d5... ops MYAPP,server:observe`
- **Note**:
  - The third argument is one comma-separated list. A token is a **capability** if it names one, and an **account**
    otherwise; `ADMIN` is recognised on its own.
  - The capabilities are `accounts:manage`, `clients:manage` and `server:observe`. See
    [Authorization](protocol.md#authorization) for which commands each one covers. An unrecognised capability name is
    refused rather than ignored, so a typo cannot read as a grant that quietly does nothing.
  - **A capability does not grant an account.** A client holding `accounts:manage` can create an account and cannot
    read a single record in it — which is what lets a provisioning credential exist without being a master key over
    every account in the database. Granting access afterwards is `ADD.CLIENT.ACCOUNT`, under `clients:manage`.
  - `ADMIN` connections have no account restrictions and hold every capability, exactly as before. An authorization
    written before capabilities existed behaves identically; nothing has to be migrated.
  - A client must be given at least one of the three: `ADMIN`, an account, or a capability.
  - If a restricted client has only ONE allowed account, the server defaults to that account if none is specified in the
    request.
  - The authorization is stored in the `$CLIENTS` file within the `SYSTEM` account, capabilities in attribute 4.

#### ADD.CLIENT.ACCOUNT

Add one or more allowed accounts to an existing authorized client. Restricted to the `SYSTEM` account.

- **Usage**: `ADD.CLIENT.ACCOUNT <name> <accounts>`
- **Example**: `ADD.CLIENT.ACCOUNT my-laptop NEWAPP,OTHERDB`

#### REMOVE.CLIENT.ACCOUNT

Remove one or more allowed accounts from an existing authorized client. Restricted to the `SYSTEM` account.

- **Usage**: `REMOVE.CLIENT.ACCOUNT <name> <accounts>`
- **Example**: `REMOVE.CLIENT.ACCOUNT my-laptop TESTDB`

#### DEAUTHORIZE.CONN

Deauthorize a client certificate by its assigned name. This command is restricted to the `SYSTEM` account.

- **Usage**: `DEAUTHORIZE.CONN <name>`
- **Example**: `DEAUTHORIZE.CONN my-laptop`

#### LIST.CONNS

List all authorized certificate names and their thumbprints. This command is restricted to the `SYSTEM` account.

- **Usage**: `LIST.CONNS`
- **Output**: name, thumbprint, allowed accounts and capabilities. `ADMIN` is expanded to the full capability set, so a
  row says what a credential is *for* rather than leaving it to be inferred from a flag.
- **Note**: The same listing is available over the [remote protocol](protocol.md) to a client holding `server:observe`
  (or `ADMIN`), and in the [web dashboard](web_dashboard.md), which is how the dashboard manages authorizations.

#### GENERATE.CERT

Generate and sign a new client certificate and private key using the system's CA, and automatically authorize it. This
command is restricted to the `SYSTEM` account and runs interactively.

- **Usage**: `GENERATE.CERT <common_name>`
- **Example**: `GENERATE.CERT myclient`
- **Note**: Admin clients can issue certificates the same way over the [remote protocol](protocol.md); the
  [web dashboard](web_dashboard.md) uses that to generate and download certificates from a browser.
- **Output**: Creates `myclient.crt`, `myclient.key` and `myclient.pfx` in the current directory. The CSR is an input to
  the signing step and is removed once the certificate is signed.
- **Workflow**:
  1. Generates files for the specified `<common_name>`.
  2. Prompts for an **Authorization Name** (defaults to `<common_name>`).
  3. Prompts for **Admin status** (Y/N).
  4. If not Admin, prompts for a comma-separated list of **accounts and/or capabilities**, parsed exactly as
     `AUTHORIZE.CONN` parses its grant list.
  5. Automatically performs the `AUTHORIZE.CONN` step.
- **Note**:
  - The `.pfx` file is **passphrase-protected**. A passphrase is generated for each issuance, printed once by this
    command (and shown once in the [web dashboard](web_dashboard.md)), and stored nowhere — not beside the bundle, not
    in `$LOGS`, and not on any command line, since it reaches `openssl` through the child process's environment rather
    than its arguments. Deliver it to whoever imports the bundle by some route other than the bundle itself. If it is
    lost, re-issue the certificate; there is nothing to recover.
  - The bundle carries the private key, the client certificate and the CA that signed it. The CA belongs in there: a
    client that selects its certificate by building a chain - Windows' Schannel, and so .NET's `SslStream` - will not
    offer a certificate it cannot chain to the CA the server asked for, and the server then drops the connection as
    unauthenticated.
  - If authorization is skipped (e.g., non-admin with neither an account nor a capability), you can still use
    `AUTHORIZE.CONN` manually later.

#### START.SERVER

Start the SSL TCP server for remote access. If the address/port is omitted, it defaults to `127.0.0.1` and the
`server_port` specified in `config.toml` (default 8443).

- **Usage**: `START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path>`
- **Example**: `START.SERVER 0.0.0.0:8443 server.crt server.key ca.crt` or `START.SERVER server.crt server.key ca.crt`
- **Note**:
  - This starts the server in a background thread.
  - The supplied certificate, key and CA paths are used as given, overriding `cert_path`, `key_path` and `ca_path`
    from `config.toml` for this server instance. Every other setting (port defaults aside, ports, durability, the
    dashboard, ...) still comes from `config.toml`.
  - All three paths must already exist; a missing file is reported as an error before the listener is bound, rather
    than silently generating new certificates at the mistyped path.
