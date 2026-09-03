//! Secondary indexes: what they answer, what keeps them honest, and what they
//! do when they cannot be trusted.
//!
//! The property every test here is built around is that an index is an
//! *optimisation*: turning one on must not change a single answer. Most of
//! these therefore run the same query twice - once indexed, once not - and
//! assert the two are identical, rather than asserting what the indexed run
//! happens to return.

use crate::db::engine::Database;
use crate::db::index;
use crate::db::models::*;
use crate::db::{DbError, IndexStats};
use crate::test_support::{TempDir, isolated_config};
use std::collections::HashMap;
use std::fs;

const ACCOUNT: &str = "IDXTEST";

/// Opens the fixture account, creating it the first time. Reopening the same
/// directory is how the restart tests get a second, cold `Database`.
fn open_account(base: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    let _ = db.create_account(ACCOUNT, Some(base));
    db.logto(ACCOUNT).unwrap();
    db
}

/// A file with a `NAME`/`CITY`/`ROLES` dictionary and `count` records, where
/// `CITY` cycles over ten values and `ROLES` is multivalued.
fn build_file(db: &Database, file: &str, count: usize) {
    db.create_table(file).unwrap();
    let handle = db.get_table_mut(file).unwrap();
    {
        let mut table = handle.write();
        for (name, position) in [("NAME", 1), ("CITY", 2), ("ROLES", 3)] {
            table.dictionary.insert(
                name.to_string(),
                Record::from_display_string(&format!("{}^{}^L^20", position, name)),
            );
        }
        table.mark_dict_dirty();
        for i in 0..count {
            table.insert_record(
                &format!("K{:04}", i),
                Record::from_display_string(&format!("NAME{}^CITY{}^ROLE{}]ROLE{}", i, i % 10, i % 3, i % 7)),
            );
        }
    }
    db.save().unwrap();
}

fn query(db: &Database, file: &str, clause: &[&str]) -> Vec<String> {
    let node = db.parse_query(file, clause).unwrap();
    db.query_for_account(ACCOUNT, file, false, &node, None)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// The same query with the index in place and with it dropped, so a difference
/// between them is the failure rather than a hand-written expectation.
fn assert_same_with_and_without_index(db: &Database, file: &str, field: &str, clause: &[&str]) {
    let indexed = query(db, file, clause);
    db.drop_index_for_account(ACCOUNT, file, field).unwrap();
    let scanned = query(db, file, clause);
    db.create_index_for_account(ACCOUNT, file, field).unwrap();
    assert_eq!(indexed, scanned, "indexed and scanned results differ for {:?}", clause);
}

fn index_of(db: &Database, file: &str, field: &str) -> IndexStats {
    db.index_statistics(ACCOUNT, file)
        .unwrap()
        .into_iter()
        .find(|stats| stats.field == field)
        .unwrap_or_else(|| panic!("no index on {}", field))
}

#[test]
fn an_index_is_created_listed_and_dropped() {
    let guard = TempDir::new("index_lifecycle");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 40);

    let stats = db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    assert_eq!(stats.field, "CITY");
    // CITY is attribute 2 and cycles over ten values across forty records.
    assert_eq!(stats.attribute, 2);
    assert_eq!(stats.values, 10);
    assert_eq!(stats.postings, 40);
    assert_eq!(stats.largest_postings, 4);
    assert!(!stats.stale);

    let listed = db.index_statistics(ACCOUNT, "PEOPLE").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], stats);

    db.drop_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    assert!(db.index_statistics(ACCOUNT, "PEOPLE").unwrap().is_empty());
    // The section is gone from disk, not merely forgotten in memory.
    assert!(index::indexed_fields(&format!("{}/PEOPLE", guard.path())).is_empty());
}

#[test]
fn an_index_survives_a_restart() {
    let guard = TempDir::new("index_restart");
    {
        let db = open_account(guard.path());
        build_file(&db, "PEOPLE", 40);
        db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
        db.save().unwrap();
    }

    let db = open_account(guard.path());
    // Read through a query first, so the file is loaded from disk exactly as an
    // ordinary client would load it.
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", "CITY3"]).len(), 4);
    let stats = index_of(&db, "PEOPLE", "CITY");
    assert_eq!(stats.values, 10);
    assert_eq!(stats.postings, 40);
    assert!(!stats.stale, "an index written cleanly must not come back stale");
}

