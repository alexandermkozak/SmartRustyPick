//! Guards `docs/protocol.md` against silent drift from the wire types.
//!
//! The remote protocol is JSON built straight from [`Request`] and [`Response`]
//! in `models.rs` and dispatched by the `match` in `handler.rs`. Rename a field
//! or add a command there and the documentation is wrong with nothing to catch
//! it. These tests fail in that case: the expected sets below are the contract,
//! every name in them must appear in `docs/protocol.md`, and the structs must
//! serialize to exactly those keys.
//!
//! When the protocol genuinely changes: update `models.rs` / `handler.rs`, then
//! `docs/protocol.md`, then the lists here.

use crate::server::models::{Request, Response};

const PROTOCOL_DOC: &str = include_str!("../../../../docs/protocol.md");

/// Every JSON key a `Request` can carry.
const REQUEST_FIELDS: &[&str] = &[
    "command",
    "account",
    "target_account",
    "file",
    "key",
    "data",
    "structured_data",
    "is_dict",
    "query_node",
    "query_string",
    "sort_specs",
    "list_name",
    "batch_size",
    "thumbprint",
    "name",
    "accounts_list",
    "is_admin",
    "durable",
];

/// Every JSON key a `Response` can carry.
const RESPONSE_FIELDS: &[&str] = &["status", "message", "record", "results", "keys", "count"];

/// Every command string accepted by `handle_request_locked`.
const COMMANDS: &[&str] = &[
    "READ",
    "WRITE",
    "DELETE",
    "QUERY",
    "SELECT",
    "GET.NEXT",
    "CREATE.ACCOUNT",
    "DELETE.ACCOUNT",
    "CREATE.FILE",
    "DELETE.FILE",
    "AUTHORIZE.CONN",
    "DEAUTHORIZE.CONN",
    "ADD.CLIENT.ACCOUNT",
    "REMOVE.CLIENT.ACCOUNT",
];

fn json_keys<T: serde::Serialize>(value: &T) -> Vec<String> {
    serde_json::to_value(value)
        .unwrap()
        .as_object()
        .expect("wire type must serialize to a JSON object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn request_struct_serializes_to_exactly_the_documented_fields() {
    let mut actual = json_keys(&Request::default());
    actual.sort();
    let mut expected: Vec<String> = REQUEST_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "Request fields changed in models.rs. Update docs/protocol.md and REQUEST_FIELDS."
    );
}

#[test]
fn response_struct_serializes_to_exactly_the_documented_fields() {
    let mut actual = json_keys(&Response::default());
    actual.sort();
    let mut expected: Vec<String> = RESPONSE_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "Response fields changed in models.rs. Update docs/protocol.md and RESPONSE_FIELDS."
    );
}

#[test]
fn every_request_field_is_documented() {
    for field in REQUEST_FIELDS {
        assert!(
            PROTOCOL_DOC.contains(&format!("`{field}`")),
            "request field `{field}` is not mentioned in docs/protocol.md"
        );
    }
}

#[test]
fn every_response_field_is_documented() {
    for field in RESPONSE_FIELDS {
        assert!(
            PROTOCOL_DOC.contains(&format!("`{field}`")),
            "response field `{field}` is not mentioned in docs/protocol.md"
        );
    }
}

#[test]
fn every_command_is_documented() {
    for command in COMMANDS {
        assert!(
            PROTOCOL_DOC.contains(&format!("`{command}`")),
            "command `{command}` is not mentioned in docs/protocol.md"
        );
    }
}
