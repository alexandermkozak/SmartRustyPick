use crate::db::{Database, Record};
use crate::server::models::{Request, Response};
use std::sync::{Arc, Mutex};

pub fn handle_request(req: Request, db: &Arc<Mutex<Database>>, client_info: &crate::db::ClientInfo) -> Response {
    let mut db = db.lock().unwrap();

    let target_account = if let Some(acc) = req.account {
        // Client specified an account
        if !client_info.is_admin && !client_info.allowed_accounts.contains(&acc) {
            let msg = format!("Access denied for account {}: Not in allowed list", acc);
            let _ = db.log_error("REMOTE", &msg);
            return Response { status: "ERROR".to_string(), message: Some(msg), record: None, results: None, keys: None, count: None };
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
            return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
        }
    };

    if let Some(ref acc) = target_account {
        if db.current_account != *acc {
            if let Err(e) = db.logto(acc) {
                let msg = format!("Remote login error for account {}: {}", acc, e);
                let _ = db.log_error("REMOTE", &msg);
                return Response { status: "ERROR".to_string(), message: Some(format!("Failed to login to account: {}", e)), record: None, results: None, keys: None, count: None };
            }
        }
    }

    match req.command.to_uppercase().as_str() {
        "READ" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
            }
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = match db.get_table_mut(&table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
            };
            let records = if is_dict { &table.dictionary } else { &table.records };
            match records.get(&key) {
                Some(r) => Response { status: "OK".to_string(), message: None, record: Some(r.to_display_string()), results: None, keys: None, count: None },
                None => Response { status: "ERROR".to_string(), message: Some("Record not found".to_string()), record: None, results: None, keys: None, count: None },
            }
        }
        "WRITE" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
            }
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let data = match req.data {
                Some(d) => d,
                None => return Response { status: "ERROR".to_string(), message: Some("Data not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = match db.get_table_mut(&table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
            };
            let records = if is_dict { &mut table.dictionary } else { &mut table.records };
            records.insert(key, Record::from_display_string(&data));
            table.dirty = true;
            match db.save() {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Save error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "DELETE" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
            }
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let table = match db.get_table_mut(&table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
            };
            let records = if is_dict { &mut table.dictionary } else { &mut table.records };
            records.remove(&key);
            table.dirty = true;
            match db.save() {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Save error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "QUERY" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
            }
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let query_node = if let Some(node) = req.query_node {
                Some(node)
            } else if let Some(q_str) = req.query_string {
                let parts: Vec<&str> = q_str.split_whitespace().collect();
                db.parse_query(&table_name, &parts)
            } else {
                None
            };

            let results = if let Some(q) = query_node {
                db.query(&table_name, is_dict, &q, None)
            } else {
                let table = match db.get_table_mut(&table_name) {
                    Ok(t) => t,
                    Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
                };
                let records = if is_dict { &table.dictionary } else { &table.records };
                // Optimization: use iterator to avoid full map clone before sorting
                let mut res: Vec<_> = records.iter().map(|(k, r)| (k.clone(), r.clone())).collect();
                res.sort_by(|a, b| a.0.cmp(&b.0));
                res
            };

            let formatted: Vec<(String, String)> = results.into_iter()
                .map(|(k, r)| (k, r.to_display_string()))
                .collect();

            Response { status: "OK".to_string(), message: None, record: None, results: Some(formatted), keys: None, count: None }
        }
        "SELECT" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), record: None, results: None, keys: None, count: None };
            }
            let table_name = match req.table {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Table not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            let is_dict = req.is_dict.unwrap_or(false);
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());

            let query_node = if let Some(node) = req.query_node {
                Some(node)
            } else if let Some(q_str) = req.query_string {
                let parts: Vec<&str> = q_str.split_whitespace().collect();
                db.parse_query(&table_name, &parts)
            } else {
                None
            };

            let results = if let Some(q) = query_node {
                db.query(&table_name, is_dict, &q, None)
            } else {
                let table = match db.get_table_mut(&table_name) {
                    Ok(t) => t,
                    Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
                };
                let records = if is_dict { &table.dictionary } else { &table.records };
                // Optimization: use iterator to avoid full map clone before sorting
                let mut res: Vec<_> = records.iter().map(|(k, r)| (k.clone(), r.clone())).collect();
                res.sort_by(|a, b| a.0.cmp(&b.0));
                res
            };

            let keys: Vec<String> = results.into_iter().map(|(k, _)| k).collect();
            let count = keys.len();
            db.remote_select_lists.insert(list_name.clone(), crate::db::SelectList { table_name, is_dict, keys });
            db.remote_select_cursors.insert(list_name, 0);

            Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: Some(count) }
        }
        "GET.NEXT" => {
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());
            let batch_size = req.batch_size.unwrap_or(1);

            let (keys_batch, table_name, is_dict) = {
                let list = match db.remote_select_lists.get(&list_name) {
                    Some(l) => l,
                    None => return Response { status: "ERROR".to_string(), message: Some("Select list not found".to_string()), record: None, results: None, keys: None, count: None },
                };

                let list_keys_len = list.keys.len();
                let table_name = list.table_name.clone();
                let is_dict = list.is_dict;

                let cursor = *db.remote_select_cursors.get(&list_name).unwrap();
                if cursor >= list_keys_len {
                    return Response { status: "EOF".to_string(), message: None, record: None, results: None, keys: None, count: None };
                }

                let end = std::cmp::min(cursor + batch_size, list_keys_len);
                let keys = list.keys[cursor..end].to_vec();
                db.remote_select_cursors.insert(list_name, end);
                (keys, table_name, is_dict)
            };

            let mut results = Vec::new();
            let table = match db.get_table_mut(&table_name) {
                Ok(t) => t,
                Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), record: None, results: None, keys: None, count: None },
            };
            let records = if is_dict { &table.dictionary } else { &table.records };

            for key in &keys_batch {
                if let Some(r) = records.get(key) {
                    results.push((key.clone(), r.to_display_string()));
                }
            }

            let results_len = results.len();
            Response { status: "OK".to_string(), message: None, record: None, results: Some(results), keys: None, count: Some(results_len) }
        }
        "CREATE.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.target_account {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Account name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            match db.create_account(&name, None) {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "DELETE.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.target_account {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Account name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            match db.delete_account(&name) {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "CREATE.FILE" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.table {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            match db.create_table(&name) {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "DELETE.FILE" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.table {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), record: None, results: None, keys: None, count: None },
            };
            match db.delete_table(&name) {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "AUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let thumbprint = match req.thumbprint {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("Thumbprint not specified".to_string()), record: None, results: None, keys: None, count: None }
            };
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), record: None, results: None, keys: None, count: None }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            let is_admin = req.is_admin.unwrap_or(false);
            match db.add_authorized_client(&name, &thumbprint, accounts, is_admin) {
                Ok(_) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "DEAUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), record: None, results: None, keys: None, count: None }
            };
            match db.remove_authorized_client(&name) {
                Ok(true) => Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None },
                Ok(false) => Response { status: "ERROR".to_string(), message: Some("Client not found".to_string()), record: None, results: None, keys: None, count: None },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), record: None, results: None, keys: None, count: None },
            }
        }
        "ADD.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), record: None, results: None, keys: None, count: None }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.add_client_account(&name, &acc) {
                    return Response { status: "ERROR".to_string(), message: Some(format!("Error adding account {}: {}", acc, e)), record: None, results: None, keys: None, count: None };
                }
            }
            Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None }
        }
        "REMOVE.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), record: None, results: None, keys: None, count: None };
            }
            let name = match req.name {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("Name not specified".to_string()), record: None, results: None, keys: None, count: None }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.remove_client_account(&name, &acc) {
                    return Response { status: "ERROR".to_string(), message: Some(format!("Error removing account {}: {}", acc, e)), record: None, results: None, keys: None, count: None };
                }
            }
            Response { status: "OK".to_string(), message: None, record: None, results: None, keys: None, count: None }
        }
        _ => Response { status: "ERROR".to_string(), message: Some("Unknown command".to_string()), record: None, results: None, keys: None, count: None },
    }
}
