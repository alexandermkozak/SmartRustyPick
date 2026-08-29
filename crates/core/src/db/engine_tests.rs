use crate::db::engine::Database;
use crate::db::models::*;
use crate::test_support::{isolated_config, TempDir};
use std::path::Path;

#[test]
fn test_lru_eviction() {
    let dir = TempDir::new("lru");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.logto("SYSTEM").unwrap();
    db.loaded_tables.clear();
    db.lru_order.clear();

    // Set max loaded to 2 for testing
    db.max_loaded = 2;

    // Create 3 tables
    db.create_table("T1").unwrap();
    db.create_table("T2").unwrap();
    db.create_table("T3").unwrap();

    // Load T1 and T2
    db.get_table_mut("T1").unwrap().records.insert("K1".to_string(), Record::from_display_string("V1"));
    db.get_table_mut("T1").unwrap().touch_all();
    let _ = db.get_table_mut("T2");

    assert_eq!(db.loaded_tables.len(), 2);
    assert!(db.is_table_loaded("T1"));
    assert!(db.is_table_loaded("T2"));

    // Loading T3 should evict T1 (oldest in LRU)
    let _ = db.get_table_mut("T3");
    assert_eq!(db.loaded_tables.len(), 2);
    assert!(!db.is_table_loaded("T1"));
    assert!(db.is_table_loaded("T2"));
    assert!(db.is_table_loaded("T3"));

    // Accessing T2 should move it to end of LRU
    db.get_table("T2");

    // Loading T1 should evict T3
    db.get_table("T1");
    assert!(!db.is_table_loaded("T3"));
    assert!(db.is_table_loaded("T2"));
    assert!(db.is_table_loaded("T1"));
}

#[test]
fn test_delete_table_and_account() {
    let dir = TempDir::new("delete");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();

    db.create_account("DEL_ACC", None).unwrap();
    db.logto("DEL_ACC").unwrap();
    db.create_table("DEL_TABLE").unwrap();
    assert!(db.is_table_available("DEL_TABLE"));

    db.delete_table("DEL_TABLE").unwrap();
    assert!(!db.is_table_available("DEL_TABLE"));

    db.logto("SYSTEM").unwrap();
    db.delete_account("DEL_ACC").unwrap();
    assert!(db.get_account_dir("DEL_ACC").is_none());
}

#[test]
fn test_account_registry_reloads_when_another_process_writes() {
    let dir = TempDir::new("stale_registry");
    let base_dir = dir.path();

    let mut writer = Database::new(base_dir, Some(isolated_config())).unwrap();
    let mut reader = Database::new(base_dir, Some(isolated_config())).unwrap();

    // The other process registers a brand new account.
    writer.create_account("NEWACC", None).unwrap();

    // Without reconnecting, the reader must be able to log to it.
    reader.logto("NEWACC").expect("account created by another process is not visible");
    assert!(reader.get_account_dir("NEWACC").is_some());

    // The reader creating its own account must not erase the other one.
    reader.create_account("OWNACC", None).unwrap();
    let mut third = Database::new(base_dir, Some(isolated_config())).unwrap();
    assert!(third.get_account_dir("NEWACC").is_some(), "registry lost an account on write");
    assert!(third.get_account_dir("OWNACC").is_some());
    let _ = third.logto("SYSTEM");
}

#[test]
fn test_authorized_clients_refresh_across_processes() {
    let dir = TempDir::new("stale_clients");
    let base_dir = dir.path();

    let mut writer = Database::new(base_dir, Some(isolated_config())).unwrap();
    let mut reader = Database::new(base_dir, Some(isolated_config())).unwrap();

    let tp = "aabbccdd";
    writer.add_authorized_client("CLIENT1", tp, vec!["SYSTEM".to_string()], false).unwrap();

    reader.refresh_clients_if_stale().unwrap();
    assert!(reader.authorized_clients.contains_key(tp), "client authorized elsewhere is not visible");

    // Deauthorization must be honoured too.
    assert!(writer.remove_authorized_client("CLIENT1").unwrap());
    reader.refresh_clients_if_stale().unwrap();
    assert!(!reader.authorized_clients.contains_key(tp), "deauthorized client still authorized");
}

