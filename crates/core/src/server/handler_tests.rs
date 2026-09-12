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

    // A request that names no attribute at all must not be read as a demotion.
    let resp = set(None, &admin);
    assert_eq!(resp.status, "ERROR");
    assert_eq!(resp.code, Some(ErrorCode::MissingField));
    assert_eq!(
        resp.message.unwrap(),
        "Nothing to set: name durable, autokey, queue, visibility_timeout or max_deliveries"
    );

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
fn test_query_explodes_an_association_group_in_lockstep() {
    let (_dir, db_arc, client_info) = exploded_test_db("explode_group");

    // The demo `PRODUCTS` file carries a group: SUPPLIERS controls, SUP.CODES
    // pairs value for value and SUP.CONTACTS pairs at the second tier.
    let query = |explode: Vec<&str>| Request {
        command: "QUERY".to_string(),
        account: Some("EXP_TEST".to_string()),
        file: Some("PRODUCTS".to_string()),
        query_string: Some("WITH DESC = Laptop".to_string()),
        explode: Some(explode.into_iter().map(str::to_string).collect()),
        ..Default::default()
    };

    let resp = handle_request(query(vec!["SUPPLIERS"]), &db_arc, &client_info);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    // Three suppliers; the second has two contacts, so it becomes two rows, and
    // the third has neither a code nor a contact but is still a row of its own.
    assert_eq!(
        resp.positions.unwrap(),
        vec![
            Some(ValuePosition::sub_value(0, 0)),
            Some(ValuePosition::sub_value(1, 0)),
            Some(ValuePosition::sub_value(1, 1)),
            Some(ValuePosition::value(2)),
        ]
    );

    // Naming a dependent, or naming several members at once, asks the same
    // question: it is the group that explodes, not the field that was named.
    for spelling in [vec!["SUP.CONTACTS"], vec!["SUPPLIERS", "SUP.CODES", "SUP.CONTACTS"]] {
        let resp = handle_request(query(spelling.clone()), &db_arc, &client_info);
        assert_eq!(resp.status, "OK", "{:?}: {:?}", spelling, resp.message);
        assert_eq!(resp.positions.unwrap().len(), 4, "{:?}", spelling);
    }

    // Two fields with no association between them still have no defined
    // pairing, and are refused with both names.
    let resp = handle_request(query(vec!["SUPPLIERS", "DESC"]), &db_arc, &client_info);
    assert_eq!(resp.code, Some(ErrorCode::InvalidQuery));
    let message = resp.message.unwrap();
    assert!(message.contains("SUPPLIERS") && message.contains("DESC"), "{message}");
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

    // An association is recorded on the dependent, naming its controller. A
    // group that names no tier pairs value for value, and the stored entry says
    // so rather than leaving attribute 6 blank for a reader to guess at.
    let resp = set_dict(
        &db_arc,
        &client_info,
        "PRICE.DATE",
        serde_json::json!({ "field": 3, "association": "PRICE" }),
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stored = resp.record.unwrap();
    assert_eq!(stored["association"], "PRICE");
    assert_eq!(stored["associationDepth"], "V");
    assert_eq!(stored["definition"], "3^PRICE.DATE^L^10^PRICE^V");

    let resp = set_dict(
        &db_arc,
        &client_info,
        "PRICE.NOTE",
        serde_json::json!({ "field": 4, "association": "PRICE", "associationDepth": "s" }),
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    assert_eq!(resp.record.unwrap()["definition"], "4^PRICE.NOTE^L^10^PRICE^S");
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
        (
            serde_json::json!({ "field": 1, "association": "NAME" }),
            "A dictionary entry cannot be associated with itself",
        ),
        (
            serde_json::json!({ "field": 1, "association": "PRICE", "associationDepth": "deep" }),
            "Association depth must be V (value) or S (sub-value)",
        ),
        (
            serde_json::json!({ "field": 1, "associationDepth": "S" }),
            "Association depth given without a controlling field",
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
    assert_eq!(
        created["files"],
        serde_json::json!(["ATTACHMENTS", "DIR", "EVENTS", "JOBS", "PRODUCTS", "USERS"])
    );

    // The fixture reaches the ordering primitive as well as the record ones, so
    // a queue is one command away from any interface.
    let stats = handle_request(
        Request {
            command: "FILE.STATS".to_string(),
            account: Some("DEMO".to_string()),
            file: Some("JOBS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    )
    .record
    .unwrap();
    assert_eq!(stats["queue"]["depth"], serde_json::json!(3));
    assert_eq!(stats["queue"]["in_flight"], serde_json::json!(0));
    assert_eq!(
        stats["queue"]["visibility_timeout_seconds"],
        serde_json::json!(90),
        "the fixture's policy is not the default, so a reader can see it is read"
    );
    assert_eq!(stats["queue"]["max_deliveries"], serde_json::json!(3));

    // And the other thing a key can be. The fixture's EVENTS records were
    // appended with no key at all, so the keys in it are minted ones and a
    // listing of them is in the order they were written.
    let events = handle_request(
        Request {
            command: "QUERY".to_string(),
            account: Some("DEMO".to_string()),
            file: Some("EVENTS".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    let mut keys: Vec<String> = events.results.unwrap().into_iter().map(|(key, _)| key).collect();
    keys.sort();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .all(|key| key.len() == 20 && key.bytes().all(|b| b.is_ascii_digit())),
        "a minted key is twenty digits: {:?}",
        keys
    );

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

/// Directory files over the wire: the round trip, the shapes the reply uses,
/// and every command that must refuse rather than answer emptily.
#[test]
fn a_directory_file_round_trips_arbitrary_bytes_over_the_protocol() {
    let dir = TempDir::new("handler_directory");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.set_current_account("");
    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "tp".to_string(),
        allowed_accounts: vec![],
        is_admin: true,
    };
    let request = |command: &str| Request {
        command: command.to_string(),
        account: Some("DIRS".to_string()),
        ..Default::default()
    };

    assert_eq!(
        handle_request(
            Request {
                target_account: Some("DIRS".to_string()),
                ..request("CREATE.ACCOUNT")
            },
            &db_arc,
            &admin
        )
        .status,
        "OK"
    );
    let created = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            directory: Some(true),
            ..request("CREATE.FILE")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(created.status, "OK", "{:?}", created.message);
    let settled = created.record.unwrap();
    assert_eq!(settled["directory"], serde_json::json!(true));
    assert!(
        settled["path"].as_str().unwrap().ends_with("SCANS/records"),
        "the reply says where the records actually are: {}",
        settled["path"]
    );

    // The marks and an embedded NUL: exactly what an ordinary record cannot
    // hold, travelling in the envelope step 1 already defined.
    let hostile = [0xFEu8, b'a', 0xFD, 0xFC, 0x00, 0xFF];
    let encoded = crate::db::base64::encode(&hostile);
    let written = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("scan.bin".to_string()),
            data: Some(serde_json::json!({ "$base64": encoded })),
            ..request("WRITE")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(written.status, "OK", "{:?}", written.message);

    let read = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("scan.bin".to_string()),
            ..request("READ")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(read.status, "OK");
    // The record is the value, not an object of field names: a directory file
    // has no field names to key one by.
    assert_eq!(read.record.unwrap(), serde_json::json!({ "$base64": encoded }));

    // Text goes as a plain string in both directions, so the common case needs
    // no envelope at all.
    handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("note.txt".to_string()),
            data: Some(serde_json::Value::String("plain".to_string())),
            ..request("WRITE")
        },
        &db_arc,
        &admin,
    );
    let read = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("note.txt".to_string()),
            ..request("READ")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(read.record.unwrap(), serde_json::json!("plain"));

    // Listing gives sizes and never content.
    let queried = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            ..request("QUERY")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(
        queried.results.unwrap(),
        vec![
            ("note.txt".to_string(), serde_json::json!({ "size": 5 })),
            ("scan.bin".to_string(), serde_json::json!({ "size": 6 })),
        ]
    );

    // SELECT then GET.NEXT pages the same rows.
    let selected = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            ..request("SELECT")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(selected.count, Some(2));
    let next = handle_request(
        Request {
            batch_size: Some(10),
            ..request("GET.NEXT")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(next.status, "OK");
    assert_eq!(next.results.unwrap().len(), 2);

    // Every refusal, each with the code a client branches on.
    let refusals: Vec<(&str, Request)> = vec![
        (
            "a criterion, which would read a field the file has not got",
            Request {
                file: Some("SCANS".to_string()),
                query_string: Some("WITH NAME = \"x\"".to_string()),
                ..request("QUERY")
            },
        ),
        (
            "the dictionary section, which governs nothing here",
            Request {
                file: Some("SCANS".to_string()),
                key: Some("scan.bin".to_string()),
                is_dict: Some(true),
                ..request("READ")
            },
        ),
        (
            "changing the file's type after it was created",
            Request {
                file: Some("SCANS".to_string()),
                directory: Some(false),
                ..request("SET.FILE")
            },
        ),
        (
            "making a directory file a queue",
            Request {
                file: Some("SPOOL".to_string()),
                directory: Some(true),
                queue: Some(true),
                ..request("CREATE.FILE")
            },
        ),
        (
            "a path on a file that is not a directory file",
            Request {
                file: Some("ORDINARY".to_string()),
                path: Some("/tmp".to_string()),
                directory: Some(false),
                ..request("CREATE.FILE")
            },
        ),
    ];
    for (what, req) in refusals {
        let response = handle_request(req, &db_arc, &admin);
        assert_eq!(
            response.code,
            Some(ErrorCode::InvalidRequest),
            "{what} should be refused with INVALID_REQUEST, got {:?}",
            response
        );
    }

    // Fields, sent to a file that has none.
    let structured = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("scan.bin".to_string()),
            structured_data: Some(serde_json::json!({ "name": "Alice" })),
            ..request("WRITE")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(structured.code, Some(ErrorCode::InvalidData));

    // A key that is not a usable file name, refused rather than repaired.
    let traversal = handle_request(
        Request {
            file: Some("SCANS".to_string()),
            key: Some("../escape".to_string()),
            data: Some(serde_json::Value::String("no".to_string())),
            ..request("WRITE")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(traversal.code, Some(ErrorCode::InvalidRequest));

    // And the record really is gone after a DELETE.
    assert_eq!(
        handle_request(
            Request {
                file: Some("SCANS".to_string()),
                key: Some("note.txt".to_string()),
                ..request("DELETE")
            },
            &db_arc,
            &admin
        )
        .status,
        "OK"
    );
    assert_eq!(
        handle_request(
            Request {
                file: Some("SCANS".to_string()),
                key: Some("note.txt".to_string()),
                ..request("READ")
            },
            &db_arc,
            &admin
        )
        .code,
        Some(ErrorCode::RecordNotFound)
    );
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

    // The fixture's directory file, warmed the same way: the first request
    // against one reads its DIR entry, and the entry is cached from then on.
    let attachment = |command: &str, key: &str| Request {
        command: command.to_string(),
        account: Some("HOT".to_string()),
        file: Some("ATTACHMENTS".to_string()),
        key: Some(key.to_string()),
        ..Default::default()
    };
    let store = |key: &str| Request {
        data: Some(serde_json::Value::String("x".repeat(4096))),
        ..attachment("WRITE", key)
    };
    assert_eq!(handle_request(store("warm"), &db_arc, &client_info).status, "OK");

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
        // The property a blob store has to have. Reading a forty megabyte
        // record must not block every writer to that file for the length of
        // the read - and it cannot, because a directory file has no table and
        // so no lock to hold. Zero, not "few": there is nothing here to lock.
        (
            "WRITE to a directory file",
            store("written"),
            0,
            "a directory file has no table; the write goes straight to the host file",
        ),
        (
            "READ from a directory file",
            attachment("READ", "warm"),
            0,
            "the record is read from the host file with no table involved",
        ),
        (
            "QUERY over a directory file",
            Request {
                key: None,
                ..attachment("QUERY", "")
            },
            0,
            "the keys come from the host directory, not from a table",
        ),
        (
            "DELETE from a directory file",
            attachment("DELETE", "written"),
            0,
            "the removal is an unlink; nothing is locked for it",
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

/// A database with one account, an admin client and a worker client, for the
/// queue tests below.
fn queue_fixture(label: &str) -> (TempDir, Arc<RwLock<Database>>, ClientInfo, ClientInfo) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_account("QUEUE_TEST", Some(dir.path())).unwrap();
    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: vec!["QUEUE_TEST".to_string()],
        is_admin: true,
    };
    let worker = ClientInfo {
        name: "worker-1".to_string(),
        thumbprint: "worker_tp".to_string(),
        allowed_accounts: vec!["QUEUE_TEST".to_string()],
        is_admin: false,
    };
    (dir, db_arc, admin, worker)
}

fn queue_request(command: &str, file: &str) -> Request {
    Request {
        command: command.to_string(),
        account: Some("QUEUE_TEST".to_string()),
        file: Some(file.to_string()),
        ..Default::default()
    }
}

#[test]
fn test_create_file_makes_a_queue_and_the_listing_says_so() {
    let (_dir, db_arc, admin, worker) = queue_fixture("handler_queue_create");

    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    create.visibility_timeout = Some(300);
    create.max_deliveries = Some(3);
    let resp = handle_request(create, &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let record = resp.record.unwrap();
    assert_eq!(record["queue"], serde_json::json!(true));
    assert_eq!(record["visibility_timeout_seconds"], serde_json::json!(300));
    assert_eq!(record["max_deliveries"], serde_json::json!(3));
    assert_eq!(
        record["durable"],
        serde_json::json!(true),
        "a queue is durable by default"
    );

    handle_request(queue_request("CREATE.FILE", "PLAIN"), &db_arc, &admin);

    // The flag is in the listing, so a client finds the queues without reading DIR.
    let resp = handle_request(
        Request {
            command: "LIST.FILES".to_string(),
            account: Some("QUEUE_TEST".to_string()),
            ..Default::default()
        },
        &db_arc,
        &worker,
    );
    let results = resp.results.unwrap();
    let flag = |name: &str| results.iter().find(|(key, _)| key == name).unwrap().1["queue"].clone();
    assert_eq!(flag("JOBS"), serde_json::json!(true));
    assert_eq!(flag("PLAIN"), serde_json::json!(false));

    // Creating one is a storage decision, like any other CREATE.FILE.
    let mut denied = queue_request("CREATE.FILE", "SNEAKY");
    denied.queue = Some(true);
    assert_eq!(
        handle_request(denied, &db_arc, &worker).code,
        Some(ErrorCode::AdminRequired)
    );
}

#[test]
fn test_a_queue_hands_each_record_to_one_consumer_over_the_protocol() {
    let (_dir, db_arc, admin, worker) = queue_fixture("handler_queue_claim");
    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    assert_eq!(handle_request(create, &db_arc, &admin).status, "OK");

    let other = ClientInfo {
        name: "worker-2".to_string(),
        ..worker.clone()
    };

    // An empty queue is not an error.
    let resp = handle_request(queue_request("DEQUEUE", "JOBS"), &db_arc, &worker);
    assert_eq!(resp.status, "EMPTY");
    assert_eq!(resp.count, Some(0));
    assert!(resp.record.is_none());

    for order in ["first", "second"] {
        let mut enqueue = queue_request("ENQUEUE", "JOBS");
        enqueue.structured_data = Some(serde_json::json!({ "1": order }));
        enqueue.data = Some(serde_json::Value::String(order.to_string()));
        let resp = handle_request(enqueue, &db_arc, &worker);
        assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
        let claim = resp.claim.unwrap();
        assert_eq!(claim["queue"], serde_json::json!("JOBS"));
        assert_eq!(claim["deliveries"], serde_json::json!(0));
        assert!(claim["key"].as_str().unwrap().len() == 20, "a minted sequence key");
    }

    // In order, and to one consumer each.
    let first = handle_request(queue_request("DEQUEUE", "JOBS"), &db_arc, &worker);
    let second = handle_request(queue_request("DEQUEUE", "JOBS"), &db_arc, &other);
    let key_of =
        |resp: &crate::server::models::Response| resp.claim.as_ref().unwrap()["key"].as_str().unwrap().to_string();
    assert_ne!(key_of(&first), key_of(&second));
    assert!(key_of(&first) < key_of(&second), "arrival order");
    assert_eq!(first.claim.as_ref().unwrap()["owner"], serde_json::json!("worker-1"));
    assert_eq!(second.claim.as_ref().unwrap()["owner"], serde_json::json!("worker-2"));
    assert_eq!(first.claim.as_ref().unwrap()["deliveries"], serde_json::json!(1));
    assert!(first.record.is_some(), "the payload comes back beside the claim");
    assert_eq!(
        handle_request(queue_request("DEQUEUE", "JOBS"), &db_arc, &worker).status,
        "EMPTY",
        "both records are claimed"
    );

    // Only the holder may settle it.
    let mut steal = queue_request("ACK", "JOBS");
    steal.key = Some(key_of(&first));
    let resp = handle_request(steal, &db_arc, &other);
    assert_eq!(resp.code, Some(ErrorCode::InvalidRequest));
    assert!(resp.message.unwrap().contains("claimed by worker-1"));

    let mut ack = queue_request("ACK", "JOBS");
    ack.key = Some(key_of(&first));
    assert_eq!(handle_request(ack, &db_arc, &worker).status, "OK");

    // NACK puts the other one straight back, and PEEK sees it without claiming.
    let mut nack = queue_request("NACK", "JOBS");
    nack.key = Some(key_of(&second));
    assert_eq!(handle_request(nack, &db_arc, &other).status, "OK");
    let peeked = handle_request(queue_request("PEEK", "JOBS"), &db_arc, &worker);
    assert_eq!(peeked.status, "OK");
    assert_eq!(key_of(&peeked), key_of(&second));
    assert!(
        peeked.claim.as_ref().unwrap().get("owner").is_none(),
        "a returned record is held by nobody"
    );
}

#[test]
fn test_queue_commands_refuse_an_ordinary_file_and_a_bad_timeout() {
    let (_dir, db_arc, admin, worker) = queue_fixture("handler_queue_refusals");
    assert_eq!(
        handle_request(queue_request("CREATE.FILE", "PLAIN"), &db_arc, &admin).status,
        "OK"
    );

    let mut enqueue = queue_request("ENQUEUE", "PLAIN");
    enqueue.data = Some(serde_json::Value::String("x".to_string()));
    let resp = handle_request(enqueue, &db_arc, &worker);
    assert_eq!(resp.code, Some(ErrorCode::InvalidRequest));
    assert!(resp.message.unwrap().contains("is not a queue file"));

    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    create.visibility_timeout = Some(0);
    assert_eq!(
        handle_request(create, &db_arc, &admin).code,
        Some(ErrorCode::InvalidData),
        "a zero timeout would hand every record to two consumers at once"
    );

    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    assert_eq!(handle_request(create, &db_arc, &admin).status, "OK");
    let mut dequeue = queue_request("DEQUEUE", "JOBS");
    dequeue.visibility_timeout = Some(999_999);
    assert_eq!(
        handle_request(dequeue, &db_arc, &worker).code,
        Some(ErrorCode::InvalidData)
    );

    // ACK and NACK need the key of a claim.
    assert_eq!(
        handle_request(queue_request("ACK", "JOBS"), &db_arc, &worker).code,
        Some(ErrorCode::MissingField)
    );
    let mut peek = queue_request("PEEK", "JOBS");
    peek.key = Some("nosuchkey".to_string());
    assert_eq!(
        handle_request(peek, &db_arc, &worker).code,
        Some(ErrorCode::RecordNotFound)
    );
}

#[test]
fn test_set_file_changes_only_what_it_names() {
    let (_dir, db_arc, admin, _worker) = queue_fixture("handler_queue_set");
    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    create.visibility_timeout = Some(120);
    create.max_deliveries = Some(2);
    assert_eq!(handle_request(create, &db_arc, &admin).status, "OK");

    // A request about durability leaves the queue - and its policy - alone.
    let mut set = queue_request("SET.FILE", "JOBS");
    set.durable = Some(false);
    let record = handle_request(set, &db_arc, &admin).record.unwrap();
    assert_eq!(record["durable"], serde_json::json!(false));
    assert_eq!(record["queue"], serde_json::json!(true));
    assert_eq!(record["visibility_timeout_seconds"], serde_json::json!(120));
    assert_eq!(record["max_deliveries"], serde_json::json!(2));

    // And one about the policy leaves durability alone.
    let mut set = queue_request("SET.FILE", "JOBS");
    set.visibility_timeout = Some(30);
    let record = handle_request(set, &db_arc, &admin).record.unwrap();
    assert_eq!(record["durable"], serde_json::json!(false));
    assert_eq!(record["visibility_timeout_seconds"], serde_json::json!(30));
    assert_eq!(record["max_deliveries"], serde_json::json!(2));

    // Turning the queue off keeps the records; turning it back on re-attaches
    // the order to them.
    let mut enqueue = queue_request("ENQUEUE", "JOBS");
    enqueue.data = Some(serde_json::Value::String("payload".to_string()));
    let key = handle_request(enqueue, &db_arc, &admin).claim.unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    let mut set = queue_request("SET.FILE", "JOBS");
    set.queue = Some(false);
    assert_eq!(
        handle_request(set, &db_arc, &admin).record.unwrap()["queue"],
        serde_json::json!(false)
    );
    assert_eq!(
        handle_request(queue_request("PEEK", "JOBS"), &db_arc, &admin).code,
        Some(ErrorCode::InvalidRequest)
    );
    let mut read = queue_request("READ", "JOBS");
    read.key = Some(key.clone());
    assert_eq!(
        handle_request(read, &db_arc, &admin).status,
        "OK",
        "the record is still there"
    );

    let mut set = queue_request("SET.FILE", "JOBS");
    set.queue = Some(true);
    assert_eq!(handle_request(set, &db_arc, &admin).status, "OK");
    let peeked = handle_request(queue_request("PEEK", "JOBS"), &db_arc, &admin);
    assert_eq!(peeked.claim.unwrap()["key"], serde_json::json!(key));
}

#[test]
fn test_file_stats_reports_the_queue_over_the_protocol() {
    let (_dir, db_arc, admin, worker) = queue_fixture("handler_queue_stats");
    let mut create = queue_request("CREATE.FILE", "JOBS");
    create.queue = Some(true);
    create.max_deliveries = Some(1);
    assert_eq!(handle_request(create, &db_arc, &admin).status, "OK");

    for n in 0..3 {
        let mut enqueue = queue_request("ENQUEUE", "JOBS");
        enqueue.data = Some(serde_json::Value::String(format!("job {}", n)));
        assert_eq!(handle_request(enqueue, &db_arc, &worker).status, "OK");
    }
    let claim = handle_request(queue_request("DEQUEUE", "JOBS"), &db_arc, &worker)
        .claim
        .unwrap();

    let stats = handle_request(queue_request("FILE.STATS", "JOBS"), &db_arc, &worker)
        .record
        .unwrap();
    let queue = &stats["queue"];
    assert_eq!(queue["depth"], serde_json::json!(2));
    assert_eq!(queue["in_flight"], serde_json::json!(1));
    assert_eq!(queue["dead_letters"], serde_json::json!(0));
    assert_eq!(queue["max_deliveries"], serde_json::json!(1));
    assert!(queue["oldest_unacknowledged_seconds"].is_number());

    // One delivery allowed, so giving it back dead-letters it.
    let mut nack = queue_request("NACK", "JOBS");
    nack.key = Some(claim["key"].as_str().unwrap().to_string());
    assert_eq!(handle_request(nack, &db_arc, &worker).status, "OK");
    let stats = handle_request(queue_request("FILE.STATS", "JOBS"), &db_arc, &worker)
        .record
        .unwrap();
    assert_eq!(stats["queue"]["dead_letters"], serde_json::json!(1));

    // An ordinary file reports no queue rather than a queue of zero.
    assert_eq!(
        handle_request(queue_request("CREATE.FILE", "PLAIN"), &db_arc, &admin).status,
        "OK"
    );
    let stats = handle_request(queue_request("FILE.STATS", "PLAIN"), &db_arc, &worker)
        .record
        .unwrap();
    assert!(stats["queue"].is_null());
}

// ------------------------------------------------------------- GET.NEXT and its account

/// Two accounts holding a same-named file with different records, and a client
/// that may reach both. That is the shape in which paging a list against the
/// wrong account answers rather than refuses.
fn two_accounts_one_file_name() -> (TempDir, Arc<RwLock<Database>>, ClientInfo) {
    let dir = TempDir::new("getnext_account");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();

    for account in ["ALPHA", "BETA"] {
        db.create_account(account, None).unwrap();
        db.create_table_for_account(account, "USERS").unwrap();
    }

    let db_arc = Arc::new(RwLock::new(db));
    let client = ClientInfo {
        name: "both".to_string(),
        thumbprint: "tp_both".to_string(),
        allowed_accounts: vec!["ALPHA".to_string(), "BETA".to_string()],
        is_admin: false,
    };

    for (account, key, name) in [("ALPHA", "1", "alpha one"), ("BETA", "9", "beta nine")] {
        let write = Request {
            command: "WRITE".to_string(),
            account: Some(account.to_string()),
            file: Some("USERS".to_string()),
            key: Some(key.to_string()),
            data: Some(serde_json::Value::String(name.to_string())),
            ..Default::default()
        };
        assert_eq!(handle_request(write, &db_arc, &client).status, "OK");
    }

    (dir, db_arc, client)
}

fn select_in(db: &Arc<RwLock<Database>>, client: &ClientInfo, account: &str, list: &str) -> i64 {
    let select = Request {
        command: "SELECT".to_string(),
        account: Some(account.to_string()),
        file: Some("USERS".to_string()),
        list_name: Some(list.to_string()),
        ..Default::default()
    };
    let response = handle_request(select, db, client);
    assert_eq!(response.status, "OK");
    response.count.unwrap() as i64
}

#[test]
fn get_next_pages_the_account_the_select_ran_in_without_being_told_it_again() {
    let (_dir, db_arc, client) = two_accounts_one_file_name();
    assert_eq!(select_in(&db_arc, &client, "ALPHA", "MYLIST"), 1);

    // No account on the request, and the client has two allowed accounts, so
    // there is no single one to fall back to. The list knows.
    let response = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            list_name: Some("MYLIST".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &client,
    );

    assert_eq!(response.status, "OK");
    let results = response.results.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "1");
}

#[test]
fn get_next_refuses_a_request_that_names_a_different_account_than_the_list() {
    let (_dir, db_arc, client) = two_accounts_one_file_name();
    select_in(&db_arc, &client, "ALPHA", "MYLIST");

    // BETA has a USERS file too. Reading ALPHA's keys against it used to answer
    // from whatever happened to match over there - silently, and short, because
    // a key that is not present is skipped rather than reported.
    let response = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            account: Some("BETA".to_string()),
            list_name: Some("MYLIST".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &client,
    );

    assert_eq!(response.status, "ERROR");
    assert_eq!(response.code, Some(ErrorCode::InvalidRequest));
    assert!(response.message.unwrap().contains("ALPHA"));
}

#[test]
fn get_next_accepts_the_account_the_list_was_selected_in() {
    let (_dir, db_arc, client) = two_accounts_one_file_name();
    select_in(&db_arc, &client, "ALPHA", "MYLIST");

    // Naming the right one is not an error: a client that sends the account it
    // selected with - which every client written against the old wording does -
    // keeps working.
    let response = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            account: Some("ALPHA".to_string()),
            list_name: Some("MYLIST".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &client,
    );

    assert_eq!(response.status, "OK");
    assert_eq!(response.results.unwrap().len(), 1);
}

#[test]
fn get_next_denies_a_client_that_may_not_reach_the_account_the_list_belongs_to() {
    let (_dir, db_arc, owner) = two_accounts_one_file_name();
    select_in(&db_arc, &owner, "ALPHA", "SHARED.NAME");

    // The lists are held by name across every connection, so taking the account
    // off the request means this is the only thing standing between a stranger
    // and somebody else's selection.
    let outsider = ClientInfo {
        name: "outsider".to_string(),
        thumbprint: "tp_out".to_string(),
        allowed_accounts: vec!["BETA".to_string()],
        is_admin: false,
    };

    let response = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            list_name: Some("SHARED.NAME".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &outsider,
    );

    assert_eq!(response.status, "ERROR");
    assert_eq!(response.code, Some(ErrorCode::AccessDenied));
}

#[test]
fn get_next_walks_an_admins_list_without_an_account_on_any_request() {
    let (_dir, db_arc, _client) = two_accounts_one_file_name();
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "tp_admin".to_string(),
        allowed_accounts: vec![],
        is_admin: true,
    };

    select_in(&db_arc, &admin, "ALPHA", "ADMINLIST");

    let batch = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            list_name: Some("ADMINLIST".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(batch.status, "OK");
    assert_eq!(batch.results.unwrap().len(), 1);

    // And the cursor still ends the list, rather than the account change losing it.
    let end = handle_request(
        Request {
            command: "GET.NEXT".to_string(),
            list_name: Some("ADMINLIST".to_string()),
            batch_size: Some(10),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(end.status, "EOF");
}

#[test]
fn selecting_into_a_list_again_resets_its_cursor() {
    let (_dir, db_arc, client) = two_accounts_one_file_name();
    select_in(&db_arc, &client, "ALPHA", "MYLIST");

    let page = |db: &Arc<RwLock<Database>>| {
        handle_request(
            Request {
                command: "GET.NEXT".to_string(),
                list_name: Some("MYLIST".to_string()),
                batch_size: Some(10),
                ..Default::default()
            },
            db,
            &client,
        )
    };

    assert_eq!(page(&db_arc).status, "OK");
    assert_eq!(page(&db_arc).status, "EOF");

    // Re-using a list name replaces the previous list and resets its cursor -
    // which is the cursor living with the list rather than in a map beside it.
    select_in(&db_arc, &client, "BETA", "MYLIST");
    let response = page(&db_arc);
    assert_eq!(response.status, "OK");

    // And it is BETA's list now, not ALPHA's.
    assert_eq!(response.results.unwrap()[0].0, "9");
}

/// The protocol side of a transaction: one client, one account, the fixture's
/// two ordinary files and its queue.
fn transact_fixture() -> (TempDir, Arc<RwLock<Database>>, ClientInfo) {
    let dir = TempDir::new("handler_transact");
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("TXN_TEST").unwrap();
    let client = ClientInfo {
        name: "txn_client".to_string(),
        thumbprint: "txn_tp".to_string(),
        allowed_accounts: vec!["TXN_TEST".to_string()],
        is_admin: false,
    };
    (dir, Arc::new(RwLock::new(db)), client)
}

fn change(op: &str, file: &str, key: &str, data: Option<serde_json::Value>) -> crate::server::models::ChangeSpec {
    crate::server::models::ChangeSpec {
        op: Some(op.to_string()),
        file: Some(file.to_string()),
        key: Some(key.to_string()),
        data,
        ..Default::default()
    }
}

fn transact(
    db: &Arc<RwLock<Database>>,
    client: &ClientInfo,
    changes: Vec<crate::server::models::ChangeSpec>,
) -> crate::server::models::Response {
    handle_request(
        Request {
            command: "TRANSACT".to_string(),
            account: Some("TXN_TEST".to_string()),
            changes: Some(changes),
            ..Default::default()
        },
        db,
        client,
    )
}

fn read_key(db: &Arc<RwLock<Database>>, client: &ClientInfo, file: &str, key: &str) -> crate::server::models::Response {
    handle_request(
        Request {
            command: "READ".to_string(),
            account: Some("TXN_TEST".to_string()),
            file: Some(file.to_string()),
            key: Some(key.to_string()),
            ..Default::default()
        },
        db,
        client,
    )
}

#[test]
fn a_transaction_writes_across_two_files_at_once() {
    let (_dir, db_arc, client) = transact_fixture();

    let response = transact(
        &db_arc,
        &client,
        vec![
            change(
                "WRITE",
                "USERS",
                "99",
                Some(serde_json::json!({"name": "Alice", "email": "alice@example.com"})),
            ),
            change(
                "write",
                "PRODUCTS",
                "P-99",
                Some(serde_json::Value::String("Widget".to_string())),
            ),
        ],
    );

    assert_eq!(response.status, "OK");
    assert_eq!(response.count, Some(2));
    assert_eq!(read_key(&db_arc, &client, "USERS", "99").status, "OK");
    assert_eq!(read_key(&db_arc, &client, "PRODUCTS", "P-99").status, "OK");
}

#[test]
fn a_transaction_naming_a_file_that_is_not_there_writes_nothing_at_all() {
    let (_dir, db_arc, client) = transact_fixture();

    let response = transact(
        &db_arc,
        &client,
        vec![
            change(
                "WRITE",
                "USERS",
                "99",
                Some(serde_json::Value::String("Alice".to_string())),
            ),
            change(
                "WRITE",
                "NOWHERE",
                "1",
                Some(serde_json::Value::String("X".to_string())),
            ),
        ],
    );

    assert_eq!(response.code, Some(ErrorCode::FileNotFound));
    // The whole point: the change to the file that does exist was not applied
    // on the way to finding out about the one that does not.
    assert_eq!(
        read_key(&db_arc, &client, "USERS", "99").code,
        Some(ErrorCode::RecordNotFound)
    );
}

#[test]
fn a_transaction_over_a_queue_file_is_refused_with_the_scope_code() {
    let (_dir, db_arc, client) = transact_fixture();

    let response = transact(
        &db_arc,
        &client,
        vec![
            change(
                "WRITE",
                "USERS",
                "99",
                Some(serde_json::Value::String("Alice".to_string())),
            ),
            change(
                "WRITE",
                "JOBS",
                "anything",
                Some(serde_json::Value::String("X".to_string())),
            ),
        ],
    );

    // A distinct code, so a client can tell this from "the database will not do
    // that at all" and fall back to writing one record at a time if it can.
    assert_eq!(response.code, Some(ErrorCode::TransactionScope));
    assert_eq!(
        read_key(&db_arc, &client, "USERS", "99").code,
        Some(ErrorCode::RecordNotFound)
    );
}

#[test]
fn a_transaction_says_which_change_it_could_not_read() {
    let (_dir, db_arc, client) = transact_fixture();

    let unknown_op = transact(
        &db_arc,
        &client,
        vec![
            change(
                "WRITE",
                "USERS",
                "99",
                Some(serde_json::Value::String("Alice".to_string())),
            ),
            change(
                "UPSERT",
                "USERS",
                "98",
                Some(serde_json::Value::String("Bob".to_string())),
            ),
        ],
    );
    assert_eq!(unknown_op.code, Some(ErrorCode::InvalidData));
    // A set is refused whole, so the message has to say which of its changes is
    // at fault - "not an operation" alone would leave a caller counting.
    assert!(
        unknown_op
            .message
            .as_deref()
            .unwrap_or_default()
            .starts_with("Change 2:"),
        "the refusal did not name the change: {:?}",
        unknown_op.message
    );

    let no_key = transact(
        &db_arc,
        &client,
        vec![crate::server::models::ChangeSpec {
            op: Some("DELETE".to_string()),
            file: Some("USERS".to_string()),
            ..Default::default()
        }],
    );
    assert_eq!(no_key.code, Some(ErrorCode::MissingField));

    let nothing = transact(&db_arc, &client, Vec::new());
    assert_eq!(nothing.code, Some(ErrorCode::MissingField));
}

#[test]
fn one_key_changed_twice_in_a_transaction_is_refused() {
    let (_dir, db_arc, client) = transact_fixture();

    let response = transact(
        &db_arc,
        &client,
        vec![
            change(
                "WRITE",
                "USERS",
                "99",
                Some(serde_json::Value::String("Alice".to_string())),
            ),
            change("DELETE", "USERS", "99", None),
        ],
    );

    assert_eq!(response.code, Some(ErrorCode::InvalidRequest));
    assert_eq!(
        read_key(&db_arc, &client, "USERS", "99").code,
        Some(ErrorCode::RecordNotFound)
    );
}

// --------------------------------------------- conditional writes and autokeys

/// Reads one record of the fixture account, whatever the read reports.
fn read_cond(
    db: &Arc<RwLock<Database>>,
    client: &ClientInfo,
    file: &str,
    key: &str,
) -> crate::server::models::Response {
    handle_request(
        Request {
            command: "READ".to_string(),
            account: Some("COND".to_string()),
            file: Some(file.to_string()),
            key: Some(key.to_string()),
            ..Default::default()
        },
        db,
        client,
    )
}

/// An account with one ordinary file, and an admin client that can create more.
fn conditional_fixture(label: &str) -> (TempDir, Arc<RwLock<Database>>, ClientInfo) {
    let dir = TempDir::new(label);
    let db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("COND").unwrap();
    db.set_current_account("");
    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: vec!["COND".to_string()],
        is_admin: true,
    };
    (dir, db_arc, admin)
}

fn write_request(file: &str, key: Option<&str>, body: &str) -> Request {
    Request {
        command: "WRITE".to_string(),
        account: Some("COND".to_string()),
        file: Some(file.to_string()),
        key: key.map(str::to_string),
        data: Some(serde_json::Value::String(body.to_string())),
        ..Default::default()
    }
}

#[test]
fn if_absent_over_the_protocol_creates_once_and_then_reports_the_collision() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_if_absent");

    let created = handle_request(
        Request {
            if_absent: Some(true),
            ..write_request("USERS", Some("NEW"), "Alice")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(created.status, "OK", "unexpected: {:?}", created.message);
    // A supplied key is not echoed back; a version always is, so the caller can
    // make its next write conditional without reading first.
    assert_eq!(created.key, None);
    assert!(created.version.is_some());

    let collided = handle_request(
        Request {
            if_absent: Some(true),
            ..write_request("USERS", Some("NEW"), "Mallory")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(collided.status, "ERROR");
    assert_eq!(
        collided.code,
        Some(ErrorCode::PreconditionFailed),
        "a collision must be distinguishable from a failure: {:?}",
        collided.message
    );
    // And it must not be *described* as a failure either: nothing was saved, so
    // nothing failed to save.
    assert!(
        !collided.message.unwrap().starts_with("Save error"),
        "a refused condition is not a save that went wrong"
    );
    assert_eq!(
        read_cond(&db_arc, &admin, "USERS", "NEW").record.unwrap()["name"],
        "Alice",
        "the refused write must not have landed"
    );
}

#[test]
fn read_reports_a_version_that_if_match_accepts_and_a_stale_one_it_does_not() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_if_match");
    handle_request(write_request("USERS", Some("RMW"), "Alice"), &db_arc, &admin);

    let stale = read_cond(&db_arc, &admin, "USERS", "RMW").version.unwrap();

    // Somebody else writes in between, so the version moves on.
    handle_request(write_request("USERS", Some("RMW"), "Bob"), &db_arc, &admin);
    let current = read_cond(&db_arc, &admin, "USERS", "RMW").version.unwrap();
    assert_ne!(stale, current, "a changed record must have a changed version");

    let refused = handle_request(
        Request {
            if_match: Some(stale),
            ..write_request("USERS", Some("RMW"), "Carol")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(refused.code, Some(ErrorCode::PreconditionFailed));
    assert_eq!(
        read_cond(&db_arc, &admin, "USERS", "RMW").record.unwrap()["name"],
        "Bob"
    );

    let applied = handle_request(
        Request {
            if_match: Some(current),
            ..write_request("USERS", Some("RMW"), "Carol")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(applied.status, "OK", "unexpected: {:?}", applied.message);
    assert_eq!(
        read_cond(&db_arc, &admin, "USERS", "RMW").record.unwrap()["name"],
        "Carol"
    );

    // And a DELETE takes the same condition, for the same reason.
    let delete = |version: Option<String>| {
        handle_request(
            Request {
                command: "DELETE".to_string(),
                account: Some("COND".to_string()),
                file: Some("USERS".to_string()),
                key: Some("RMW".to_string()),
                if_match: version,
                ..Default::default()
            },
            &db_arc,
            &admin,
        )
    };
    assert_eq!(
        delete(Some("deadbeefdeadbeef".to_string())).code,
        Some(ErrorCode::PreconditionFailed)
    );
    let current = read_cond(&db_arc, &admin, "USERS", "RMW").version.unwrap();
    assert_eq!(delete(Some(current)).status, "OK");
    assert_eq!(
        read_cond(&db_arc, &admin, "USERS", "RMW").code,
        Some(ErrorCode::RecordNotFound)
    );
}

#[test]
fn two_conditions_at_once_are_refused_rather_than_one_of_them_being_picked() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_two_conditions");

    let both = handle_request(
        Request {
            if_absent: Some(true),
            if_match: Some("deadbeefdeadbeef".to_string()),
            ..write_request("USERS", Some("X"), "Alice")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(both.code, Some(ErrorCode::InvalidRequest));

    // `if_absent: false` is no condition, not "must exist": it writes.
    let neither = handle_request(
        Request {
            if_absent: Some(false),
            ..write_request("USERS", Some("X"), "Alice")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(neither.status, "OK", "unexpected: {:?}", neither.message);

    // An empty `if_match` is a client bug rather than a condition that matches
    // nothing, and is told so.
    let empty = handle_request(
        Request {
            if_match: Some(String::new()),
            ..write_request("USERS", Some("X"), "Bob")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(empty.code, Some(ErrorCode::InvalidData));
}

#[test]
fn a_directory_file_refuses_a_condition_rather_than_promising_one() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_dir_condition");

    let refused = handle_request(
        Request {
            if_absent: Some(true),
            ..write_request("ATTACHMENTS", Some("NEW.txt"), "hello")
        },
        &db_arc,
        &admin,
    );
    assert_eq!(refused.code, Some(ErrorCode::InvalidRequest));
    assert!(
        refused.message.unwrap().contains("host files"),
        "the refusal has to say why a version off a host file means nothing"
    );
}

#[test]
fn create_file_autokey_mints_a_key_for_a_write_that_names_none() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_autokey");

    let created = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("COND".to_string()),
            file: Some("AUDIT".to_string()),
            autokey: Some(true),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(created.status, "OK", "unexpected: {:?}", created.message);
    assert_eq!(created.record.unwrap()["autokey"], serde_json::json!(true));

    let mut minted = Vec::new();
    for n in 0..3 {
        let appended = handle_request(write_request("AUDIT", None, &format!("EVENT-{}", n)), &db_arc, &admin);
        assert_eq!(appended.status, "OK", "unexpected: {:?}", appended.message);
        let key = appended.key.expect("a minted key has to come back, or it is lost");
        assert_eq!(key.len(), 20);
        minted.push(key);
    }
    let mut sorted = minted.clone();
    sorted.sort();
    assert_eq!(sorted, minted, "minted keys must come back in arrival order");

    // The record really is under the key that was reported.
    let read = read_cond(&db_arc, &admin, "AUDIT", &minted[1]);
    assert_eq!(read.status, "OK");

    // A keyless write to an ordinary file names the flag that would allow it.
    let refused = handle_request(write_request("USERS", None, "X"), &db_arc, &admin);
    assert_eq!(refused.code, Some(ErrorCode::InvalidRequest));
    assert!(refused.message.unwrap().contains("AUTOKEY"));

    // An empty key is a missing key, not a request to mint one.
    let empty = handle_request(write_request("AUDIT", Some(""), "X"), &db_arc, &admin);
    assert_eq!(empty.code, Some(ErrorCode::MissingField));
}

#[test]
fn a_file_cannot_be_both_a_queue_and_an_autokey_file() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_autokey_queue");

    let both = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("COND".to_string()),
            file: Some("CONFUSED".to_string()),
            queue: Some(true),
            autokey: Some(true),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(both.code, Some(ErrorCode::InvalidRequest));
    assert!(both.message.unwrap().contains("ENQUEUE"));

    let directory = handle_request(
        Request {
            command: "CREATE.FILE".to_string(),
            account: Some("COND".to_string()),
            file: Some("SCANS".to_string()),
            directory: Some(true),
            autokey: Some(true),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    assert_eq!(directory.code, Some(ErrorCode::InvalidRequest));
}

#[test]
fn set_file_turns_minting_on_and_off_and_the_listing_says_which() {
    let (_dir, db_arc, admin) = conditional_fixture("proto_set_autokey");
    let set = |autokey: Option<bool>, durable: Option<bool>| {
        handle_request(
            Request {
                command: "SET.FILE".to_string(),
                account: Some("COND".to_string()),
                file: Some("USERS".to_string()),
                autokey,
                durable,
                ..Default::default()
            },
            &db_arc,
            &admin,
        )
    };

    let on = set(Some(true), None);
    assert_eq!(on.status, "OK", "unexpected: {:?}", on.message);
    assert_eq!(on.record.unwrap()["autokey"], serde_json::json!(true));
    assert_eq!(
        handle_request(write_request("USERS", None, "Appended"), &db_arc, &admin).status,
        "OK"
    );

    // Only what is named changes: a request about durability leaves minting on.
    assert_eq!(
        set(None, Some(true)).record.unwrap()["autokey"],
        serde_json::json!(true),
        "a SET.FILE that did not mention autokey must not have turned it off"
    );

    let listed = handle_request(
        Request {
            command: "LIST.FILES".to_string(),
            account: Some("COND".to_string()),
            ..Default::default()
        },
        &db_arc,
        &admin,
    );
    let users = listed
        .results
        .unwrap()
        .into_iter()
        .find(|(name, _)| name == "USERS")
        .unwrap()
        .1;
    assert_eq!(users["autokey"], serde_json::json!(true));

    assert_eq!(
        set(Some(false), None).record.unwrap()["autokey"],
        serde_json::json!(false)
    );
    assert_eq!(
        handle_request(write_request("USERS", None, "X"), &db_arc, &admin).code,
        Some(ErrorCode::InvalidRequest)
    );
}
