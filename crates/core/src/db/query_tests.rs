use crate::db::engine::Database;
use crate::db::models::*;
use std::fs;
use std::path::Path;

#[test]
fn test_compare_values() {
    assert!(Database::compare_values("abc", "=", "abc"));
    assert!(!Database::compare_values("abc", "=", "def"));
    assert!(Database::compare_values("abc", "!=", "def"));

    // Lexicographical comparison for strings (as currently implemented)
    assert!(Database::compare_values("5", ">", "10")); // "5" > "1"
    assert!(Database::compare_values("10", "<", "5")); // "1" < "5"

    // Wildcard handling in value (Pick style)
    assert!(Database::compare_values("football", "=", "[ball")); // Ends with
    assert!(Database::compare_values("football", "=", "foot]")); // Starts with
    assert!(Database::compare_values("football", "=", "[otba]")); // Contains
    assert!(!Database::compare_values("football", "!=", "[ball"));
    assert!(Database::compare_values("football", "!=", "ball]"));

    // Unknown operator
    assert!(!Database::compare_values("abc", "??", "abc"));

    // Word aliases
    assert!(Database::compare_values("abc", "EQ", "abc"));
    assert!(Database::compare_values("abc", "eq", "abc"));
    assert!(Database::compare_values("abc", "NE", "def"));
    assert!(Database::compare_values("10", "LT", "20"));
    assert!(Database::compare_values("20", "GT", "10"));
    assert!(Database::compare_values("10", "LE", "10"));
    assert!(Database::compare_values("10", "LE", "20"));
    assert!(Database::compare_values("20", "GE", "20"));
    assert!(Database::compare_values("20", "GE", "10"));

    // Trim check
    assert!(Database::compare_values("  abc  ", "=", "abc"));
    assert!(!Database::compare_values("abc", "=", "  abc  ")); // search_val is no longer trimmed in compare_values
}

