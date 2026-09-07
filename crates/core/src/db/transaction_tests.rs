//! What a transaction promises, tested at the two places it can be broken: the
//! refusals that keep a set out of scope from being half-applied, and a process
//! killed between the two halves of a set that was accepted.

use crate::db::engine::Database;
use crate::db::hashfile;
use crate::db::models::*;
use crate::db::transaction::{self, Change, ChangeOp, Intent, MAX_CHANGES};
use crate::db::{DbError, FileAttributes, QueuePolicy};
use crate::test_support::{TempDir, isolated_config};
use std::collections::HashMap;

fn record(value: &str) -> Record {
    Record::from_display_string(value)
}

/// An account whose directory is the database's own, which is also the layout
/// that puts the transaction log beside the account's files - so every test
/// here is one that would notice the log being mistaken for a file.
fn open_account(base: &str, account: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account(account, Some(base)).unwrap();
    db.logto(account).unwrap();
    db
}

fn two_files(base: &str, account: &str) -> Database {
    let db = open_account(base, account);
    db.create_table("A").unwrap();
    db.create_table("B").unwrap();
    db.save().unwrap();
    db
}

/// One record as the files on disk have it, without opening the database - so a
/// test can see what a crash left behind before a replay changes it.
fn on_disk(base: &str, file: &str, key: &str) -> Option<Record> {
    let mut records = HashMap::new();
    hashfile::load(&format!("{}/{}/data", base, file), &mut records).ok()?;
    records.remove(key)
}

fn in_memory(db: &Database, file: &str, key: &str) -> Option<Record> {
    db.get_table_mut(file).unwrap().read().records.get(key).cloned()
}

// ---------------------------------------------------------------- the format

#[test]
fn an_intent_reads_back_as_the_set_that_was_written() {
    let changes = vec![
        Change::write("ORDERS", "O-1", record("PLACED^7")),
        Change::delete("BASKETS", "B-1"),
        Change::write("ORDERS", "AMOUNT", record("2^Amount^R^8")).in_dictionary(),
    ];
    let bytes = transaction::encode("SALES", &changes);
    assert_eq!(
        transaction::decode(&bytes),
        Some(Intent {
            account: "SALES".to_string(),
            changes,
        })
    );
}

#[test]
fn a_record_that_is_not_text_survives_the_intent() {
    // A record is a byte container, and an intent that could only carry UTF-8
    // would quietly refuse to make a transaction of exactly the writes that are
    // hardest to repeat by hand.
    let mut binary = Record::new();
    binary.fields.push(Field {
        values: vec![Value::bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])],
    });
    let changes = vec![Change::write("BLOBS", "K1", binary.clone())];
    let back = transaction::decode(&transaction::encode("SALES", &changes)).unwrap();
    assert_eq!(back.changes[0].op, ChangeOp::Write(binary));
}

#[test]
fn a_torn_or_altered_intent_is_not_read_as_one() {
    let bytes = transaction::encode("SALES", &[Change::write("ORDERS", "O-1", record("PLACED"))]);

    // Truncated at every length: none of them may decode as a shorter set.
    for cut in 0..bytes.len() {
        assert_eq!(
            transaction::decode(&bytes[..cut]),
            None,
            "an intent truncated to {} bytes decoded",
            cut
        );
    }
    // A byte changed anywhere, which is what the checksum is for.
    let mut flipped = bytes.clone();
    let last = flipped.len() - 6;
    flipped[last] ^= 0xFF;
    assert_eq!(transaction::decode(&flipped), None);

    // Something else entirely, and something with our magic but no frame.
    assert_eq!(transaction::decode(b"not an intent at all"), None);
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"more");
    assert_eq!(transaction::decode(&trailing), None);
}

// ----------------------------------------------------------- applying a set

