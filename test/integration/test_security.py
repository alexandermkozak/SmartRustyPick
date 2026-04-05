import socket
import ssl
import json
import time
import subprocess
import os
import shutil

def generate_certs():
    # Use -addext for modern SSL requirements
    subprocess.run("openssl genrsa -out ca.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -x509 -new -nodes -key ca.key -sha256 -days 365 -out ca.crt -subj '/CN=Test CA' -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl genrsa -out server.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key server.key -out server.csr -subj '/CN=localhost'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    
    # Client 1: ADMIN
    subprocess.run("openssl genrsa -out admin.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key admin.key -out admin.csr -subj '/CN=Admin Client'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in admin.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out admin.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    admin_thumbprint = subprocess.check_output("openssl x509 -in admin.crt -fingerprint -noout -sha256", shell=True).decode().split('=')[1].replace(':', '').strip().lower()

    # Client 2: USER (no admin)
    subprocess.run("openssl genrsa -out user.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key user.key -out user.csr -subj '/CN=User Client'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in user.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out user.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    user_thumbprint = subprocess.check_output("openssl x509 -in user.crt -fingerprint -noout -sha256", shell=True).decode().split('=')[1].replace(':', '').strip().lower()

    return admin_thumbprint, user_thumbprint

def run_request(port, request, certfile, keyfile, cafile):
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

def cleanup():
    for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "admin.key", "admin.csr", "admin.crt", "user.key", "user.csr", "user.crt", "config.toml"]:
        if os.path.exists(f): os.remove(f)
    if os.path.exists("db_storage_sec"):
        shutil.rmtree("db_storage_sec")

def log_result(test_name, status, message=""):
    with open("integration_results.md", "a") as f:
        f.write(f"| {test_name} | {status} | {message} |\n")

def test_security():
    cleanup()
    admin_tp, user_tp = generate_certs()
    print(f"Admin TP: {admin_tp}")
    print(f"User TP: {user_tp}")

    # Setup DB state
    os.makedirs("db_storage_sec/SYSTEM/$CLIENTS", exist_ok=True)
    # Admin client record: <tp>\xfe\xfeY
    with open("db_storage_sec/SYSTEM/$CLIENTS/data", "wb") as f:
        key = b"admin"
        data = admin_tp.encode() + b"\xfe\xfeY"
        f.write(len(key).to_bytes(8, 'little'))
        f.write(key)
        f.write(len(data).to_bytes(8, 'little'))
        f.write(data)
        
        key = b"user"
        data = user_tp.encode() + b"\xfeTEST_ACC\xfeN"
        f.write(len(key).to_bytes(8, 'little'))
        f.write(key)
        f.write(len(data).to_bytes(8, 'little'))
        f.write(data)

    os.makedirs("db_storage_sec/TEST_ACC", exist_ok=True)
    with open("db_storage_sec/accounts.reg", "wb") as f:
        key = b"registry"
        data = b"TEST_ACC\xfe" + os.path.abspath("db_storage_sec/TEST_ACC").encode()
        f.write(len(key).to_bytes(8, 'little'))
        f.write(key)
        f.write(len(data).to_bytes(8, 'little'))
        f.write(data)

    with open("config.toml", "w") as f:
        f.write('server_port = 9997\nserver_addr = "127.0.0.1"\ncert_path = "server.crt"\nkey_path = "server.key"\nca_path = "ca.crt"')

    if os.path.exists("db_storage"):
        shutil.move("db_storage", "db_storage_bak")
    shutil.move("db_storage_sec", "db_storage")

    proc = subprocess.Popen(["./target/debug/smart-rusty-pick-server"])
    time.sleep(2)

    try:
        # 1. User attempts to create an account (Should FAIL)
        print("Testing: User attempts CREATE.ACCOUNT...")
        req = {"command": "CREATE.ACCOUNT", "target_account": "EVIL_ACC"}
        resp = run_request(9997, req, "user.crt", "user.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "ERROR" and "Admin privileges required" in resp["message"]:
            log_result("Security: User CREATE.ACCOUNT", "Success", "Correctly blocked")
        else:
            log_result("Security: User CREATE.ACCOUNT", "Failure", f"Unexpected response: {resp}")
        assert resp["status"] == "ERROR"
        assert "Admin privileges required" in resp["message"]

        # 2. Admin attempts to create an account (Should SUCCEED)
        print("Testing: Admin attempts CREATE.ACCOUNT...")
        req = {"command": "CREATE.ACCOUNT", "target_account": "NEW_ACC"}
        resp = run_request(9997, req, "admin.crt", "admin.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "OK":
            log_result("Security: Admin CREATE.ACCOUNT", "Success", "Allowed")
        else:
            log_result("Security: Admin CREATE.ACCOUNT", "Failure", resp.get("message", "Error"))
        assert resp["status"] == "OK"

        # 3. User attempts to create a file (Should FAIL)
        print("Testing: User attempts CREATE.FILE...")
        req = {"command": "CREATE.FILE", "table": "EVIL_TABLE", "account": "TEST_ACC"}
        resp = run_request(9997, req, "user.crt", "user.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "ERROR" and "Admin privileges required" in resp["message"]:
            log_result("Security: User CREATE.FILE", "Success", "Correctly blocked")
        else:
            log_result("Security: User CREATE.FILE", "Failure", f"Unexpected response: {resp}")
        assert resp["status"] == "ERROR"
        assert "Admin privileges required" in resp["message"]

        # 4. Admin attempts to create a file (Should SUCCEED)
        print("Testing: Admin attempts CREATE.FILE...")
        req = {"command": "CREATE.FILE", "table": "GOOD_TABLE", "account": "TEST_ACC"}
        resp = run_request(9997, req, "admin.crt", "admin.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "OK":
            log_result("Security: Admin CREATE.FILE", "Success", "Allowed")
        else:
            log_result("Security: Admin CREATE.FILE", "Failure", resp.get("message", "Error"))
        assert resp["status"] == "OK"

        # 5. User attempts to authorize a new client (Should FAIL)
        print("Testing: User attempts AUTHORIZE.CONN...")
        req = {"command": "AUTHORIZE.CONN", "thumbprint": "1234", "name": "evil_client", "is_admin": True}
        resp = run_request(9997, req, "user.crt", "user.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "ERROR" and "Admin privileges required" in resp["message"]:
            log_result("Security: User AUTHORIZE.CONN", "Success", "Correctly blocked")
        else:
            log_result("Security: User AUTHORIZE.CONN", "Failure", f"Unexpected response: {resp}")
        assert resp["status"] == "ERROR"
        assert "Admin privileges required" in resp["message"]

        # 6. Admin attempts to authorize a new client (Should SUCCEED)
        print("Testing: Admin attempts AUTHORIZE.CONN...")
        req = {"command": "AUTHORIZE.CONN", "thumbprint": "5678", "name": "new_client", "accounts_list": ["TEST_ACC"]}
        resp = run_request(9997, req, "admin.crt", "admin.key", "ca.crt")
        print(f"Response: {resp}")
        if resp["status"] == "OK":
            log_result("Security: Admin AUTHORIZE.CONN", "Success", "Allowed")
        else:
            log_result("Security: Admin AUTHORIZE.CONN", "Failure", resp.get("message", "Error"))
        assert resp["status"] == "OK"

        print("\nSecurity verification PASSED!")

    finally:
        proc.terminate()
        proc.wait()
        shutil.rmtree("db_storage")
        if os.path.exists("db_storage_bak"):
            shutil.move("db_storage_bak", "db_storage")
        cleanup()

if __name__ == "__main__":
    test_security()
