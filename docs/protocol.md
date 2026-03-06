# Remote Connection Protocol

SmartRustyPick supports a TCP-based remote connection protocol using SSL/TLS with client certificate authentication.

## Authentication

Connections are secured via TLS 1.3 (or 1.2).
Clients **must** provide a certificate.
The server verifies the client certificate against its CA and then checks if the certificate's SHA-256 thumbprint (
hex-encoded) is authorized in the database.

Authorization is managed via the CLI:

- `AUTHORIZE.CONN <thumbprint>`
- `DEAUTHORIZE.CONN <thumbprint>`
- `LIST.CONNS`

## Message Format

The protocol uses line-delimited JSON messages. Each request must be a single line of JSON, and the server responds with
a single line of JSON.

### Request

```json
{
  "command": "READ" | "WRITE" | "DELETE" | "QUERY" | "READNEXT" | "GETLIST",
  "account": "ACCOUNT_NAME", (optional, switches context if provided)
  "table": "TABLE_NAME",
  "key": "RECORD_KEY",
  "data": "RECORD_DATA", (for WRITE, using display format: ^ for FM, ] for VM, \ for SVM)
  "is_dict": true | false, (optional, default: false)
  "query_string": "WITH First.Name = \"Ted\" AND Last.Name = \"Smith\"",
  (optional,
  alternative
  to
  'query_node'
  )
  "query_node": {
    ...
    structured
    QueryNode
    object
    ...
  },
  (optional,
  alternative
  to
  'query_string'
  )
  "list_name": "LIST_NAME", (optional, for QUERY, READNEXT, GETLIST)
  "batch_size": 10, (optional, for READNEXT)
}
```

### Response

```json
{
  "status": "OK" | "ERROR" | "NOT_FOUND" | "EOF",
  "message": "Error message if any",
  "record": "Returned record data for READ",
  "results": [["key1", "data1"], ["key2", "data2"]], (for QUERY without list_name)
  "keys": ["key1", "key2", ...], (for READNEXT, GETLIST)
  "count": 42 (for QUERY with list_name, READNEXT, GETLIST)
}
```

## Commands

### READ

Retrieves a record.

- Required fields: `table`, `key`.
- Optional fields: `is_dict`.

### WRITE

Stores a record.

- Required fields: `table`, `key`, `data`.
- Optional fields: `is_dict`.

### DELETE

Removes a record.

- Required fields: `table`, `key`.
- Optional fields: `is_dict`.

### QUERY

Performs a search.

- Required fields: `table`, `query_string` (or `query_node`).
- Optional fields: `is_dict`, `list_name`.
- If `list_name` is provided, the result keys are stored in a named select list on the server, and only the `count` is
  returned.

### READNEXT

Retrieves the next batch of keys from a named select list.

- Required fields: `list_name`.
- Optional fields: `batch_size` (default: 1).
- Returns `keys` and `count`.
- Returns `status: "EOF"` when the end of the list is reached.

### GETLIST

Retrieves all keys from a named select list.

- Required fields: `list_name`.
- Returns `keys` and `count`.

## Starting the Server

The server is started from the CLI:
`START.SERVER <addr:port> <cert_path> <key_path> <ca_path>`

Example:
`START.SERVER 0.0.0.0:8443 server.crt server.key ca.crt`
