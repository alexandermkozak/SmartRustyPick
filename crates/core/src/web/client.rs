//! The dashboard's connection to the database.
//!
//! The dashboard is deliberately not a second door into the engine: it holds a
//! client certificate and speaks the same line-delimited JSON protocol as any
//! other remote client, over the same TLS listener. Everything it can do, a
//! `LIST.CONNS` on that listener will show it doing, and revoking its
//! authorization locks it out exactly like any other client.
//!
//! One connection is kept open and shared. Reconnecting per request would work,
//! but it would also make the dashboard the noisiest entry in the very
//! connection list it exists to display.

use crate::server::certs::{load_certs, load_key};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

/// Longest response line accepted from the server. A QUERY over a large file
/// can be big; a management client asking for statistics never is, but the
/// ceiling keeps a runaway response from being buffered without bound.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

type Session = (
    BufReader<tokio::io::ReadHalf<TlsStream<TcpStream>>>,
    tokio::io::WriteHalf<TlsStream<TcpStream>>,
);

pub struct ProtocolClient {
    addr: String,
    server_name: ServerName<'static>,
    connector: TlsConnector,
    session: Mutex<Option<Session>>,
}

impl ProtocolClient {
    /// Builds a client that authenticates with `cert_path`/`key_path` and trusts
    /// only the given CA - the same CA the server verifies clients against.
    pub fn new(addr: &str, cert_path: &str, key_path: &str, ca_path: &str) -> std::io::Result<Self> {
        let mut roots = RootCertStore::empty();
        for cert in load_certs(ca_path)? {
            roots
                .add(cert)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        }

        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(load_certs(cert_path)?, load_key(key_path)?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        Ok(ProtocolClient {
            addr: addr.to_string(),
            // The server certificate is issued for `localhost`; the dashboard
            // always reaches it over the loopback interface.
            server_name: ServerName::try_from("localhost").expect("localhost is a valid server name"),
            connector: TlsConnector::from(Arc::new(config)),
            session: Mutex::new(None),
        })
    }

    async fn connect(&self) -> std::io::Result<Session> {
        let stream = TcpStream::connect(&self.addr).await?;
        let tls = self.connector.connect(self.server_name.clone(), stream).await?;
        let (reader, writer) = tokio::io::split(tls);
        Ok((BufReader::new(reader), writer))
    }

    /// Sends one request and returns the parsed response.
    ///
    /// A dropped connection - the server restarted, or the session idled out -
    /// is not an error the caller should have to think about, so the request is
    /// retried once on a fresh connection.
    pub async fn request(&self, payload: serde_json::Value) -> std::io::Result<serde_json::Value> {
        let mut session = self.session.lock().await;
        let mut last_error = None;

        for attempt in 0..2 {
            if session.is_none() {
                match self.connect().await {
                    Ok(fresh) => *session = Some(fresh),
                    Err(e) => {
                        last_error = Some(e);
                        continue;
                    }
                }
            }

            match Self::exchange(session.as_mut().expect("session was just established"), &payload).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    // Drop the session so the retry - and any later request -
                    // starts from a connection known to be good.
                    *session = None;
                    last_error = Some(e);
                    if attempt == 1 {
                        break;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| std::io::Error::other("Request failed")))
    }

    async fn exchange(session: &mut Session, payload: &serde_json::Value) -> std::io::Result<serde_json::Value> {
        let (reader, writer) = session;
        let mut line = serde_json::to_string(payload)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;

        let mut response = String::new();
        let read = (&mut *reader)
            .take(MAX_RESPONSE_BYTES as u64)
            .read_line(&mut response)
            .await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Server closed the connection",
            ));
        }
        if !response.ends_with('\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Response exceeded the size limit",
            ));
        }
        serde_json::from_str(&response).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
