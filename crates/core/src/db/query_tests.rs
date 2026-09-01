use crate::db::engine::Database;
use crate::db::models::*;
use crate::db::query::SortValue;
use crate::test_support::{isolated_config, TempDir};

/// The sort half of `parse_clause_specs`, so the tests that predate `BY.EXP`
/// keep reading the way they did.
fn parse_sort_specs<'a>(parts: &[&'a str]) -> (Vec<&'a str>, Vec<SortSpec>) {
    let (rest, specs, _) = Database::parse_clause_specs(parts);
    (rest, specs)
}


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
    let dir = TempDir::new("parse_query_trim");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();

    let q = db.parse_query("T1", &["WITH", "NAME", "=", "  John  "]).unwrap();
    if let QueryNode::Condition(c) = q {
        assert_eq!(c.value, "John");
    }
}

#[test]
fn test_parse_query() {
    let dir = TempDir::new("parse_query");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();

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
}

#[test]
fn test_query_execution() {
    let dir = TempDir::new("query_exec");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
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
        let users_handle = db.get_table_mut("USERS").unwrap();
        let mut users = users_handle.write();
        let rec = Record::from_display_string("Skill]Rust]Go^rust@example.com");
        users.records.insert("3".to_string(), rec);
        users.touch_all();
        drop(users);
        db.save().unwrap();
    }

    let q5 = db.parse_query("USERS", &["NAME", "=", "Rust"]).unwrap();
    let results5 = db.query("USERS", false, &q5, None);
    assert_eq!(results5.len(), 1);
    assert_eq!(results5[0].0, "3");
}

#[test]
fn test_query_with_conversion() {
    let dir = TempDir::new("query_conv");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();

    // 1. Create a table and dictionary entry for PRICE with MD2
    db.create_table("PRODUCTS").unwrap();
    {
        let table_handle = db.get_table_mut("PRODUCTS").unwrap();
        let mut table = table_handle.write();

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
        let table_handle = db.get_table_mut("PRODUCTS").unwrap();
        let mut table = table_handle.write();
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
}

#[test]
fn test_query_with_wildcards() {
    let dir = TempDir::new("query_wildcards");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();

    db.create_table("ITEMS").unwrap();
    {
        let table_handle = db.get_table_mut("ITEMS").unwrap();
        let mut table = table_handle.write();

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
}

#[test]
fn test_parse_sort_specs() {
    // No sort clauses
    let (rest, specs) = parse_sort_specs(&["WITH", "DESC", "=", "[new]"]);
    assert_eq!(rest, vec!["WITH", "DESC", "=", "[new]"]);
    assert!(specs.is_empty());

    // Ascending only
    let (rest, specs) = parse_sort_specs(&["PRICE_COL", "BY", "PRICE"]);
    assert_eq!(rest, vec!["PRICE_COL"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: false }]);

    // Descending only
    let (rest, specs) = parse_sort_specs(&["BY.DSND", "PRICE"]);
    assert!(rest.is_empty());
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: true }]);

    // Multiple sorts keep left-to-right order
    let (rest, specs) = parse_sort_specs(&["WITH", "DESC", "=", "[new]", "BY", "PRICE", "BY.DSND", "CREATE.DATE"]);
    assert_eq!(rest, vec!["WITH", "DESC", "=", "[new]"]);
    assert_eq!(specs, vec![
        SortSpec { field_name: "PRICE".to_string(), descending: false },
        SortSpec { field_name: "CREATE.DATE".to_string(), descending: true },
    ]);

    // Case insensitive operators
    let (_, specs) = parse_sort_specs(&["by.dsnd", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: true }]);

    // Sort and column specifiers are order-agnostic
    let (rest, specs) = parse_sort_specs(&["BY.DSND", "DESC", "DESC", "PRICE"]);
    assert_eq!(rest, vec!["DESC", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "DESC".to_string(), descending: true }]);

    let (rest, specs) = parse_sort_specs(&["DESC", "PRICE", "BY.DSND", "DESC"]);
    assert_eq!(rest, vec!["DESC", "PRICE"]);
    assert_eq!(specs, vec![SortSpec { field_name: "DESC".to_string(), descending: true }]);

    // Dangling operator without a field is not a sort; the token is kept in the clause
    let (rest, specs) = parse_sort_specs(&["PRICE_COL", "BY"]);
    assert_eq!(rest, vec!["PRICE_COL", "BY"]);
    assert!(specs.is_empty());

    let (rest, specs) = parse_sort_specs(&["BY", "PRICE", "BY.DSND"]);
    assert_eq!(rest, vec!["BY.DSND"]);
    assert_eq!(specs, vec![SortSpec { field_name: "PRICE".to_string(), descending: false }]);
}

