//! The two ways a `WRITE` stops losing a record: a condition on what is there
//! now, and a key the engine chooses rather than the caller.
//!
//! The tests that matter here are the racing ones. A conditional write that
//! works when nothing else is happening has demonstrated nothing - the whole
//! claim is about what two writers do to each other, so two of these run real
//! threads against one file and assert on what the losers were told.

use crate::db::engine::Database;
use crate::db::error::DbError;
use crate::db::models::*;
use crate::db::sequence;
use crate::db::{Condition, Written};
use crate::test_support::{TempDir, isolated_config};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn open_account(base: &str, account: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account(account, Some(base)).unwrap();
    db.logto(account).unwrap();
    db
}

fn file(db: &Database, account: &str, name: &str, autokey: bool) {
    db.create_table_with(
        account,
        name,
        FileAttributes {
            // Durable, so every write here flushes before it returns. That is
            // the harder setting for the racing tests below - it puts a real
            // flush inside the contention rather than after it - and it is what
            // makes the hard-stop test's records be on disk when the child dies.
            durable: true,
            queue: None,
            autokey,
            directory: None,
        },
    )
    .unwrap();
}

fn record(text: &str) -> Record {
    Record::from_display_string(text)
}

/// A write through the engine's own path, as a connection would make it.
fn write(
    db: &Database,
    account: &str,
    name: &str,
    key: Option<&str>,
    text: &str,
    condition: &Condition,
) -> crate::db::DbResult<Written> {
    let handle = db.get_table_mut_for_account(account, name)?;
    db.write_record_in(account, name, &handle, key, record(text), false, condition)
}

fn delete(db: &Database, account: &str, name: &str, key: &str, condition: &Condition) -> crate::db::DbResult<bool> {
    let handle = db.get_table_mut_for_account(account, name)?;
    db.delete_record_in(account, name, &handle, key, false, condition)
}

fn stored(db: &Database, account: &str, name: &str, key: &str) -> Option<Record> {
    let handle = db.get_table_mut_for_account(account, name).ok()?;
    let table = handle.read();
    table.records.get(key).cloned()
}

fn version_of(db: &Database, account: &str, name: &str, key: &str) -> String {
    stored(db, account, name, key).expect("record must exist").version()
}

// ------------------------------------------------------------- the conditions

#[test]
fn if_absent_writes_a_key_that_is_free_and_refuses_one_that_is_taken() {
    let guard = TempDir::new("cond_if_absent");
    let db = open_account(guard.path(), "C1");
    file(&db, "C1", "LOG", false);

    let first = write(&db, "C1", "LOG", Some("K1"), "ONE", &Condition::IfAbsent).unwrap();
    assert_eq!(first.key, "K1");
    assert!(!first.minted, "the caller supplied this key");

    let second = write(&db, "C1", "LOG", Some("K1"), "TWO", &Condition::IfAbsent).unwrap_err();
    assert!(
        matches!(second, DbError::PreconditionFailed(_)),
        "a taken key is a collision, not a failure: {:?}",
        second
    );
    // The refusal wrote nothing. This is the whole point: the first record is
    // still the one that is there.
    assert_eq!(stored(&db, "C1", "LOG", "K1"), Some(record("ONE")));
}

#[test]
fn if_match_refuses_a_write_over_a_record_that_changed_since_it_was_read() {
    let guard = TempDir::new("cond_if_match");
    let db = open_account(guard.path(), "C2");
    file(&db, "C2", "LOG", false);
    write(&db, "C2", "LOG", Some("K1"), "ONE", &Condition::Always).unwrap();

    // What a read-modify-write client does: read, and keep the version.
    let read = version_of(&db, "C2", "LOG", "K1");

    // Somebody else gets in between.
    write(&db, "C2", "LOG", Some("K1"), "INTERLEAVED", &Condition::Always).unwrap();

    let refused = write(&db, "C2", "LOG", Some("K1"), "MINE", &Condition::IfMatch(read)).unwrap_err();
    assert!(matches!(refused, DbError::PreconditionFailed(_)), "{:?}", refused);
    assert_eq!(
        stored(&db, "C2", "LOG", "K1"),
        Some(record("INTERLEAVED")),
        "the refused write must not have overwritten the interleaved one"
    );

    // Reading again and retrying is what the code is for, and it succeeds.
    let current = version_of(&db, "C2", "LOG", "K1");
    write(&db, "C2", "LOG", Some("K1"), "MINE", &Condition::IfMatch(current)).unwrap();
    assert_eq!(stored(&db, "C2", "LOG", "K1"), Some(record("MINE")));
}