#[test]
fn a_set_across_two_files_is_applied_and_is_on_disk_before_it_is_acknowledged() {
    let guard = TempDir::new("txn_two_files");
    let base = guard.path();
    let db = two_files(base, "TXN");

    // Nothing here is durable and nothing is due a flush, so anything on disk
    // afterwards was put there by the transaction itself.
    let applied = db
        .apply_transaction(
            "TXN",
            vec![
                Change::write("A", "K1", record("ONE")),
                Change::write("B", "K1", record("TWO")),
            ],
        )
        .unwrap();

    assert_eq!(applied, 2);
    assert_eq!(in_memory(&db, "A", "K1"), Some(record("ONE")));
    assert_eq!(in_memory(&db, "B", "K1"), Some(record("TWO")));
    assert_eq!(on_disk(base, "A", "K1"), Some(record("ONE")));
    assert_eq!(on_disk(base, "B", "K1"), Some(record("TWO")));
    // Applied and durable, so nothing is left to replay.
    assert!(transaction::pending(base).is_empty());
}

#[test]
fn a_set_mixes_writes_and_deletes_across_files() {
    let guard = TempDir::new("txn_mixed");
    let base = guard.path();
    let db = two_files(base, "TXN");
    db.get_table_mut("A")
        .unwrap()
        .write()
        .insert_record("GONE", record("OLD"));
    db.save().unwrap();

    db.apply_transaction(
        "TXN",
        vec![
            Change::delete("A", "GONE"),
            Change::write("B", "NEW", record("ARRIVED")),
        ],
    )
    .unwrap();

    assert_eq!(on_disk(base, "A", "GONE"), None);
    assert_eq!(on_disk(base, "B", "NEW"), Some(record("ARRIVED")));
}

#[test]
fn a_dictionary_entry_travels_in_the_same_set_as_the_record_that_needs_it() {
    let guard = TempDir::new("txn_dict");
    let base = guard.path();
    let db = two_files(base, "TXN");

    db.apply_transaction(
        "TXN",
        vec![
            Change::write("A", "AMOUNT", record("2^Amount^R^8")).in_dictionary(),
            Change::write("A", "K1", record("SOMETHING^42")),
        ],
    )
    .unwrap();

    let handle = db.get_table_mut("A").unwrap();
    let table = handle.read();
    assert_eq!(table.dictionary.get("AMOUNT"), Some(&record("2^Amount^R^8")));
    assert_eq!(table.records.get("K1"), Some(&record("SOMETHING^42")));
    drop(table);

    // And both are on disk, in their own sections, before the call returned.
    db.clear_loaded_tables();
    let reloaded = db.get_table_mut("A").unwrap();
    let table = reloaded.read();
    assert_eq!(table.dictionary.get("AMOUNT"), Some(&record("2^Amount^R^8")));
    assert_eq!(table.records.get("K1"), Some(&record("SOMETHING^42")));
}

#[test]
fn an_index_sees_the_records_a_transaction_wrote() {
    let guard = TempDir::new("txn_index");
    let base = guard.path();
    let db = open_account(base, "TXN");
    db.create_table("A").unwrap();
    {
        let handle = db.get_table_mut("A").unwrap();
        let mut table = handle.write();
        table.dictionary.insert("STATUS".to_string(), record("1^Status^L^10"));
        table.mark_dict_dirty();
    }
    db.create_index_for_account("TXN", "A", "STATUS").unwrap();

    db.apply_transaction("TXN", vec![Change::write("A", "K1", record("OPEN"))])
        .unwrap();

    let values = db.index_values("TXN", "A", "STATUS", 10).unwrap();
    assert!(
        values.iter().any(|value| value.value == "OPEN" && value.keys == 1),
        "the index did not see the transaction's write: {:?}",
        values
    );
}

#[test]
fn the_log_directory_is_not_one_of_the_accounts_files() {
    let guard = TempDir::new("txn_listing");
    let base = guard.path();
    let db = two_files(base, "TXN");
    db.apply_transaction("TXN", vec![Change::write("A", "K1", record("ONE"))])
        .unwrap();

    // The log lives beside the account's files whenever the account is
    // registered against the database's own directory, which is exactly the
    // layout these tests use.
    let files = db.list_tables_for_account("TXN");
    assert!(
        !files.iter().any(|name| name.starts_with('.')),
        "the transaction log was listed as a file: {:?}",
        files
    );
}

// -------------------------------------------------------------- the refusals

