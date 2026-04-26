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
            let table_name = match req.file {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
            };
            let key = match req.key {
                Some(k) => k,
                None => return Response { status: "ERROR".to_string(), message: Some("Key not specified".to_string()), ..Default::default() },
            };
            let is_dict = req.is_dict.unwrap_or(false);

            let structured_opt = {
                let (record_clone, table_name_clone) = {
                    let table = match db.get_table_mut_for_account(acc, &table_name) {
                        Ok(t) => t,
                        Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
                    };
                    let records = if is_dict { &table.dictionary } else { &table.records };
                    match records.get(&key) {
                        Some(r) => (Some(r.clone()), table_name.clone()),
                        None => (None, table_name.clone()),
                    }
                };

                match record_clone {
                    Some(r) => Some(db.serialize_record_for_account(acc, &table_name_clone, &r)),
                    None => None,
                }
            };

            match structured_opt {
                Some(structured) => Response {
                    status: "OK".to_string(),
                    record: Some(structured),
                    ..Default::default()
                },
                None => Response { status: "ERROR".to_string(), message: Some("Record not found".to_string()), ..Default::default() },
            }
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
            let records = if is_dict { &mut table.dictionary } else { &mut table.records };
            records.insert(key, record);
            table.dirty = true;
            match db.save() {
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
            let records = if is_dict { &mut table.dictionary } else { &mut table.records };
            records.remove(&key);
            table.dirty = true;
            match db.save() {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Save error: {}", e)), ..Default::default() },
            }
        }
        "QUERY" => {
            if target_account.is_none() {
                return Response { status: "ERROR".to_string(), message: Some("Account not specified".to_string()), ..Default::default() };
            }
            let table_name = match req.file {
                Some(t) => t,
                None => return Response { status: "ERROR".to_string(), message: Some("File not specified".to_string()), ..Default::default() },
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
                db.query_for_account(acc, &table_name, is_dict, &q, None)
            } else {
                let table = match db.get_table_mut_for_account(acc, &table_name) {
                    Ok(t) => t,
                    Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
                };
                let records = if is_dict { &table.dictionary } else { &table.records };
                // Optimization: use iterator to avoid full map clone before sorting
                let mut res: Vec<_> = records.iter().map(|(k, r)| (k.clone(), r.clone())).collect();
                res.sort_by(|a, b| a.0.cmp(&b.0));
                res
            };

            let results_processed: Vec<(String, serde_json::Value)> = results.into_iter()
                .map(|(k, r)| (k, db.serialize_record_for_account(acc, &table_name, &r)))
                .collect();

            Response {
                status: "OK".to_string(),
                results: Some(results_processed),
                ..Default::default()
            }
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

            let query_node = if let Some(node) = req.query_node {
                Some(node)
            } else if let Some(q_str) = req.query_string {
                let parts: Vec<&str> = q_str.split_whitespace().collect();
                db.parse_query(&table_name, &parts)
            } else {
                None
            };

            let keys = if let Some(q) = query_node {
                let results = db.query_for_account(acc, &table_name, is_dict, &q, None);
                results.into_iter().map(|(k, _)| k).collect()
            } else {
                let table = match db.get_table_mut_for_account(acc, &table_name) {
                    Ok(t) => t,
                    Err(e) => return Response { status: "ERROR".to_string(), message: Some(format!("Table error: {}", e)), ..Default::default() },
                };
                let records = if is_dict { &table.dictionary } else { &table.records };
                // Optimization: Collect and sort keys directly to avoid cloning full records
                let mut keys: Vec<String> = records.keys().cloned().collect();
                keys.sort();
                keys
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
            let name = match req.file {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), ..Default::default() },
            };
            match db.create_table(&name) {
                Ok(_) => Response { status: "OK".to_string(), ..Default::default() },
                Err(e) => Response { status: "ERROR".to_string(), message: Some(format!("Error: {}", e)), ..Default::default() },
            }
        }
        "DELETE.FILE" => {
            if !client_info.is_admin {
                return Response { status: "ERROR".to_string(), message: Some("Admin privileges required".to_string()), ..Default::default() };
            }
            let name = match req.file {
                Some(n) => n,
                None => return Response { status: "ERROR".to_string(), message: Some("File name not specified".to_string()), ..Default::default() },
            };
            match db.delete_table(&name) {
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
