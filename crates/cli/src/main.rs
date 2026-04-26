use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::{Database, Record};
use smart_rusty_pick_core::server;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut initial_account = None;
    if let Some(pos) = args.iter().position(|a| a == "--account") {
        if pos + 1 < args.len() {
            initial_account = Some(args[pos + 1].clone());
        }
    }

    let mut db_dir = "db_storage".to_string();
    if let Some(pos) = args.iter().position(|a| a == "-d" || a == "--db-dir") {
        if pos + 1 < args.len() {
            db_dir = args[pos + 1].clone();
        }
    }

    let config = Config::load();
    let config_arc = Arc::new(config.clone());

    // We use a directory "db_storage" to hold our tables
    let db = Arc::new(Mutex::new(Database::new(&db_dir, Some(config.clone()))?));

    // Check if server should be auto-started in background for CLI
    if config.cert_path.is_some() && config.key_path.is_some() && config.ca_path.is_some() {
        if let Err(e) = server::ensure_certificates(&config) {
            eprintln!("Failed to ensure certificates: {}", e);
        }

        let db_clone = db.clone();
        let config_clone = config.clone();

        let config_arc_clone = config_arc.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let addr = config_clone.server_addr.clone().unwrap_or_else(|| "127.0.0.1".to_string());
                let port = config_clone.server_port.unwrap_or(8443);
                let full_addr = if addr.contains(':') { addr } else { format!("{}:{}", addr, port) };
                let _ = server::start_server(config_arc_clone, db_clone, Some(full_addr)).await;
            });
        });
        println!("Database service attached and running in background.");
    }

    println!("SmartRustyPick CLI. Type 'HELP' for commands.");

    // Auto-login based on current directory
    let auto_account = {
        let db_lock = db.lock().unwrap();
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        db_lock.get_account_for_dir(current_dir.to_str().unwrap_or("."))
    };

    if let Some(account_name) = auto_account {
        let mut db_lock = db.lock().unwrap();
        let acc_to_log = account_name.clone();
        if db_lock.logto(&acc_to_log).is_ok() {
            println!("Auto-logged into account '{}' based on current directory.", account_name);
            let _ = check_dir_file(&mut db_lock);
        }
    }

    // Account login prompt if not logged in
    loop {
        {
            let db_lock = db.lock().unwrap();
            if !db_lock.current_account.is_empty() {
                break;
            }
        }

        let account_name = if let Some(acc) = initial_account.take() {
            acc
        } else {
            print!("Account: ");
            io::stdout().flush()?;
            let mut account_input = String::new();
            if io::stdin().read_line(&mut account_input)? == 0 {
                return Ok(());
            }
            account_input.trim().to_string()
        };

        if account_name.is_empty() {
            continue;
        }

        let mut db_lock = db.lock().unwrap();
        if let Err(e) = db_lock.logto(&account_name) {
            let msg = format!("Login error: {}", e);
            let _ = db_lock.log_error("CLI", &msg);
            println!("Error: {}", e);
            println!("Account '{}' not found. Create it? (Y/N)", account_name);
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            if choice.trim().to_uppercase() == "Y" {
                db_lock.create_account(&account_name, None)?;
                db_lock.logto(&account_name)?;
                let _ = check_dir_file(&mut db_lock);
                break;
            } else {
                continue;
            }
        } else {
            let _ = check_dir_file(&mut db_lock);
            break;
        }
    }

    loop {
        let prompt = {
            let db_lock = db.lock().unwrap();
            let acc = db_lock.current_account.clone();
            if acc.is_empty() {
                "PICK> ".to_string()
            } else {
                format!("{} PICK> ", acc)
            }
        };
        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            break;
        }
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() { continue; }
        let command = parts[0].to_uppercase();

        match command.as_str() {
            "SET" => {
                handle_set(&mut db.lock().unwrap(), &parts);
            }
            "GET" => {
                handle_get(&mut db.lock().unwrap(), &parts);
            }
            "DELETE" => {
                handle_delete(&mut db.lock().unwrap(), &parts);
            }
            "LIST" => {
                handle_list(&mut db.lock().unwrap(), &parts);
            }
            "SELECT" => {
                handle_select(&mut db.lock().unwrap(), &parts);
            }
            "EDIT" => {
                handle_edit(&mut db.lock().unwrap(), &parts, &config);
            }
            "CT" => {
                handle_ct(&mut db.lock().unwrap(), &parts);
            }
            "SAVE-LIST" => {
                handle_save_list(&mut db.lock().unwrap(), &parts);
            }
            "GET-LIST" => {
                handle_get_list(&mut db.lock().unwrap(), &parts);
            }
            "CREATE.FILE" => {
                let mut db_lock = db.lock().unwrap();
                handle_create_file(&mut db_lock, &parts);
            }
            "DELETE.FILE" => {
                let mut db_lock = db.lock().unwrap();
                handle_delete_file(&mut db_lock, &parts);
            }
            "CREATE.ACCOUNT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_create_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "CREATE.TEST.ACCOUNT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_create_test_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "DELETE.ACCOUNT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_delete_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "LOGTO" => {
                let mut db_lock = db.lock().unwrap();
                handle_logto(&mut db_lock, &parts);
                let _ = check_dir_file(&mut db_lock);
            }
            "LIST.FILES" => {
                handle_list_files(&mut db.lock().unwrap());
            }
            "AUTHORIZE.CONN" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_authorize_conn(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "ADD.CLIENT.ACCOUNT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_add_client_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "REMOVE.CLIENT.ACCOUNT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_remove_client_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "DEAUTHORIZE.CONN" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_deauthorize_conn(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "LIST.CONNS" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_list_conns(&mut db_lock);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "GENERATE.CERT" => {
                let mut db_lock = db.lock().unwrap();
                if db_lock.current_account == "SYSTEM" {
                    handle_generate_cert(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "START.SERVER" => {
                handle_start_server(db.clone(), &parts, config_arc.clone());
            }
            "SAVE" => {
                db.lock().unwrap().save()?;
                println!("OK");
            }
            "HELP" => {
                let db_lock = db.lock().unwrap();
                print_help(&db_lock.current_account);
            }
            "EXIT" | "QUIT" => break,
            _ => println!("Unknown command: {}", command),
        }
    }

    // Auto-save on exit
    db.lock().unwrap().save()?;
    Ok(())
}

fn handle_set(db: &mut Database, parts: &[&str]) {
    // SET [DICT] <table> <key> <data>
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 3 {
        println!("Usage: SET [DICT] <table> <key> <data>");
        return;
    }

    let table_name = parts[offset];
    let key = parts[offset + 1].to_string();
    let data = parts[offset + 2..].join(" ");

    let table = match db.get_table_mut(table_name) {
        Ok(t) => t,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    let record = Record::from_display_string(&data);
    if is_dict {
        table.dictionary.insert(key, record);
    } else {
        table.records.insert(key, record);
    }
    table.dirty = true;
    if table_name == "$CLIENTS" {
        let _ = db.save();
    }
    println!("OK");
}

fn handle_get(db: &mut Database, parts: &[&str]) {
    // GET [DICT] <table> [<key>]
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        println!("Usage: GET [DICT] <table> [<key>]");
        return;
    }

    let table_name = parts[offset];

    if parts.len() < offset + 2 {
        // Try to use active select list
        let mut keys_from_list = None;
        if let Some(list) = &db.active_select_list {
            if list.table_name == table_name && list.is_dict == is_dict {
                keys_from_list = Some(list.keys.clone());
            }
        }

        if let Some(keys) = keys_from_list {
            if let Some(table) = db.get_table(table_name) {
                let map = if is_dict { &table.dictionary } else { &table.records };
                for key in &keys {
                    if let Some(record) = map.get(key) {
                        println!("{}: {}", key, record.to_display_string());
                    }
                }
            }
            db.active_select_list = None;
        } else {
            println!("Usage: GET [DICT] <table> <key>");
            println!("(Or use an active SELECT list)");
        }
        return;
    }

    let key = parts[offset + 1];

    if let Some(table) = db.get_table(table_name) {
        let map = if is_dict { &table.dictionary } else { &table.records };
        if let Some(record) = map.get(key) {
            println!("{}", record.to_display_string());
        } else {
            println!("NOT FOUND");
        }
    } else {
        println!("TABLE NOT FOUND");
    }
}

fn handle_delete(db: &mut Database, parts: &[&str]) {
    // DELETE [DICT] <table> [<key>]
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        println!("Usage: DELETE [DICT] <table> [<key>]");
        return;
    }

    let table_name = parts[offset];

    if parts.len() < offset + 2 {
        // Try to use active select list
        let mut keys_to_delete = Vec::new();
        let mut used_list = false;
        if let Some(list) = &db.active_select_list {
            if list.table_name == table_name && list.is_dict == is_dict {
                keys_to_delete = list.keys.clone();
                used_list = true;
            }
        }

        if used_list {
            let table = match db.get_table_mut(table_name) {
                Ok(t) => t,
                Err(e) => {
                    println!("Error: {}", e);
                    return;
                }
            };
            let map = if is_dict { &mut table.dictionary } else { &mut table.records };
            let mut count = 0;
            for key in keys_to_delete {
                if map.remove(&key).is_some() {
                    count += 1;
                }
            }
            if count > 0 {
                table.dirty = true;
                println!("[{}] records deleted", count);
            } else {
                println!("NO RECORDS DELETED");
            }
            db.active_select_list = None;
        } else {
            println!("Usage: DELETE [DICT] <table> <key>");
            println!("(Or use an active SELECT list)");
        }
        return;
    }

    let key = parts[offset + 1];

    let table = match db.get_table_mut(table_name) {
        Ok(t) => t,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    let map = if is_dict { &mut table.dictionary } else { &mut table.records };
    if map.remove(key).is_some() {
        table.dirty = true;
        if table_name == "$CLIENTS" {
            let _ = db.save();
        }
        println!("OK");
    } else {
        println!("NOT FOUND");
    }
}

fn handle_list(db: &mut Database, parts: &[&str]) {
    // LIST [DICT] <table> [<fields>...]
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        // List all tables
        let tables = db.list_tables();
        for t in tables {
            println!("{}", t);
        }
        return;
    }

    let table_name = parts[offset];
    let field_names = &parts[offset + 1..];

    let mut use_select_list = false;
    let mut selected_keys = Vec::new();
    if let Some(list) = &db.active_select_list {
        if list.table_name == table_name && list.is_dict == is_dict {
            use_select_list = true;
            selected_keys = list.keys.clone();
        }
    }

    let table_exists = db.list_tables().contains(&table_name.to_string());
    if table_exists {
        let (map_keys, is_dict_val) = {
            let table = match db.get_table_mut(table_name) {
                Ok(t) => t,
                Err(e) => {
                    println!("Error: {}", e);
                    return;
                }
            };
            let map = if is_dict { &table.dictionary } else { &table.records };
            let keys = if use_select_list {
                selected_keys
            } else {
                let mut k: Vec<_> = map.keys().cloned().collect();
                k.sort();
                k
            };
            (keys, is_dict)
        };

        if field_names.is_empty() {
            for key in map_keys {
                println!("{}", key);
            }
        } else {
            // Collect field info for headers and formatting
            struct FieldFormat {
                name: String,
                header: String,
                width: usize,
                justify: String,
            }

            let mut formats = Vec::new();
            // First column is always ID
            formats.push(FieldFormat {
                name: "ID".to_string(),
                header: "ID".to_string(),
                width: 10,
                justify: "L".to_string(),
            });

            let mut expanded_fields = Vec::new();
            for &name in field_names {
                if name == "*" {
                    expanded_fields.extend(db.get_all_dict_fields_read_only_for_account(&db.current_account, table_name));
                } else {
                    expanded_fields.push(name.to_string());
                }
            }

            for name in &expanded_fields {
                let header = db.get_field_header_read_only_for_account(&db.current_account, table_name, name);
                let width = db.get_field_width_read_only_for_account(&db.current_account, table_name, name);
                let justify = db.get_field_justification_read_only_for_account(&db.current_account, table_name, name);
                formats.push(FieldFormat {
                    name: name.clone(),
                    header,
                    width,
                    justify,
                });
            }

            // Print headers
            let mut header_line = String::new();
            let mut separator_line = String::new();
            for (i, fmt) in formats.iter().enumerate() {
                if i > 0 {
                    header_line.push(' ');
                    separator_line.push(' ');
                }
                let cell = if fmt.justify == "R" {
                    format!("{:>width$.width$}", fmt.header, width = fmt.width)
                } else {
                    format!("{:<width$.width$}", fmt.header, width = fmt.width)
                };
                header_line.push_str(&cell);
                separator_line.push_str(&"-".repeat(fmt.width));
            }
            println!("{}", header_line);
            println!("{}", separator_line);

            // Now iterate over records
            for key in map_keys {
                let record = {
                    let table = match db.get_table_mut(table_name) {
                        Ok(t) => t,
                        Err(e) => {
                            println!("Error: {}", e);
                            return;
                        }
                    };
                    let map = if is_dict_val { &table.dictionary } else { &table.records };
                    map.get(&key).cloned()
                };

                if let Some(record) = record {
                    let mut row_line = String::new();
                    for (i, fmt) in formats.iter().enumerate() {
                        if i > 0 {
                            row_line.push(' ');
                        }

                        let formatted_val = if fmt.name == "ID" {
                            key.clone()
                        } else {
                            db.format_record_field(table_name, &record, &fmt.name)
                        };

                        let cell = if fmt.justify == "R" {
                            format!("{:>width$.width$}", formatted_val, width = fmt.width)
                        } else {
                            format!("{:<width$.width$}", formatted_val, width = fmt.width)
                        };
                        row_line.push_str(&cell);
                    }
                    println!("{}", row_line);
                }
            }
        }
    } else {
        println!("TABLE NOT FOUND");
    }

    if use_select_list {
        db.active_select_list = None;
    }
}

fn handle_select(db: &mut Database, parts: &[&str]) {
    // SELECT [DICT] <table> [WITH <field> <op> <value>]
    // e.g. SELECT USERS WITH First.Name = "Ted"
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        println!("Usage: SELECT [DICT] <table> [WITH <field> <op> <value>]");
        return;
    }

    let table_name = parts[offset];

    // Check if we should refine the active select list
    let keys_to_filter = if let Some(list) = &db.active_select_list {
        if list.table_name == table_name && list.is_dict == is_dict {
            Some(list.keys.clone())
        } else {
            None
        }
    } else {
        None
    };

    let results = if parts.len() >= offset + 2 && parts[offset + 1].to_uppercase() == "WITH" {
        if let Some(query) = db.parse_query(table_name, &parts[offset + 1..]) {
            db.query(table_name, is_dict, &query, keys_to_filter.as_deref())
        } else {
            println!("INVALID QUERY FORMAT");
            return;
        }
    } else if parts.len() == offset + 1 {
        if let Some(table) = db.get_table(table_name) {
            let map = if is_dict { &table.dictionary } else { &table.records };
            let mut res: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            res.sort_by(|a, b| a.0.cmp(&b.0));
            res
        } else {
            println!("TABLE NOT FOUND");
            return;
        }
    } else {
        println!("Usage: SELECT [DICT] <table> [WITH <field> <op> <value>]");
        return;
    };

    if results.is_empty() {
        println!("NO RECORDS FOUND");
        db.active_select_list = None;
    } else {
        let keys: Vec<String> = results.iter().map(|(k, _)| k.clone()).collect();
        println!("[{}] records selected", keys.len());
        db.active_select_list = Some(smart_rusty_pick_core::db::SelectList {
            table_name: table_name.to_string(),
            is_dict,
            keys,
        });
    }
}

