# Remote Connection Protocol

SmartRustyPick exposes a TCP remote protocol secured with TLS and client-certificate
authentication. This document is the client author's reference: every wire field, every
command, its requirements, its response shape and its errors.

> The field and command names below are pinned by
> `crates/core/src/server/protocol_doc_tests.rs`. If the request/response structs in
> `crates/core/src/server/models.rs` or the command list in
> `crates/core/src/server/handler.rs` change without this file being updated, `cargo test`
> fails.

## Transport and authentication

- Connections use TLS (1.3, or 1.2 as a fallback).
- The client **must** present a certificate. The server verifies it against the configured
  CA, then computes the certificate's SHA-256 thumbprint (lowercase hex) and looks it up in
  the authorized-clients table. An unknown thumbprint is logged and the connection is
  dropped without a response.
- Each authorized client carries a **name**, a set of **allowed accounts** and an **admin**
  flag. These govern which accounts the connection may touch and whether it may run admin
  commands.
- Authorize a client with the `AUTHORIZE.CONN` command (available from the CLI and over the wire), or let
  `GENERATE.CERT` issue and authorize a certificate in one step. `LIST.CONNS`
  reports what is currently authorized.

## Message framing

Line-delimited JSON. Each request is exactly one line of JSON terminated by `\n`; the
server replies with exactly one line of JSON terminated by `\n`. The connection stays open
for further requests until either side closes it. Buffered writes made during the
connection are flushed to disk when it closes.

## Request object

All fields other than `command` are optional at the JSON level; whether a given command
*requires* one is listed per command below. Unknown fields are ignored. `command` is
matched case-insensitively.

