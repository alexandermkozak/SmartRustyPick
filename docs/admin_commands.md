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

Authorize a client certificate SHA-256 thumbprint with a name for easy management. This command is restricted to the
`SYSTEM` account.

- **Usage**: `AUTHORIZE.CONN <thumbprint> <name>`
- **Example**: `AUTHORIZE.CONN ef9d7b4d5... my-laptop`
- **Note**: The authorization is stored in the `$CLIENTS` file within the `SYSTEM` account for durability.

#### DEAUTHORIZE.CONN

Deauthorize a client certificate by its assigned name. This command is restricted to the `SYSTEM` account.

- **Usage**: `DEAUTHORIZE.CONN <name>`
- **Example**: `DEAUTHORIZE.CONN my-laptop`

#### LIST.CONNS

List all authorized certificate names and their thumbprints. This command is restricted to the `SYSTEM` account.

- **Usage**: `LIST.CONNS`

#### GENERATE.CERT

Generate and sign a new client certificate and private key using the system's CA. This command is restricted to the
`SYSTEM` account.

- **Usage**: `GENERATE.CERT <common_name>`
- **Example**: `GENERATE.CERT myclient`
- **Output**: Creates `myclient.crt`, `myclient.csr`, and `myclient.key` in the current directory.
- **Note**: After generation, you must use `AUTHORIZE.CONN <thumbprint> <name>` to allow the certificate to connect to
  the server.

#### START.SERVER

Start the SSL TCP server for remote access. If the address/port is omitted, it defaults to `127.0.0.1` and the
`server_port` specified in `config.toml` (default 8443).

- **Usage**: `START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path>`
- **Example**: `START.SERVER 0.0.0.0:8443 server.crt server.key ca.crt` or `START.SERVER server.crt server.key ca.crt`
- **Note**: This starts the server in a background thread.