fn handle_edit(db: &mut Database, parts: &[&str], config: &Config) {
    // EDIT [DICT] <table> <key>
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 2 {
        println!("Usage: EDIT [DICT] <table> <key>");
        return;
    }

    let table_name = parts[offset];
    let key = parts[offset + 1];

    // Get current record content or empty string
    let current_content = if let Some(table) = db.get_table(table_name) {
        let map = if is_dict { &table.dictionary } else { &table.records };
        if let Some(record) = map.get(key) {
            record.to_edit_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Create temporary file
    let temp_file_path = format!(".edit_{}_{}.tmp", table_name, key);
    if let Err(e) = std::fs::write(&temp_file_path, current_content) {
        println!("Error creating temporary file: {}", e);
        return;
    }

    // Launch editor
    // Priority: config.toml > EDITOR env var > nano
    let editor = config.editor.clone()
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "nano".to_string());

    // Split editor command to handle arguments (e.g., "python3 fake_editor.py")
    let editor_parts: Vec<&str> = editor.split_whitespace().collect();
    if editor_parts.is_empty() {
        println!("Invalid editor configuration");
        return;
    }

    let status = std::process::Command::new(editor_parts[0])
        .args(&editor_parts[1..])
        .arg(&temp_file_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Read back the content
            match std::fs::read_to_string(&temp_file_path) {
                Ok(new_content) => {
                    let table = match db.get_table_mut(table_name) {
                        Ok(t) => t,
                        Err(e) => {
                            println!("Error: {}", e);
                            return;
                        }
                    };
                    let record = Record::from_edit_string(&new_content);
                    let key_str = key.to_string();
                    if is_dict {
                        table.dictionary.insert(key_str, record);
                    } else {
                        table.records.insert(key_str, record);
                    }
                    table.dirty = true;
                    if table_name == "$CLIENTS" {
                        let _ = db.save();
                    }
                    println!("OK");
                }
                Err(e) => println!("Error reading back content: {}", e),
            }
        }
        Ok(s) => println!("Editor exited with error: {}", s),
        Err(e) => println!("Failed to launch editor: {}", e),
    }

    // Cleanup
    let _ = std::fs::remove_file(&temp_file_path);
}

