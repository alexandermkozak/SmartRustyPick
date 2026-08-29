use crate::db::engine::Database;
use crate::db::models::*;
use crate::db::query::SortValue;
use std::fs;
use std::path::Path;

/// Orders one pair the way a sort would. The sort itself resolves each value
/// once up front; this spells that out for a test that only has two.
fn sort_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    SortValue::new(left).compare(&SortValue::new(right))
}

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
        users.touch_all();
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

#[test]
fn test_parse_sort_specs() {
    // No sort clauses
    let (rest, specs) = Database::parse_sort_specs(&["WITH", "DESC", "=", "[new]"]);
    assert_eq!(rest, vec!["WITH", "DESC", "=", "[new]"]);
    assert!(specs.is_empty());

    // Ascending only
    let (rest, specs) = Database::parse_sort_specs(&["PRICE_COL", "BY", "PRICE"]);
    assert_eq!(rest, vec!["PRICE_COL"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: false }]);

    // Descending only
    let (rest, specs) = Database::parse_sort_specs(&["BY.DSND", "PRICE"]);
    assert!(rest.is_empty());
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: true }]);

    // Multiple sorts keep left-to-right order
    let (rest, specs) = Database::parse_sort_specs(&["WITH", "DESC", "=", "[new]", "BY", "PRICE", "BY.DSND", "CREATE.DATE"]);
    assert_eq!(rest, vec!["WITH", "DESC", "=", "[new]"]);
    assert_eq!(specs, vec![
        SortSpec { field_name: "PRICE".to_string(), descending: false },
        SortSpec { field_name: "CREATE.DATE".to_string(), descending: true },
    ]);

    // Case insensitive operators
    let (_, specs) = Database::parse_sort_specs(&["by.dsnd", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: true }]);

    // Sort and column specifiers are order-agnostic
    let (rest, specs) = Database::parse_sort_specs(&["BY.DSND", "DESC", "DESC", "PRICE"]);
    assert_eq!(rest, vec!["DESC", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "DESC".to_string(), descending: true }]);

    let (rest, specs) = Database::parse_sort_specs(&["DESC", "PRICE", "BY.DSND", "DESC"]);
    assert_eq!(rest, vec!["DESC", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "DESC".to_string(), descending: true }]);

    // Dangling operator without a field is not a sort; the token is kept in the clause
    let (rest, specs) = Database::parse_sort_specs(&["PRICE_COL", "BY"]);
    assert_eq!(rest, vec!["PRICE_COL", "BY"]);
    assert!(specs.is_empty());

    let (rest, specs) = Database::parse_sort_specs(&["BY", "PRICE", "BY.DSND"]);
    assert_eq!(rest, vec!["BY.DSND"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: false }]);
}

fn setup_sort_db(test_dir: &str) -> Database {
    if Path::new(test_dir).exists() {
        fs::remove_dir_all(test_dir).unwrap();
    }
    let mut db = Database::new(test_dir, None).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();
    db.create_table("PRODUCTS").unwrap();
    {
        let table = db.get_table_mut("PRODUCTS").unwrap();
        table.dictionary.insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
        table.dictionary.insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10"));
        table.dictionary.insert("CREATE.DATE".to_string(), Record::from_display_string("3^CREATED^L^10"));

        table.records.insert("P1".to_string(), Record::from_display_string("new laptop^300^2024-01-01"));
        table.records.insert("P2".to_string(), Record::from_display_string("new mouse^25^2024-03-01"));
        table.records.insert("P3".to_string(), Record::from_display_string("old keyboard^100^2024-02-01"));
        table.records.insert("P4".to_string(), Record::from_display_string("new cable^25^2024-05-01"));
    }
    db
}

#[test]
fn test_sort_results_ascending_and_descending() {
    let test_dir = "test_sort_asc_dsnd";
    let mut db = setup_sort_db(test_dir);

    let ids = |res: &Vec<(String, Record)>| res.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();

    // BY PRICE - numeric ascending, not lexicographic ("25" before "100")
    let (_, specs) = Database::parse_sort_specs(&["BY", "PRICE"]);
    let mut res = db.query("PRODUCTS", false, &QueryNode::Condition(QueryCondition {
        field_name: "ID".to_string(),
        op: "!=".to_string(),
        value: "".to_string(),
    }), None);
    db.sort_results("PRODUCTS", &mut res, &specs);
    // P2 and P4 both 25, tie broken by ID
    assert_eq!(ids(&res), vec!["P2", "P4", "P3", "P1"]);

    // BY.DSND PRICE
    let (_, specs) = Database::parse_sort_specs(&["BY.DSND", "PRICE"]);
    db.sort_results("PRODUCTS", &mut res, &specs);
    assert_eq!(ids(&res), vec!["P1", "P3", "P2", "P4"]);

    fs::remove_dir_all(test_dir).unwrap();
}

#[test]
fn test_sort_results_multiple_keys() {
    let test_dir = "test_sort_multi";
    let mut db = setup_sort_db(test_dir);

    // BY PRICE BY.DSND CREATE.DATE
    let (clause, specs) = Database::parse_sort_specs(&["WITH", "DESC", "=", "[new]", "BY", "PRICE", "BY.DSND", "CREATE.DATE"]);
    let query = db.parse_query("PRODUCTS", &clause).unwrap();
    let mut res = db.query("PRODUCTS", false, &query, None);
    db.sort_results("PRODUCTS", &mut res, &specs);

    let ids: Vec<String> = res.iter().map(|(k, _)| k.clone()).collect();
    // Only the "new" products; P4/P2 share price 25 so the later date (P4) comes first.
    assert_eq!(ids, vec!["P4", "P2", "P1"]);

    fs::remove_dir_all(test_dir).unwrap();
}

