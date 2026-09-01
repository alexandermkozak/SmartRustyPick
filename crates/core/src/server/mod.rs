pub mod models;
pub mod certs;
pub mod handler;
pub mod stats;
#[cfg(test)]
mod handler_tests;
#[cfg(test)]
mod protocol_doc_tests;

use crate::config::Config;
pub use certs::{ensure_certificates, load_certs, load_key};
pub use handler::{SharedDb, handle_request, handle_request_locked, read_lock, write_lock};
pub use models::{Request, Response};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::rustls::server::WebPkiClientVerifier;

static ACTIVE_CONFIG: OnceLock<Arc<Config>> = OnceLock::new();

/// The configuration the running server was started with.
///
/// Commands that reach outside the database - issuing a certificate needs the
/// CA paths - have no other way to find it: the handler is given a database and
/// a client, and threading a config through every call site would touch every
/// command to serve one.
pub fn active_config() -> Option<Arc<Config>> {
    ACTIVE_CONFIG.get().cloned()
}

/// Publishes the configuration for [`active_config`]. The first server to start
/// in a process wins; a second one would be sharing the same database anyway.
pub fn set_active_config(config: Arc<Config>) {
    let _ = ACTIVE_CONFIG.set(config);
}

pub async fn start_server(config: Arc<Config>, db: SharedDb, override_addr: Option<String>) -> tokio::io::Result<()> {
    // Install default crypto provider for rustls
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

    let addr = override_addr.unwrap_or_else(|| format!("{}:{}", config.server_addr.as_ref().unwrap_or(&"0.0.0.0".to_string()), config.server_port.unwrap_or(8443)));

    ensure_certificates(&config)?;
    set_active_config(config.clone());

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
    stats::set_listen_addr(&addr);

    spawn_flusher(db.clone());
    crate::web::spawn_dashboard(config.clone(), db.clone(), &addr);

    // Bounds how many connections are held open at once: an authorised client
    // is trusted to issue requests, not to open unlimited sockets. Beyond the
    // limit, new connections are rejected outright rather than queued, so a
    // flood cannot build up unbounded backlog of its own.
    let max_connections = config.max_connections();
    let connection_slots = Arc::new(Semaphore::new(max_connections));

    loop {
        let (stream, peer_addr) = listener.accept().await?;

        let permit = match connection_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let msg = format!("Connection limit ({}) reached; rejecting {}", max_connections, peer_addr);
                eprintln!("{}", msg);
                stats::note_rejected();
                let db = db.clone();
                // The rejected socket is dropped as soon as this task returns, which
                // is enough to close it; logging still goes through spawn_blocking so
                // a slow disk never stalls the accept loop.
                tokio::spawn(async move {
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = write_lock(&db).log_error("SYSTEM", &msg);
                    })
                        .await;
                });
                continue;
            }
        };

        let acceptor = acceptor.clone();
        let db = db.clone();
        let config = config.clone();

        tokio::spawn(async move {
            // Held for the lifetime of this task; dropping it (on any return path)
            // frees the slot for the next connection.
            let _permit = permit;

            let handshake_timeout = std::time::Duration::from_millis(config.handshake_timeout_ms());
            let tls_stream = match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    eprintln!("TLS accept error from {}: {}", peer_addr, e);
                    return;
                }
                Err(_) => {
                    eprintln!("TLS handshake from {} did not complete within {:?}", peer_addr, handshake_timeout);
                    return;
                }
            };

            let (_, session) = tls_stream.get_ref();
            let mut client_cert_thumbprint = None;
            if let Some(certs) = session.peer_certificates()
                && let Some(cert) = certs.first() {
                    let mut hasher = Sha256::new();
                    hasher.update(cert);
                    client_cert_thumbprint = Some(hex::encode(hasher.finalize()));
                }

            let thumbprint = match client_cert_thumbprint {
                Some(t) => t,
                None => {
                    let msg = format!("No client certificate provided from {}", peer_addr);
                    eprintln!("{}", msg);
                    stats::note_rejected();
                    let db = db.clone();
                    // Logging writes to disk, so keep it off the async worker thread.
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = write_lock(&db).log_error("SYSTEM", &msg);
                    })
                        .await;
                    return;
                }
            };

            // Check authorization
            let client = {
                // Even taking the lock has to happen off the async worker: while a
                // flush holds it, waiting here would block every other connection
                // scheduled on this thread.
                let db_for_task = db.clone();
                let tp = thumbprint.clone();
                let client = tokio::task::spawn_blocking(move || {
                    read_lock(&db_for_task).client_for_thumbprint(&tp)
                })
                    .await
                    .ok()
                    .flatten();
                match client {
                    Some(client) => client,
                    None => {
                        let msg = format!("Unauthorized certificate {} from {}", thumbprint, peer_addr);
                        eprintln!("{}", msg);
                        stats::note_rejected();
                        let db = db.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = write_lock(&db).log_error("SYSTEM", &msg);
                        })
                            .await;
                        return;
                    }
                }
            };

            // From here the session is real, so it belongs in the live view a
            // management client can ask for.
            let connection_id = stats::open(&peer_addr.to_string(), &client.name, &thumbprint, client.is_admin);

            let (reader, mut writer) = tokio::io::split(tls_stream);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            let max_request_bytes = config.max_request_bytes() as u64;
            let idle_timeout = config.idle_timeout();

            loop {
                line.clear();
                // A fresh `Take` every iteration, so the byte allowance is per
                // request rather than shared across the whole connection.
                let mut bounded = (&mut reader).take(max_request_bytes);
                let read = bounded.read_line(&mut line);
                let read_result = match idle_timeout {
                    Some(timeout) => match tokio::time::timeout(timeout, read).await {
                        Ok(result) => result,
                        Err(_) => {
                            eprintln!("Connection from {} idle for more than {:?}; closing", peer_addr, timeout);
                            break;
                        }
                    },
                    None => read.await,
                };

                match read_result {
                    Ok(0) => break, // EOF
                    Ok(n) if n as u64 >= max_request_bytes && !line.ends_with('\n') => {
                        // The line reader hit the byte cap without finding a
                        // terminator: either a genuine oversized request or a
                        // client streaming bytes with no newline at all. Either
                        // way, unread bytes may still be sitting on the socket,
                        // so the only safe response is to close the connection
                        // rather than try to resynchronise on the next line.
                        eprintln!("Request from {} exceeded max_request_bytes ({} bytes); closing connection", peer_addr, max_request_bytes);
                        let resp = Response {
                            status: "ERROR".to_string(),
                            message: Some(format!("Request too large (max {} bytes)", max_request_bytes)),
                            ..Default::default()
                        };
                        if let Ok(resp_json) = serde_json::to_string(&resp) {
                            let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                        }
                        break;
                    }
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

                        // The engine is synchronous and file backed: running it on
                        // the async worker would stall every other task on that
                        // thread, handshakes of unrelated connections included. The
                        // client info is re-fetched inside the same task, both to
                        // support dynamic permission updates and to keep the lock off
                        // this thread entirely.
                        let db_for_task = db.clone();
                        let tp = thumbprint.clone();
                        let command = req.command.to_uppercase();
                        let handled = tokio::task::spawn_blocking(move || {
                            let info = read_lock(&db_for_task).client_for_thumbprint(&tp);
                            info.map(|info| handle_request(req, &db_for_task, &info))
                        })
                            .await;

                        match handled {
                            Ok(Some(resp)) => {
                                stats::note_request(connection_id, &command, resp.status != "OK");
                                if let Ok(resp_json) = serde_json::to_string(&resp) {
                                    let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                                }
                            }
                            Err(e) => {
                                stats::note_request(connection_id, &command, true);
                                let resp = Response { status: "ERROR".to_string(), message: Some(format!("Request failed: {}", e)), ..Default::default() };
                                if let Ok(resp_json) = serde_json::to_string(&resp) {
                                    let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                                }
                            }
                            Ok(None) => {
                                stats::note_request(connection_id, &command, true);
                                let resp = Response { status: "ERROR".to_string(), message: Some("Client deauthorized".to_string()), ..Default::default() };
                                if let Ok(resp_json) = serde_json::to_string(&resp) {
                                    let _ = writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                                }
                                break; // Terminate connection if client no longer exists
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Read error from {}: {}", peer_addr, e);
                        break;
                    }
                }
            }

            stats::close(connection_id);

            // A client that finished writing should not have to wait for the
            // ticker before its changes reach disk.
            let _ = tokio::task::spawn_blocking(move || {
                if read_lock(&db).has_pending_writes() {
                    let _ = write_lock(&db).save();
                }
            })
                .await;
        });
    }
}

/// Periodically writes out buffered changes.
///
/// Writes are batched in memory instead of hitting the disk one record at a
/// time; without a ticker the last writes of a burst would linger until the
/// next request arrived. The interval bounds how long that can be.
fn spawn_flusher(db: SharedDb) {
    let interval = {
        let db_lock = read_lock(&db);
        if db_lock.durable_writes {
            return; // Every write is already flushed synchronously.
        }
        db_lock.flush_interval
    };
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval.max(std::time::Duration::from_millis(10)));
        loop {
            ticker.tick().await;
            let db = db.clone();
            // Flushing touches the disk, so keep it off the async worker thread.
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = write_lock(&db).flush_if_due() {
                    eprintln!("Background flush error: {}", e);
                }
            })
                .await;
        }
    });
}