#[test]
fn an_indexed_query_returns_exactly_what_a_scan_returns() {
    let guard = TempDir::new("index_identical");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 60);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    db.create_index_for_account(ACCOUNT, "PEOPLE", "ROLES").unwrap();

    for clause in [
        vec!["WITH", "CITY", "=", "CITY3"],
        vec!["WITH", "CITY", "=", "NOWHERE"],
        vec!["WITH", "CITY", "=", "CITY3", "AND", "NAME", "=", "NAME13"],
        vec!["WITH", "CITY", "=", "CITY3", "OR", "CITY", "=", "CITY4"],
        vec!["WITH", "CITY", "=", "CITY3", "OR", "NAME", "=", "NAME7"],
        vec!["WITH", "CITY", "#", "CITY3"],
        // Wildcards are not what an equality index answers; the scan behind it
        // still has to.
        vec!["WITH", "CITY", "=", "CITY]"],
        vec!["WITH", "CITY", "=", "[3"],
        vec!["WITH", "ROLES", "=", "ROLE2"],
        vec!["WITH", "ROLES", "=", "ROLE2", "AND", "CITY", "=", "CITY4"],
    ] {
        assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &clause);
    }
}

#[test]
fn writes_updates_and_deletes_keep_the_index_consistent() {
    let guard = TempDir::new("index_maintenance");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 30);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    let write = |key: &str, data: &str| {
        db.get_table_mut("PEOPLE")
            .unwrap()
            .write()
            .insert_record(key, Record::from_display_string(data));
    };

    // A value moving between records: K0000 leaves CITY0, a new record joins it.
    write("K0000", "NAME0^CITY9^ROLE0");
    write("NEW", "NEWNAME^CITY0^ROLE0");
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY0"]);
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY9"]);

    // A record losing the field entirely. It then matches `= ""`, exactly as it
    // does without an index, which is why the empty value is indexed at all.
    write("K0001", "NAME1");
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", ""]);
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY1"]);

    // A deletion.
    db.get_table_mut("PEOPLE").unwrap().write().remove_record("K0002");
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY2"]);

    // And the same after a flush and a reload, so the maintained form and the
    // form that comes back off disk agree.
    db.save().unwrap();
    db.clear_loaded_tables();
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY0"]);
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", ""]);
}

#[test]
fn a_multivalued_field_indexes_every_value() {
    let guard = TempDir::new("index_multivalue");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 21);
    let stats = db.create_index_for_account(ACCOUNT, "PEOPLE", "ROLES").unwrap();

    // Each record carries ROLE(i%3) and ROLE(i%7), so the values are ROLE0 to
    // ROLE6. The two coincide for i of 0, 1 and 2, which is why twenty-one
    // records yield thirty-nine postings rather than forty-two.
    assert_eq!(stats.values, 7);
    assert_eq!(stats.postings, 39);

    for role in ["ROLE0", "ROLE2", "ROLE6"] {
        assert_same_with_and_without_index(&db, "PEOPLE", "ROLES", &["WITH", "ROLES", "=", role]);
    }
    // A sub-valued position counts as its own value too.
    db.get_table_mut("PEOPLE")
        .unwrap()
        .write()
        .insert_record("SUB", Record::from_display_string("NAMEX^CITYX^ROLE0]TEAM1\\SITE2"));
    for role in ["ROLE0", "TEAM1", "SITE2"] {
        assert_same_with_and_without_index(&db, "PEOPLE", "ROLES", &["WITH", "ROLES", "=", role]);
    }
}

#[test]
fn an_index_on_a_converted_field_is_looked_up_with_the_stored_value() {
    let guard = TempDir::new("index_conversion");
    let db = open_account(guard.path());
    db.create_table("PRODUCTS").unwrap();
    {
        let handle = db.get_table_mut("PRODUCTS").unwrap();
        let mut table = handle.write();
        table
            .dictionary
            .insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
        // PRICE stores pennies and displays pounds, so a query is written in
        // pounds and the index has to be looked up in pennies.
        table
            .dictionary
            .insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10^^^^MD2"));
        table.mark_dict_dirty();
        table.insert_record("P1", Record::from_display_string("Laptop^120000"));
        table.insert_record("P2", Record::from_display_string("Mouse^2500"));
        table.insert_record("P3", Record::from_display_string("Cable^2500"));
    }
    db.create_index_for_account(ACCOUNT, "PRODUCTS", "PRICE").unwrap();

    assert_eq!(query(&db, "PRODUCTS", &["WITH", "PRICE", "=", "25.00"]), ["P2", "P3"]);
    assert_same_with_and_without_index(&db, "PRODUCTS", "PRICE", &["WITH", "PRICE", "=", "25.00"]);
    assert_same_with_and_without_index(&db, "PRODUCTS", "PRICE", &["WITH", "PRICE", "=", "1200.00"]);
}

