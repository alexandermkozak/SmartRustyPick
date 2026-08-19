use crate::db::{Database, Record, Table};
use crate::server::models::{Request, Response};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// The database handle shared by every connection.
///
/// A read/write lock rather than a mutex: the commands that only look at the
/// data are the common case and have no reason to exclude each other.
pub type SharedDb = Arc<RwLock<Database>>;

/// Takes the shared lock, ignoring poisoning.
///
/// A panic in one handler leaves the database no less readable than it was, so
/// refusing every later request would turn a single failed command into a dead
/// server.
pub fn read_lock(db: &SharedDb) -> RwLockReadGuard<'_, Database> {
    db.read().unwrap_or_else(|e| e.into_inner())
}

/// Takes the exclusive lock, ignoring poisoning. See [`read_lock`].
pub fn write_lock(db: &SharedDb) -> RwLockWriteGuard<'_, Database> {
    db.write().unwrap_or_else(|e| e.into_inner())
}

fn error(message: impl Into<String>) -> Response {
    Response { status: "ERROR".to_string(), message: Some(message.into()), ..Default::default() }
}

/// The commands that never modify the database and can therefore run under the
/// shared lock, in parallel with each other.
fn is_read_only(command: &str) -> bool {
    matches!(command, "READ" | "QUERY")
}

pub fn handle_request(req: Request, db: &SharedDb, client_info: &crate::db::ClientInfo) -> Response {
    let command = req.command.to_uppercase();

    // Fast path: a read of an already loaded, still current table needs nothing
    // exclusive. Anything else - a table that has to be loaded or reloaded, a
    // denied request that wants to be logged - falls through to the slow path,
    // which is also where the response is produced, so behaviour is identical.
    if is_read_only(&command) {
        if let (Some(acc), Some(table_name)) = (allowed_account(&req, client_info), req.file.as_deref()) {
            let db = read_lock(db);
            if let Some(table) = db.table_ready_for_read(&acc, table_name) {
                return match command.as_str() {
                    "READ" => read_command(&db, table, &req),
                    _ => query_command(&db, table, &req),
                };
            }
        }
    }

    let mut db = write_lock(db);
    handle_request_locked(req, &mut db, client_info)
}

/// The account a request targets, or `None` when resolving it needs the slow
/// path (unspecified, or denied and therefore worth an error log entry).
fn allowed_account(req: &Request, client_info: &crate::db::ClientInfo) -> Option<String> {
    match req.account.as_deref() {
        Some(acc) => {
            if client_info.is_admin || client_info.allowed_accounts.iter().any(|a| a == acc) {
                Some(acc.to_string())
            } else {
                None
            }
        }
        None if client_info.allowed_accounts.len() == 1 => Some(client_info.allowed_accounts[0].clone()),
        None => None,
    }
}