#[test]
fn test_apply_conversion() {
    // MD2: 12345 -> 123.45
    assert_eq!(Database::apply_conversion("12345", "MD2"), "123.45");
    // MD0: 12345 -> 12345
    assert_eq!(Database::apply_conversion("12345", "MD0"), "12345");
    // Invalid number
    assert_eq!(Database::apply_conversion("abc", "MD2"), "abc");
    // Non-MD code
    assert_eq!(Database::apply_conversion("12345", "G"), "12345");
}

#[test]
fn test_sync_dir_file() {
    let dir = TempDir::new("sync");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.create_account("SYNC_ACC", None).unwrap();
    db.logto("SYNC_ACC").unwrap();

    db.create_table("T1").unwrap();
    db.create_table("T2").unwrap();
    db.create_table("DIR").unwrap(); // Ensure DIR exists for this account

    // Manually remove DIR entry
    {
        let dir = db.get_table_mut("DIR").unwrap();
        dir.records.remove("T1");
        dir.touch_all();
    }
    db.save().unwrap();

    db.sync_dir_file().unwrap();
    {
        let dir = db.get_table("DIR").expect("DIR table should exist");
        assert!(dir.records.contains_key("T1"));
        assert!(dir.records.contains_key("T2"));
    }
}

#[test]
fn test_directory_traversal_vulnerability() {
    let dir = TempDir::new("traversal");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();

    // Create a "secret" account
    db.create_account("SECRET", None).unwrap();
    db.logto("SECRET").unwrap();
    db.create_table("PRIVATE").unwrap();

    // Switch to a normal account
    db.create_account("USER", None).unwrap();
    db.logto("USER").unwrap();

    // Attempt directory traversal to access the SECRET account's table
    let traversal_name = "../SECRET/PRIVATE";

    // This call should now return an error.
    let res = db.get_table_mut(traversal_name);
    assert!(res.is_err());

    let _secret_table_path = Path::new(base_dir).join("SECRET").join("PRIVATE");
    // It should NOT have been re-created or modified via the traversal path in USER's dir.
    // Wait, create_table("PRIVATE") already created it.
    // Let's use a name that DOESN'T exist.
    let traversal_name_new = "../SECRET/NEW_PRIVATE";
    let res2 = db.get_table_mut(traversal_name_new);
    assert!(res2.is_err());
    let new_secret_table_path = Path::new(base_dir).join("SECRET").join("NEW_PRIVATE");
    assert!(!new_secret_table_path.exists());

    // Verify that "INVALID_TABLE_NAME" is NOT created in loaded_tables
    assert!(!db.is_table_loaded("INVALID_TABLE_NAME"));
}

#[test]
fn test_cache_reloads_when_another_process_writes() {
    let dir = TempDir::new("stale_cache");
    let base_dir = dir.path();

    // "Server" instance creates the table and one record.
    let mut writer = Database::new(base_dir, Some(isolated_config())).unwrap();
    writer.create_account("SHARED", None).unwrap();
    writer.logto("SHARED").unwrap();
    writer.create_table("ENTITIES").unwrap();
    {
        let t = writer.get_table_mut("ENTITIES").unwrap();
        t.records.insert("E1".to_string(), Record::from_display_string("FIRST"));
        t.touch_all();
    }
    writer.save().unwrap();

    // "Local CLI" instance reads it, populating its own cache.
    let mut reader = Database::new(base_dir, Some(isolated_config())).unwrap();
    reader.logto("SHARED").unwrap();
    assert!(reader.get_table("ENTITIES").unwrap().records.contains_key("E1"));

    // The other process commits a new entity and a brand new table.
    {
        let t = writer.get_table_mut("ENTITIES").unwrap();
        t.records.insert("E2".to_string(), Record::from_display_string("SECOND"));
        t.touch_all();
    }
    writer.create_table("NEWFILE").unwrap();
    writer.save().unwrap();

    // Without disconnecting, the reader must see the committed changes.
    let table = reader.get_table("ENTITIES").expect("ENTITIES should be readable");
    assert!(table.records.contains_key("E2"), "cached table was not refreshed from disk");
    assert!(reader.get_table("NEWFILE").is_some(), "new table created by another process is not visible");
}

