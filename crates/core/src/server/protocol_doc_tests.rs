//! Guards `docs/protocol.md` against silent drift from the wire types.
//!
//! The remote protocol is JSON built straight from [`Request`] and [`Response`]
//! in `models.rs` and dispatched by the `match` in `handler.rs`. Rename a field
//! or add a command there and the documentation is wrong with nothing to catch
//! it. These tests fail in that case: the expected sets below are the contract,
//! every name in them must appear in `docs/protocol.md`, and the structs must
//! serialize to exactly those keys. The commands are read out of `handler.rs`
//! itself, so a new one fails here until it is written up, and the nested
//! objects the management commands return in `record` and `results` are pinned
//! the same way as the wire structs.
//!
//! When the protocol genuinely changes: update `models.rs` / `handler.rs`, then
//! `docs/protocol.md`, then the lists here.

use crate::server::models::{ErrorCode, Request, Response};

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
    "explode",
    "list_name",
    "batch_size",
    "thumbprint",
    "name",
    "accounts_list",
    "is_admin",
    "durable",
    "field",
    "values",
    "limit",
];

/// Every JSON key a `Response` can carry.
const RESPONSE_FIELDS: &[&str] = &[
    "status",
    "message",
    "code",
    "record",
    "results",
    "keys",
    "count",
    "positions",
];

/// Every command string accepted by `handle_request_locked`.
const COMMANDS: &[&str] = &[
    "READ",
    "WRITE",
    "DELETE",
    "QUERY",
    "SELECT",
    "GET.NEXT",
    "CREATE.ACCOUNT",
    "CREATE.TEST.ACCOUNT",
    "DELETE.ACCOUNT",
    "CREATE.FILE",
    "SET.FILE",
    "DELETE.FILE",
    "AUTHORIZE.CONN",
    "DEAUTHORIZE.CONN",
    "ADD.CLIENT.ACCOUNT",
    "REMOVE.CLIENT.ACCOUNT",
    "GENERATE.CERT",
    "LIST.CONNS",
    "LIST.ACCOUNTS",
    "LIST.FILES",
    "FILE.STATS",
    "LIST.DICT",
    "SET.DICT",
    "CREATE.INDEX",
    "REBUILD.INDEX",
    "DELETE.INDEX",
    "LIST.INDEXES",
    "INDEX.STATS",
    "SET.INDEX.EXCLUDE",
    "SERVER.STATS",
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
    // Every field is `skip_serializing_if`, so only a fully populated response
    // shows the whole set. The empty one is pinned separately below.
    let populated = Response {
        status: "OK".to_string(),
        message: Some(String::new()),
        code: Some(ErrorCode::IoError),
        record: Some(serde_json::Value::Null),
        results: Some(Vec::new()),
        keys: Some(Vec::new()),
        count: Some(0),
        positions: Some(Vec::new()),
    };
    let mut actual = json_keys(&populated);
    actual.sort();
    let mut expected: Vec<String> = RESPONSE_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "Response fields changed in models.rs. Update docs/protocol.md and RESPONSE_FIELDS."
    );
}

/// `docs/protocol.md` promises "Only `status` is always present. Every other
/// field is omitted from the JSON unless the command populates it." A field
/// that loses its `skip_serializing_if` breaks that promise silently, so it is
/// pinned here rather than left to a reader of the documentation to notice.
#[test]
fn an_unpopulated_response_carries_only_status() {
    assert_eq!(json_keys(&Response::default()), vec!["status".to_string()]);
}