/// Reads a single record from `table`, which the caller has already resolved.
fn read_command(db: &Database, table: &Table, req: &Request) -> Response {
    if req.file.is_none() {
        return error("File not specified");
    }
    let key = match req.key.as_deref() {
        Some(k) => k,
        None => return error("Key not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    let records = if is_dict { &table.dictionary } else { &table.records };
    match records.get(key) {
        Some(record) => Response {
            status: "OK".to_string(),
            record: Some(db.serialize_record_in(table, record)),
            ..Default::default()
        },
        None => error("Record not found"),
    }
}

/// Runs a QUERY against `table`, which the caller has already resolved.
fn query_command(db: &Database, table: &Table, req: &Request) -> Response {
    let table_name = match req.file.as_deref() {
        Some(t) => t,
        None => return error("File not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    let mut sort_specs = req.sort_specs.clone().unwrap_or_default();
    let query_node = if let Some(node) = req.query_node.clone() {
        Some(node)
    } else if let Some(q_str) = req.query_string.as_deref() {
        let parts: Vec<&str> = q_str.split_whitespace().collect();
        let (clause_parts, parsed_sorts) = Database::parse_sort_specs(&parts);
        if sort_specs.is_empty() { sort_specs = parsed_sorts; }
        db.parse_query_read_only(table_name, &clause_parts)
    } else {
        None
    };

    let results_processed: Vec<(String, serde_json::Value)> = if let Some(q) = query_node {
        let mut results = Database::query_in(table, is_dict, &q, None);
        Database::sort_results_in(table, &mut results, &sort_specs);
        results.into_iter()
            .map(|(k, r)| (k, db.serialize_record_in(table, &r)))
            .collect()
    } else {
        // Full scan: sort the keys only, then serialize each record by reference so the
        // whole table is never cloned into memory.
        let records = if is_dict { &table.dictionary } else { &table.records };
        let mut keys: Vec<String> = records.keys().cloned().collect();
        if sort_specs.is_empty() {
            // `sort_keys_in` already falls back to the ID, so only sort here.
            keys.sort();
        } else {
            keys = Database::sort_keys_in(table, is_dict, keys, &sort_specs);
        }

        keys.into_iter()
            .filter_map(|k| {
                let record = records.get(&k)?;
                Some((k, db.serialize_record_in(table, record)))
            })
            .collect()
    };

    Response {
        status: "OK".to_string(),
        results: Some(results_processed),
        ..Default::default()
    }
}

/// Handles a request against an exclusively borrowed database. Commands that
/// only read still go through here whenever the shared path could not serve
/// them, for instance because the table had to be loaded first.
pub fn handle_request_locked(req: Request, db: &mut Database, client_info: &crate::db::ClientInfo) -> Response {
    let target_account = if let Some(acc) = req.account.clone() {
        // Client specified an account
        if !client_info.is_admin && !client_info.allowed_accounts.contains(&acc) {
            let msg = format!("Access denied for account {}: Not in allowed list", acc);
            let _ = db.log_error("REMOTE", &msg);
            return Response { status: "ERROR".to_string(), message: Some(msg), ..Default::default() };
        }
        Some(acc)
    } else {
        // Client did not specify an account
        if client_info.allowed_accounts.len() == 1 {
            // Default to the only allowed account
            Some(client_info.allowed_accounts[0].clone())
        } else if client_info.is_admin {
            // Admin can access SYSTEM or other accounts, but must specify one if multiple are possible.
            None
        } else {
            return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
        }
    };

    let acc = match target_account {
        Some(ref a) => a.as_str(),
        None => "", // Some commands might not need an account, or will fail later
    };

    match req.command.to_uppercase().as_str() {
        "READ" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file.as_deref() {
                Some(t) => t.to_string(),
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };
            // Load the table, then serve the request exactly as the shared path would.
            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() };
            }
            let table = match db.get_table_read_only_for_account(acc, &table_name) {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {} not loaded", table_name)), ..Default::default() },
            };
            read_command(db, table, &req)
        }
        "WRITE" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };

            // Pre-load table to ensure dictionary is available for deserialization
            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() };
            }

            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), ..Default::default() },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let record = if let Some(structured) = req.structured_data {
                match db.deserialize_record_for_account(acc, &table_name, &structured) {
                    Some(r) => r,
                    None => return Response { status: "ERROR".to_string(), message: Some("Invalid structured data".to_string()), ..Default::default() },
                }
            } else if let Some(data_val) = req.data {
                match data_val {
                    serde_json::Value::String(s) => Record::from_display_string(&s),
                    serde_json::Value::Object(_) => {
                        match db.deserialize_record_for_account(acc, &table_name, &data_val) {
                            Some(r) => r,
                            None => return Response { status: "ERROR".to_string(), message: Some("Invalid structured data in data field".to_string()), ..Default::default() },
                        }
                    }
                    _ => return Response { status: "ERROR".to_string(), message: Some("Invalid data type in data field: expected string or object".to_string()), ..Default::default() },
                }
            } else {
                return Response { status: "ERROR".to_string(), message: Some("Data not specified".to_string()), ..Default::default() };
            };

            let table = match db.get_table_mut_for_account(acc, &table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
            };
            if is_dict {
                table.dictionary.insert(key, record);
                table.mark_dict_dirty();
            } else {
                table.insert_record(&key, record);
            }
            match db.note_write_for(acc, &table_name) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Save error: {}", e)), ..Default::default() },
            }
        }
        "DELETE" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), ..Default::default() },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = match db.get_table_mut_for_account(acc, &table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
            };
            if is_dict {
                table.dictionary.remove(&key);
                table.mark_dict_dirty();
            } else {
                table.remove_record(&key);
            }
            match db.note_write_for(acc, &table_name) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Save error: {}", e)), ..Default::default() },
            }
        }
        "QUERY" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file.as_deref() {
                Some(t) => t.to_string(),
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };
            // Load the table, then serve the request exactly as the shared path would.
            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() };
            }
            let table = match db.get_table_read_only_for_account(acc, &table_name) {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {} not loaded", table_name)), ..Default::default() },
            };
            query_command(db, table, &req)
        }
        "SELECT" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };
            let is_dict = req.is_dict.unwrap_or(false);
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());

            let mut sort_specs = req.sort_specs.unwrap_or_default();
            let query_node = if let Some(node) = req.query_node {
                Some(node)
            } else if let Some(q_str) = req.query_string.as_deref() {
                let parts: Vec<&str> = q_str.split_whitespace().collect();
                let (clause_parts, parsed_sorts) = Database::parse_sort_specs(&parts);
                if sort_specs.is_empty() { sort_specs = parsed_sorts; }
                db.parse_query(&table_name, &clause_parts)
            } else {
                None
            };

            let keys = if let Some(q) = query_node {
                let mut results = db.query_for_account(acc, &table_name, is_dict, &q, None);
                db.sort_results_for_account(acc, &table_name, &mut results, &sort_specs);
                results.into_iter().map(|(k, _)| k).collect()
            } else {
                let mut keys: Vec<String> = {
                    let table = match db.get_table_mut_for_account(acc, &table_name) {
                        Ok(t) => t,
                        Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
                    };
                    let records = if is_dict { &table.dictionary } else { &table.records };
                    records.keys().cloned().collect()
                };
                if sort_specs.is_empty() {
                    // `sort_keys_for_account` already falls back to the ID, so only sort here.
                    keys.sort();
                    keys
                } else {
                    db.sort_keys_for_account(acc, &table_name, is_dict, keys, &sort_specs)
                }
            };
            let count = keys.len();
            db.remote_select_lists.insert(list_name.clone(), crate::db::SelectList { table_name, is_dict, keys });
            db.remote_select_cursors.insert(list_name, 0);

            Response { status: "OK".to_string(), count: Some(count), ..Default::default() }
        }
        "GET.NEXT" => {
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());
            let batch_size = req.batch_size.unwrap_or(1);

            let (keys_batch, table_name, is_dict) = {
                let list = match db.remote_select_lists.get(&list_name) {
                    Some(l) => l,
                    None => return Response { status: "ERROR".to_string(), message: Some("Select list not found".to_string()), ..Default::default() },
                };

                let list_keys_len = list.keys.len();
                let table_name = list.table_name.clone();
                let is_dict = list.is_dict;

                let cursor = *db.remote_select_cursors.get(&list_name).unwrap();
                if cursor >= list_keys_len {
                    return Response { status: "EOF".to_string(), ..Default::default() };
                }

                let end = std::cmp::min(cursor + batch_size, list_keys_len);
                let keys = list.keys[cursor..end].to_vec();
                db.remote_select_cursors.insert(list_name, end);
                (keys, table_name, is_dict)
            };

            let mut results = Vec::new();
            {
                let table = match db.get_table_mut_for_account(acc, &table_name) {
                    Ok(t) => t,
                    Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
                };
                let records = if is_dict { &table.dictionary } else { &table.records };

                for key in &keys_batch {
                    if let Some(r) = records.get(key) {
                        results.push((key.clone(), r.clone()));
                    }
                }
            }

            let results_len = results.len();
            let results_processed: Vec<(String, serde_json::Value)> = results.into_iter()
                .map(|(k, r)| (k, db.serialize_record_for_account(acc, &table_name, &r)))
                .collect();

            Response {
                status: "OK".to_string(),
                results: Some(results_processed),
                count: Some(results_len),
                ..Default::default()
            }
        }
        "CREATE.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.target_account {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Account name not specified".to_string()), ..Default::default() },
            };
            match db.create_account(&name, None) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "DELETE.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.target_account {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Account name not specified".to_string()), ..Default::default() },
            };
            match db.delete_account(&name) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "CREATE.FILE" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let name = match req.file {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), ..Default::default() },
            };
            let durable = req.durable.unwrap_or(false);
            match db.create_table_for_account_durable(acc, &name, durable) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "DELETE.FILE" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let name = match req.file {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), ..Default::default() },
            };
            match db.delete_table_for_account(acc, &name) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "AUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let thumbprint = match req.thumbprint {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Thumbprint not specified".to_string()), ..Default::default() }
            };
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), ..Default::default() }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            let is_admin = req.is_admin.unwrap_or(false);
            match db.add_authorized_client(&name, &thumbprint, accounts, is_admin) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "DEAUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), ..Default::default() }
            };
            match db.remove_authorized_client(&name) {
                Ok(true) => Response { status: "OK".to_string(), ..Default::default() },
                Ok(false) => Response { status: "ERROR".to_string(), message: Some("Client not found".to_string()), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "ADD.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), ..Default::default() }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.add_client_account(&name, &acc) {
                    return Response { status: "ERROR".to_string(), message: Some(format!("Error adding account {}: {}", acc, e)), ..Default::default() };
                }
            }
            Response { status: "OK".to_string(), ..Default::default() }
        }
        "REMOVE.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), ..Default::default() }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.remove_client_account(&name, &acc) {
                    return Response { status: "ERROR".to_string(), message: Some(format!("Error removing account {}: {}", acc, e)), ..Default::default() };
                }
            }
            Response { status: "OK".to_string(), ..Default::default() }
        }
        _ => Response { status: "ERROR".to_string(), message: Some("Unknown command".to_string()), ..Default::default() },
    }
}
