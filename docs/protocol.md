# Remote Connection Protocol

SmartRustyPick exposes a TCP remote protocol secured with TLS and client-certificate
authentication. This document is the client author's reference: every wire field, every
command, its requirements, its response shape and its errors.

> The field names, the command list and the shape of every object returned in `record` or
> `results` are pinned by `crates/core/src/server/protocol_doc_tests.rs`. If the
> request/response structs in `crates/core/src/server/models.rs`, the commands dispatched by
> `crates/core/src/server/handler.rs` or the structs behind those objects change without this
> file being updated, `cargo test` fails.

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
| `account`         | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `GET.NEXT`, `CREATE.FILE`, `SET.FILE`, `DELETE.FILE`, `LIST.FILES`, `FILE.STATS`, `LIST.DICT`, `SET.DICT` | Account context for the operation. If omitted and the client has exactly one allowed account, that account is used. An admin client with more than one possible account must send it. Access is denied if the account is not in the client's allowed list (admins may reach any account). |
| `target_account`  | string           | `CREATE.ACCOUNT`, `CREATE.TEST.ACCOUNT`, `DELETE.ACCOUNT`                                                          | Name of the account to create or drop. (Distinct from `account`, which selects an existing context.)                                                                                                                                                                                      |
| `file`            | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `CREATE.FILE`, `SET.FILE`, `DELETE.FILE`, `FILE.STATS`, `LIST.DICT`, `SET.DICT` | Table (file) name.                                                                                                                                                                                                                                                                        |
| `key`             | string           | `READ`, `WRITE`, `DELETE`, `SET.DICT`                                                                              | Record key; for `SET.DICT`, the name of the dictionary entry.                                                                                                                                                                                                                                                                               |
| `data`            | string \| object | `WRITE`                                                                                                            | Record contents. A string is parsed as a display-format record (`^` field mark, `]` value mark, `\` sub-value mark). An object maps field names — original dictionary names or their camelCase form — to values, applying the dictionary's input conversions (ICONV).                     |
| `structured_data` | object           | `WRITE`, `SET.DICT`                                                                                                | `WRITE`: same object form as `data`, checked first when present — use either this or `data`, not both. `SET.DICT`: the dictionary attributes of one entry.                                                                                                                                 |
| `is_dict`         | bool             | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`                                                                       | Operate on the file's dictionary section instead of its data section. Default `false`.                                                                                                                                                                                                    |
| `query_string`    | string           | `QUERY`, `SELECT`                                                                                                  | Pick-style query, e.g. `WITH NAME = "John" BY NAME`. Alternative to `query_node`. A bare command with neither selects every record.                                                                                                                                                       |
| `query_node`      | object           | `QUERY`, `SELECT`                                                                                                  | Structured query tree. Takes precedence over `query_string`. See [Query node](#query-node).                                                                                                                                                                                               |
| `sort_specs`      | array of objects | `QUERY`, `SELECT`                                                                                                  | Explicit sort order: `[{"field_name": "NAME", "descending": false}]`. Overrides any `BY`/`BY.DSND` parsed from `query_string`.                                                                                                                                                            |
| `explode`         | array of strings | `QUERY`, `SELECT`                                                                                                  | Multivalued fields to explode, so each matching value becomes its own result row. Only the first is used. Overrides any `BY.EXP` parsed from `query_string`. See [Exploded results](#exploded-results).                                                                                    |
| `list_name`       | string           | `SELECT`, `GET.NEXT`                                                                                               | Names the server-side select list. Default `"DEFAULT"`.                                                                                                                                                                                                                                   |
| `batch_size`      | integer          | `GET.NEXT`                                                                                                         | Records per batch. Default `1`.                                                                                                                                                                                                                                                           |
| `thumbprint`      | string           | `AUTHORIZE.CONN`                                                                                                   | SHA-256 thumbprint (lowercase hex) of the client certificate to authorize.                                                                                                                                                                                                                |
| `name`            | string           | `AUTHORIZE.CONN`, `DEAUTHORIZE.CONN`, `ADD.CLIENT.ACCOUNT`, `REMOVE.CLIENT.ACCOUNT`, `GENERATE.CERT`               | Human-readable client name; the identifier for later management. For `GENERATE.CERT` it is also the certificate's common name, so it is limited to letters, digits, `.`, `-` and `_`.                                                                                                     |
| `accounts_list`   | array of strings | `AUTHORIZE.CONN`, `ADD.CLIENT.ACCOUNT`, `REMOVE.CLIENT.ACCOUNT`, `GENERATE.CERT`                                   | Allowed accounts for the client. Default `[]`.                                                                                                                                                                                                                                            |
| `is_admin`        | bool             | `AUTHORIZE.CONN`, `GENERATE.CERT`                                                                                  | Grant the client admin rights. Default `false`.                                                                                                                                                                                                                                           |
| `durable`         | bool             | `CREATE.FILE`, `SET.FILE`                                                                                          | Per-file durable writes. Optional for `CREATE.FILE`, default `false`; required for `SET.FILE`, where an absent flag is refused rather than read as a demotion. See [Storage Engine](storage.md).                                                                                          |

## Response object

Only `status` is always present. Every other field is present only when that command
populates it.

| Field       | Type                      | Populated by                                                                                         | Notes                                                                                                                                                                                                            |
|-------------|---------------------------|------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `status`    | string                    | all                                                                                                  | `"OK"`, `"ERROR"` or `"EOF"`.                                                                                                                                                                                    |
| `message`   | string                    | errors                                                                                               | Human-readable error text; set whenever `status` is `"ERROR"`.                                                                                                                                                   |
| `record`    | object                    | `READ`, `CREATE.TEST.ACCOUNT`, `SET.FILE`, `FILE.STATS`, `SET.DICT`, `SERVER.STATS`, `GENERATE.CERT` | For `READ`, the record as field-name → display-formatted string (see [Record shape](#record-shape)). The management commands use it for their single result object, whose shape is documented with each command. |
| `results`   | array of `[key, record]`  | `QUERY`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`, `LIST.FILES`, `LIST.DICT`                        | Ordered `[string, object]` pairs. For `QUERY` and `GET.NEXT` each `record` has the same shape as `READ`; the management commands document their own.                                                             |
| `keys`      | array of strings          | `LIST.FILES`, `LIST.DICT`                                                                            | Plain list of names: the files in the account, or the file's dictionary entries. Both commands fill `results` as well, with what is known about each name.                                                       |
| `count`     | integer                   | `SELECT`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`, `LIST.FILES`, `LIST.DICT`                       | `SELECT`: number of keys selected into the list. `GET.NEXT`: number of records in the batch just returned. The list commands: number of entries returned.                                                        |
| `positions` | array of objects or nulls | `QUERY`, `GET.NEXT`                                                                                  | Present only for an exploded result. Index-aligned with `results`: the position within the exploded field that put each row there. See [Exploded results](#exploded-results).                                    |

There is no `NOT_FOUND` status. A missing record, table or list yields
`status: "ERROR"` with an explanatory `message`.

### Record shape

A serialized record is a JSON object built from the file's dictionary: one entry per
dictionary field that maps to attribute 1 or higher. Keys are the dictionary name lowered
to camelCase (`FIRST.NAME` → `firstName`). Values are in display format, with the
dictionary's output conversion (OCONV) applied to each of them. The record **key itself is
not included** — it appears as the pair's first element in `results`, or is the `key` you
sent to `READ`. Fields with no dictionary entry are not returned.

A field holding one value is a **string**. A multivalued field is an **array** of its
values, and a value that has sub-values is a **nested array** of those:

```json
{"name": "Jane Smith", "roles": ["DEV", ["TEST", "LAB"]]}
```

`WRITE` accepts the same shapes back, so a record read, edited and written again keeps its
multivalue structure. A plain string is always stored as a single value — it is never
re-split on `]` — so a value that genuinely contains that character survives the round
trip.

### Exploded results

`QUERY` and `SELECT` can explode a multivalued field: instead of one row per record, the
result carries one row per value of that field, and — when a criterion names the same field
— only the values that satisfied it. This is the wire form of the CLI's `BY.EXP` clause.

Name the field with `explode`, or spell it inside `query_string`; both of these ask the
same question:

```json
{"command": "QUERY", "account": "SYSTEM", "file": "$CLIENTS",
 "explode": ["ACCOUNTS"], "query_string": "WITH ACCOUNTS = \"TEST\""}
```

```json
{"command": "QUERY", "account": "SYSTEM", "file": "$CLIENTS",
 "query_string": "BY.EXP ACCOUNTS = \"TEST\""}
```

A key appears once per matching position, and `positions` says which position each row
came from. `sub_value` is `null` when the whole value matched:

```json
{"status": "OK",
 "results": [["WEB", {"accounts": ["TEST", "PAYROLL"]}],
             ["API", {"accounts": ["DEV", "TEST"]}]],
 "positions": [{"value": 0, "sub_value": null}, {"value": 1, "sub_value": null}]}
```

The record in each row is still the whole record; `positions` is what tells you which part
of it answered the query. Records kept by a condition on some other field appear once with
a `null` position. `positions` is omitted entirely when nothing was exploded.

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
| `CREATE.TEST.ACCOUNT`   |  yes  |    —    | `target_account`                                     | `record`                                |
| `DELETE.ACCOUNT`        |  yes  |    —    | `target_account`                                     | `status: "OK"`                          |
| `CREATE.FILE`           |  yes  |   yes   | `account`, `file`                                    | `status: "OK"`                          |
| `SET.FILE`              |  yes  |   yes   | `account`, `file`, `durable`                         | `record`                                |
| `DELETE.FILE`           |  yes  |   yes   | `account`, `file`                                    | `status: "OK"`                          |
| `AUTHORIZE.CONN`        |  yes  |    —    | `thumbprint`, `name`                                 | `status: "OK"`                          |
| `DEAUTHORIZE.CONN`      |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `ADD.CLIENT.ACCOUNT`    |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `REMOVE.CLIENT.ACCOUNT` |  yes  |    —    | `name`                                               | `status: "OK"`                          |
| `GENERATE.CERT`         |  yes  |    —    | `name`                                               | `record`                                |
| `LIST.CONNS`            |  yes  |    —    | —                                                    | `results` + `count`                     |
| `LIST.ACCOUNTS`         |       |    —    | —                                                    | `results` + `count`                     |
| `LIST.FILES`            |       |   yes   | `account`                                            | `keys` + `results` + `count`            |
| `FILE.STATS`            |       |   yes   | `account`, `file`                                    | `record`                                |
| `LIST.DICT`             |       |   yes   | `account`, `file`                                    | `keys` + `results` + `count`            |
| `SET.DICT`              |       |   yes   | `account`, `file`, `key`, `structured_data`          | `record`                                |
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
  `sort_specs`, `explode`. With no query given, every record is returned.
- Response: `results`, an ordered list of `[key, record]` pairs, plus `positions` when
  `explode` was given.
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
  `sort_specs`, `explode`, `list_name` (default `"DEFAULT"`).
- Re-using a `list_name` replaces the previous list and resets its cursor.
- With `explode`, `count` is the number of exploded rows, not of distinct records, and the
  positions are remembered for `GET.NEXT`.
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
- Response: `results` (`[key, record]` pairs) and `count` (batch size), plus `positions`
  when the list was exploded. When the cursor is already at the end, `status: "EOF"` with
  no other fields.
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
- A created account comes with its `DIR` file, the listing that describes the files it holds
  and carries their durability flags. Everything that reads one treats a missing `DIR` as an
  error rather than as an empty account, so no client has to remember to create it.
- Dropping an account takes every file in it. `SYSTEM` cannot be dropped: it holds the
  account registry and the authorized clients.
- Errors: `"Admin privileges required"`, `"Account name not specified"`, `"Error: <detail>"`.

```json
{"command": "CREATE.ACCOUNT", "target_account": "SALES"}
```

```json
{"status": "OK"}
```

### CREATE.TEST.ACCOUNT — admin

Create an account already populated with the demo fixture — the same one the CLI's
`CREATE.TEST.ACCOUNT` makes, so there is something to query without typing records in first.

- Required: `target_account`. Admin only. The CLI restricts the command to the `SYSTEM`
  account; over the wire an admin certificate is the equivalent gate.
- The account gets a `DIR`, a `USERS` file and a `PRODUCTS` file, each with a dictionary and a
  couple of records. Between them they reach every level of the hierarchy — `ROLES` is
  multivalued and one of its values is sub-valued — and `PRODUCTS.PRICE` carries an `MD2`
  conversion, so the fixture exercises multivalues and conversions rather than only flat text.
- `record` names the account and the files it was given, read back after the fact rather than
  listed from a constant, so it describes whatever the fixture creates today.
- The account must not already exist; nothing is written when it does.
- Errors: `"Admin privileges required"`, `"Account name not specified"`, `"Error: <detail>"`.

```json
{"command": "CREATE.TEST.ACCOUNT", "target_account": "DEMO"}
```

```json
{"status": "OK", "record": {"account": "DEMO", "files": ["DIR", "PRODUCTS", "USERS"]}}
```

### CREATE.FILE — admin

Create a table (data and dictionary sections) in `account`.

- Required: `account`, `file`. Optional: `durable`. Admin only.
- The file is added to the account's `DIR` listing, which is created first if the account
  has not got one.
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

### SET.FILE — admin

Change the durability of a file that already exists, without recreating it — so a file can be
promoted to mission critical, or demoted back, while keeping the records it holds.

- Required: `account`, `file`, `durable`. Admin only.
- With `durable: true` the file's pending writes are flushed as part of the change, so the
  flag never gets ahead of the data it promises to protect. `durable: false` returns the file
  to the database's buffering policy. See [Storage Engine](storage.md).
- The flag is stored in the account's `DIR` entry for the file; an account without a `DIR`
  file gets one.
- `DIR` itself cannot be set: it carries the flags rather than one of its own, and its writes
  are always flushed at once.
- Errors: `"Admin privileges required"`, `"Account not specified"`,
  `"File name not specified"`, `"Durability flag not specified"` (an absent flag is refused
  rather than read as a demotion), `"Error: Table '<name>' not found in account '<account>'"`.

```json
{"command": "SET.FILE", "account": "SALES", "file": "LEDGER", "durable": true}
```

```json
{"status": "OK", "record": {"account": "SALES", "name": "LEDGER", "durable": true}}
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

The files in one account, sorted.

- Required: `account` (or a client with exactly one allowed account).
- `keys` is the plain list of names. `results` pairs each name with what is known about the
  file beside its name — currently `durable`, so a client can see which files flush every
  write without reading the account's `DIR` file. A database running with
  `durable_writes = true` reports every file as durable, because every write then is.
- Errors: `"Account not specified"`, access-denied.

```json
{"command": "LIST.FILES", "account": "SALES"}
```

```json
{"status": "OK", "count": 3, "keys": ["DIR", "LEDGER", "USERS"], "results": [
  ["DIR", {"durable": false}],
  ["LEDGER", {"durable": true}],
  ["USERS", {"durable": false}]
]}
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

### LIST.DICT

Every dictionary entry of one file, decomposed into the attributes
[Data Structures](data_structures.md#dictionary-items) documents.

- Required: `account` (or a single-account client), `file`.
- A dictionary record is a record like any other, so `READ` with `is_dict` labels it with the
  *data* file's field names — attribute 1 comes back under whatever attribute 1 of the file
  is called. This reads the fixed dictionary positions instead. `definition` carries the raw
  display string, so an entry using a position not named here is still visible.
- `field` and `width` are `null` when the entry does not hold a number there.
- Entries come back ordered by attribute number, then by name.
- Errors: `"Account not specified"`, `"File not specified"`, `"Table error: <detail>"`,
  access-denied.

```json
{"command": "LIST.DICT", "account": "SALES", "file": "USERS"}
```

```json
{"status": "OK", "count": 2, "keys": ["NAME", "PRICE"], "results": [
  ["NAME", {"field": 1, "heading": "NAME", "justification": "L", "width": 20,
            "conversion": "", "definition": "1^NAME^L^20"}],
  ["PRICE", {"field": 2, "heading": "PRICE", "justification": "R", "width": 10,
             "conversion": "MD2", "definition": "2^PRICE^R^10^^^^MD2"}]
]}
```

### SET.DICT

Create or replace one dictionary entry, named by `key`, from the attributes in
`structured_data`.

- Required: `account` (or a single-account client), `file`, `key`, `structured_data`.
- `structured_data` holds `field` (the 1-based attribute number, required), and optionally
  `heading` (defaults to `key`), `justification` (`"L"` or `"R"`, defaults to `"L"`),
  `width` (defaults to `10`) and `conversion`. Numbers may be sent as JSON numbers or as
  strings, which is what an HTML form has to hand.
- The rules are enforced here rather than by the caller: an entry with no attribute number is
  invisible to every query, and one with a justification `LIST` does not understand lays out
  wrongly — neither shows up until someone reads the file.
- `record` is the stored entry in `LIST.DICT`'s shape, so a caller sees the defaults that
  were filled in.
- `WRITE` with `is_dict` still stores a dictionary entry from a display string. `SET.DICT` is
  for a caller that has the attributes rather than the string, and wants them checked.
- To remove an entry, use `DELETE` with `is_dict: true`.
- Errors: `"Account not specified"`, `"File not specified"`, `"Key not specified"`,
  `"Dictionary attributes not specified"`, `"Attribute number not specified"`,
  `"Attribute number must be 1 or greater"`, `"Attribute number is not a whole number: <text>"`,
  `"Display width must be 1 or greater"`, `"Display width is not a whole number: <text>"`,
  `"Justification must be L or R"`, `"Table error: <detail>"`, access-denied.

```json
{"command": "SET.DICT", "account": "SALES", "file": "USERS", "key": "PRICE",
 "structured_data": {"field": 2, "heading": "PRICE", "justification": "R",
                     "width": 10, "conversion": "MD2"}}
```

```json
{"status": "OK", "record": {"field": 2, "heading": "PRICE", "justification": "R",
                            "width": 10, "conversion": "MD2",
                            "definition": "2^PRICE^R^10^^^^MD2"}}
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