/// A `TempDir` rooted database with the `PRODUCTS` fixture used by the sort
/// tests below. The guard is returned alongside the database so callers keep
/// the directory alive for as long as they use it.
fn setup_sort_db(label: &str) -> (TempDir, Database) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC1", None).unwrap();
    db.logto("ACC1").unwrap();
    db.create_table("PRODUCTS").unwrap();
    {
        let table_handle = db.get_table_mut("PRODUCTS").unwrap();
        let mut table = table_handle.write();
        table.dictionary.insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
        table.dictionary.insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10"));
        table.dictionary.insert("CREATE.DATE".to_string(), Record::from_display_string("3^CREATED^L^10"));

        table.records.insert("P1".to_string(), Record::from_display_string("new laptop^300^2024-01-01"));
        table.records.insert("P2".to_string(), Record::from_display_string("new mouse^25^2024-03-01"));
        table.records.insert("P3".to_string(), Record::from_display_string("old keyboard^100^2024-02-01"));
        table.records.insert("P4".to_string(), Record::from_display_string("new cable^25^2024-05-01"));
    }
    (dir, db)
}

#[test]
fn test_sort_results_ascending_and_descending() {
    let (_dir, db) = setup_sort_db("sort_asc_dsnd");

    let ids = |res: &Vec<(String, Record)>| res.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();

    // BY PRICE - numeric ascending, not lexicographic ("25" before "100")
    let (_, specs) = parse_sort_specs(&["BY", "PRICE"]);
    let mut res = db.query("PRODUCTS", false, &QueryNode::Condition(QueryCondition {
        field_name: "ID".to_string(),
        op: "!=".to_string(),
        value: "".to_string(),
    }), None);
    db.sort_results("PRODUCTS", &mut res, &specs);
    // P2 and P4 both 25, tie broken by ID
    assert_eq!(ids(&res), vec!["P2", "P4", "P3", "P1"]);

    // BY.DSND PRICE
    let (_, specs) = parse_sort_specs(&["BY.DSND", "PRICE"]);
    db.sort_results("PRODUCTS", &mut res, &specs);
    assert_eq!(ids(&res), vec!["P1", "P3", "P2", "P4"]);
}

#[test]
fn test_sort_results_multiple_keys() {
    let (_dir, db) = setup_sort_db("sort_multi");

    // BY PRICE BY.DSND CREATE.DATE
    let (clause, specs) = parse_sort_specs(&["WITH", "DESC", "=", "[new]", "BY", "PRICE", "BY.DSND", "CREATE.DATE"]);
    let query = db.parse_query("PRODUCTS", &clause).unwrap();
    let mut res = db.query("PRODUCTS", false, &query, None);
    db.sort_results("PRODUCTS", &mut res, &specs);

    let ids: Vec<String> = res.iter().map(|(k, _)| k.clone()).collect();
    // Only the "new" products; P4/P2 share price 25 so the later date (P4) comes first.
    assert_eq!(ids, vec!["P4", "P2", "P1"]);
}

