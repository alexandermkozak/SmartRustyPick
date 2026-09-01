use crate::db::*;
use crate::test_support::{TempDir, isolated_config};
use std::io;

#[test]
fn test_system_dictionary_auto_creation() -> io::Result<()> {
    let dir = TempDir::new("system_dict");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("SYSTEM")?;

        // Verify $LOGS dictionary
        let logs_handle = db.get_table("$LOGS").unwrap();
        let logs = logs_handle.read();
        assert!(logs.dictionary.contains_key("MESSAGE"));
        assert!(logs.dictionary.contains_key("DETAIL"));
        drop(logs);

        // Verify $ACCOUNTS dictionary
        let accounts_handle = db.get_table("$ACCOUNTS").unwrap();
        let accounts = accounts_handle.read();
        assert!(accounts.dictionary.contains_key("PATH"));
        drop(accounts);

        // Verify $CLIENTS dictionary
        let clients_handle = db.get_table("$CLIENTS").unwrap();
        let clients = clients_handle.read();
        assert!(clients.dictionary.contains_key("THUMBPRINT"));
        assert!(clients.dictionary.contains_key("ACCOUNTS"));
        assert!(clients.dictionary.contains_key("ADMIN"));
        drop(clients);

        // Verify $SAVEDLISTS dictionary
        let savedlists_handle = db.get_table("$SAVEDLISTS").unwrap();
        let savedlists = savedlists_handle.read();
        assert!(savedlists.dictionary.contains_key("TABLE"));
        assert!(savedlists.dictionary.contains_key("IS_DICT"));
        drop(savedlists);

        // Verify DIR dictionary
        let dir_table_handle = db.get_table("DIR").unwrap();
        let dir_table = dir_table_handle.read();
        assert!(dir_table.dictionary.contains_key("TYPE"));
        drop(dir_table);

        // Manually corrupt dictionary for $LOGS
        {
            let logs_mut_handle = db.get_table_mut("$LOGS").unwrap();
            let mut logs_mut = logs_mut_handle.write();
            logs_mut.dictionary.remove("MESSAGE");
            // Add an override
            logs_mut.dictionary.insert(
                "DETAIL".to_string(),
                Record::from_display_string("2^OVERRIDE_DETAIL^L^10"),
            );
            logs_mut.touch_all();
        }
        db.save()?;
    }

    // Restart and check for self-healing
    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("SYSTEM")?;

        let logs_handle = db.get_table("$LOGS").unwrap();
        let logs = logs_handle.read();
        // Should be restored
        assert!(
            logs.dictionary.contains_key("MESSAGE"),
            "MESSAGE dictionary should be restored"
        );
        // Should NOT be overwritten
        let detail_dict = logs.dictionary.get("DETAIL").unwrap();
        let detail_val = detail_dict.get_field_display_string(1);
        assert!(
            detail_val.contains("OVERRIDE_DETAIL"),
            "Existing dictionary entry should NOT be overwritten (got '{}')",
            detail_val
        );
    }

    Ok(())
}

#[test]
fn test_system_account_auto_creation() -> io::Result<()> {
    let dir = TempDir::new("system_account");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        // Check if SYSTEM account exists
        assert!(
            db.get_account_dir("SYSTEM").is_some(),
            "SYSTEM account should be automatically created"
        );
    }

    Ok(())
}

#[test]
fn test_system_logs_auto_creation() -> io::Result<()> {
    let dir = TempDir::new("system_logs");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        // Check if $LOGS file exists in SYSTEM account
        db.logto("SYSTEM")?;
        assert!(
            db.is_table_available("$LOGS"),
            "$LOGS table should be automatically created in SYSTEM account"
        );
    }

    Ok(())
}

#[test]
fn test_error_logging() -> io::Result<()> {
    let dir = TempDir::new("error_logging");
    let base_dir = dir.path();

    {
        let mut db = Database::new(base_dir, Some(isolated_config()))?;
        db.log_detail = "detailed".to_string();
        db.max_log_records = 2;

        db.log_error("TEST_ACC", "First error")?;
        db.log_error("TEST_ACC", "Second error")?;
        db.log_error("TEST_ACC", "Third error")?; // Should evict first

        db.logto("SYSTEM")?;
        let logs_handle = db.get_table("$LOGS").expect("$LOGS should exist");
        let logs = logs_handle.read();
        assert_eq!(logs.records.len(), 2, "Should respect max_log_records");

        let mut keys: Vec<_> = logs.records.keys().cloned().collect();
        keys.sort();

        // Check contents
        let rec2 = logs.records.get(&keys[0]).unwrap();
        assert_eq!(rec2.fields[0].values[0].sub_values[0], "Second error");
        assert!(rec2.fields.len() > 1, "Should have detailed field");

        let rec3 = logs.records.get(&keys[1]).unwrap();
        assert_eq!(rec3.fields[0].values[0].sub_values[0], "Third error");
    }

    Ok(())
}

