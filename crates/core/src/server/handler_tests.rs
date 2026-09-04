use crate::db::{ClientInfo, Database, ValuePosition};
use crate::server::handler::handle_request;
use crate::server::models::{ErrorCode, Request};
use crate::test_support::{TempDir, isolated_config};
use std::path::Path;
use std::sync::{Arc, RwLock};

#[test]
fn test_handle_request_read_write() {
    let dir = TempDir::new("handler");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("SERVER_TEST").unwrap();

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "test_tp".to_string(),
        allowed_accounts: vec!["SERVER_TEST".to_string()],
        is_admin: false,
    };

    // Test WRITE
    let req_write = Request {
        command: "WRITE".to_string(),
        account: Some("SERVER_TEST".to_string()),
        file: Some("USERS".to_string()),
        key: Some("3".to_string()),
        data: Some(serde_json::Value::String("Alice^alice@example.com".to_string())),
        ..Default::default()
    };
    let resp_write = handle_request(req_write, &db_arc, &client_info);
    assert_eq!(resp_write.status, "OK");

    // Test READ
    let req_read = Request {
        command: "READ".to_string(),
        account: Some("SERVER_TEST".to_string()),
        file: Some("USERS".to_string()),
        key: Some("3".to_string()),
        ..Default::default()
    };
    let resp_read = handle_request(req_read, &db_arc, &client_info);
    assert_eq!(resp_read.status, "OK");
    // Verify record is now structured (Value::Object)
    let record = resp_read.record.unwrap();
    assert!(record.is_object());
    assert_eq!(
        record.as_object().unwrap().get("name").unwrap().as_str().unwrap(),
        "Alice"
    );
    assert_eq!(
        record.as_object().unwrap().get("email").unwrap().as_str().unwrap(),
        "alice@example.com"
    );

    // Test Access Denied
    let req_denied = Request {
        command: "READ".to_string(),
        account: Some("SYSTEM".to_string()),
        file: Some("$ACCOUNTS".to_string()),
        key: Some("SYSTEM".to_string()),
        ..Default::default()
    };
    let resp_denied = handle_request(req_denied, &db_arc, &client_info);
    assert_eq!(resp_denied.status, "ERROR");
    assert_eq!(resp_denied.code, Some(ErrorCode::AccessDenied));
}

#[test]
fn test_create_and_delete_file_target_the_requested_account() {
    // A headless server is not logged into any account, so these commands must act on
    // the account named in the request rather than on `current_account`.
    let dir = TempDir::new("server_create_file");
    let base_dir = dir.path();
    let db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.create_account("FILE_TEST", None).unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("FILE_TEST".to_string()),
            file: Some("STOCK".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert!(Path::new(base_dir).join("FILE_TEST").join("STOCK").exists());

    // The new file must be usable straight away through the same account.
    let resp = handle_request(
        Request {
            command: "WRITE".to_string(),
            account: Some("FILE_TEST".to_string()),
            file: Some("STOCK".to_string()),
            key: Some("ITEM1".to_string()),
            data: Some(serde_json::Value::String("Widget".to_string())),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);

    // Without an account there is nothing to act on, so the request must be rejected.
    let resp = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            file: Some("ORPHAN".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::AccountNotSpecified));

    let resp = handle_request(
        Request {
            command: "DELETE.FILE".to_string(),
            account: Some("FILE_TEST".to_string()),
            file: Some("STOCK".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert!(!Path::new(base_dir).join("FILE_TEST").join("STOCK").exists());
}

#[test]
fn test_create_file_durable_flag_is_honoured() {
    // A file created with `durable` must be flushed on every write even though
    // the database as a whole buffers.
    let dir = TempDir::new("server_durable_file");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("DUR_TEST", None).unwrap();
    db.set_current_account("");
    db.durable_writes = false;
    db.flush_max_pending = 1_000;
    db.flush_interval = std::time::Duration::from_secs(3_600);

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    for (file, durable) in [("LEDGER", true), ("SCRATCH", false)] {
        let resp = handle_request(
            Request {
                command: "CREATE.FILE".to_string(),
                account: Some("DUR_TEST".to_string()),
                file: Some(file.to_string()),
                durable: Some(durable),
                ..Default::default()
            },
            &db_arc,
            &admin,
        );
        assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    }

    let write = |file: &str| {
        handle_request(
            Request {
                command: "WRITE".to_string(),
                account: Some("DUR_TEST".to_string()),
                file: Some(file.to_string()),
                key: Some("K1".to_string()),
                data: Some(serde_json::Value::String("V1".to_string())),
                ..Default::default()
            },
            &db_arc,
            &admin,
        )
    };

    assert_eq!(write("SCRATCH").status, "OK");
    assert!(
        db_arc.read().unwrap().has_pending_writes(),
        "a normal file should be buffered"
    );

    assert_eq!(write("LEDGER").status, "OK");
    assert!(
        !db_arc.read().unwrap().has_pending_writes(),
        "a durable file must flush at once"
    );
    assert!(
        db_arc
            .write()
            .unwrap()
            .is_table_durable_for_account("DUR_TEST", "LEDGER")
    );
    assert!(
        !db_arc
            .write()
            .unwrap()
            .is_table_durable_for_account("DUR_TEST", "SCRATCH")
    );
}

#[test]
fn test_set_file_promotes_and_demotes_an_existing_file() {
    // The reason the command exists: changing durability without recreating the
    // file, so the data it already holds is not the price of the flag.
    let dir = TempDir::new("server_set_file");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("SET_TEST", None).unwrap();
    db.set_current_account("");
    db.durable_writes = false;
    db.flush_max_pending = 1_000;
    db.flush_interval = std::time::Duration::from_secs(3_600);

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };
    let client = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "client_tp".to_string(),
        allowed_accounts: vec!["SET_TEST".to_string()],
        is_admin: false,
    };

    let set = |durable: Option<bool>, who: &ClientInfo| {
        handle_request(
            Request {
                command: "SET.FILE".to_string(),
                account: Some("SET_TEST".to_string()),
                file: Some("LEDGER".to_string()),
                durable,
                ..Default::default()
            },
            &db_arc,
            who,
        )
    };

    let resp = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("SET_TEST".to_string()),
            file: Some("LEDGER".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);

    let write = || {
        handle_request(
            Request {
                command: "WRITE".to_string(),
                account: Some("SET_TEST".to_string()),
                file: Some("LEDGER".to_string()),
                key: Some("K1".to_string()),
                data: Some(serde_json::Value::String("V1".to_string())),
                ..Default::default()
            },
            &db_arc,
            &admin,
        )
    };

    assert_eq!(write().status, "OK");
    assert!(
        db_arc.read().unwrap().has_pending_writes(),
        "the write should still be buffered"
    );

    // Promoting flushes what the file had buffered, and every later write goes
    // to disk before it is acknowledged.
    let resp = set(Some(true), &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.record.unwrap()["durable"], serde_json::json!(true));
    assert!(
        !db_arc.read().unwrap().has_pending_writes(),
        "promoting must flush what was buffered"
    );
    assert!(
        db_arc
            .write()
            .unwrap()
            .is_table_durable_for_account("SET_TEST", "LEDGER")
    );

    assert_eq!(write().status, "OK");
    assert!(
        !db_arc.read().unwrap().has_pending_writes(),
        "a promoted file must flush at once"
    );

    // And back again.
    assert_eq!(set(Some(false), &admin).status, "OK");
    assert!(
        !db_arc
            .write()
            .unwrap()
            .is_table_durable_for_account("SET_TEST", "LEDGER")
    );
    assert_eq!(write().status, "OK");
    assert!(
        db_arc.read().unwrap().has_pending_writes(),
        "a demoted file buffers again"
    );

    // A request that names no flag must not be read as a demotion.
    let resp = set(None, &admin);
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::MissingField));
    assert_eq!(resp.message.unwrap(), "Durability flag not specified");

    // Storage decisions are administrative, like creating the file was.
    let resp = set(Some(true), &client);
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::AdminRequired));
    assert!(
        !db_arc
            .write()
            .unwrap()
            .is_table_durable_for_account("SET_TEST", "LEDGER")
    );

    // A file that does not exist is a not-found error, not a silent success.
    let resp = handle_request(
        Request {
            command: "SET.FILE".to_string(),
            account: Some("SET_TEST".to_string()),
            file: Some("NOPE".to_string()),
            durable: Some(true),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::FileNotFound));
}