#[test]
fn an_index_narrows_a_select_list_and_a_sorted_query_the_same_way() {
    let guard = TempDir::new("index_select");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 50);

    let handle = db.get_table_mut("PEOPLE").unwrap();
    let node = db.parse_query("PEOPLE", &["WITH", "CITY", "=", "CITY4"]).unwrap();
    let specs = [SortSpec {
        field_name: "NAME".to_string(),
        descending: true,
    }];
    let scanned = Database::select_entries_in(&handle.read(), false, Some(&node), None, None, &specs);
    drop(handle);

    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    let handle = db.get_table_mut("PEOPLE").unwrap();
    let indexed = Database::select_entries_in(&handle.read(), false, Some(&node), None, None, &specs);
    assert_eq!(indexed, scanned);
}

#[test]
fn a_caller_supplied_key_list_keeps_its_own_order_and_membership() {
    let guard = TempDir::new("index_keys_to_filter");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 30);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    let node = db.parse_query("PEOPLE", &["WITH", "CITY", "=", "CITY5"]).unwrap();
    let keys = vec![
        "K0025".to_string(),
        "K0005".to_string(),
        "K0006".to_string(),
        "MISSING".to_string(),
    ];
    let handle = db.get_table_mut("PEOPLE").unwrap();
    let filtered = Database::query_keys_in(&handle.read(), false, &node, Some(&keys));
    // The caller's order survives, the non-match and the absent key do not.
    assert_eq!(filtered, ["K0025", "K0005"]);
}

#[test]
fn an_index_whose_field_moves_in_the_dictionary_is_rebuilt_before_it_is_trusted() {
    let guard = TempDir::new("index_dict_move");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 20);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    {
        // CITY now describes attribute 1, which holds the names.
        let handle = db.get_table_mut("PEOPLE").unwrap();
        let mut table = handle.write();
        table
            .dictionary
            .insert("CITY".to_string(), Record::from_display_string("1^CITY^L^20"));
        table.mark_dict_dirty();
        assert!(
            table.indexes["CITY"].needs_rebuild,
            "moving the field must mark the index for a rebuild"
        );
        // Nothing consults an index in that state.
        assert!(table.index_candidates("CITY", "CITY3").is_none());
    }

    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", "NAME3"]), ["K0003"]);
    db.save().unwrap();
    let stats = index_of(&db, "PEOPLE", "CITY");
    assert_eq!(stats.attribute, 1);
    assert!(!stats.stale);
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", "NAME3"]), ["K0003"]);
}

#[test]
fn an_index_left_behind_by_a_crash_is_detectably_stale_and_rebuilt() {
    let guard = TempDir::new("index_stale");
    let base = guard.path();
    {
        let db = open_account(base);
        build_file(&db, "PEOPLE", 20);
        db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
        db.save().unwrap();
    }

    // Exactly what a crash between the data flush and the index flush leaves:
    // records that have moved on, and an index whose `state` still names the
    // version before them.
    let file_dir = format!("{}/PEOPLE", base);
    let section = index::section_path(&file_dir, "CITY");
    let state = index::read_state(&section).unwrap();
    index::write_state(
        &section,
        &index::IndexState {
            data_version: state.data_version - 1,
            ..state
        },
        crate::db::hashfile::FsyncPolicy::Never,
    )
    .unwrap();

    // Seen from outside, before anything loads the file: stale, and said so.
    let cold = Database::new(base, Some(isolated_config())).unwrap();
    assert!(
        cold.index_statistics(ACCOUNT, "PEOPLE").unwrap()[0].stale,
        "an index whose state does not match the data must report as stale"
    );

    let db = open_account(base);
    // Loading the file rebuilds it, and the answers are the ones a scan gives.
    assert_same_with_and_without_index(&db, "PEOPLE", "CITY", &["WITH", "CITY", "=", "CITY3"]);
    assert!(!index_of(&db, "PEOPLE", "CITY").stale);
}

#[test]
fn a_damaged_index_section_costs_the_index_and_not_the_file() {
    let guard = TempDir::new("index_damaged");
    let base = guard.path();
    {
        let db = open_account(base);
        build_file(&db, "PEOPLE", 20);
        db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
        db.save().unwrap();
    }

    // Corrupt every group of the index section, leaving the records untouched.
    let section_dir = crate::db::hashfile::section_dir(&index::section_path(&format!("{}/PEOPLE", base), "CITY"));
    for entry in fs::read_dir(&section_dir).unwrap().flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.starts_with('g')) {
            fs::write(entry.path(), b"not a group file").unwrap();
        }
    }

    let db = open_account(base);
    // The file still reads, and the index is rebuilt rather than trusted.
    assert_eq!(
        query(&db, "PEOPLE", &["WITH", "CITY", "=", "CITY3"]),
        ["K0003", "K0013"]
    );
    db.save().unwrap();
    let stats = index_of(&db, "PEOPLE", "CITY");
    assert!(!stats.stale);
    assert_eq!(stats.values, 10);
}

