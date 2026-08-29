//! Live view of what the remote-protocol server is doing.
//!
//! The connection loop is the only place that knows a connection exists at all:
//! the engine sees requests, never sessions. This registry is where the loop
//! records them so a management view can answer "who is connected, and how busy
//! is the server" without the engine having to carry session state it has no
//! use for.
//!
//! It is process wide because a process runs one server. That also means a
//! command handler can reach it without a handle being threaded through every
//! signature.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One connection currently held open by a client.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ConnectionSnapshot {
    /// Monotonic id, unique for the lifetime of the process.
    pub id: u64,
    pub peer: String,
    /// The name the client was authorized under, empty while unidentified.
    pub client_name: String,
    pub thumbprint: String,
    pub is_admin: bool,
    pub connected_seconds: u64,
    pub requests: u64,
    /// The last command this connection ran, empty before its first request.
    pub last_command: String,
    pub idle_seconds: u64,
}

/// Everything the dashboard's overview needs about the running server.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ServerSnapshot {
    pub uptime_seconds: u64,
    /// Wall-clock start, as a Unix timestamp, for display next to the uptime.
    pub started_at: u64,
    pub listen_addr: String,
    pub total_connections: u64,
    pub rejected_connections: u64,
    pub total_requests: u64,
    pub failed_requests: u64,
    pub active_connections: Vec<ConnectionSnapshot>,
}

struct Connection {
    peer: String,
    client_name: String,
    thumbprint: String,
    is_admin: bool,
    connected_at: Instant,
    last_activity: Instant,
    requests: u64,
    last_command: String,
}

struct Registry {
    started: Instant,
    started_at: SystemTime,
    listen_addr: Mutex<String>,
    next_id: AtomicU64,
    total_connections: AtomicU64,
    rejected_connections: AtomicU64,
    total_requests: AtomicU64,
    failed_requests: AtomicU64,
    connections: Mutex<HashMap<u64, Connection>>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| Registry {
        started: Instant::now(),
        started_at: SystemTime::now(),
        listen_addr: Mutex::new(String::new()),
        next_id: AtomicU64::new(1),
        total_connections: AtomicU64::new(0),
        rejected_connections: AtomicU64::new(0),
        total_requests: AtomicU64::new(0),
        failed_requests: AtomicU64::new(0),
        connections: Mutex::new(HashMap::new()),
    })
}

/// Takes the lock, ignoring poisoning: a panicked handler must not turn the
/// statistics into a permanent error, and no invariant spans the lock.
fn connections() -> std::sync::MutexGuard<'static, HashMap<u64, Connection>> {
    registry().connections.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records the address the protocol server is listening on, and starts the
/// uptime clock if nothing has touched the registry yet.
pub fn set_listen_addr(addr: &str) {
    let registry = registry();
    let mut current = registry.listen_addr.lock().unwrap_or_else(|e| e.into_inner());
    *current = addr.to_string();
}

/// A connection that was refused before it could run anything: no client
/// certificate, or a certificate nobody authorized.
pub fn note_rejected() {
    registry().rejected_connections.fetch_add(1, Ordering::Relaxed);
}

/// Registers an accepted, authorized connection and returns its id. The caller
/// must pass that id to [`close`] when the connection ends.
pub fn open(peer: &str, client_name: &str, thumbprint: &str, is_admin: bool) -> u64 {
    let registry = registry();
    let id = registry.next_id.fetch_add(1, Ordering::Relaxed);
    registry.total_connections.fetch_add(1, Ordering::Relaxed);
    let now = Instant::now();
    connections().insert(
        id,
        Connection {
            peer: peer.to_string(),
            client_name: client_name.to_string(),
            thumbprint: thumbprint.to_string(),
            is_admin,
            connected_at: now,
            last_activity: now,
            requests: 0,
            last_command: String::new(),
        },
    );
    id
}

/// Counts one handled request against a connection and the server totals.
pub fn note_request(id: u64, command: &str, failed: bool) {
    let registry = registry();
    registry.total_requests.fetch_add(1, Ordering::Relaxed);
    if failed {
        registry.failed_requests.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(connection) = connections().get_mut(&id) {
        connection.requests += 1;
        connection.last_command = command.to_string();
        connection.last_activity = Instant::now();
    }
}

/// Drops a finished connection from the active list. Its requests stay in the
/// totals.
pub fn close(id: u64) {
    connections().remove(&id);
}

/// The current state of the server, with connections ordered oldest first.
pub fn snapshot() -> ServerSnapshot {
    let registry = registry();
    let now = Instant::now();
    let mut active: Vec<ConnectionSnapshot> = connections()
        .iter()
        .map(|(id, connection)| ConnectionSnapshot {
            id: *id,
            peer: connection.peer.clone(),
            client_name: connection.client_name.clone(),
            thumbprint: connection.thumbprint.clone(),
            is_admin: connection.is_admin,
            connected_seconds: now.saturating_duration_since(connection.connected_at).as_secs(),
            requests: connection.requests,
            last_command: connection.last_command.clone(),
            idle_seconds: now.saturating_duration_since(connection.last_activity).as_secs(),
        })
        .collect();
    active.sort_by(|a, b| b.connected_seconds.cmp(&a.connected_seconds).then(a.id.cmp(&b.id)));

    ServerSnapshot {
        uptime_seconds: now.saturating_duration_since(registry.started).as_secs(),
        started_at: registry.started_at.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs(),
        listen_addr: registry.listen_addr.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        total_connections: registry.total_connections.load(Ordering::Relaxed),
        rejected_connections: registry.rejected_connections.load(Ordering::Relaxed),
        total_requests: registry.total_requests.load(Ordering::Relaxed),
        failed_requests: registry.failed_requests.load(Ordering::Relaxed),
        active_connections: active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_appears_while_open_and_leaves_the_totals_behind() {
        let before = snapshot();
        let id = open("127.0.0.1:9999", "TEST.CLIENT", "abc123", true);

        let during = snapshot();
        let mine = during.active_connections.iter().find(|c| c.id == id).expect("connection is listed while open");
        assert_eq!(mine.client_name, "TEST.CLIENT");
        assert_eq!(mine.requests, 0);
        assert_eq!(during.total_connections, before.total_connections + 1);

        note_request(id, "READ", false);
        note_request(id, "QUERY", true);
        let busy = snapshot();
        let mine = busy.active_connections.iter().find(|c| c.id == id).unwrap();
        assert_eq!(mine.requests, 2);
        assert_eq!(mine.last_command, "QUERY");

        close(id);
        let after = snapshot();
        assert!(after.active_connections.iter().all(|c| c.id != id), "closed connection is gone");
        assert_eq!(after.total_requests, before.total_requests + 2);
        assert_eq!(after.failed_requests, before.failed_requests + 1);
    }

    #[test]
    fn rejected_connections_are_counted_without_being_listed() {
        let before = snapshot();
        note_rejected();
        let after = snapshot();
        assert_eq!(after.rejected_connections, before.rejected_connections + 1);
    }
}
