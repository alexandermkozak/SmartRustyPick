#[cfg(test)]
mod tests {
    use crate::db::*;
    use std::fs;
    use std::io;
    use std::path::Path;

    #[test]
    fn test_system_dictionary_auto_creation() -> io::Result<()> {
        let base_dir = "test_system_dict_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("SYSTEM")?;

            // Verify $LOGS dictionary
            let logs = db.get_table("$LOGS").unwrap();
            assert!(logs.dictionary.contains_key("MESSAGE"));
            assert!(logs.dictionary.contains_key("DETAIL"));

            // Verify $ACCOUNTS dictionary
            let accounts = db.get_table("$ACCOUNTS").unwrap();
            assert!(accounts.dictionary.contains_key("PATH"));

            // Verify $CLIENTS dictionary
            let clients = db.get_table("$CLIENTS").unwrap();
            assert!(clients.dictionary.contains_key("THUMBPRINT"));
            assert!(clients.dictionary.contains_key("ACCOUNTS"));
            assert!(clients.dictionary.contains_key("ADMIN"));

            // Verify $SAVEDLISTS dictionary
            let savedlists = db.get_table("$SAVEDLISTS").unwrap();
            assert!(savedlists.dictionary.contains_key("TABLE"));
            assert!(savedlists.dictionary.contains_key("IS_DICT"));

            // Verify DIR dictionary
            let dir_table = db.get_table("DIR").unwrap();
            assert!(dir_table.dictionary.contains_key("TYPE"));

            // Manually corrupt dictionary for $LOGS
            {
                let logs_mut = db.get_table_mut("$LOGS").unwrap();
                logs_mut.dictionary.remove("MESSAGE");
                // Add an override
                logs_mut.dictionary.insert("DETAIL".to_string(), Record::from_display_string("2^OVERRIDE_DETAIL^L^10"));
                logs_mut.dirty = true;
            }
            db.save()?;
        }

        // Restart and check for self-healing
        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("SYSTEM")?;