#[test]
fn a_bulk_change_that_names_no_keys_stops_the_index_being_trusted() {
    let guard = TempDir::new("index_bulk");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 20);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    {
        let handle = db.get_table_mut("PEOPLE").unwrap();
        let mut table = handle.write();
        table.records.clear();
        table
            .records
            .insert("ONLY".to_string(), Record::from_display_string("N^CITY3^R"));
        table.touch_all();
        assert!(table.indexes["CITY"].needs_rebuild);
    }
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", "CITY3"]), ["ONLY"]);
    db.save().unwrap();
    assert_eq!(index_of(&db, "PEOPLE", "CITY").values, 1);
}

#[test]
fn rebuilding_repairs_an_index_edited_out_from_under_the_server() {
    let guard = TempDir::new("index_rebuild");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 20);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    // Reach past the write path and drop a posting, which is the one thing that
    // can leave an index wrong without anything noticing.
    {
        let handle = db.get_table_mut("PEOPLE").unwrap();
        let mut table = handle.write();
        table.records.remove("K0003");
    }
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", "CITY3"]), ["K0013"]);
    let before = index_of(&db, "PEOPLE", "CITY");
    assert_eq!(before.postings, 20, "the withdrawn record is still indexed");

    let after = db.rebuild_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    assert_eq!(after.postings, 19);
    assert!(!after.stale);
}

/// The refusals are asserted on the variant rather than on the wording, so the
/// message can be reworded without a test to update - and so the reason a
/// caller branches on is the one being checked.
#[test]
fn what_cannot_be_indexed_is_refused_with_the_reason() {
    let guard = TempDir::new("index_refusals");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 5);

    let refusal = |field: &str| db.create_index_for_account(ACCOUNT, "PEOPLE", field).unwrap_err();
    for field in ["ID", "NOSUCH", "../escape"] {
        let error = refusal(field);
        assert!(
            matches!(&error, DbError::InvalidField { field: named, .. } if named == field),
            "{field} was refused as {error:?}"
        );
        // The reason is still said out loud, since somebody has to read it.
        assert!(!error.to_string().is_empty());
    }

    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    assert!(matches!(
        refusal("CITY"),
        DbError::IndexExists { ref file, ref field } if file == "PEOPLE" && field == "CITY"
    ));
    assert!(matches!(
        db.drop_index_for_account(ACCOUNT, "PEOPLE", "NAME").unwrap_err(),
        DbError::IndexNotFound { ref file, ref field } if file == "PEOPLE" && field == "NAME"
    ));
}

#[test]
fn a_value_too_long_to_be_a_key_is_still_found() {
    let guard = TempDir::new("index_long_value");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 5);

    // Two values that share the first MAX_INDEX_KEY bytes: they collapse into
    // one index entry, and the evaluation behind the lookup separates them.
    let long = "L".repeat(index::MAX_INDEX_KEY + 32);
    let other = format!("{}DIFFERENT", "L".repeat(index::MAX_INDEX_KEY + 32));
    {
        let handle = db.get_table_mut("PEOPLE").unwrap();
        let mut table = handle.write();
        table.insert_record("LONG1", Record::from_display_string(&format!("N^{}^R", long)));
        table.insert_record("LONG2", Record::from_display_string(&format!("N^{}^R", other)));
    }
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", &long]), ["LONG1"]);

    // And it survives the round trip through the group format, which is the
    // reason the key is truncated in the first place.
    db.save().unwrap();
    db.clear_loaded_tables();
    assert_eq!(query(&db, "PEOPLE", &["WITH", "CITY", "=", &long]), ["LONG1"]);
}

#[test]
fn file_statistics_report_the_indexes_a_file_carries() {
    let guard = TempDir::new("index_file_stats");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 30);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    db.create_index_for_account(ACCOUNT, "PEOPLE", "NAME").unwrap();
    db.save().unwrap();

    let stats = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    let fields: Vec<&str> = stats.indexes.iter().map(|index| index.field.as_str()).collect();
    assert_eq!(fields, ["CITY", "NAME"]);
    assert!(stats.indexes.iter().all(|index| index.loaded));
    assert!(stats.indexes.iter().all(|index| index.disk_bytes > 0));

    // The same answer for a file that is not in memory, read from its sections.
    db.clear_loaded_tables();
    let cold = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    for (hot, cold) in stats.indexes.iter().zip(&cold.indexes) {
        assert_eq!(
            (hot.field.as_str(), hot.values, hot.postings),
            (cold.field.as_str(), cold.values, cold.postings)
        );
        assert!(!cold.loaded);
        assert!(!cold.stale);
    }
}

