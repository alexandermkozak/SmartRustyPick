//! Byte transfers against a real database and a real buffered reader.
//!
//! The subject here is not "does a record round trip" - the integration suite
//! answers that over TLS. It is the two things that can only go wrong at this
//! seam: bytes lost between the request line and the body, and a socket left
//! sitting in the middle of one.

use super::transfer;
use crate::db::{ClientInfo, Database, DirectoryPolicy, FileAttributes};
use crate::server::handler::SharedDb;
use crate::server::models::{ErrorCode, Request, Response};
use crate::test_support::{TempDir, isolated_config};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const ACCOUNT: &str = "DIRS";
const FILE: &str = "SCANS";

/// Bytes an ordinary record cannot hold, and - the point here - bytes holding
/// newlines, so a body that was mistaken for lines would be torn apart.
fn hostile(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn database() -> (TempDir, SharedDb) {
    let dir = TempDir::new("transfer");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account(ACCOUNT, Some(dir.path())).unwrap();
    db.logto(ACCOUNT).unwrap();
    db.create_table_with(
        ACCOUNT,
        FILE,
        FileAttributes {
            durable: false,
            queue: None,
            directory: Some(DirectoryPolicy::default_path()),
        },
    )
    .unwrap();
    db.create_table_for_account(ACCOUNT, "USERS").unwrap();
    (dir, Arc::new(RwLock::new(db)))
}

fn client() -> ClientInfo {
    ClientInfo {
        name: "tester".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec![ACCOUNT.to_string()],
        is_admin: false,
    }
}

fn put(key: &str, length: u64) -> Request {
    Request {
        command: "PUT.BYTES".to_string(),
        account: Some(ACCOUNT.to_string()),
        file: Some(FILE.to_string()),
        key: Some(key.to_string()),
        length: Some(length),
        ..Default::default()
    }
}

fn get(key: &str) -> Request {
    Request {
        command: "GET.BYTES".to_string(),
        account: Some(ACCOUNT.to_string()),
        file: Some(FILE.to_string()),
        key: Some(key.to_string()),
        ..Default::default()
    }
}

/// The response line a transfer wrote, and whatever bytes followed it.
fn split_reply(written: &[u8]) -> (Response, Vec<u8>) {
    let at = written
        .iter()
        .position(|&b| b == b'\n')
        .expect("a transfer always answers with one JSON line");
    let response = serde_json::from_slice(&written[..at]).expect("the reply is JSON");
    (response, written[at + 1..].to_vec())
}

const NO_STALL: Duration = Duration::from_secs(30);
const MAX: u64 = 1024 * 1024;

/// A reader built the way the connection loop builds one, with a buffer far too
/// small to hold the body - so the body necessarily straddles it.
fn reader_over(bytes: Vec<u8>) -> BufReader<std::io::Cursor<Vec<u8>>> {
    BufReader::with_capacity(16, std::io::Cursor::new(bytes))
}

#[tokio::test]
async fn a_body_is_read_through_the_buffer_the_request_line_was() {
    let (_dir, db) = database();
    let body = hostile(4096);

    // Exactly what arrives on the socket: the request line and the body in one
    // stream, as a client writes them.
    let mut wire = serde_json::to_vec(&put("scan.bin", body.len() as u64)).unwrap();
    wire.push(b'\n');
    wire.extend_from_slice(&body);

    let mut reader = reader_over(wire);
    let mut written = Vec::new();

    // The line is read the way the connection loop reads it, which fills the
    // buffer past the newline and leaves the first bytes of the body inside it.
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let req: Request = serde_json::from_str(&line).unwrap();

    // The precondition, asserted so this test cannot quietly stop testing
    // anything: reading the line pulled body bytes into the buffer with it, and
    // those bytes are no longer on the underlying stream.
    assert!(
        !reader.buffer().is_empty(),
        "the request line should have carried body bytes into the buffer with it"
    );

    let outcome = transfer::put_bytes(&mut reader, &mut written, &req, &db, &client(), MAX, NO_STALL).await;
    assert!(!outcome.close, "the connection stays open");

    let (response, trailing) = split_reply(&written);
    assert_eq!(response.status, "OK", "{:?}", response.message);
    assert_eq!(response.length, Some(body.len() as u64));
    assert!(trailing.is_empty(), "a PUT.BYTES reply carries no body");

    // The whole argument: the bytes buffered before the body was asked for are
    // in the record. Reading the body from the underlying stream instead would
    // lose exactly the first sixteen and store the rest, and every assertion
    // above would still pass.
    let stored = db
        .read()
        .unwrap()
        .read_directory_record(ACCOUNT, FILE, "scan.bin")
        .unwrap();
    assert_eq!(stored.as_deref(), Some(body.as_slice()));
}

#[tokio::test]
async fn a_refused_transfer_drains_its_body_so_the_next_request_is_still_read() {
    let (_dir, db) = database();
    let body = hostile(2048);

    // A key no filesystem would take, a body, and then an ordinary request
    // line behind it - which is what the connection would read next.
    let mut wire = serde_json::to_vec(&put("../escape", body.len() as u64)).unwrap();
    wire.push(b'\n');
    wire.extend_from_slice(&body);
    wire.extend_from_slice(b"{\"command\":\"NEXT\"}\n");

    let mut reader = reader_over(wire);
    let mut written = Vec::new();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let req: Request = serde_json::from_str(&line).unwrap();

    let outcome = transfer::put_bytes(&mut reader, &mut written, &req, &db, &client(), MAX, NO_STALL).await;
    assert!(
        !outcome.close,
        "a refusal within the limit drains the body and keeps the connection"
    );
    assert!(outcome.failed, "and it is counted as the failed request it is");
    let (response, _) = split_reply(&written);
    assert_eq!(response.code, Some(ErrorCode::InvalidRequest));

    // The socket is back at a request boundary. Without the drain this would
    // read the middle of somebody's PDF as JSON.
    let mut next = String::new();
    reader.read_line(&mut next).await.unwrap();
    assert_eq!(next.trim(), r#"{"command":"NEXT"}"#);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_without_being_read_and_closes_the_connection() {
    let (_dir, db) = database();
    // Announced far past the limit, with nothing behind the line: draining it
    // would be the denial of service the limit exists to prevent, so the only
    // answer left is to close.
    let req = put("huge.bin", MAX * 100);
    let mut reader = reader_over(Vec::new());
    let mut written = Vec::new();

    let outcome = transfer::put_bytes(&mut reader, &mut written, &req, &db, &client(), MAX, NO_STALL).await;
    assert!(outcome.close);
    let (response, _) = split_reply(&written);
    assert_eq!(response.code, Some(ErrorCode::InvalidRequest));
    assert!(
        response.message.unwrap_or_default().contains("limited to"),
        "the refusal says what the limit is"
    );
}

#[tokio::test]
async fn a_body_that_runs_short_is_refused_rather_than_stored_truncated() {
    let (_dir, db) = database();
    let body = hostile(40);
    let mut wire = serde_json::to_vec(&put("short.bin", 100)).unwrap();
    wire.push(b'\n');
    wire.extend_from_slice(&body); // 40 of the 100 announced, then EOF

    let mut reader = reader_over(wire);
    let mut written = Vec::new();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let req: Request = serde_json::from_str(&line).unwrap();

    let outcome = transfer::put_bytes(&mut reader, &mut written, &req, &db, &client(), MAX, NO_STALL).await;
    assert!(outcome.close, "the socket ended mid-body");
    let (response, _) = split_reply(&written);
    assert_eq!(response.status, "ERROR");

    // Nothing stored, and nothing left behind: a truncated record under a key a
    // caller would then trust is the corruption the file type exists to stop.
    assert!(
        db.read()
            .unwrap()
            .read_directory_record(ACCOUNT, FILE, "short.bin")
            .unwrap()
            .is_none()
    );
    assert!(db.read().unwrap().directory_records(ACCOUNT, FILE).unwrap().is_empty());
}

#[tokio::test]
async fn a_stalled_body_closes_the_connection_rather_than_waiting_on_it() {
    let (_dir, db) = database();
    // A pipe whose far end sends the first bytes and then nothing. The transfer
    // is neither idle - a request is in flight - nor finished, which is the
    // case `idle_timeout_ms` cannot see.
    let (mut far, near) = tokio::io::duplex(4096);
    far.write_all(&hostile(64)).await.unwrap();

    let mut reader = BufReader::with_capacity(16, near);
    let mut written = Vec::new();
    let req = put("stalled.bin", 4096);

    let started = std::time::Instant::now();
    let outcome = transfer::put_bytes(
        &mut reader,
        &mut written,
        &req,
        &db,
        &client(),
        MAX,
        Duration::from_millis(120),
    )
    .await;
    assert!(outcome.close);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the stall timeout, not the far end, ended this"
    );
    // The far end is still open, so nothing but the timeout could have stopped it.
    drop(far);
}

#[tokio::test]
async fn get_bytes_announces_the_length_and_then_sends_exactly_that_many() {
    let (_dir, db) = database();
    let body = hostile(9000);
    db.read()
        .unwrap()
        .write_directory_record(ACCOUNT, FILE, "scan.bin", &body)
        .unwrap();

    let mut written = Vec::new();
    let outcome = transfer::get_bytes(&mut written, &get("scan.bin"), &db, &client()).await;
    assert!(!outcome.close, "the connection stays open");

    let (response, sent) = split_reply(&written);
    assert_eq!(response.status, "OK");
    assert_eq!(response.length, Some(body.len() as u64));
    // Exactly that many, and exactly those: a client reads the count off the
    // line and then takes that many bytes, so one byte either way desynchronises
    // it for the rest of the session.
    assert_eq!(sent, body);
}

#[tokio::test]
async fn a_refused_get_is_an_ordinary_reply_and_the_connection_carries_on() {
    let (_dir, db) = database();
    let mut written = Vec::new();
    let outcome = transfer::get_bytes(&mut written, &get("absent.bin"), &db, &client()).await;

    // Nothing was on the socket to resynchronise against - the client sent a
    // line and is waiting - so a refusal costs the connection nothing.
    assert!(!outcome.close, "the connection stays open");
    let (response, trailing) = split_reply(&written);
    assert_eq!(response.code, Some(ErrorCode::RecordNotFound));
    assert!(response.length.is_none(), "no length is announced for no body");
    assert!(trailing.is_empty());
}

#[tokio::test]
async fn a_transfer_will_not_reach_an_ordinary_file_or_an_account_the_client_may_not() {
    let (_dir, db) = database();
    let mut written = Vec::new();

    // An ordinary file's records are fields in a hashed section; storing raw
    // bytes there is the corruption directory files exist to rule out.
    let ordinary = Request {
        file: Some("USERS".to_string()),
        ..get("anything")
    };
    let outcome = transfer::get_bytes(&mut written, &ordinary, &db, &client()).await;
    assert!(!outcome.close, "the connection stays open");
    assert_eq!(split_reply(&written).0.code, Some(ErrorCode::InvalidRequest));

    // And the account rules are the ordinary ones: a transfer must not become a
    // way to reach an account a READ could not.
    written.clear();
    let elsewhere = Request {
        account: Some("SYSTEM".to_string()),
        ..get("anything")
    };
    let outcome = transfer::get_bytes(&mut written, &elsewhere, &db, &client()).await;
    assert!(!outcome.close, "the connection stays open");
    assert_eq!(split_reply(&written).0.code, Some(ErrorCode::AccessDenied));
}

/// The property the whole file type exists for, at the one place it is most
/// likely to be lost: a transfer moves megabytes over a network, and doing that
/// under a file's lock would block every writer to it for the length of the
/// wire, which is exactly the contention per-file locking removed.
///
/// Debug builds only - the counter compiles out of a release build.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_transfer_takes_no_table_lock_at_all() {
    use crate::db::engine::table_locks_taken;

    let (_dir, db) = database();
    let body = hostile(8192);

    let mut wire = serde_json::to_vec(&put("scan.bin", body.len() as u64)).unwrap();
    wire.push(b'\n');
    wire.extend_from_slice(&body);
    let mut reader = reader_over(wire);
    let mut written = Vec::new();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let req: Request = serde_json::from_str(&line).unwrap();

    // Warmed first: the first request against a file reads its DIR entry, and
    // the entry is cached from then on. Steady state is what the count is about.
    let _ = db.read().unwrap().directory_records(ACCOUNT, FILE).unwrap();

    let before = table_locks_taken();
    let outcome = transfer::put_bytes(&mut reader, &mut written, &req, &db, &client(), MAX, NO_STALL).await;
    let mut sent = Vec::new();
    let _ = transfer::get_bytes(&mut sent, &get("scan.bin"), &db, &client()).await;
    let taken = table_locks_taken() - before;

    assert!(!outcome.close);
    assert_eq!(
        taken, 0,
        "a transfer locked a file {taken} times. A directory file has no table, so there is nothing \
         to lock - and holding one across a network transfer would block every writer to that file \
         for as long as the client took to send."
    );
}

#[tokio::test]
async fn a_transfer_command_on_a_connection_that_cannot_carry_a_body_says_so() {
    // The ordinary dispatch is reached by an in-process caller or a client that
    // sent the line and nothing else. `UNKNOWN_COMMAND` would be wrong - the
    // command exists - and silence would be worse.
    let (_dir, db) = database();
    let response = crate::server::handle_request(put("scan.bin", 10), &db, &client());
    assert_eq!(response.code, Some(ErrorCode::InvalidRequest));
    assert!(response.message.unwrap_or_default().contains("carries raw bytes"));
}