#[test]
fn test_sort_text_is_case_insensitive() {
    let test_dir = "test_sort_case";
    let mut db = setup_sort_db(test_dir);
    {
        let table = db.get_table_mut("PRODUCTS").unwrap();
        table.records.insert("P5".to_string(), Record::from_display_string("Ztest^2^2024-06-01"));
        table.records.insert("P10".to_string(), Record::from_display_string("test!^2^2024-06-01"));
    }

    let (_, specs) = Database::parse_sort_specs(&["BY", "DESC"]);
    let keys = vec!["P5".to_string(), "P10".to_string()];
    let sorted = db.sort_keys("PRODUCTS", false, keys, &specs);
    // "test!" sorts before "Ztest" once case is ignored.
    assert_eq!(sorted, vec!["P10", "P5"]);

    // Values differing only in case are adjacent and keep a deterministic order.
    assert_eq!(sort_cmp("apple", "APPLE"), std::cmp::Ordering::Greater);
    assert_eq!(sort_cmp("Banana", "apple"), std::cmp::Ordering::Greater);

    fs::remove_dir_all(test_dir).unwrap();
}

#[test]
fn test_sort_keys_and_unknown_field() {
    let test_dir = "test_sort_keys";
    let mut db = setup_sort_db(test_dir);

    let (_, specs) = Database::parse_sort_specs(&["BY.DSND", "DESC"]);
    let keys = vec!["P1".to_string(), "P2".to_string(), "P3".to_string(), "P4".to_string()];
    let sorted = db.sort_keys("PRODUCTS", false, keys.clone(), &specs);
    assert_eq!(sorted, vec!["P3", "P2", "P1", "P4"]);

    // Unknown sort field falls back to ID order without panicking
    let (_, specs) = Database::parse_sort_specs(&["BY", "NOPE"]);
    let sorted = db.sort_keys("PRODUCTS", false, keys.clone(), &specs);
    assert_eq!(sorted, vec!["P1", "P2", "P3", "P4"]);

    // ID is sortable directly
    let (_, specs) = Database::parse_sort_specs(&["BY.DSND", "ID"]);
    let sorted = db.sort_keys("PRODUCTS", false, keys, &specs);
    assert_eq!(sorted, vec!["P4", "P3", "P2", "P1"]);

    fs::remove_dir_all(test_dir).unwrap();
}

/// The sort resolves each value once instead of parsing inside the comparator.
/// These pin the behaviour that refactoring must not change.
#[test]
fn test_sort_value_ordering_rules() {
    use std::cmp::Ordering;

    // Both numeric: compared as numbers, not as text ("9" < "10").
    assert_eq!(sort_cmp("9", "10"), Ordering::Less);
    assert_eq!(sort_cmp("10", "9"), Ordering::Greater);
    assert_eq!(sort_cmp("2.50", "2.5"), Ordering::Equal);
    assert_eq!(sort_cmp("-3", "2"), Ordering::Less);

    // Surrounding whitespace is not part of the value.
    assert_eq!(sort_cmp("  42  ", "42"), Ordering::Equal);
    assert_eq!(sort_cmp(" apple ", "apple"), Ordering::Equal);

    // Only one side numeric: falls back to text, so "10" precedes "apple".
    assert_eq!(sort_cmp("10", "apple"), Ordering::Less);
    assert_eq!(sort_cmp("apple", "10"), Ordering::Greater);

    // Neither numeric: case-insensitive, with the raw text breaking the tie.
    assert_eq!(sort_cmp("Ztest", "test!"), Ordering::Greater);
    assert_eq!(sort_cmp("apple", "apple"), Ordering::Equal);

    // A value that is not a number must not be coerced into one.
    assert_eq!(sort_cmp("1abc", "2abc"), Ordering::Less);
    assert_eq!(sort_cmp("", "0"), Ordering::Less);
}

/// Lowercasing is precomputed rather than folded during each comparison. It has
/// to agree with folding lazily, including where one character lowercases to
/// several and where a titlecase character is not classified as uppercase.
#[test]
fn test_sort_value_case_folding_matches_lazy_folding() {
    let cases = [
        ("apple", "APPLE"),
        ("Ztest", "test!"),
        // U+0130 lowercases to two code points.
        ("\u{0130}stanbul", "istanbul"),
        // U+01C5 is titlecase: not uppercase, but not its own lowercase either.
        ("\u{01C5}ex", "\u{01C6}ex"),
        ("Stra\u{00DF}e", "STRASSE"),
        ("\u{00E9}clair", "\u{00C9}CLAIR"),
    ];
    for (left, right) in cases {
        let lazy = left
            .trim()
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(right.trim().chars().flat_map(char::to_lowercase));
        let resolved = SortValue::new(left).compare(&SortValue::new(right));
        // The resolved comparison breaks a case-fold tie with the raw text, so
        // only a decided lazy ordering has to match exactly.
        if lazy != std::cmp::Ordering::Equal {
            assert_eq!(resolved, lazy, "{left:?} vs {right:?}");
        }
    }
}