#[test]
fn test_sort_text_is_case_insensitive() {
    let (_dir, db) = setup_sort_db("sort_case");
    {
        let table_handle = db.get_table_mut("PRODUCTS").unwrap();
        let mut table = table_handle.write();
        table.records.insert("P5".to_string(), Record::from_display_string("Ztest^2^2024-06-01"));
        table.records.insert("P10".to_string(), Record::from_display_string("test!^2^2024-06-01"));
    }

    let (_, specs) = parse_sort_specs(&["BY", "DESC"]);
    let keys = vec!["P5".to_string(), "P10".to_string()];
    let sorted = db.sort_keys("PRODUCTS", false, keys, &specs);
    // "test!" sorts before "Ztest" once case is ignored.
    assert_eq!(sorted, vec!["P10", "P5"]);

    // Values differing only in case are adjacent and keep a deterministic order.
    assert_eq!(sort_cmp("apple", "APPLE"), std::cmp::Ordering::Greater);
    assert_eq!(sort_cmp("Banana", "apple"), std::cmp::Ordering::Greater);
}

#[test]
fn test_sort_keys_and_unknown_field() {
    let (_dir, db) = setup_sort_db("sort_keys");

    let (_, specs) = parse_sort_specs(&["BY.DSND", "DESC"]);
    let keys = vec!["P1".to_string(), "P2".to_string(), "P3".to_string(), "P4".to_string()];
    let sorted = db.sort_keys("PRODUCTS", false, keys.clone(), &specs);
    assert_eq!(sorted, vec!["P3", "P2", "P1", "P4"]);

    // Unknown sort field falls back to ID order without panicking
    let (_, specs) = parse_sort_specs(&["BY", "NOPE"]);
    let sorted = db.sort_keys("PRODUCTS", false, keys.clone(), &specs);
    assert_eq!(sorted, vec!["P1", "P2", "P3", "P4"]);

    // ID is sortable directly
    let (_, specs) = parse_sort_specs(&["BY.DSND", "ID"]);
    let sorted = db.sort_keys("PRODUCTS", false, keys, &specs);
    assert_eq!(sorted, vec!["P4", "P3", "P2", "P1"]);
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

#[test]
fn test_parse_clause_specs_explode() {
    // The compact spelling absorbs the operator and value that follow the field.
    let (rest, sorts, explodes) = Database::parse_clause_specs(&["BY.EXP", "ACCOUNTS", "=", "\"TEST\"", "ACCOUNTS"]);
    assert_eq!(rest, vec!["ACCOUNTS"]);
    assert!(sorts.is_empty());
    assert_eq!(explodes.len(), 1);
    assert_eq!(explodes[0].field_name, "ACCOUNTS");
    assert_eq!(
        explodes[0].condition,
        Some(QueryCondition { field_name: "ACCOUNTS".to_string(), op: "=".to_string(), value: "TEST".to_string() })
    );

    // A bare column name after the field is a column, not an operator.
    let (rest, _, explodes) = Database::parse_clause_specs(&["BY.EXP", "ACCOUNTS", "NAME"]);
    assert_eq!(rest, vec!["NAME"]);
    assert_eq!(explodes.len(), 1);
    assert_eq!(explodes[0].condition, None);

    // Word-alias operators are recognised just as the symbols are.
    let (rest, _, explodes) = Database::parse_clause_specs(&["BY.EXP", "ROLES", "EQ", "DEV"]);
    assert!(rest.is_empty());
    assert_eq!(explodes[0].condition.as_ref().unwrap().value, "DEV");

    // Case-insensitive, and freely interleaved with sorts and columns.
    let (rest, sorts, explodes) = Database::parse_clause_specs(&["NAME", "by.exp", "ROLES", "BY.DSND", "ROLES"]);
    assert_eq!(rest, vec!["NAME"]);
    assert_eq!(sorts, vec![SortSpec { field_name: "ROLES".to_string(), descending: true }]);
    assert_eq!(explodes.len(), 1);

    // A trailing operator with no field name is kept as a plain token.
    let (rest, _, explodes) = Database::parse_clause_specs(&["NAME", "BY.EXP"]);
    assert_eq!(rest, vec!["NAME", "BY.EXP"]);
    assert!(explodes.is_empty());

    // An operator with no value after it is not absorbed.
    let (rest, _, explodes) = Database::parse_clause_specs(&["BY.EXP", "ROLES", "="]);
    assert_eq!(rest, vec!["="]);
    assert_eq!(explodes[0].condition, None);
}

#[test]
fn test_parse_query_consuming_reports_its_end() {
    let dir = TempDir::new("parse_query_consuming");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();

    // The criteria end, and the columns after them are left to the caller.
    let (node, consumed) = db.parse_query_consuming("T1", &["WITH", "NAME", "=", "Bob", "NAME", "EMAIL"]);
    assert!(node.is_some());
    assert_eq!(consumed, 4);

    let (node, consumed) = db.parse_query_consuming("T1", &["WITH", "NAME", "=", "Bob", "AND", "AGE", ">", "20", "NAME"]);
    assert!(matches!(node, Some(QueryNode::Logical { .. })));
    assert_eq!(consumed, 8);

    // A trailing AND with nothing after it stops the clause rather than
    // swallowing the token.
    let (node, consumed) = db.parse_query_consuming("T1", &["WITH", "NAME", "=", "Bob", "AND"]);
    assert!(node.is_some());
    assert_eq!(consumed, 4);

    // Too short to be a condition at all.
    assert_eq!(db.parse_query_consuming("T1", &["WITH", "NAME", "="]).1, 0);
    assert!(db.parse_query_consuming("T1", &[]).0.is_none());
}

/// A file whose ROLES field is multivalued, and whose second record has a
/// sub-valued role, so the tests below reach every level of the hierarchy. The
/// guard is returned alongside the database so callers keep the directory alive
/// for as long as they use it.
fn roles_db(label: &str) -> (TempDir, Database) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("ACC", None).unwrap();
    db.logto("ACC").unwrap();
    db.create_table("USERS").unwrap();
    let table_handle = db.get_table_mut("USERS").unwrap();
    let mut table = table_handle.write();
    table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
    table.dictionary.insert("ROLES".to_string(), Record::from_display_string("2^ROLES^L^20"));
    table.records.insert("1".to_string(), Record::from_display_string("John^ADMIN]DEV]TEST"));
    table.records.insert("2".to_string(), Record::from_display_string("Jane^DEV]TEST\\LAB"));
    table.records.insert("3".to_string(), Record::from_display_string("Zed^SALES"));
    table.mark_dict_dirty();
    table.touch_all();
    drop(table);
    (dir, db)
}

