import socket
import ssl
import json
import time
import subprocess
import os

def generate_certs():
    # Use -addext for modern SSL requirements
    subprocess.run("openssl genrsa -out ca.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -x509 -new -nodes -key ca.key -sha256 -days 365 -out ca.crt -subj '/CN=Test CA' -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl genrsa -out server.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key server.key -out server.csr -subj '/CN=localhost'", shell=True, check=True, capture_output=True)
    # Use -extfile for SAN and basicConstraints during signing
    with open("server.ext", "w") as f:
        f.write("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nsubjectAltName = DNS:localhost, IP:127.0.0.1")
    subprocess.run("openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 365 -sha256 -extfile server.ext", shell=True, check=True, capture_output=True)
    os.remove("server.ext")
    
    subprocess.run("openssl genrsa -out client.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key client.key -out client.csr -subj '/CN=Test Client'", shell=True, check=True, capture_output=True)
    with open("client.ext", "w") as f:
        f.write("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature")
    subprocess.run("openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 365 -sha256 -extfile client.ext", shell=True, check=True, capture_output=True)
    os.remove("client.ext")
    
    thumbprint = subprocess.check_output("openssl x509 -in client.crt -fingerprint -noout -sha256", shell=True).decode().split('=')[1].replace(':', '').strip().lower()
    return thumbprint

def run_request(port, request, certfile, keyfile, cafile, existing_ssock=None):
    if existing_ssock:
        existing_ssock.sendall(json.dumps(request).encode() + b'\n')
        response = existing_ssock.recv(4096).decode()
        if not response: return None
        return json.loads(response)

    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=cafile)
    context.load_cert_chain(certfile=certfile, keyfile=keyfile)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED

    with socket.create_connection(('127.0.0.1', port)) as sock:
        with context.wrap_socket(sock, server_hostname='localhost') as ssock:
            try:
                ssock.sendall(json.dumps(request).encode() + b'\n')
                response = ssock.recv(4096).decode()
                if not response: return None
                return json.loads(response)
            finally:
                try:
                    ssock.shutdown(socket.SHUT_RDWR)
                    ssock.unwrap()
                except:
                    pass

def log_result(test_name, status, message=""):
    with open("integration_results.md", "a") as f:
        f.write(f"| {test_name} | {status} | {message} |\n")