#[test]
fn test_listed_table_still_refreshes_after_save() {
    let dir = TempDir::new("stale_after_save");
    let base_dir = dir.path();

    let mut writer = Database::new(base_dir, Some(isolated_config())).unwrap();
    writer.create_account("SHARED3", None).unwrap();
    writer.logto("SHARED3").unwrap();
    writer.create_table("ENTITIES").unwrap();
    {
        let t = writer.get_table_mut("ENTITIES").unwrap();
        t.records.insert("E1".to_string(), Record::from_display_string("FIRST"));
        t.touch_all();
    }
    writer.save().unwrap();

    // Local CLI lists the file, caching a clean snapshot.
    let mut reader = Database::new(base_dir, Some(isolated_config())).unwrap();
    reader.logto("SHARED3").unwrap();
    assert!(reader.list_tables().contains(&"ENTITIES".to_string()));
    assert!(reader.get_table("ENTITIES").unwrap().records.contains_key("E1"));

    // Another process commits a new record.
    {
        let t = writer.get_table_mut("ENTITIES").unwrap();
        t.records.insert("E2".to_string(), Record::from_display_string("SECOND"));
        t.touch_all();
    }
    writer.save().unwrap();

    // A save on the reader side (e.g. triggered by LOGTO/admin commands) must not
    // mark the stale snapshot as up to date.
    reader.save().unwrap();

    let table = reader.get_table("ENTITIES").unwrap();
    assert!(table.records.contains_key("E2"), "stale snapshot was frozen by an unrelated save");
}

#[test]
fn test_local_changes_survive_staleness_check() {
    let dir = TempDir::new("stale_dirty");
    let base_dir = dir.path();

    let mut writer = Database::new(base_dir, Some(isolated_config())).unwrap();
    writer.create_account("SHARED2", None).unwrap();
    writer.logto("SHARED2").unwrap();
    writer.create_table("ENTITIES").unwrap();
    writer.save().unwrap();

    let mut reader = Database::new(base_dir, Some(isolated_config())).unwrap();
    reader.logto("SHARED2").unwrap();
    {
        let t = reader.get_table_mut("ENTITIES").unwrap();
        t.records.insert("LOCAL".to_string(), Record::from_display_string("PENDING"));
        t.touch_all();
    }

    // Another process writes to the same table while we hold unsaved changes.
    {
        let t = writer.get_table_mut("ENTITIES").unwrap();
        t.records.insert("REMOTE".to_string(), Record::from_display_string("COMMITTED"));
        t.touch_all();
    }
    writer.save().unwrap();

    // Pending local changes must not be silently discarded.
    let table = reader.get_table("ENTITIES").unwrap();
    assert!(table.records.contains_key("LOCAL"));
}

#[test]
fn test_all_dict_fields() {
    let dir = TempDir::new("dict_fields");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.logto("SYSTEM").unwrap();

    db.create_table("USERS").unwrap();
    {
        let table = db.get_table_mut("USERS").unwrap();
        // EMAIL -> field 1
        table.dictionary.insert("EMAIL".to_string(), Record::from_display_string("1^Email Address^L^15"));
        // NAME -> field 2
        table.dictionary.insert("NAME".to_string(), Record::from_display_string("2^User Name^L^15"));
        // ALT_NAME -> field 2
        table.dictionary.insert("ALT_NAME".to_string(), Record::from_display_string("2^Alternate Name^L^15"));
        // ZIP -> field 3
        table.dictionary.insert("ZIP".to_string(), Record::from_display_string("3^Zip Code^L^5"));
    }

    let fields = db.get_all_dict_fields_read_only_for_account("SYSTEM", "USERS");

    // Should contain EMAIL (1), then one of {ALT_NAME, NAME} (2), then ZIP (3).
    // Based on sorting keys: ALT_NAME comes before NAME.
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], "EMAIL");
    assert_eq!(fields[1], "ALT_NAME");
    assert_eq!(fields[2], "ZIP");
}

/// A file whose dictionary covers a plain field, a multivalued one and a
/// multivalued one carrying an MD2 conversion. The guard is returned alongside
/// the database so callers keep the directory alive for as long as they use it.
fn json_shape_db(label: &str) -> (TempDir, Database) {
    let dir = TempDir::new(label);
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC", None).unwrap();
    db.logto("ACC").unwrap();
    db.create_table("USERS").unwrap();
    let table = db.get_table_mut("USERS").unwrap();
    table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
    table.dictionary.insert("ROLES".to_string(), Record::from_display_string("2^ROLES^L^20"));
    table.dictionary.insert("PRICE".to_string(), Record::from_display_string("3^PRICE^R^10^^^^MD2"));
    table.mark_dict_dirty();
    (dir, db)
}

