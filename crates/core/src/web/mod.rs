//! The web management dashboard.
//!
//! Started by [`crate::server::start_server`], so every way of running the
//! database - the headless service, or the CLI's background server - brings the
//! dashboard up with it.
//!
//! Two properties are worth stating plainly, because they are what make an
//! HTTP surface on a database defensible:
//!
//! * The dashboard is an ordinary remote client. It holds a client certificate,
//!   it connects to the same TLS listener, and it speaks the same commands
//!   documented in `docs/protocol.md`. It has no privileged path into the
//!   engine, so it cannot outgrow what the protocol allows.
//! * Its certificate is reissued on every boot and re-authorized under a fixed
//!   name, which replaces the previous entry. A certificate from an earlier run
//!   is therefore dead the moment the server restarts, and one that leaks is
//!   worth only what remains of a single uptime.

pub mod api;
pub mod client;
pub mod http;
#[cfg(test)]
mod tests;

use crate::config::Config;
use crate::server::SharedDb;
use crate::server::certs::generate_client_cert;
use crate::server::handler::write_lock;
use client::ProtocolClient;
use http::{Incoming, Response};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::TcpListener;

/// The name the dashboard's certificate is authorized under. Fixed, so each
/// boot's re-authorization replaces the previous boot's entry instead of
/// leaving a trail of live credentials behind.
pub const DASHBOARD_CLIENT_NAME: &str = "WEB.DASHBOARD";
/// Common name of the generated certificate, and the stem of its files.
const DASHBOARD_COMMON_NAME: &str = "web-dashboard";
/// Days the dashboard certificate is valid for. It is replaced on every boot;
/// this only bounds a server left running for a very long time.
const DASHBOARD_CERT_DAYS: u32 = 1;
/// Cookie the browser carries the dashboard token in.
const TOKEN_COOKIE: &str = "srp_token";
/// How long a kept-alive connection may sit idle before it is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_CSS: &str = include_str!("assets/app.css");
const APP_JS: &str = include_str!("assets/app.js");

/// Starts the dashboard beside a running protocol server, unless it is switched
/// off in the configuration.
///
/// Never fatal: a dashboard that cannot bind its port, or cannot be issued a
/// certificate, reports the reason and leaves the database serving.
pub fn spawn_dashboard(config: Arc<Config>, db: SharedDb, protocol_addr: &str) {
    if !config.web_enabled() {
        return;
    }
    let protocol_addr = protocol_addr.to_string();
    tokio::spawn(async move {
        if let Err(e) = run(config, db, protocol_addr).await {
            eprintln!("Web dashboard unavailable: {}", e);
        }
    });
}

/// The host part of an `addr:port`, with any brackets around an IPv6 literal
/// removed.
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map(|(host, _)| host).unwrap_or("");
    host.trim_matches(|c| c == '[' || c == ']')
}

/// True when `addr` binds every interface, and is therefore reachable from
/// outside this machine - or outside this container.
fn is_wildcard(addr: &str) -> bool {
    matches!(host_of(addr), "0.0.0.0" | "::" | "")
}

/// True when `addr` can only be reached from the machine it runs on.
fn is_loopback(addr: &str) -> bool {
    let host = host_of(addr);
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// The address a client on this machine should use to reach a server bound to
/// `addr`. A wildcard bind is reachable on the loopback interface, which is
/// also the only name the server certificate is issued for.
fn loopback_target(addr: &str) -> String {
    let port = addr.rsplit(':').next().unwrap_or("8443");
    if is_wildcard(addr) {
        return format!("127.0.0.1:{}", port);
    }
    format!("{}:{}", host_of(addr), port)
}

/// A token nobody can guess, from the system's entropy source.
///
/// Falls back to `openssl rand` - already a hard dependency for certificate
/// handling - rather than to anything time-derived, because a predictable token
/// is worse than no dashboard at all.
fn random_token() -> std::io::Result<String> {
    use std::io::Read;
    if let Ok(mut source) = std::fs::File::open("/dev/urandom") {
        let mut bytes = [0u8; 24];
        if source.read_exact(&mut bytes).is_ok() {
            return Ok(hex::encode(bytes));
        }
    }
    let output = std::process::Command::new("openssl").args(["rand", "-hex", "24"]).output()?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.len() < 32 {
        return Err(std::io::Error::other("Could not generate a dashboard token"));
    }
    Ok(token)
}

/// Constant-time comparison, so a wrong token cannot be narrowed down by how
/// long the rejection took.
fn tokens_match(expected: &str, provided: &str) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(provided.as_bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Issues the dashboard's certificate and authorizes it, replacing whatever the
/// previous boot left behind.
///
/// This is the one thing the dashboard cannot do over the protocol: it needs an
/// authorized certificate before it can connect at all, so the bootstrap goes
/// straight to the engine and everything afterwards goes over the wire.
fn issue_dashboard_certificate(config: &Config, db: &SharedDb) -> std::io::Result<crate::server::certs::GeneratedCert> {
    let generated = generate_client_cert(config, DASHBOARD_COMMON_NAME, DASHBOARD_CERT_DAYS, false)?;
    write_lock(db).add_authorized_client(DASHBOARD_CLIENT_NAME, &generated.thumbprint, Vec::new(), true)?;
    Ok(generated)
}

async fn run(config: Arc<Config>, db: SharedDb, protocol_addr: String) -> std::io::Result<()> {
    let protocol_target = loopback_target(&protocol_addr);
    let bind_addr = config.web_bind_addr();
    let token = match config.web_token.clone() {
        Some(token) if !token.trim().is_empty() => token.trim().to_string(),
        _ => random_token()?,
    };

    // Certificate work shells out to openssl and takes the database lock, so it
    // belongs off the async worker thread.
    let generated = {
        let config = config.clone();
        let db = db.clone();
        tokio::task::spawn_blocking(move || issue_dashboard_certificate(&config, &db))
            .await
            .map_err(std::io::Error::other)??
    };

    let ca_path = config
        .ca_path
        .clone()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "ca_path is required for the dashboard"))?;
    let client = Arc::new(ProtocolClient::new(
        &protocol_target,
        &generated.cert_path,
        &generated.key_path,
        &ca_path,
    )?);

    let listener = TcpListener::bind(&bind_addr).await?;
    println!("Web dashboard on http://{}/?token={}", bind_addr, token);
    println!(
        "  authorized as {} (thumbprint {}), reissued on every start",
        DASHBOARD_CLIENT_NAME, generated.thumbprint
    );
    if !is_loopback(&bind_addr) {
        println!("  warning: the dashboard is bound to a non-loopback address and is served over plain HTTP");
    } else if is_wildcard(&protocol_addr) {
        // The database is reachable from elsewhere and the dashboard is not.
        // Inside a container that reads as the dashboard simply not working:
        // the published port lands on an interface nothing is listening on.
        println!(
            "  note: the database accepts connections on {} but the dashboard is bound to {},",
            protocol_addr, bind_addr
        );
        println!("        so it can only be opened from this machine - inside the container, if this is one.");
        println!("        Set web_addr in config.toml to expose it, and keep it behind a TLS proxy if you do.");
    }

    let token = Arc::new(token);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("Dashboard accept error: {}", e);
                continue;
            }
        };
        let client = client.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(stream, client, token).await {
                // A browser closing a tab mid-request is normal, not news.
                if e.kind() != std::io::ErrorKind::UnexpectedEof && e.kind() != std::io::ErrorKind::BrokenPipe {
                    eprintln!("Dashboard connection error from {}: {}", peer, e);
                }
            }
        });
    }
}