/// The other half of the contract: a client that omits a field must still
/// deserialize, which is what lets a response round-trip through the wire form.
#[test]
fn an_omitted_response_field_reads_back_as_unpopulated() {
    let response: Response = serde_json::from_str(r#"{"status": "OK"}"#).unwrap();
    assert!(response.message.is_none());
    assert!(response.code.is_none());
    assert!(response.record.is_none());
    assert!(response.results.is_none());
    assert!(response.keys.is_none());
    assert!(response.count.is_none());
    assert!(response.positions.is_none());
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

/// The codes are the half of an error response a client is allowed to branch
/// on, so every one of them has to be written up. A code added to the enum and
/// left out of `docs/protocol.md` fails here.
#[test]
fn every_error_code_is_documented() {
    for code in ErrorCode::ALL {
        assert!(
            PROTOCOL_DOC.contains(&format!("`{code}`")),
            "error code `{code}` is not listed in docs/protocol.md"
        );
    }
}

/// The wire spelling is the interface: renaming one silently would break every
/// client that branches on it, so the round trip is pinned rather than left to
/// the enum's own ordering.
#[test]
fn every_error_code_round_trips_through_its_wire_string() {
    for code in ErrorCode::ALL {
        assert_eq!(ErrorCode::from_wire(code.as_str()), Some(*code));
        assert_eq!(
            serde_json::to_string(code).unwrap(),
            format!("\"{}\"", code.as_str()),
            "`{code}` is not sent as the string it names"
        );
    }
    assert_eq!(ErrorCode::from_wire("NO_SUCH_CODE"), None);
}

/// Every error reply carries a code beside its message. A response without one
/// is exactly what this whole field exists to stop.
#[test]
fn an_error_reply_carries_a_code() {
    let refused: Response = serde_json::from_str(
        r#"{"status": "ERROR", "message": "Table 'X' not found in account 'Y'", "code": "FILE_NOT_FOUND"}"#,
    )
    .unwrap();
    assert_eq!(refused.code, Some(ErrorCode::FileNotFound));

    // A code from a newer server leaves the rest of the response readable.
    let newer: Response =
        serde_json::from_str(r#"{"status": "ERROR", "message": "Something new", "code": "SOMETHING_NEW"}"#).unwrap();
    assert_eq!(newer.code, None);
    assert_eq!(newer.message.as_deref(), Some("Something new"));
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

/// The command arms of the dispatch `match` in `handler.rs`, read out of the
/// source. Parsing beats a second hand-kept list: the point is to fail when a
/// command is added and left undocumented, which one more list maintained by
/// hand could never do.
fn commands_in_handler() -> Vec<String> {
    const HANDLER: &str = include_str!("handler.rs");
    // From `handle_request_locked` onwards: that function holds the one
    // complete dispatch, and the record commands are dispatched a second time,
    // at the same indent, by the helper the shared path shares with it.
    let (_, dispatch) = HANDLER
        .split_once("pub fn handle_request_locked")
        .expect("handler.rs must still have a handle_request_locked");
    dispatch
        .lines()
        .filter_map(|line| {
            // The arms of that `match`, and only those: they sit at one indent
            // inside `handle_request_locked`.
            let arm = line.strip_prefix("        \"")?;
            let (command, rest) = arm.split_once('"')?;
            rest.trim_start().starts_with("=>").then(|| command.to_string())
        })
        .collect()
}

/// The keys of a JSON object a command returns inside `record` or `results`.
fn value_keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .expect("must be a JSON object")
        .keys()
        .cloned()
        .collect()
}

fn assert_documented_shape(what: &str, actual: Vec<String>, expected: &[&str]) {
    let mut actual = actual;
    actual.sort();
    let mut wanted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    wanted.sort();
    assert_eq!(
        actual, wanted,
        "the {what} object changed. Update docs/protocol.md and this list."
    );
    for key in expected {
        assert!(
            PROTOCOL_DOC.contains(&format!("\"{key}\"")),
            "`{key}` of the {what} object is not shown in docs/protocol.md"
        );
    }
}

#[test]
fn documented_commands_match_the_handler_dispatch() {
    let mut actual = commands_in_handler();
    actual.sort();
    let mut expected: Vec<String> = COMMANDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "the commands handler.rs dispatches are no longer the documented ones. \
         Update docs/protocol.md and COMMANDS (an empty left side means the arms \
         of the dispatch match moved and commands_in_handler can no longer read them)."
    );
}

#[test]
fn list_accounts_entry_is_documented() {
    let value = serde_json::to_value(crate::db::AccountStats::default()).unwrap();
    assert_documented_shape(
        "LIST.ACCOUNTS",
        value_keys(&value),
        &[
            "name",
            "directory",
            "file_count",
            "record_count",
            "disk_bytes",
            "index_count",
            "stale_indexes",
            "unhealthy_files",
            "health",
        ],
    );
}

#[test]
fn file_stats_record_is_documented() {
    let value = serde_json::to_value(crate::db::FileStats::default()).unwrap();
    assert_documented_shape(
        "FILE.STATS",
        value_keys(&value),
        &[
            "account",
            "name",
            "record_count",
            "dict_count",
            "modulus",
            "version",
            "group_count",
            "smallest_group_bytes",
            "largest_group_bytes",
            "disk_bytes",
            "checksums",
            "legacy",
            "durable",
            "loaded",
            "modified_seconds_ago",
            "indexes",
            "group_bytes",
            "index_bytes",
            "group_records",
            "records_per_group_target",
            "load_factor",
            "records_until_growth",
            "records_until_shrink",
            "largest_group_share",
            "skew",
            "health",
        ],
    );
}

/// The nested objects `FILE.STATS` carries. Pinned separately because they are
/// their own shapes, and a field added to one of them is as invisible to a
/// reader of the documentation as a field added to the reply itself.
#[test]
fn file_stats_derived_objects_are_documented() {
    assert_documented_shape(
        "FILE.STATS group_records",
        value_keys(&serde_json::to_value(crate::db::GroupDistribution::default()).unwrap()),
        &[
            "groups",
            "min",
            "max",
            "mean",
            "median",
            "empty",
            "overweight",
            "unreadable",
            "buckets",
        ],
    );
    assert_documented_shape(
        "FILE.STATS group_records.buckets",
        value_keys(&serde_json::to_value(crate::db::DistributionBucket::default()).unwrap()),
        &["min", "max", "groups"],
    );
}

/// A verdict is the half of a measure a client is allowed to branch on, in the
/// same way an error code is: the wording of a `detail` may change, a verdict
/// may not.
#[test]
fn health_objects_are_documented() {
    assert_documented_shape(
        "health",
        value_keys(&serde_json::to_value(crate::db::Health::default()).unwrap()),
        &["verdict", "measures"],
    );
    assert_documented_shape(
        "health.measures",
        value_keys(&serde_json::to_value(crate::db::Measure::default()).unwrap()),
        &["id", "label", "value", "verdict", "threshold", "detail"],
    );
    assert_documented_shape(
        "LIST.FILES / LIST.ACCOUNTS health",
        value_keys(&serde_json::to_value(crate::db::HealthSummary::default()).unwrap()),
        &["verdict", "reasons"],
    );
    for verdict in [
        crate::db::Verdict::Good,
        crate::db::Verdict::Watch,
        crate::db::Verdict::Act,
    ] {
        assert_eq!(
            serde_json::to_string(&verdict).unwrap(),
            format!("\"{}\"", verdict.as_str()),
            "a verdict is not sent as the string it names"
        );
        assert!(
            PROTOCOL_DOC.contains(&format!("`{}`", verdict.as_str())),
            "verdict `{verdict}` is not listed in docs/protocol.md"
        );
    }
}

/// What `INDEX.STATS` adds on top of the listing's per-index object.
#[test]
fn index_report_record_is_documented() {
    assert_documented_shape(
        "INDEX.STATS",
        value_keys(&serde_json::to_value(crate::db::IndexReport::default()).unwrap()),
        &["record_count", "index", "top_values", "values_available"],
    );
    assert_documented_shape(
        "INDEX.STATS top_values",
        value_keys(&serde_json::to_value(crate::db::IndexValue::default()).unwrap()),
        &["value", "keys"],
    );
}

#[test]
fn index_stats_record_is_documented() {
    let value = serde_json::to_value(crate::db::IndexStats::default()).unwrap();
    assert_documented_shape(
        "LIST.INDEXES / CREATE.INDEX",
        value_keys(&value),
        &[
            "file",
            "field",
            "attribute",
            "values",
            "postings",
            "largest_postings",
            "modulus",
            "version",
            "group_count",
            "disk_bytes",
            "data_version",
            "stale",
            "loaded",
            "built_seconds_ago",
            "excluded",
            "usage",
            "health",
        ],
    );
    assert_documented_shape(
        "LIST.INDEXES usage",
        value_keys(&serde_json::to_value(crate::db::IndexUsageStats::default()).unwrap()),
        &[
            "lookups",
            "candidates",
            "matched",
            "measured_lookups",
            "excluded_lookups",
        ],
    );
}

#[test]
fn server_stats_record_is_documented() {
    let value = serde_json::to_value(crate::server::stats::ServerSnapshot::default()).unwrap();
    assert_documented_shape(
        "SERVER.STATS",
        value_keys(&value),
        &[
            "uptime_seconds",
            "started_at",
            "listen_addr",
            "total_connections",
            "rejected_connections",
            "total_requests",
            "failed_requests",
            "active_connections",
        ],
    );
    // The engine side of the snapshot, added to the object by the handler
    // rather than carried by the struct.
    for key in ["pending_writes", "loaded_tables", "authorized_clients"] {
        assert!(
            PROTOCOL_DOC.contains(&format!("\"{key}\"")),
            "`{key}` of the SERVER.STATS record is not shown in docs/protocol.md"
        );
    }
}

#[test]
fn server_stats_connection_is_documented() {
    let value = serde_json::to_value(crate::server::stats::ConnectionSnapshot::default()).unwrap();
    assert_documented_shape(
        "SERVER.STATS active_connections",
        value_keys(&value),
        &[
            "id",
            "peer",
            "client_name",
            "thumbprint",
            "is_admin",
            "connected_seconds",
            "requests",
            "last_command",
            "idle_seconds",
        ],
    );
}

#[test]
fn generate_cert_record_is_documented() {
    let generated = crate::server::certs::GeneratedCert {
        common_name: String::new(),
        thumbprint: String::new(),
        certificate_pem: String::new(),
        private_key_pem: String::new(),
        ca_pem: String::new(),
        cert_path: String::new(),
        key_path: String::new(),
        pfx_path: None,
    };
    let value = serde_json::to_value(&generated).unwrap();
    assert_documented_shape(
        "GENERATE.CERT",
        value_keys(&value),
        &[
            "common_name",
            "thumbprint",
            "certificate_pem",
            "private_key_pem",
            "ca_pem",
            "cert_path",
            "key_path",
            "pfx_path",
        ],
    );
}

#[test]
fn dictionary_entry_is_documented() {
    let value = crate::server::handler::dictionary_entry(&crate::db::Record::default());
    assert_documented_shape(
        "LIST.DICT / SET.DICT",
        value_keys(&value),
        &[
            "field",
            "heading",
            "justification",
            "width",
            "association",
            "associationDepth",
            "conversion",
            "definition",
        ],
    );
}

#[test]
fn exploded_position_is_documented() {
    let value = serde_json::to_value(crate::db::ValuePosition::value(0)).unwrap();
    assert_documented_shape("positions", value_keys(&value), &["value", "sub_value"]);
}