#[test]
fn test_handle_request_query_select() {
    let dir = TempDir::new("server_query");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("QUERY_TEST").unwrap();
    db.logto("QUERY_TEST").unwrap();

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "test_tp".to_string(),
        allowed_accounts: vec!["QUERY_TEST".to_string()],
        is_admin: true, // Admin to access SYSTEM if needed, but we use QUERY_TEST
    };

    // Test QUERY
    let req_query = Request {
        command: "QUERY".to_string(),
        account: Some("QUERY_TEST".to_string()),
        file: Some("USERS".to_string()),
        query_string: Some("NAME = [John]".to_string()),
        ..Default::default()
    };
    let resp_query = handle_request(req_query, &db_arc, &client_info);
    assert_eq!(resp_query.status, "OK");
    let results = resp_query.results.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "1");
    // Verify results are now structured (Value::Object instead of Value::String)
    assert!(results[0].1.is_object());
    assert_eq!(
        results[0].1.as_object().unwrap().get("name").unwrap().as_str().unwrap(),
        "John Doe"
    );

    // Test SELECT and GET.NEXT
    let req_select = Request {
        command: "SELECT".to_string(),
        account: Some("QUERY_TEST".to_string()),
        file: Some("USERS".to_string()),
        list_name: Some("MYLIST".to_string()),
        ..Default::default()
    };
    let resp_select = handle_request(req_select, &db_arc, &client_info);
    assert_eq!(resp_select.status, "OK");
    assert_eq!(resp_select.count, Some(2));

    let req_next = Request {
        command: "GET.NEXT".to_string(),
        list_name: Some("MYLIST".to_string()),
        batch_size: Some(1),
        ..Default::default()
    };
    let resp_next = handle_request(req_next, &db_arc, &client_info);
    assert_eq!(resp_next.status, "OK");
    let next_results = resp_next.results.unwrap();
    assert_eq!(next_results.len(), 1);
    assert!(next_results[0].1.is_object());
}

