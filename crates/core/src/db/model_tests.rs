use crate::db::models::*;

#[test]
fn test_record_bytes_roundtrip() {
    let mut rec = Record::new();

    // Field 0: Multiple values
    let mut f0 = Field::default();
    f0.values.push(Value::texts(["V1", "V2"]));
    f0.values.push(Value::text("V3"));
    rec.fields.push(f0);

    // Field 1: Single value
    let mut f1 = Field::default();
    f1.values.push(Value::text("F2"));
    rec.fields.push(f1);

    let bytes = rec.to_bytes();
    let decoded = Record::from_bytes(&bytes);
    assert_eq!(rec, decoded);
    assert_eq!(decoded.fields.len(), 2);
    assert_eq!(decoded.fields[0].values.len(), 2);
    assert_eq!(decoded.fields[0].values[0].sub_values.len(), 2);
    assert_eq!(text_of(&decoded.fields[0].values[0].sub_values[0]), "V1");
    assert_eq!(text_of(&decoded.fields[0].values[0].sub_values[1]), "V2");
    assert_eq!(text_of(&decoded.fields[0].values[1].sub_values[0]), "V3");
}

#[test]
fn test_record_display_string() {
    let s = "F1^V1]V2^S1\\S2";
    let rec = Record::from_display_string(s);
    assert_eq!(rec.fields.len(), 3);
    assert_eq!(text_of(&rec.fields[0].values[0].sub_values[0]), "F1");
    assert_eq!(rec.fields[1].values.len(), 2);
    assert_eq!(text_of(&rec.fields[1].values[0].sub_values[0]), "V1");
    assert_eq!(text_of(&rec.fields[1].values[1].sub_values[0]), "V2");
    assert_eq!(rec.fields[2].values[0].sub_values.len(), 2);
    assert_eq!(text_of(&rec.fields[2].values[0].sub_values[0]), "S1");
    assert_eq!(text_of(&rec.fields[2].values[0].sub_values[1]), "S2");

    let out = rec.to_display_string();
    assert_eq!(s, out);
}

#[test]
fn test_record_edit_string() {
    let s = "F1\nV1]V2\nS1\\S2";
    let rec = Record::from_edit_string(s);
    assert_eq!(rec.fields.len(), 3);
    assert_eq!(text_of(&rec.fields[0].values[0].sub_values[0]), "F1");
    assert_eq!(rec.fields[1].values.len(), 2);

    let out = rec.to_edit_string();
    assert_eq!(s, out);

    // Test with trailing newline (should be ignored)
    let s_with_nl = "F1\nV1\n";
    let rec2 = Record::from_edit_string(s_with_nl);
    assert_eq!(rec2.fields.len(), 2);
    assert_eq!(text_of(&rec2.fields[1].values[0].sub_values[0]), "V1");
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
    assert!(!table.is_dirty());
}

#[test]
fn test_get_value_display_string() {
    let record = Record::from_display_string("John^ADMIN]DEV]TEST\\LAB");

    // No position is the whole field, exactly as get_field_display_string.
    assert_eq!(record.get_value_display_string(1, None), "ADMIN]DEV]TEST\\LAB");
    assert_eq!(
        record.get_value_display_string(1, None),
        record.get_field_display_string(1)
    );

    // A value position takes one value, sub-values still joined.
    assert_eq!(
        record.get_value_display_string(1, Some(ValuePosition::value(0))),
        "ADMIN"
    );
    assert_eq!(
        record.get_value_display_string(1, Some(ValuePosition::value(2))),
        "TEST\\LAB"
    );

    // A sub-value position reaches one level deeper.
    assert_eq!(
        record.get_value_display_string(1, Some(ValuePosition::sub_value(2, 1))),
        "LAB"
    );

    // Out of range renders empty rather than panicking, so a select list that
    // outlived an edit to its records degrades quietly.
    assert_eq!(record.get_value_display_string(1, Some(ValuePosition::value(9))), "");
    assert_eq!(
        record.get_value_display_string(1, Some(ValuePosition::sub_value(0, 9))),
        ""
    );
    assert_eq!(record.get_value_display_string(9, Some(ValuePosition::value(0))), "");
    assert_eq!(record.get_value_display_string(9, None), "");
}