#[test]
fn test_system_clients_file() -> io::Result<()> {
    let dir = TempDir::new("system_clients_file");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.add_authorized_client("CLIENT1", "aabbccdd", vec!["ACC1".to_string()], false)?;
        db.add_authorized_client("CLIENT2", "11223344", vec![], true)?; // ADMIN

        // Verify $CLIENTS exists and contains CLIENT1 and CLIENT2
        db.logto("SYSTEM")?;
        assert!(
            db.is_table_available("$CLIENTS"),
            "$CLIENTS table should exist in SYSTEM account"
        );

        {
            let clients_table_handle = db.get_table("$CLIENTS").expect("$CLIENTS should be loadable");
            let clients_table = clients_table_handle.read();
            assert!(
                clients_table.records.contains_key("CLIENT1"),
                "$CLIENTS should contain CLIENT1"
            );
            assert!(
                clients_table.records.contains_key("CLIENT2"),
                "$CLIENTS should contain CLIENT2"
            );

            let rec1 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1.fields[0].values[0].sub_values[0], "aabbccdd");
            assert_eq!(rec1.fields[1].values[0].sub_values[0], "ACC1");
            assert_eq!(rec1.fields[2].values[0].sub_values[0], "");

            let rec2 = clients_table.records.get("CLIENT2").unwrap();
            assert_eq!(rec2.fields[0].values[0].sub_values[0], "11223344");
            assert_eq!(rec2.fields[2].values[0].sub_values[0], "Y");
        }

        // Verify in-memory map
        assert!(db.client_for_thumbprint("aabbccdd").is_some());
        assert_eq!(
            db.client_for_thumbprint("aabbccdd").unwrap().allowed_accounts,
            vec!["ACC1"]
        );
        assert!(!db.client_for_thumbprint("aabbccdd").unwrap().is_admin);

        assert!(db.client_for_thumbprint("11223344").is_some());
        assert!(db.client_for_thumbprint("11223344").unwrap().is_admin);

        // Test add_client_account
        db.add_client_account("CLIENT1", "ACC2")?;
        db.logto("SYSTEM")?;
        {
            let clients_table_handle = db.get_table("$CLIENTS").unwrap();
            let clients_table = clients_table_handle.read();
            let rec1_v2 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1_v2.fields[1].values.len(), 2);
            assert_eq!(rec1_v2.fields[1].values[1].sub_values[0], "ACC2");
        }
        assert!(
            db.client_for_thumbprint("aabbccdd")
                .unwrap()
                .allowed_accounts
                .contains(&"ACC2".to_string())
        );

        // Test remove_client_account
        db.remove_client_account("CLIENT1", "ACC1")?;
        db.logto("SYSTEM")?;
        {
            let clients_table_handle = db.get_table("$CLIENTS").unwrap();
            let clients_table = clients_table_handle.read();
            let rec1_v3 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1_v3.fields[1].values.len(), 1);
            assert_eq!(rec1_v3.fields[1].values[0].sub_values[0], "ACC2");
        }
        assert!(
            !db.client_for_thumbprint("aabbccdd")
                .unwrap()
                .allowed_accounts
                .contains(&"ACC1".to_string())
        );

        // Test removal of client
        db.remove_authorized_client("CLIENT1")?;
        db.logto("SYSTEM")?;
        {
            let clients_table_handle = db.get_table("$CLIENTS").unwrap();
            let clients_table = clients_table_handle.read();
            assert!(
                !clients_table.records.contains_key("CLIENT1"),
                "$CLIENTS should not contain CLIENT1 after removal"
            );
        }
        assert!(
            db.client_for_thumbprint("aabbccdd").is_none(),
            "In-memory map should be updated"
        );
    }

    // Test auto-population on restart
    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        assert!(
            db.client_for_thumbprint("11223344").is_some(),
            "Should load CLIENT2 from $CLIENTS on restart"
        );
        assert!(db.client_for_thumbprint("11223344").unwrap().is_admin);
    }

    Ok(())
}

