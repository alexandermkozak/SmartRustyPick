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