            let logs = db.get_table("$LOGS").unwrap();
            // Should be restored
            assert!(logs.dictionary.contains_key("MESSAGE"), "MESSAGE dictionary should be restored");
            // Should NOT be overwritten
            let detail_dict = logs.dictionary.get("DETAIL").unwrap();
            let detail_val = detail_dict.get_field_display_string(1);
            assert!(detail_val.contains("OVERRIDE_DETAIL"), "Existing dictionary entry should NOT be overwritten (got '{}')", detail_val);
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_system_account_auto_creation() -> io::Result<()> {
        let base_dir = "test_system_account_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let db = Database::new(base_dir, None)?;
            // Check if SYSTEM account exists
            assert!(db.get_account_dir("SYSTEM").is_some(), "SYSTEM account should be automatically created");
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_system_logs_auto_creation() -> io::Result<()> {
        let base_dir = "test_system_logs_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            // Check if $LOGS file exists in SYSTEM account
            db.logto("SYSTEM")?;
            assert!(db.available_tables.contains("$LOGS"), "$LOGS table should be automatically created in SYSTEM account");
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_error_logging() -> io::Result<()> {
        let base_dir = "test_error_logging_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.log_detail = "detailed".to_string();
            db.max_log_records = 2;

            db.log_error("TEST_ACC", "First error")?;
            db.log_error("TEST_ACC", "Second error")?;
            db.log_error("TEST_ACC", "Third error")?; // Should evict first

            db.logto("SYSTEM")?;
            let logs = db.get_table("$LOGS").expect("$LOGS should exist");
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

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_system_clients_file() -> io::Result<()> {
        let base_dir = "test_system_clients_file_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.add_authorized_client("CLIENT1", "aabbccdd", vec!["ACC1".to_string()], false)?;
            db.add_authorized_client("CLIENT2", "11223344", vec![], true)?; // ADMIN

            // Verify $CLIENTS exists and contains CLIENT1 and CLIENT2
            db.logto("SYSTEM")?;
            assert!(db.available_tables.contains("$CLIENTS"), "$CLIENTS table should exist in SYSTEM account");

            let clients_table = db.get_table("$CLIENTS").expect("$CLIENTS should be loadable");
            assert!(clients_table.records.contains_key("CLIENT1"), "$CLIENTS should contain CLIENT1");
            assert!(clients_table.records.contains_key("CLIENT2"), "$CLIENTS should contain CLIENT2");

            let rec1 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1.fields[0].values[0].sub_values[0], "aabbccdd");
            assert_eq!(rec1.fields[1].values[0].sub_values[0], "ACC1");
            assert_eq!(rec1.fields[2].values[0].sub_values[0], "");

            let rec2 = clients_table.records.get("CLIENT2").unwrap();
            assert_eq!(rec2.fields[0].values[0].sub_values[0], "11223344");
            assert_eq!(rec2.fields[2].values[0].sub_values[0], "Y");

            // Verify in-memory map
            assert!(db.authorized_clients.contains_key("aabbccdd"));
            assert_eq!(db.authorized_clients.get("aabbccdd").unwrap().allowed_accounts, vec!["ACC1"]);
            assert!(!db.authorized_clients.get("aabbccdd").unwrap().is_admin);

            assert!(db.authorized_clients.contains_key("11223344"));
            assert!(db.authorized_clients.get("11223344").unwrap().is_admin);

            // Test add_client_account
            db.add_client_account("CLIENT1", "ACC2")?;
            db.logto("SYSTEM")?;
            let clients_table = db.get_table("$CLIENTS").unwrap();
            let rec1_v2 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1_v2.fields[1].values.len(), 2);
            assert_eq!(rec1_v2.fields[1].values[1].sub_values[0], "ACC2");
            assert!(db.authorized_clients.get("aabbccdd").unwrap().allowed_accounts.contains(&"ACC2".to_string()));

            // Test remove_client_account
            db.remove_client_account("CLIENT1", "ACC1")?;
            db.logto("SYSTEM")?;
            let clients_table = db.get_table("$CLIENTS").unwrap();
            let rec1_v3 = clients_table.records.get("CLIENT1").unwrap();
            assert_eq!(rec1_v3.fields[1].values.len(), 1);
            assert_eq!(rec1_v3.fields[1].values[0].sub_values[0], "ACC2");
            assert!(!db.authorized_clients.get("aabbccdd").unwrap().allowed_accounts.contains(&"ACC1".to_string()));

            // Test removal of client
            db.remove_authorized_client("CLIENT1")?;
            db.logto("SYSTEM")?;
            let clients_table = db.get_table("$CLIENTS").unwrap();
            assert!(!clients_table.records.contains_key("CLIENT1"), "$CLIENTS should not contain CLIENT1 after removal");
            assert!(!db.authorized_clients.contains_key("aabbccdd"), "In-memory map should be updated");
        }

        // Test auto-population on restart
        {
            let db = Database::new(base_dir, None)?;
            assert!(db.authorized_clients.contains_key("11223344"), "Should load CLIENT2 from $CLIENTS on restart");
            assert!(db.authorized_clients.get("11223344").unwrap().is_admin);
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_system_accounts_file() -> io::Result<()> {
        let base_dir = "test_system_accounts_file_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("SYSTEM")?;
            db.create_account("USER1", None)?;
            db.create_account("USER2", Some("custom_path/user2"))?;

            // Verify $ACCOUNTS exists and contains USER1 and USER2
            db.logto("SYSTEM")?;
            assert!(db.available_tables.contains("$ACCOUNTS"), "$ACCOUNTS table should exist in SYSTEM account");

            let accounts_table = db.get_table("$ACCOUNTS").expect("$ACCOUNTS should be loadable");
            assert!(accounts_table.records.contains_key("USER1"), "$ACCOUNTS should contain USER1");
            assert!(accounts_table.records.contains_key("USER2"), "$ACCOUNTS should contain USER2");
            assert!(!accounts_table.records.contains_key("SYSTEM"), "$ACCOUNTS should NOT contain SYSTEM");

            let rec1 = accounts_table.records.get("USER1").unwrap();
            assert!(rec1.fields[0].values[0].sub_values[0].contains("USER1"));

            let rec2 = accounts_table.records.get("USER2").unwrap();
            assert_eq!(rec2.fields[0].values[0].sub_values[0], "custom_path/user2");

            // Test deletion
            db.delete_account("USER1")?;
            db.logto("SYSTEM")?;
            let accounts_table = db.get_table("$ACCOUNTS").unwrap();
            assert!(!accounts_table.records.contains_key("USER1"), "$ACCOUNTS should not contain USER1 after deletion");
        }

        // Test auto-population on restart
        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("SYSTEM")?;
            let accounts_table = db.get_table("$ACCOUNTS").unwrap();
            assert!(accounts_table.records.contains_key("USER2"), "$ACCOUNTS should contain USER2 after restart");
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_accounts() -> io::Result<()> {
        let base_dir = "test_accounts_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.create_account("ACC1", None)?;
            db.create_account("ACC2", None)?;

            // Log to ACC1 and create a table
            db.logto("ACC1")?;
            db.create_table("T1")?;
            let t1 = db.get_table_mut("T1").unwrap();
            t1.records.insert("K1".to_string(), Record::from_bytes(b"VAL1"));
            t1.dirty = true;
            db.save()?;

            // Log to ACC2 and create a table with same name but different content
            db.logto("ACC2")?;
            db.create_table("T1")?;
            let t1_acc2 = db.get_table_mut("T1").unwrap();
            t1_acc2.records.insert("K1".to_string(), Record::from_bytes(b"VAL2"));
            t1_acc2.dirty = true;
            db.save()?;
        }

        // Re-open and verify isolation
        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("ACC1")?;
            let t1 = db.get_table("T1").unwrap();
            assert_eq!(String::from_utf8_lossy(&t1.records.get("K1").unwrap().to_bytes()), "VAL1");

            db.logto("ACC2")?;
            let t1 = db.get_table("T1").unwrap();
            assert_eq!(String::from_utf8_lossy(&t1.records.get("K1").unwrap().to_bytes()), "VAL2");
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_dir_file_auto_creation() -> io::Result<()> {
        let base_dir = "test_dir_auto_creation_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.logto("SYSTEM")?;
            assert!(db.available_tables.contains("DIR"), "DIR table should be automatically created in SYSTEM account");

            let dir_table = db.get_table("DIR").unwrap();
            assert!(dir_table.records.contains_key("$LOGS"));
            assert!(dir_table.records.contains_key("$ACCOUNTS"));
            assert!(dir_table.records.contains_key("$CLIENTS"));
            assert!(dir_table.records.contains_key("$SAVEDLISTS"));

            // Check record content
            let logs_dir_rec = dir_table.records.get("$LOGS").unwrap();
            assert_eq!(logs_dir_rec.fields[0].values[0].sub_values[0], "F");

            // Test create_test_account
            db.create_test_account("TEST_DIR")?;
            db.logto("TEST_DIR")?;
            assert!(db.available_tables.contains("DIR"), "DIR table should be created in test account");
            let dir_table_test = db.get_table("DIR").unwrap();
            assert!(dir_table_test.records.contains_key("USERS"));
            assert!(dir_table_test.records.contains_key("PRODUCTS"));
            assert!(!dir_table_test.records.contains_key("DIR"));
        }

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_dictionary_field_index() -> io::Result<()> {
        let base_dir = "test_dict_field_index_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
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

            let users = db.get_table("USERS").unwrap();
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
                let products = db.get_table("PRODUCTS").unwrap();
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

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_record_serialization() -> io::Result<()> {
        let base_dir = "test_serialization_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir, None)?;
            db.create_test_account("SERIAL_TEST")?;
            db.logto("SERIAL_TEST")?;

            // Setup a table with complex dictionary names
            db.create_table("CUSTOM")?;
            {
                let table = db.get_table_mut("CUSTOM").unwrap();
                table.dictionary.insert("FIRST.NAME".to_string(), Record::from_display_string("1^First Name^L^15"));
                table.dictionary.insert("LAST.NAME".to_string(), Record::from_display_string("2^Last Name^L^15"));
                table.dictionary.insert("AGE".to_string(), Record::from_display_string("3^Age^R^3"));
                table.records.insert("K1".to_string(), Record::from_display_string("John^Doe^30"));
                table.dirty = true;
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

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }
}
