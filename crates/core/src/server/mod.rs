pub mod models;
pub mod certs;
pub mod handler;
#[cfg(test)]
mod handler_tests;

use crate::config::Config;
use crate::db::Database;
pub use certs::{ensure_certificates, load_certs, load_key};
pub use handler::handle_request;
pub use models::{Request, Response};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::TlsAcceptor;

pub async fn start_server(config: Arc<Config>, db: Arc<Mutex<Database>>, override_addr: Option<String>) -> tokio::io::Result<()> {
    // Install default crypto provider for rustls
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

    let addr = override_addr.unwrap_or_else(|| format!("{}:{}", config.server_addr.as_ref().unwrap_or(&"0.0.0.0".to_string()), config.server_port.unwrap_or(8443)));

    ensure_certificates(&config)?;

    let certs = load_certs(config.cert_path.as_ref().unwrap())?;
    let key = load_key(config.key_path.as_ref().unwrap())?;
    let ca_certs = load_certs(config.ca_path.as_ref().unwrap())?;

    let mut root_cert_store = RootCertStore::empty();
    for cert in ca_certs {
        root_cert_store.add(cert).map_err(|e| tokio::io::Error::new(tokio::io::ErrorKind::InvalidInput, e))?;
    }

    let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_cert_store))
        .build()
        .map_err(|e| tokio::io::Error::new(tokio::io::ErrorKind::InvalidInput, e))?;

    let server_config = tokio_rustls::rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)
        .map_err(|e| tokio::io::Error::new(tokio::io::ErrorKind::InvalidInput, e))?;

    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind(&addr).await?;

    println!("Server listening on TLS {}", addr);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let db = db.clone();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("TLS accept error from {}: {}", peer_addr, e);
                    return;
                }
            };

            let (_, session) = tls_stream.get_ref();
            let mut client_cert_thumbprint = None;
            if let Some(certs) = session.peer_certificates() {
                if let Some(cert) = certs.first() {
                    let mut hasher = Sha256::new();
                    hasher.update(cert);
                    client_cert_thumbprint = Some(hex::encode(hasher.finalize()));
                }
            }

            let thumbprint = match client_cert_thumbprint {
                Some(t) => t,
                None => {
                    let msg = format!("No client certificate provided from {}", peer_addr);
                    eprintln!("{}", msg);
                    let mut db_lock = db.lock().unwrap();
                    let _ = db_lock.log_error("SYSTEM", &msg);
                    return;
                }
            };

            // Check authorization
            let client_info = {
                let db_lock = db.lock().unwrap();
                db_lock.authorized_clients.get(&thumbprint).cloned()
            };

            if client_info.is_none() {
                let msg = format!("Unauthorized certificate {} from {}", thumbprint, peer_addr);
                eprintln!("{}", msg);
                let mut db_lock = db.lock().unwrap();
                let _ = db_lock.log_error("SYSTEM", &msg);
                return;
            }
            let client_info = client_info.unwrap();

            let (reader, mut writer) = tokio::io::split(tls_stream);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let req: Request = match serde_json::from_str(&line) {
                            Ok(r) => r,
                            Err(e) => {
                                let resp = Response { status: "ERROR".to_string(), message: Some(format!("Invalid JSON: {}", e)), ..Default::default() };
                                if let Ok(resp_json) = serde_json::to_string(&resp) {
                                    let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                                }
                                continue;
                            }
                        };

                        let resp = handle_request(req, &db, &client_info);
                        if let Ok(resp_json) = serde_json::to_string(&resp) {
                            let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                        }
                    }
                    Err(e) => {
                        eprintln!("Read error from {}: {}", peer_addr, e);
                        break;
                    }
                }
            }
        });
    }
}
