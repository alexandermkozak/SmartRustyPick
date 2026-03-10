# SmartRustyPick MCP Server

This MCP server provides a bridge between Model Context Protocol (MCP) and the SmartRustyPick database. It allows other
agents to interact with the database using standardized tools.

## Installation

Ensure you have the `mcp` Python package installed:

```bash
pip install mcp
```

## Configuration

The server connects to the SmartRustyPick database via TLS. You need to provide the following environment variables if
the defaults are not suitable:

- `DB_HOST`: Database server host (default: `127.0.0.1`)
- `DB_PORT`: Database server port (default: `8443`)
- `DB_CA_CERT`: Path to the CA certificate (default: `ca.crt`)
- `DB_CLIENT_CERT`: Path to the client certificate (default: `client.crt`)
- `DB_CLIENT_KEY`: Path to the client private key (default: `client.key`)

## Running the Server

You can run the server directly using Python:

```bash
python mcp/server.py
```

## Tools Provided

- `read_record(table, key, is_dict=False, account=None)`: Retrieves a record from the database.
- `write_record(table, key, data, is_dict=False, account=None)`: Stores a record in the database.
- `delete_record(table, key, is_dict=False, account=None)`: Deletes a record from the database.
- `query_records(table, query_string, list_name=None, is_dict=False, account=None)`: Performs a search.
- `get_list_keys(list_name, account=None)`: Retrieves keys from a named server-side select list.

## Database Integration

Before using the MCP server, ensure that:

1. The SmartRustyPick database server is running.
2. The client certificate used by this MCP server is authorized in the database (use `AUTHORIZE.CONN <thumbprint>` in
   the CLI).
