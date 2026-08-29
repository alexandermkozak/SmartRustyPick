use crate::db::{ClientInfo, Database};
use crate::server::handler::handle_request;
use crate::server::models::Request;
use crate::test_support::{isolated_config, TempDir};
use std::path::Path;
use std::sync::{Arc, RwLock};

#[test]
fn test_handle_request_read_write() {
    let dir = TempDir::new("handler");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
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
    assert_eq!(record.as_object().unwrap().get("name").unwrap().as_str().unwrap(), "Alice");
    assert_eq!(record.as_object().unwrap().get("email").unwrap().as_str().unwrap(), "alice@example.com");

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
    assert!(resp_denied.message.unwrap().contains("Access denied"));
}

#[test]
fn test_create_and_delete_file_target_the_requested_account() {
    // A headless server is not logged into any account, so these commands must act on
    // the account named in the request rather than on `current_account`.
    let dir = TempDir::new("server_create_file");
    let base_dir = dir.path();
    let mut db = Database::new(base_dir, Some(isolated_config())).unwrap();
    db.create_account("FILE_TEST", None).unwrap();
    db.current_account = String::new();

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
    assert_eq!(resp.message.unwrap(), "Account not specified");

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
    db.current_account = String::new();
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

    let write = |file: &str| handle_request(
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
    );

    assert_eq!(write("SCRATCH").status, "OK");
    assert!(db_arc.read().unwrap().has_pending_writes(), "a normal file should be buffered");

    assert_eq!(write("LEDGER").status, "OK");
    assert!(!db_arc.read().unwrap().has_pending_writes(), "a durable file must flush at once");
    assert!(db_arc.write().unwrap().is_table_durable_for_account("DUR_TEST", "LEDGER"));
    assert!(!db_arc.write().unwrap().is_table_durable_for_account("DUR_TEST", "SCRATCH"));
}

#[test]
fn test_handle_request_query_select() {
    let dir = TempDir::new("server_query");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
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
    assert_eq!(results[0].1.as_object().unwrap().get("name").unwrap().as_str().unwrap(), "John Doe");

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
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("MGMT_TEST").unwrap();
    db.current_account = String::new();

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(Request { command: "LIST.ACCOUNTS".to_string(), ..Default::default() }, &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let accounts = resp.results.unwrap();
    let account = accounts.iter().find(|(name, _)| name == "MGMT_TEST").expect("the created account is listed");
    assert!(account.1["file_count"].as_u64().unwrap() > 0);
    assert!(account.1["directory"].as_str().unwrap().contains("MGMT_TEST"));

    let resp = handle_request(
        Request { command: "LIST.FILES".to_string(), account: Some("MGMT_TEST".to_string()), ..Default::default() },
        &db_arc,
        &admin,
    );
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let files = resp.keys.unwrap();
    assert!(files.contains(&"USERS".to_string()), "USERS missing from {:?}", files);
    assert_eq!(resp.count, Some(files.len()));

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
    assert!(stats.get("records").is_none(), "statistics must not carry record contents");

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
    assert!(resp.message.unwrap().contains("not found"));
}

#[test]
fn test_management_commands_respect_the_clients_permissions() {
    let dir = TempDir::new("server_management_perm");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.create_test_account("VISIBLE").unwrap();
    db.create_test_account("HIDDEN").unwrap();
    db.current_account = String::new();

    let db_arc = Arc::new(RwLock::new(db));
    let client_info = ClientInfo {
        name: "test_client".to_string(),
        thumbprint: "test_tp".to_string(),
        allowed_accounts: vec!["VISIBLE".to_string()],
        is_admin: false,
    };

    // An account the client cannot reach must not even be named to it.
    let resp = handle_request(Request { command: "LIST.ACCOUNTS".to_string(), ..Default::default() }, &db_arc, &client_info);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let names: Vec<String> = resp.results.unwrap().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, vec!["VISIBLE".to_string()]);

    let resp = handle_request(
        Request { command: "LIST.FILES".to_string(), account: Some("HIDDEN".to_string()), ..Default::default() },
        &db_arc,
        &client_info,
    );
    assert_eq!(resp.status, "ERROR");
    assert!(resp.message.unwrap().contains("Access denied"));

    // The management views of the server itself are administrative.
    for command in ["SERVER.STATS", "LIST.CONNS", "GENERATE.CERT"] {
        let resp = handle_request(
            Request { command: command.to_string(), name: Some("intruder".to_string()), ..Default::default() },
            &db_arc,
            &client_info,
        );
        assert_eq!(resp.status, "ERROR", "{} must be refused", command);
        assert_eq!(resp.message.unwrap(), "Admin privileges required");
    }
}

#[test]
fn test_list_conns_and_server_stats_describe_the_running_server() {
    let dir = TempDir::new("server_stats");
    let mut db = Database::new(dir.path(), Some(isolated_config())).unwrap();
    db.add_authorized_client("reporting-bot", "AB12CD", vec!["SALES".to_string()], false).unwrap();
    db.current_account = String::new();

    let db_arc = Arc::new(RwLock::new(db));
    let admin = ClientInfo {
        name: "test_admin".to_string(),
        thumbprint: "admin_tp".to_string(),
        allowed_accounts: Vec::new(),
        is_admin: true,
    };

    let resp = handle_request(Request { command: "LIST.CONNS".to_string(), ..Default::default() }, &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let clients = resp.results.unwrap();
    let (name, info) = clients.iter().find(|(name, _)| name == "reporting-bot").expect("the authorized client is listed");
    assert_eq!(name, "reporting-bot");
    // Thumbprints are stored lowercase, whatever case they were given in.
    assert_eq!(info["thumbprint"].as_str().unwrap(), "ab12cd");
    assert_eq!(info["accounts"][0].as_str().unwrap(), "SALES");
    assert_eq!(info["is_admin"].as_bool().unwrap(), false);

    let resp = handle_request(Request { command: "SERVER.STATS".to_string(), ..Default::default() }, &db_arc, &admin);
    assert_eq!(resp.status, "OK", "unexpected message: {:?}", resp.message);
    let stats = resp.record.unwrap();
    assert!(stats["active_connections"].is_array());
    assert!(stats["total_requests"].is_number());
    // The engine-side numbers are merged into the same object.
    assert_eq!(stats["authorized_clients"].as_u64().unwrap(), 1);
    assert!(stats["pending_writes"].is_number());
}
