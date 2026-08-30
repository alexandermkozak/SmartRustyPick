use crate::db::engine::{Database, TableHandle};
use crate::db::models::*;
use crate::db::report;
use crate::test_support::{isolated_config, TempDir};

/// A file with a multivalued ROLES column, a sub-valued role, and a
/// right-justified numeric column carrying an MD2 conversion. The guard is
/// returned alongside the database so callers keep the directory alive for as
/// long as they use it.
fn report_db(label: &str) -> (TempDir, Database) {
    let dir = TempDir::new(label);
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC", None).unwrap();
    db.logto("ACC").unwrap();
    db.create_table("USERS").unwrap();
    let table_handle = db.get_table_mut("USERS").unwrap();
    let mut table = table_handle.write();
    table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^10"));
    table.dictionary.insert("ROLES".to_string(), Record::from_display_string("2^ROLES^L^12"));
    table.dictionary.insert("PRICE".to_string(), Record::from_display_string("3^PRICE^R^12^^^^MD2"));
    table.records.insert("1".to_string(), Record::from_display_string("John^ADMIN]DEV^120000]250"));
    table.records.insert("2".to_string(), Record::from_display_string("Jane^TEST\\LAB^500"));
    table.mark_dict_dirty();
    table.touch_all();
    drop(table);
    (dir, db)
}

/// The rows a render is given. The records are cloned out from under the
/// table's lock rather than borrowed through it, because a borrow cannot
/// outlive the guard it came from.
/// The file the renderer reads its columns from.
fn table(db: &Database) -> TableHandle {
    db.get_table_read_only_for_account("ACC", "USERS").unwrap()
}

fn rows_for(db: &Database, entries: Vec<SelectEntry>) -> Vec<(SelectEntry, Record)> {
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();
    entries.into_iter().map(|e| {
        let record = table.records.get(&e.key).unwrap().clone();
        (e, record)
    }).collect()
}

#[test]
fn test_render_list_headers_and_widths() {
    let (_dir, db) = report_db("render_headers");
    let rows = rows_for(&db, vec![SelectEntry::new("1".to_string())]);

    let lines = report::render_list(&table(&db).read(), &["NAME".to_string()], None, &rows);

    // ID is always the first column and always ten wide; NAME takes its width
    // and justification from the dictionary.
    assert_eq!(lines[0], "ID         NAME      ");
    assert_eq!(lines[1], "---------- ----------");
    assert_eq!(lines[2], "1          John      ");

}

#[test]
fn test_render_list_joins_a_whole_multivalued_field() {
    let (_dir, db) = report_db("render_joined");
    let rows = rows_for(&db, vec![SelectEntry::new("1".to_string()), SelectEntry::new("2".to_string())]);

    let lines = report::render_list(&table(&db).read(), &["ROLES".to_string()], None, &rows);

    // With no position, a multivalued field renders as it always has.
    assert_eq!(lines[2].trim_end(), "1          ADMIN]DEV");
    assert_eq!(lines[3].trim_end(), "2          TEST\\LAB");

}

#[test]
fn test_render_list_explodes_only_the_exploded_column() {
    let (_dir, db) = report_db("render_exploded");
    let rows = rows_for(&db, vec![
        SelectEntry::at("1".to_string(), ValuePosition::value(0)),
        SelectEntry::at("1".to_string(), ValuePosition::value(1)),
        SelectEntry::at("2".to_string(), ValuePosition::sub_value(0, 1)),
    ]);

    let lines = report::render_list(
        &table(&db).read(),
        &["NAME".to_string(), "ROLES".to_string()],
        Some("ROLES"),
        &rows,
    );

    // The ID repeats, ROLES narrows to the position, and NAME - which is not the
    // exploded field - keeps its whole value on every row rather than being
    // blanked out by a position it has no value at.
    assert_eq!(lines[2].trim_end(), "1          John       ADMIN");
    assert_eq!(lines[3].trim_end(), "1          John       DEV");
    assert_eq!(lines[4].trim_end(), "2          Jane       LAB");

}

#[test]
fn test_render_list_converts_each_value_of_a_multivalued_column() {
    let (_dir, db) = report_db("render_conv");
    let rows = rows_for(&db, vec![SelectEntry::new("1".to_string())]);

    let lines = report::render_list(&table(&db).read(), &["PRICE".to_string()], None, &rows);

    // MD2 applies to each value. Handing the joined "120000]250" to the
    // conversion parses as no number at all and comes back unconverted.
    assert_eq!(lines[2].trim_end(), "1          1200.00]2.50");

    // The column is right-justified per its dictionary entry, and truncated to
    // its width.
    let single = rows_for(&db, vec![SelectEntry::new("2".to_string())]);
    let lines = report::render_list(&table(&db).read(), &["PRICE".to_string()], None, &single);
    assert_eq!(lines[2], "2                  5.00");

}

#[test]
fn test_render_list_expands_the_wildcard_column() {
    let (_dir, db) = report_db("render_wildcard");
    let rows = rows_for(&db, vec![SelectEntry::new("1".to_string())]);

    let lines = report::render_list(&table(&db).read(), &["*".to_string()], None, &rows);

    // Every dictionary field, in attribute order, after the ID column.
    assert!(lines[0].starts_with("ID         NAME       ROLES        "));
    assert!(lines[0].contains("PRICE"));

}

#[test]
fn test_render_list_survives_a_stale_position() {
    let (_dir, db) = report_db("render_stale");
    // A list held over an edit that shortened the field: the position no longer
    // exists. It renders empty rather than panicking.
    let rows = rows_for(&db, vec![SelectEntry::at("2".to_string(), ValuePosition::value(9))]);

    let lines = report::render_list(&table(&db).read(), &["ROLES".to_string()], Some("ROLES"), &rows);
    assert_eq!(lines[2].trim_end(), "2");

}
