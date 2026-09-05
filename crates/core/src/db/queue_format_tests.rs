//! The queue file format, tested without a database.
//!
//! [`queue_tests`](super::queue_tests) drives the engine and checks what comes
//! back. These check the layer under it: the `queue` file's bytes, the keys the
//! engine mints, and the two `DIR` attributes a policy is read from. They are
//! separated because these are the paths where being wrong is *quiet* - a
//! checksum that is not enforced, a key that parses when it should not, a
//! policy that silently becomes zero - and because they need no fixture, so
//! there is no reason for a test of them to cost one.

use crate::db::hashfile::FsyncPolicy;
use crate::db::models::Record;
use crate::db::queue::{
    self, DEFAULT_MAX_DELIVERIES, DEFAULT_VISIBILITY, KEY_DIGITS, MAX_DELIVERY_LIMIT, MAX_VISIBILITY_SECONDS,
    PersistedQueue, QueuePolicy, QueueState,
};
use crate::test_support::TempDir;
use std::collections::HashMap;
use std::fs;
use std::time::Duration;

/// One way of damaging a state file, and what to call it when the damage is
/// not caught.
type Mangle = (&'static str, fn(String) -> String);

fn persisted(next: u64, deliveries: &[(&str, u32)]) -> PersistedQueue {
    PersistedQueue {
        next_sequence: next,
        deliveries: deliveries.iter().map(|(key, n)| (key.to_string(), *n)).collect(),
    }
}

/// Record keys, which is all [`QueueState::attach`] reads of the records.
fn records(keys: &[&str]) -> HashMap<String, Record> {
    keys.iter()
        .map(|key| (key.to_string(), Record::from_display_string("x")))
        .collect()
}

#[test]
fn the_state_file_round_trips_and_writes_the_same_bytes_twice() {
    let guard = TempDir::new("queue_state_round_trip");
    let dir = guard.path();

    let state = persisted(
        1_764_950_412_345_000_131,
        // Deliberately out of order: the writer sorts, so that the same state
        // cannot produce two different files - and therefore two different
        // checksums - depending on how a HashMap happened to iterate.
        &[("01764950412345000009", 4), ("01764950412345000001", 1)],
    );
    queue::write_state(dir, &state, FsyncPolicy::Never).unwrap();
    let read = queue::read_state(dir).expect("a file just written must read back");
    assert_eq!(read.next_sequence, state.next_sequence);
    assert_eq!(
        read.deliveries,
        vec![
            ("01764950412345000001".to_string(), 1),
            ("01764950412345000009".to_string(), 4),
        ]
    );

    let first = fs::read(queue::state_path(dir)).unwrap();
    queue::write_state(dir, &read, FsyncPolicy::Never).unwrap();
    assert_eq!(
        fs::read(queue::state_path(dir)).unwrap(),
        first,
        "the same state, the same bytes"
    );

    // Nothing is left behind by the temporary file the write goes through.
    assert!(!fs::exists(std::path::Path::new(dir).join("queue.tmp")).unwrap_or(false));
}

#[test]
fn a_queue_with_nothing_to_remember_still_writes_a_readable_file() {
    let guard = TempDir::new("queue_state_empty");
    let dir = guard.path();
    let empty = persisted(0, &[]);
    queue::write_state(dir, &empty, FsyncPolicy::Never).unwrap();
    assert_eq!(queue::read_state(dir), Some(empty));
}

#[test]
fn a_state_file_that_does_not_check_out_reads_as_absent() {
    let guard = TempDir::new("queue_state_corrupt");
    let dir = guard.path();
    let path = queue::state_path(dir);
    let good = persisted(42, &[("00000000000000000001", 3)]);

    // Reading as absent rather than as an error is the whole point: the records
    // are the queue, and this file only says where the sequence had got to and
    // how often each record had been delivered. Losing it costs a few extra
    // redeliveries; refusing to open the queue would cost the queue.
    let mangled: [Mangle; 5] = [
        ("a changed value", |body| body.replace("next=42", "next=99")),
        ("a changed delivery count", |body| body.replace(":3", ":4")),
        ("a truncated file", |body| body[..body.len() / 2].to_string()),
        ("no checksum line", |body| {
            body.lines().skip(1).collect::<Vec<_>>().join("\n")
        }),
        ("something else entirely", |_| "garbage".to_string()),
    ];
    for (what, mangle) in mangled {
        queue::write_state(dir, &good, FsyncPolicy::Never).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        fs::write(&path, mangle(body)).unwrap();
        assert_eq!(queue::read_state(dir), None, "{} must not be trusted", what);
    }

    // And a file that is simply not there.
    fs::remove_file(&path).unwrap();
    assert_eq!(queue::read_state(dir), None);
}

#[test]
fn removing_the_state_is_what_a_file_that_is_no_longer_a_queue_gets() {
    let guard = TempDir::new("queue_state_remove");
    let dir = guard.path();
    queue::write_state(dir, &persisted(7, &[]), FsyncPolicy::Never).unwrap();
    assert!(queue::state_path(dir).exists());

    queue::remove_state(dir).unwrap();
    assert!(!queue::state_path(dir).exists());
    // Idempotent: a file that was never a queue is not an error to tidy up.
    queue::remove_state(dir).unwrap();
}

#[test]
fn minting_survives_a_clock_that_steps_backwards() {
    let mut state = QueueState::attach(&records(&[]), PersistedQueue::default());

    let first = state.mint(1_000);
    // The clock goes back a second between two enqueues. The order must not.
    let second = state.mint(999);
    let third = state.mint(1_000);
    let fourth = state.mint(2_000);

    assert!(
        first < second && second < third && third < fourth,
        "keys only ever rise"
    );
    assert_eq!(first, 1_000 * queue::SUB_MILLISECOND, "the clock when it is trusted");
    assert_eq!(second, first + 1, "and the counter when it is not");
    assert_eq!(fourth, 2_000 * queue::SUB_MILLISECOND, "caught up once time passes it");

    // The keys they become sort the same way, which is what DEQUEUE walks.
    let keys: Vec<String> = [first, second, third, fourth]
        .iter()
        .map(|sequence| queue::format_key(*sequence))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, keys);
    assert!(keys.iter().all(|key| key.len() == KEY_DIGITS));
}