async fn serve_connection(stream: tokio::net::TcpStream, client: Arc<ProtocolClient>, token: Arc<String>) -> std::io::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    loop {
        let incoming = match tokio::time::timeout(IDLE_TIMEOUT, http::read_request(&mut reader)).await {
            Ok(result) => result?,
            Err(_) => return Ok(()), // idle for too long; let the client reconnect
        };

        match incoming {
            Incoming::Eof => return Ok(()),
            Incoming::Rejected(response) => {
                http::write_response(&mut writer, &response, false).await?;
                return Ok(());
            }
            Incoming::Request(request) => {
                let keep_alive = request.keep_alive;
                let response = handle(&client, &token, &request).await;
                http::write_response(&mut writer, &response, keep_alive).await?;
                if !keep_alive {
                    return Ok(());
                }
            }
        }
    }
}

/// Whether a request carries the dashboard token, in any of the three places a
/// browser or a script can put it.
fn authenticated(request: &http::Request, token: &str) -> bool {
    if let Some(cookie) = request.cookie(TOKEN_COOKIE)
        && tokens_match(token, &cookie)
    {
        return true;
    }
    if let Some(header) = request.header("authorization")
        && let Some(bearer) = header.strip_prefix("Bearer ")
        && tokens_match(token, bearer.trim())
    {
        return true;
    }
    request.query.get("token").is_some_and(|value| tokens_match(token, value))
}

async fn handle(client: &Arc<ProtocolClient>, token: &str, request: &http::Request) -> Response {
    // Liveness needs no token: it says the dashboard is up and nothing else.
    if request.path == "/health" {
        return Response::json(200, &serde_json::json!({ "status": "ok" }));
    }

    if !authenticated(request, token) {
        return unauthorized(request);
    }

    match request.path.as_str() {
        "/" | "/index.html" => {
            let response = Response::html(INDEX_HTML);
            // Arriving with `?token=` is how the URL printed at startup works.
            // Storing it lets the page's own requests carry it without the
            // token ever reaching the page's JavaScript.
            match request.query.get("token") {
                Some(_) => response.with_header(
                    "Set-Cookie",
                    format!("{}={}; Path=/; HttpOnly; SameSite=Strict", TOKEN_COOKIE, token),
                ),
                None => response,
            }
        }
        "/app.css" => Response::new(200, "text/css; charset=utf-8", APP_CSS),
        "/app.js" => Response::new(200, "application/javascript; charset=utf-8", APP_JS),
        path if path.starts_with("/api/") => api::route(client, request).await,
        _ => Response::error(404, "Not found"),
    }
}

/// A JSON refusal for the API, and a page that says what to do for anything a
/// person might have typed into the address bar.
fn unauthorized(request: &http::Request) -> Response {
    if request.path.starts_with("/api/") {
        return Response::error(401, "A valid dashboard token is required");
    }
    Response::new(
        401,
        "text/html; charset=utf-8",
        "<!doctype html><meta charset=\"utf-8\"><title>SmartRustyPick</title>\
         <body style=\"font-family:system-ui;margin:3rem;max-width:40rem\">\
         <h1>Token required</h1>\
         <p>The management dashboard is reached through the address printed in the database server's console at startup, \
         which carries a one-time token for this run.</p>\
         <p>Restart the server, or check its output, to get the current link.</p>",
    )
}
