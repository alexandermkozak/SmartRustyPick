use crate::db::{Database, QueryNode, Record};
use rustls_pemfile::{certs, pkcs8_private_keys};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, BufReader as SyncBufReader};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::TlsAcceptor;

#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub command: String,
    pub account: Option<String>,
    pub table: Option<String>,
    pub key: Option<String>,
    pub data: Option<String>,
    pub is_dict: Option<bool>,
    pub query_node: Option<QueryNode>,
    pub query_string: Option<String>,
    pub list_name: Option<String>,
    pub batch_size: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Response {
    pub status: String,
    pub message: Option<String>,
    pub record: Option<String>,
    pub results: Option<Vec<(String, String)>>,
    pub keys: Option<Vec<String>>,
    pub count: Option<usize>,
}

use crate::config::Config;
use std::path::Path;

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

        let _ = std::fs::remove_file(csr_path);
        let _ = std::fs::remove_file(ext_path);

        if !status.success() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to sign server certificate"));
        }
    }

    Ok(())
}

pub async fn run_server(
    addr: &str,
    db: Arc<Mutex<Database>>,
    cert_path: &str,
    key_path: &str,
    ca_path: &str,
) -> io::Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();

    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let ca_certs = load_certs(ca_path)?;

    let mut client_auth_roots = RootCertStore::empty();
    for cert in ca_certs {
        client_auth_roots.add(cert).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    }

    let client_verifier = WebPkiClientVerifier::builder(Arc::new(client_auth_roots)).build().map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {}: {}", addr, e);
            return Err(e);
        }
    };

    println!("TCP Server listening on {}", addr);
    use std::io::Write;
    std::io::stdout().flush()?;

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let db = db.clone();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    let mut msg = format!("TLS accept error from {}: {}", peer_addr, e);
                    if msg.contains("UnknownIssuer") {
                        msg.push_str(" (Check if client cert is signed by the server's CA)");
                    } else if msg.contains("UnknownCA") {
                        msg.push_str(" (Check if the client trusts the server's CA)");
                    }
                    eprintln!("{}", msg);
                    let mut db_lock = db.lock().unwrap();
                    let _ = db_lock.log_error("SYSTEM", &msg);
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
                                let resp = Response { status: "ERROR".to_string(), message: Some(format!("Invalid JSON: {}", e)), record: None, results: None, keys: None, count: None };
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

