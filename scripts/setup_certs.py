import socket
import ssl
import json
import subprocess
import os

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

if __name__ == "__main__":
    thumbprint = generate_certs()
    print(thumbprint)