#[test]
fn a_file_the_account_does_not_have_refuses_the_whole_set() {
    let guard = TempDir::new("txn_missing_file");
    let base = guard.path();
    let db = two_files(base, "TXN");

    let refused = db
        .apply_transaction(
            "TXN",
            vec![
                Change::write("A", "K1", record("ONE")),
                Change::write("NOWHERE", "K1", record("TWO")),
            ],
        )
        .unwrap_err();

    assert!(matches!(refused, DbError::FileNotFound { .. }), "{:?}", refused);
    // The point of the refusal: the change to the file that *does* exist was
    // not applied on the way to finding out about the one that does not.
    assert_eq!(in_memory(&db, "A", "K1"), None);
    assert!(transaction::pending(base).is_empty());
}

#[test]
fn a_queue_file_is_refused_with_a_scope_error() {
    let guard = TempDir::new("txn_queue");
    let base = guard.path();
    let db = two_files(base, "TXN");
    db.set_file_attributes(
        "TXN",
        "B",
        FileAttributes {
            durable: false,
            queue: Some(QueuePolicy::default()),
            directory: None,
        },
    )
    .unwrap();

    let refused = db
        .apply_transaction(
            "TXN",
            vec![
                Change::write("A", "K1", record("ONE")),
                Change::write("B", "K1", record("TWO")),
            ],
        )
        .unwrap_err();

    assert!(matches!(refused, DbError::TransactionScope(_)), "{:?}", refused);
    assert_eq!(in_memory(&db, "A", "K1"), None);
}

#[test]
fn a_set_larger_than_the_limit_is_refused_rather_than_truncated() {
    let guard = TempDir::new("txn_too_large");
    let base = guard.path();
    let db = two_files(base, "TXN");

    let changes: Vec<Change> = (0..=MAX_CHANGES)
        .map(|n| Change::write("A", &format!("K{}", n), record("X")))
        .collect();
    let refused = db.apply_transaction("TXN", changes).unwrap_err();

    assert!(matches!(refused, DbError::TransactionScope(_)), "{:?}", refused);
    assert_eq!(in_memory(&db, "A", "K0"), None);
}

#[test]
fn one_key_changed_twice_in_a_set_is_refused() {
    let guard = TempDir::new("txn_duplicate");
    let base = guard.path();
    let db = two_files(base, "TXN");

    let refused = db
        .apply_transaction(
            "TXN",
            vec![Change::write("A", "K1", record("ONE")), Change::delete("A", "K1")],
        )
        .unwrap_err();

    assert!(matches!(refused, DbError::InvalidRequest(_)), "{:?}", refused);
    assert_eq!(in_memory(&db, "A", "K1"), None);
}

#[test]
fn an_empty_set_is_refused() {
    let guard = TempDir::new("txn_empty");
    let base = guard.path();
    let db = two_files(base, "TXN");
    assert!(matches!(
        db.apply_transaction("TXN", Vec::new()).unwrap_err(),
        DbError::InvalidRequest(_)
    ));
}

// ------------------------------------------------------------- the crash half

/// Set on the re-executed test binary to make it play the victim.
const CHILD_DIR: &str = "SRP_TXN_CHILD_DIR";

/// Writes one set across two files and never returns: the crash point named by
/// `SRP_TXN_CRASH` SIGKILLs the process partway through, so nothing is unwound,
/// no destructor runs and no further write can happen. What is on disk
/// afterwards is what a power loss would have left.
fn transact_then_die(base: &str) -> ! {
    let db = two_files(base, "TXN");
    let _ = db.apply_transaction(
        "TXN",
        vec![
            Change::write("A", "K1", record("ONE")),
            Change::write("B", "K1", record("TWO")),
        ],
    );
    unreachable!("the crash point should have killed the process");
}

/// Runs this test again as a child that dies at `stage`, and hands back the
/// directory it died in.
fn died_at(stage: &str, test: &str, label: &str) -> TempDir {
    let guard = TempDir::new(label);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD_DIR, guard.path())
        .env(crate::db::engine::transaction::CRASH_AT, stage)
        .status()
        .unwrap();
    assert!(
        status.code().is_none(),
        "the child should have died from a signal, not exited: {:?}",
        status
    );
    guard
}

/// Reopens the database the child died in and answers what the two files hold.
fn after_reopen(base: &str) -> (Option<Record>, Option<Record>) {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.logto("TXN").unwrap();
    (in_memory(&db, "A", "K1"), in_memory(&db, "B", "K1"))
}