#[test]
fn index_keys_cover_exactly_what_a_comparison_looks_at() {
    // The two sides of the correctness argument: a lookup only ever finds a
    // record whose value a comparison would also accept, and the empty string
    // stands for a field that is not there at all.
    assert_eq!(index::keys_of(None, 1), [""]);
    let record = Record::from_display_string("A^ B ]C\\D^");
    assert_eq!(index::keys_of(Some(&record), 1), ["B", "C", "D"]);
    assert_eq!(index::keys_of(Some(&record), 2), [""]);
    assert_eq!(index::keys_of(Some(&record), 9), [""]);
    // Trimming matches what `compare_values` does to a stored value.
    assert_eq!(index::index_key("  spaced  "), "spaced");
}

#[test]
fn a_field_name_that_cannot_be_a_directory_is_not_one() {
    assert!(index::is_valid_field_name("CITY"));
    assert!(index::is_valid_field_name("ID.CODE"));
    assert!(index::is_valid_field_name("A_B-C$D#E%F"));
    assert!(!index::is_valid_field_name(""));
    assert!(!index::is_valid_field_name(".."));
    assert!(!index::is_valid_field_name("a/b"));
    assert!(!index::is_valid_field_name("a b"));
    assert!(!index::is_valid_field_name(&"A".repeat(129)));
}

#[test]
fn an_index_writes_only_the_groups_its_changed_values_touch() {
    let guard = TempDir::new("index_incremental");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 400);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "NAME").unwrap();
    db.save().unwrap();

    let section_dir =
        crate::db::hashfile::section_dir(&index::section_path(&format!("{}/PEOPLE", guard.path()), "NAME"));
    let stamps = || -> HashMap<String, (u64, std::time::SystemTime)> {
        fs::read_dir(&section_dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_str().is_some_and(|name| name.starts_with('g')))
            .map(|entry| {
                let meta = entry.metadata().unwrap();
                (
                    entry.file_name().to_str().unwrap().to_string(),
                    (meta.len(), meta.modified().unwrap()),
                )
            })
            .collect()
    };
    let before = stamps();
    assert!(before.len() > 4, "the index should be spread over several groups");

    db.get_table_mut("PEOPLE")
        .unwrap()
        .write()
        .insert_record("K0000", Record::from_display_string("RENAMED^CITY0^ROLE0"));
    db.save().unwrap();

    let after = stamps();
    let rewritten = before
        .iter()
        .filter(|(name, stamp)| after.get(*name) != Some(stamp))
        .count();
    // One value withdrawn and one added, so at most two groups - and never the
    // whole section, which is the property the format exists for.
    assert!(
        rewritten <= 2 && after.len() >= before.len() - 1,
        "a single write rewrote {} of {} index groups",
        rewritten,
        before.len()
    );
}

// --- Excluded values --------------------------------------------------------
//
// The whole risk of the feature is the planner: an index that does not hold a
// value must answer "I cannot help, scan for it" and never "no records". These
// tests are built the same way as the rest of the file - the same query with
// and without, asserting the two are identical - because that is the only
// property that matters.

/// A file where one value dominates: `STATUS` is `ACTIVE` on nine records in
/// ten, `PENDING` on the rest, and absent from one in twenty.
fn build_skewed_file(db: &Database, file: &str, count: usize) {
    db.create_table(file).unwrap();
    let handle = db.get_table_mut(file).unwrap();
    {
        let mut table = handle.write();
        for (name, position) in [("NAME", 1), ("STATUS", 2)] {
            table.dictionary.insert(
                name.to_string(),
                Record::from_display_string(&format!("{}^{}^L^20", position, name)),
            );
        }
        table.mark_dict_dirty();
        for i in 0..count {
            let status = match i % 20 {
                0 => "",
                n if n % 10 == 1 => "PENDING",
                _ => "ACTIVE",
            };
            table.insert_record(
                &format!("K{:04}", i),
                Record::from_display_string(&format!("NAME{}^{}", i, status)),
            );
        }
    }
    db.save().unwrap();
}

#[test]
fn an_excluded_value_answers_exactly_what_no_index_answers() {
    let guard = TempDir::new("index_exclude_answers");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 200);

    // The answers with no index at all: the baseline every other run has to
    // reproduce, byte for byte.
    let scanned: Vec<Vec<String>> = [
        vec!["WITH", "STATUS", "=", "ACTIVE"],
        vec!["WITH", "STATUS", "=", "PENDING"],
        vec!["WITH", "STATUS", "=", ""],
        vec!["WITH", "STATUS", "=", "NOSUCHSTATUS"],
        vec!["WITH", "STATUS", "=", "ACTIVE", "AND", "WITH", "NAME", "=", "NAME3"],
        vec!["WITH", "STATUS", "=", "ACTIVE", "OR", "WITH", "STATUS", "=", "PENDING"],
    ]
    .iter()
    .map(|clause| query(&db, "ORDERS", clause))
    .collect();
    assert!(!scanned[0].is_empty() && !scanned[1].is_empty() && !scanned[2].is_empty());

    // The same queries through an index that excludes the dominant value and
    // the empty one. The excluded values are the interesting cases: an empty
    // posting list read as "no records" would return nothing at all here.
    db.create_index_excluding(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string(), String::new()])
        .unwrap();
    let indexed: Vec<Vec<String>> = [
        vec!["WITH", "STATUS", "=", "ACTIVE"],
        vec!["WITH", "STATUS", "=", "PENDING"],
        vec!["WITH", "STATUS", "=", ""],
        vec!["WITH", "STATUS", "=", "NOSUCHSTATUS"],
        vec!["WITH", "STATUS", "=", "ACTIVE", "AND", "WITH", "NAME", "=", "NAME3"],
        vec!["WITH", "STATUS", "=", "ACTIVE", "OR", "WITH", "STATUS", "=", "PENDING"],
    ]
    .iter()
    .map(|clause| query(&db, "ORDERS", clause))
    .collect();

    assert_eq!(
        indexed, scanned,
        "excluding a value changed an answer; the planner is trusting an empty posting list"
    );
}

