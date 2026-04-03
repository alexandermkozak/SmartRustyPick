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

Create a new test account with the specified name and populate it with sample tables (e.g., USERS, PRODUCTS) and
dictionary definitions. This command is restricted to the `SYSTEM` account.

- **Usage**: `CREATE.TEST.ACCOUNT <account name>`
- **Example**: `CREATE.TEST.ACCOUNT TESTDB`

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

#### GENERATE.CERT

Generate and sign a new client certificate and private key using the system's CA, and automatically authorize it. This
command is restricted to the `SYSTEM` account and runs interactively.

- **Usage**: `GENERATE.CERT <common_name>`
- **Example**: `GENERATE.CERT myclient`
- **Output**: Creates `myclient.crt`, `myclient.csr`, `myclient.key`, and `myclient.pfx` in the current directory.
- **Workflow**:
  1. Generates files for the specified `<common_name>`.
  2. Prompts for an **Authorization Name** (defaults to `<common_name>`).
  3. Prompts for **Admin status** (Y/N).
  4. If not Admin, prompts for a comma-separated list of **Allowed Accounts**.
  5. Automatically performs the `AUTHORIZE.CONN` step.
- **Note**:
  - The `.pfx` file is generated with an empty password.
  - If authorization is skipped (e.g., non-admin with no accounts), you can still use `AUTHORIZE.CONN` manually later.

#### START.SERVER

Start the SSL TCP server for remote access. If the address/port is omitted, it defaults to `127.0.0.1` and the
`server_port` specified in `config.toml` (default 8443).

- **Usage**: `START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path>`
- **Example**: `START.SERVER 0.0.0.0:8443 server.crt server.key ca.crt` or `START.SERVER server.crt server.key ca.crt`
- **Note**: This starts the server in a background thread.