#[test]
fn test_query_exploded_matches_every_position() {
    let (_dir, db) = roles_db("exploded_positions");

    let query = db.parse_query("USERS", &["WITH", "ROLES", "=", "[TEST]"]).unwrap();
    let explode = ExplodeSpec { field_name: "ROLES".to_string(), condition: None };
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let rows = Database::query_exploded_in(&table, false, Some(&query), Some(&explode), None);
    let seen: Vec<(&str, Option<ValuePosition>)> =
        rows.iter().map(|(e, _)| (e.key.as_str(), e.position)).collect();

    // John's TEST is the third value; Jane's is the first sub-value of her
    // second, so the position reaches down to the sub-value.
    assert_eq!(seen, vec![
        ("1", Some(ValuePosition::value(2))),
        ("2", Some(ValuePosition::sub_value(1, 0))),
    ]);

}

#[test]
fn test_query_exploded_without_criterion_is_one_row_per_value() {
    let (_dir, db) = roles_db("exploded_bare");
    let explode = ExplodeSpec { field_name: "ROLES".to_string(), condition: None };
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let rows = Database::query_exploded_in(&table, false, None, Some(&explode), None);
    let seen: Vec<(&str, Option<ValuePosition>)> =
        rows.iter().map(|(e, _)| (e.key.as_str(), e.position)).collect();

    // Every value gets a row, sub-values stay together, and a single-valued
    // record still gets its one row.
    assert_eq!(seen, vec![
        ("1", Some(ValuePosition::value(0))),
        ("1", Some(ValuePosition::value(1))),
        ("1", Some(ValuePosition::value(2))),
        ("2", Some(ValuePosition::value(0))),
        ("2", Some(ValuePosition::value(1))),
        ("3", Some(ValuePosition::value(0))),
    ]);

}