#[test]
fn test_serialize_record_shapes_multivalues_as_arrays() {
    let (_dir, db) = json_shape_db("serialize_shape");

    // A single value stays a string, so existing clients see no change.
    let plain = Record::from_display_string("John^ADMIN^500");
    let json = db.serialize_record("USERS", &plain);
    assert_eq!(json["name"], serde_json::json!("John"));
    assert_eq!(json["roles"], serde_json::json!("ADMIN"));
    assert_eq!(json["price"], serde_json::json!("5.00"));

    // Multiple values become an array; a value with sub-values nests.
    let multi = Record::from_display_string("Jane^DEV]TEST\\LAB^120000]250");
    let json = db.serialize_record("USERS", &multi);
    assert_eq!(json["name"], serde_json::json!("Jane"));
    assert_eq!(json["roles"], serde_json::json!(["DEV", ["TEST", "LAB"]]));
    // The conversion applies per value rather than to the joined string.
    assert_eq!(json["price"], serde_json::json!(["1200.00", "2.50"]));

    // An absent field is an empty string, as it always was.
    let short = Record::from_display_string("Zed");
    let json = db.serialize_record("USERS", &short);
    assert_eq!(json["roles"], serde_json::json!(""));

}

#[test]
fn test_multivalue_record_survives_a_json_round_trip() {
    let (_dir, db) = json_shape_db("json_roundtrip");

    // Reading a record, then writing it back unchanged, used to collapse the
    // multivalued field into one value holding a literal `]`.
    for display in ["Jane^DEV]TEST\\LAB^120000]250", "John^ADMIN^500", "Zed^^"] {
        let original = Record::from_display_string(display);
        let json = db.serialize_record("USERS", &original);
        let back = db.deserialize_record("USERS", &json).unwrap();
        assert_eq!(
            back.get_field_display_string(1),
            original.get_field_display_string(1),
            "ROLES did not survive the round trip of {display}"
        );
        assert_eq!(
            back.get_field_display_string(2),
            original.get_field_display_string(2),
            "PRICE did not survive the round trip of {display}"
        );
    }

}

#[test]
fn test_deserialize_does_not_resplit_a_plain_string() {
    let (_dir, db) = json_shape_db("json_nosplit");

    // A client that means to store a `]` still can: only an array makes values.
    let json = serde_json::json!({ "name": "Jane", "roles": "A]B" });
    let record = db.deserialize_record("USERS", &json).unwrap();
    assert_eq!(record.fields[1].values.len(), 1);
    assert_eq!(record.fields[1].values[0].sub_values[0], "A]B");

    // Numbers and booleans keep the scalar handling they always had.
    let json = serde_json::json!({ "roles": 7, "price": [true, 4.0] });
    let record = db.deserialize_record("USERS", &json).unwrap();
    assert_eq!(record.fields[1].values[0].sub_values[0], "7");
    assert_eq!(record.fields[2].values.len(), 2);
    assert_eq!(record.fields[2].values[0].sub_values[0], "100");
    assert_eq!(record.fields[2].values[1].sub_values[0], "400");

}

#[test]
fn test_format_record_field_at_narrows_to_a_position() {
    let (_dir, db) = json_shape_db("format_at");
    let record = Record::from_display_string("Jane^DEV]TEST\\LAB^120000]250");

    assert_eq!(db.format_record_field("USERS", &record, "ROLES"), "DEV]TEST\\LAB");
    assert_eq!(
        db.format_record_field_at("USERS", &record, "ROLES", Some(ValuePosition::value(1))),
        "TEST\\LAB"
    );
    assert_eq!(
        db.format_record_field_at("USERS", &record, "ROLES", Some(ValuePosition::sub_value(1, 1))),
        "LAB"
    );
    // The conversion still applies when a position narrows the field.
    assert_eq!(
        db.format_record_field_at("USERS", &record, "PRICE", Some(ValuePosition::value(1))),
        "2.50"
    );
    // An unknown field is empty, as it always was.
    assert_eq!(db.format_record_field_at("USERS", &record, "NOPE", None), "");

}