fn handle_ct(db: &mut Database, parts: &[&str]) {
    // CT [DICT] <table> [<key>]
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        println!("Usage: CT [DICT] <table> [<key>]");
        return;
    }

    let table_name = parts[offset];

    if parts.len() < offset + 2 {
        // Try to use active select list
        let mut keys_from_list = None;
        if let Some(list) = &db.active_select_list {
            if list.table_name == table_name && list.is_dict == is_dict {
                keys_from_list = Some(list.keys.clone());
            }
        }

        if let Some(keys) = keys_from_list {
            if let Some(table) = db.get_table(table_name) {
                let map = if is_dict { &table.dictionary } else { &table.records };
                for (idx, key) in keys.iter().enumerate() {
                    if let Some(record) = map.get(key) {
                        println!("{}:", key);
                        print_record_fields(record);
                        if idx < keys.len() - 1 {
                            println!();
                        }
                    }
                }
            }
            db.active_select_list = None;
        } else {
            println!("Usage: CT [DICT] <table> <key>");
            println!("(Or use an active SELECT list)");
        }
        return;
    }

    let key = parts[offset + 1];

    if let Some(table) = db.get_table(table_name) {
        let map = if is_dict { &table.dictionary } else { &table.records };
        if let Some(record) = map.get(key) {
            print_record_fields(record);
        } else {
            println!("NOT FOUND");
        }
    } else {
        println!("TABLE NOT FOUND");
    }
}

