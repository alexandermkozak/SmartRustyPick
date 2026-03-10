from typing import Any, Dict, List, Optional
import socket
import ssl
import json
import os
import sys
from mcp.server.fastmcp import FastMCP

# Initialize FastMCP server
mcp = FastMCP("SmartRustyPick")

# Database connection details
# Defaults can be overridden by environment variables
DB_HOST = os.environ.get("DB_HOST", "127.0.0.1")
DB_PORT = int(os.environ.get("DB_PORT", "8443"))
CA_CERT = os.environ.get("DB_CA_CERT", "ca.crt")
CLIENT_CERT = os.environ.get("DB_CLIENT_CERT", "client.crt")
CLIENT_KEY = os.environ.get("DB_CLIENT_KEY", "client.key")

def _send_request(request: Dict[str, Any]) -> Dict[str, Any]:
    """Helper to send a JSON request to the database server via TLS."""
    try:
        context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=CA_CERT)
        context.load_cert_chain(certfile=CLIENT_CERT, keyfile=CLIENT_KEY)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_REQUIRED

        with socket.create_connection((DB_HOST, DB_PORT), timeout=10) as sock:
            with context.wrap_socket(sock, server_hostname='localhost') as ssock:
                ssock.sendall(json.dumps(request).encode() + b'\n')
                response_data = b""
                while True:
                    chunk = ssock.recv(4096)
                    if not chunk:
                        break
                    response_data += chunk
                    if b'\n' in response_data:
                        break
                
                if not response_data:
                    return {"status": "ERROR", "message": "No response from server"}
                
                return json.loads(response_data.decode().strip())
    except Exception as e:
        return {"status": "ERROR", "message": str(e)}

@mcp.tool()
def read_record(table: str, key: str, is_dict: bool = False, account: Optional[str] = None) -> str:
    """
    Read a record from the database.
    
    :param table: Name of the table to read from.
    :param key: Key of the record to retrieve.
    :param is_dict: Whether to treat the table as a dictionary table.
    :param account: Optional account name to switch context.
    """
    req = {
        "command": "READ",
        "table": table,
        "key": key,
        "is_dict": is_dict
    }
    if account:
        req["account"] = account
        
    resp = _send_request(req)
    if resp.get("status") == "OK":
        return resp.get("record", "")
    else:
        return f"Error: {resp.get('message', 'Unknown error')} (Status: {resp.get('status')})"

@mcp.tool()
def write_record(table: str, key: str, data: str, is_dict: bool = False, account: Optional[str] = None) -> str:
    """
    Write a record to the database.
    
    :param table: Name of the table to write to.
    :param key: Key of the record to store.
    :param data: Record data in Pick format (^ for FM, ] for VM, \\ for SVM).
    :param is_dict: Whether to treat the table as a dictionary table.
    :param account: Optional account name to switch context.
    """
    req = {
        "command": "WRITE",
        "table": table,
        "key": key,
        "data": data,
        "is_dict": is_dict
    }
    if account:
        req["account"] = account
        
    resp = _send_request(req)
    if resp.get("status") == "OK":
        return "Success: Record written."
    else:
        return f"Error: {resp.get('message', 'Unknown error')} (Status: {resp.get('status')})"

@mcp.tool()
def delete_record(table: str, key: str, is_dict: bool = False, account: Optional[str] = None) -> str:
    """
    Delete a record from the database.
    
    :param table: Name of the table to delete from.
    :param key: Key of the record to delete.
    :param is_dict: Whether to treat the table as a dictionary table.
    :param account: Optional account name to switch context.
    """
    req = {
        "command": "DELETE",
        "table": table,
        "key": key,
        "is_dict": is_dict
    }
    if account:
        req["account"] = account
        
    resp = _send_request(req)
    if resp.get("status") == "OK":
        return "Success: Record deleted."
    else:
        return f"Error: {resp.get('message', 'Unknown error')} (Status: {resp.get('status')})"

@mcp.tool()
def query_records(table: str, query_string: str, list_name: Optional[str] = None, is_dict: bool = False, account: Optional[str] = None) -> str:
    """
    Query records from the database using a query string.
    
    :param table: Name of the table to query.
    :param query_string: Pick-style query string (e.g., 'WITH First.Name = "John"').
    :param list_name: Optional name for a server-side select list. If provided, returns count only.
    :param is_dict: Whether to treat the table as a dictionary table.
    :param account: Optional account name to switch context.
    """
    req = {
        "command": "QUERY",
        "table": table,
        "query_string": query_string,
        "is_dict": is_dict
    }
    if list_name:
        req["list_name"] = list_name
    if account:
        req["account"] = account
        
    resp = _send_request(req)
    if resp.get("status") == "OK":
        if "results" in resp:
            return json.dumps(resp["results"], indent=2)
        elif "count" in resp:
            return f"Success: {resp['count']} records selected into list '{list_name}'."
        else:
            return "Success: Query completed."
    else:
        return f"Error: {resp.get('message', 'Unknown error')} (Status: {resp.get('status')})"

@mcp.tool()
def get_list_keys(list_name: str, account: Optional[str] = None) -> str:
    """
    Retrieve all keys from a named server-side select list.
    
    :param list_name: Name of the select list.
    :param account: Optional account name.
    """
    req = {
        "command": "GETLIST",
        "list_name": list_name
    }
    if account:
        req["account"] = account
        
    resp = _send_request(req)
    if resp.get("status") == "OK":
        return json.dumps(resp.get("keys", []), indent=2)
    else:
        return f"Error: {resp.get('message', 'Unknown error')} (Status: {resp.get('status')})"

if __name__ == "__main__":
    mcp.run()
