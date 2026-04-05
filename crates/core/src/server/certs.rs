use crate::config::Config;
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::fs::File;
use std::io::{self, BufReader as SyncBufReader};
use std::path::Path;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub fn ensure_certificates(config: &Config) -> std::io::Result<()> {
    let cert_path = config.cert_path.as_ref().expect("cert_path missing");
    let key_path = config.key_path.as_ref().expect("key_path missing");
    let ca_path = config.ca_path.as_ref().expect("ca_path missing");
    let ca_key_path = "ca.key"; // Private key for CA

    let cert_exists = Path::new(cert_path).exists();
    let key_exists = Path::new(key_path).exists();
    let ca_exists = Path::new(ca_path).exists();

    if cert_exists && key_exists && ca_exists {
        return Ok(());
    }

    println!("Generating certificates for first-time startup...");

    // 1. Generate CA key and certificate if needed
    if !Path::new(ca_key_path).exists() || !ca_exists {
        println!("Generating CA certificate...");
        let status = std::process::Command::new("openssl")
            .args(&[
                "req", "-new", "-x509", "-days", "3650",
                "-nodes",
                "-newkey", "rsa:2048",
                "-keyout", ca_key_path,
                "-out", ca_path,
                "-subj", "/CN=SmartRustyPick Root CA",
                "-addext", "basicConstraints=critical,CA:TRUE",
                "-addext", "keyUsage=critical,keyCertSign,cRLSign"
            ])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to generate CA certificate"));
        }
    }

    // 2. Generate server key and CSR
    if !key_exists {
        println!("Generating server certificate...");
        let csr_path = "server.csr";
        let status = std::process::Command::new("openssl")
            .args(&[
                "req", "-new",
                "-nodes",
                "-newkey", "rsa:2048",
                "-keyout", key_path,
                "-out", csr_path,
                "-subj", "/CN=localhost"
            ])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to generate server CSR"));
        }

        // 3. Sign server certificate with CA
        let ext_path = "server.ext";
        std::fs::write(&ext_path, "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nsubjectAltName = DNS:localhost, IP:127.0.0.1")?;

        let status = std::process::Command::new("openssl")
            .args(&[
                "x509", "-req",
                "-in", csr_path,
                "-CA", ca_path,
                "-CAkey", ca_key_path,
                "-CAcreateserial",
                "-out", cert_path,
                "-days", "365",
                "-sha256",
                "-extfile", ext_path
            ])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to sign server certificate"));
        }
    }

    Ok(())
}

pub fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    certs(&mut reader).collect()
}

pub fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    let keys = pkcs8_private_keys(&mut reader).collect::<io::Result<Vec<_>>>()?;
    if keys.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "No private key found"));
    }
    Ok(PrivateKeyDer::Pkcs8(keys[0].clone_key()))
}