#[test]
fn an_excluded_value_is_not_indexed_and_the_rest_still_is() {
    let guard = TempDir::new("index_exclude_shape");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 200);

    let whole = db.create_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();
    assert_eq!(whole.values, 3, "ACTIVE, PENDING and the empty value");
    assert_eq!(whole.postings, 200);
    // The dominant value is most of the file, which is the shape the exclusion
    // exists for - and the health verdict says so rather than leaving it to be
    // worked out from the counts.
    assert!(whole.dominant_share(200) > 0.25);
    assert_eq!(whole.health.verdict, crate::db::Verdict::Act);

    let trimmed = db
        .set_index_exclusions(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
        .unwrap();
    assert_eq!(trimmed.excluded, vec!["ACTIVE".to_string()]);
    assert_eq!(trimmed.values, 2, "PENDING and the empty value are still held");
    assert_eq!(
        trimmed.postings,
        whole.postings - 170,
        "ACTIVE is 170 of the 200 records"
    );
    assert!(!trimmed.stale, "the rebuild happened before the reply was built");
    // What the exclusion bought: the longest posting list is gone, so the entry
    // rewritten most expensively on every write is no longer there.
    assert!(trimmed.largest_postings < whole.largest_postings);

    // Clearing it puts everything back.
    let restored = db.set_index_exclusions(ACCOUNT, "ORDERS", "STATUS", &[]).unwrap();
    assert!(restored.excluded.is_empty());
    assert_eq!(restored.values, whole.values);
    assert_eq!(restored.postings, whole.postings);
}

#[test]
fn exclusions_survive_a_restart() {
    let guard = TempDir::new("index_exclude_restart");
    {
        let db = open_account(guard.path());
        build_skewed_file(&db, "ORDERS", 120);
        db.create_index_excluding(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
            .unwrap();
        db.save().unwrap();
    }

    let db = open_account(guard.path());
    // Touching the file loads it, which is what brings the index back into
    // memory with whatever its `state` said.
    let stats = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(stats.excluded, vec!["ACTIVE".to_string()]);
    assert!(!stats.stale, "the exclusions are part of the state, not a change to it");

    // And the answers are still the unindexed ones.
    let indexed = query(&db, "ORDERS", &["WITH", "STATUS", "=", "ACTIVE"]);
    db.drop_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();
    let scanned = query(&db, "ORDERS", &["WITH", "STATUS", "=", "ACTIVE"]);
    assert_eq!(indexed, scanned);
}

#[test]
fn changing_the_exclusions_rebuilds_rather_than_leaving_a_half_right_index() {
    let guard = TempDir::new("index_exclude_rebuild");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 60);
    db.create_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();

    // Adding an exclusion has to drop a posting list; removing one has to
    // derive a list that was never kept. Both are a rebuild, and the index says
    // so the moment the set changes.
    let handle = db.get_table_mut("ORDERS").unwrap();
    {
        let mut table = handle.write();
        let index = table.indexes.get_mut("STATUS").unwrap();
        assert!(index.set_excluded(["ACTIVE".to_string()].into_iter().collect()));
        assert!(index.needs_rebuild, "a changed exclusion set did not mark a rebuild");
        // A lookup on an index in that state cannot answer at all.
        assert!(index.candidates("PENDING").is_none());
        // Setting the same values again is not a change and does not re-mark.
        index.needs_rebuild = false;
        assert!(!index.set_excluded(["ACTIVE".to_string()].into_iter().collect()));
        assert!(!index.needs_rebuild);
    }
}

