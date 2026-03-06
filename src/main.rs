mod db;
mod config;
mod server;

use config::Config;
use db::{Database, Record};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

fn main() -> io::Result<()> {
    let config = Config::load();
    
    // We use a directory "db_storage" to hold our tables
    let db = Arc::new(Mutex::new(Database::new("db_storage")?));
    println!("SmartRustyPick CLI. Type 'HELP' for commands.");

    // Account login prompt
    loop {
        print!("Account: ");
        io::stdout().flush()?;
        let mut account_input = String::new();
        if io::stdin().read_line(&mut account_input)? == 0 {
            return Ok(());
        }
        let account_name = account_input.trim();
        if account_name.is_empty() {
            continue;
        }

        let mut db_lock = db.lock().unwrap();
        if let Err(_) = db_lock.logto(account_name) {
            println!("Account '{}' not found. Create it? (Y/N)", account_name);
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            if choice.trim().to_uppercase() == "Y" {
                db_lock.create_account(account_name, None)?;
                db_lock.logto(account_name)?;
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
                handle_create_file(&mut db.lock().unwrap(), &parts);
            }
            "DELETE.FILE" => {
                handle_delete_file(&mut db.lock().unwrap(), &parts);
            }
            "CREATE.ACCOUNT" => {
                handle_create_account(&mut db.lock().unwrap(), &parts);
            }
            "DELETE.ACCOUNT" => {
                handle_delete_account(&mut db.lock().unwrap(), &parts);
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
                handle_authorize_conn(&mut db.lock().unwrap(), &parts);
            }
            "DEAUTHORIZE.CONN" => {
                handle_deauthorize_conn(&mut db.lock().unwrap(), &parts);
            }
            "LIST.CONNS" => {
                handle_list_conns(&db.lock().unwrap());
            }
            "START.SERVER" => {
                handle_start_server(db.clone(), &parts, &config);
            }
            "SAVE" => {
                db.lock().unwrap().save()?;
                println!("OK");
            }
            "HELP" => {
                print_help();
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
    
    let table = db.get_table_mut(table_name);
    let record = Record::from_display_string(&data);
    if is_dict {
        table.dictionary.insert(key, record);
    } else {
        table.records.insert(key, record);
    }
    table.dirty = true;
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
            let table = db.get_table_mut(table_name);
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

    let table = db.get_table_mut(table_name);
    let map = if is_dict { &mut table.dictionary } else { &mut table.records };
    if map.remove(key).is_some() {
        table.dirty = true;
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

    if field_names.is_empty() {
        if let Some(table) = db.get_table(table_name) {
            let map = if is_dict { &table.dictionary } else { &table.records };
            let keys = if use_select_list {
                selected_keys
            } else {
                let mut k: Vec<_> = map.keys().cloned().collect();
                k.sort();
                k
            };
            for key in keys {
                println!("{}", key);
            }
        } else {
            println!("TABLE NOT FOUND");
        }
    } else {
        // Resolve field names to indices
        let mut field_indices = Vec::new();
        let mut conversion_codes = Vec::new();
        for name in field_names {
            field_indices.push(db.get_field_index(table_name, name));
            conversion_codes.push(db.get_conversion_code(table_name, name));
        }

        if let Some(table) = db.get_table(table_name) {
            let map = if is_dict { &table.dictionary } else { &table.records };
            let keys = if use_select_list {
                selected_keys
            } else {
                let mut k: Vec<_> = map.keys().cloned().collect();
                k.sort();
                k
            };
            for key in keys {
                if let Some(record) = map.get(&key) {
                    let mut line = key.clone();
                    for (i, opt_idx) in field_indices.iter().enumerate() {
                        line.push(' ');
                        if let Some(idx) = *opt_idx {
                            let raw_val = record.get_field_display_string(idx);
                            let formatted_val = if let Some(code) = &conversion_codes[i] {
                                Database::apply_conversion(&raw_val, code)
                            } else {
                                raw_val
                            };
                            line.push_str(&formatted_val);
                        }
                    }
                    println!("{}", line);
                }
            }
        } else {
            println!("TABLE NOT FOUND");
        }
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
            db.query_new(table_name, is_dict, &query, keys_to_filter.as_deref())
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
        db.active_select_list = Some(db::SelectList {
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
                    let table = db.get_table_mut(table_name);
                    let record = Record::from_edit_string(&new_content);
                    let key_str = key.to_string();
                    if is_dict {
                        table.dictionary.insert(key_str, record);
                    } else {
                        table.records.insert(key_str, record);
                    }
                    table.dirty = true;
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
            if j > 0 { res.push(db::VM); }
            for (k, sv) in v.sub_values.iter().enumerate() {
                if k > 0 { res.push(db::SVM); }
                res.extend_from_slice(sv.as_bytes());
            }
        }
        let display_bytes: Vec<u8> = res.iter().map(|&b| match b {
            db::VM => b']',
            db::SVM => b'\\',
            _ => b
        }).collect();
        println!("{:03} {}", i + 1, String::from_utf8_lossy(&display_bytes));
    }
}

fn print_help() {
    println!("Commands:");
    println!("  SET [DICT] <table> <key> <data>       - Store a record.");
    println!("  GET [DICT] <table> [<key>]             - Retrieve record(s). Uses SELECT list if key omitted.");
    println!("  DELETE [DICT] <table> [<key>]          - Remove record(s). Uses SELECT list if key omitted.");
    println!("  LIST [DICT] [<table> [<fields>...]]   - List tables, keys, or records. Uses SELECT list if applicable.");
    println!("  SELECT [DICT] <table> [WITH <field> <op> <value>] - Create/refine active select list.");
    println!("    Operators: =, #, <, >, <=, >=, [ (ends with), ] (starts with), [] (contains)");
    println!("    Wildcards in value with = or #: [ (ends with), ] (starts with), [ ] (contains)");
    println!("  EDIT [DICT] <table> <key>             - Edit a record using external editor.");
    println!("  CT [DICT] <table> [<key>]             - Print record contents, field by field. Uses SELECT list if key omitted.");
    println!("  SAVE                                  - Save database to disk.");
    println!("  HELP                                  - Show this help.");
    println!("  SAVE-LIST <name>                      - Save active select list.");
    println!("  GET-LIST <name>                       - Restore a saved select list.");
    println!("  CREATE.FILE <name>                    - Create a new file (data and dict).");
    println!("  DELETE.FILE <name>                    - Delete a file (data and dict).");
    println!("  CREATE.ACCOUNT <name> [<dir>]         - Create a new account.");
    println!("  DELETE.ACCOUNT <name>                 - Delete an account and all its files.");
    println!("  LOGTO <name>                          - Switch to a different account.");
    println!("  LIST.FILES                            - List all files in the current account.");
    println!("  AUTHORIZE.CONN <thumbprint>           - Authorize an SSL cert thumbprint.");
    println!("  DEAUTHORIZE.CONN <thumbprint>         - Deauthorize an SSL cert thumbprint.");
    println!("  LIST.CONNS                            - List authorized thumbprints.");
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
    data.push(db::FM);
    data.extend_from_slice(if list.is_dict { b"1" } else { b"0" });
    for key in &list.keys {
        data.push(db::FM);
        data.extend_from_slice(key.as_bytes());
    }
    
    let record = Record::from_bytes(&data);
    let table = db.get_table_mut("$SAVEDLISTS");
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
    
    let table = db.get_table_mut("$SAVEDLISTS");
    if let Some(record) = table.records.get(list_name) {
        let data = record.to_bytes();
        let fields: Vec<&[u8]> = data.split(|&b| b == db::FM).collect();
        
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
        
        db.active_select_list = Some(db::SelectList {
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
    if parts.len() < 2 {
        println!("Usage: AUTHORIZE.CONN <thumbprint>");
        return;
    }
    let thumbprint = parts[1].to_lowercase();
    db.authorized_certs.insert(thumbprint.clone());
    let _ = db.save_certs();
    println!("Authorized: {}", thumbprint);
}

fn handle_deauthorize_conn(db: &mut Database, parts: &[&str]) {
    if parts.len() < 2 {
        println!("Usage: DEAUTHORIZE.CONN <thumbprint>");
        return;
    }
    let thumbprint = parts[1].to_lowercase();
    if db.authorized_certs.remove(&thumbprint) {
        let _ = db.save_certs();
        println!("Deauthorized: {}", thumbprint);
    } else {
        println!("Not found: {}", thumbprint);
    }
}

fn handle_list_conns(db: &Database) {
    println!("Authorized Connection Thumbprints:");
    for thumbprint in &db.authorized_certs {
        println!("  {}", thumbprint);
    }
}

fn handle_start_server(db: Arc<Mutex<Database>>, parts: &[&str], config: &Config) {
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

    let cert_path = parts[offset].to_string();
    let key_path = parts[offset + 1].to_string();
    let ca_path = parts[offset + 2].to_string();

    let addr_clone = addr.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            if let Err(e) = server::run_server(&addr_clone, db, &cert_path, &key_path, &ca_path).await {
                eprintln!("Server error: {}", e);
            }
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