#[test]
fn test_select_list_keys() {
    let list = SelectList {
        table_name: "USERS".to_string(),
        is_dict: false,
        explode_field: Some("ROLES".to_string()),
        entries: vec![
            SelectEntry::at("1".to_string(), ValuePosition::value(0)),
            SelectEntry::at("1".to_string(), ValuePosition::value(2)),
            SelectEntry::at("2".to_string(), ValuePosition::sub_value(1, 0)),
        ],
    };

    // An exploded list repeats a key once per position...
    assert_eq!(list.keys().collect::<Vec<_>>(), vec!["1", "1", "2"]);
    assert_eq!(list.len(), 3);
    // ...but the commands that act on records want each record once.
    assert_eq!(list.unique_keys(), vec!["1".to_string(), "2".to_string()]);

    let plain = SelectList::from_keys("USERS".to_string(), false, vec!["1".to_string(), "2".to_string()]);
    assert!(plain.explode_field.is_none());
    assert!(plain.entries.iter().all(|e| e.position.is_none()));
    assert!(!plain.is_empty());
}

#[test]
fn attributes_survive_the_mark_characters_a_display_string_would_split_on() {
    // A dictionary heading is text someone typed. Built through the display
    // form, a `^` in it would silently become a new attribute; built from
    // attributes, it is stored as the character it is.
    let record = Record::from_attributes(["1", "PRICE ^ TAX", "L", "20"]);
    assert_eq!(record.fields.len(), 4);
    assert_eq!(record.get_field_display_string(1), "PRICE ^ TAX");

    assert_ne!(record, Record::from_display_string(&record.to_display_string()));
    assert_eq!(
        Record::from_display_string("1^NAME^L^20"),
        Record::from_attributes(["1", "NAME", "L", "20"])
    );
    assert!(Record::from_attributes(Vec::<String>::new()).fields.is_empty());
}

/// The bug this codec change exists to kill.
///
/// `from_bytes` used to run every sub-value through `String::from_utf8_lossy`,
/// so a byte that was not valid UTF-8 came back as `U+FFFD` and the original
/// was gone. The write reported success; the read returned something else.
#[test]
fn a_sub_value_that_is_not_utf8_survives_a_round_trip() {
    // A lone continuation byte, a truncated two-byte sequence, an over-long
    // encoding and a bare NUL: each of these used to become U+FFFD.
    let nasty: &[&[u8]] = &[
        b"\x80",
        b"\xC3",
        b"\xC0\xAF",
        b"\xED\xA0\x80",
        b"\x00",
        b"before\x00after",
        b"\xFF\xFB\x00\x01",
    ];

    for bytes in nasty {
        let mut record = Record::new();
        record.fields.push(Field {
            values: vec![Value::bytes(bytes.to_vec())],
        });

        let decoded = Record::from_bytes(&record.to_bytes());
        assert_eq!(decoded, record, "{:?} did not survive the round trip", bytes);
        assert_eq!(decoded.fields[0].values[0].sub_values[0], bytes.to_vec());
    }
}

/// Every byte a sub-value may hold, in one value, in one pass.
#[test]
fn every_non_mark_byte_survives_a_round_trip() {
    // The marks are excluded because they are the structure - see below.
    let payload: Vec<u8> = (0..=255u8).filter(|b| ![FM, VM, SVM].contains(b)).collect();
    let record = Record {
        fields: vec![Field {
            values: vec![Value::bytes(payload.clone())],
        }],
    };

    let decoded = Record::from_bytes(&record.to_bytes());
    assert_eq!(decoded.fields[0].values[0].sub_values[0], payload);
    assert_eq!(decoded, record);
}

/// The limit, asserted so that it is a decision rather than a surprise.
///
/// `FM`/`VM`/`SVM` *are* the record's structure, so a mark inside a sub-value
/// is indistinguishable from the separator it is and splits the value on the
/// way back. That is the MultiValue data model, and it is why content that may
/// hold arbitrary bytes belongs in a blob referenced by the record rather than
/// inlined into one.
#[test]
fn a_mark_byte_inside_a_sub_value_is_still_structural() {
    let record = Record {
        fields: vec![Field {
            values: vec![Value::bytes(vec![b'a', VM, b'b'])],
        }],
    };

    let decoded = Record::from_bytes(&record.to_bytes());
    assert_eq!(
        decoded.fields[0].values.len(),
        2,
        "a value mark inside a sub-value separates two values, as it always has"
    );
    assert_eq!(decoded.fields[0].values[0].sub_values[0], b"a".to_vec());
    assert_eq!(decoded.fields[0].values[1].sub_values[0], b"b".to_vec());
}