#[test]
fn test_query_exploded_unions_positions_across_conditions() {
    let (_dir, db) = roles_db("exploded_union");

    // Two conditions on the exploded field: both their positions become rows.
    let query = db.parse_query("USERS", &["WITH", "ROLES", "=", "DEV", "OR", "ROLES", "=", "ADMIN"]).unwrap();
    let explode = ExplodeSpec { field_name: "ROLES".to_string(), condition: None };
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let rows = Database::query_exploded_in(&table, false, Some(&query), Some(&explode), None);
    let seen: Vec<(&str, Option<ValuePosition>)> =
        rows.iter().map(|(e, _)| (e.key.as_str(), e.position)).collect();
    assert_eq!(seen, vec![
        ("1", Some(ValuePosition::value(0))),
        ("1", Some(ValuePosition::value(1))),
        ("2", Some(ValuePosition::value(0))),
    ]);

}

#[test]
fn test_query_exploded_keeps_records_matched_on_another_field() {
    let (_dir, db) = roles_db("exploded_other_field");

    // The criterion names NAME, not the exploded ROLES. Inclusion is still the
    // query's decision, so the record survives - as one unexploded row.
    let query = db.parse_query("USERS", &["WITH", "NAME", "=", "Zed"]).unwrap();
    let explode = ExplodeSpec { field_name: "ROLES".to_string(), condition: None };
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let rows = Database::query_exploded_in(&table, false, Some(&query), Some(&explode), None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.key, "3");
    assert_eq!(rows[0].0.position, Some(ValuePosition::value(0)));

}

#[test]
fn test_query_exploded_without_spec_is_an_ordinary_selection() {
    let (_dir, db) = roles_db("exploded_none");
    let query = db.parse_query("USERS", &["WITH", "ROLES", "=", "[TEST]"]).unwrap();
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let rows = Database::query_exploded_in(&table, false, Some(&query), None, None);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(e, _)| e.position.is_none()));

    // An unknown explode field is no explode at all rather than an error.
    let unknown = ExplodeSpec { field_name: "NOPE".to_string(), condition: None };
    let rows = Database::query_exploded_in(&table, false, Some(&query), Some(&unknown), None);
    assert!(rows.iter().all(|(e, _)| e.position.is_none()));

}

#[test]
fn test_sort_entries_uses_the_exploded_value() {
    let (_dir, db) = roles_db("sort_entries");
    let explode = ExplodeSpec { field_name: "ROLES".to_string(), condition: None };
    let table_handle = db.get_table_read_only_for_account("ACC", "USERS").unwrap();
    let table = table_handle.read();

    let mut rows = Database::query_exploded_in(&table, false, None, Some(&explode), None);
    let explode_idx = Database::explode_field_index(&table, Some(&explode));
    assert_eq!(explode_idx, Some(1));
    let specs = vec![SortSpec { field_name: "ROLES".to_string(), descending: false }];
    Database::sort_entries_in(&table, &mut rows, &specs, explode_idx);

    // Ordered by each row's own value, not by the whole joined field - which
    // would have kept every one of record 1's rows together.
    let values: Vec<String> = rows.iter()
        .map(|(e, r)| r.get_value_display_string(1, e.position))
        .collect();
    assert_eq!(values, vec!["ADMIN", "DEV", "DEV", "SALES", "TEST", "TEST\\LAB"]);

}