#[test]
fn a_write_hands_back_the_version_the_record_now_has() {
    let guard = TempDir::new("cond_version_back");
    let db = open_account(guard.path(), "C3");
    file(&db, "C3", "LOG", false);

    let written = write(&db, "C3", "LOG", Some("K1"), "ONE", &Condition::Always).unwrap();
    assert_eq!(
        written.version,
        version_of(&db, "C3", "LOG", "K1"),
        "the version a write reports must be the one a read would report"
    );
    // So a client can chain writes without reading in between.
    write(
        &db,
        "C3",
        "LOG",
        Some("K1"),
        "TWO",
        &Condition::IfMatch(written.version),
    )
    .unwrap();
    assert_eq!(stored(&db, "C3", "LOG", "K1"), Some(record("TWO")));
}

#[test]
fn if_match_on_a_delete_refuses_the_record_somebody_else_changed() {
    let guard = TempDir::new("cond_delete");
    let db = open_account(guard.path(), "C4");
    file(&db, "C4", "LOG", false);
    write(&db, "C4", "LOG", Some("K1"), "ONE", &Condition::Always).unwrap();
    let read = version_of(&db, "C4", "LOG", "K1");
    write(&db, "C4", "LOG", Some("K1"), "CHANGED", &Condition::Always).unwrap();

    let refused = delete(&db, "C4", "LOG", "K1", &Condition::IfMatch(read)).unwrap_err();
    assert!(matches!(refused, DbError::PreconditionFailed(_)), "{:?}", refused);
    assert_eq!(
        stored(&db, "C4", "LOG", "K1"),
        Some(record("CHANGED")),
        "deleting a record somebody else changed is the same bug in a different hat"
    );

    let current = version_of(&db, "C4", "LOG", "K1");
    assert!(delete(&db, "C4", "LOG", "K1", &Condition::IfMatch(current)).unwrap());
    assert_eq!(stored(&db, "C4", "LOG", "K1"), None);
}

#[test]
fn a_condition_on_a_key_that_is_not_there_says_which_way_it_failed() {
    let guard = TempDir::new("cond_absent_record");
    let db = open_account(guard.path(), "C5");
    file(&db, "C5", "LOG", false);

    // Nothing to match, so `if_match` refuses rather than creating the record:
    // a caller that meant "create it" has `if_absent` to say so.
    let refused = write(
        &db,
        "C5",
        "LOG",
        Some("GONE"),
        "X",
        &Condition::IfMatch("0123456789abcdef".to_string()),
    )
    .unwrap_err();
    assert!(matches!(refused, DbError::PreconditionFailed(_)), "{:?}", refused);
    assert_eq!(stored(&db, "C5", "LOG", "GONE"), None);

    // And an unconditional delete of a key that is not there is still not an
    // error, which is what `DELETE` has always done.
    assert!(!delete(&db, "C5", "LOG", "GONE", &Condition::Always).unwrap());
}

#[test]
fn a_version_changes_with_the_record_and_only_with_the_record() {
    let one = record("A^B");
    assert_eq!(
        one.version(),
        record("A^B").version(),
        "the same bytes are the same version"
    );
    assert_ne!(one.version(), record("A^C").version());
    // A field added at the end changes the bytes, so it changes the version -
    // which is what stops a write that only appends from looking unchanged.
    assert_ne!(one.version(), record("A^B^").version());
}

