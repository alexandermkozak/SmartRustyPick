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
            try:
                ssock.sendall(json.dumps(request).encode() + b'\n')
                response = ssock.recv(4096).decode()
                if not response: return None
                return json.loads(response)
            finally:
                # Cleanly shutdown SSL
                try:
                    ssock.shutdown(socket.SHUT_RDWR)
                    ssock.unwrap()
                except:
                    pass

def cleanup_system():
    # Clean up previous data
    if os.path.exists("db_storage/accounts.reg"): os.remove("db_storage/accounts.reg")
    if os.path.exists("db_storage/certs.reg"): os.remove("db_storage/certs.reg")
    if os.path.exists("db_storage/SYSTEM/$CLIENTS/data"): os.remove("db_storage/SYSTEM/$CLIENTS/data")
    if os.path.exists("db_storage/TEST_ACC"): shutil.rmtree("db_storage/TEST_ACC")
    for f in ["ca.key", "ca.crt", "ca.srl", "server.key", "server.csr", "server.crt", "client.key", "client.csr", "client.crt"]:
        if os.path.exists(f): os.remove(f)
    if os.path.exists("db_storage_test"):
        shutil.rmtree("db_storage_test")
    if os.path.exists("TEST_ACC_DIR"):
        shutil.rmtree("TEST_ACC_DIR")

def test_headless_and_cli_attachment():
    cleanup_system()
    thumbprint = generate_certs()
    print(f"Generated client thumbprint: {thumbprint}")

    os.makedirs("db_storage_test/SYSTEM/$CLIENTS", exist_ok=True)
    with open("db_storage_test/SYSTEM/$CLIENTS/dict", "wb") as f:
        pass
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
    # Field 0 (Names): Value { sub_values: ["TEST_ACC"] }
    # Field 1 (Dirs): Value { sub_values: [abspath] }
    # Length prefixed storage format: <key_len><key><data_len><data>
    # <key_len> is 8 bytes little endian (u64)
    registry_data = acc_name_data + b"\xfe" + acc_dir_data
    with open("db_storage_test/accounts.reg", "wb") as f:
        key = b"registry"
        f.write(len(key).to_bytes(8, 'little'))
        f.write(key)
        f.write(len(registry_data).to_bytes(8, 'little'))
        f.write(registry_data)

    # $CLIENTS record for test client (Thumbprint, Accounts, Admin)
    # Using Field 0 for Thumbprint, Field 1 for Accounts (empty), Field 2 for Admin (Y)
    # Data: <thumbprint>\xfe\xfeY
    client_rec_data = thumbprint.encode() + b"\xfe\xfeY"
    with open("db_storage_test/SYSTEM/$CLIENTS/data", "wb") as f:
        key = b"test_client"
        f.write(len(key).to_bytes(8, 'little'))
        f.write(key)
        f.write(len(client_rec_data).to_bytes(8, 'little'))
        f.write(client_rec_data)

    # Create config.toml (if it already exists, back it up)
    config_backup = None
    if os.path.exists("config.toml"):
        with open("config.toml", "r") as f:
            config_backup = f.read()

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
    if os.path.exists("db_storage_backup"):
        shutil.rmtree("db_storage_backup")
    if os.path.exists("db_storage"):
        shutil.move("db_storage", "db_storage_backup")
    shutil.move("db_storage_test", "db_storage")

    headless_proc = subprocess.Popen(["./target/debug/smart-rusty-pick-server"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    
    # Print server output in background (disabled for final submission to keep logs clean)
    # import threading
    # def log_server(pipe):
    #     for line in pipe:
    #         print(f"SERVER: {line.strip()}")
    # threading.Thread(target=log_server, args=(headless_proc.stdout,), daemon=True).start()
    # threading.Thread(target=log_server, args=(headless_proc.stderr,), daemon=True).start()

    try:
        time.sleep(5) # Wait for server to start
        
        # 1. Test Accessibility of Headless Server
        print("Testing accessibility of headless server...")
        # Headless server doesn't log into an account by default, so we might need to send a command that works or just check connection.
        # Actually, the server commands require being logged in for many things, but "READ" on a non-existent table might still return a protocol response.
        resp = run_request(9998, {"command": "READ", "table": "USERS", "key": "K1"}, "client.crt", "client.key", "ca.crt")
        print(f"Server response: {resp}")
        assert resp is not None
        # Should be "ERROR" with "Account not specified" message
        assert resp["status"] == "ERROR"
        assert "Account not specified" in resp["message"]

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
        cli_proc = subprocess.Popen(["../target/debug/smart-rusty-pick-cli"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        
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
            if os.path.exists("db_storage"):
                shutil.rmtree("db_storage")
            shutil.move("db_storage_backup", "db_storage")
        
        # Restore config.toml if we backed it up, otherwise remove our test one
        if config_backup is not None:
            with open("config.toml", "w") as f:
                f.write(config_backup)
        elif os.path.exists("config.toml"):
            os.remove("config.toml")

        cleanup_system()

if __name__ == "__main__":
    test_headless_and_cli_attachment()