#[test]
fn an_excluded_value_is_never_written_into_the_postings() {
    let guard = TempDir::new("index_exclude_writes");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 40);
    db.create_index_excluding(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
        .unwrap();

    let before = index_of(&db, "ORDERS", "STATUS").postings;
    let handle = db.get_table_mut("ORDERS").unwrap();
    {
        let mut table = handle.write();
        // A new record carrying the excluded value adds nothing...
        table.insert_record("NEW1", Record::from_display_string("NAMENEW^ACTIVE"));
        // ...and moving an indexed value *to* the excluded one withdraws it,
        // which is why `apply` filters both sides rather than only the new one.
        table.insert_record("K0001", Record::from_display_string("NAME1^ACTIVE"));
    }
    db.save().unwrap();

    let after = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(
        after.postings,
        before - 1,
        "the excluded value was written into the postings, or the old value was left behind"
    );
    assert!(!after.excluded.is_empty());

    // And the record carrying the excluded value is still findable.
    assert!(query(&db, "ORDERS", &["WITH", "STATUS", "=", "ACTIVE"]).contains(&"NEW1".to_string()));
}

#[test]
fn the_value_histogram_names_the_value_that_dominates() {
    let guard = TempDir::new("index_histogram");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 200);
    db.create_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();

    let report = db.index_report(ACCOUNT, "ORDERS", "STATUS", 10).unwrap();
    assert!(report.values_available);
    assert_eq!(report.record_count, 200);
    assert_eq!(report.index.file, "ORDERS");
    // Largest first: the answer to "which value is the problem" is the first row.
    assert_eq!(report.top_values[0].value, "ACTIVE");
    assert_eq!(report.top_values[0].keys, 170);
    assert!(report.top_values.windows(2).all(|pair| pair[0].keys >= pair[1].keys));
    // The empty value is a value like any other, and is named rather than lost.
    assert!(report.top_values.iter().any(|value| value.value.is_empty()));

    // The limit is honoured and clamped rather than trusted.
    assert_eq!(
        db.index_report(ACCOUNT, "ORDERS", "STATUS", 1)
            .unwrap()
            .top_values
            .len(),
        1
    );
    assert!(
        db.index_report(ACCOUNT, "ORDERS", "STATUS", 0)
            .unwrap()
            .top_values
            .len()
            <= 3
    );

    // An excluded value holds no posting list, so it is not in the histogram -
    // it is in `excluded`, which is where a reader looks for it.
    db.set_index_exclusions(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
        .unwrap();
    let report = db.index_report(ACCOUNT, "ORDERS", "STATUS", 10).unwrap();
    assert!(report.top_values.iter().all(|value| value.value != "ACTIVE"));
    assert_eq!(report.index.excluded, vec!["ACTIVE".to_string()]);
}

#[test]
fn usage_counts_what_the_read_path_actually_asked() {
    let guard = TempDir::new("index_usage");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 100);
    db.create_index_excluding(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
        .unwrap();

    // A fresh index has served nothing, which is the signal the counter exists
    // to give: it is maintained on every write and is saving nobody anything.
    let fresh = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(fresh.usage.lookups, 0);
    assert_eq!(fresh.health.verdict, crate::db::Verdict::Watch);
    assert!(
        fresh
            .health
            .measures
            .iter()
            .any(|m| m.id == "usage" && m.verdict == crate::db::Verdict::Watch),
        "an unused index is not reported as unused"
    );

    let pending = query(&db, "ORDERS", &["WITH", "STATUS", "=", "PENDING"]);
    let after = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(after.usage.lookups, 1);
    assert_eq!(after.usage.candidates, pending.len() as u64);
    // One index resolved the whole query, so its survivors are attributable.
    assert_eq!(after.usage.measured_lookups, 1);
    assert_eq!(after.usage.matched, pending.len() as u64);

    // A lookup on the excluded value is a fall-back to a scan, counted apart
    // from the lookups the index actually answered.
    query(&db, "ORDERS", &["WITH", "STATUS", "=", "ACTIVE"]);
    let after = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(after.usage.excluded_lookups, 1);
    assert_eq!(after.usage.lookups, 1, "an excluded lookup is not a lookup served");

    // A composed query counts its lookup but leaves the precision alone: there
    // is no honest way to attribute a survivor to one of two conditions.
    let measured = after.usage.measured_lookups;
    query(
        &db,
        "ORDERS",
        &["WITH", "STATUS", "=", "PENDING", "AND", "WITH", "NAME", "=", "NAME11"],
    );
    let composed = index_of(&db, "ORDERS", "STATUS");
    assert_eq!(composed.usage.lookups, 2);
    assert_eq!(composed.usage.measured_lookups, measured);
}