/// The test the whole of #108 is about: two writers racing to create one key.
#[test]
fn two_writers_racing_to_create_one_key_produce_one_winner_and_one_collision() {
    let guard = TempDir::new("cond_race_create");
    let db = Arc::new(open_account(guard.path(), "C6"));
    file(&db, "C6", "LOG", false);

    let winners = Arc::new(AtomicUsize::new(0));
    let collisions = Arc::new(AtomicUsize::new(0));
    // More than two, because two threads can miss each other by luck and the
    // test would pass having raced nothing. Every one of these but one has to
    // be told it lost.
    let racers = 8;
    std::thread::scope(|scope| {
        for racer in 0..racers {
            let db = Arc::clone(&db);
            let winners = Arc::clone(&winners);
            let collisions = Arc::clone(&collisions);
            scope.spawn(move || {
                let body = format!("RACER-{}", racer);
                match write(&db, "C6", "LOG", Some("NEXT"), &body, &Condition::IfAbsent) {
                    Ok(_) => winners.fetch_add(1, Ordering::Relaxed),
                    Err(DbError::PreconditionFailed(_)) => collisions.fetch_add(1, Ordering::Relaxed),
                    Err(e) => panic!("a racer failed for the wrong reason: {:?} / {}", e, e),
                };
            });
        }
    });

    assert_eq!(
        winners.load(Ordering::Relaxed),
        1,
        "exactly one writer may create the key"
    );
    assert_eq!(collisions.load(Ordering::Relaxed), racers - 1);
    // And the record that is there is one of the racers' own, whole: nothing
    // was half-applied and nothing overwrote the winner.
    let survivor = stored(&db, "C6", "LOG", "NEXT").expect("the winner's record must be there");
    assert!(
        (0..racers).any(|racer| survivor == record(&format!("RACER-{}", racer))),
        "the surviving record is not one that was written: {:?}",
        survivor
    );
}