#[test]
fn a_millisecond_holds_a_million_keys_and_then_borrows_the_next() {
    // Started one short of the millisecond's last sequence number, rather than
    // by minting a million keys to get there.
    let last = 1_000 * queue::SUB_MILLISECOND + (queue::SUB_MILLISECOND - 1);
    let mut state = QueueState::attach(&records(&[]), persisted(last, &[]));

    assert_eq!(state.mint(1_000), last, "the last key this millisecond holds");
    let borrowed = state.mint(1_000);
    assert_eq!(
        borrowed,
        1_001 * queue::SUB_MILLISECOND,
        "the next enqueue in the same millisecond borrows from the next one rather than colliding"
    );
    assert_eq!(queue::key_enqueued_millis(&queue::format_key(borrowed)), Some(1_001));
}

#[test]
fn a_key_the_engine_did_not_mint_has_no_place_in_the_order() {
    // A queue file is still a file, so a key written by hand is a perfectly good
    // record - it simply carries no arrival time, and guessing one would put it
    // in the order in a position nothing chose.
    for key in [
        "1",
        "0000000000000000000x",
        "",
        "999999999999999999999",
        " 0000000000000000001",
    ] {
        assert_eq!(queue::key_sequence(key), None, "{:?} is not a sequence key", key);
        assert_eq!(queue::key_enqueued_millis(key), None);
    }

    let key = queue::format_key(1_764_950_412_345_000_001);
    assert_eq!(key, "01764950412345000001");
    assert_eq!(queue::key_sequence(&key), Some(1_764_950_412_345_000_001));
    assert_eq!(queue::key_enqueued_millis(&key), Some(1_764_950_412_345));
}

#[test]
fn dir_attributes_that_do_not_describe_a_policy_fall_back_rather_than_breaking_the_queue() {
    let default = QueuePolicy::default();
    assert_eq!(default.visibility, DEFAULT_VISIBILITY);
    assert_eq!(default.max_deliveries, DEFAULT_MAX_DELIVERIES);

    // A hand-edited DIR entry must not be able to make a queue unusable: a
    // timeout of zero would hand every record to two consumers at once, and a
    // delivery limit of zero would dead-letter everything on first sight.
    for (timeout, retries) in [("", ""), ("0", "0"), ("abc", "-1"), ("  ", "3.5")] {
        assert_eq!(
            QueuePolicy::from_attributes(timeout, retries),
            default,
            "{:?}/{:?} must fall back",
            timeout,
            retries
        );
    }

    // Sane values are taken as they are, and space around them is not fatal.
    let policy = QueuePolicy::from_attributes(" 300 ", " 3 ");
    assert_eq!(policy.visibility, Duration::from_secs(300));
    assert_eq!(policy.max_deliveries, 3);
    assert_eq!(policy.visibility_seconds(), 300);

    // Absurd ones are clamped rather than refused, for the same reason.
    let clamped = QueuePolicy::from_attributes("999999999", "999999");
    assert_eq!(clamped.visibility, Duration::from_secs(MAX_VISIBILITY_SECONDS));
    assert_eq!(clamped.max_deliveries, MAX_DELIVERY_LIMIT);
}

