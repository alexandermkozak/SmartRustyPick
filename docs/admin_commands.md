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
[remote protocol](protocol.md) this is admin only, like `CREATE.FILE`; in the CLI it applies to the current account.

- **Usage**: `SET.FILE <name> DURABLE | BUFFERED`
- **Example**: `SET.FILE LEDGER DURABLE`
- **Note**:
  - Promoting a file flushes what it still had buffered, so the flag never gets ahead of the data it protects.
  - The flag is stored as attribute 2 (`DURABLE`) of the file's `DIR` entry; an account without a `DIR` file gets one.
  - `DIR` itself cannot be set: it carries the flags rather than one of its own, and its writes are always flushed.
  - The current setting shows in `LIST.FILES`, in `FILE.STATS` and in the [web dashboard](web_dashboard.md). See
    [Storage Engine](storage.md).

#### AUTHORIZE.CONN

Authorize a client certificate SHA-256 thumbprint with a name and access restrictions. This command is restricted to the
`SYSTEM` account.

- **Usage**: `AUTHORIZE.CONN <thumbprint> <name> <ADMIN | accounts>`
- **Example (Admin)**: `AUTHORIZE.CONN ef9d7b4d5... my-laptop ADMIN`
- **Example (Restricted)**: `AUTHORIZE.CONN ef9d7b4d5... my-laptop MYAPP,TESTDB`
- **Note**:
  - `ADMIN` connections have no account restrictions.
  - Restricted connections MUST provide a comma-separated list of allowed accounts.
  - If a restricted client has only ONE allowed account, the server defaults to that account if none is specified in the
    request.
  - The authorization is stored in the `$CLIENTS` file within the `SYSTEM` account.

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
- **Note**: The same listing is available to admin clients over the [remote protocol](protocol.md) and in the
  [web dashboard](web_dashboard.md), which is how the dashboard manages authorizations.

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
  4. If not Admin, prompts for a comma-separated list of **Allowed Accounts**.
  5. Automatically performs the `AUTHORIZE.CONN` step.
- **Note**:
  - The `.pfx` file is generated with an empty password. It bundles the private key, the client certificate and the CA
    that signed it. The CA belongs in there: a client that selects its certificate by building a chain - Windows'
    Schannel, and so .NET's `SslStream` - will not offer a certificate it cannot chain to the CA the server asked for,
    and the server then drops the connection as unauthenticated.
  - If authorization is skipped (e.g., non-admin with no accounts), you can still use `AUTHORIZE.CONN` manually later.

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
