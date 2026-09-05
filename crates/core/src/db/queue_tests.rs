//! Queue files end to end: the order, the claim, the timeout, the dead-letter
//! file and what survives a restart.

use crate::db::engine::Database;
use crate::db::models::*;
use crate::db::queue::{self, QueuePolicy};
use crate::test_support::{TempDir, isolated_config};
use std::collections::HashSet;
use std::time::Duration;

fn open_account(base: &str, account: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account(account, Some(base)).unwrap();
    db.logto(account).unwrap();
    db
}

/// A queue whose claims lapse after `visibility` seconds and whose records get
/// `deliveries` attempts.
fn queue_file(db: &Database, account: &str, name: &str, visibility: u64, deliveries: u32) {
    db.create_table_with(
        account,
        name,
        FileAttributes {
            durable: true,
            queue: Some(QueuePolicy {
                visibility: Duration::from_secs(visibility),
                max_deliveries: deliveries,
            }),
        },
    )
    .unwrap();
}

fn body(record: &Record) -> String {
    record.to_display_string()
}

fn enqueue(db: &Database, account: &str, name: &str, text: &str) -> String {
    db.enqueue(account, name, Record::from_display_string(text)).unwrap()
}

#[test]
fn a_queue_file_is_marked_in_dir_and_survives_a_rebuild() {
    let guard = TempDir::new("queue_dir_flag");
    let base = guard.path();
    let db = open_account(base, "Q1");
    queue_file(&db, "Q1", "JOBS", 30, 3);
    db.create_table_for_account("Q1", "PLAIN").unwrap();

    let attributes = db.file_attributes_for_account("Q1", "JOBS");
    assert_eq!(
        attributes.queue,
        Some(QueuePolicy {
            visibility: Duration::from_secs(30),
            max_deliveries: 3,
        })
    );
    assert!(attributes.durable, "a queue is durable unless asked otherwise");
    assert!(db.file_attributes_for_account("Q1", "PLAIN").queue.is_none());

    // The DIR entry itself, not just the cache in front of it.
    let entry = db.get_table_mut_for_account("Q1", "DIR").unwrap().read().records["JOBS"].clone();
    assert_eq!(entry.get_field_display_string(DIR_QUEUE_IDX), "Y");
    assert_eq!(entry.get_field_display_string(DIR_QUEUE_TIMEOUT_IDX), "30");
    assert_eq!(entry.get_field_display_string(DIR_QUEUE_RETRIES_IDX), "3");

    // A rebuild reconstructs the listing from the filesystem, which knows none
    // of this: every attribute has to be carried across.
    db.sync_dir_file_for_account("Q1").unwrap();
    let rebuilt = db.get_table_mut_for_account("Q1", "DIR").unwrap().read().records["JOBS"].clone();
    assert_eq!(FileAttributes::of(&rebuilt), attributes);
}

#[test]
fn the_flag_and_the_policy_survive_a_restart() {
    let guard = TempDir::new("queue_restart_flag");
    let base = guard.path();
    {
        let db = open_account(base, "Q2");
        queue_file(&db, "Q2", "JOBS", 45, 2);
        enqueue(&db, "Q2", "JOBS", "first");
        db.save().unwrap();
    }
    let db = Database::new(base, Some(isolated_config())).unwrap();
    let attributes = db.file_attributes_for_account("Q2", "JOBS");
    assert_eq!(attributes.queue.unwrap().visibility, Duration::from_secs(45));
    assert_eq!(attributes.queue.unwrap().max_deliveries, 2);
    assert!(db.is_table_queue_for_account("Q2", "JOBS"));
}

#[test]
fn dequeue_returns_records_in_enqueue_order() {
    let guard = TempDir::new("queue_order");
    let base = guard.path();
    let db = open_account(base, "Q3");
    queue_file(&db, "Q3", "JOBS", 60, 5);

    let expected: Vec<String> = (0..25).map(|n| format!("job {}", n)).collect();
    let mut keys = Vec::new();
    for text in &expected {
        keys.push(enqueue(&db, "Q3", "JOBS", text));
    }

    // Minted in order, and the keys sort the same way as text and as numbers.
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, keys, "sequence keys must sort into arrival order");

    let mut seen = Vec::new();
    while let Some(delivery) = db.dequeue("Q3", "JOBS", "worker", None).unwrap() {
        seen.push(body(&delivery.record));
        db.ack("Q3", "JOBS", &delivery.key, "worker").unwrap();
    }
    assert_eq!(seen, expected);
}