| Field             | Type             | Used by                                                                                                            | Notes                                                                                                                                                                                                                                                                                     |
|-------------------|------------------|--------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `command`         | string           | all                                                                                                                | Required. See [Commands](#commands).                                                                                                                                                                                                                                                      |
| `account`         | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `GET.NEXT`, `CREATE.FILE`, `DELETE.FILE`, `LIST.FILES`, `FILE.STATS` | Account context for the operation. If omitted and the client has exactly one allowed account, that account is used. An admin client with more than one possible account must send it. Access is denied if the account is not in the client's allowed list (admins may reach any account). |
| `target_account`  | string           | `CREATE.ACCOUNT`, `DELETE.ACCOUNT`                                                                                 | Name of the account to create or drop. (Distinct from `account`, which selects an existing context.)                                                                                                                                                                                      |
| `file`            | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `CREATE.FILE`, `DELETE.FILE`, `FILE.STATS`                           | Table (file) name.                                                                                                                                                                                                                                                                        |
| `key`             | string           | `READ`, `WRITE`, `DELETE`                                                                                          | Record key.                                                                                                                                                                                                                                                                               |
| `data`            | string \| object | `WRITE`                                                                                                            | Record contents. A string is parsed as a display-format record (`^` field mark, `]` value mark, `\` sub-value mark). An object maps field names — original dictionary names or their camelCase form — to values, applying the dictionary's input conversions (ICONV).                     |
| `structured_data` | object           | `WRITE`                                                                                                            | Same object form as `data`, checked first when present. Use either this or `data`, not both.                                                                                                                                                                                              |
| `is_dict`         | bool             | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`                                                                       | Operate on the file's dictionary section instead of its data section. Default `false`.                                                                                                                                                                                                    |
| `query_string`    | string           | `QUERY`, `SELECT`                                                                                                  | Pick-style query, e.g. `WITH NAME = "John" BY NAME`. Alternative to `query_node`. A bare command with neither selects every record.                                                                                                                                                       |
| `query_node`      | object           | `QUERY`, `SELECT`                                                                                                  | Structured query tree. Takes precedence over `query_string`. See [Query node](#query-node).                                                                                                                                                                                               |
| `sort_specs`      | array of objects | `QUERY`, `SELECT`                                                                                                  | Explicit sort order: `[{"field_name": "NAME", "descending": false}]`. Overrides any `BY`/`BY.DSND` parsed from `query_string`.                                                                                                                                                            |
| `list_name`       | string           | `SELECT`, `GET.NEXT`                                                                                               | Names the server-side select list. Default `"DEFAULT"`.                                                                                                                                                                                                                                   |
| `batch_size`      | integer          | `GET.NEXT`                                                                                                         | Records per batch. Default `1`.                                                                                                                                                                                                                                                           |
| `thumbprint`      | string           | `AUTHORIZE.CONN`                                                                                                   | SHA-256 thumbprint (lowercase hex) of the client certificate to authorize.                                                                                                                                                                                                                |
| `name`            | string           | `AUTHORIZE.CONN`, `DEAUTHORIZE.CONN`, `ADD.CLIENT.ACCOUNT`, `REMOVE.CLIENT.ACCOUNT`, `GENERATE.CERT`               | Human-readable client name; the identifier for later management. For `GENERATE.CERT` it is also the certificate's common name, so it is limited to letters, digits, `.`, `-` and `_`.                                                                                                     |
| `accounts_list`   | array of strings | `AUTHORIZE.CONN`, `ADD.CLIENT.ACCOUNT`, `REMOVE.CLIENT.ACCOUNT`, `GENERATE.CERT`                                   | Allowed accounts for the client. Default `[]`.                                                                                                                                                                                                                                            |
| `is_admin`        | bool             | `AUTHORIZE.CONN`, `GENERATE.CERT`                                                                                  | Grant the client admin rights. Default `false`.                                                                                                                                                                                                                                           |
| `durable`         | bool             | `CREATE.FILE`                                                                                                      | Create the file with per-file durable writes. Default `false`. See [Storage Engine](storage.md).                                                                                                                                                                                          |

## Response object

Only `status` is always present. Every other field is present only when that command
populates it.

| Field     | Type                     | Populated by                                                      | Notes                                                                                                                                                                                                            |
|-----------|--------------------------|-------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `status`  | string                   | all                                                               | `"OK"`, `"ERROR"` or `"EOF"`.                                                                                                                                                                                    |
| `message` | string                   | errors                                                            | Human-readable error text; set whenever `status` is `"ERROR"`.                                                                                                                                                   |
| `record`  | object                   | `READ`, `FILE.STATS`, `SERVER.STATS`, `GENERATE.CERT`             | For `READ`, the record as field-name → display-formatted string (see [Record shape](#record-shape)). The management commands use it for their single result object, whose shape is documented with each command. |
| `results` | array of `[key, record]` | `QUERY`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`                | Ordered `[string, object]` pairs. For `QUERY` and `GET.NEXT` each `record` has the same shape as `READ`; the management commands document their own.                                                             |
| `keys`    | array of strings         | `LIST.FILES`                                                      | Plain list of names.                                                                                                                                                                                             |
| `count`   | integer                  | `SELECT`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`, `LIST.FILES` | `SELECT`: number of keys selected into the list. `GET.NEXT`: number of records in the batch just returned. The list commands: number of entries returned.                                                        |

There is no `NOT_FOUND` status. A missing record, table or list yields
`status: "ERROR"` with an explanatory `message`.

### Record shape

A serialized record is a JSON object built from the file's dictionary: one entry per
dictionary field that maps to attribute 1 or higher. Keys are the dictionary name lowered
to camelCase (`FIRST.NAME` → `firstName`). Values are always strings, in display format,
with the dictionary's output conversion (OCONV) applied. The record **key itself is not
included** — it appears as the pair's first element in `results`, or is the `key` you sent
to `READ`. Fields with no dictionary entry are not returned.

### Query node

`query_node` is a recursive tree. serde's default enum encoding is used:

```json
{ "Condition": { "field_name": "NAME", "op": "=", "value": "John" } }
```

```json
{
  "Logical": {
    "op": "And",
    "left":  { "Condition": { "field_name": "AGE", "op": ">", "value": "21" } },
    "right": { "Condition": { "field_name": "STATE", "op": "=", "value": "CA" } }
  }
}
```

`op` for `Logical` is `"And"` or `"Or"`.

## Commands

| Command                 | Admin | Account | Required fields                                      | Response on success                     |
|-------------------------|:-----:|:-------:|------------------------------------------------------|-----------------------------------------|
| `READ`                  |       |   yes   | `file`, `key`                                        | `record`                                |
| `WRITE`                 |       |   yes   | `file`, `key`, and one of `data` / `structured_data` | `status: "OK"`                          |
| `DELETE`                |       |   yes   | `file`, `key`                                        | `status: "OK"`                          |
| `QUERY`                 |       |   yes   | `file`                                               | `results`                               |
| `SELECT`                |       |   yes   | `file`                                               | `count`                                 |
| `GET.NEXT`              |       |  yes¹   | `list_name` (defaults to `"DEFAULT"`)                | `results` + `count`, or `status: "EOF"` |
| `CREATE.ACCOUNT`        |  yes  |    —    | `target_account`                                     | `status: "OK"`                          |
| `DELETE.ACCOUNT`        |  yes  |    —    | `target_account`                                     | `status: "OK"`                          |
| `CREATE.FILE`           |  yes  |   yes   | `account`, `file`                                    | `status: "OK"`                          |
| `DELETE.FILE`           |  yes  |   yes   | `account`, `file`                                    | `status: "OK"`                          |
| `AUTHORIZE.CONN`        |  yes  |    —    | `thumbprint`, `name`                                 | `status: "OK"`                          |
| `DEAUTHORIZE.CONN`      |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `ADD.CLIENT.ACCOUNT`    |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `REMOVE.CLIENT.ACCOUNT` |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `GENERATE.CERT`         |  yes  |    —    | `name`                                               | `record`                                |
| `LIST.CONNS`            |  yes  |    —    | —                                                    | `results` + `count`                     |
| `LIST.ACCOUNTS`         |       |    —    | —                                                    | `results` + `count`                     |
| `LIST.FILES`            |       |   yes   | `account`                                            | `keys` + `count`                        |
| `FILE.STATS`            |       |   yes   | `account`, `file`                                    | `record`                                |
| `SERVER.STATS`          |  yes  |    —    | —                                                    | `record`                                |

¹ `GET.NEXT` resolves its account from the select list created by `SELECT`, so it does not
need `account` on the request itself.

### READ

Retrieve one record.

- Required: `file`, `key`. Optional: `account`, `is_dict`.
- Errors: `"File not specified"`, `"Key not specified"`, `"Account not specified"`,
  `"Record not found"`, `"Access denied for account <name>: Not in allowed list"`,
  `"Table error: <detail>"`.

```json
{"command": "READ", "account": "SALES", "file": "USERS", "key": "3"}
```

```json
{"status": "OK", "record": {"name": "Alice", "email": "alice@example.com"}}
```

### WRITE

Create or replace one record. The table's dictionary is pre-loaded so object data maps
correctly.

- Required: `file`, `key`, and one of `data` or `structured_data`. Optional: `account`,
  `is_dict`.
- `data` as a string is a raw display-format record; `data` as an object, or
  `structured_data`, is field-name → value with ICONV applied.
- Errors: `"File not specified"`, `"Key not specified"`, `"Data not specified"`,
  `"Invalid structured data"`, `"Invalid data type in data field: expected string or
  object"`, `"Save error: <detail>"`, `"Table error: <detail>"`.

```json
{"command": "WRITE", "account": "SALES", "file": "USERS", "key": "3",
 "data": {"name": "Alice", "email": "alice@example.com"}}
```

```json
{"command": "WRITE", "account": "SALES", "file": "USERS", "key": "3",
 "data": "Alice^alice@example.com"}
```

```json
{"status": "OK"}
```

### DELETE

Remove one record. Succeeds whether or not the key existed.

- Required: `file`, `key`. Optional: `account`, `is_dict`.
- Errors: `"File not specified"`, `"Key not specified"`, `"Save error: <detail>"`,
  `"Table error: <detail>"`.

```json
{"command": "DELETE", "account": "SALES", "file": "USERS", "key": "3"}
```

```json
{"status": "OK"}
```

### QUERY

Search a file and return the matching records inline, in one response.

- Required: `file`. Optional: `account`, `is_dict`, `query_string`, `query_node`,
  `sort_specs`. With no query given, every record is returned.
- Response: `results`, an ordered list of `[key, record]` pairs.
- Errors: `"File not specified"`, `"Table error: <detail>"`, access-denied.

```json
{"command": "QUERY", "account": "SALES", "file": "USERS",
 "query_string": "WITH NAME = \"John Doe\""}
```

```json
{"status": "OK", "results": [["1", {"name": "John Doe", "email": "john@example.com"}]]}
```

### SELECT

Run the same search as `QUERY` but store the resulting keys in a named server-side select
list for paged retrieval with `GET.NEXT`. Only the count is returned.

- Required: `file`. Optional: `account`, `is_dict`, `query_string`, `query_node`,
  `sort_specs`, `list_name` (default `"DEFAULT"`).
- Re-using a `list_name` replaces the previous list and resets its cursor.
- Errors: `"File not specified"`, `"Table error: <detail>"`, access-denied.

```json
{"command": "SELECT", "account": "SALES", "file": "USERS",
 "query_string": "BY NAME", "list_name": "MYLIST"}
```

```json
{"status": "OK", "count": 2}
```

### GET.NEXT

Fetch the next batch of records from a select list. Advances the list's cursor.

- Required: `list_name` (default `"DEFAULT"`). Optional: `batch_size` (default `1`).
- Response: `results` (`[key, record]` pairs) and `count` (batch size). When the cursor is
  already at the end, `status: "EOF"` with no other fields.
- Errors: `"Select list not found"`, `"Table error: <detail>"`.

```json
{"command": "GET.NEXT", "list_name": "MYLIST", "batch_size": 50}
```

```json
{"status": "OK", "count": 1, "results": [["1", {"name": "John Doe", "email": "john@example.com"}]]}
```

```json
{"status": "EOF"}
```

### CREATE.ACCOUNT / DELETE.ACCOUNT — admin

Create or drop an account. Names the account with `target_account`, not `account`.

- Required: `target_account`. Admin only.
- Errors: `"Admin privileges required"`, `"Account name not specified"`, `"Error: <detail>"`.

```json
{"command": "CREATE.ACCOUNT", "target_account": "SALES"}
```

```json
{"status": "OK"}
```

### CREATE.FILE — admin

Create a table (data and dictionary sections) in `account`.

- Required: `account`, `file`. Optional: `durable`. Admin only.
- With `durable: true` the file is marked mission critical in the account's `DIR` entry, so
  every write to it is flushed to disk before it is acknowledged while the rest of the
  database keeps buffering writes. See [Storage Engine](storage.md).
- Errors: `"Admin privileges required"`, `"Account not specified"`,
  `"File name not specified"`, `"Error: <detail>"`.

```json
{"command": "CREATE.FILE", "account": "SALES", "file": "LEDGER", "durable": true}
```

```json
{"status": "OK"}
```

### DELETE.FILE — admin

Drop a table from `account`.

- Required: `account`, `file`. Admin only.
- Errors: as `CREATE.FILE`.

```json
{"command": "DELETE.FILE", "account": "SALES", "file": "LEDGER"}
```

```json
{"status": "OK"}
```

### AUTHORIZE.CONN — admin

Authorize a client certificate.

- Required: `thumbprint`, `name`. Optional: `accounts_list` (default `[]`), `is_admin`
  (default `false`). Admin only.
- Errors: `"Admin privileges required"`, `"Thumbprint not specified"`,
  `"Name not specified"`, `"Error: <detail>"`.

```json
{"command": "AUTHORIZE.CONN", "thumbprint": "9f86d081...", "name": "reporting-bot",
 "accounts_list": ["SALES"], "is_admin": false}
```

```json
{"status": "OK"}
```

### DEAUTHORIZE.CONN — admin

Revoke a client by name. An active connection for that client is dropped after its next
request with `message: "Client deauthorized"`.

- Required: `name`. Admin only.
- Errors: `"Admin privileges required"`, `"Name not specified"`, `"Client not found"`,
  `"Error: <detail>"`.

```json
{"command": "DEAUTHORIZE.CONN", "name": "reporting-bot"}
```

```json
{"status": "OK"}
```

### ADD.CLIENT.ACCOUNT / REMOVE.CLIENT.ACCOUNT — admin

Add or remove allowed accounts for an existing client. Each account in `accounts_list` is
applied in turn; the first failure aborts and is reported.

- Required: `name`. Optional: `accounts_list` (default `[]`). Admin only.
- Errors: `"Admin privileges required"`, `"Name not specified"`,
  `"Error adding account <name>: <detail>"`,
  `"Error removing account <name>: <detail>"`.

```json
{"command": "ADD.CLIENT.ACCOUNT", "name": "reporting-bot", "accounts_list": ["SUPPORT"]}
```

```json
{"status": "OK"}
```

### GENERATE.CERT — admin

Issue a client certificate signed by the server's CA and authorize it in one step. The private key is generated on the
server, written next to the CA (alongside a PKCS#12 bundle when `openssl` can produce one) and returned in the response,
which is the only time it is sent anywhere.

- Required: `name`, which is both the certificate's common name and the authorization name. Optional: `accounts_list`,
  `is_admin` (default `false`). Admin only.
- A non-admin certificate must be given at least one account, since a client with neither admin rights nor an allowed
  account could do nothing.
- Certificates are valid for 365 days. Re-issuing under an existing name replaces that client's authorization, which
  revokes the previous certificate.
- Errors: `"Admin privileges required"`, `"Name not specified"`,
  `"A non-admin certificate needs at least one allowed account"`,
  `"Certificate generation is unavailable: no server configuration"`, `"Error: <detail>"`.

```json
{"command": "GENERATE.CERT", "name": "reporting-bot", "accounts_list": ["SALES"]}
```

```json
{"status": "OK", "record": {
  "common_name": "reporting-bot",
  "thumbprint": "9f86d081...",
  "certificate_pem": "-----BEGIN CERTIFICATE-----\n...",
  "private_key_pem": "-----BEGIN PRIVATE KEY-----\n...",
  "ca_pem": "-----BEGIN CERTIFICATE-----\n...",
  "cert_path": ".local/certs/reporting-bot.crt",
  "key_path": ".local/certs/reporting-bot.key",
  "pfx_path": ".local/certs/reporting-bot.pfx"
}}
```

### LIST.CONNS — admin

List every authorized client. `results` pairs the authorization name with its details; this is the authorization list,
not the list of open sessions, which `SERVER.STATS` carries.

- Required: nothing. Admin only.
- Errors: `"Admin privileges required"`.

```json
{"command": "LIST.CONNS"}
```

```json
{"status": "OK", "count": 1,
 "results": [["reporting-bot", {"thumbprint": "9f86d081...", "accounts": ["SALES"], "is_admin": false}]]}
```

### LIST.ACCOUNTS

Summarise the accounts the client may reach — every account for an admin, the allowed ones otherwise. Counts come from
each file's section metadata, so no records are read.

- Required: nothing.

```json
{"command": "LIST.ACCOUNTS"}
```

```json
{"status": "OK", "count": 1,
 "results": [["SALES", {"name": "SALES", "directory": "db_storage/SALES",
                        "file_count": 3, "record_count": 1280, "disk_bytes": 262144}]]}
```

### LIST.FILES

Names of the files in one account, sorted.

- Required: `account` (or a client with exactly one allowed account).
- Errors: `"Account not specified"`, access-denied.

```json
{"command": "LIST.FILES", "account": "SALES"}
```

```json
{"status": "OK", "count": 3, "keys": ["DIR", "LEDGER", "USERS"]}
```

### FILE.STATS

Describe one file: how many records it holds, how they are spread across hash groups and what it costs on disk. No
record is returned, and none is read to answer it unless the file is still in the pre-hashfile flat format.

- Required: `account` (or a single-account client), `file`.
- Errors: `"Account not specified"`, `"File not specified"`, `"Error: <detail>"`, access-denied.

```json
{"command": "FILE.STATS", "account": "SALES", "file": "USERS"}
```

```json
{"status": "OK", "record": {
  "account": "SALES", "name": "USERS",
  "record_count": 1280, "dict_count": 4,
  "modulus": 128, "version": 42,
  "group_count": 128, "smallest_group_bytes": 96, "largest_group_bytes": 512,
  "disk_bytes": 262144, "checksums": true, "legacy": false,
  "durable": false, "loaded": true, "modified_seconds_ago": 12
}}
```

### SERVER.STATS — admin

The running server: how long it has been up, what it has served and which sessions are open right now.

- Required: nothing. Admin only.
- `active_connections` lists the sessions holding a TLS connection at this instant, the caller's own included. Totals
  are counted since the process started.
- Errors: `"Admin privileges required"`.

```json
{"command": "SERVER.STATS"}
```

```json
{"status": "OK", "record": {
  "uptime_seconds": 3600, "started_at": 1756400000, "listen_addr": "127.0.0.1:8443",
  "total_connections": 12, "rejected_connections": 1,
  "total_requests": 340, "failed_requests": 2,
  "pending_writes": 0, "loaded_tables": 3, "authorized_clients": 2,
  "active_connections": [
    {"id": 12, "peer": "127.0.0.1:52344", "client_name": "WEB.DASHBOARD",
     "thumbprint": "9f86d081...", "is_admin": true, "connected_seconds": 300,
     "requests": 40, "last_command": "SERVER.STATS", "idle_seconds": 0}
  ]
}}
```

### Unknown commands

```json
{"status": "ERROR", "message": "Unknown command"}
```

Malformed JSON yields `{"status": "ERROR", "message": "Invalid JSON: <detail>"}` and the
connection stays open.

## A full session

```
-> {"command": "CREATE.FILE", "account": "SALES", "file": "USERS"}
<- {"status": "OK"}
-> {"command": "WRITE", "account": "SALES", "file": "USERS", "key": "1", "data": {"name": "John Doe", "email": "john@example.com"}}
<- {"status": "OK"}
-> {"command": "WRITE", "account": "SALES", "file": "USERS", "key": "2", "data": {"name": "Jane Roe", "email": "jane@example.com"}}
<- {"status": "OK"}
-> {"command": "SELECT", "account": "SALES", "file": "USERS", "query_string": "BY NAME", "list_name": "L"}
<- {"status": "OK", "count": 2}
-> {"command": "GET.NEXT", "list_name": "L", "batch_size": 1}
<- {"status": "OK", "count": 1, "results": [["2", {"name": "Jane Roe", "email": "jane@example.com"}]]}
-> {"command": "GET.NEXT", "list_name": "L", "batch_size": 1}
<- {"status": "OK", "count": 1, "results": [["1", {"name": "John Doe", "email": "john@example.com"}]]}
-> {"command": "GET.NEXT", "list_name": "L", "batch_size": 1}
<- {"status": "EOF"}
```

## Starting the server

### Headless mode

`./SmartRustyPick --headless` — requires `cert_path`, `key_path` and `ca_path` in
`config.toml`.

### Automatic background service

If `cert_path`, `key_path` and `ca_path` are set in `config.toml`, the server starts in the
background when the CLI launches.

### The web dashboard

Every one of those start-up paths also brings up the web management dashboard, which is a remote client like any other:
it holds a certificate reissued on each boot and drives the commands above. See [Web Dashboard](web_dashboard.md).

### From the CLI

`START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path>`

```
START.SERVER 0.0.0.0:8443 server.crt server.key ca.crt
```

## MCP server integration

For high-level interaction via AI agents, a Model Context Protocol server lives in `mcp/`.
It wraps this protocol into tools such as `read_record`, `write_record` and
`query_records`. See [mcp/README.md](../mcp/README.md).