#[test]
fn test_management_commands_report_accounts_files_and_statistics() {
    // The dashboard navigates the database through these three commands, so
    // between them they have to describe an account without ever handing back a
    // record.
    let dir = TempDir::new("server_management");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("MGMT_TEST").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "LIST.ACCOUNTS".to_string(),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let accounts = resp.results.unwrap();
    let account = accounts
        .iter()
        .find(|(name, _)| name == "MGMT_TEST")
        .expect("the created account is listed");
    assert!(account.1["file_count"].as_u64().unwrap() > 0);
    assert!(account.1["directory"].as_str().unwrap().contains("MGMT_TEST"));

    let resp = handle_request(
        Request {
            command: "LIST.FILES".to_string(),
            account: Some("MGMT_TEST".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let files = resp.keys.unwrap();
    assert!(files.contains(&"USERS".to_string()), "USERS missing from {:?}", files);
    assert_eq!(resp.count, Some(files.len()));
    // The listing carries the durability flag beside the name, so a client does
    // not have to read the account's DIR file to find it.
    let listed = resp.results.unwrap();
    assert_eq!(listed.len(), files.len());
    let users = listed
        .iter()
        .find(|(name, _)| name == "USERS")
        .expect("USERS is listed");
    assert_eq!(users.1["durable"], serde_json::json!(false));

    let resp = handle_request(
        Request {
            command: "FILE.STATS".to_string(),
            account: Some("MGMT_TEST".to_string()),
            file: Some("USERS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stats = resp.record.unwrap();
    assert_eq!(stats["account"].as_str().unwrap(), "MGMT_TEST");
    assert_eq!(stats["name"].as_str().unwrap(), "USERS");
    assert_eq!(stats["record_count"].as_u64().unwrap(), 2);
    assert!(stats["dict_count"].as_u64().unwrap() > 0);
    assert!(stats["modulus"].as_u64().unwrap() > 0);
    assert!(
        stats.get("records").is_none(),
        "statistics must not carry record contents"
    );

    // A file that does not exist is a not-found error, not an empty answer.
    let resp = handle_request(
        Request {
            command: "FILE.STATS".to_string(),
            account: Some("MGMT_TEST".to_string()),
            file: Some("NOPE".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::FileNotFound));
}

#[test]
fn test_management_commands_respect_the_clients_permissions() {
    let dir = TempDir::new("server_management_perm");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("VISIBLE").unwrap();
    db.create_test_account("HIDDEN").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "test_tp".to_string(),
        allowed_accounts: vec!["VISIBLE".to_string()],
        is_admin: false,
    };

    // An account the client cannot reach must not even be named to it.
    let resp = handle_request(
        Request {
            command: "LIST.ACCOUNTS".to_string(),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let names: Vec<String> = resp.results.unwrap().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["VISIBLE".to_string()]);

    let resp = handle_request(
        Request {
            command: "LIST.FILES".to_string(),
            account: Some("HIDDEN".to_string()),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::AccessDenied));

    // The management views of the server itself are administrative.
    for command in ["SERVER.STATS", "LIST.CONNS", "GENERATE.CERT"] {
        let resp = handle_request(
            Request {
                command: command.to_string(),
                name: Some("intruder".to_string()),
                ..Default::default()
            },
            &db_arc,
            &client_info,
        );
        assert_eq!(resp.status, "ERROR", "{} must be refused", command);
        assert_eq!(resp.code, Some(ErrorCode::AdminRequired));
    }
}

#[test]
fn test_list_conns_and_server_stats_describe_the_running_server() {
    let dir = TempDir::new("server_stats");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.add_authorized_client("reporting-bot", "AB12CD", vec!["SALES".to_string()], false)
        .unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "LIST.CONNS".to_string(),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let clients = resp.results.unwrap();
    let (name, info) = clients
        .iter()
        .find(|(name, _)| name == "reporting-bot")
        .expect("the authorized client is listed");
    assert_eq!(name, "reporting-bot");
    // Thumbprints are stored lowercase, whatever case they were given in.
    assert_eq!(info["thumbprint"].as_str().unwrap(), "ab12cd");
    assert_eq!(info["accounts"][0].as_str().unwrap(), "SALES");
    assert!(!info["is_admin"].as_bool().unwrap());

    let resp = handle_request(
        Request {
            command: "SERVER.STATS".to_string(),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stats = resp.record.unwrap();
    assert!(stats["active_connections"].is_array());
    assert!(stats["total_requests"].is_number());
    // The engine-side numbers are merged into the same object.
    assert_eq!(stats["authorized_clients"].as_u64().unwrap(), 1);
    assert!(stats["pending_writes"].is_number());
}

/// The test account's USERS file: John has three roles, Jane has two and her
/// second is sub-valued. The guard is returned alongside the database so callers
/// keep the directory alive for as long as they use it.
fn exploded_test_db(label: &str) -> (TempDir, Arc<RwLock<Database>>, ClientInfo) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("EXP_TEST").unwrap();
    db.logto("EXP_TEST").unwrap();
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "test_tp".to_string(),
        allowed_accounts: vec!["EXP_TEST".to_string()],
        is_admin: false,
    };
    (dir, Arc::new(RwLock::new(db)), client_info)
}

#[test]
fn test_query_returns_multivalued_fields_as_arrays() {
    let (_dir, db_arc, client_info) = exploded_test_db("mv_shape");

    let req = Request {
        command: "QUERY".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("USERS".to_string()),
        ..Default::default()
    };
    let resp = handle_request(req, &db_arc, &client_info);
    assert_eq!(resp.status, "OK");
    // Nothing was exploded, so no positions are sent.
    assert!(resp.positions.is_none());

    let results = resp.results.unwrap();
    let john = &results.iter().find(|(k, _)| k == "1").unwrap().1;
    assert_eq!(john["roles"], serde_json::json!(["ADMIN", "DEV", "TEST"]));
    // A single-valued field is still a plain string.
    assert_eq!(john["name"], serde_json::json!("John Doe"));

    let jane = &results.iter().find(|(k, _)| k == "2").unwrap().1;
    assert_eq!(jane["roles"], serde_json::json!(["DEV", ["TEST", "LAB"]]));
}

#[test]
fn test_query_explodes_and_reports_positions() {
    let (_dir, db_arc, client_info) = exploded_test_db("explode_query");

    // The explode field named on its own, with the criterion in query_string.
    let req = Request {
        command: "QUERY".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("USERS".to_string()),
        query_string: Some("WITH ROLES = [TEST]".to_string()),
        explode: Some(vec!["ROLES".to_string()]),
        ..Default::default()
    };
    let resp = handle_request(req, &db_arc, &client_info);
    assert_eq!(resp.status, "OK");

    let results = resp.results.unwrap();
    let positions = resp.positions.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(positions.len(), results.len());
    assert_eq!(results[0].0, "1");
    assert_eq!(positions[0], Some(ValuePosition::value(2)));
    assert_eq!(results[1].0, "2");
    assert_eq!(positions[1], Some(ValuePosition::sub_value(1, 0)));

    // The same question spelled entirely inside query_string.
    let req = Request {
        command: "QUERY".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("USERS".to_string()),
        query_string: Some("BY.EXP ROLES = [TEST]".to_string()),
        ..Default::default()
    };
    let resp = handle_request(req, &db_arc, &client_info);
    assert_eq!(resp.status, "OK");
    assert_eq!(resp.results.unwrap().len(), 2);
    assert_eq!(
        resp.positions.unwrap(),
        vec![Some(ValuePosition::value(2)), Some(ValuePosition::sub_value(1, 0)),]
    );
}

#[test]
fn test_select_explodes_and_get_next_carries_the_positions() {
    let (_dir, db_arc, client_info) = exploded_test_db("explode_select");

    // A bare explode: every value of every record becomes a row, so the count
    // is of rows rather than of distinct records.
    let req_select = Request {
        command: "SELECT".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("USERS".to_string()),
        list_name: Some("MVLIST".to_string()),
        explode: Some(vec!["ROLES".to_string()]),
        ..Default::default()
    };
    let resp = handle_request(req_select, &db_arc, &client_info);
    assert_eq!(resp.status, "OK");
    assert_eq!(resp.count, Some(5));

    let req_next = Request {
        command: "GET.NEXT".to_string(),
        account: Some("EXP_TEST".to_string()),
        list_name: Some("MVLIST".to_string()),
        batch_size: Some(10),
        ..Default::default()
    };
    let resp = handle_request(req_next, &db_arc, &client_info);
    assert_eq!(resp.status, "OK");
    let results = resp.results.unwrap();
    let positions = resp.positions.unwrap();
    assert_eq!(results.len(), 5);
    assert_eq!(positions.len(), 5);
    let seen: Vec<(&str, Option<ValuePosition>)> = results
        .iter()
        .map(|(k, _)| k.as_str())
        .zip(positions.iter().copied())
        .collect();
    assert_eq!(
        seen,
        vec![
            ("1", Some(ValuePosition::value(0))),
            ("1", Some(ValuePosition::value(1))),
            ("1", Some(ValuePosition::value(2))),
            ("2", Some(ValuePosition::value(0))),
            ("2", Some(ValuePosition::value(1))),
        ]
    );

    // The cursor is exhausted, so the list reports EOF as it always has.
    let req_next = Request {
        command: "GET.NEXT".to_string(),
        account: Some("EXP_TEST".to_string()),
        list_name: Some("MVLIST".to_string()),
        ..Default::default()
    };
    assert_eq!(handle_request(req_next, &db_arc, &client_info).status, "EOF");
}

#[test]
fn test_unexploded_select_sends_no_positions() {
    let (_dir, db_arc, client_info) = exploded_test_db("no_positions");

    let req_select = Request {
        command: "SELECT".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("USERS".to_string()),
        list_name: Some("PLAIN".to_string()),
        ..Default::default()
    };
    assert_eq!(handle_request(req_select, &db_arc, &client_info).count, Some(2));

    let req_next = Request {
        command: "GET.NEXT".to_string(),
        account: Some("EXP_TEST".to_string()),
        list_name: Some("PLAIN".to_string()),
        batch_size: Some(10),
        ..Default::default()
    };
    let resp = handle_request(req_next, &db_arc, &client_info);
    assert_eq!(resp.results.unwrap().len(), 2);
    // An ordinary list leaves the field out rather than sending a run of nulls.
    assert!(resp.positions.is_none());
}

/// A database with one account and one file, and a client that may reach it.
fn dictionary_test_db(name: &str) -> (TempDir, Arc<RwLock<Database>>, ClientInfo) {
    let dir = TempDir::new(name);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("DICT_TEST", None).unwrap();
    db.create_table_for_account("DICT_TEST", "STOCK").unwrap();
    db.set_current_account("");

    let client_info = ClientInfo {
        name: "dict_client".to_string(),
        thumbprint: "dict_tp".to_string(),
        allowed_accounts: vec!["DICT_TEST".to_string()],
        is_admin: false,
    };
    (dir, Arc::new(RwLock::new(db)), client_info)
}

fn set_dict(
    db: &Arc<RwLock<Database>>,
    client: &ClientInfo,
    key: &str,
    attributes: serde_json::Value,
) -> crate::server::models::Response {
    handle_request(
        Request {
            command: "SET.DICT".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("STOCK".to_string()),
            key: Some(key.to_string()),
            structured_data: Some(attributes),
            ..Default::default()
        },
        db,
        client,
    )
}

fn list_dict(db: &Arc<RwLock<Database>>, client: &ClientInfo) -> crate::server::models::Response {
    handle_request(
        Request {
            command: "LIST.DICT".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("STOCK".to_string()),
            ..Default::default()
        },
        db,
        client,
    )
}

#[test]
fn test_set_dict_stores_an_entry_and_fills_in_its_defaults() {
    let (_dir, db_arc, client_info) = dictionary_test_db("set_dict");

    // Only the attribute number is required; everything else has a default, and
    // the response is the stored entry so the caller can see what they were.
    let resp = set_dict(&db_arc, &client_info, "NAME", serde_json::json!({ "field": 1 }));
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stored = resp.record.unwrap();
    assert_eq!(stored["field"], 1);
    assert_eq!(stored["heading"], "NAME");
    assert_eq!(stored["justification"], "L");
    assert_eq!(stored["width"], 10);
    assert_eq!(stored["conversion"], "");
    // An entry with no conversion is the four attributes the CLI writes, not
    // four followed by a run of empty ones.
    assert_eq!(stored["definition"], "1^NAME^L^10");

    // A form sends numbers as strings, and a lowercase justification is a
    // spelling rather than a mistake.
    let resp = set_dict(
        &db_arc,
        &client_info,
        "PRICE",
        serde_json::json!({ "field": "2", "heading": "Unit price", "justification": "r", "width": "12", "conversion": "MD2" }),
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stored = resp.record.unwrap();
    assert_eq!(stored["field"], 2);
    assert_eq!(stored["justification"], "R");
    assert_eq!(stored["width"], 12);
    assert_eq!(stored["definition"], "2^Unit price^R^12^^^^MD2");
}

#[test]
fn test_set_dict_refuses_a_definition_no_query_could_use() {
    let (_dir, db_arc, client_info) = dictionary_test_db("set_dict_refusals");

    let cases: Vec<(serde_json::Value, &str)> = vec![
        (serde_json::json!({}), "Attribute number not specified"),
        (
            serde_json::json!({ "field": 0 }),
            "Attribute number must be 1 or greater",
        ),
        (
            serde_json::json!({ "field": "first" }),
            "Attribute number is not a whole number: first",
        ),
        (
            serde_json::json!({ "field": 1, "width": 0 }),
            "Display width must be 1 or greater",
        ),
        (
            serde_json::json!({ "field": 1, "width": "wide" }),
            "Display width is not a whole number: wide",
        ),
        (
            serde_json::json!({ "field": 1, "justification": "centre" }),
            "Justification must be L or R",
        ),
    ];
    for (attributes, expected) in cases {
        let resp = set_dict(&db_arc, &client_info, "NAME", attributes.clone());
        assert_eq!(resp.status, "ERROR", "{} was accepted", attributes);
        assert_eq!(resp.code, Some(ErrorCode::InvalidData));
        // The code says what kind of failure it is; the message still has to
        // say which attribute, since that is all the caller has to go on.
        assert_eq!(resp.message.unwrap(), expected);
    }

    // A refused entry is not a stored one.
    assert!(list_dict(&db_arc, &client_info).results.unwrap().is_empty());

    // The attributes themselves are required, and so is a name to file them under.
    let resp = handle_request(
        Request {
            command: "SET.DICT".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("STOCK".to_string()),
            key: Some("NAME".to_string()),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.code, Some(ErrorCode::MissingField));
    assert_eq!(resp.message.unwrap(), "Dictionary attributes not specified");

    let resp = set_dict(&db_arc, &client_info, "   ", serde_json::json!({ "field": 1 }));
    assert_eq!(resp.code, Some(ErrorCode::MissingField));
    assert_eq!(resp.message.unwrap(), "Key not specified");
}

#[test]
fn test_list_dict_reads_the_dictionary_positions_rather_than_the_files_own_names() {
    let (_dir, db_arc, client_info) = dictionary_test_db("list_dict");

    set_dict(
        &db_arc,
        &client_info,
        "PRICE",
        serde_json::json!({ "field": 2, "justification": "R", "conversion": "MD2" }),
    );
    set_dict(
        &db_arc,
        &client_info,
        "NAME",
        serde_json::json!({ "field": 1, "heading": "Item", "width": 20 }),
    );

    let resp = list_dict(&db_arc, &client_info);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.count, Some(2));
    // Ordered by attribute number, which is how a dictionary is read - not by
    // the hash order the entries happen to sit in.
    assert_eq!(resp.keys.unwrap(), vec!["NAME", "PRICE"]);

    let results = resp.results.unwrap();
    assert_eq!(results[0].0, "NAME");
    assert_eq!(results[0].1["heading"], "Item");
    assert_eq!(results[0].1["width"], 20);
    assert_eq!(results[1].1["conversion"], "MD2");
    assert_eq!(results[1].1["definition"], "2^PRICE^R^10^^^^MD2");

    // READ with is_dict serializes against the *data* file's dictionary, so the
    // same entry comes back labelled with the file's own field names. That is
    // what LIST.DICT exists to avoid, and the difference is asserted rather
    // than described.
    let read = handle_request(
        Request {
            command: "READ".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("STOCK".to_string()),
            key: Some("NAME".to_string()),
            is_dict: Some(true),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(read.status, "OK");
    let record = read.record.unwrap();
    assert_eq!(
        record["name"], "1",
        "attribute 1 of the entry read as the file's NAME field"
    );
    assert!(record.get("heading").is_none());
}

#[test]
fn test_a_dictionary_entry_is_removed_by_delete_with_is_dict() {
    let (_dir, db_arc, client_info) = dictionary_test_db("delete_dict");

    set_dict(&db_arc, &client_info, "NAME", serde_json::json!({ "field": 1 }));
    set_dict(&db_arc, &client_info, "PRICE", serde_json::json!({ "field": 2 }));

    let resp = handle_request(
        Request {
            command: "DELETE".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("STOCK".to_string()),
            key: Some("PRICE".to_string()),
            is_dict: Some(true),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(list_dict(&db_arc, &client_info).keys.unwrap(), vec!["NAME"]);
}

#[test]
fn test_dictionary_commands_need_an_account_a_file_and_permission() {
    let (_dir, db_arc, client_info) = dictionary_test_db("dict_guards");

    let resp = handle_request(
        Request {
            command: "LIST.DICT".to_string(),
            account: Some("DICT_TEST".to_string()),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.code, Some(ErrorCode::MissingField));

    let resp = handle_request(
        Request {
            command: "LIST.DICT".to_string(),
            account: Some("DICT_TEST".to_string()),
            file: Some("NO_SUCH_FILE".to_string()),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.code, Some(ErrorCode::FileNotFound));

    // A client may only reach the accounts it was authorized for, dictionary or not.
    let resp = handle_request(
        Request {
            command: "LIST.DICT".to_string(),
            account: Some("SYSTEM".to_string()),
            file: Some("$CLIENTS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.code, Some(ErrorCode::AccessDenied));
}

#[test]
fn test_an_account_created_over_the_protocol_gets_a_dir_file() {
    // The dashboard creates an account and then files in it, and never logs in
    // anywhere. Nothing in that path used to make a DIR file, so the account's
    // own listing did not exist until somebody opened the CLI and answered a
    // prompt - and until then the per-file durability flags had nowhere to live.
    let dir = TempDir::new("protocol_dir_file");
    let base_dir = dir.path();
    let db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "CREATE.ACCOUNT".to_string(),
            target_account: Some("NEW_ACC".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert!(
        Path::new(base_dir).join("NEW_ACC").join("DIR").exists(),
        "a new account has no DIR file"
    );

    let listed = |db_arc: &Arc<RwLock<Database>>| {
        handle_request(
            Request {
                command: "LIST.FILES".to_string(),
                account: Some("NEW_ACC".to_string()),
                ..Default::default()
            },
            db_arc,
            &admin,
        )
        .keys
        .unwrap()
    };
    assert_eq!(listed(&db_arc), vec!["DIR"]);

    for file in ["LEDGER", "STOCK"] {
        let resp = handle_request(
            Request {
                command: "CREATE.FILE".to_string(),
                account: Some("NEW_ACC".to_string()),
                file: Some(file.to_string()),
                ..Default::default()
            },
            &db_arc,
            &admin,
        );
        assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    }

    // Both files are in the account's own listing, not just on the filesystem.
    assert_eq!(listed(&db_arc), vec!["DIR", "LEDGER", "STOCK"]);
    let dir_entries = {
        let db = crate::server::handler::write_lock(&db_arc);
        let table_handle = db.get_table_mut_for_account("NEW_ACC", "DIR").unwrap();
        let table = table_handle.write();
        let mut keys: Vec<String> = table.records.keys().cloned().collect();
        drop(table);
        keys.sort();
        keys
    };
    assert_eq!(dir_entries, vec!["LEDGER", "STOCK"]);
}

#[test]
fn test_a_file_created_in_an_account_that_lost_its_dir_brings_it_back() {
    // Accounts made before DIR came with them, and any account whose listing
    // was dropped, must not stay unlisted for the rest of their lives.
    let dir = TempDir::new("protocol_dir_recovery");
    let base_dir = dir.path();
    let db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.create_account("OLD_ACC", None).unwrap();
    db.delete_table_for_account("OLD_ACC", "DIR").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("OLD_ACC".to_string()),
            file: Some("LEDGER".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);

    let resp = handle_request(
        Request {
            command: "LIST.FILES".to_string(),
            account: Some("OLD_ACC".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.keys.unwrap(), vec!["DIR", "LEDGER"]);
}

#[test]
fn test_create_test_account_populates_the_demo_fixture_over_the_protocol() {
    // The CLI restricts this to the SYSTEM account. A headless server is not
    // logged into one, so the wire equivalent is an admin certificate - and the
    // command has to work without any account context at all.
    let dir = TempDir::new("protocol_demo_account");
    let base_dir = dir.path();
    let db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(
        Request {
            command: "CREATE.TEST.ACCOUNT".to_string(),
            target_account: Some("DEMO".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let created = resp.record.unwrap();
    assert_eq!(created["account"], "DEMO");
    assert_eq!(created["files"], serde_json::json!(["DIR", "PRODUCTS", "USERS"]));

    // Populated, not just created: a record read back carries the dictionary's
    // names, its multivalues and the MD2 conversion the fixture exists to show.
    let read = |file: &str, key: &str| {
        handle_request(
            Request {
                command: "READ".to_string(),
                account: Some("DEMO".to_string()),
                file: Some(file.to_string()),
                key: Some(key.to_string()),
                ..Default::default()
            },
            &db_arc,
            &admin,
        )
        .record
        .unwrap()
    };
    let user = read("USERS", "2");
    assert_eq!(user["name"], "Jane Smith");
    assert_eq!(user["roles"], serde_json::json!(["DEV", ["TEST", "LAB"]]));
    assert_eq!(read("PRODUCTS", "P1")["price"], "1200.00");

    // The account left no login behind on a server that had none.
    assert_eq!(crate::server::handler::read_lock(&db_arc).current_account(), "");

    // Making it twice is refused rather than half-rebuilt over the first.
    let resp = handle_request(
        Request {
            command: "CREATE.TEST.ACCOUNT".to_string(),
            target_account: Some("DEMO".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::AccountExists));
}

#[test]
fn test_the_demo_account_is_admin_only_and_needs_a_name() {
    let dir = TempDir::new("protocol_demo_guards");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("PLAIN", None).unwrap();
    db.set_current_account("");
    let db_arc = Arc::new(RwLock::new(db));

    let ordinary = ClientInfo {
        name: "reporting".to_string(),
        thumbprint: "reporting_tp".to_string(),
        allowed_accounts: vec!["PLAIN".to_string()],
        is_admin: false,
    };
    let resp = handle_request(
        Request {
            command: "CREATE.TEST.ACCOUNT".to_string(),
            target_account: Some("DEMO".to_string()),
            ..Default::default()
        },
        &db_arc,
        &ordinary,
    );
    assert_eq!(resp.code, Some(ErrorCode::AdminRequired));

    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };
    let resp = handle_request(
        Request {
            command: "CREATE.TEST.ACCOUNT".to_string(),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.code, Some(ErrorCode::MissingField));
}

/// Each hot path takes a fixed, small number of file locks per request.
///
/// This pins the shape of a regression nothing else here would catch. An extra
/// `get_table_mut` in a command handler is free under one database-wide lock -
/// it runs inside a lock already held - but with a lock per file it is another
/// acquisition of the very lock every connection writing that file is queueing
/// for. It cost about 20% of the throughput of eight writers on one file, and
/// no threshold in the performance suite could have caught it: run-to-run
/// variance there is several times wider than the regression, and the
/// distinct-versus-shared ratio recorded beside it would have *improved*,
/// because the arm that got slower is its denominator.
///
/// The count, unlike the throughput it governs, is exact. These are upper
/// bounds: taking fewer locks passes, and taking more is a failure to argue
/// with rather than a number to nudge.
///
/// Debug builds only - the counter compiles out of a release build.
#[cfg(debug_assertions)]
#[test]
fn the_hot_paths_lock_a_file_a_fixed_number_of_times() {
    use crate::db::engine::table_locks_taken;

    let dir = TempDir::new("hot_path_locks");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("HOT").unwrap();
    // Nothing may flush mid-request: a flush legitimately takes the file again,
    // and this is measuring the request, not the flush.
    db.flush_interval = std::time::Duration::from_secs(3_600);
    db.flush_max_pending = 1_000_000;

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "hot_client".to_string(),
        thumbprint: "hot_tp".to_string(),
        allowed_accounts: vec!["HOT".to_string()],
        is_admin: false,
    };

    let request = |command: &str, key: &str| Request {
        command: command.to_string(),
        account: Some("HOT".to_string()),
        file: Some("USERS".to_string()),
        key: Some(key.to_string()),
        ..Default::default()
    };
    let write = |key: &str| Request {
        data: Some(serde_json::Value::String("Alice^alice@example.com".to_string())),
        ..request("WRITE", key)
    };

    // Steady state is what the counts describe: the first write to a file also
    // loads it and reads the account's durability flags out of DIR.
    for i in 0..3 {
        assert_eq!(
            handle_request(write(&format!("warm{i}")), &db_arc, &client_info).status,
            "OK"
        );
    }

    let structured = Request {
        structured_data: Some(serde_json::json!({ "name": "Bob" })),
        ..request("WRITE", "structured")
    };
    let query = Request {
        key: None,
        ..request("QUERY", "")
    };

    // (what it does, the request, how many times it may lock the file, why)
    let budgets: Vec<(&str, Request, u64, &str)> = vec![
        (
            "WRITE",
            write("written"),
            2,
            "the freshness check, then the write itself",
        ),
        (
            "WRITE with structured data",
            structured,
            3,
            "the same two, plus reading the dictionary to deserialize the record",
        ),
        (
            "READ",
            request("READ", "warm0"),
            2,
            "the freshness check, then serving the record",
        ),
        ("QUERY", query, 2, "the freshness check, then the scan"),
        (
            "DELETE",
            request("DELETE", "warm1"),
            2,
            "the freshness check, then the removal",
        ),
    ];

    for (what, req, budget, why) in budgets {
        let before = table_locks_taken();
        let response = handle_request(req, &db_arc, &client_info);
        let taken = table_locks_taken() - before;
        assert_eq!(response.status, "OK", "{what} did not succeed: {:?}", response.message);
        assert!(
            taken <= budget,
            "{what} locked the file {taken} times, over its budget of {budget} ({why}). \
             Every acquisition beyond the budget is one more turn in the queue for a file \
             other connections are working on. Resolve the file once and reuse the handle.",
        );
    }
}

#[test]
fn test_index_commands_create_list_rebuild_and_drop_over_the_wire() {
    let dir = TempDir::new("server_indexes");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("IDX_TEST").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec![],
        is_admin: true,
    };
    let index_request = |command: &str, field: Option<&str>| Request {
        command: command.to_string(),
        account: Some("IDX_TEST".to_string()),
        file: Some("USERS".to_string()),
        field: field.map(str::to_string),
        ..Default::default()
    };

    let resp = handle_request(index_request("CREATE.INDEX", Some("EMAIL")), &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let created = resp.record.unwrap();
    assert_eq!(created["field"], serde_json::json!("EMAIL"));
    assert_eq!(created["attribute"], serde_json::json!(2));
    // The demo fixture holds two users with distinct addresses.
    assert_eq!(created["values"], serde_json::json!(2));
    assert_eq!(created["postings"], serde_json::json!(2));
    assert_eq!(created["stale"], serde_json::json!(false));

    let resp = handle_request(index_request("LIST.INDEXES", None), &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.keys.unwrap(), vec!["EMAIL".to_string()]);
    assert_eq!(resp.count, Some(1));
    let listed = resp.results.unwrap();
    assert_eq!(listed[0].1, created);

    // FILE.STATS carries the same objects, so a dashboard needs one request.
    let resp = handle_request(
        Request {
            command: "FILE.STATS".to_string(),
            account: Some("IDX_TEST".to_string()),
            file: Some("USERS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK");
    let indexes = resp.record.unwrap()["indexes"].clone();
    assert_eq!(indexes.as_array().unwrap().len(), 1);
    assert_eq!(indexes[0]["field"], serde_json::json!("EMAIL"));

    // A query resolves through it, and returns what it always returned.
    let resp = handle_request(
        Request {
            command: "QUERY".to_string(),
            account: Some("IDX_TEST".to_string()),
            file: Some("USERS".to_string()),
            query_string: Some("WITH EMAIL = \"jane@example.com\"".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK");
    let keys: Vec<String> = resp.results.unwrap().into_iter().map(|(key, _)| key).collect();
    assert_eq!(keys, vec!["2".to_string()]);

    let resp = handle_request(index_request("REBUILD.INDEX", Some("EMAIL")), &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.record.unwrap()["postings"], serde_json::json!(2));

    let resp = handle_request(index_request("DELETE.INDEX", Some("EMAIL")), &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let resp = handle_request(index_request("LIST.INDEXES", None), &db_arc, &admin);
    assert_eq!(resp.count, Some(0));
}

#[test]
fn test_index_commands_report_what_they_were_not_given() {
    let dir = TempDir::new("server_index_errors");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("IDX_ERR").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec![],
        is_admin: true,
    };
    let refusal = |req: Request| {
        let resp = handle_request(req, &db_arc, &admin);
        assert_eq!(resp.status, "ERROR");
        // The message is still there for a person; what is asserted is the code,
        // so rewording a refusal does not fail this.
        assert!(resp.message.is_some());
        resp.code
    };

    assert_eq!(
        refusal(Request {
            command: "CREATE.INDEX".to_string(),
            account: Some("IDX_ERR".to_string()),
            ..Default::default()
        }),
        Some(ErrorCode::MissingField)
    );
    assert_eq!(
        refusal(Request {
            command: "CREATE.INDEX".to_string(),
            account: Some("IDX_ERR".to_string()),
            file: Some("USERS".to_string()),
            ..Default::default()
        }),
        Some(ErrorCode::MissingField)
    );
    assert_eq!(
        refusal(Request {
            command: "CREATE.INDEX".to_string(),
            account: Some("IDX_ERR".to_string()),
            file: Some("USERS".to_string()),
            field: Some("NOSUCH".to_string()),
            ..Default::default()
        }),
        Some(ErrorCode::InvalidField)
    );
    assert_eq!(
        refusal(Request {
            command: "LIST.INDEXES".to_string(),
            account: Some("IDX_ERR".to_string()),
            file: Some("NOPE".to_string()),
            ..Default::default()
        }),
        Some(ErrorCode::FileNotFound)
    );
}

#[test]
fn test_changing_an_index_needs_admin_but_reading_them_does_not() {
    // Creating an index is a storage decision about a file, gated like creating
    // the file. Listing them is not, any more than listing the files is.
    let dir = TempDir::new("server_index_perm");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("IDX_PERM").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let user = ClientInfo {
        name: "user".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec!["IDX_PERM".to_string()],
        is_admin: false,
    };

    for command in ["CREATE.INDEX", "REBUILD.INDEX", "DELETE.INDEX"] {
        let resp = handle_request(
            Request {
                command: command.to_string(),
                account: Some("IDX_PERM".to_string()),
                file: Some("USERS".to_string()),
                field: Some("EMAIL".to_string()),
                ..Default::default()
            },
            &db_arc,
            &user,
        );
        assert_eq!(resp.status, "ERROR", "{} must be refused", command);
        assert_eq!(resp.code, Some(ErrorCode::AdminRequired));
    }

    let resp = handle_request(
        Request {
            command: "LIST.INDEXES".to_string(),
            account: Some("IDX_PERM".to_string()),
            file: Some("USERS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &user,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.count, Some(0));
}

/// Every refusal carries a code, whatever it was that went wrong.
///
/// The point of the codes is that a client never has to read the message, and
/// that only holds if there is no refusal without one. A command that grows a
/// new failure path and returns a bare message fails here rather than being
/// found by whoever writes the client.
#[test]
fn every_refusal_carries_a_code_and_a_message() {
    let dir = TempDir::new("server_every_refusal");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("CODES").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };
    let ordinary = ClientInfo {
        name: "plain".to_string(),
        thumbprint: "tp2".to_string(),
        allowed_accounts: vec!["CODES".to_string()],
        is_admin: false,
    };

    // One refusal per command, spanning the missing field, the missing thing,
    // the thing already there and the request that is simply not allowed.
    let named = |command: &str| Request {
        command: command.to_string(),
        ..Default::default()
    };
    let in_account = |command: &str, file: Option<&str>| Request {
        command: command.to_string(),
        account: Some("CODES".to_string()),
        file: file.map(str::to_string),
        ..Default::default()
    };
    let cases: Vec<(Request, &ClientInfo, ErrorCode)> = vec![
        (named("NO.SUCH.COMMAND"), &admin, ErrorCode::UnknownCommand),
        (in_account("READ", Some("USERS")), &ordinary, ErrorCode::MissingField),
        (
            Request {
                key: Some("NOPE".to_string()),
                ..in_account("READ", Some("USERS"))
            },
            &ordinary,
            ErrorCode::RecordNotFound,
        ),
        (
            Request {
                key: Some("1".to_string()),
                ..in_account("READ", Some("NO_SUCH_FILE"))
            },
            &ordinary,
            ErrorCode::FileNotFound,
        ),
        (in_account("WRITE", Some("USERS")), &ordinary, ErrorCode::MissingField),
        (
            Request {
                key: Some("1".to_string()),
                data: Some(serde_json::json!(7)),
                ..in_account("WRITE", Some("USERS"))
            },
            &ordinary,
            ErrorCode::InvalidData,
        ),
        (in_account("DELETE", Some("USERS")), &ordinary, ErrorCode::MissingField),
        (in_account("QUERY", None), &ordinary, ErrorCode::MissingField),
        (in_account("SELECT", None), &ordinary, ErrorCode::MissingField),
        (named("GET.NEXT"), &ordinary, ErrorCode::SelectListNotFound),
        (named("CREATE.ACCOUNT"), &ordinary, ErrorCode::AdminRequired),
        (named("CREATE.ACCOUNT"), &admin, ErrorCode::MissingField),
        (
            Request {
                target_account: Some("CODES".to_string()),
                ..named("CREATE.ACCOUNT")
            },
            &admin,
            ErrorCode::AccountExists,
        ),
        (
            Request {
                target_account: Some("NO_SUCH_ACCOUNT".to_string()),
                ..named("DELETE.ACCOUNT")
            },
            &admin,
            ErrorCode::AccountNotFound,
        ),
        (
            Request {
                target_account: Some("SYSTEM".to_string()),
                ..named("DELETE.ACCOUNT")
            },
            &admin,
            ErrorCode::AccountProtected,
        ),
        (named("CREATE.TEST.ACCOUNT"), &admin, ErrorCode::MissingField),
        (named("CREATE.FILE"), &admin, ErrorCode::AccountNotSpecified),
        (in_account("CREATE.FILE", Some("USERS")), &admin, ErrorCode::FileExists),
        (in_account("SET.FILE", Some("USERS")), &admin, ErrorCode::MissingField),
        (
            Request {
                durable: Some(true),
                ..in_account("SET.FILE", Some("DIR"))
            },
            &admin,
            ErrorCode::InvalidRequest,
        ),
        (
            in_account("DELETE.FILE", Some("NO_SUCH_FILE")),
            &admin,
            ErrorCode::FileNotFound,
        ),
        (named("AUTHORIZE.CONN"), &admin, ErrorCode::MissingField),
        (
            Request {
                name: Some("nobody".to_string()),
                ..named("DEAUTHORIZE.CONN")
            },
            &admin,
            ErrorCode::ClientNotFound,
        ),
        (named("ADD.CLIENT.ACCOUNT"), &admin, ErrorCode::MissingField),
        (named("REMOVE.CLIENT.ACCOUNT"), &admin, ErrorCode::MissingField),
        (named("GENERATE.CERT"), &admin, ErrorCode::MissingField),
        (
            Request {
                name: Some("someone".to_string()),
                ..named("GENERATE.CERT")
            },
            &admin,
            // No server configuration is active in a unit test, so certificate
            // generation cannot be attempted at all.
            ErrorCode::Unavailable,
        ),
        (named("LIST.CONNS"), &ordinary, ErrorCode::AdminRequired),
        (named("LIST.FILES"), &admin, ErrorCode::AccountNotSpecified),
        (in_account("FILE.STATS", None), &ordinary, ErrorCode::MissingField),
        (
            in_account("FILE.STATS", Some("NO_SUCH_FILE")),
            &ordinary,
            ErrorCode::FileNotFound,
        ),
        (in_account("LIST.DICT", None), &ordinary, ErrorCode::MissingField),
        (
            in_account("SET.DICT", Some("USERS")),
            &ordinary,
            ErrorCode::MissingField,
        ),
        (
            in_account("CREATE.INDEX", Some("USERS")),
            &admin,
            ErrorCode::MissingField,
        ),
        (
            Request {
                field: Some("NOSUCH".to_string()),
                ..in_account("REBUILD.INDEX", Some("USERS"))
            },
            &admin,
            ErrorCode::IndexNotFound,
        ),
        (
            Request {
                field: Some("NAME".to_string()),
                ..in_account("DELETE.INDEX", Some("USERS"))
            },
            &admin,
            ErrorCode::IndexNotFound,
        ),
        // `LIST.INDEXES` with no file is the account-wide listing rather than a
        // refusal, so the missing-field case moves to the commands that still
        // need a file and a field.
        (in_account("INDEX.STATS", None), &ordinary, ErrorCode::MissingField),
        (
            Request {
                field: Some("NOSUCH".to_string()),
                ..in_account("INDEX.STATS", Some("USERS"))
            },
            &ordinary,
            ErrorCode::IndexNotFound,
        ),
        (
            Request {
                field: Some("NOSUCH".to_string()),
                ..in_account("SET.INDEX.EXCLUDE", Some("USERS"))
            },
            &admin,
            ErrorCode::IndexNotFound,
        ),
        (
            Request {
                field: Some("NAME".to_string()),
                ..in_account("SET.INDEX.EXCLUDE", Some("USERS"))
            },
            &ordinary,
            ErrorCode::AdminRequired,
        ),
        (named("SERVER.STATS"), &ordinary, ErrorCode::AdminRequired),
        (
            Request {
                account: Some("SYSTEM".to_string()),
                file: Some("$CLIENTS".to_string()),
                key: Some("x".to_string()),
                ..named("READ")
            },
            &ordinary,
            ErrorCode::AccessDenied,
        ),
    ];

    for (request, client, expected) in cases {
        let command = request.command.clone();
        let response = handle_request(request, &db_arc, client);
        assert_eq!(response.status, "ERROR", "{command} was not refused");
        assert_eq!(response.code, Some(expected), "{command} was refused as {:?}", response);
        assert!(
            response.message.is_some_and(|message| !message.is_empty()),
            "{command} was refused with no message to read"
        );
    }
}

/// A query string that is not a query is refused rather than ignored.
///
/// It used to parse to "no criteria", which is what an absent clause parses to,
/// so a mistyped `WITH` came back as every record in the file with
/// `status: "OK"` - a wrong answer, and the kind a caller cannot even see is
/// wrong. Selecting everything is still what an *absent* clause does.
#[test]
fn a_query_string_that_is_not_a_query_is_refused_rather_than_read_as_no_query() {
    let dir = TempDir::new("server_bad_query");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("BAD_QUERY").unwrap();
    db.set_current_account("");

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec!["BAD_QUERY".to_string()],
        is_admin: false,
    };
    let ask = |command: &str, query: Option<&str>| {
        handle_request(
            Request {
                command: command.to_string(),
                account: Some("BAD_QUERY".to_string()),
                file: Some("USERS".to_string()),
                query_string: query.map(str::to_string),
                ..Default::default()
            },
            &db_arc,
            &client_info,
        )
    };

    for command in ["QUERY", "SELECT"] {
        // A criterion that is not three tokens is not a criterion.
        let resp = ask(command, Some("WITH NAME"));
        assert_eq!(resp.status, "ERROR", "{command} accepted a half-written clause");
        assert_eq!(resp.code, Some(ErrorCode::InvalidQuery));
        assert!(resp.message.unwrap().contains("NAME"), "the refusal must quote it back");
    }

    // Nothing to parse is not a failure to parse: it still means everything.
    let resp = ask("QUERY", None);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.results.unwrap().len(), 2);

    // Nor is a clause that carries only an ordering.
    let resp = ask("QUERY", Some("BY NAME"));
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.results.unwrap().len(), 2);

    // And a well-formed one still selects.
    let resp = ask("QUERY", Some("WITH NAME = [John]"));
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.results.unwrap().len(), 1);
}