#[test]
fn attaching_reconciles_the_remembered_state_with_the_records_that_are_there() {
    // A delivery count naming a record the data section has not got is dropped:
    // the `queue` file is written after the records, so this is exactly what a
    // crash in between leaves behind.
    let state = QueueState::attach(
        &records(&["00000000000000000001", "00000000000000000002"]),
        persisted(5_000, &[("00000000000000000001", 2), ("00000000000000000099", 7)]),
    );
    assert_eq!(state.depth(), 2, "every record is available");
    assert_eq!(state.in_flight(), 0, "a restart holds no claims");
    assert_eq!(
        state.deliveries("00000000000000000001"),
        2,
        "a count that still names a record"
    );
    assert_eq!(
        state.deliveries("00000000000000000099"),
        0,
        "one that does not is dropped"
    );
    assert_eq!(state.next_sequence(), 5_000);

    // With no `queue` file at all - lost, or never written - the sequence is
    // recovered from the keys, so a fresh one cannot collide with a record that
    // is still there.
    let recovered = QueueState::attach(
        &records(&["00000000000000000001", "00000000000000000042"]),
        PersistedQueue::default(),
    );
    assert_eq!(recovered.next_sequence(), 43);
    assert_eq!(recovered.deliveries("00000000000000000001"), 0);

    // A key nobody minted is still a record to hand out; it just has no
    // sequence, so it cannot pull the sequence backwards either.
    let by_hand = QueueState::attach(&records(&["BYHAND"]), PersistedQueue::default());
    assert_eq!(by_hand.depth(), 1);
    assert_eq!(by_hand.next_sequence(), 0);
    assert_eq!(
        by_hand.oldest_unacknowledged_seconds(1_000),
        None,
        "no arrival time to report"
    );
}

#[test]
fn the_oldest_record_is_found_from_the_front_rather_than_by_scanning() {
    // The available keys are sorted, so the oldest is at the front - but only
    // among the keys that are sequence keys. A record written by hand may sort
    // either side of them, and must be stepped over rather than answered with
    // or allowed to end the search. Taking a `min` over every parsed key gave
    // the right answer too; it just walked the whole backlog to do it, inside
    // the lock every consumer of the queue is waiting on.
    let mut state = QueueState::attach(
        &records(&[
            "!written-by-hand",     // sorts ahead of every digit
            "0000000000000000000x", // the right width, not a number
            "00000000000005000000", // sequence 5_000_000, so 5ms
            "00000000000009000000", // 9ms
            "ZZZ",                  // sorts after every digit
        ]),
        PersistedQueue::default(),
    );
    assert_eq!(state.depth(), 5);
    assert_eq!(
        state.oldest_unacknowledged_seconds(10_000),
        Some(9),
        "10s minus the 5ms the oldest sequence key carries"
    );

    // A claim is part of the queue's age too: the in-flight set is separate
    // from the available one, so an answer taken only from the front of the
    // order would forget the record a consumer is holding.
    let claimed = state.claim("worker", 10_000, QueuePolicy::default()).unwrap();
    assert_eq!(claimed.0, "!written-by-hand", "the front of the order, whatever it is");
    let (key, _) = state.claim("worker", 10_000, QueuePolicy::default()).unwrap();
    assert_eq!(key, "0000000000000000000x");
    let (key, _) = state.claim("worker", 10_000, QueuePolicy::default()).unwrap();
    assert_eq!(key, "00000000000005000000", "the oldest minted record");
    assert_eq!(
        state.oldest_unacknowledged_seconds(10_000),
        Some(9),
        "still the oldest, now that it is claimed rather than waiting"
    );

    // Nothing minted left anywhere: no arrival time to report, rather than a
    // guess.
    let none = QueueState::attach(&records(&["ZZZ"]), PersistedQueue::default());
    assert_eq!(none.oldest_unacknowledged_seconds(10_000), None);
}

#[test]
fn a_dead_letter_file_is_named_from_its_queue_and_recognises_itself() {
    assert_eq!(queue::dead_letter_name("JOBS"), "JOBS.DEAD");
    assert!(!queue::is_dead_letter_name("JOBS"));
    assert!(queue::is_dead_letter_name("JOBS.DEAD"));
    // The name is derivable in both directions, which is how `FILE.STATS`
    // counts a queue's dead letters without being told where they are.
    assert!(queue::is_dead_letter_name(&queue::dead_letter_name("JOBS")));
}
