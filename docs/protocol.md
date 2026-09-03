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
| `account`         | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `GET.NEXT`, `CREATE.FILE`, `SET.FILE`, `DELETE.FILE`, `LIST.FILES`, `FILE.STATS`, `LIST.DICT`, `SET.DICT`, `CREATE.INDEX`, `REBUILD.INDEX`, `DELETE.INDEX`, `LIST.INDEXES`, `INDEX.STATS`, `SET.INDEX.EXCLUDE` | Account context for the operation. If omitted and the client has exactly one allowed account, that account is used. An admin client with more than one possible account must send it. Access is denied if the account is not in the client's allowed list (admins may reach any account). |
| `target_account`  | string           | `CREATE.ACCOUNT`, `CREATE.TEST.ACCOUNT`, `DELETE.ACCOUNT`                                                          | Name of the account to create or drop. (Distinct from `account`, which selects an existing context.)                                                                                                                                                                                      |
| `file`            | string           | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`, `CREATE.FILE`, `SET.FILE`, `DELETE.FILE`, `FILE.STATS`, `LIST.DICT`, `SET.DICT`, `CREATE.INDEX`, `REBUILD.INDEX`, `DELETE.INDEX`, `INDEX.STATS`, `SET.INDEX.EXCLUDE` | Table (file) name. Optional on `LIST.INDEXES`, which lists the whole account without it. |                                                                                                                                                                                                                                                                        |
| `key`             | string           | `READ`, `WRITE`, `DELETE`, `SET.DICT`                                                                              | Record key; for `SET.DICT`, the name of the dictionary entry.                                                                                                                                                                                                                                                                               |
| `data`            | string \| object | `WRITE`                                                                                                            | Record contents. A string is parsed as a display-format record (`^` field mark, `]` value mark, `\` sub-value mark). An object maps field names — original dictionary names or their camelCase form — to values, applying the dictionary's input conversions (ICONV).                     |
| `structured_data` | object           | `WRITE`, `SET.DICT`                                                                                                | `WRITE`: same object form as `data`, checked first when present — use either this or `data`, not both. `SET.DICT`: the dictionary attributes of one entry.                                                                                                                                 |
| `is_dict`         | bool             | `READ`, `WRITE`, `DELETE`, `QUERY`, `SELECT`                                                                       | Operate on the file's dictionary section instead of its data section. Default `false`.                                                                                                                                                                                                    |
| `query_string`    | string           | `QUERY`, `SELECT`                                                                                                  | Pick-style query, e.g. `WITH NAME = "John" BY NAME`. Alternative to `query_node`. A bare command with neither selects every record; a `query_string` that is not a query is refused with `INVALID_QUERY` rather than read as one.                                                          |
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
| `field`           | string           | `CREATE.INDEX`, `REBUILD.INDEX`, `DELETE.INDEX`, `INDEX.STATS`, `SET.INDEX.EXCLUDE`                                | The dictionary field the index is on. Required by all of them. See [Storage Engine](storage.md#secondary-indexes).                                                                                                                                                                         |
| `values`          | array of strings | `CREATE.INDEX`, `SET.INDEX.EXCLUDE`                                                                                | Values the index is to skip. Optional on `CREATE.INDEX`. On `SET.INDEX.EXCLUDE` it replaces the set, and an absent or empty list clears it.                                                                                                                                                |
| `limit`           | number           | `INDEX.STATS`                                                                                                      | How many of the commonest values to return. Defaults to 10 and is clamped to 200, so one request cannot ask the server to sort and send every distinct value an index holds.                                                                                                               |

## Response object

Only `status` is always present. Every other field is **omitted from the JSON** unless the
command populates it — read an absent field as "not populated", the same as the `null` an
older server sent in its place.

| Field       | Type                      | Populated by                                                                                         | Notes                                                                                                                                                                                                            |
|-------------|---------------------------|------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `status`    | string                    | all                                                                                                  | `"OK"`, `"ERROR"` or `"EOF"`.                                                                                                                                                                                    |
| `message`   | string                    | errors                                                                                               | Human-readable error text; set whenever `status` is `"ERROR"`. For a person to read, not for a client to match on - the wording may change.                                                                       |
| `code`      | string                    | errors                                                                                               | The error's stable classification, set whenever `status` is `"ERROR"`. This is what a client branches on. See [Error codes](#error-codes).                                                                        |
| `record`    | object                    | `READ`, `CREATE.TEST.ACCOUNT`, `SET.FILE`, `FILE.STATS`, `SET.DICT`, `SERVER.STATS`, `GENERATE.CERT` | For `READ`, the record as field-name → display-formatted string (see [Record shape](#record-shape)). The management commands use it for their single result object, whose shape is documented with each command. |
| `results`   | array of `[key, record]`  | `QUERY`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`, `LIST.FILES`, `LIST.DICT`                        | Ordered `[string, object]` pairs. For `QUERY` and `GET.NEXT` each `record` has the same shape as `READ`; the management commands document their own.                                                             |
| `keys`      | array of strings          | `LIST.FILES`, `LIST.DICT`                                                                            | Plain list of names: the files in the account, or the file's dictionary entries. Both commands fill `results` as well, with what is known about each name.                                                       |
| `count`     | integer                   | `SELECT`, `GET.NEXT`, `LIST.CONNS`, `LIST.ACCOUNTS`, `LIST.FILES`, `LIST.DICT`                       | `SELECT`: number of keys selected into the list. `GET.NEXT`: number of records in the batch just returned. The list commands: number of entries returned.                                                        |
| `positions` | array of objects or nulls | `QUERY`, `GET.NEXT`                                                                                  | Present only for an exploded result. Index-aligned with `results`: the position within the exploded field that put each row there. See [Exploded results](#exploded-results).                                    |

There is no `NOT_FOUND` status. A missing record, table or list yields
`status: "ERROR"` with the `code` that says which, and a `message` that says it
in English:

```json
{"status": "ERROR", "code": "FILE_NOT_FOUND", "message": "Table 'ORDERS' not found in account 'SALES'"}
```

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

## Error codes

Every `status: "ERROR"` reply carries a `code` beside its `message`. The code is
the interface: it is what a client branches on, and it does not change. The
message is for a person, and its wording may change at any time - a client that
matches on it will break, and none of these codes needs it to.

This is the complete list. The command sections below name the codes each
command can answer with; `crates/core/src/server/protocol_doc_tests.rs` fails if
a code is added to the server and not written up here.

| Code                    | Means                                                                                            |
|-------------------------|--------------------------------------------------------------------------------------------------|
| `MISSING_FIELD`         | A field the command requires was absent or empty. The message names it.                          |
| `INVALID_DATA`          | A field was present but does not describe what it has to - a record, or a dictionary entry.       |
| `INVALID_QUERY`         | A `query_string` that is not a query. An absent one is not an error: it selects every record.      |
| `INVALID_JSON`          | The request line was not JSON. The connection stays open.                                         |
| `UNKNOWN_COMMAND`       | No command of that name.                                                                          |
| `ADMIN_REQUIRED`        | The command is admin only and this client's certificate is not.                                   |
| `ACCESS_DENIED`         | The account is not in the client's allowed list. Logged by the server.                            |
| `DEAUTHORIZED`          | The client's authorization was revoked while it was connected; the connection closes after this.  |
| `ACCOUNT_NOT_SPECIFIED` | The command works inside an account, none was named, and there is no single allowed one to use.   |
| `ACCOUNT_NOT_FOUND`     | No account of that name is registered.                                                            |
| `ACCOUNT_EXISTS`        | An account of that name is already registered; nothing was written.                               |
| `ACCOUNT_PROTECTED`     | `SYSTEM` holds the account registry and the authorized clients, so it cannot be dropped.          |
| `FILE_NOT_FOUND`        | The account has no file of that name.                                                             |
| `FILE_EXISTS`           | The account already has a file of that name; nothing was written.                                 |
| `RECORD_NOT_FOUND`      | The file holds no record under that key.                                                          |
| `SELECT_LIST_NOT_FOUND` | `GET.NEXT` named a list no `SELECT` has filled.                                                   |
| `CLIENT_NOT_FOUND`      | No client is authorized under that name.                                                          |
| `INDEX_NOT_FOUND`       | The file carries no index on that field.                                                          |
| `INDEX_EXISTS`          | The file already carries an index on that field.                                                  |
| `INVALID_FIELD`         | The field cannot carry an index. The message says why.                                            |
| `INVALID_REQUEST`       | Understood and refused: the database will not do this. The message says why.                      |
| `CORRUPT_DATA`          | What is on disk does not decode. The file needs repair; retrying will not help.                   |
| `PERMISSION_DENIED`     | The server may not touch a file or directory it needs. An operator problem, not the client's.     |
| `IO_ERROR`              | Any other I/O failure - a full disk, a short write. Worth retrying, unlike the two above.         |
| `UNAVAILABLE`           | The server cannot answer this command as it is currently configured.                              |

A code this list does not name may appear in a future version, so read an
unrecognised one the way you would read a missing one: the request failed, and
the `message` says how. Every command may answer with `CORRUPT_DATA`,
`PERMISSION_DENIED` or `IO_ERROR` if the storage underneath it fails, so those
three are not repeated in the per-command lists below.

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
| `CREATE.INDEX`          |  yes  |   yes   | `account`, `file`, `field`, `values`                 | `record`                                |
| `REBUILD.INDEX`         |  yes  |   yes   | `account`, `file`, `field`                           | `record`                                |
| `DELETE.INDEX`          |  yes  |   yes   | `account`, `file`, `field`                           | `status: "OK"`                          |
| `LIST.INDEXES`          |       |   yes   | `account`, `file` (optional)                         | `keys` + `results` + `count`            |
| `INDEX.STATS`           |       |   yes   | `account`, `file`, `field`, `limit`                  | `record`                                |
| `SET.INDEX.EXCLUDE`     |  yes  |   yes   | `account`, `file`, `field`, `values`                 | `record`                                |
| `SERVER.STATS`          |  yes  |    —    | —                                                    | `record`                                |

¹ `GET.NEXT` resolves its account from the select list created by `SELECT`, so it does not
need `account` on the request itself.

### READ

Retrieve one record.

- Required: `file`, `key`. Optional: `account`, `is_dict`.
- Errors: `MISSING_FIELD` (no `file` or `key`), `ACCOUNT_NOT_SPECIFIED`, `ACCESS_DENIED`,
  `FILE_NOT_FOUND`, `RECORD_NOT_FOUND`.

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
- Errors: `MISSING_FIELD` (no `file`, `key` or data), `INVALID_DATA` (data that is not a
  record), `ACCOUNT_NOT_SPECIFIED`, `ACCESS_DENIED`, `FILE_NOT_FOUND`.

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
- Errors: `MISSING_FIELD` (no `file` or `key`), `ACCOUNT_NOT_SPECIFIED`, `ACCESS_DENIED`,
  `FILE_NOT_FOUND`.

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
- Errors: `MISSING_FIELD` (no `file`), `INVALID_QUERY`, `ACCOUNT_NOT_SPECIFIED`,
  `ACCESS_DENIED`, `FILE_NOT_FOUND`.

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
- Errors: `MISSING_FIELD` (no `file`), `INVALID_QUERY`, `ACCOUNT_NOT_SPECIFIED`,
  `ACCESS_DENIED`, `FILE_NOT_FOUND`.

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
- Errors: `SELECT_LIST_NOT_FOUND`, `FILE_NOT_FOUND`.

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `target_account`), `ACCOUNT_EXISTS`,
  `ACCOUNT_NOT_FOUND` (dropping one that is not there), `ACCOUNT_PROTECTED` (dropping
  `SYSTEM`).

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `target_account`), `ACCOUNT_EXISTS`.

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
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`),
  `FILE_EXISTS`.

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
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`, or no
  `durable` - an absent flag is refused rather than read as a demotion), `FILE_NOT_FOUND`,
  `INVALID_REQUEST` (naming `DIR`).

