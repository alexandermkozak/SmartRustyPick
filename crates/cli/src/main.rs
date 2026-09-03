use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::{Database, Record, SelectEntry, SelectList, ValuePosition, report};
use smart_rusty_pick_core::server;
use std::io::{self, Write};
use std::sync::{Arc, RwLock};

/// Lifetime of a certificate issued by `GENERATE.CERT`, matching the server's.
const CLIENT_CERT_DAYS: u32 = 365;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut initial_account = None;
    if let Some(pos) = args.iter().position(|a| a == "--account")
        && pos + 1 < args.len()
    {
        initial_account = Some(args[pos + 1].clone());
    }

    let mut db_dir = "db_storage".to_string();
    if let Some(pos) = args.iter().position(|a| a == "-d" || a == "--db-dir")
        && pos + 1 < args.len()
    {
        db_dir = args[pos + 1].clone();
    }

    let config = Config::load();
    let config_arc = Arc::new(config.clone());

    // We use a directory "db_storage" to hold our tables
    let db = Arc::new(RwLock::new(Database::new(&db_dir, Some(config.clone()))?));

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
                let addr = config_clone
                    .server_addr
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1".to_string());
                let port = config_clone.server_port.unwrap_or(8443);
                let full_addr = if addr.contains(':') {
                    addr
                } else {
                    format!("{}:{}", addr, port)
                };
                let _ = server::start_server(config_arc_clone, db_clone, Some(full_addr)).await;
            });
        });
        println!("Database service attached and running in background.");
    }

    println!("SmartRustyPick CLI. Type 'HELP' for commands.");

    // Auto-login based on current directory
    let auto_account = {
        let db_lock = db.read().unwrap();
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        db_lock.get_account_for_dir(current_dir.to_str().unwrap_or("."))
    };

    if let Some(account_name) = auto_account {
        let mut db_lock = db.write().unwrap();
        let acc_to_log = account_name.clone();
        if db_lock.logto(&acc_to_log).is_ok() {
            println!(
                "Auto-logged into account '{}' based on current directory.",
                account_name
            );
            let _ = check_dir_file(&mut db_lock);
        }
    }

    // Account login prompt if not logged in
    loop {
        {
            let db_lock = db.read().unwrap();
            if !db_lock.has_no_current_account() {
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

        let mut db_lock = db.write().unwrap();
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
            let db_lock = db.read().unwrap();
            let acc = db_lock.current_account();
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
        if parts.is_empty() {
            continue;
        }
        let command = parts[0].to_uppercase();

        match command.as_str() {
            "SET" => {
                handle_set(&mut db.write().unwrap(), &parts);
            }
            "GET" => {
                handle_get(&mut db.write().unwrap(), &parts);
            }
            "DELETE" => {
                handle_delete(&mut db.write().unwrap(), &parts);
            }
            "LIST" => {
                handle_list(&mut db.write().unwrap(), &parts);
            }
            "SELECT" => {
                handle_select(&mut db.write().unwrap(), &parts);
            }
            "EDIT" => {
                handle_edit(&mut db.write().unwrap(), &parts, &config);
            }
            "CT" => {
                handle_ct(&mut db.write().unwrap(), &parts);
            }
            "SAVE-LIST" => {
                handle_save_list(&mut db.write().unwrap(), &parts);
            }
            "GET-LIST" => {
                handle_get_list(&mut db.write().unwrap(), &parts);
            }
            "CREATE.FILE" => {
                let mut db_lock = db.write().unwrap();
                handle_create_file(&mut db_lock, &parts);
            }
            "SET.FILE" => {
                let mut db_lock = db.write().unwrap();
                handle_set_file(&mut db_lock, &parts);
            }
            "DELETE.FILE" => {
                let mut db_lock = db.write().unwrap();
                handle_delete_file(&mut db_lock, &parts);
            }
            "CREATE.INDEX" => {
                handle_create_index(&db.write().unwrap(), &parts);
            }
            "REBUILD.INDEX" => {
                handle_rebuild_index(&db.write().unwrap(), &parts);
            }
            "DELETE.INDEX" => {
                handle_delete_index(&db.write().unwrap(), &parts);
            }
            "LIST.INDEXES" => {
                handle_list_indexes(&db.write().unwrap(), &parts);
            }
            "INDEX.STATS" => {
                handle_index_stats(&db.write().unwrap(), &parts);
            }
            "SET.INDEX.EXCLUDE" => {
                handle_set_index_exclude(&db.write().unwrap(), &parts);
            }
            "FILE.STATS" => {
                handle_file_stats(&db.write().unwrap(), &parts);
            }
            "CREATE.ACCOUNT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_create_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "CREATE.TEST.ACCOUNT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_create_test_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "DELETE.ACCOUNT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_delete_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "LOGTO" => {
                let mut db_lock = db.write().unwrap();
                handle_logto(&mut db_lock, &parts);
                let _ = check_dir_file(&mut db_lock);
            }
            "LIST.FILES" => {
                handle_list_files(&mut db.write().unwrap());
            }
            "AUTHORIZE.CONN" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_authorize_conn(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "ADD.CLIENT.ACCOUNT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_add_client_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "REMOVE.CLIENT.ACCOUNT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_remove_client_account(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "DEAUTHORIZE.CONN" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_deauthorize_conn(&mut db_lock, &parts);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "LIST.CONNS" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_list_conns(&mut db_lock);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "GENERATE.CERT" => {
                let mut db_lock = db.write().unwrap();
                if db_lock.is_current_account("SYSTEM") {
                    handle_generate_cert(&mut db_lock, &parts, &config);
                } else {
                    println!("Unknown command: {}", command);
                }
            }
            "START.SERVER" => {
                handle_start_server(db.clone(), &parts, config_arc.clone());
            }
            "SAVE" => {
                db.write().unwrap().save()?;
                println!("OK");
            }
            "HELP" => {
                let db_lock = db.read().unwrap();
                print_help(&db_lock.current_account());
            }
            "EXIT" | "QUIT" => break,
            _ => println!("Unknown command: {}", command),
        }
    }

    // Auto-save on exit
    db.write().unwrap().save()?;
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

    let handle = match db.get_table_mut(table_name) {
        Ok(handle) => handle,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    {
        let mut table = handle.write();
        let record = Record::from_display_string(&data);
        if is_dict {
            table.dictionary.insert(key, record);
            table.mark_dict_dirty();
        } else {
            table.insert_record(&key, record);
        }
    }
    // The file's lock goes before the flush: a flush locks each dirty file in
    // turn, and would wait here for a lock this thread is holding.
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
        if let Some(list) = &db.active_select_list
            && list.table_name == table_name
            && list.is_dict == is_dict
        {
            keys_from_list = Some(list.unique_keys());
        }

        if let Some(keys) = keys_from_list {
            if let Some(handle) = db.get_table(table_name) {
                let table = handle.read();
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

    if let Some(handle) = db.get_table(table_name) {
        let table = handle.read();
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
        if let Some(list) = &db.active_select_list
            && list.table_name == table_name
            && list.is_dict == is_dict
        {
            keys_to_delete = list.unique_keys();
            used_list = true;
        }

        if used_list {
            let handle = match db.get_table_mut(table_name) {
                Ok(handle) => handle,
                Err(e) => {
                    println!("Error: {}", e);
                    return;
                }
            };
            let mut table = handle.write();
            let mut count = 0;
            for key in keys_to_delete {
                let removed = if is_dict {
                    table.dictionary.remove(&key).is_some()
                } else {
                    table.remove_record(&key).is_some()
                };
                if removed {
                    count += 1;
                }
            }
            if count > 0 {
                if is_dict {
                    table.mark_dict_dirty();
                }
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

    let handle = match db.get_table_mut(table_name) {
        Ok(handle) => handle,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    let removed = {
        let mut table = handle.write();
        if is_dict {
            let removed = table.dictionary.remove(key).is_some();
            if removed {
                table.mark_dict_dirty();
            }
            removed
        } else {
            table.remove_record(key).is_some()
        }
    };
    if removed {
        // Flushing locks each dirty file in turn, so the file's own lock has to
        // be released before asking for one.
        if table_name == "$CLIENTS" {
            let _ = db.save();
        }
        println!("OK");
    } else {
        println!("NOT FOUND");
    }
}

fn handle_list(db: &mut Database, parts: &[&str]) {
    // LIST [DICT] <table> [<fields>...] [WITH <field> <op> <value>]
    //                     [BY|BY.DSND <field> ...] [BY.EXP <field> [<op> <value>]]
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
    if !db.list_tables().contains(&table_name.to_string()) {
        println!("TABLE NOT FOUND");
        return;
    }

    // Strip the sort and explode clauses; a WITH clause, if any, ends the
    // column list. What is left in front of it are the fields to display.
    let (clause_parts, sort_specs, explode_specs) = Database::parse_clause_specs(&parts[offset + 1..]);
    let explode = match explode_specs.len() {
        0 => None,
        1 => explode_specs.into_iter().next(),
        _ => {
            println!("Only one BY.EXP field may be given");
            return;
        }
    };

    // Columns may sit on either side of the selection clause, so the criteria
    // are cut out of the token list and what is left is the column list.
    let with_at = clause_parts.iter().position(|p| p.to_uppercase() == "WITH");
    let (mut query, field_names) = match with_at {
        None => (None, clause_parts.clone()),
        Some(at) => {
            let (node, consumed) = db.parse_query_consuming(table_name, &clause_parts[at..]);
            if node.is_none() {
                println!("INVALID QUERY FORMAT");
                return;
            }
            let mut columns = clause_parts[..at].to_vec();
            columns.extend_from_slice(&clause_parts[at + consumed..]);
            (node, columns)
        }
    };
    if let Some(condition) = explode.as_ref().and_then(|e| e.condition.clone()) {
        // The compact `BY.EXP ACCOUNTS = "TEST"` filters exactly as the
        // explicit `BY.EXP ACCOUNTS WITH ACCOUNTS = "TEST"` does.
        query = Database::and_condition(query, Some(condition));
    }

    // An active list for this table stands in for a fresh scan, keeping the
    // positions - and the field they belong to - that a preceding
    // SELECT ... BY.EXP recorded.
    let active = db
        .active_select_list
        .as_ref()
        .filter(|l| l.table_name == table_name && l.is_dict == is_dict);
    let from_list = active.map(|l| l.entries.clone());
    let list_explode_field = active.and_then(|l| l.explode_field.clone());
    let use_select_list = from_list.is_some();

    let lines = {
        let account = db.current_account();
        let Ok(handle) = db.get_table_mut_for_account(&account, table_name) else {
            println!("TABLE NOT FOUND");
            return;
        };
        let table = &*handle.read();

        let mut rows: Vec<(SelectEntry, &Record)> = match &from_list {
            // The list already decided which rows exist, positions included;
            // re-running the selection over it would only lose them.
            Some(entries) => {
                let map = if is_dict { &table.dictionary } else { &table.records };
                entries
                    .iter()
                    .filter_map(|e| Some((e.clone(), map.get(&e.key)?)))
                    .collect()
            }
            None => Database::query_exploded_in(table, is_dict, query.as_ref(), explode.as_ref(), None),
        };

        // Rows already arrive in key order, and within a key in value-position
        // order, so an absent sort clause needs nothing further.
        let explode_idx = Database::explode_field_index(table, explode.as_ref());
        Database::sort_entries_in(table, &mut rows, &sort_specs, explode_idx);

        if field_names.is_empty() {
            rows.iter().map(|(entry, _)| entry.key.clone()).collect()
        } else {
            let columns: Vec<String> = field_names.iter().map(|s| s.to_string()).collect();
            let explode_field = explode
                .as_ref()
                .map(|e| e.field_name.clone())
                .or(list_explode_field.clone());
            report::render_list(table, &columns, explode_field.as_deref(), &rows)
        }
    };

    for line in lines {
        println!("{}", line);
    }

    if use_select_list {
        db.active_select_list = None;
    }
}

fn handle_select(db: &mut Database, parts: &[&str]) {
    // SELECT [DICT] <table> [WITH <field> <op> <value>] [BY|BY.DSND <field> ...]
    // e.g. SELECT USERS WITH First.Name = "Ted"
    // e.g. SELECT PRODUCTS WITH DESC = "[new]" BY PRICE BY.DSND CREATE.DATE
    let mut offset = 1;
    let is_dict = if parts.len() > offset && parts[offset].to_uppercase() == "DICT" {
        offset += 1;
        true
    } else {
        false
    };

    if parts.len() < offset + 1 {
        println!("Usage: SELECT [DICT] <table> [WITH <field> <op> <value>] [BY|BY.DSND <field> ...]");
        return;
    }

    let table_name = parts[offset];

    // Check if we should refine the active select list
    let keys_to_filter = if let Some(list) = &db.active_select_list {
        if list.table_name == table_name && list.is_dict == is_dict {
            Some(list.unique_keys())
        } else {
            None
        }
    } else {
        None
    };

    // Strip the sort and explode clauses before parsing the selection criteria.
    let (clause_parts, sort_specs, explode_specs) = Database::parse_clause_specs(&parts[offset + 1..]);
    let explode = match explode_specs.len() {
        0 => None,
        1 => explode_specs.into_iter().next(),
        _ => {
            println!("Only one BY.EXP field may be given");
            return;
        }
    };

    let mut query = if clause_parts.is_empty() {
        None
    } else if clause_parts[0].to_uppercase() == "WITH" {
        match db.parse_query(table_name, &clause_parts) {
            Some(q) => Some(q),
            None => {
                println!("INVALID QUERY FORMAT");
                return;
            }
        }
    } else {
        println!(
            "Usage: SELECT [DICT] <table> [WITH <field> <op> <value>] [BY|BY.DSND <field> ...] [BY.EXP <field> [<op> <value>]]"
        );
        return;
    };
    if let Some(condition) = explode.as_ref().and_then(|e| e.condition.clone()) {
        query = Database::and_condition(query, Some(condition));
    }

    if !db.list_tables().contains(&table_name.to_string()) {
        println!("TABLE NOT FOUND");
        return;
    }

    let entries: Vec<SelectEntry> = {
        let account = db.current_account();
        let Ok(handle) = db.get_table_mut_for_account(&account, table_name) else {
            println!("TABLE NOT FOUND");
            return;
        };
        Database::select_entries_in(
            &handle.read(),
            is_dict,
            query.as_ref(),
            explode.as_ref(),
            keys_to_filter.as_deref(),
            &sort_specs,
        )
    };

    if entries.is_empty() {
        println!("NO RECORDS FOUND");
        db.active_select_list = None;
    } else {
        println!("[{}] records selected", entries.len());
        db.active_select_list = Some(SelectList {
            table_name: table_name.to_string(),
            is_dict,
            explode_field: explode.map(|e| e.field_name),
            entries,
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
    let current_content = if let Some(handle) = db.get_table(table_name) {
        let table = handle.read();
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
    let editor = config
        .editor
        .clone()
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
                    let handle = match db.get_table_mut(table_name) {
                        Ok(handle) => handle,
                        Err(e) => {
                            println!("Error: {}", e);
                            return;
                        }
                    };
                    {
                        let mut table = handle.write();
                        let record = Record::from_edit_string(&new_content);
                        let key_str = key.to_string();
                        if is_dict {
                            table.dictionary.insert(key_str, record);
                            table.mark_dict_dirty();
                        } else {
                            table.insert_record(&key_str, record);
                        }
                    }
                    // Released before the flush: a flush wants this file's lock too.
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
        if let Some(list) = &db.active_select_list
            && list.table_name == table_name
            && list.is_dict == is_dict
        {
            keys_from_list = Some(list.unique_keys());
        }

        if let Some(keys) = keys_from_list {
            if let Some(handle) = db.get_table(table_name) {
                let table = handle.read();
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

    if let Some(handle) = db.get_table(table_name) {
        let table = handle.read();
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
            if j > 0 {
                res.push(smart_rusty_pick_core::db::VM);
            }
            for (k, sv) in v.sub_values.iter().enumerate() {
                if k > 0 {
                    res.push(smart_rusty_pick_core::db::SVM);
                }
                res.extend_from_slice(sv.as_bytes());
            }
        }
        let display_bytes: Vec<u8> = res
            .iter()
            .map(|&b| match b {
                smart_rusty_pick_core::db::VM => b']',
                smart_rusty_pick_core::db::SVM => b'\\',
                _ => b,
            })
            .collect();
        println!("{:03} {}", i + 1, String::from_utf8_lossy(&display_bytes));
    }
}

fn print_help(current_account: &str) {
    println!("Commands:");
    println!("  SET [DICT] <table> <key> <data>       - Store a record.");
    println!("  GET [DICT] <table> [<key>]             - Retrieve record(s). Uses SELECT list if key omitted.");
    println!("  DELETE [DICT] <table> [<key>]          - Remove record(s). Uses SELECT list if key omitted.");
    println!(
        "  LIST [DICT] [<table> [<fields>...]]   - List tables, keys, or records. Uses SELECT list if applicable."
    );
    println!("  SELECT [DICT] <table> [WITH <field> <op> <value>] - Create/refine active select list.");
    println!("    Operators: =, #, <>, <, >, <=, >=, EQ, NE, LT, GT, LE, GE");
    println!("    Wildcards (with = or #): [value (ends with), value] (starts with), [value] (contains)");
    println!("    Selection (LIST and SELECT): WITH <field> <op> <value> [AND|OR ...]");
    println!("    Sorting (LIST and SELECT): BY <field> (ascending), BY.DSND <field> (descending)");
    println!("      Any number may be given; they are applied from left to right.");
    println!("      Sort operators and column names may appear in any order.");
    println!("      e.g. SELECT PRODUCTS WITH DESC = \"[new]\" BY PRICE BY.DSND CREATE.DATE");
    println!("    Multivalue (LIST and SELECT): BY.EXP <field> [<op> <value>]");
    println!("      Gives each value of a multivalued field its own row. With a");
    println!("      criterion, only the values that satisfied it are shown, and a");
    println!("      following LIST of the same file keeps them.");
    println!("      e.g. LIST $CLIENTS BY.EXP ACCOUNTS = \"TEST\" ACCOUNTS");
    println!("  EDIT [DICT] <table> <key>             - Edit a record using external editor.");
    println!(
        "  CT [DICT] <table> [<key>]             - Print record contents, field by field. Uses SELECT list if key omitted."
    );
    println!("  SAVE                                  - Save database to disk.");
    println!("  HELP                                  - Show this help.");
    println!("  SAVE-LIST <name>                      - Save active select list.");
    println!("  GET-LIST <name>                       - Restore a saved select list.");
    println!("  CREATE.FILE <name> [DURABLE]          - Create a new file (data and dict) (SYSTEM only).");
    println!("                                          DURABLE flushes every write to that file immediately.");
    println!("  SET.FILE <name> DURABLE | BUFFERED    - Turn durable writes on or off for an existing file.");
    println!("                                          Turning it on flushes what the file still had buffered.");
    println!("  DELETE.FILE <name>                    - Delete a file (data and dict) (SYSTEM only).");
    println!("  CREATE.INDEX <file> <field> [EXCLUDE <value>...] - Index a dictionary field, so WITH <field> = ...");
    println!("                                          stops scanning. EXCLUDE names values not worth indexing.");
    println!("  LIST.INDEXES [<file>]                 - List a file's indexes, or every index in the account.");
    println!("  INDEX.STATS <file> <field> [<n>]      - One index in full: its verdicts and its commonest values.");
    println!("  SET.INDEX.EXCLUDE <file> <field> [<value>...] - Replace the values an index skips, and rebuild it.");
    println!("                                          With no values, the exclusions are cleared.");
    println!("  REBUILD.INDEX <file> <field>          - Derive an index from the records again.");
    println!("  DELETE.INDEX <file> <field>           - Drop an index and remove its section.");
    println!("  FILE.STATS <file>                     - A file's layout, how healthy it is and what to do about it.");
    println!("  CREATE.ACCOUNT <name> [<dir>]         - Create a new account (SYSTEM only).");
    if current_account == "SYSTEM" {
        println!("  CREATE.TEST.ACCOUNT <name>            - Create and populate a test account (SYSTEM only).");
    }
    println!("  DELETE.ACCOUNT <name>                 - Delete an account and all its files (SYSTEM only).");
    println!("  LOGTO <name>                          - Switch to a different account.");
    println!("  LIST.FILES                            - List all files in the current account, with their durability.");
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

    // Field 1 is the file, field 2 the dict flag (and the exploded field, if
    // any), and one field per entry:
    // `key`, or `key]value` / `key]value]sub_value` for an exploded list. A
    // list saved before positions existed has no value mark in its key fields,
    // so it still loads as a plain list of keys.
    let mut data = Vec::new();
    data.extend_from_slice(list.table_name.as_bytes());
    data.push(smart_rusty_pick_core::db::FM);
    data.extend_from_slice(if list.is_dict { b"1" } else { b"0" });
    if let Some(field) = &list.explode_field {
        data.push(smart_rusty_pick_core::db::VM);
        data.extend_from_slice(field.as_bytes());
    }
    for entry in &list.entries {
        data.push(smart_rusty_pick_core::db::FM);
        data.extend_from_slice(entry.key.as_bytes());
        if let Some(pos) = entry.position {
            data.push(smart_rusty_pick_core::db::VM);
            data.extend_from_slice(pos.value.to_string().as_bytes());
            if let Some(sub) = pos.sub_value {
                data.push(smart_rusty_pick_core::db::VM);
                data.extend_from_slice(sub.to_string().as_bytes());
            }
        }
    }

    let record = Record::from_bytes(&data);
    // `$SAVEDLISTS` lives in SYSTEM, so this is a missing file rather than an
    // impossibility - report it instead of taking the whole CLI down.
    let handle = match db.get_table_mut("$SAVEDLISTS") {
        Ok(handle) => handle,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    handle.write().insert_record(list_name, record);

    db.active_select_list = None;
    println!("List '{}' saved", list_name);
}

/// Reads one saved-list field: `key`, `key]value`, or `key]value]sub_value`.
/// A position that is not a number is discarded rather than rejecting the whole
/// list - the key is the part that matters.
fn parse_saved_entry(field: &[u8]) -> SelectEntry {
    let mut parts = field.split(|&b| b == smart_rusty_pick_core::db::VM);
    let key = String::from_utf8_lossy(parts.next().unwrap_or(b"")).to_string();
    let number = |p: Option<&[u8]>| p.and_then(|b| String::from_utf8_lossy(b).parse::<usize>().ok());

    match number(parts.next()) {
        Some(value) => SelectEntry::at(
            key,
            ValuePosition {
                value,
                sub_value: number(parts.next()),
            },
        ),
        None => SelectEntry::new(key),
    }
}

fn handle_get_list(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: GET-LIST <list_name>");
        return;
    }

    let list_name = parts[1];

    let handle = match db.get_table_mut("$SAVEDLISTS") {
        Ok(handle) => handle,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    let table = handle.read();
    if let Some(record) = table.records.get(list_name) {
        let data = record.to_bytes();
        let fields: Vec<&[u8]> = data.split(|&b| b == smart_rusty_pick_core::db::FM).collect();

        if fields.len() < 2 {
            println!("INVALID SAVED LIST FORMAT");
            return;
        }

        let table_name = String::from_utf8_lossy(fields[0]).to_string();
        let mut flags = fields[1].splitn(2, |&b| b == smart_rusty_pick_core::db::VM);
        let is_dict = flags.next() == Some(b"1");
        let explode_field = flags.next().map(|f| String::from_utf8_lossy(f).to_string());
        let entries: Vec<SelectEntry> = fields[2..].iter().map(|f| parse_saved_entry(f)).collect();
        let count = entries.len();

        db.active_select_list = Some(SelectList {
            table_name,
            is_dict,
            explode_field,
            entries,
        });
        println!("[{}] records retrieved", count);
    } else {
        println!("LIST '{}' NOT FOUND", list_name);
    }
}

fn handle_create_file(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: CREATE.FILE <file_name> [DURABLE]");
        return;
    }
    let file_name = parts[1];
    let mut durable = false;
    for flag in &parts[2..] {
        match flag.to_uppercase().as_str() {
            "DURABLE" | "-D" => durable = true,
            other => {
                println!("Unknown option '{}'. Usage: CREATE.FILE <file_name> [DURABLE]", other);
                return;
            }
        }
    }
    match db.create_table_durable(file_name, durable) {
        Ok(_) => {
            if durable {
                println!("[{}] created (data and dict, durable writes)", file_name);
            } else {
                println!("[{}] created (data and dict)", file_name);
            }
        }
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_set_file(db: &mut Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: SET.FILE <file_name> DURABLE | BUFFERED");
        return;
    }
    let file_name = parts[1];
    let durable = match parts[2].to_uppercase().as_str() {
        "DURABLE" | "-D" => true,
        "BUFFERED" | "NODURABLE" | "-B" => false,
        other => {
            println!(
                "Unknown option '{}'. Usage: SET.FILE <file_name> DURABLE | BUFFERED",
                other
            );
            return;
        }
    };
    match db.set_table_durable(file_name, durable) {
        Ok(_) => {
            if durable {
                println!("[{}] now flushes every write before acknowledging it", file_name);
            } else {
                println!("[{}] now follows the database's buffering policy", file_name);
            }
        }
        Err(e) => println!("Error: {}", e),
    }
}

/// Prints one index the way `LIST.INDEXES` does, so creating one and listing it
/// report the same numbers in the same shape.
///
/// "per lookup" is the postings divided by the values: how many records a
/// lookup on this field hands back to the filter behind it, on average. It is
/// the number that says whether the index is worth what it costs, and it is
/// not records-per-value - on a multivalued field one record contributes
/// several postings.
fn print_index(stats: &smart_rusty_pick_core::db::IndexStats) {
    let per_lookup = if stats.values == 0 {
        0.0
    } else {
        stats.postings as f64 / stats.values as f64
    };
    println!(
        "  {:<16} attribute {:<4} {} values, {} keys, largest {}, {:.1} per lookup, {} lookups served [{}]{}",
        stats.field,
        stats.attribute,
        stats.values,
        stats.postings,
        stats.largest_postings,
        per_lookup,
        stats.usage.lookups,
        stats.health.verdict,
        if stats.stale { " (STALE - rebuild it)" } else { "" },
    );
}

/// The values after an `EXCLUDE` keyword, and whether one was given at all.
///
/// Quoted values are unwrapped so `EXCLUDE ""` means the empty value - which is
/// the commonest exclusion there is, a sparse field most records do not carry,
/// and is unspellable otherwise.
fn exclusions_after(parts: &[&str], from: usize) -> Vec<String> {
    let Some(at) = parts[from..]
        .iter()
        .position(|part| part.eq_ignore_ascii_case("EXCLUDE"))
    else {
        return Vec::new();
    };
    parts[from + at + 1..].iter().map(|part| unquote(part)).collect()
}

/// Strips one layer of matching quotes, so `""` is the empty value rather than
/// two characters.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn handle_create_index(db: &Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: CREATE.INDEX <file_name> <field_name> [EXCLUDE <value>...]");
        return;
    }
    let account = db.current_account();
    let exclude = exclusions_after(parts, 3);
    match db.create_index_excluding(&account, parts[1], parts[2], &exclude) {
        Ok(stats) => {
            println!("[{}] indexed on {}", parts[1], stats.field);
            print_index(&stats);
        }
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_set_index_exclude(db: &Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: SET.INDEX.EXCLUDE <file_name> <field_name> [<value>...]");
        println!("  With no values, the index stops excluding anything.");
        return;
    }
    let account = db.current_account();
    let values: Vec<String> = parts[3..].iter().map(|part| unquote(part)).collect();
    match db.set_index_exclusions(&account, parts[1], parts[2], &values) {
        Ok(stats) => {
            if stats.excluded.is_empty() {
                println!("[{}] index on {} excludes nothing", parts[1], stats.field);
            } else {
                println!(
                    "[{}] index on {} now excludes {}",
                    parts[1],
                    stats.field,
                    describe_values(&stats.excluded)
                );
            }
            print_index(&stats);
        }
        Err(e) => println!("Error: {}", e),
    }
}

/// Values as prose, naming the empty one rather than printing nothing for it.
fn describe_values(values: &[String]) -> String {
    values
        .iter()
        .map(|value| {
            if value.is_empty() {
                "\"\" (the empty value)".to_string()
            } else {
                format!("\"{}\"", value)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn handle_index_stats(db: &Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: INDEX.STATS <file_name> <field_name> [<how_many_values>]");
        return;
    }
    let account = db.current_account();
    let limit = parts
        .get(3)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(smart_rusty_pick_core::db::health::thresholds::HISTOGRAM_DEFAULT);
    let report = match db.index_report(&account, parts[1], parts[2], limit) {
        Ok(report) => report,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    println!(
        "Index on {} of [{}] - {} records in the file",
        report.index.field, report.index.file, report.record_count
    );
    print_index(&report.index);
    if !report.index.excluded.is_empty() {
        println!("  excludes {}", describe_values(&report.index.excluded));
    }
    println!();
    print_health(&report.index.health);

    if !report.values_available {
        println!();
        println!("  The values could not be read: the index is stale or its section would not load.");
        return;
    }
    if report.top_values.is_empty() {
        return;
    }
    println!();
    println!("Commonest values:");
    for value in &report.top_values {
        let share = if report.record_count == 0 {
            0.0
        } else {
            value.keys as f64 / report.record_count as f64
        };
        println!(
            "  {:<32} {:>8} keys  {:>5}",
            if value.value.is_empty() {
                "\"\" (the empty value)".to_string()
            } else {
                value.value.clone()
            },
            value.keys,
            smart_rusty_pick_core::db::health::percent(share),
        );
    }
}

/// A set of verdicts, worst first, in the shape the dashboard renders them.
///
/// The verdicts themselves come from the engine, never from here: the CLI, the
/// remote protocol and the browser describe the same file, and three copies of
/// "5% is the line" is three chances to disagree.
fn print_health(health: &smart_rusty_pick_core::db::Health) {
    use smart_rusty_pick_core::db::Verdict;
    println!("Health: {}", health.verdict.as_str().to_uppercase());
    let mut measures: Vec<_> = health.measures.iter().collect();
    measures.sort_by(|a, b| b.verdict.cmp(&a.verdict));
    for measure in measures {
        let mark = match measure.verdict {
            Verdict::Good => "  ok  ",
            Verdict::Watch => " watch",
            Verdict::Act => " ACT  ",
        };
        println!(
            "{} {:<22} {:>12}   {}",
            mark, measure.label, measure.value, measure.detail
        );
        if measure.verdict != Verdict::Good {
            println!("         threshold: {}", measure.threshold);
        }
    }
}

fn handle_file_stats(db: &Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: FILE.STATS <file_name>");
        return;
    }
    let account = db.current_account();
    let stats = match db.file_statistics(&account, parts[1]) {
        Ok(stats) => stats,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    let groups = &stats.group_records;
    println!("[{}/{}]", stats.account, stats.name);
    println!(
        "  {} records, {} dictionary entries, {} on disk",
        stats.record_count,
        stats.dict_count,
        smart_rusty_pick_core::db::health::bytes(stats.disk_bytes),
    );
    println!(
        "  modulus {} over {} groups; records per group min {} / median {} / mean {} / max {}",
        stats.modulus,
        stats.group_count,
        groups.min,
        groups.median,
        smart_rusty_pick_core::db::health::ratio(groups.mean),
        groups.max,
    );
    println!();
    print_health(&stats.health);
    if !stats.indexes.is_empty() {
        println!();
        println!("Indexes:");
        for index in &stats.indexes {
            print_index(index);
        }
    }
}

fn handle_rebuild_index(db: &Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: REBUILD.INDEX <file_name> <field_name>");
        return;
    }
    let account = db.current_account();
    match db.rebuild_index_for_account(&account, parts[1], parts[2]) {
        Ok(stats) => {
            println!("[{}] index on {} rebuilt", parts[1], stats.field);
            print_index(&stats);
        }
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_delete_index(db: &Database, parts: &[&str]) {
    if parts.len() < 3 {
        println!("Usage: DELETE.INDEX <file_name> <field_name>");
        return;
    }
    let account = db.current_account();
    match db.drop_index_for_account(&account, parts[1], parts[2]) {
        Ok(()) => println!("[{}] index on {} deleted", parts[1], parts[2]),
        Err(e) => println!("Error: {}", e),
    }
}

fn handle_list_indexes(db: &Database, parts: &[&str]) {
    let account = db.current_account();
    // No file named: every index in the account, so a database with forty files
    // can be asked "which of these is worth my attention" in one command.
    if parts.len() < 2 {
        match db.index_statistics_for_account(&account) {
            Ok(indexes) if indexes.is_empty() => println!("No indexes in {}.", account),
            Ok(indexes) => {
                println!("Indexes in {}:", account);
                for (file, stats) in &indexes {
                    println!("  [{}]", file);
                    print_index(stats);
                }
            }
            Err(e) => println!("Error: {}", e),
        }
        return;
    }
    let indexes = match db.index_statistics(&account, parts[1]) {
        Ok(indexes) => indexes,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };
    if indexes.is_empty() {
        println!("[{}] has no indexes.", parts[1]);
        return;
    }
    // The file's record count is what a value count has to be read against: an
    // index over four values is excellent on a file of four thousand records
    // and pointless on one of six.
    match db.file_statistics(&account, parts[1]) {
        Ok(file) => println!("Indexes on [{}] ({} records):", parts[1], file.record_count),
        Err(_) => println!("Indexes on [{}]:", parts[1]),
    }
    for stats in &indexes {
        print_index(stats);
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
    if !db.is_current_account("SYSTEM") {
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
    if db.has_no_current_account() {
        println!("Error: Not logged into an account");
        return;
    }

    // Collected first: reading each file's durability needs the database again,
    // and the DIR table is borrowed from it here.
    let listed: Vec<String> = match db.get_table("DIR") {
        Some(handle) => {
            let table = handle.read();
            let mut files: Vec<_> = table
                .records
                .iter()
                .filter(|(_, record)| {
                    record
                        .fields
                        .first()
                        .and_then(|f| f.values.first())
                        .and_then(|v| v.sub_values.first())
                        .map(|s| s.as_str())
                        .unwrap_or("")
                        == "F"
                })
                .map(|(name, _)| name.clone())
                .collect();
            files.sort();
            files
        }
        None => {
            println!("Error: DIR file not found. Use LOGTO or check account.");
            return;
        }
    };

    println!("{:<20} {:<10} {:<10}", "File", "Type", "Durable");
    println!("{:-<20} {:-<10} {:-<10}", "", "", "");

    // Asked per file rather than read off the DIR record: a database running
    // with durable_writes makes every file durable, and a listing that said
    // otherwise would be describing the DIR entry rather than what a write does.
    for name in listed {
        let durable = if db.is_table_durable(&name) { "yes" } else { "no" };
        println!("{:<20} {:<10} {:<10}", name, "F", durable);
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
        (
            false,
            arg3.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
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
    let accounts: Vec<&str> = parts[2]
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

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
    let accounts: Vec<&str> = parts[2]
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

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
        let handle = db.get_table_mut("$CLIENTS")?;
        let table = handle.read();
        let mut names: Vec<_> = table.records.keys().cloned().collect();
        names.sort();

        for name in names {
            if let Some(record) = table.records.get(&name) {
                let thumbprint = record
                    .fields
                    .first()
                    .and_then(|f| f.values.first())
                    .and_then(|v| v.sub_values.first())
                    .cloned()
                    .unwrap_or_else(|| "N/A".to_string());
                println!("{:<20} {:<64}", name, thumbprint);
            }
        }
        Ok(())
    });
}

fn handle_generate_cert(db: &mut Database, parts: &[&str], config: &Config) {
    if parts.len() < 2 {
        println!("Usage: GENERATE.CERT <common_name>");
        return;
    }

    // Issuing is shared with the server's `GENERATE.CERT` command, so a
    // certificate made here and one made from the dashboard are the same
    // certificate, signed the same way and named the same way.
    let cn = parts[1];
    let generated = match server::certs::generate_client_cert(config, cn, CLIENT_CERT_DAYS, true) {
        Ok(generated) => generated,
        Err(e) => {
            println!("Error generating certificate: {}", e);
            return;
        }
    };

    println!("Certificate generated: {}", generated.cert_path);
    println!("Private key: {}", generated.key_path);
    match &generated.pfx_path {
        Some(path) => println!("PFX file: {}", path),
        None => println!("PFX file: not generated"),
    }
    println!("SHA-256 Thumbprint: {}", generated.thumbprint);

    // Interactive authorization
    println!("\n--- Connection Authorization ---");
    print!("Enter authorization name [{}]: ", cn);
    io::stdout().flush().unwrap();
    let mut auth_name = String::new();
    io::stdin().read_line(&mut auth_name).unwrap();
    let auth_name = if auth_name.trim().is_empty() {
        cn.to_string()
    } else {
        auth_name.trim().to_string()
    };

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
        accs_input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if !is_admin && accounts.is_empty() {
        println!("Error: Non-admin connections must have at least one allowed account.");
        println!("Authorization skipped. Use AUTHORIZE.CONN to authorize manually.");
        return;
    }

    match db.add_authorized_client(&auth_name, &generated.thumbprint, accounts, is_admin) {
        Ok(_) => {
            if is_admin {
                println!(
                    "Successfully authorized: {} as {} (ADMIN)",
                    generated.thumbprint, auth_name
                );
            } else {
                println!("Successfully authorized: {} as {}", generated.thumbprint, auth_name);
            }
        }
        Err(e) => println!("Error authorizing: {}", e),
    }
}

fn handle_start_server(db: Arc<RwLock<Database>>, parts: &[&str], config: Arc<Config>) {
    let mut offset = 1;
    let mut addr = "127.0.0.1".to_string();

    // Check if the first part looks like an address/port (contains : or .)
    // but exclude cert/key filenames by checking for common extensions
    if parts.len() > offset {
        let first_arg = parts[offset];
        if first_arg.contains(':')
            || (first_arg.contains('.')
                && !first_arg.ends_with(".crt")
                && !first_arg.ends_with(".key")
                && !first_arg.ends_with(".pem"))
        {
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

    let cert_path = parts[offset].to_string();
    let key_path = parts[offset + 1].to_string();
    let ca_path = parts[offset + 2].to_string();

    // Explicit paths are trusted to be exact: silently falling back to generating
    // fresh material at a mistyped path would hide the typo instead of failing on it.
    for (label, path) in [
        ("Certificate", &cert_path),
        ("Key", &key_path),
        ("CA certificate", &ca_path),
    ] {
        if !std::path::Path::new(path).exists() {
            println!("Error: {} file '{}' does not exist.", label, path);
            return;
        }
    }

    // Every other setting (ports, durability, dashboard, ...) still comes from
    // config.toml; only the TLS material is overridden by what was typed.
    let server_config = Arc::new(Config {
        cert_path: Some(cert_path),
        key_path: Some(key_path),
        ca_path: Some(ca_path),
        ..(*config).clone()
    });

    let addr_clone = addr.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = server::start_server(server_config, db, Some(addr_clone)).await;
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
            Err(e.into())
        }
    }
}
