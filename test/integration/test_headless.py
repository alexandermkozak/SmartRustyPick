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
    subprocess.run("openssl genrsa -out client.key 2048", shell=True, check=True, capture_output=True)
    subprocess.run("openssl req -new -key client.key -out client.csr -subj '/CN=Test Client'", shell=True, check=True, capture_output=True)
    subprocess.run("openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 365 -sha256", shell=True, check=True, capture_output=True)
    
    thumbprint = subprocess.check_output("openssl x509 -in client.crt -fingerprint -noout -sha256", shell=True).decode().split('=')[1].replace(':', '').strip().lower()
    return thumbprint

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
            # Cleanly shutdown SSL
            try:
                ssock.unwrap()
            except ssl.SSLEOFError:
                pass
            return json.loads(response)

def cleanup():
    for f in ["config.toml", "ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
        if os.path.exists(f): os.remove(f)
    if os.path.exists("db_storage_test"):
        shutil.rmtree("db_storage_test")
    if os.path.exists("TEST_ACC_DIR"):
        shutil.rmtree("TEST_ACC_DIR")

def test_headless_and_cli_attachment():
    cleanup()
    thumbprint = generate_certs()
    print(f"Generated client thumbprint: {thumbprint}")

    os.makedirs("db_storage_test", exist_ok=True)
    os.makedirs("TEST_ACC_DIR", exist_ok=True)
    
    # Pre-create a valid DIR file to avoid prompt
    # In this app, tables are directories with 'data' and 'dict' files inside.
    os.makedirs("TEST_ACC_DIR/DIR", exist_ok=True)
    with open("TEST_ACC_DIR/DIR/data", "wb") as f:
        pass # Empty file is a valid section
    with open("TEST_ACC_DIR/DIR/dict", "wb") as f:
        pass
    
    # We also need to create some other table to make DIR prompt go away?
    # No, DIR existence should be enough.
    # WAIT! The app also checks available_tables during logto/init_available_in_dir.
    # It scans for DIRECTORIES.

    # Pre-configure the database with an account and authorized thumbprint
    # We do this by creating the registry files directly to avoid interactive setup
    # Accounts registry (Field 1: names, Field 2: dirs)
    # FM=254 (\xfe), VM=253 (\xfd), SVM=252 (\xfc)
    acc_name_data = b"TEST_ACC"
    acc_dir_data = os.path.abspath('TEST_ACC_DIR').encode()
    
    # Record structure for accounts.reg:
    # Field 1 (Names): Value { sub_values: ["TEST_ACC"] }
    # Field 2 (Dirs): Value { sub_values: [abspath] }
    registry_data = acc_name_data + b"\xfe" + acc_dir_data
    # Wait, the binary format uses bytes directly.
    # From db.rs:175: Record is mapped from registry file.
    # Record::to_bytes() puts FM (254) between fields.
    # But it also puts VM (253) between values and SVM (252) between sub-values.
    # Single name "TEST_ACC" in field 0, single dir in field 1.
    # Let's be precise.
    
    # Field 0: [Value{["TEST_ACC"]}] -> b"TEST_ACC"
    # Field 1: [Value{[abspath]}] -> b"abspath"
    # Length prefixed storage format: <key_len><key><data_len><data>
    with open("db_storage_test/accounts.reg", "wb") as f:
        key = b"registry"
        f.write(len(key).to_bytes(4, 'little'))
        f.write(key)
        f.write(len(registry_data).to_bytes(4, 'little'))
        f.write(registry_data)

    # Certs registry (Field 1: thumbprints)
    certs_data = thumbprint.encode()
    with open("db_storage_test/certs.reg", "wb") as f:
        key = b"certs"
        f.write(len(key).to_bytes(4, 'little'))
        f.write(key)
        f.write(len(certs_data).to_bytes(4, 'little'))
        f.write(certs_data)

    # Create config.toml
    config_content = f"""
server_port = 9998
server_addr = "127.0.0.1"
cert_path = "server.crt"
key_path = "server.key"
ca_path = "ca.crt"
"""
    with open("config.toml", "w") as f:
        f.write(config_content)

    print("Starting headless server...")
    # Override storage dir via environment if possible, or just use the default "db_storage"
    # Wait, the app uses "db_storage" hardcoded in main.rs. Let's rename ours.
    if os.path.exists("db_storage"):
        shutil.move("db_storage", "db_storage_backup")
    shutil.move("db_storage_test", "db_storage")

    headless_proc = subprocess.Popen(["./target/debug/SmartRustyPick", "--headless"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    
    try:
        time.sleep(2) # Wait for server to start
        
        # 1. Test Accessibility of Headless Server
        print("Testing accessibility of headless server...")
        # Headless server doesn't log into an account by default, so we might need to send a command that works or just check connection.
        # Actually, the server commands require being logged in for many things, but "READ" on a non-existent table might still return a protocol response.
        resp = run_request(9998, {"command": "READ", "table": "USERS", "key": "K1"}, "client.crt", "client.key", "ca.crt")
        print(f"Server response: {resp}")
        assert resp is not None
        # Should be "ERROR" with "Not logged into any account" message
        assert resp["status"] == "ERROR"
        assert "Not logged into any account" in resp["message"]

        # 2. Test CLI attachment
        print("Testing CLI attachment...")
        # Change to the account directory to test auto-login
        # But first, we need to make sure the CLI can find the database.
        # We'll create a symlink in TEST_ACC_DIR pointing back to db_storage.
        if os.path.exists("TEST_ACC_DIR/db_storage"):
            os.remove("TEST_ACC_DIR/db_storage")
        os.symlink("../db_storage", "TEST_ACC_DIR/db_storage")
    
        os.chdir("TEST_ACC_DIR")
        # Run CLI in a way that we can talk to it.
        # We need to go back up to run the binary.
        cli_proc = subprocess.Popen(["../target/debug/SmartRustyPick"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        
        # The CLI should auto-login and show "Database service attached and running in background."
        # We can send a command to it.
        cli_proc.stdin.write("CREATE.FILE USERS\n")
        cli_proc.stdin.write("SET USERS K1 Hello\n")
        cli_proc.stdin.write("SAVE\n")
        cli_proc.stdin.write("EXIT\n")
        cli_out, cli_err = cli_proc.communicate(timeout=10)
        
        print(f"CLI Output:\n{cli_out}")
        assert "Auto-logged into account 'TEST_ACC'" in cli_out
        # In current implementation, if another server is already running, 
        # the CLI process might fail to bind but continue as a client-only CLI 
        # (though it currently spawns the thread and ignores errors or just crashes the thread).
        # We check for OK to see if commands were processed.
        assert "OK" in cli_out
        
        print("Integration tests PASSED")
        
    finally:
        os.chdir("..")
        headless_proc.terminate()
        headless_proc.wait()
        if os.path.exists("db_storage_backup"):
            shutil.rmtree("db_storage")
            shutil.move("db_storage_backup", "db_storage")
        cleanup()

if __name__ == "__main__":
    test_headless_and_cli_attachment()