fn print_record_fields(record: &Record) {
    for (i, field) in record.fields.iter().enumerate() {
        let mut res = Vec::new();
        for (j, v) in field.values.iter().enumerate() {
            if j > 0 { res.push(smart_rusty_pick_core::db::VM); }
            for (k, sv) in v.sub_values.iter().enumerate() {
                if k > 0 { res.push(smart_rusty_pick_core::db::SVM); }
                res.extend_from_slice(sv.as_bytes());
            }
        }
        let display_bytes: Vec<u8> = res.iter().map(|&b| match b {
            smart_rusty_pick_core::db::VM => b']',
            smart_rusty_pick_core::db::SVM => b'\\',
            _ => b
        }).collect();
        println!("{:03} {}", i + 1, String::from_utf8_lossy(&display_bytes));
    }
}

fn print_help(current_account: &str) {
    println!("Commands:");
    println!("  SET [DICT] <table> <key> <data>       - Store a record.");
    println!("  GET [DICT] <table> [<key>]             - Retrieve record(s). Uses SELECT list if key omitted.");
    println!("  DELETE [DICT] <table> [<key>]          - Remove record(s). Uses SELECT list if key omitted.");
    println!("  LIST [DICT] [<table> [<fields>...]]   - List tables, keys, or records. Uses SELECT list if applicable.");
    println!("  SELECT [DICT] <table> [WITH <field> <op> <value>] - Create/refine active select list.");
    println!("    Operators: =, #, <>, <, >, <=, >=, EQ, NE, LT, GT, LE, GE");
    println!("    Wildcards (with = or #): [value (ends with), value] (starts with), [value] (contains)");
    println!("  EDIT [DICT] <table> <key>             - Edit a record using external editor.");
    println!("  CT [DICT] <table> [<key>]             - Print record contents, field by field. Uses SELECT list if key omitted.");
    println!("  SAVE                                  - Save database to disk.");
    println!("  HELP                                  - Show this help.");
    println!("  SAVE-LIST <name>                      - Save active select list.");
    println!("  GET-LIST <name>                       - Restore a saved select list.");
    println!("  CREATE.FILE <name>                    - Create a new file (data and dict) (SYSTEM only).");
    println!("  DELETE.FILE <name>                    - Delete a file (data and dict) (SYSTEM only).");
    println!("  CREATE.ACCOUNT <name> [<dir>]         - Create a new account (SYSTEM only).");
    if current_account == "SYSTEM" {
        println!("  CREATE.TEST.ACCOUNT <name>            - Create and populate a test account (SYSTEM only).");
    }
    println!("  DELETE.ACCOUNT <name>                 - Delete an account and all its files (SYSTEM only).");
    println!("  LOGTO <name>                          - Switch to a different account.");
    println!("  LIST.FILES                            - List all files in the current account.");
    if current_account == "SYSTEM" {
        println!("  AUTHORIZE.CONN <thumbprint> <name> <ADMIN | accounts> - Authorize a client.");
        println!("  ADD.CLIENT.ACCOUNT <name> <accounts>  - Add allowed accounts to a client.");
        println!("  REMOVE.CLIENT.ACCOUNT <name> <accounts> - Remove allowed accounts from a client.");
        println!("  DEAUTHORIZE.CONN <name>               - Deauthorize an SSL cert by name.");
        println!("  LIST.CONNS                            - List authorized connections.");
        println!("  GENERATE.CERT <common_name>           - Generate and sign a new client certificate (SYSTEM only).");
    }
    println!("  START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path> - Start TCP SSL server.");
    println!("  SAVE                                  - Save all changes to disk.");
    println!("  EXIT or QUIT                          - Exit the shell.");
}

