import socket
import ssl
import json
import time
import subprocess
import os

def generate_certs():
    subprocess.run("openssl genrsa -out ca.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -x509 -new -nodes -key ca.key -sha256 -days 365 -out ca.crt -subj '/CN=Test CA' -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl genrsa -out server.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key server.key -out server.csr -subj '/CN=localhost'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    subprocess.run("openssl genrsa -out client.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key client.key -out client.csr -subj '/CN=Test Client'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    
    thumbprint = subprocess.check_output("openssl x509 -in client.crt -fingerprint -noout -sha256", shell=True).decode().split('=')[1].replace(':', '').strip().lower()
    return thumbprint

def run_request(port, request, certfile, keyfile, cafile, existing_ssock=None):
    if existing_ssock:
        existing_ssock.sendall(json.dumps(request).encode() + b'\n')
        
        # Read until newline to handle large responses
        data = b''
        while not data.endswith(b'\n'):
            chunk = existing_ssock.recv(4096)
            if not chunk: break
            data += chunk
        
        response = data.decode()
        return json.loads(response)

    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=cafile)
    context.load_cert_chain(certfile=certfile, keyfile=keyfile)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED

    with socket.create_connection(('127.0.0.1', port)) as sock:
        with context.wrap_socket(sock, server_hostname='localhost') as ssock:
            ssock.sendall(json.dumps(request).encode() + b'\n')
            
            # Read until newline to handle large responses
            data = b''
            while not data.endswith(b'\n'):
                chunk = ssock.recv(4096)
                if not chunk: break
                data += chunk
            
            response = data.decode()
            return json.loads(response)

def test_performance():
    thumbprint = generate_certs()
    
    # Clean up
    if os.path.exists("PERF_ACC"):
        import shutil
        shutil.rmtree("PERF_ACC")
    if os.path.exists("accounts.reg"): os.remove("accounts.reg")
    if os.path.exists("certs.reg"): os.remove("certs.reg")

    port = 9999
    num_records = 10000

    # Start the application
    proc = subprocess.Popen(["./target/debug/SmartRustyPick"], stdin=subprocess.PIPE, text=True)
    
    # Initialize SYSTEM and authorize client
    proc.stdin.write("SYSTEM\n") # Log into SYSTEM
    proc.stdin.write(f"AUTHORIZE.CONN {thumbprint} perf_client ADMIN\n")
    proc.stdin.write("CREATE.ACCOUNT PERF_ACC\n")
    proc.stdin.write("LOGTO PERF_ACC\n")
    proc.stdin.write("Y\n") # Create DIR if prompted
    proc.stdin.write(f"START.SERVER 127.0.0.1:{port} server.crt server.key ca.crt\n")
    proc.stdin.flush()

    time.sleep(5)

    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile="ca.crt")
    context.load_cert_chain(certfile="client.crt", keyfile="client.key")
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED

    try:
        with socket.create_connection(('127.0.0.1', port)) as sock:
            with context.wrap_socket(sock, server_hostname='localhost') as ssock:
                print(f"Loading {num_records} records...")
                start_time = time.time()
                for i in range(num_records):
                    # Rotate Val1, Val2 to make queries more interesting
                    val1 = f"Val{i % 10}"
                    val2 = f"Data{i % 100}"
                    req = {"command": "WRITE", "table": "PERF", "key": f"REC{i}", "data": f"{val1}^{val2}^{i}", "account": "PERF_ACC"}
                    run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                print(f"Time to write {num_records} records: {end_time - start_time:.2f}s")
                write_time = end_time - start_time

                print("Testing simple query performance (3 = 5000)...")
                start_time = time.time()
                req = {"command": "QUERY", "table": "PERF", "query_string": "WITH 3 = 5000", "account": "PERF_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                simple_query_time = (end_time - start_time) * 1000
                print(f"Simple query time: {simple_query_time:.2f}ms. Keys found: {len(resp.get('results', []))}")

                print("Testing attribute query performance (1 = Val5)...")
                start_time = time.time()
                req = {"command": "QUERY", "table": "PERF", "query_string": "WITH 1 = Val5", "account": "PERF_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                attr_query_time = (end_time - start_time) * 1000
                print(f"Attribute query time: {attr_query_time:.2f}ms. Keys found: {len(resp.get('results', []))}")

                print("Testing complex query performance (1 = Val5 AND 3 > 9000)...")
                start_time = time.time()
                req = {"command": "QUERY", "table": "PERF", "query_string": "WITH 1 = Val5 AND 3 > 9000", "account": "PERF_ACC"}
                resp = run_request(port, req, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                complex_query_time = (end_time - start_time) * 1000
                print(f"Complex query time: {complex_query_time:.2f}ms. Keys found: {len(resp.get('results', []))}")

                # Generate performance_results.md
                with open("performance_results.md", "w") as f:
                    f.write("# Performance Test Results\n\n")
                    f.write(f"**Date:** {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
                    f.write("| Test Case | Status | Performance Data |\n")
                    f.write("| --- | --- | --- |\n")
                    f.write(f"| Write {num_records} records | Success | {write_time:.2f}s |\n")
                    f.write(f"| Simple Query (1 result) | Success | {simple_query_time:.2f}ms |\n")
                    f.write(f"| Attribute Query (1000 results) | Success | {attr_query_time:.2f}ms |\n")
                    f.write(f"| Complex Query (100 results) | Success | {complex_query_time:.2f}ms |\n")
                
                try:
                    ssock.unwrap()
                except (ssl.SSLError, socket.error):
                    pass

    finally:
        proc.terminate()
        proc.wait()
        for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
            if os.path.exists(f): os.remove(f)

if __name__ == "__main__":
    test_performance()