/// Text goes in and text comes out: the ordinary path is unchanged.
#[test]
fn text_values_are_unaffected_by_the_byte_representation() {
    let record = Record::from_display_string("1^NAME]OTHER^L");
    let decoded = Record::from_bytes(&record.to_bytes());
    assert_eq!(decoded, record);
    assert_eq!(decoded.to_display_string(), "1^NAME]OTHER^L");
    assert_eq!(decoded.fields[1].values[0].first_text().unwrap(), "NAME");
    assert_eq!(decoded.fields[1].values[1].first_text().unwrap(), "OTHER");
}

// ---------------------------------------------------------------------------
// Capabilities (#111): the two questions `is_admin` used to answer at once.
// ---------------------------------------------------------------------------

#[test]
fn test_admin_still_means_every_capability_and_every_account() {
    // The compatibility promise: an authorization written before capabilities
    // existed behaves exactly as it did, so nothing has to be migrated.
    let admin = ClientInfo {
        name: "root".to_string(),
        is_admin: true,
        ..Default::default()
    };
    for capability in Capability::ALL {
        assert!(admin.can(capability), "{capability} must be held by ADMIN");
    }
    assert!(admin.may_reach("ANY_ACCOUNT"));
    assert!(admin.may_reach("SYSTEM"));
    assert_eq!(admin.effective_capabilities(), Capability::ALL.to_vec());
}

#[test]
fn test_a_capability_does_not_grant_an_account() {
    // The whole point: a credential that provisions is not thereby a credential
    // that reads. A client holding accounts:manage can create an account and
    // cannot look inside it - or inside anybody else's.
    let provisioner = ClientInfo {
        name: "provisioner".to_string(),
        capabilities: vec![Capability::AccountsManage],
        ..Default::default()
    };
    assert!(provisioner.can(Capability::AccountsManage));
    assert!(!provisioner.may_reach("SALES"));
    assert!(!provisioner.may_reach("SYSTEM"));
    // Nor does one capability imply another: authorizing clients is the step
    // that would turn provisioning into total power, so it is held separately.
    assert!(!provisioner.can(Capability::ClientsManage));
    assert!(!provisioner.can(Capability::ServerObserve));
}

#[test]
fn test_an_account_does_not_grant_a_capability() {
    // And the converse, which is what stops the two growing back together.
    let user = ClientInfo {
        name: "user".to_string(),
        allowed_accounts: vec!["SALES".to_string()],
        ..Default::default()
    };
    assert!(user.may_reach("SALES"));
    assert!(!user.may_reach("SUPPORT"));
    for capability in Capability::ALL {
        assert!(!user.can(capability), "{capability} must not come with an account");
    }
    assert!(user.effective_capabilities().is_empty());
}

#[test]
fn test_capability_names_round_trip_and_unknown_names_are_refused() {
    for capability in Capability::ALL {
        assert_eq!(Capability::parse(capability.as_str()), Some(capability));
        // Case-insensitively, because an operator typing at a prompt should not
        // have to remember which half of `Accounts:Manage` is capitalised.
        assert_eq!(Capability::parse(&capability.as_str().to_uppercase()), Some(capability));
        assert_eq!(
            Capability::parse(&format!("  {}  ", capability.as_str())),
            Some(capability)
        );
    }
    // A typo must not read as a grant that quietly does nothing.
    assert_eq!(Capability::parse("accounts:mange"), None);
    assert_eq!(Capability::parse("accounts"), None);
    assert_eq!(Capability::parse("admin"), None);
    assert_eq!(Capability::parse(""), None);
}

#[test]
fn test_effective_capabilities_are_sorted_and_deduplicated() {
    // The listing is an inventory, so it must not depend on the order values
    // happened to be written into `$CLIENTS`.
    let client = ClientInfo {
        name: "ops".to_string(),
        capabilities: vec![
            Capability::ServerObserve,
            Capability::AccountsManage,
            Capability::ServerObserve,
        ],
        ..Default::default()
    };
    assert_eq!(
        client.effective_capabilities(),
        vec![Capability::AccountsManage, Capability::ServerObserve]
    );
}