def test_integration():
    # Initialize integration_results.md
    with open("integration_results.md", "w") as f:
        f.write("# Integration Test Results\n\n")
        f.write(f"**Date:** {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        f.write("| Test Name | Status | Details |\n")
        f.write("| --- | --- | --- |\n")

    thumbprint = generate_certs()
    print(f"Generated client thumbprint: {thumbprint}")

    # Clean up previous data
    if os.path.exists("TEST_ACC"):
        import shutil
        shutil.rmtree("TEST_ACC")
    if os.path.exists("accounts.reg"): os.remove("accounts.reg")
    if os.path.exists("certs.reg"): os.remove("certs.reg")
    if os.path.exists("db_storage/TEST_ACC"):
        import shutil
        shutil.rmtree("db_storage/TEST_ACC")
    if os.path.exists("db_storage/accounts.reg"): os.remove("db_storage/accounts.reg")
    if os.path.exists("db_storage/certs.reg"): os.remove("db_storage/certs.reg")
    if os.path.exists("db_storage/SYSTEM/$CLIENTS/data"): os.remove("db_storage/SYSTEM/$CLIENTS/data")

    # Start the application
    proc = subprocess.Popen(["./target/debug/smart-rusty-pick-cli", "--account", "SYSTEM"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    
    # Wait for initial prompt and handle auto-login if any
    time.sleep(2)
    
    # Authorize client thumbprint
    proc.stdin.write(f"AUTHORIZE.CONN {thumbprint} test_client ADMIN\n")
    proc.stdin.write("CREATE.ACCOUNT TEST_ACC\n")
    proc.stdin.write("LOGTO TEST_ACC\n")
    proc.stdin.write("Y\n") # Create DIR if prompted
    proc.stdin.write("CREATE.FILE USERS\n")
    # Create dictionary entry for field 1. Field 2 in DICT is the attribute number.
    # We found in models.rs: DICT_FIELD_IDX = 0.
    # Pick standard: F1 = D (Type), F2 = Attribute#
    # Our code: rec.fields[DICT_FIELD_IDX] should contain Attribute#
    proc.stdin.write("SET DICT USERS NAME 1\n")
    proc.stdin.write("SAVE\n")
    proc.stdin.write("LOGTO SYSTEM\n")
    proc.stdin.write("START.SERVER 127.0.0.1:9999 server.crt server.key ca.crt\n")
    proc.stdin.flush()

    time.sleep(5) # Wait for server to start

    port = 9999
    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile="ca.crt")
    context.load_cert_chain(certfile="client.crt", keyfile="client.key")
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED

    try:
        with socket.create_connection(('127.0.0.1', port)) as sock:
            with context.wrap_socket(sock, server_hostname='localhost') as ssock:
                # 1. WRITE
                print("Testing WRITE...")
                req = {"command": "WRITE", "table": "USERS", "key": "USER1", "data": "John^Doe^30", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                print(f"WRITE response: {resp}")
                if resp["status"] == "OK":
                    log_result("WRITE", "Success", "Record USER1 created")
                else:
                    log_result("WRITE", "Failure", resp.get("message", "Unknown error"))
                assert resp["status"] == "OK"

                # 2. READ
                print("Testing READ...")
                req = {"command": "READ", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                if resp["status"] == "OK" and resp["record"] == "John^Doe^30":
                    log_result("READ", "Success", "Record USER1 read correctly")
                else:
                    log_result("READ", "Failure", resp.get("message", "Data mismatch"))
                assert resp["status"] == "OK"
                assert resp["record"] == "John^Doe^30"

                # 3. QUERY
                print("Testing QUERY...")
                # Try querying by ID which doesn't need a dictionary
                req = {"command": "QUERY", "table": "USERS", "query_string": "WITH ID = USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                print(f"QUERY by ID response: {resp}")
                assert resp["status"] == "OK"
                keys = [item[0] for item in resp["results"]]
                if "USER1" in keys:
                    log_result("QUERY (by ID)", "Success", "Found USER1")
                else:
                    log_result("QUERY (by ID)", "Failure", "USER1 not found in results")
                assert "USER1" in keys

                # Try querying by NAME with the dictionary we set up
                req = {"command": "QUERY", "table": "USERS", "query_string": "WITH NAME = John", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                print(f"QUERY by NAME response: {resp}")
                assert resp["status"] == "OK"
                keys = [item[0] for item in resp["results"]]
                if "USER1" in keys:
                    log_result("QUERY (by NAME)", "Success", "Found USER1 by NAME")
                else:
                    log_result("QUERY (by NAME)", "Failure", "USER1 not found by NAME")
                assert "USER1" in keys

                # 4. SELECT (Create named list)
                print("Testing SELECT...")
                req = {"command": "SELECT", "table": "USERS", "query_string": "WITH NAME = John", "list_name": "MYLIST", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                if resp["status"] == "OK" and resp["count"] == 1:
                    log_result("SELECT", "Success", "Created MYLIST with 1 record")
                else:
                    log_result("SELECT", "Failure", resp.get("message", "Count mismatch"))
                assert resp["status"] == "OK"
                assert resp["count"] == 1

                # 5. GET.NEXT
                print("Testing GET.NEXT...")
                req = {"command": "GET.NEXT", "list_name": "MYLIST", "batch_size": 1, "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "OK"
                # Results is a list of [key, record_string]
                keys = [item[0] for item in resp["results"]]
                if keys == ["USER1"]:
                    log_result("GET.NEXT", "Success", "Retrieved USER1 from MYLIST")
                else:
                    log_result("GET.NEXT", "Failure", f"Got {keys} instead of USER1")
                assert keys == ["USER1"]

                # 6. DELETE
                print("Testing DELETE...")
                req = {"command": "DELETE", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                if resp["status"] == "OK":
                    log_result("DELETE", "Success", "Record USER1 deleted")
                else:
                    log_result("DELETE", "Failure", resp.get("message", "Delete failed"))
                assert resp["status"] == "OK"

                # 7. READ (should fail)
                print("Testing READ (after DELETE)...")
                req = {"command": "READ", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                print(f"READ after DELETE response: {resp}")
                if resp["status"] == "ERROR" and "Record not found" in resp["message"]:
                    log_result("READ (after DELETE)", "Success", "Confirmed record deleted")
                else:
                    log_result("READ (after DELETE)", "Failure", "Record still exists or unexpected error")
                assert resp["status"] == "ERROR"
                assert "Record not found" in resp["message"]

                print("Integration tests PASSED")
                try:
                    ssock.shutdown(socket.SHUT_RDWR)
                except:
                    pass
    finally:
        # Print stdout/stderr from CLI for debugging
        try:
            stdout, stderr = proc.communicate(timeout=2)
            print("--- CLI STDOUT ---")
            print(stdout)
            print("--- CLI STDERR ---")
            print(stderr)
        except:
            pass
        proc.terminate()
        proc.wait()
        # Cleanup certs
        for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
            if os.path.exists(f): os.remove(f)

if __name__ == "__main__":
    test_integration()
