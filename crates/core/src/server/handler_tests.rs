use crate::db::{ClientInfo, Database};
use crate::server::handler::handle_request;
use crate::server::models::Request;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn test_handle_request_read_write() {
    let base_dir = "test_server_handler_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
    db.create_test_account("SERVER_TEST").unwrap();

    let db_arc = Arc::new(Mutex::new(db));
    let client_info = ClientInfo {
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

    fs::remove_dir_all(base_dir).unwrap();
}

#[test]
fn test_handle_request_query_select() {
    let base_dir = "test_server_query_dir";
    if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir).unwrap(); }
    let mut db = Database::new(base_dir, None).unwrap();
    db.create_test_account("QUERY_TEST").unwrap();
    db.logto("QUERY_TEST").unwrap();

    let db_arc = Arc::new(Mutex::new(db));
    let client_info = ClientInfo {
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

    fs::remove_dir_all(base_dir).unwrap();
}