#[test]
fn two_consumers_never_receive_the_same_record() {
    let guard = TempDir::new("queue_exclusive");
    let base = guard.path();
    let db = open_account(base, "Q4");
    queue_file(&db, "Q4", "JOBS", 60, 5);
    for n in 0..40 {
        enqueue(&db, "Q4", "JOBS", &format!("job {}", n));
    }

    // Alternating consumers, neither acknowledging: a record handed to one must
    // not be visible to the other while the claim stands.
    let mut keys = Vec::new();
    loop {
        let one = db.dequeue("Q4", "JOBS", "alice", None).unwrap();
        let two = db.dequeue("Q4", "JOBS", "bob", None).unwrap();
        match (one, two) {
            (None, None) => break,
            (a, b) => keys.extend(a.into_iter().chain(b).map(|delivery| delivery.key)),
        }
    }
    let unique: HashSet<&String> = keys.iter().collect();
    assert_eq!(keys.len(), 40);
    assert_eq!(unique.len(), 40, "every record was claimed exactly once");
}

#[test]
fn a_claim_that_is_not_acknowledged_comes_back_and_counts_the_delivery() {
    let guard = TempDir::new("queue_timeout");
    let base = guard.path();
    let db = open_account(base, "Q5");
    // One second of visibility, so the timeout is observable without a long
    // test; five deliveries, so the redelivery is not a dead letter.
    queue_file(&db, "Q5", "JOBS", 1, 5);
    let key = enqueue(&db, "Q5", "JOBS", "slow job");

    let first = db.dequeue("Q5", "JOBS", "alice", None).unwrap().unwrap();
    assert_eq!(first.key, key);
    assert_eq!(first.deliveries, 1);
    assert!(
        db.dequeue("Q5", "JOBS", "bob", None).unwrap().is_none(),
        "still claimed"
    );

    std::thread::sleep(Duration::from_millis(1_100));

    let second = db.dequeue("Q5", "JOBS", "bob", None).unwrap().unwrap();
    assert_eq!(second.key, key, "the lapsed claim was redelivered");
    assert_eq!(second.deliveries, 2, "and the delivery count went up");
    assert!(
        db.dequeue("Q5", "JOBS", "alice", None).unwrap().is_none(),
        "redelivered once, not to everyone at once"
    );

    // The consumer that lost its claim cannot acknowledge the record out from
    // under the one that now holds it.
    let stolen = db.ack("Q5", "JOBS", &key, "alice").unwrap_err();
    assert!(stolen.to_string().contains("claimed by bob"), "{}", stolen);
}

#[test]
fn a_record_that_runs_out_of_deliveries_is_dead_lettered_with_its_count() {
    let guard = TempDir::new("queue_dead_letter");
    let base = guard.path();
    let db = open_account(base, "Q6");
    queue_file(&db, "Q6", "JOBS", 60, 2);
    let key = enqueue(&db, "Q6", "JOBS", "poison");

    // Two deliveries, each given straight back.
    for delivery in 1..=2 {
        let claimed = db.dequeue("Q6", "JOBS", "alice", None).unwrap().unwrap();
        assert_eq!(claimed.deliveries, delivery);
        db.nack("Q6", "JOBS", &claimed.key, "alice").unwrap();
    }

    assert!(
        db.dequeue("Q6", "JOBS", "alice", None).unwrap().is_none(),
        "the queue is empty: the record has gone to the dead-letter file"
    );

    let dead = queue::dead_letter_name("JOBS");
    let letter = db.peek("Q6", &dead, Some(&key)).unwrap().unwrap();
    assert_eq!(letter.key, key, "the sequence key came across");
    assert_eq!(letter.deliveries, 2, "and so did the count that killed it");
    assert_eq!(body(&letter.record), "poison");

    // The dead-letter file is a queue itself, so a fixed consumer can drain it.
    let replay = db.dequeue("Q6", &dead, "operator", None).unwrap().unwrap();
    assert_eq!(replay.key, key);
    db.ack("Q6", &dead, &key, "operator").unwrap();
    assert!(db.dequeue("Q6", &dead, "operator", None).unwrap().is_none());
}

