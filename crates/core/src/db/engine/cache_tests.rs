//! Tests for the table cache: what the eviction rules protect, and what a
//! cached table is checked against to decide it is still the copy on disk.
//!
//! These sit next to [`super::cache`] rather than in `db/engine_tests.rs`
//! because they reach the cache's own interface - `disk_stamp`, `file_stamp`,
//! `account_tables` - which is `pub(super)` and invisible from a sibling of
//! `engine`. What they cannot reach is `evict_if_over_budget` itself, so the
//! eviction rules are driven the way a request drives them: by loading one
//! table too many and asking which one survived.

use super::Database;
use crate::db::DbError;
use crate::db::models::*;
use crate::test_support::{TempDir, isolated_config};

/// A database on a fresh directory, logged in to `ACCT`, with room for
/// `max_loaded` tables. The directory is returned so it outlives the database.
fn open(label: &str, max_loaded: usize) -> (TempDir, Database) {
    let dir = TempDir::new(label);
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACCT", None).unwrap();
    db.logto("ACCT").unwrap();
    db.max_loaded = max_loaded;
    (dir, db)
}

/// Creates the named files and leaves the cache empty, so a test starts from a
/// known number of loaded tables rather than from whatever creating them left.
fn create_cold(db: &Database, names: &[&str]) {
    for name in names {
        db.create_table(name).unwrap();
    }
    db.clear_loaded_tables();
}

/// Writes one record and marks the file dirty, without flushing it.
fn buffer_record(db: &Database, file: &str, key: &str, value: &str) {
    let handle = db.get_table_mut(file).unwrap();
    let mut table = handle.write();
    table
        .records
        .insert(key.to_string(), Record::from_display_string(value));
    table.touch_all();
}

#[test]
fn a_table_somebody_is_working_on_is_not_the_one_evicted() {
    let (_dir, db) = open("evict_held", 2);
    create_cold(&db, &["T1", "T2", "T3"]);

    // T1 is the coldest, so it is what eviction reaches for first. The handle
    // held here is a third reference to it - the map's, eviction's own clone,
    // and this one - which is what makes it ineligible.
    let held = db.get_table_mut("T1").unwrap();
    db.get_table_mut("T2").unwrap();
    assert_eq!(db.loaded_table_count(), 2);

    db.get_table_mut("T3").unwrap();

    assert!(db.is_table_loaded("T1"), "a table in use must survive eviction");
    assert!(!db.is_table_loaded("T2"), "the next coldest goes in its place");
    assert!(db.is_table_loaded("T3"));

    // Skipping T1 re-queued it at the *young* end rather than dropping it out
    // of the order, so being in use at the wrong moment buys a table a full
    // trip round the queue. Releasing the handle does not make it the next to
    // go: the other resident is colder.
    drop(held);
    db.get_table_mut("T2").unwrap();
    assert!(db.is_table_loaded("T1"), "a skipped table is re-queued, not forgotten");
    assert!(!db.is_table_loaded("T3"), "T3 is the cold end now");

    // Once it has aged out again it is evicted like anything else.
    db.get_table_mut("T3").unwrap();
    assert!(!db.is_table_loaded("T1"), "and nothing keeps it in the cache for good");
}

#[test]
fn a_cache_whose_tables_are_all_in_use_goes_over_budget_rather_than_lose_one() {
    let (_dir, db) = open("evict_all_held", 1);
    create_cold(&db, &["T1", "T2", "T3"]);

    // Evicting a table another thread holds would let a third thread load a
    // second copy of it, and the two would overwrite each other. The budget is
    // what gives way instead.
    let _h1 = db.get_table_mut("T1").unwrap();
    let _h2 = db.get_table_mut("T2").unwrap();
    let _h3 = db.get_table_mut("T3").unwrap();

    assert_eq!(db.loaded_table_count(), 3, "budget of 1, three tables in use");
}

#[test]
fn an_evicted_table_keeps_the_changes_it_had_buffered() {
    let (_dir, db) = open("evict_flush", 1);
    create_cold(&db, &["T1", "T2"]);

    buffer_record(&db, "T1", "K1", "V1");
    assert!(db.has_pending_writes(), "the record is only in the cached copy");

    // Loading T2 puts the cache over budget and takes T1, which is dirty.
    db.get_table_mut("T2").unwrap();
    assert!(!db.is_table_loaded("T1"), "T1 must have been evicted");

    // Reading T1 again is a fresh load from disk, so this is what eviction
    // wrote out rather than what it dropped.
    let reloaded = db.get_table("T1").unwrap();
    let record = reloaded.read().records.get("K1").cloned();
    assert_eq!(
        record.map(|r| r.to_display_string()),
        Some("V1".to_string()),
        "eviction must write a dirty table out before dropping it"
    );
}

#[test]
fn a_table_served_from_the_read_path_is_not_the_next_one_evicted() {
    let (_dir, db) = open("read_path_lru", 2);
    create_cold(&db, &["T1", "T2", "T3"]);

    db.get_table_mut("T1").unwrap();
    db.get_table_mut("T2").unwrap();

    // The read path serves T1 without loading or refreshing it, and moves it
    // off the cold end. The handle it returns is dropped within this statement,
    // so what keeps T1 below is the eviction order and not the rule above.
    assert!(db.table_ready_for_read("ACCT", "T1").is_some());

    db.get_table_mut("T3").unwrap();

    assert!(db.is_table_loaded("T1"), "a table just read is not the coldest");
    assert!(!db.is_table_loaded("T2"), "T2 is the cold end now");
}

#[test]
fn a_rewritten_section_gets_a_new_stamp_even_when_the_clock_does_not_move() {
    let (_dir, db) = open("stamp_version", 8);
    db.create_table("T").unwrap();

    buffer_record(&db, "T", "K1", "V1");
    db.save().unwrap();
    let first = db.disk_stamp("ACCT", "T");

    buffer_record(&db, "T", "K2", "V2");
    db.save().unwrap();
    let second = db.disk_stamp("ACCT", "T");

    // A hashed section spreads its records over many files, so the stamp reads
    // the flush counter out of the meta file rather than a length. Two writes
    // close enough together to share a timestamp still get different stamps,
    // which is the whole reason the counter is used.
    assert_ne!(first, second, "a rewrite must change the stamp");
    assert!(
        second.data_len > first.data_len,
        "the flush counter must advance: {} then {}",
        first.data_len,
        second.data_len
    );
}

#[test]
fn a_file_that_is_not_there_stamps_as_absent() {
    let (dir, _db) = open("stamp_missing", 8);

    // The stamp of a file that does not exist has to be a value, not an error:
    // it is what a cached table is compared against when its file is deleted,
    // and what a file appearing later has to differ from.
    let (modified, len) = Database::file_stamp(&format!("{}/no-such-file", dir.path()));

    assert!(modified.is_none());
    assert_eq!(len, 0);
}

#[test]
fn listing_the_files_of_an_account_that_does_not_exist_is_an_error() {
    let (_dir, db) = open("unknown_account", 8);

    assert!(matches!(db.account_tables("NOPE"), Err(DbError::AccountNotFound(_))));
    assert!(!db.account_has_table("NOPE", "T"), "and reports no files");
}