```json
{"command": "SET.FILE", "account": "SALES", "file": "LEDGER", "durable": true}
```

```json
{"status": "OK", "record": {"account": "SALES", "name": "LEDGER", "durable": true}}
```

### DELETE.FILE — admin

Drop a table from `account`.

- Required: `account`, `file`. Admin only.
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`),
  `FILE_NOT_FOUND`.

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `thumbprint` or `name`).

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `name`), `CLIENT_NOT_FOUND`.

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `name`). The message of a failure part way
  through names the account it stopped on.

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
- Errors: `ADMIN_REQUIRED`, `MISSING_FIELD` (no `name`), `INVALID_REQUEST` (a non-admin
  certificate with no allowed account), `INVALID_DATA` (a common name that is not
  `[A-Za-z0-9._-]`), `UNAVAILABLE` (the server has no certificate configuration to sign
  with).

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
- Errors: `ADMIN_REQUIRED`.

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
                        "file_count": 3, "record_count": 1280, "disk_bytes": 262144,
                        "index_count": 4, "stale_indexes": 1, "unhealthy_files": 1,
                        "health": {"verdict": "act",
                                   "reasons": ["1 of 3 files need attention"]}}]]}
```

`index_count` and `stale_indexes` count the [secondary indexes](storage.md#secondary-indexes)
across every file in the account, and `unhealthy_files` how many files the cheap check
reports as anything but `good`. `health` is the worst of those file verdicts, in the summary
form [Health](#health-verdicts-and-measures) describes — enough to know an account is worth
opening, and no more.

### LIST.FILES

The files in one account, sorted.

- Required: `account` (or a client with exactly one allowed account).
- `keys` is the plain list of names. `results` pairs each name with what is known about the
  file beside its name: `durable`, so a client can see which files flush every write without
  reading the account's `DIR` file, and a [health](#health-verdicts-and-measures) verdict, so
  a problem file can be found without opening every file in turn. A database running with
  `durable_writes = true` reports every file as durable, because every write then is.
- `health` here is the *cheap* verdict — one of `good`, `watch` or `act`, derived from the
  section metadata and the index `state` files alone. It reads no group trailer and no
  record, because a listing must not cost what opening a file costs. `health_reasons` names
  what is wrong in short phrases, and is `[]` when nothing is. The full measures, including
  everything that needs the group distribution, arrive with `FILE.STATS`.
- Errors: `ACCOUNT_NOT_SPECIFIED`, `ACCESS_DENIED`.

```json
{"command": "LIST.FILES", "account": "SALES"}
```

```json
{"status": "OK", "count": 3, "keys": ["DIR", "LEDGER", "USERS"], "results": [
  ["DIR", {"durable": false, "health": "good", "health_reasons": []}],
  ["LEDGER", {"durable": true, "health": "good", "health_reasons": []}],
  ["USERS", {"durable": false, "health": "act",
             "health_reasons": ["1 of 2 indexes stale"]}]
]}
```

### Health: verdicts and measures

Several replies carry a `health` object rather than leaving a reader to decide whether a
number is bad. The verdicts are decided by the server, in one place, so the CLI, this
protocol and the web dashboard cannot disagree about where the line is.

A `health` object has two fields:

| Field      | Type             | Meaning                                                 |
|------------|------------------|---------------------------------------------------------|
| `verdict`  | string           | The worst verdict among `measures`.                     |
| `measures` | array of objects | One entry per thing measured, in a stable order.        |

One measure:

| Field       | Type   | Meaning                                                                    |
|-------------|--------|----------------------------------------------------------------------------|
| `id`        | string | Stable identifier — `skew`, `load_factor`, `dominant_value` and so on.     |
| `label`     | string | Short name for a person.                                                   |
| `value`     | string | The measurement, already formatted. `"—"` when there is nothing to report. |
| `verdict`   | string | `good`, `watch` or `act`.                                                  |
| `threshold` | string | The rule that produced the verdict, so a reader can argue with it.         |
| `detail`    | string | What it means, and for anything but `good`, what to do about it.           |

The three verdicts:

- `good` — nothing to do.
- `watch` — not wrong now, and heading somewhere: a file about to rehash, an index nothing
  has queried yet.
- `act` — something is costing more than it should, and `detail` says what to do.

**A client branches on `id` and `verdict`, never on `label`, `threshold` or `detail`.** Those
are prose for a person and may be reworded; the identifiers and the verdicts are the
interface, exactly as an [error code](#error-codes) is and its message is not. A measure that
cannot be judged yet — skew over four records, usage on a server that started a second ago —
reports `good` and says so in its `detail` rather than inventing a verdict.

`LIST.FILES` and `LIST.ACCOUNTS` carry a cheaper form instead: a `verdict` string and a
`reasons` array of short phrases, with no measures behind them.

### FILE.STATS

Describe one file: how many records it holds, how they are spread across hash groups, what it costs on disk — and
whether any of that is a problem. No record is returned, and none is read to answer it unless the file is still in the
pre-hashfile flat format.

- Required: `account` (or a single-account client), `file`.
- Errors: `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`), `ACCESS_DENIED`,
  `FILE_NOT_FOUND`.

```json
{"command": "FILE.STATS", "account": "SALES", "file": "USERS"}
```

```json
{"status": "OK", "record": {
  "account": "SALES", "name": "USERS",
  "record_count": 1280, "dict_count": 4,
  "modulus": 128, "version": 42,
  "group_count": 128, "smallest_group_bytes": 96, "largest_group_bytes": 512,
  "disk_bytes": 262144, "group_bytes": 212992, "index_bytes": 20480,
  "checksums": true, "legacy": false,
  "durable": false, "loaded": true, "modified_seconds_ago": 12,
  "records_per_group_target": 16, "load_factor": 0.625,
  "records_until_growth": 769, "records_until_shrink": 768,
  "largest_group_share": 0.021, "skew": 2.7,
  "group_records": {
    "groups": 128,
    "min": 3, "max": 27, "mean": 10.0, "median": 10,
    "empty": 0, "overweight": 4, "unreadable": 0,
    "buckets": [{"min": 3, "max": 4, "groups": 6},
                {"min": 5, "max": 6, "groups": 21},
                {"min": 25, "max": 27, "groups": 1}]
  },
  "health": {"verdict": "good", "measures": [
    {"id": "skew", "label": "Group skew", "value": "2.7x", "verdict": "good",
     "threshold": "watch above 3x the mean group, act above 6x; not judged below 4 records per group",
     "detail": "Records are spread evenly: the largest group holds 27 against a mean of 10."}
  ]},
  "indexes": [
    {"file": "USERS", "field": "CITY", "attribute": 2, "values": 64, "postings": 1280,
     "largest_postings": 41, "modulus": 8, "version": 7, "group_count": 8,
     "disk_bytes": 20480, "data_version": 42, "stale": false, "loaded": true,
     "built_seconds_ago": 12, "excluded": [],
     "usage": {"lookups": 0, "candidates": 0, "matched": 0,
               "measured_lookups": 0, "excluded_lookups": 0},
     "health": {"verdict": "watch", "measures": []}}
  ]
}}
```

`indexes` describes the file's [secondary indexes](storage.md#secondary-indexes), in field
order, with the same objects `LIST.INDEXES` returns. It is `[]` for a file that has none. The
worst index verdict is rolled into the file's own `health`, so a badly shaped index is
visible from the file rather than only from the index table.

**Bytes.** `disk_bytes` is the whole file directory. `group_bytes` is the record groups alone
and `index_bytes` the index sections, so the remainder is the dictionary and the small
metadata files. There is no "wasted space" figure because there is no waste to report: a
group is rewritten whole from its live entries, so a deleted record leaves nothing behind
inside one.

**Layout and headroom.** `records_per_group_target` is the records per group the modulus aims
for, and `load_factor` is `record_count / (modulus * records_per_group_target)`. Past 1.0 the
next flush picks a larger modulus, which rewrites *every* group —
`records_until_growth` says how far away that is, and `records_until_shrink` how far the
other way, or `null` when the modulus is already at its floor and will not shrink.

**Distribution.** `group_records` is how the records are spread over the groups, read from
each group's 20-byte trailer rather than by loading anything — one seek per group, no record.
It is over `groups`, which is the **modulus** and not `group_count`: a group holding nothing
has no file at all, so averaging the files that exist would report a file whose records had
piled into four groups out of thirty-two as perfectly even. `empty` counts the groups holding
nothing, file or no file.
`skew` is `max / mean`, which is scale-free and so readable on a file of any size;
`largest_group_share` is the same extreme against the whole file. `overweight` counts the
groups above twice the mean, which says something one extreme cannot: that the hash itself is
not spreading rather than that one group is unlucky. `unreadable` counts groups written
before the format appended a trailer, whose counts are left out of every other figure rather
than folded in as zero. `buckets` is the shape, as at most sixteen equal-width columns over
the record counts, so a file with a modulus of 65,536 still answers in a small reply.

`health` is described under [Health](#health-verdicts-and-measures).

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
- Errors: `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`), `ACCESS_DENIED`,
  `FILE_NOT_FOUND`.

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
- Errors: `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file`, `key` or
  `structured_data`), `INVALID_DATA` (attributes that do not describe an entry - the message
  names the attribute and what is wrong with it), `ACCESS_DENIED`, `FILE_NOT_FOUND`.

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

### CREATE.INDEX / REBUILD.INDEX — admin

Build a [secondary index](storage.md#secondary-indexes) on a dictionary field, so
`WITH <field> = <value>` resolves through the index instead of scanning the file.
`REBUILD.INDEX` derives an existing one from the records again — the repair for an index
reported as `stale`, and the way to bring one back after its section has been damaged.

- Required: `account` (or a single-account client), `file`, `field`.
- Optional on `CREATE.INDEX`: `values`, the values the index is not to hold. See
  [SET.INDEX.EXCLUDE](#setindexexclude--admin) for what that is for and what it costs.
- Both are one pass over the file's records, which is the only cost an index has that grows
  with the file. Maintaining it afterwards rides the ordinary write path.
- `record` is the index as `LIST.INDEXES` describes it, read back after the build rather
  than echoed from the request.
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file` or
  `field`), `FILE_NOT_FOUND`, `INDEX_EXISTS` (`CREATE.INDEX` on a field already indexed),
  `INDEX_NOT_FOUND` (`REBUILD.INDEX` on one that is not), `INVALID_FIELD` for a field that
  is not in the file's dictionary, is `ID` (the record key, already found without a scan),
  or whose name cannot become a directory.

```json
{"command": "CREATE.INDEX", "account": "SALES", "file": "USERS", "field": "CITY"}
```

```json
{"command": "CREATE.INDEX", "account": "SALES", "file": "ORDERS", "field": "STATUS",
 "values": ["ACTIVE", ""]}
```

```json
{"status": "OK", "record": {
  "file": "USERS", "field": "CITY", "attribute": 2,
  "values": 64, "postings": 1280, "largest_postings": 41,
  "modulus": 8, "version": 1, "group_count": 8, "disk_bytes": 20480,
  "data_version": 42, "stale": false, "loaded": true, "built_seconds_ago": 0,
  "excluded": [],
  "usage": {"lookups": 0, "candidates": 0, "matched": 0,
            "measured_lookups": 0, "excluded_lookups": 0},
  "health": {"verdict": "good", "measures": []}
}}
```

### DELETE.INDEX — admin

Drop an index and remove its section from disk. The file's records are untouched, and
queries that were using it go back to scanning.

- Required: `account` (or a single-account client), `file`, `field`.
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file` or
  `field`), `FILE_NOT_FOUND`, `INDEX_NOT_FOUND`.

```json
{"command": "DELETE.INDEX", "account": "SALES", "file": "USERS", "field": "CITY"}
```

### LIST.INDEXES

Every index of one file — or, with no `file`, every index in the account, so index health is
visible without walking file by file.

- Required: `account` (or a single-account client). `file` is optional.
- With a `file`, `keys` is the plain list of indexed fields. Without one, each key is
  `<file>/<field>`, so two files indexing the same field name are still two rows. Either way
  every entry names its own `file`, and a client renders one table for both.
- `values` against the file's `record_count` is how selective the field is; `postings` is
  the total (value, key) pairs, which is what maintaining the index costs per write; and
  `largest_postings` is the skew the average hides — an index whose biggest value covers
  half the file saves nothing on that value. `health` turns all of that into a verdict; see
  [Health](#health-verdicts-and-measures).
- `stale` means the index does not match the data and has to be rebuilt before it is used.
  Loading a file rebuilds a stale index as it opens, so this is normally only ever seen for
  a file that is not in memory. `data_version` is the data section version the index matches;
  `version` is the index section's own flush counter.
- `excluded` lists the values this index deliberately does not hold. See
  [SET.INDEX.EXCLUDE](#setindexexclude--admin).
- `usage` is what the read path has actually asked of this index **since the server
  started**, and is never persisted:
  - `lookups` — lookups it answered. Zero is the clearest possible signal that an index is
    pure cost, since it is maintained on every write to its field whether or not anything
    queries it. Read it against how long the server has been up.
  - `candidates` — record keys those lookups handed to the filter behind them.
  - `matched` — how many of those survived the filter, over `measured_lookups`.
  - `measured_lookups` — lookups whose survivors could be attributed to this index, which is
    a query one index resolved on its own. Once an `AND` intersects two indexes there is no
    honest way to say which of them a surviving record is owed to, so such a query counts in
    `lookups` and `candidates` and not here.
  - `excluded_lookups` — lookups that fell back to a scan because the value asked for is
    excluded.
- Read from memory when the file is loaded, so an index changed but not yet flushed is
  described as it now is. For a file that is not loaded the index sections are read instead —
  they hold values and keys rather than record bodies — and `usage` is all zeroes, because
  the counters live on the in-memory index.
- Errors: `ACCOUNT_NOT_SPECIFIED`, `ACCESS_DENIED`, `FILE_NOT_FOUND`, `ACCOUNT_NOT_FOUND`
  (the account-wide form).

```json
{"command": "LIST.INDEXES", "account": "SALES", "file": "USERS"}
```

```json
{"status": "OK", "count": 1, "keys": ["CITY"], "results": [
  ["CITY", {"file": "USERS", "field": "CITY", "attribute": 2, "values": 64, "postings": 1280,
            "largest_postings": 41, "modulus": 8, "version": 7, "group_count": 8,
            "disk_bytes": 20480, "data_version": 42, "stale": false, "loaded": true,
            "built_seconds_ago": 12, "excluded": [],
            "usage": {"lookups": 812, "candidates": 16240, "matched": 15980,
                      "measured_lookups": 800, "excluded_lookups": 0},
            "health": {"verdict": "good", "measures": []}}]
]}
```

Every index in the account, keyed by file and field:

```json
{"command": "LIST.INDEXES", "account": "SALES"}
```

```json
{"status": "OK", "count": 2, "keys": ["ORDERS/STATUS", "USERS/CITY"], "results": [
  ["ORDERS/STATUS", {"file": "ORDERS", "field": "STATUS", "...": "..."}],
  ["USERS/CITY", {"file": "USERS", "field": "CITY", "...": "..."}]
]}
```

### INDEX.STATS

One index in full: its statistics, its verdicts, and the values that dominate it.

- Required: `account` (or a single-account client), `file`, `field`. Optional: `limit`.
- Its own command rather than a wider `LIST.INDEXES` because it costs more: `LIST.INDEXES` is
  a per-file listing read on every navigation and should stay cheap, while this sorts the
  index's values and is asked for deliberately.
- `top_values` is the `limit` values holding the most record keys, largest first, ties broken
  on the value so the same index always reports the same list. `limit` defaults to 10 and is
  clamped to 200.
- This is what turns "this index is skewed" into "`STATUS = ACTIVE` is 91% of it", which is
  what makes the diagnosis actionable: the value it names is the one to hand to
  [SET.INDEX.EXCLUDE](#setindexexclude--admin).
- `values_available` is false when the values could not be read — a stale index, whose
  postings do not describe the records, or a section that would not load. `top_values` is
  then `[]`, which is not the same as an empty index and is why the flag is there.
- No record is read: an index holds values and record keys, never record bodies.
- Errors: `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file` or `field`), `ACCESS_DENIED`,
  `FILE_NOT_FOUND`, `INDEX_NOT_FOUND`.

```json
{"command": "INDEX.STATS", "account": "SALES", "file": "ORDERS", "field": "STATUS",
 "limit": 5}
```

```json
{"status": "OK", "record": {
  "record_count": 1280, "values_available": true,
  "index": {"file": "ORDERS", "field": "STATUS", "attribute": 4, "...": "..."},
  "top_values": [
    {"value": "ACTIVE", "keys": 1164},
    {"value": "PENDING", "keys": 71},
    {"value": "CANCELLED", "keys": 33},
    {"value": "", "keys": 12}
  ]
}}
```

### SET.INDEX.EXCLUDE — admin

Replace the values one index deliberately does not hold.

The remedy between leaving an index alone and dropping it. Take the common real shape: a
field where 90% of records carry one value and the remaining 10% are spread over hundreds.
That field is *excellent* to index — for the 10%. Indexing the dominant value buys nothing,
because the lookup hands the scan behind it most of the file and the scan does that work
anyway, and it costs the most, because it is the longest posting list and so the entry
rewritten most expensively on every write that touches it. `"index everything except the
empty string"` is the other common spelling: a sparse field most records simply do not carry.

- Required: `account` (or a single-account client), `file`, `field`. Optional: `values`.
- `values` **replaces** the set. An absent or empty list clears the exclusions, since
  replacing the set is the whole command.
- Changing the set rebuilds the index, exactly as moving its field to another attribute does:
  the index no longer holds what it says it holds. The set is stored in the index section's
  `state` file, so it survives a restart and is part of what a staleness check compares.
- Values are compared after trimming, which is what a query does to a value before testing
  it, so `"ACTIVE"` and `" ACTIVE "` are the same exclusion.
- **A query for an excluded value returns exactly what it returned without the index.** The
  planner is told "I cannot help, scan for it" rather than being handed an empty posting list
  it would read as "no records" — and the same applies inside `AND` and `OR`: an excluded
  side is an unknown side, not an empty one. That is sound because the index only ever
  narrows and the evaluation behind it decides, so "I do not know" was already an answer the
  planner handled.
- There is no automatic variant that skips any value over some share of the file. It would
  make an index's contents depend on the data distribution at build time, so the same command
  would produce different indexes on different days, and a value could silently cross the
  threshold as the file grew. The exclusions stay explicit; `INDEX.STATS` suggests them.
- `record` is the index as `LIST.INDEXES` describes it, read back after the rebuild.
- Errors: `ADMIN_REQUIRED`, `ACCOUNT_NOT_SPECIFIED`, `MISSING_FIELD` (no `file` or `field`),
  `ACCESS_DENIED`, `FILE_NOT_FOUND`, `INDEX_NOT_FOUND`.

```json
{"command": "SET.INDEX.EXCLUDE", "account": "SALES", "file": "ORDERS",
 "field": "STATUS", "values": ["ACTIVE", ""]}
```

```json
{"status": "OK", "record": {
  "file": "ORDERS", "field": "STATUS", "values": 104, "postings": 104,
  "largest_postings": 71, "excluded": ["", "ACTIVE"], "stale": false, "...": "..."
}}
```

### SERVER.STATS — admin

The running server: how long it has been up, what it has served and which sessions are open right now.

- Required: nothing. Admin only.
- `active_connections` lists the sessions holding a TLS connection at this instant, the caller's own included. Totals
  are counted since the process started.
- Errors: `ADMIN_REQUIRED`.

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
{"status": "ERROR", "code": "UNKNOWN_COMMAND", "message": "Unknown command"}
```

Malformed JSON yields `{"status": "ERROR", "code": "INVALID_JSON", "message": "Invalid JSON:
<detail>"}` and the connection stays open.

A client whose authorization is revoked while it is connected gets
`{"status": "ERROR", "code": "DEAUTHORIZED", "message": "Client deauthorized"}` in answer to
its next request, and the connection then closes.

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