/// The read-modify-write half, raced: every writer reads, changes and writes
/// back, and no change may be lost without being reported.
#[test]
fn a_raced_read_modify_write_loses_no_change_without_saying_so() {
    let guard = TempDir::new("cond_race_rmw");
    let db = Arc::new(open_account(guard.path(), "C7"));
    file(&db, "C7", "COUNTER", false);
    write(&db, "C7", "COUNTER", Some("N"), "0", &Condition::Always).unwrap();

    let applied = Arc::new(AtomicUsize::new(0));
    let writers = 8;
    std::thread::scope(|scope| {
        for _ in 0..writers {
            let db = Arc::clone(&db);
            let applied = Arc::clone(&applied);
            scope.spawn(move || {
                // One attempt each, deliberately not retried: the property
                // being tested is that a refusal is reported, not that a retry
                // loop eventually gets there.
                let seen = version_of(&db, "C7", "COUNTER", "N");
                let current: u64 = stored(&db, "C7", "COUNTER", "N")
                    .unwrap()
                    .to_display_string()
                    .parse()
                    .unwrap();
                let next = (current + 1).to_string();
                if write(&db, "C7", "COUNTER", Some("N"), &next, &Condition::IfMatch(seen)).is_ok() {
                    applied.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    let applied = applied.load(Ordering::Relaxed);
    let held: u64 = stored(&db, "C7", "COUNTER", "N")
        .unwrap()
        .to_display_string()
        .parse()
        .unwrap();
    // The count on disk is exactly the number of writes that were told they
    // succeeded. Before the condition existed this was `writers` successes and
    // a count of anything from 1 upwards.
    assert_eq!(
        held, applied as u64,
        "{} writes were acknowledged but the record counted {}",
        applied, held
    );
    assert!(applied >= 1 && applied <= writers);
}

// ---------------------------------------------------------------- minted keys

#[test]
fn a_keyless_write_on_an_autokey_file_mints_the_key_and_reports_it() {
    let guard = TempDir::new("auto_mint");
    let db = open_account(guard.path(), "A1");
    file(&db, "A1", "EVENTS", true);

    let written = write(&db, "A1", "EVENTS", None, "FIRST", &Condition::Always).unwrap();
    assert!(written.minted, "the server chose this key");
    assert_eq!(written.key.len(), sequence::KEY_DIGITS);
    assert!(written.key.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(stored(&db, "A1", "EVENTS", &written.key), Some(record("FIRST")));
}

#[test]
fn a_keyless_write_on_a_file_without_the_flag_is_refused_with_what_to_do_instead() {
    let guard = TempDir::new("auto_refused");
    let db = open_account(guard.path(), "A2");
    file(&db, "A2", "PLAIN", false);

    let refused = write(&db, "A2", "PLAIN", None, "X", &Condition::Always).unwrap_err();
    let DbError::InvalidRequest(message) = &refused else {
        panic!(
            "a file that does not mint keys must refuse, not invent one: {:?}",
            refused
        );
    };
    assert!(
        message.contains("AUTOKEY"),
        "the refusal has to name the flag that would allow it: {}",
        message
    );
    assert!(
        db.get_table_mut_for_account("A2", "PLAIN")
            .unwrap()
            .read()
            .records
            .is_empty()
    );
}

#[test]
fn minted_keys_read_back_in_the_order_they_were_written() {
    let guard = TempDir::new("auto_order");
    let db = open_account(guard.path(), "A3");
    file(&db, "A3", "EVENTS", true);

    let mut minted = Vec::new();
    for n in 0..50 {
        minted.push(
            write(&db, "A3", "EVENTS", None, &format!("E{}", n), &Condition::Always)
                .unwrap()
                .key,
        );
    }
    // Sorting as *text* is the guarantee: a caller reading a key range gets
    // arrival order without knowing how wide the counter is or parsing it.
    let mut sorted = minted.clone();
    sorted.sort();
    assert_eq!(sorted, minted, "minted keys must already be in text order");

    // And the records under them are the ones written, in that order.
    let bodies: Vec<String> = minted
        .iter()
        .map(|key| stored(&db, "A3", "EVENTS", key).unwrap().to_display_string())
        .collect();
    assert_eq!(bodies, (0..50).map(|n| format!("E{}", n)).collect::<Vec<_>>());
}

/// The test #109 is about: N appenders at once, N records, N distinct keys.
#[test]
fn concurrent_appenders_all_succeed_with_distinct_keys() {
    let guard = TempDir::new("auto_race");
    let db = Arc::new(open_account(guard.path(), "A4"));
    file(&db, "A4", "EVENTS", true);

    let appenders = 8;
    let each = 25;
    let keys = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for appender in 0..appenders {
            let db = Arc::clone(&db);
            let keys = &keys;
            scope.spawn(move || {
                let mut mine = Vec::new();
                for n in 0..each {
                    let body = format!("{}-{}", appender, n);
                    mine.push(write(&db, "A4", "EVENTS", None, &body, &Condition::Always).unwrap().key);
                }
                keys.lock().unwrap().extend(mine);
            });
        }
    });

    let keys = keys.into_inner().unwrap();
    let distinct: HashSet<&String> = keys.iter().collect();
    assert_eq!(keys.len(), appenders * each);
    assert_eq!(distinct.len(), keys.len(), "two appenders were given the same key");
    // No losses: every key still holds a record, so nothing was overwritten by
    // a key that came round twice.
    let handle = db.get_table_mut_for_account("A4", "EVENTS").unwrap();
    let held = handle.read().records.len();
    assert_eq!(held, keys.len());
}

#[test]
fn a_key_written_by_hand_is_stepped_over_rather_than_minted_again() {
    let guard = TempDir::new("auto_by_hand");
    let db = open_account(guard.path(), "A5");
    file(&db, "A5", "EVENTS", true);

    // A minted key put there by hand - which a repair, an import or a restore
    // does - has to pull the counter past it, or the next mint overwrites it.
    let ahead = sequence::format_key(u64::MAX / 2);
    write(&db, "A5", "EVENTS", Some(&ahead), "BY HAND", &Condition::Always).unwrap();
    let minted = write(&db, "A5", "EVENTS", None, "AFTER", &Condition::Always)
        .unwrap()
        .key;

    assert!(minted > ahead, "{} does not come after {}", minted, ahead);
    assert_eq!(stored(&db, "A5", "EVENTS", &ahead), Some(record("BY HAND")));
}

#[test]
fn the_flag_survives_a_restart_and_can_be_turned_off_again() {
    let guard = TempDir::new("auto_flag_restart");
    let base = guard.path();
    {
        let db = open_account(base, "A6");
        file(&db, "A6", "EVENTS", true);
        write(&db, "A6", "EVENTS", None, "ONE", &Condition::Always).unwrap();
        db.save().unwrap();
    }
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.logto("A6").unwrap();
    assert!(db.is_table_autokey_for_account("A6", "EVENTS"));
    write(&db, "A6", "EVENTS", None, "TWO", &Condition::Always).unwrap();

    // Turned off, the file refuses a keyless write again and the counter's file
    // goes with it, so nothing on disk describes a counter it has not got.
    db.set_file_attributes(
        "A6",
        "EVENTS",
        FileAttributes {
            durable: true,
            queue: None,
            autokey: false,
            directory: None,
        },
    )
    .unwrap();
    assert!(matches!(
        write(&db, "A6", "EVENTS", None, "THREE", &Condition::Always).unwrap_err(),
        DbError::InvalidRequest(_)
    ));
    assert!(!std::path::Path::new(&format!("{}/EVENTS/autokey", base)).exists());
}

#[test]
fn a_queue_file_and_a_directory_file_are_never_autokey_files_as_well() {
    let guard = TempDir::new("auto_exclusive");
    let db = open_account(guard.path(), "A7");

    // A hand-edited DIR entry claiming both: the queue wins, because it is the
    // half that says where the records are handed out from, and two counters on
    // one file is the ambiguity the flag combination is refused to avoid.
    let mut entry = Record::new();
    while entry.fields.len() <= DIR_AUTOKEY_IDX {
        entry.fields.push(Field::default());
    }
    entry.fields[DIR_TYPE_IDX].values = vec![Value::text(DIR_TYPE_FILE)];
    entry.fields[DIR_QUEUE_IDX].values = vec![Value::text("Y")];
    entry.fields[DIR_AUTOKEY_IDX].values = vec![Value::text("Y")];
    let attributes = FileAttributes::of(&entry);
    assert!(attributes.queue.is_some());
    assert!(!attributes.autokey);

    // And a directory file reads as neither, whatever attribute 7 says.
    entry.fields[DIR_TYPE_IDX].values = vec![Value::text(DIR_TYPE_DIRECTORY)];
    let attributes = FileAttributes::of(&entry);
    assert!(attributes.is_directory());
    assert!(!attributes.autokey);

    // The flag round-trips through a DIR entry on an ordinary file.
    file(&db, "A7", "EVENTS", true);
    let written = FileAttributes::of(&FileAttributes::of(&entry).to_record());
    assert!(!written.autokey, "a directory file's entry must not claim to mint keys");
    assert!(db.file_attributes_for_account("A7", "EVENTS").autokey);
}

// ------------------------------------------------- the counter after a crash

/// Set on the re-executed test binary to make it play the victim.
const CHILD_DIR: &str = "SRP_AUTOKEY_CHILD_DIR";

/// Appends records, flushes them, and dies where a power cut would: SIGKILL, so
/// nothing is unwound, no destructor runs and no further write can happen.
fn append_then_die(base: &str) -> ! {
    let db = open_account(base, "A8");
    file(&db, "A8", "EVENTS", true);
    let mut minted = Vec::new();
    for n in 0..5 {
        minted.push(
            write(&db, "A8", "EVENTS", None, &format!("BEFORE-{}", n), &Condition::Always)
                .unwrap()
                .key,
        );
    }
    // Handed to the parent through the records themselves rather than through
    // the pipe, because after the kill the only channel left is the disk.
    db.save().unwrap();
    // Deleting the highest key is what makes the test about the *counter*: with
    // the record gone, a counter rebuilt from the records alone would hand that
    // key straight back out.
    delete(&db, "A8", "EVENTS", minted.last().unwrap(), &Condition::Always).unwrap();
    db.save().unwrap();
    let pid = std::process::id().to_string();
    let _ = std::process::Command::new("kill").args(["-9", &pid]).status();
    std::thread::sleep(std::time::Duration::from_secs(30));
    unreachable!("the process should have been killed");
}

#[test]
fn the_counter_survives_a_hard_stop_without_handing_a_key_out_twice() {
    if let Ok(base) = std::env::var(CHILD_DIR) {
        append_then_die(&base);
    }
    let guard = TempDir::new("auto_hard_stop");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::write_tests::the_counter_survives_a_hard_stop_without_handing_a_key_out_twice",
            "--nocapture",
        ])
        .env(CHILD_DIR, guard.path())
        .status()
        .unwrap();
    assert!(
        status.code().is_none(),
        "the child should have died from a signal, not exited: {:?}",
        status
    );

    let base = guard.path();
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.logto("A8").unwrap();
    let handle = db.get_table_mut_for_account("A8", "EVENTS").unwrap();
    let survived: HashSet<String> = handle.read().records.keys().cloned().collect();
    assert_eq!(survived.len(), 4, "the child's records did not reach the disk");

    // The deleted key is gone, and it must not come back. Appending now is the
    // moment a counter that had been rebuilt from the records would reuse it.
    let highest = survived.iter().max().cloned().unwrap();
    for n in 0..5 {
        let minted = write(&db, "A8", "EVENTS", None, &format!("AFTER-{}", n), &Condition::Always)
            .unwrap()
            .key;
        assert!(
            !survived.contains(&minted),
            "{} was handed out again after the hard stop",
            minted
        );
        assert!(
            minted > highest,
            "{} does not come after the records that survived ({})",
            minted,
            highest
        );
    }
}

#[test]
fn file_stats_says_where_the_counter_got_to_without_loading_the_file() {
    let guard = TempDir::new("auto_stats");
    let base = guard.path();
    let db = open_account(base, "A9");
    file(&db, "A9", "EVENTS", true);
    file(&db, "A9", "PLAIN", false);

    assert!(
        db.file_statistics("A9", "PLAIN").unwrap().autokey.is_none(),
        "a file that does not mint keys has no counter to report"
    );

    let minted = write(&db, "A9", "EVENTS", None, "ONE", &Condition::Always).unwrap().key;
    let stats = db
        .file_statistics("A9", "EVENTS")
        .unwrap()
        .autokey
        .expect("an autokey file");
    assert!(stats.loaded, "the file is open, so the counter is the one in memory");
    assert!(
        stats.next_key > minted,
        "the next key must come after the one just handed out: {} then {}",
        minted,
        stats.next_key
    );
    assert_eq!(stats.next_key.len(), sequence::KEY_DIGITS);

    // Dropped from the cache, the counter still reads back - from the `autokey`
    // file, and without pulling the records in behind it.
    db.save().unwrap();
    db.clear_loaded_tables();
    let stats = db
        .file_statistics("A9", "EVENTS")
        .unwrap()
        .autokey
        .expect("an autokey file");
    assert!(!stats.loaded, "nothing is in memory, so this came off the disk");
    assert!(
        stats.next_sequence > sequence::key_sequence(&minted).unwrap(),
        "the persisted counter is behind the key it minted"
    );
    assert!(
        !db.is_table_loaded("EVENTS"),
        "describing a file must not be what loads it"
    );

    // And the key it promised is one the file will actually accept.
    let next = write(&db, "A9", "EVENTS", None, "TWO", &Condition::Always).unwrap().key;
    assert!(
        next >= stats.next_key,
        "{} was reported as the next key and {} was handed out",
        stats.next_key,
        next
    );
}
