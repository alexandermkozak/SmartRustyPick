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
        response = existing_ssock.recv(1024*1024).decode()
        return json.loads(response)

    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=cafile)
    context.load_cert_chain(certfile=certfile, keyfile=keyfile)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED

    with socket.create_connection(('127.0.0.1', port)) as sock:
        with context.wrap_socket(sock, server_hostname='localhost') as ssock:
            ssock.sendall(json.dumps(request).encode() + b'\n')
            response = ssock.recv(1024*1024).decode()
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
    num_records = 1000

    # proc = subprocess.Popen(["./target/debug/SmartRustyPick"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    proc = subprocess.Popen(["./target/debug/SmartRustyPick"], stdin=subprocess.PIPE, text=True)
    proc.stdin.write("PERF_ACC\nY\nY\n")
    proc.stdin.write(f"AUTHORIZE.CONN {thumbprint}\n")
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
                    run_request(port, {"command": "WRITE", "table": "PERF", "key": f"REC{i}", "data": f"Val1^Val2^{i}"}, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                print(f"Time to write {num_records} records: {end_time - start_time:.2f}s")

                print("Testing simple query performance...")
                start_time = time.time()
                resp = run_request(port, {"command": "QUERY", "table": "PERF", "query_string": "WITH 3 = 500"}, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                print(f"Simple query time: {(end_time - start_time)*1000:.2f}ms. Keys found: {len(resp.get('results', []))}")

                print("Testing complex query performance...")
                start_time = time.time()
                resp = run_request(port, {"command": "QUERY", "table": "PERF", "query_string": "WITH 1 = Val1 AND 3 > 900"}, "client.crt", "client.key", "ca.crt", existing_ssock=ssock)
                end_time = time.time()
                print(f"Complex query time: {(end_time - start_time)*1000:.2f}ms. Keys found: {len(resp.get('results', []))}")

    finally:
        proc.terminate()
        proc.wait()
        for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
            if os.path.exists(f): os.remove(f)

if __name__ == "__main__":
    test_performance()
