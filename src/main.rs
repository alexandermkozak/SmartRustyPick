mod db;
mod config;

use config::Config;
use db::{Database, Record};
use std::io::{self, Write};

fn main() -> io::Result<()> {
    // Load config
    let config = Config::load();
    
    // We use a directory "db_storage" to hold our tables
    let mut db = Database::new("db_storage")?;
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

        if let Err(_) = db.logto(account_name) {
            println!("Account '{}' not found. Create it? (Y/N)", account_name);
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            if choice.trim().to_uppercase() == "Y" {
                db.create_account(account_name, None)?;
                db.logto(account_name)?;
                break;
            }
        } else {
            break;
        }
    }

    loop {
        let prompt = if db.current_account.is_empty() {
            "PICK> ".to_string()
        } else {
            format!("{} PICK> ", db.current_account)
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
                handle_set(&mut db, &parts);
            }
            "GET" => {
                handle_get(&mut db, &parts);
            }
            "DELETE" => {
                handle_delete(&mut db, &parts);
            }
            "LIST" => {
                handle_list(&mut db, &parts);
            }
            "SELECT" => {
                handle_select(&mut db, &parts);
            }
            "EDIT" => {
                handle_edit(&mut db, &parts, &config);
            }
            "CT" => {
                handle_ct(&mut db, &parts);
            }
            "SAVE-LIST" => {
                handle_save_list(&mut db, &parts);
            }
            "GET-LIST" => {
                handle_get_list(&mut db, &parts);
            }
            "CREATE.FILE" => {
                handle_create_file(&mut db, &parts);
            }
            "DELETE.FILE" => {
                handle_delete_file(&mut db, &parts);
            }
            "CREATE.ACCOUNT" => {
                handle_create_account(&mut db, &parts);
            }
            "DELETE.ACCOUNT" => {
                handle_delete_account(&mut db, &parts);
            }
            "LOGTO" => {
                handle_logto(&mut db, &parts);
            }
            "SAVE" => {
                db.save()?;
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
    db.save()?;
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

    let operators = ["=", "#", "<", ">", "<=", ">=", "[", "]", "[]"];
    let results = if parts.len() >= offset + 5 && parts[offset + 1].to_uppercase() == "WITH" && operators.contains(&parts[offset + 3]) {
        let field_name = parts[offset + 2];
        let op = parts[offset + 3];
        let mut value = parts[offset + 4].to_string();
        
        // Remove quotes if present
        if value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len()-1].to_string();
        }
        db.query(table_name, is_dict, field_name, op, &value, keys_to_filter.as_deref())
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