fn handle_request(req: Request, db: &Arc<Mutex<Database>>, client_info: &crate::db::ClientInfo) -> Response {
    let mut db = db.lock().unwrap();

    // Account restriction and defaulting logic
    let target_account = if let Some(acc) = req.account {
        // Client specified an account
        if !client_info.is_admin && !client_info.allowed_accounts.contains(&acc) {
            let msg = format!("Access denied for account {}: Not in allowed list", acc);
            let _ = db.log_error("REMOTE", &msg);
            return Response { status: "ERROR".to_string(), message: Some(msg), record: None, results: None, keys: None, count: None };
        }
        acc
    } else {
        // Client did not specify an account
        if client_info.allowed_accounts.len() == 1 {
            // Default to the only allowed account
            client_info.allowed_accounts[0].clone()
        } else {
            return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
        }
    };

    if db.current_account != target_account {
        if let Err(e) = db.logto(&target_account) {
            let msg = format!("Remote login error for account {}: {}", target_account, e);
            let _ = db.log_error("REMOTE", &msg);
            return Response { status: "ERROR".to_string(), message: Some(format!("Failed to login to account: {}", e)), record: None, results: None, keys: None, count: None };
        }
    }

    match req.command.to_uppercase().as_str() {
        "READ" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            if let Some(table) = db.get_table(&table_name) {
                let map = if is_dict { &table.dictionary } else { &table.records };
                if let Some(record) = map.get(&key) {
                    Response { status: "OK".to_string(), message: None, record: Some(record.to_display_string()), results: None, keys: None, count: None }
                } else {
                    Response { status: "NOT_FOUND".to_string(), message: None, record: None, results: None, keys: None, count: None }
                }
            } else {
                Response { status: "ERROR".to_string(), message: Some("Table not found".to_string()), record: None, results: None, keys: None, count: None }
            }
        }
        "WRITE" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let data = match req.data {
                Some(d) => d,
                None => return Response { status: "ERROR".to_string(), message: Some("Data not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let record = Record::from_display_string(&data);
            let table = db.get_table_mut(&table_name);
            if is_dict {
                table.dictionary.insert(key, record);
            } else {
                table.records.insert(key, record);
            }
            table.dirty = true;
            let _ = db.save();
            Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None }
        }
        "DELETE" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = db.get_table_mut(&table_name);
            let map = if is_dict { &mut table.dictionary } else { &mut table.records };
            if map.remove(&key).is_some() {
                table.dirty = true;
                let _ = db.save();
                Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None }
            } else {
                Response { status: "NOT_FOUND".to_string(), message: None, record: None, results: None, keys: None, count: None }
            }
        }
        "QUERY" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let records = if let Some(node) = req.query_node {
                db.query(&table_name, is_dict, &node, None)
            } else if let Some(qs) = req.query_string {
                let parts: Vec<&str> = qs.split_whitespace().collect();
                if let Some(node) = db.parse_query(&table_name, &parts) {
                    db.query(&table_name, is_dict, &node, None)
                } else {
                    return Response { status: "ERROR".to_string(), message: Some("Invalid query string".to_string()), record: None, results: None, keys: None, count: None };
                }
            } else {
                return Response { status: "ERROR".to_string(), message: Some("Query not specified".to_string()), record: None, results: None, keys: None, count: None };
            };

            if let Some(name) = req.list_name {
                let keys: Vec<String> = records.iter().map(|(k, _)| k.clone()).collect();
                let count = keys.len();
                db.remote_select_lists.insert(name.clone(), crate::db::SelectList {
                    table_name,
                    is_dict,
                    keys,
                });
                db.remote_select_cursors.insert(name, 0);
                Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: Some(count) }
            } else {
                let results = records.into_iter().map(|(k, r)| (k, r.to_display_string())).collect();
                Response { status: "OK".to_string(), message: None, record: None, results: Some(results), keys: None, count: None }
            }
        }
        "READNEXT" => {
            let list_name = match req.list_name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("List name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let batch_size = req.batch_size.unwrap_or(1);

            let (keys, count) = if db.remote_select_lists.contains_key(&list_name) {
                let list = db.remote_select_lists.get(&list_name).unwrap();
                let list_len = list.keys.len();
                let start = *db.remote_select_cursors.get(&list_name).unwrap();
                let end = std::cmp::min(start + batch_size, list_len);

                if start >= list_len {
                    (vec![], 0)
                } else {
                    let k = list.keys[start..end].to_vec();
                    let cursor = db.remote_select_cursors.get_mut(&list_name).unwrap();
                    *cursor = end;
                    (k, end - start)
                }
            } else {
                return Response { status: "ERROR".to_string(), message: Some("Select list not found".to_string()), record: None, results: None, keys: None, count: None };
            };

            if count == 0 && batch_size > 0 {
                Response { status: "EOF".to_string(), message: None, record: None, results: None, keys: Some(vec![]), count: Some(0) }
            } else {
                Response { status: "OK".to_string(), message: None, record: None, results: None, keys: Some(keys), count: Some(count) }
            }
        }
        "GETLIST" => {
            let list_name = match req.list_name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("List name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };

            if let Some(list) = db.remote_select_lists.get(&list_name) {
                Response { status: "OK".to_string(), message: None, record: None, results: None, keys: Some(list.keys.clone()), count: Some(list.keys.len()) }
            } else {
                Response { status: "ERROR".to_string(), message: Some("Select list not found".to_string()), record: None, results: None, keys: None, count: None }
            }
        }
        _ => Response { status: "ERROR".to_string(), message: Some("Unknown command".to_string()), record: None, results: None, keys: None, count: None },
    }
}

fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    certs(&mut reader).collect()
}

fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    let keys = pkcs8_private_keys(&mut reader).collect::<io::Result<Vec<_>>>()?;
    if keys.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "No private key found"));
    }
    Ok(PrivateKeyDer::Pkcs8(keys[0].clone_key()))
}