#[test]
fn peek_reads_the_head_without_claiming_it() {
    let guard = TempDir::new("queue_peek");
    let base = guard.path();
    let db = open_account(base, "Q7");
    queue_file(&db, "Q7", "JOBS", 60, 5);
    let first = enqueue(&db, "Q7", "JOBS", "one");
    enqueue(&db, "Q7", "JOBS", "two");

    let peeked = db.peek("Q7", "JOBS", None).unwrap().unwrap();
    assert_eq!(peeked.key, first);
    assert_eq!(peeked.deliveries, 0, "a peek is not a delivery");
    assert!(peeked.owner.is_none());

    let claimed = db.dequeue("Q7", "JOBS", "alice", None).unwrap().unwrap();
    assert_eq!(claimed.key, first, "the peek left it available");

    // Peeking a named key shows who is holding it.
    let held = db.peek("Q7", "JOBS", Some(&first)).unwrap().unwrap();
    assert_eq!(held.owner.as_deref(), Some("alice"));
    assert!(db.peek("Q7", "JOBS", Some("nosuchkey")).unwrap().is_none());
}

#[test]
fn a_visibility_override_applies_to_one_claim_only() {
    let guard = TempDir::new("queue_override");
    let base = guard.path();
    let db = open_account(base, "Q8");
    queue_file(&db, "Q8", "JOBS", 3_600, 5);
    let key = enqueue(&db, "Q8", "JOBS", "quick");

    db.dequeue("Q8", "JOBS", "alice", Some(Duration::from_secs(1)))
        .unwrap()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1_100));
    let second = db.dequeue("Q8", "JOBS", "bob", None).unwrap().unwrap();
    assert_eq!(second.key, key, "the shorter timeout was the one that applied");
    // Back on the queue's own hour-long timeout, this claim does not lapse.
    assert!(db.dequeue("Q8", "JOBS", "alice", None).unwrap().is_none());
}

#[test]
fn acknowledged_records_stay_gone_and_claimed_ones_come_back_after_a_restart() {
    let guard = TempDir::new("queue_restart");
    let base = guard.path();
    let (acked, claimed) = {
        let db = open_account(base, "Q9");
        queue_file(&db, "Q9", "JOBS", 3_600, 5);
        let acked = enqueue(&db, "Q9", "JOBS", "done");
        let claimed = enqueue(&db, "Q9", "JOBS", "in flight");

        let first = db.dequeue("Q9", "JOBS", "alice", None).unwrap().unwrap();
        assert_eq!(first.key, acked);
        db.ack("Q9", "JOBS", &acked, "alice").unwrap();
        // Claimed with an hour of visibility and deliberately not acknowledged.
        let second = db.dequeue("Q9", "JOBS", "alice", None).unwrap().unwrap();
        assert_eq!(second.key, claimed);
        db.save().unwrap();
        (acked, claimed)
    };

    // A restart has no connections, so it has no claims: the record the dead
    // consumer was holding is available again, and its delivery count is intact.
    let db = Database::new(base, Some(isolated_config())).unwrap();
    let back = db.dequeue("Q9", "JOBS", "bob", None).unwrap().unwrap();
    assert_eq!(back.key, claimed, "nothing claimed-and-lost disappeared");
    assert_eq!(back.deliveries, 2, "the delivery count survived the restart");
    assert!(
        db.peek("Q9", "JOBS", Some(&acked)).unwrap().is_none(),
        "nothing claimed-and-acknowledged reappeared"
    );
}