fn handle_save_list(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: SAVE-LIST <list_name>");
        return;
    }

    let list_name = parts[1];

    let list = match &db.active_select_list {
        Some(l) => l.clone(),
        None => {
            println!("NO ACTIVE SELECT LIST");
            return;
        }
    };

    let mut data = Vec::new();
    data.extend_from_slice(list.table_name.as_bytes());
    data.push(smart_rusty_pick_core::db::FM);
    data.extend_from_slice(if list.is_dict { b"1" } else { b"0" });
    for key in &list.keys {
        data.push(smart_rusty_pick_core::db::FM);
        data.extend_from_slice(key.as_bytes());
    }

    let record = Record::from_bytes(&data);
    let table = db.get_table_mut("$SAVEDLISTS").unwrap();
    table.records.insert(list_name.to_string(), record);
    table.dirty = true;

    db.active_select_list = None;
    println!("List '{}' saved", list_name);
}

fn handle_get_list(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: GET-LIST <list_name>");
        return;
    }

    let list_name = parts[1];

    let table = db.get_table_mut("$SAVEDLISTS").unwrap();
    if let Some(record) = table.records.get(list_name) {
        let data = record.to_bytes();
        let fields: Vec<&[u8]> = data.split(|&b| b == smart_rusty_pick_core::db::FM).collect();

        if fields.len() < 2 {
            println!("INVALID SAVED LIST FORMAT");
            return;
        }

        let table_name = String::from_utf8_lossy(fields[0]).to_string();
        let is_dict = fields[1] == b"1";
        let mut keys = Vec::new();
        for f in &fields[2..] {
            keys.push(String::from_utf8_lossy(f).to_string());
        }

        db.active_select_list = Some(smart_rusty_pick_core::db::SelectList {
            table_name,
            is_dict,
            keys,
        });
        println!("[{}] records retrieved", db.active_select_list.as_ref().unwrap().keys.len());
    } else {
        println!("LIST '{}' NOT FOUND", list_name);
    }
}

