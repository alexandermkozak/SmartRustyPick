//! `DEQUEUE … WAIT`: the waiting is done by the connection loop between
//! attempts, so it is tested there - against a real database and a client the
//! database knows by thumbprint, exactly as a connection would present it.

use super::dequeue_waiting;
use crate::db::{ClientGrant, Database, FileAttributes, QueuePolicy, Record, queue};
use crate::server::handler::{SharedDb, read_lock};
use crate::server::models::{ErrorCode, Response};
use crate::test_support::{TempDir, isolated_config};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const ACCOUNT: &str = "WAIT_TEST";
const THUMBPRINT: &str = "waiter_tp";

fn database(label: &str) -> (TempDir, SharedDb) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account(ACCOUNT, Some(dir.path())).unwrap();
    db.create_table_with(
        ACCOUNT,
        "JOBS",
        FileAttributes {
            durable: true,
            queue: Some(QueuePolicy::default()),
            autokey: false,
            directory: None,
        },
    )
    .unwrap();
    db.add_authorized_client(
        "waiter",
        THUMBPRINT,
        ClientGrant {
            allowed_accounts: vec![ACCOUNT.to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    (dir, Arc::new(RwLock::new(db)))
}

fn dequeue_line(wait: u64) -> String {
    serde_json::json!({
        "command": "DEQUEUE",
        "account": ACCOUNT,
        "file": "JOBS",
        "wait_seconds": wait,
    })
    .to_string()
}

async fn wait_for(db: &SharedDb, seconds: u64) -> Response {
    dequeue_waiting(&dequeue_line(seconds), Duration::from_secs(seconds), db, THUMBPRINT)
        .await
        .expect("the attempt task does not panic")
        .expect("the client is authorized")
}

fn enqueue_after(db: &SharedDb, delay: Duration, due: Option<u64>) {
    let db = db.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        tokio::task::spawn_blocking(move || {
            read_lock(&db)
                .enqueue_due(ACCOUNT, "JOBS", Record::from_display_string("work"), due)
                .unwrap();
        })
        .await
        .unwrap();
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_queue_is_answered_empty_when_the_wait_runs_out() {
    let (_dir, db) = database("wait_empty");
    let started = Instant::now();
    let resp = wait_for(&db, 1).await;
    assert_eq!(resp.status, "EMPTY");
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "answered after {:?}, before the wait was up",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_enqueued_during_the_wait_is_handed_over_at_once() {
    let (_dir, db) = database("wait_arrival");
    enqueue_after(&db, Duration::from_millis(150), None);
    let started = Instant::now();
    let resp = wait_for(&db, 10).await;
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    // Woken by the enqueue, not by the once-a-second recheck.
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "took {:?}: the arrival should have woken the waiter",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_record_coming_due_is_found_by_the_recheck() {
    let (_dir, db) = database("wait_due");
    enqueue_after(&db, Duration::ZERO, Some(queue::now_millis() + 300));
    let started = Instant::now();
    let resp = wait_for(&db, 10).await;
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert!(
        started.elapsed() < queue::WAIT_RECHECK + Duration::from_millis(600),
        "took {:?}: a due record should be found within one recheck",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_already_there_is_handed_over_without_waiting() {
    let (_dir, db) = database("wait_ready");
    read_lock(&db)
        .enqueue(ACCOUNT, "JOBS", Record::from_display_string("ready"))
        .unwrap();
    let started = Instant::now();
    let resp = wait_for(&db, 10).await;
    assert_eq!(resp.status, "OK");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn a_wait_beyond_the_ceiling_is_refused_before_any_waiting() {
    let (_dir, db) = database("wait_ceiling");
    let mut req: crate::server::models::Request =
        serde_json::from_str(&dequeue_line(queue::MAX_WAIT_SECONDS + 1)).unwrap();
    req.wait_seconds = Some(queue::MAX_WAIT_SECONDS + 1);
    let info = read_lock(&db).client_for_thumbprint(THUMBPRINT).unwrap();
    let resp = crate::server::handle_request(req, &db, &info);
    assert_eq!(resp.code, Some(ErrorCode::InvalidData));
}