#[test]
fn file_statistics_report_the_queue() {
    let guard = TempDir::new("queue_stats");
    let base = guard.path();
    let db = open_account(base, "QA");
    queue_file(&db, "QA", "JOBS", 60, 1);
    for n in 0..4 {
        enqueue(&db, "QA", "JOBS", &format!("job {}", n));
    }
    let claimed = db.dequeue("QA", "JOBS", "alice", None).unwrap().unwrap();

    let stats = db.file_statistics("QA", "JOBS").unwrap().queue.unwrap();
    assert_eq!(stats.depth, 3);
    assert_eq!(stats.in_flight, 1);
    assert_eq!(stats.dead_letters, 0);
    assert_eq!(stats.visibility_timeout_seconds, 60);
    assert_eq!(stats.max_deliveries, 1);
    assert!(!stats.dead_letter);
    assert!(
        stats.oldest_unacknowledged_seconds.is_some(),
        "a queue holding records has an oldest one"
    );

    // One delivery allowed, so giving it back dead-letters it.
    db.nack("QA", "JOBS", &claimed.key, "alice").unwrap();
    let stats = db.file_statistics("QA", "JOBS").unwrap().queue.unwrap();
    assert_eq!(stats.depth, 3);
    assert_eq!(stats.in_flight, 0);
    assert_eq!(stats.dead_letters, 1);

    // The dead-letter file knows what it is, and reports no dead letters of its
    // own rather than pointing at a file that will never exist.
    let dead = db.file_statistics("QA", &queue::dead_letter_name("JOBS")).unwrap();
    let dead = dead.queue.unwrap();
    assert!(dead.dead_letter);
    assert_eq!(dead.depth, 1);
    assert_eq!(dead.dead_letters, 0);

    // An ordinary file reports no queue at all rather than a queue of zero.
    db.create_table_for_account("QA", "PLAIN").unwrap();
    assert!(db.file_statistics("QA", "PLAIN").unwrap().queue.is_none());
}

#[test]
fn queue_commands_refuse_a_file_that_is_not_a_queue() {
    let guard = TempDir::new("queue_refusal");
    let base = guard.path();
    let db = open_account(base, "QB");
    db.create_table_for_account("QB", "PLAIN").unwrap();

    let refused = db.enqueue("QB", "PLAIN", Record::from_display_string("x")).unwrap_err();
    assert!(refused.to_string().contains("is not a queue file"), "{}", refused);
    assert!(db.dequeue("QB", "PLAIN", "alice", None).is_err());
    assert!(db.peek("QB", "PLAIN", None).is_err());
}

#[test]
fn a_file_can_be_made_a_queue_and_returned_to_an_ordinary_one() {
    let guard = TempDir::new("queue_convert");
    let base = guard.path();
    let db = open_account(base, "QC");
    db.create_table_for_account("QC", "OUTBOX").unwrap();

    db.set_file_attributes(
        "QC",
        "OUTBOX",
        FileAttributes {
            durable: true,
            queue: Some(QueuePolicy::default()),
        },
    )
    .unwrap();
    let key = enqueue(&db, "QC", "OUTBOX", "letter");
    assert_eq!(db.peek("QC", "OUTBOX", None).unwrap().unwrap().key, key);

    db.set_file_attributes(
        "QC",
        "OUTBOX",
        FileAttributes {
            durable: true,
            queue: None,
        },
    )
    .unwrap();
    assert!(db.peek("QC", "OUTBOX", None).is_err(), "no longer a queue");
    // The records are untouched by the conversion in either direction.
    let table = db.get_table_mut_for_account("QC", "OUTBOX").unwrap();
    assert_eq!(table.read().records.len(), 1);
}

#[test]
fn records_written_or_deleted_outside_the_queue_are_reconciled_into_the_order() {
    let guard = TempDir::new("queue_reconcile");
    let base = guard.path();
    let db = open_account(base, "QD");
    queue_file(&db, "QD", "JOBS", 60, 5);
    let queued = enqueue(&db, "QD", "JOBS", "minted");

    // A record put in by hand, under a key the engine did not mint.
    {
        let handle = db.get_table_mut_for_account("QD", "JOBS").unwrap();
        handle
            .write()
            .insert_record("BYHAND", Record::from_display_string("by hand"));
    }
    let first = db.dequeue("QD", "JOBS", "alice", None).unwrap().unwrap();
    let second = db.dequeue("QD", "JOBS", "alice", None).unwrap().unwrap();
    let claimed: HashSet<String> = [first.key.clone(), second.key].into_iter().collect();
    assert!(claimed.contains(&queued) && claimed.contains("BYHAND"));

    // And one deleted out from under a claim: the queue forgets it rather than
    // handing out a key with no record behind it.
    {
        let handle = db.get_table_mut_for_account("QD", "JOBS").unwrap();
        handle.write().remove_record(&queued);
    }
    assert!(db.dequeue("QD", "JOBS", "bob", None).unwrap().is_none());
}