#[test]
fn test_system_accounts_file() -> io::Result<()> {
    let dir = TempDir::new("system_accounts_file");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("SYSTEM")?;
        db.create_account("USER1", None)?;
        db.create_account("USER2", Some(&format!("{}/custom_path/user2", base_dir)))?;

        // Verify $ACCOUNTS exists and contains USER1 and USER2
        db.logto("SYSTEM")?;
        assert!(
            db.is_table_available("$ACCOUNTS"),
            "$ACCOUNTS table should exist in SYSTEM account"
        );

        {
            let accounts_table_handle = db.get_table("$ACCOUNTS").expect("$ACCOUNTS should be loadable");
            let accounts_table = accounts_table_handle.read();
            assert!(
                accounts_table.records.contains_key("USER1"),
                "$ACCOUNTS should contain USER1"
            );
            assert!(
                accounts_table.records.contains_key("USER2"),
                "$ACCOUNTS should contain USER2"
            );
            assert!(
                !accounts_table.records.contains_key("SYSTEM"),
                "$ACCOUNTS should NOT contain SYSTEM"
            );

            let rec1 = accounts_table.records.get("USER1").unwrap();
            assert!(rec1.fields[0].values[0].sub_values[0].contains("USER1"));

            let rec2 = accounts_table.records.get("USER2").unwrap();
            assert_eq!(
                rec2.fields[0].values[0].sub_values[0],
                format!("{}/custom_path/user2", base_dir)
            );
        }

        // Test deletion
        db.delete_account("USER1")?;
        db.logto("SYSTEM")?;
        let accounts_table_handle = db.get_table("$ACCOUNTS").unwrap();
        let accounts_table = accounts_table_handle.read();
        assert!(
            !accounts_table.records.contains_key("USER1"),
            "$ACCOUNTS should not contain USER1 after deletion"
        );
    }

    // Test auto-population on restart
    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("SYSTEM")?;
        let accounts_table_handle = db.get_table("$ACCOUNTS").unwrap();
        let accounts_table = accounts_table_handle.read();
        assert!(
            accounts_table.records.contains_key("USER2"),
            "$ACCOUNTS should contain USER2 after restart"
        );
    }

    Ok(())
}

#[test]
fn test_accounts() -> io::Result<()> {
    let dir = TempDir::new("accounts");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.create_account("ACC1", None)?;
        db.create_account("ACC2", None)?;

        // Log to ACC1 and create a table
        db.logto("ACC1")?;
        db.create_table("T1")?;
        let t1_handle = db.get_table_mut("T1").unwrap();
        let mut t1 = t1_handle.write();
        t1.records.insert("K1".to_string(), Record::from_bytes(b"VAL1"));
        t1.touch_all();
        drop(t1);
        db.save()?;

        // Log to ACC2 and create a table with same name but different content
        db.logto("ACC2")?;
        db.create_table("T1")?;
        let t1_acc2_handle = db.get_table_mut("T1").unwrap();
        let mut t1_acc2 = t1_acc2_handle.write();
        t1_acc2.records.insert("K1".to_string(), Record::from_bytes(b"VAL2"));
        t1_acc2.touch_all();
        drop(t1_acc2);
        db.save()?;
    }

    // Re-open and verify isolation
    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("ACC1")?;
        let t1_handle = db.get_table("T1").unwrap();
        let t1 = t1_handle.read();
        assert_eq!(
            String::from_utf8_lossy(&t1.records.get("K1").unwrap().to_bytes()),
            "VAL1"
        );
        drop(t1);

        db.logto("ACC2")?;
        let t1_handle = db.get_table("T1").unwrap();
        let t1 = t1_handle.read();
        assert_eq!(
            String::from_utf8_lossy(&t1.records.get("K1").unwrap().to_bytes()),
            "VAL2"
        );
    }

    Ok(())
}

#[test]
fn test_dir_file_auto_creation() -> io::Result<()> {
    let dir = TempDir::new("dir_auto_creation");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.logto("SYSTEM")?;
        assert!(
            db.is_table_available("DIR"),
            "DIR table should be automatically created in SYSTEM account"
        );

        {
            let dir_table_handle = db.get_table("DIR").unwrap();
            let dir_table = dir_table_handle.read();
            assert!(dir_table.records.contains_key("$LOGS"));
            assert!(dir_table.records.contains_key("$ACCOUNTS"));
            assert!(dir_table.records.contains_key("$CLIENTS"));
            assert!(dir_table.records.contains_key("$SAVEDLISTS"));

            // Check record content
            let logs_dir_rec = dir_table.records.get("$LOGS").unwrap();
            assert_eq!(logs_dir_rec.fields[0].values[0].sub_values[0], "F");
        }

        // Test create_test_account
        db.create_test_account("TEST_DIR")?;
        db.logto("TEST_DIR")?;
        assert!(
            db.is_table_available("DIR"),
            "DIR table should be created in test account"
        );
        let dir_table_test_handle = db.get_table("DIR").unwrap();
        let dir_table_test = dir_table_test_handle.read();
        assert!(dir_table_test.records.contains_key("USERS"));
        assert!(dir_table_test.records.contains_key("PRODUCTS"));
        assert!(!dir_table_test.records.contains_key("DIR"));
    }

    Ok(())
}