#[test]
fn the_account_wide_listing_finds_an_index_without_opening_its_file() {
    let guard = TempDir::new("index_account_listing");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 40);
    build_skewed_file(&db, "ORDERS", 200);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    db.create_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();

    let all = db.index_statistics_for_account(ACCOUNT).unwrap();
    let named: Vec<(String, String)> = all
        .iter()
        .map(|(file, stats)| (file.clone(), stats.field.clone()))
        .collect();
    assert_eq!(
        named,
        vec![
            ("ORDERS".to_string(), "STATUS".to_string()),
            ("PEOPLE".to_string(), "CITY".to_string()),
        ],
        "sorted by file then field, so the same account always lists the same way"
    );
    // Each row names its own file, so one table renders both listings.
    assert!(all.iter().all(|(file, stats)| &stats.file == file));

    // The point of the view: the badly shaped index is the one that stands out,
    // without anyone having had to open the page for its file.
    let worrying: Vec<&str> = all
        .iter()
        .filter(|(_, stats)| stats.health.verdict == crate::db::Verdict::Act)
        .map(|(_, stats)| stats.field.as_str())
        .collect();
    assert_eq!(worrying, vec!["STATUS"]);

    assert!(db.index_statistics_for_account("NOSUCHACCOUNT").is_err());
}

#[test]
fn an_exclusion_round_trips_through_the_state_file_whatever_it_holds() {
    // The state file is line oriented and its reader trims, so a value carrying
    // a newline or edge whitespace would come back as a different value than it
    // went in as - and an exclusion that reads back wrong is an index silently
    // holding what it says it does not.
    let guard = TempDir::new("index_state_escaping");
    let path = format!("{}/section", guard.path());
    let awkward: std::collections::BTreeSet<String> = [
        String::new(),
        "ACTIVE".to_string(),
        "two words".to_string(),
        "a=b".to_string(),
        "back\\slash".to_string(),
        "tab\there".to_string(),
    ]
    .into_iter()
    .collect();
    let state = index::IndexState {
        field: "STATUS".to_string(),
        attribute: 2,
        data_version: 7,
        excluded: awkward.clone(),
    };
    index::write_state(&path, &state, crate::db::hashfile::FsyncPolicy::Never).unwrap();
    assert_eq!(index::read_state(&path), Some(state));

    // A value with a newline in it cannot be spelled by the CLI, but the format
    // has to survive one rather than truncate the file at it.
    let with_newline = index::IndexState {
        field: "STATUS".to_string(),
        attribute: 2,
        data_version: 7,
        excluded: ["line\nbreak".to_string()].into_iter().collect(),
    };
    index::write_state(&path, &with_newline, crate::db::hashfile::FsyncPolicy::Never).unwrap();
    assert_eq!(index::read_state(&path), Some(with_newline));
}

#[test]
fn excluding_the_dominant_value_is_what_makes_the_write_cheaper() {
    // The cost side of the feature, asserted rather than argued. An index entry
    // is one record holding every key that carries its value, so the dominant
    // value's entry is both the largest thing in the index and the thing
    // rewritten on every write that moves a record into or out of it.
    //
    // Bytes rather than time: what the exclusion changes is how much has to be
    // written, and that is measurable without depending on the disk underneath.
    let guard = TempDir::new("index_exclude_cost");
    let db = open_account(guard.path());
    build_skewed_file(&db, "ORDERS", 2_000);

    let section_bytes = || -> u64 {
        let dir = crate::db::hashfile::section_dir(&index::section_path(&format!("{}/ORDERS", guard.path()), "STATUS"));
        let Ok(entries) = fs::read_dir(dir) else { return 0 };
        entries
            .flatten()
            .filter_map(|entry| entry.metadata().ok())
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
            .sum()
    };

    // A write that moves one record's status, which is what a status field does
    // in life and the only kind of write an index pays for at all - re-storing
    // a record with the value it already had compares two short lists and stops.
    let flip = |db: &Database, to: &str| {
        let handle = db.get_table_mut("ORDERS").unwrap();
        handle
            .write()
            .insert_record("K0001", Record::from_display_string(&format!("NAME1^{}", to)));
        drop(handle);
        db.save().unwrap();
    };

    db.create_index_for_account(ACCOUNT, "ORDERS", "STATUS").unwrap();
    flip(&db, "ACTIVE");
    let whole = section_bytes();

    db.set_index_exclusions(ACCOUNT, "ORDERS", "STATUS", &["ACTIVE".to_string()])
        .unwrap();
    flip(&db, "ACTIVE");
    let trimmed = section_bytes();

    // The dominant value is 85% of this file, and its entry is 85% of the index
    // - so dropping it should be most of the section, not a rounding error.
    assert!(
        trimmed * 3 < whole,
        "excluding the dominant value left the index at {} bytes against {}; \
         it should be a fraction of it",
        trimmed,
        whole
    );

    // And the write is still correct: the record moved, and a query for the
    // excluded value finds it exactly as a scan would.
    assert!(query(&db, "ORDERS", &["WITH", "STATUS", "=", "ACTIVE"]).contains(&"K0001".to_string()));
}
