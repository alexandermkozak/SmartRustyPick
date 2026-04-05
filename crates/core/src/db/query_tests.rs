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

    // Pick specific operators
    assert!(Database::compare_values("football", "[", "ball")); // Ends with
    assert!(Database::compare_values("football", "]", "foot")); // Starts with
    assert!(Database::compare_values("football", "[]", "otba")); // Contains

    // Unknown operator
    assert!(!Database::compare_values("abc", "??", "abc"));

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

    // Query USERS: WITH NAME [] "Smith"
    let q2 = db.parse_query("USERS", &["NAME", "[]", "Smith"]).unwrap();
    let results2 = db.query("USERS", false, &q2, None);
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].0, "2");

    // Query with ID
    let q3 = db.parse_query("USERS", &["ID", "=", "2"]).unwrap();
    let results3 = db.query("USERS", false, &q3, None);
    assert_eq!(results3.len(), 1);
    assert_eq!(results3[0].0, "2");

    // Query with AND
    let q4 = db.parse_query("USERS", &["NAME", "[]", "John", "AND", "EMAIL", "[]", "example"]).unwrap();
    let results4 = db.query("USERS", false, &q4, None);
    assert_eq!(results4.len(), 1);

    // Multi-value match (if it was supported/tested)
    // Create a record with multi-values
    {
        let users = db.get_table_mut("USERS");
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