fn handle_create_file(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: CREATE.FILE <file_name>");
        return;
    }
    let file_name = parts[1];
    match db.create_table(file_name) {
        Ok(_) => println!("[{}] created (data and dict)", file_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_delete_file(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: DELETE.FILE <file_name>");
        return;
    }
    let file_name = parts[1];
    match db.delete_table(file_name) {
        Ok(_) => println!("[{}] deleted (data and dict)", file_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_create_account(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: CREATE.ACCOUNT <account_name> [<directory>]");
        return;
    }
    let account_name = parts[1];
    let directory = if parts.len() > 2 { Some(parts[2]) } else { None };
    match db.create_account(account_name, directory) {
        Ok(_) => println!("Account '{}' created", account_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_create_test_account(db: &mut Database, parts: &[&str]) {
    if db.current_account != "SYSTEM" {
        println!("Error: CREATE.TEST.ACCOUNT can only be executed from the SYSTEM account");
        return;
    }
    if parts.len() < 2 {
        println!("Usage: CREATE.TEST.ACCOUNT <account_name>");
        return;
    }
    let account_name = parts[1];
    match db.create_test_account(account_name) {
        Ok(_) => println!("Test account '{}' created and populated", account_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_delete_account(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: DELETE.ACCOUNT <account_name>");
        return;
    }
    let account_name = parts[1];
    match db.delete_account(account_name) {
        Ok(_) => println!("Account '{}' deleted", account_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_logto(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: LOGTO <account_name>");
        return;
    }
    let account_name = parts[1];
    match db.logto(account_name) {
        Ok(_) => println!("Logged into account '{}'", account_name),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_list_files(db: &mut Database) {
    if db.current_account.is_empty() {
        println!("Error: Not logged into an account");
        return;
    }

    match db.get_table("DIR") {
        Some(table) => {
            println!("{:<20} {:<10}", "File", "Type");
            println!("{:-<20} {:-<10}", "", "");

            let mut files: Vec<_> = table.records.iter().collect();
            files.sort_by_key(|(k, _)| *k);

            for (name, record) in files {
                let file_type = record.fields.get(0)
                    .and_then(|f| f.values.get(0))
                    .and_then(|v| v.sub_values.get(0))
                    .map(|s| s.as_str())
                    .unwrap_or("");

                if file_type == "F" {
                    println!("{:<20} {:<10}", name, file_type);
                }
            }
        }
        None => {
            println!("Error: DIR file not found. Use LOGTO or check account.");
        }
    }
}

fn handle_authorize_conn(db: &mut Database, parts: &[&str]) {
    if parts.len() < 4 {
        println!("Usage: AUTHORIZE.CONN <thumbprint> <name> <ADMIN | accounts>");
        println!("  'accounts' is a comma separated list of allowed accounts.");
        return;
    }
    let thumbprint = parts[1];
    let name = parts[2];
    let arg3 = parts[3].to_uppercase();

    let (is_admin, accounts) = if arg3 == "ADMIN" {
        (true, Vec::new())
    } else {
        (false, arg3.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
    };

    if !is_admin && accounts.is_empty() {
        println!("Error: Must provide ADMIN or at least one account.");
        return;
    }

    match db.add_authorized_client(name, thumbprint, accounts, is_admin) {
        Ok(_) => {
            if is_admin {
                println!("Authorized: {} as {} (ADMIN)", thumbprint, name);
            } else {
                println!("Authorized: {} as {}", thumbprint, name);
            }
        }
        Err(e) => println!("Error authorizing: {}", e),
    }
}

fn handle_add_client_account(db: &mut Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: ADD.CLIENT.ACCOUNT <name> <accounts>");
        return;
    }
    let name = parts[1];
    let accounts: Vec<&str> = parts[2].split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

    let mut count = 0;
    for acc in accounts {
        match db.add_client_account(name, acc) {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => {
                println!("Error adding account {}: {}", acc, e);
                return;
            }
        }
    }
    println!("Added {} accounts to client {}", count, name);
}

fn handle_remove_client_account(db: &mut Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: REMOVE.CLIENT.ACCOUNT <name> <accounts>");
        return;
    }
    let name = parts[1];
    let accounts: Vec<&str> = parts[2].split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

    let mut count = 0;
    for acc in accounts {
        match db.remove_client_account(name, acc) {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => {
                println!("Error removing account {}: {}", acc, e);
                return;
            }
        }
    }
    println!("Removed {} accounts from client {}", count, name);
}

fn handle_deauthorize_conn(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: DEAUTHORIZE.CONN <name>");
        return;
    }
    let name = parts[1];
    match db.remove_authorized_client(name) {
        Ok(true) => println!("Deauthorized client: {}", name),
        Ok(false) => println!("Client not found: {}", name),
        Err(e) => println!("Error deauthorizing: {}", e),
    }
}

fn handle_list_conns(db: &mut Database) {
    println!("{:<20} {:<64}", "Name", "Thumbprint");
    println!("{:-<20} {:-<64}", "", "");

    let _ = db.run_in_system_account(|db| {
        let table = db.get_table_mut("$CLIENTS")?;
        let mut names: Vec<_> = table.records.keys().cloned().collect();
        names.sort();

        for name in names {
            if let Some(record) = table.records.get(&name) {
                let thumbprint = record.fields.get(0)
                    .and_then(|f| f.values.get(0))
                    .and_then(|v| v.sub_values.get(0))
                    .cloned()
                    .unwrap_or_else(|| "N/A".to_string());
                println!("{:<20} {:<64}", name, thumbprint);
            }
        }
        Ok(())
    });
}

fn handle_generate_cert(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: GENERATE.CERT <common_name>");
        return;
    }

    let cn = parts[1];
    // Sanitize common_name to prevent option injection or directory traversal
    if cn.starts_with('-') || cn.contains('/') || cn.contains('\\') || cn.contains("..") {
        println!("Error: Invalid common_name. Must not start with '-' or contain path separators.");
        return;
    }
    let key_file = format!("{}.key", cn);
    let csr_file = format!("{}.csr", cn);
    let crt_file = format!("{}.crt", cn);
    let pfx_file = format!("{}.pfx", cn);
    let ext_file = format!("{}.ext", cn);

    // 1. Generate RSA key
    let status = std::process::Command::new("openssl")
        .args(&["genrsa", "-out", &key_file, "2048"])
        .status();

    if status.is_err() || !status.unwrap().success() {
        println!("Error generating RSA key");
        return;
    }

    // 2. Generate CSR
    let subj = format!("/CN={}", cn);
    let status = std::process::Command::new("openssl")
        .args(&["req", "-new", "-key", &key_file, "-out", &csr_file, "-subj", &subj])
        .status();

    if status.is_err() || !status.unwrap().success() {
        println!("Error generating CSR");
        return;
    }

    // 3. Create extension file for SAN if needed
    let mut san = "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nsubjectAltName = DNS:".to_string();
    san.push_str(cn);
    if cn == "localhost" {
        san.push_str(", IP:127.0.0.1");
    }
    if let Err(e) = std::fs::write(&ext_file, san) {
        println!("Error creating extension file: {}", e);
        return;
    }

    // 4. Sign CSR with system CA
    // Assuming ca.crt and ca.key are in the root directory (as seen in the project root)
    let status = std::process::Command::new("openssl")
        .args(&[
            "x509", "-req",
            "-in", &csr_file,
            "-CA", "ca.crt",
            "-CAkey", "ca.key",
            "-CAcreateserial",
            "-out", &crt_file,
            "-days", "365",
            "-sha256",
            "-extfile", &ext_file
        ])
        .status();

    let _ = std::fs::remove_file(&ext_file);

    if status.is_err() || !status.unwrap().success() {
        println!("Error signing certificate. Ensure ca.crt and ca.key are in the project root.");
        return;
    }

    // 5. Create PFX file
    let status = std::process::Command::new("openssl")
        .args(&[
            "pkcs12", "-export",
            "-out", &pfx_file,
            "-inkey", &key_file,
            "-in", &crt_file,
            "-passout", "pass:"
        ])
        .status();

    if status.is_err() || !status.unwrap().success() {
        println!("Error generating PFX file.");
    }

    // 5. Calculate thumbprint for convenience
    let output = std::process::Command::new("openssl")
        .args(&["x509", "-in", &crt_file, "-fingerprint", "-noout", "-sha256"])
        .output();

    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(thumbprint) = text.split('=').nth(1) {
            let thumbprint = thumbprint.replace(':', "").trim().to_lowercase();
            println!("Certificate generated: {}", crt_file);
            println!("Private key: {}", key_file);
            println!("PFX file: {}", pfx_file);
            println!("SHA-256 Thumbprint: {}", thumbprint);

            // Interactive authorization
            println!("\n--- Connection Authorization ---");
            print!("Enter authorization name [{}]: ", cn);
            io::stdout().flush().unwrap();
            let mut auth_name = String::new();
            io::stdin().read_line(&mut auth_name).unwrap();
            let auth_name = if auth_name.trim().is_empty() { cn.to_string() } else { auth_name.trim().to_string() };

            print!("Is this an ADMIN connection? (Y/N) [N]: ");
            io::stdout().flush().unwrap();
            let mut is_admin_input = String::new();
            io::stdin().read_line(&mut is_admin_input).unwrap();
            let is_admin = is_admin_input.trim().to_uppercase() == "Y";

            let accounts = if is_admin {
                Vec::new()
            } else {
                print!("Enter comma-separated list of allowed accounts: ");
                io::stdout().flush().unwrap();
                let mut accs_input = String::new();
                io::stdin().read_line(&mut accs_input).unwrap();
                accs_input.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };

            if !is_admin && accounts.is_empty() {
                println!("Error: Non-admin connections must have at least one allowed account.");
                println!("Authorization skipped. Use AUTHORIZE.CONN to authorize manually.");
            } else {
                match db.add_authorized_client(&auth_name, &thumbprint, accounts, is_admin) {
                    Ok(_) => {
                        if is_admin {
                            println!("Successfully authorized: {} as {} (ADMIN)", thumbprint, auth_name);
                        } else {
                            println!("Successfully authorized: {} as {}", thumbprint, auth_name);
                        }
                    }
                    Err(e) => println!("Error authorizing: {}", e),
                }
            }
        }
    } else {
        println!("Certificate generated: {}", crt_file);
        println!("Private key: {}", key_file);
        println!("PFX file: {}", pfx_file);
    }
}

fn handle_start_server(db: Arc<Mutex<Database>>, parts: &[&str], config: Arc<Config>) {
    let mut offset = 1;
    let mut addr = "127.0.0.1".to_string();

    // Check if the first part looks like an address/port (contains : or .)
    // but exclude cert/key filenames by checking for common extensions
    if parts.len() > offset {
        let first_arg = parts[offset];
        if first_arg.contains(':') || (first_arg.contains('.') && !first_arg.ends_with(".crt") && !first_arg.ends_with(".key") && !first_arg.ends_with(".pem")) {
            addr = first_arg.to_string();
            offset += 1;
        }
    }

    // Append default port if not specified
    if !addr.contains(':') {
        let port = config.server_port.unwrap_or(8443);
        addr = format!("{}:{}", addr, port);
    }

    if parts.len() < offset + 3 {
        println!("Usage: START.SERVER [<addr:port>] <cert_path> <key_path> <ca_path>");
        println!("Default port: {}", config.server_port.unwrap_or(8443));
        return;
    }

    let _ = parts[offset].to_string();
    let _ = parts[offset + 1].to_string();
    let _ = parts[offset + 2].to_string();

    let addr_clone = addr.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = server::start_server(config, db, Some(addr_clone)).await;
        });
    });
    println!("Server start initiated on {}.", addr);
}

fn check_dir_file(db: &mut Database) -> io::Result<()> {
    match db.ensure_dir_file() {
        Ok(true) => Ok(()),
        Ok(false) => {
            print!("DIR file missing. Create and populate? (Y/N): ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            if choice.trim().to_uppercase() == "Y" {
                db.create_dir_file()?;
                println!("DIR file created and populated.");
            }
            Ok(())
        }
        Err(e) => {
            println!("Error checking DIR file: {}", e);
            Err(e)
        }
    }
}