#[test]
fn test_parse_query_trim() {
    let base_dir = "test_parse_query_trim_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();

    let q = db.parse_query("T1", &["WITH", "NAME", "=", "  John  "]).unwrap();
    if let QueryNode::Condition(c) = q {
        assert_eq!(c.value, "John");
    }
    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_parse_query() {
    let base_dir = "test_parse_query_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();

    // Simple WITH
    let q1 = db.parse_query("T1", &["WITH", "NAME", "=", "John"]);
    assert!(q1.is_some());
    if let Some(QueryNode::Condition(c)) = q1 {
        assert_eq!(c.field_name, "NAME");
        assert_eq!(c.op, "=");
        assert_eq!(c.value, "John");
    }

    // AND
    let q2 = db.parse_query("T1", &["NAME", "=", "John", "AND", "AGE", ">", "20"]);
    assert!(q2.is_some());
    if let Some(QueryNode::Logical { op, .. }) = q2 {
        match op {
            LogicalOp::And => {}
            _ => panic!("Expected AND"),
        }
    }

    // OR with quotes
    let q3 = db.parse_query("T1", &["NAME", "=", "\"John Doe\"", "OR", "NAME", "=", "Jane"]);
    assert!(q3.is_some());
    if let Some(QueryNode::Logical { right, .. }) = q3 {
        if let QueryNode::Condition(c) = *right {
            assert_eq!(c.value, "Jane");
        }
    }

    // Invalid
    assert!(db.parse_query("T1", &[]).is_none());
    assert!(db.parse_query("T1", &["NAME", "="]).is_none()); // Missing value

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_query_execution() {
    let base_dir = "test_query_exec_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
    db.create_test_account("QUERY_TEST").unwrap();
    db.logto("QUERY_TEST").unwrap();

    // Query USERS: WITH NAME = "John Doe"
    let q1 = db.parse_query("USERS", &["WITH", "NAME", "=", "\"John Doe\""]).unwrap();
    let results1 = db.query("USERS", false, &q1, None);
    assert_eq!(results1.len(), 1);
    assert_eq!(results1[0].0, "1");

    // Query USERS: WITH NAME = "[Smith]"
    let q2 = db.parse_query("USERS", &["NAME", "=", "[Smith]"]).unwrap();
    let results2 = db.query("USERS", false, &q2, None);
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].0, "2");

    // Query USERS: WITH NAME EQ "[Smith]"
    let q2_alt = db.parse_query("USERS", &["NAME", "EQ", "[Smith]"]).unwrap();
    let results2_alt = db.query("USERS", false, &q2_alt, None);
    assert_eq!(results2_alt.len(), 1);
    assert_eq!(results2_alt[0].0, "2");

    // Query with ID
    let q3 = db.parse_query("USERS", &["ID", "=", "2"]).unwrap();
    let results3 = db.query("USERS", false, &q3, None);
    assert_eq!(results3.len(), 1);
    assert_eq!(results3[0].0, "2");

    // Query with AND
    let q4 = db.parse_query("USERS", &["NAME", "=", "[John]", "AND", "EMAIL", "=", "[example]"]).unwrap();
    let results4 = db.query("USERS", false, &q4, None);
    assert_eq!(results4.len(), 1);

    // Multi-value match (if it was supported/tested)
    // Create a record with multi-values
    {
        let users = db.get_table_mut("USERS").unwrap();
        let rec = Record::from_display_string("Skill]Rust]Go^rust@example.com");
        users.records.insert("3".to_string(), rec);
        users.dirty = true;
        db.save().unwrap();
    }

    let q5 = db.parse_query("USERS", &["NAME", "=", "Rust"]).unwrap();
    let results5 = db.query("USERS", false, &q5, None);
    assert_eq!(results5.len(), 1);
    assert_eq!(results5[0].0, "3");

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_query_with_conversion() {
    let test_dir = "test_query_conv";
    if Path::new(test_dir).exists() {
        fs::remove_dir_all(test_dir).unwrap();
    }

    let mut db = Database::new(test_dir, None).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();

    // 1. Create a table and dictionary entry for PRICE with MD2
    db.create_table("PRODUCTS").unwrap();
    {
        let table = db.get_table_mut("PRODUCTS").unwrap();

        // PRICE dictionary entry
        let mut price_dict = Record::new();
        // Field 0: Attribute index (1-based)
        price_dict.fields.push(Field { values: vec![Value { sub_values: vec!["1".to_string()] }] });
        // Field 1: Name
        price_dict.fields.push(Field { values: vec![Value { sub_values: vec!["PRICE".to_string()] }] });
        // Field 2-6: empty
        for _ in 0..5 { price_dict.fields.push(Field::default()); }
        // Field 7: Conversion MD2
        price_dict.fields.push(Field { values: vec![Value { sub_values: vec!["MD2".to_string()] }] });

        table.dictionary.insert("PRICE".to_string(), price_dict);
    }

    // 2. Add a record with PRICE = 200 (internal format for 2.00)
    {
        let table = db.get_table_mut("PRODUCTS").unwrap();
        let mut record = Record::new();
        record.fields.push(Field { values: vec![Value { sub_values: vec!["200".to_string()] }] });
        table.records.insert("P1".to_string(), record);
    }

    // 3. Query WITH PRICE = "2.00"
    let query_str = vec!["WITH", "PRICE", "=", "2.00"];
    let query = db.parse_query("PRODUCTS", &query_str).unwrap();
    let results = db.query("PRODUCTS", false, &query, None);

    assert_eq!(results.len(), 1, "Should have found P1 with PRICE = 2.00 (via conversion)");
    assert_eq!(results[0].0, "P1");

    // 4. Query WITH PRICE = "200"
    let query_str2 = vec!["WITH", "PRICE", "=", "200"];
    let query2 = db.parse_query("PRODUCTS", &query_str2).unwrap();
    let results2 = db.query("PRODUCTS", false, &query2, None);

    assert_eq!(results2.len(), 0, "Should NOT have found P1 with PRICE = 200 (200 converted with MD2 would be 20000)");

    fs::remove_dir_all(test_dir).unwrap();
}

#[test]
fn test_query_with_wildcards() {
    let test_dir = "test_query_wildcards";
    if Path::new(test_dir).exists() {
        fs::remove_dir_all(test_dir).unwrap();
    }

    let mut db = Database::new(test_dir, None).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();

    db.create_table("ITEMS").unwrap();
    {
        let table = db.get_table_mut("ITEMS").unwrap();

        // Dictionary entry for DESC
        let mut desc_dict = Record::new();
        // Field 0: Attribute index (1-based). Let's use 1.
        desc_dict.fields.push(Field { values: vec![Value { sub_values: vec!["1".to_string()] }] });
        // Field 1: Name
        desc_dict.fields.push(Field { values: vec![Value { sub_values: vec!["DESC".to_string()] }] });
        table.dictionary.insert("DESC".to_string(), desc_dict);

        let mut r1 = Record::new();
        r1.fields.push(Field { values: vec![Value { sub_values: vec!["brand new item".to_string()] }] });
        table.records.insert("1".to_string(), r1);

        let mut r2 = Record::new();
        r2.fields.push(Field { values: vec![Value { sub_values: vec!["old item".to_string()] }] });
        table.records.insert("2".to_string(), r2);

        let mut r3 = Record::new();
        r3.fields.push(Field { values: vec![Value { sub_values: vec!["newest thing".to_string()] }] });
        table.records.insert("3".to_string(), r3);
    }

    // DESC is field 0

    // 1. Contains "new": [new]
    let query1 = db.parse_query("ITEMS", &vec!["WITH", "DESC", "=", "[new]"]).unwrap();
    let res1 = db.query("ITEMS", false, &query1, None);
    // Should find "brand new item" and "newest thing"
    assert!(res1.iter().any(|(id, _)| id == "1"), "Should find 'brand new item'");
    assert!(res1.iter().any(|(id, _)| id == "3"), "Should find 'newest thing'");
    assert!(!res1.iter().any(|(id, _)| id == "2"), "Should NOT find 'old item'");

    // 2. Starts with "new": new]
    let query2 = db.parse_query("ITEMS", &vec!["WITH", "DESC", "=", "new]"]).unwrap();
    let res2 = db.query("ITEMS", false, &query2, None);
    // Should find "newest thing"
    assert!(res2.iter().any(|(id, _)| id == "3"), "Should find 'newest thing'");
    assert!(!res2.iter().any(|(id, _)| id == "1"), "Should NOT find 'brand new item'");

    // 3. Ends with "item": [item
    let query3 = db.parse_query("ITEMS", &vec!["WITH", "DESC", "=", "[item"]).unwrap();
    let res3 = db.query("ITEMS", false, &query3, None);
    // Should find "brand new item" and "old item"
    assert!(res3.iter().any(|(id, _)| id == "1"), "Should find 'brand new item'");
    assert!(res3.iter().any(|(id, _)| id == "2"), "Should find 'old item'");
    assert!(!res3.iter().any(|(id, _)| id == "3"), "Should NOT find 'newest thing'");

    fs::remove_dir_all(test_dir).unwrap();
}
