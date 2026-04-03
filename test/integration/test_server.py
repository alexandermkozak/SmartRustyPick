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
            ssock.sendall(json.dumps(request).encode() + b'\n')
            response = ssock.recv(4096).decode()
            if not response: return None
            return json.loads(response)

def test_integration():
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

    # Start the application
    proc = subprocess.Popen(["./target/debug/SmartRustyPick"], stdin=subprocess.PIPE, text=True)
    
    # Initialize SYSTEM and authorize client
    proc.stdin.write("SYSTEM\n") # Log into SYSTEM
    proc.stdin.write(f"AUTHORIZE.CONN {thumbprint} test_client ADMIN\n")
    proc.stdin.write("CREATE.ACCOUNT TEST_ACC\n")
    proc.stdin.write("LOGTO TEST_ACC\n")
    proc.stdin.write("Y\n") # Create DIR if prompted
    proc.stdin.write("CREATE.FILE USERS\n")
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
                assert resp["status"] == "OK"

                # 2. READ
                print("Testing READ...")
                req = {"command": "READ", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "OK"
                assert resp["record"] == "John^Doe^30"

                print("Testing QUERY...")
                # Use field 1 which corresponds to "John"
                # Since table USERS might not have a dictionary yet, it relies on numeric index
                req = {"command": "QUERY", "table": "USERS", "query_string": "WITH 1 = John", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                print(f"QUERY response: {resp}")
                assert resp["status"] == "OK"
                # Results is a list of [key, record_string]
                # Let's check what we got
                if not resp["results"]:
                    # Maybe it needs quotes?
                    req = {"command": "QUERY", "table": "USERS", "query_string": "WITH 1 = \"John\"", "account": "TEST_ACC"}
                    resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                    print(f"QUERY response with quotes: {resp}")
                
                keys = [item[0] for item in resp["results"]]
                assert "USER1" in keys

                # 4. SELECT LIST (QUERY with list_name)
                print("Testing SELECT LIST...")
                req = {"command": "QUERY", "table": "USERS", "query_string": "WITH 1 = John", "list_name": "MYLIST", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "OK"
                assert resp["count"] == 1

                # 5. READNEXT
                print("Testing READNEXT...")
                req = {"command": "READNEXT", "list_name": "MYLIST", "batch_size": 1, "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "OK"
                assert resp["keys"] == ["USER1"]

                # 6. DELETE
                print("Testing DELETE...")
                req = {"command": "DELETE", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "OK"

                # 7. READ (should fail)
                print("Testing READ (after DELETE)...")
                req = {"command": "READ", "table": "USERS", "key": "USER1", "account": "TEST_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                assert resp["status"] == "NOT_FOUND"

                print("Integration tests PASSED")
                try:
                    ssock.unwrap()
                except (ssl.SSLError, socket.error):
                    pass
    finally:
        proc.terminate()
        proc.wait()
        # Cleanup certs
        for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
            if os.path.exists(f): os.remove(f)

if __name__ == "__main__":
    test_integration()
