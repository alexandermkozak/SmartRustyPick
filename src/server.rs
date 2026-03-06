use crate::db::{Database, Record};
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
    pub query: Option<QueryArgs>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct QueryArgs {
    pub field_name: String,
    pub op: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Response {
    pub status: String,
    pub message: Option<String>,
    pub record: Option<String>,
    pub results: Option<Vec<(String, String)>>,
}

pub async fn run_server(
    addr: &str,
    db: Arc<Mutex<Database>>,
    cert_path: &str,
    key_path: &str,
    ca_path: &str,
) -> io::Result<()> {
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
                    eprintln!("No client certificate provided from {}", peer_addr);
                    return;
                }
            };

            // Check authorization
            {
                let db_lock = db.lock().unwrap();
                if !db_lock.authorized_certs.contains(&thumbprint) {
                    eprintln!("Unauthorized certificate {} from {}", thumbprint, peer_addr);
                    return;
                }
            }

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
                                let resp = Response { status: "ERROR".to_string(), message: Some(format!("Invalid JSON: {}", e)), record: None, results: None };
                                let _ = writer.write_all(format!("{}\n", serde_json::to_string(&resp).unwrap()).as_bytes()).await;
                                continue;
                            }
                        };

                        let resp = handle_request(req, &db);
                        let _ = writer.write_all(format!("{}\n", serde_json::to_string(&resp).unwrap()).as_bytes()).await;
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

fn handle_request(req: Request, db: &Arc<Mutex<Database>>) -> Response {
    let mut db = db.lock().unwrap();

    // If account is specified, logto it
    if let Some(acc) = req.account {
        if let Err(e) = db.logto(&acc) {
            return Response { status: "ERROR".to_string(), message: Some(format!("Failed to login to account: {}", e)), record: None, results: None };
        }
    } else if db.current_account.is_empty() {
        return Response { status: "ERROR".to_string(), message: Some("Not logged into any account".to_string()), record: None, results: None };
    }

    match req.command.to_uppercase().as_str() {
        "READ" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            if let Some(table) = db.get_table(&table_name) {
                let map = if is_dict { &table.dictionary } else { &table.records };
                if let Some(record) = map.get(&key) {
                    Response { status: "OK".to_string(), message: None, record: Some(record.to_display_string()), results: None }
                } else {
                    Response { status: "NOT_FOUND".to_string(), message: None, record: None, results: None }
                }
            } else {
                Response { status: "ERROR".to_string(), message: Some("Table not found".to_string()), record: None, results: None }
            }
        }
        "WRITE" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None },
            };
            let data = match req.data {
                Some(d) => d,
                None => return Response { status: "ERROR".to_string(), message: Some("Data not specified".to_string()), record: None, results: None },
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
            Response { status: "OK".to_string(), message: None, record: None, results: None }
        }
        "DELETE" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = db.get_table_mut(&table_name);
            let map = if is_dict { &mut table.dictionary } else { &mut table.records };
            if map.remove(&key).is_some() {
                table.dirty = true;
                let _ = db.save();
                Response { status: "OK".to_string(), message: None, record: None, results: None }
            } else {
                Response { status: "NOT_FOUND".to_string(), message: None, record: None, results: None }
            }
        }
        "QUERY" => {
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None },
            };
            let q = match req.query {
                Some(q) => q,
                None => return Response { status: "ERROR".to_string(), message: Some("Query not specified".to_string()), record: None, results: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let records = db.query(&table_name, is_dict, &q.field_name, &q.op, &q.value, None);
            let results = records.into_iter().map(|(k, r)| (k, r.to_display_string())).collect();
            Response { status: "OK".to_string(), message: None, record: None, results: Some(results) }
        }
        _ => Response { status: "ERROR".to_string(), message: Some("Unknown command".to_string()), record: None, results: None },
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
