use crate::db::models::*;

#[test]
fn test_record_bytes_roundtrip() {
    let mut rec = Record::new();

    // Field 0: Multiple values
    let mut f0 = Field::default();
    f0.values.push(Value { sub_values: vec!["V1".to_string(), "V2".to_string()] });
    f0.values.push(Value { sub_values: vec!["V3".to_string()] });
    rec.fields.push(f0);

    // Field 1: Single value
    let mut f1 = Field::default();
    f1.values.push(Value { sub_values: vec!["F2".to_string()] });
    rec.fields.push(f1);

    let bytes = rec.to_bytes();
    let decoded = Record::from_bytes(&bytes);
    assert_eq!(rec, decoded);
    assert_eq!(decoded.fields.len(), 2);
    assert_eq!(decoded.fields[0].values.len(), 2);
    assert_eq!(decoded.fields[0].values[0].sub_values.len(), 2);
    assert_eq!(decoded.fields[0].values[0].sub_values[0], "V1");
    assert_eq!(decoded.fields[0].values[0].sub_values[1], "V2");
    assert_eq!(decoded.fields[0].values[1].sub_values[0], "V3");
}

#[test]
fn test_record_display_string() {
    let s = "F1^V1]V2^S1\\S2";
    let rec = Record::from_display_string(s);
    assert_eq!(rec.fields.len(), 3);
    assert_eq!(rec.fields[0].values[0].sub_values[0], "F1");
    assert_eq!(rec.fields[1].values.len(), 2);
    assert_eq!(rec.fields[1].values[0].sub_values[0], "V1");
    assert_eq!(rec.fields[1].values[1].sub_values[0], "V2");
    assert_eq!(rec.fields[2].values[0].sub_values.len(), 2);
    assert_eq!(rec.fields[2].values[0].sub_values[0], "S1");
    assert_eq!(rec.fields[2].values[0].sub_values[1], "S2");

    let out = rec.to_display_string();
    assert_eq!(s, out);
}

#[test]
fn test_record_edit_string() {
    let s = "F1\nV1]V2\nS1\\S2";
    let rec = Record::from_edit_string(s);
    assert_eq!(rec.fields.len(), 3);
    assert_eq!(rec.fields[0].values[0].sub_values[0], "F1");
    assert_eq!(rec.fields[1].values.len(), 2);

    let out = rec.to_edit_string();
    assert_eq!(s, out);

    // Test with trailing newline (should be ignored)
    let s_with_nl = "F1\nV1\n";
    let rec2 = Record::from_edit_string(s_with_nl);
    assert_eq!(rec2.fields.len(), 2);
    assert_eq!(rec2.fields[1].values[0].sub_values[0], "V1");
}

#[test]
fn test_empty_record() {
    let rec = Record::from_bytes(&[]);
    assert_eq!(rec.fields.len(), 0);
    assert_eq!(rec.to_bytes().len(), 0);
    assert_eq!(rec.to_display_string(), "");
}

#[test]
fn test_get_field_display_string() {
    let rec = Record::from_display_string("A^B]C^D\\E");
    assert_eq!(rec.get_field_display_string(0), "A");
    assert_eq!(rec.get_field_display_string(1), "B]C");
    assert_eq!(rec.get_field_display_string(2), "D\\E");
    assert_eq!(rec.get_field_display_string(3), ""); // Out of bounds
}

#[test]
fn test_table_new() {
    let table = Table::new();
    assert_eq!(table.records.len(), 0);
    assert_eq!(table.dictionary.len(), 0);
    assert!(!table.dirty);
}