#[test]
fn test_dictionary_field_index() -> io::Result<()> {
    let dir = TempDir::new("dict_field_index");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.create_test_account("DICT_TEST")?;
        db.logto("DICT_TEST")?;

        // USERS table has dictionary:
        // NAME -> 1^NAME^L^15
        // EMAIL -> 2^EMAIL^L^20
        // Records:
        // 1 -> John Doe^john@example.com
        // 2 -> Jane Smith^jane@example.com

        // Verify field indices
        // Ensure tables are loaded for read-only index lookups
        db.get_table("USERS").unwrap();
        db.get_table("PRODUCTS").unwrap();

        // "ID" should always be 0
        assert_eq!(db.get_field_index("USERS", "ID"), Some(0));
        // "NAME" should be 0 (Attribute 1 - 1 = 0)
        assert_eq!(db.get_field_index("USERS", "NAME"), Some(0));
        // "EMAIL" should be 1 (Attribute 2 - 1 = 1)
        assert_eq!(db.get_field_index("USERS", "EMAIL"), Some(1));

        // Verify data retrieval via indices
        let name_idx = db.get_field_index("USERS", "NAME").unwrap();
        let email_idx = db.get_field_index("USERS", "EMAIL").unwrap();

        let users_handle = db.get_table("USERS").unwrap();
        let users = users_handle.read();
        let rec1 = users.records.get("1").unwrap();

        assert_eq!(rec1.get_field_display_string(name_idx), "John Doe");
        assert_eq!(rec1.get_field_display_string(email_idx), "john@example.com");

        // PRODUCTS table has dictionary:
        // DESC -> 1^DESCRIPTION^L^20
        // PRICE -> 2^PRICE^R^10^MD2
        // Records:
        // P1 -> Laptop^120000

        let desc_idx = db.get_field_index("PRODUCTS", "DESC").unwrap();
        let price_idx = db.get_field_index("PRODUCTS", "PRICE").unwrap();

        let p1 = {
            let products_handle = db.get_table("PRODUCTS").unwrap();
            let products = products_handle.read();
            products.records.get("P1").unwrap().clone()
        };

        assert_eq!(p1.get_field_display_string(desc_idx), "Laptop");
        assert_eq!(p1.get_field_display_string(price_idx), "120000");

        // Verify conversion
        let price_conv = db.get_conversion_code("PRODUCTS", "PRICE");
        assert_eq!(price_conv, Some("MD2".to_string()));
        let raw_price = p1.get_field_display_string(price_idx);
        let formatted_price = Database::apply_conversion(&raw_price, &price_conv.unwrap());
        assert_eq!(formatted_price, "1200.00");

        // Test unified formatting method
        assert_eq!(db.format_record_field("PRODUCTS", &p1, "DESC"), "Laptop");
        assert_eq!(db.format_record_field("PRODUCTS", &p1, "PRICE"), "1200.00");
    }

    Ok(())
}

#[test]
fn test_record_serialization() -> io::Result<()> {
    let dir = TempDir::new("serialization");
    let base_dir = dir.path();

    {
        let db = Database::new(base_dir, Some(isolated_config()))?;
        db.create_test_account("SERIAL_TEST")?;
        db.logto("SERIAL_TEST")?;

        // Setup a table with complex dictionary names
        db.create_table("CUSTOM")?;
        {
            let table_handle = db.get_table_mut("CUSTOM").unwrap();
            let mut table = table_handle.write();
            table.dictionary.insert(
                "FIRST.NAME".to_string(),
                Record::from_display_string("1^First Name^L^15"),
            );
            table
                .dictionary
                .insert("LAST.NAME".to_string(), Record::from_display_string("2^Last Name^L^15"));
            table
                .dictionary
                .insert("AGE".to_string(), Record::from_display_string("3^Age^R^3"));
            table
                .records
                .insert("K1".to_string(), Record::from_display_string("John^Doe^30"));
            table.touch_all();
        }
        db.save()?;

        // Load to ensure available_tables is populated
        db.get_table("CUSTOM").unwrap();

        let record = Record::from_display_string("John^Doe^30");
        let serialized = db.serialize_record("CUSTOM", &record);

        assert!(serialized.is_object());
        let obj = serialized.as_object().unwrap();

        // Check camelCase conversion
        assert_eq!(obj.get("firstName").unwrap().as_str().unwrap(), "John");
        assert_eq!(obj.get("lastName").unwrap().as_str().unwrap(), "Doe");
        assert_eq!(obj.get("age").unwrap().as_str().unwrap(), "30");

        // Test Round-trip
        let deserialized = db.deserialize_record("CUSTOM", &serialized).unwrap();
        assert_eq!(deserialized.fields.len(), 3);
        assert_eq!(deserialized.fields[0].values[0].sub_values[0], "John");
        assert_eq!(deserialized.fields[1].values[0].sub_values[0], "Doe");
        assert_eq!(deserialized.fields[2].values[0].sub_values[0], "30");
    }

    Ok(())
}
