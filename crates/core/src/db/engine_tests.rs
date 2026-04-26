use crate::db::engine::Database;
use crate::db::models::*;
use std::fs;
use std::path::Path;

#[test]
fn test_lru_eviction() {
    let base_dir = "test_lru_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
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
    db.get_table_mut("T1").unwrap().dirty = true;
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

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_delete_table_and_account() {
    let base_dir = "test_delete_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();

    db.create_account("DEL_ACC", None).unwrap();
    db.logto("DEL_ACC").unwrap();
    db.create_table("DEL_TABLE").unwrap();
    assert!(db.is_table_available("DEL_TABLE"));

    db.delete_table("DEL_TABLE").unwrap();
    assert!(!db.is_table_available("DEL_TABLE"));

    db.logto("SYSTEM").unwrap();
    db.delete_account("DEL_ACC").unwrap();
    assert!(db.get_account_dir("DEL_ACC").is_none());

    fs::remove_dir_all(base_dir).unwrap();
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
    let base_dir = "test_sync_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
    db.create_account("SYNC_ACC", None).unwrap();
    db.logto("SYNC_ACC").unwrap();

    db.create_table("T1").unwrap();
    db.create_table("T2").unwrap();
    db.create_table("DIR").unwrap(); // Ensure DIR exists for this account

    // Manually remove DIR entry
    {
        let dir = db.get_table_mut("DIR").unwrap();
        dir.records.remove("T1");
        dir.dirty = true;
    }
    db.save().unwrap();

    db.sync_dir_file().unwrap();
    {
        let dir = db.get_table("DIR").expect("DIR table should exist");
        assert!(dir.records.contains_key("T1"));
        assert!(dir.records.contains_key("T2"));
    }

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_directory_traversal_vulnerability() {
    let base_dir = "test_traversal_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();

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

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_all_dict_fields() {
    let base_dir = "test_dict_fields_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
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

    fs::remove_dir_all(base_dir).unwrap();
}