#[test]
fn a_crash_before_the_intent_leaves_no_trace_of_the_set() {
    if let Ok(base) = std::env::var(CHILD_DIR) {
        transact_then_die(&base);
    }
    let guard = died_at(
        "before-intent",
        "db::transaction_tests::a_crash_before_the_intent_leaves_no_trace_of_the_set",
        "txn_crash_before",
    );
    let base = guard.path();

    assert!(transaction::pending(base).is_empty(), "an intent was written too early");
    assert_eq!(on_disk(base, "A", "K1"), None);
    assert_eq!(on_disk(base, "B", "K1"), None);
    // Nothing was promised, so nothing arrives on the reopen either.
    assert_eq!(after_reopen(base), (None, None));
}

#[test]
fn a_crash_after_the_intent_and_before_any_file_still_applies_the_whole_set() {
    if let Ok(base) = std::env::var(CHILD_DIR) {
        transact_then_die(&base);
    }
    let guard = died_at(
        "after-intent",
        "db::transaction_tests::a_crash_after_the_intent_and_before_any_file_still_applies_the_whole_set",
        "txn_crash_after_intent",
    );
    let base = guard.path();

    assert_eq!(transaction::pending(base).len(), 1, "the intent is not there to replay");
    assert_eq!(on_disk(base, "A", "K1"), None);
    assert_eq!(on_disk(base, "B", "K1"), None);

    assert_eq!(after_reopen(base), (Some(record("ONE")), Some(record("TWO"))));
    assert!(
        transaction::pending(base).is_empty(),
        "the intent outlived the replay that used it"
    );
}

#[test]
fn a_crash_between_the_two_halves_of_a_set_is_completed_on_the_next_open() {
    if let Ok(base) = std::env::var(CHILD_DIR) {
        transact_then_die(&base);
    }
    let guard = died_at(
        "between-files",
        "db::transaction_tests::a_crash_between_the_two_halves_of_a_set_is_completed_on_the_next_open",
        "txn_crash_between",
    );
    let base = guard.path();

    // The crash really did land between the halves: one file has its record and
    // the other does not. This is the state the whole feature exists to make
    // unobservable.
    assert_eq!(on_disk(base, "A", "K1"), Some(record("ONE")));
    assert_eq!(on_disk(base, "B", "K1"), None);
    assert_eq!(transaction::pending(base).len(), 1);

    // Opening the database is what makes it whole - and it is whole before the
    // first read, because the replay runs inside `Database::new`.
    assert_eq!(after_reopen(base), (Some(record("ONE")), Some(record("TWO"))));
    assert_eq!(on_disk(base, "B", "K1"), Some(record("TWO")));
    assert!(transaction::pending(base).is_empty());
}

#[test]
fn an_intent_naming_a_file_that_has_since_been_dropped_does_not_stop_the_database_opening() {
    let guard = TempDir::new("txn_replay_missing");
    let base = guard.path();
    {
        let db = two_files(base, "TXN");
        // Written by hand rather than by a crash: the point is the replay, and
        // an intent is the same bytes however it got there.
        transaction::write_intent(
            base,
            "TXN",
            &[
                Change::write("A", "K1", record("ONE")),
                Change::write("B", "K1", record("TWO")),
            ],
        )
        .unwrap();
        db.delete_table("B").unwrap();
    }

    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.logto("TXN").unwrap();
    // What can still be applied is; what has no file left to go to is not, and
    // the database opens either way.
    assert_eq!(in_memory(&db, "A", "K1"), Some(record("ONE")));
    assert!(transaction::pending(base).is_empty());
}

#[test]
fn an_intent_that_does_not_decode_is_discarded_rather_than_replayed() {
    let guard = TempDir::new("txn_replay_torn");
    let base = guard.path();
    {
        let _db = two_files(base, "TXN");
    }
    let dir = transaction::log_dir(base);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("00000000000000000001-0000000000-1.intent"), b"half a frame").unwrap();

    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.logto("TXN").unwrap();
    assert!(
        transaction::pending(base).is_empty(),
        "a file that is not an intent was left behind"
    );
    assert_eq!(in_memory(&db, "A", "K1"), None);
}
