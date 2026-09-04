use crate::db::{Database, DbError, ExplodeSpec, IndexStats, QueryNode, Record, SortSpec, Table};
use crate::server::models::{ErrorCode, Request, Response};
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

/// Lifetime of a certificate issued through `GENERATE.CERT`. A year matches
/// what the CLI has always handed out; the dashboard's own certificate is far
/// shorter lived and is issued separately.
const CLIENT_CERT_DAYS: u32 = 365;

/// An error reply: the code a client branches on, and the message a person
/// reads. Both, always - a refusal that carries only prose is one no client can
/// act on, which is why every failure in this file goes through here.
fn error(code: ErrorCode, message: impl Into<String>) -> Response {
    Response {
        status: "ERROR".to_string(),
        message: Some(message.into()),
        code: Some(code),
        ..Default::default()
    }
}

/// The engine's own failure, classified by its variant rather than by reading
/// what it says.
fn db_error(e: DbError) -> Response {
    error(ErrorCode::from(&e), e.to_string())
}

/// The same, where the error's own words do not say what was being attempted:
/// "No space left on device" is not much use without "Save error" in front of
/// it. The code is unchanged - the context is for the reader.
fn db_error_in(context: &str, e: DbError) -> Response {
    error(ErrorCode::from(&e), format!("{}: {}", context, e))
}

/// The default display width `SET.DICT` gives an entry that does not name one.
const DEFAULT_DICT_WIDTH: i64 = 10;
/// The justifications a dictionary entry may carry, as `LIST` understands them.
const DICT_JUSTIFICATIONS: [&str; 2] = ["L", "R"];

/// One dictionary entry decomposed into the attributes
/// [Data Structures](../../../../docs/data_structures.md) documents.
///
/// A dictionary record is a record like any other, so serializing it the way
/// `READ` does would label it with the *data* file's field names - attribute 1
/// would come back as whatever attribute 1 of the file is called. This reads
/// the fixed positions instead, and carries the raw display string alongside
/// them so an entry using a position this does not name is still visible.
pub(crate) fn dictionary_entry(record: &Record) -> serde_json::Value {
    let attribute = |idx: usize| record.get_field_display_string(idx);
    let number = |idx: usize| attribute(idx).trim().parse::<i64>().ok();
    serde_json::json!({
        "field": number(crate::db::DICT_FIELD_IDX),
        "heading": attribute(crate::db::DICT_NAME_IDX),
        "justification": attribute(crate::db::DICT_JUSTIFY_IDX),
        "width": number(crate::db::DICT_WIDTH_IDX),
        "conversion": attribute(crate::db::DICT_CONV_IDX),
        "definition": record.to_display_string(),
    })
}

/// A field of the `structured_data` object `SET.DICT` takes, as text. A number
/// is accepted for a field a form would more naturally send as one.
fn dict_text(spec: &serde_json::Value, name: &str) -> Option<String> {
    match spec.get(name) {
        Some(serde_json::Value::String(text)) => Some(text.trim().to_string()),
        Some(serde_json::Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

/// The same, as a whole number. `Err` carries what was sent instead, so a
/// mistyped width is refused with the reason rather than treated as absent.
fn dict_number(spec: &serde_json::Value, name: &str) -> Result<Option<i64>, String> {
    match spec.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => match number.as_i64() {
            Some(value) => Ok(Some(value)),
            None => Err(number.to_string()),
        },
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(serde_json::Value::String(text)) => match text.trim().parse::<i64>() {
            Ok(value) => Ok(Some(value)),
            Err(_) => Err(text.clone()),
        },
        Some(other) => Err(other.to_string()),
    }
}

/// Builds the dictionary record `SET.DICT` stores, or says why the attributes
/// it was given do not describe one.
///
/// Validating here rather than in a caller is the point of the command: a
/// dictionary entry with no attribute number is invisible to every query, and
/// one with a justification `LIST` does not understand lays out wrongly, and
/// neither failure shows up until someone reads the file.
fn dictionary_record(key: &str, spec: &serde_json::Value) -> Result<Record, String> {
    let field = match dict_number(spec, "field") {
        Ok(Some(number)) if number >= 1 => number,
        Ok(Some(_)) => return Err("Attribute number must be 1 or greater".to_string()),
        Ok(None) => return Err("Attribute number not specified".to_string()),
        Err(text) => return Err(format!("Attribute number is not a whole number: {}", text)),
    };
    let width = match dict_number(spec, "width") {
        Ok(Some(number)) if number >= 1 => number,
        Ok(Some(_)) => return Err("Display width must be 1 or greater".to_string()),
        Ok(None) => DEFAULT_DICT_WIDTH,
        Err(text) => return Err(format!("Display width is not a whole number: {}", text)),
    };

    // An entry with no heading of its own is headed by its name, which is what
    // every dictionary written by hand in this database already does.
    let heading = match dict_text(spec, "heading") {
        Some(heading) if !heading.is_empty() => heading,
        _ => key.to_string(),
    };
    let justification = match dict_text(spec, "justification") {
        Some(text) if !text.is_empty() => text.to_uppercase(),
        _ => DICT_JUSTIFICATIONS[0].to_string(),
    };
    if !DICT_JUSTIFICATIONS.contains(&justification.as_str()) {
        return Err(format!(
            "Justification must be {} or {}",
            DICT_JUSTIFICATIONS[0], DICT_JUSTIFICATIONS[1]
        ));
    }
    let conversion = dict_text(spec, "conversion").unwrap_or_default();

    // The conversion sits at attribute 8, so the positions between it and the
    // width are filled and then trimmed back off when nothing occupies them -
    // an entry without a conversion is `1^NAME^L^20`, as the CLI writes it.
    let mut attributes = vec![
        field.to_string(),
        heading,
        justification,
        width.to_string(),
        String::new(),
        String::new(),
        String::new(),
        conversion,
    ];
    while attributes.last().is_some_and(String::is_empty) {
        attributes.pop();
    }
    Ok(Record::from_attributes(attributes))
}

/// The file and dictionary field an index command names.
#[allow(clippy::result_large_err)]
fn index_target(req: &Request) -> Result<(String, String), Response> {
    let file = match req.file.as_deref().map(str::trim) {
        Some(file) if !file.is_empty() => file.to_string(),
        _ => return Err(error(ErrorCode::MissingField, "File not specified")),
    };
    let field = match req.field.as_deref().map(str::trim) {
        Some(field) if !field.is_empty() => field.to_string(),
        _ => return Err(error(ErrorCode::MissingField, "Field not specified")),
    };
    Ok((file, field))
}

/// One index, as the reply describes it.
fn index_response(stats: IndexStats) -> Response {
    Response {
        status: "OK".to_string(),
        record: Some(serde_json::to_value(stats).unwrap_or(serde_json::Value::Null)),
        ..Default::default()
    }
}

/// Every index of a file, paired with its field name the way the other listings
/// pair a name with what is worth knowing about it.
///
/// Keyed by the bare field name, which is what it has always been and what a
/// client keying off it expects. Each entry names its own file, so this and the
/// account-wide listing below hand a client the same row.
fn index_listing(indexes: Vec<IndexStats>) -> Response {
    listing_of(indexes.into_iter().map(|stats| (stats.field.clone(), stats)).collect())
}

/// Every index of an account, keyed `<file>/<field>` so two files indexing the
/// same field name are still two rows.
fn account_index_listing(indexes: Vec<(String, IndexStats)>) -> Response {
    listing_of(
        indexes
            .into_iter()
            .map(|(file, stats)| (format!("{}/{}", file, stats.field), stats))
            .collect(),
    )
}

fn listing_of(entries: Vec<(String, IndexStats)>) -> Response {
    let results: Vec<(String, serde_json::Value)> = entries
        .into_iter()
        .map(|(key, stats)| (key, serde_json::to_value(stats).unwrap_or(serde_json::Value::Null)))
        .collect();
    let keys: Vec<String> = results.iter().map(|(key, _)| key.clone()).collect();
    let count = results.len();
    Response {
        status: "OK".to_string(),
        keys: Some(keys),
        results: Some(results),
        count: Some(count),
        ..Default::default()
    }
}

/// The commands that work on the records of a single file.
///
/// Each of these locks the one file it names, so two connections working on two
/// different files never wait for each other - not even when one of them is
/// writing. Everything else (creating files and accounts, changing
/// authorizations, the stateful select lists) still takes the database
/// exclusively, which is cheap because none of it is on the hot path.
fn is_record_command(command: &str) -> bool {
    matches!(command, "READ" | "WRITE" | "DELETE" | "QUERY")
}

pub fn handle_request(req: Request, db: &SharedDb, client_info: &crate::db::ClientInfo) -> Response {
    let command = req.command.to_uppercase();

    // Fast path: record work needs nothing exclusive, because the file it names
    // carries its own lock. Anything else - an unresolvable account, a denied
    // request that wants to be logged - falls through to the slow path, which is
    // also where the response is produced, so behaviour is identical.
    if is_record_command(&command) {
        let account = allowed_account(&req, client_info).map(str::to_string);
        if let Some(acc) = account {
            let db = read_lock(db);
            return record_command(&command, req, &db, &acc);
        }
    }

    let mut db = write_lock(db);
    handle_request_locked(req, &mut db, client_info)
}

/// Runs one of the [record commands](is_record_command) against an account the
/// caller has already checked the client may reach.
///
/// Shared by both paths, so a request served under the shared lock and one that
/// fell through to the exclusive lock cannot drift apart.
fn record_command(command: &str, req: Request, db: &Database, acc: &str) -> Response {
    match command {
        "READ" => read_record(db, acc, &req),
        "WRITE" => write_record(db, acc, req),
        "DELETE" => delete_record(db, acc, req),
        _ => query_records(db, acc, &req),
    }
}

/// Resolves the file a request names, loading it if it is not in memory.
// The `Err` variant is a ready-to-send `Response`, which every caller returns as
// its own value. Boxing it to shrink the `Result` would only add an allocation on
// the error path and an unboxing at each call site.
#[allow(clippy::result_large_err)]
fn resolve_file(db: &Database, acc: &str, name: &str) -> Result<crate::db::TableHandle, Response> {
    db.get_table_mut_for_account(acc, name).map_err(db_error)
}

/// The file a request names, or the error to send back when it names none.
#[allow(clippy::result_large_err)]
fn requested_file(req: &Request) -> Result<&str, Response> {
    req.file
        .as_deref()
        .ok_or_else(|| error(ErrorCode::MissingField, "File not specified"))
}

fn read_record(db: &Database, acc: &str, req: &Request) -> Response {
    let table_name = match requested_file(req) {
        Ok(name) => name,
        Err(resp) => return resp,
    };
    // An already loaded, still current file needs no freshness check of its own.
    let handle = match db.table_ready_for_read(acc, table_name) {
        Some(handle) => handle,
        None => match resolve_file(db, acc, table_name) {
            Ok(handle) => handle,
            Err(resp) => return resp,
        },
    };
    let table = handle.read();
    read_command(db, &table, req)
}

fn query_records(db: &Database, acc: &str, req: &Request) -> Response {
    let table_name = match requested_file(req) {
        Ok(name) => name,
        Err(resp) => return resp,
    };
    let handle = match db.table_ready_for_read(acc, table_name) {
        Some(handle) => handle,
        None => match resolve_file(db, acc, table_name) {
            Ok(handle) => handle,
            Err(resp) => return resp,
        },
    };
    let table = handle.read();
    query_command(db, &table, req)
}

fn write_record(db: &Database, acc: &str, req: Request) -> Response {
    let table_name = match requested_file(&req) {
        Ok(name) => name.to_string(),
        Err(resp) => return resp,
    };
    // Resolved once, and held: deserialization needs the dictionary and the
    // write needs the records. Resolving a second time would take this file's
    // lock again - and on a file several connections are writing at once, that
    // is the contended one.
    let handle = match resolve_file(db, acc, &table_name) {
        Ok(handle) => handle,
        Err(resp) => return resp,
    };

    let key = match req.key {
        Some(k) => k,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    let record = if let Some(structured) = req.structured_data {
        match db.deserialize_record_in(&handle.read(), &structured) {
            Some(r) => r,
            None => return error(ErrorCode::InvalidData, "Invalid structured data"),
        }
    } else if let Some(data_val) = req.data {
        match data_val {
            serde_json::Value::String(s) => Record::from_display_string(&s),
            serde_json::Value::Object(_) => match db.deserialize_record_in(&handle.read(), &data_val) {
                Some(r) => r,
                None => return error(ErrorCode::InvalidData, "Invalid structured data in data field"),
            },
            _ => {
                return error(
                    ErrorCode::InvalidData,
                    "Invalid data type in data field: expected string or object",
                );
            }
        }
    } else {
        return error(ErrorCode::MissingField, "Data not specified");
    };

    // The file's lock is dropped before the flush below: `note_write_for` may
    // decide to save, and a save locks every dirty file in turn.
    {
        let mut table = handle.write();
        if is_dict {
            table.dictionary.insert(key, record);
            table.mark_dict_dirty();
        } else {
            table.insert_record(&key, record);
        }
    }
    match db.note_write_for(acc, &table_name) {
        Ok(_) => Response {
            status: "OK".to_string(),
            ..Default::default()
        },
        Err(e) => db_error_in("Save error", e),
    }
}

fn delete_record(db: &Database, acc: &str, req: Request) -> Response {
    let table_name = match requested_file(&req) {
        Ok(name) => name.to_string(),
        Err(resp) => return resp,
    };
    let key = match req.key {
        Some(k) => k,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    {
        let handle = match resolve_file(db, acc, &table_name) {
            Ok(handle) => handle,
            Err(resp) => return resp,
        };
        let mut table = handle.write();
        if is_dict {
            table.dictionary.remove(&key);
            table.mark_dict_dirty();
        } else {
            table.remove_record(&key);
        }
    }
    match db.note_write_for(acc, &table_name) {
        Ok(_) => Response {
            status: "OK".to_string(),
            ..Default::default()
        },
        Err(e) => db_error_in("Save error", e),
    }
}

/// The account a request targets, or `None` when resolving it needs the slow
/// path (unspecified, or denied and therefore worth an error log entry).
fn allowed_account<'a>(req: &'a Request, client_info: &'a crate::db::ClientInfo) -> Option<&'a str> {
    match req.account.as_deref() {
        Some(acc) => {
            if client_info.is_admin || client_info.allowed_accounts.iter().any(|a| a == acc) {
                Some(acc)
            } else {
                None
            }
        }
        None if client_info.allowed_accounts.len() == 1 => Some(&client_info.allowed_accounts[0]),
        None => None,
    }
}

/// Reads a single record from `table`, which the caller has already resolved.
fn read_command(db: &Database, table: &Table, req: &Request) -> Response {
    if req.file.is_none() {
        return error(ErrorCode::MissingField, "File not specified");
    }
    let key = match req.key.as_deref() {
        Some(k) => k,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    let records = if is_dict { &table.dictionary } else { &table.records };
    match records.get(key) {
        Some(record) => Response {
            status: "OK".to_string(),
            record: Some(db.serialize_record_in(table, record)),
            ..Default::default()
        },
        None => error(ErrorCode::RecordNotFound, "Record not found"),
    }
}

/// Runs a QUERY against `table`, which the caller has already resolved.
fn query_command(db: &Database, table: &Table, req: &Request) -> Response {
    let table_name = match req.file.as_deref() {
        Some(t) => t,
        None => return error(ErrorCode::MissingField, "File not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);

    let (query_node, sort_specs, explode) = match resolve_clause(db, table_name, req) {
        Ok(clause) => clause,
        Err(response) => return response,
    };

    // Resolve the dictionary once for the whole result set rather than per record.
    let schema = db.record_schema(table);

    if query_node.is_none() && explode.is_none() {
        // Full scan with nothing to explode: sort the keys only, then serialize
        // each record by reference so the whole table is never cloned into
        // memory.
        let records = if is_dict { &table.dictionary } else { &table.records };
        let mut keys: Vec<String> = records.keys().cloned().collect();
        if sort_specs.is_empty() {
            // `sort_keys_in` already falls back to the ID, so only sort here.
            keys.sort();
        } else {
            keys = Database::sort_keys_in(table, is_dict, keys, &sort_specs);
        }

        let results_processed: Vec<(String, serde_json::Value)> = keys
            .into_iter()
            .filter_map(|k| {
                let record = records.get(&k)?;
                Some((k, db.serialize_record_with_schema(&schema, record)))
            })
            .collect();

        return Response {
            status: "OK".to_string(),
            results: Some(results_processed),
            ..Default::default()
        };
    }

    let mut rows = Database::query_exploded_in(table, is_dict, query_node.as_ref(), explode.as_ref(), None);
    let explode_idx = Database::explode_field_index(table, explode.as_ref());
    Database::sort_entries_in(table, &mut rows, &sort_specs, explode_idx);

    let exploded = explode.is_some();
    let mut results_processed = Vec::with_capacity(rows.len());
    let mut positions = Vec::with_capacity(rows.len());
    for (entry, record) in rows {
        results_processed.push((entry.key, db.serialize_record_with_schema(&schema, record)));
        positions.push(entry.position);
    }

    Response {
        status: "OK".to_string(),
        results: Some(results_processed),
        positions: exploded.then_some(positions),
        ..Default::default()
    }
}

/// What a QUERY or SELECT selects: the criteria, the ordering, and the field
/// whose values become rows of their own.
type Clause = (Option<QueryNode>, Vec<SortSpec>, Option<ExplodeSpec>);

/// Resolves the selection clause a QUERY or SELECT carries, however it was
/// spelled: a pre-built `query_node`, or a `query_string` re-parsed here.
/// `sort_specs` and `explode` given as their own fields win over anything the
/// query string spells out, so a structured client is never second-guessed.
///
/// `Err` is a query string that was given and not understood. It used to parse
/// to "no criteria", which is the same thing an absent clause parses to - so a
/// mistyped `WITH` came back as the whole file with `status: "OK"`, a wrong
/// answer rather than a refusal.
#[allow(clippy::result_large_err)]
fn resolve_clause(db: &Database, table_name: &str, req: &Request) -> Result<Clause, Response> {
    let mut sort_specs = req.sort_specs.clone().unwrap_or_default();
    let mut explode = req
        .explode
        .as_ref()
        .and_then(|names| names.first())
        .map(|name| ExplodeSpec {
            field_name: name.clone(),
            condition: None,
        });

    let mut query_node = req.query_node.clone();
    if let (None, Some(q_str)) = (&query_node, req.query_string.as_deref()) {
        let parts: Vec<&str> = q_str.split_whitespace().collect();
        let (clause_parts, parsed_sorts, parsed_explodes) = Database::parse_clause_specs(&parts);
        if sort_specs.is_empty() {
            sort_specs = parsed_sorts;
        }
        query_node = db.parse_query_read_only(table_name, &clause_parts);
        if query_node.is_none() && !clause_parts.is_empty() {
            return Err(error(
                ErrorCode::InvalidQuery,
                format!("Query is not understood: {}", clause_parts.join(" ")),
            ));
        }
        if let (None, Some(spec)) = (&explode, parsed_explodes.into_iter().next()) {
            query_node = Database::and_condition(query_node, spec.condition.clone());
            explode = Some(spec);
        }
    }

    Ok((query_node, sort_specs, explode))
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
            return error(ErrorCode::AccessDenied, msg);
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
            return error(ErrorCode::AccountNotSpecified, "Account not specified");
        }
    };

    let acc = match target_account {
        Some(ref a) => a.as_str(),
        None => "", // Some commands might not need an account, or will fail later
    };

    match req.command.to_uppercase().as_str() {
        // The record commands carry their own file lock, so they run
        // identically here and on the shared path in [`handle_request`]. These
        // arms are for the callers that hold the database exclusively already:
        // a request whose account could not be resolved without logging a
        // denial, and the tests.
        "READ" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            read_record(db, acc, &req)
        }
        "WRITE" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            write_record(db, acc, req)
        }
        "DELETE" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            delete_record(db, acc, req)
        }
        "QUERY" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            query_records(db, acc, &req)
        }
        "SELECT" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let table_name = match req.file.clone() {
                Some(t) => t,
                None => {
                    return error(ErrorCode::MissingField, "File not specified");
                }
            };
            let is_dict = req.is_dict.unwrap_or(false);
            let list_name = req.list_name.clone().unwrap_or_else(|| "DEFAULT".to_string());

            let (query_node, sort_specs, explode) = match resolve_clause(db, &table_name, &req) {
                Ok(clause) => clause,
                Err(response) => return response,
            };

            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return db_error(e);
            }
            let entries = match db.get_table_read_only_for_account(acc, &table_name) {
                Some(handle) => Database::select_entries_in(
                    &handle.read(),
                    is_dict,
                    query_node.as_ref(),
                    explode.as_ref(),
                    None,
                    &sort_specs,
                ),
                None => return error(ErrorCode::FileNotFound, format!("File '{}' is not loaded", table_name)),
            };

            let list = crate::db::SelectList {
                table_name,
                is_dict,
                explode_field: explode.map(|e| e.field_name),
                entries,
            };
            let count = list.len();
            db.remote_select_lists.insert(list_name.clone(), list);
            db.remote_select_cursors.insert(list_name, 0);

            Response {
                status: "OK".to_string(),
                count: Some(count),
                ..Default::default()
            }
        }
        "GET.NEXT" => {
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());
            let batch_size = req.batch_size.unwrap_or(1);

            let (entries_batch, table_name, is_dict) = {
                let list = match db.remote_select_lists.get(&list_name) {
                    Some(l) => l,
                    None => {
                        return error(ErrorCode::SelectListNotFound, "Select list not found");
                    }
                };

                let list_len = list.len();
                let table_name = list.table_name.clone();
                let is_dict = list.is_dict;

                let cursor = *db.remote_select_cursors.get(&list_name).unwrap();
                if cursor >= list_len {
                    return Response {
                        status: "EOF".to_string(),
                        ..Default::default()
                    };
                }

                let end = std::cmp::min(cursor + batch_size, list_len);
                let entries = list.entries[cursor..end].to_vec();
                db.remote_select_cursors.insert(list_name, end);
                (entries, table_name, is_dict)
            };

            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return db_error(e);
            }
            let handle = match db.get_table_read_only_for_account(acc, &table_name) {
                Some(handle) => handle,
                None => return error(ErrorCode::FileNotFound, format!("File '{}' is not loaded", table_name)),
            };
            let table = handle.read();
            let records = if is_dict { &table.dictionary } else { &table.records };

            // One dictionary walk for the batch, and each record serialized by
            // reference instead of cloned.
            let schema = db.record_schema(&table);
            let mut results_processed = Vec::with_capacity(entries_batch.len());
            let mut positions = Vec::with_capacity(entries_batch.len());
            for entry in &entries_batch {
                let Some(record) = records.get(&entry.key) else {
                    continue;
                };
                results_processed.push((entry.key.clone(), db.serialize_record_with_schema(&schema, record)));
                positions.push(entry.position);
            }
            let results_len = results_processed.len();
            // Only an exploded list has anything to say here; an ordinary one
            // leaves the field out entirely rather than sending a run of nulls.
            let exploded = positions.iter().any(Option::is_some);

            Response {
                status: "OK".to_string(),
                results: Some(results_processed),
                count: Some(results_len),
                positions: exploded.then_some(positions),
                ..Default::default()
            }
        }
        "CREATE.ACCOUNT" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.target_account {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Account name not specified");
                }
            };
            match db.create_account(&name, None) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "CREATE.TEST.ACCOUNT" => {
            // The CLI restricts this to the SYSTEM account; over the wire the
            // equivalent is an admin certificate, the same gate the other
            // account commands sit behind.
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.target_account {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Account name not specified");
                }
            };
            match db.create_test_account(&name) {
                // The files are read back rather than listed here, so this
                // reports whatever the fixture actually creates today.
                Ok(_) => {
                    let files = db.list_tables_for_account(&name);
                    Response {
                        status: "OK".to_string(),
                        record: Some(serde_json::json!({ "account": name, "files": files })),
                        ..Default::default()
                    }
                }
                Err(e) => db_error(e),
            }
        }
        "DELETE.ACCOUNT" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.target_account {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Account name not specified");
                }
            };
            match db.delete_account(&name) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "CREATE.FILE" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File name not specified");
                }
            };
            let durable = req.durable.unwrap_or(false);
            match db.create_table_for_account_durable(acc, &name, durable) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "SET.FILE" => {
            // Promoting a file to durable is a storage decision for the account,
            // like creating one, so it is gated the same way.
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File name not specified");
                }
            };
            // Absent rather than false: an omitted flag would otherwise quietly
            // demote a file the caller only meant to name.
            let durable = match req.durable {
                Some(d) => d,
                None => {
                    return error(ErrorCode::MissingField, "Durability flag not specified");
                }
            };
            match db.set_table_durable_for_account(acc, &name, durable) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    record: Some(serde_json::json!({ "account": acc, "name": name, "durable": durable })),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "DELETE.FILE" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File name not specified");
                }
            };
            match db.delete_table_for_account(acc, &name) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        // Indexes. Creating, rebuilding and dropping one are storage decisions
        // about a file, so they are gated exactly as creating the file is;
        // listing them is not, any more than listing the files is.
        "CREATE.INDEX" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let (file, field) = match index_target(&req) {
                Ok(target) => target,
                Err(response) => return response,
            };
            let exclude = req.values.clone().unwrap_or_default();
            match db.create_index_excluding(acc, &file, &field, &exclude) {
                Ok(stats) => index_response(stats),
                Err(e) => db_error(e),
            }
        }
        // The remedy between leaving an index alone and dropping it: a field
        // where one value covers most of the file is excellent to index for
        // everything else, and excluding that value keeps what the index is
        // good at without paying for the entry that saves nothing.
        "SET.INDEX.EXCLUDE" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let (file, field) = match index_target(&req) {
                Ok(target) => target,
                Err(response) => return response,
            };
            // Absent and empty mean the same thing here: the command replaces
            // the set, so sending no values is how the set is cleared.
            let values = req.values.clone().unwrap_or_default();
            match db.set_index_exclusions(acc, &file, &field, &values) {
                Ok(stats) => index_response(stats),
                Err(e) => db_error(e),
            }
        }
        // One index in full, with the values that dominate it. Its own command
        // rather than a wider `LIST.INDEXES`, which is a per-file listing read
        // on every navigation and should stay cheap.
        "INDEX.STATS" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let (file, field) = match index_target(&req) {
                Ok(target) => target,
                Err(response) => return response,
            };
            let limit = req.limit.unwrap_or(crate::db::health::thresholds::HISTOGRAM_DEFAULT);
            match db.index_report(acc, &file, &field, limit) {
                Ok(report) => Response {
                    status: "OK".to_string(),
                    record: Some(serde_json::to_value(report).unwrap_or(serde_json::Value::Null)),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "REBUILD.INDEX" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let (file, field) = match index_target(&req) {
                Ok(target) => target,
                Err(response) => return response,
            };
            match db.rebuild_index_for_account(acc, &file, &field) {
                Ok(stats) => index_response(stats),
                Err(e) => db_error(e),
            }
        }
        "DELETE.INDEX" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let (file, field) = match index_target(&req) {
                Ok(target) => target,
                Err(response) => return response,
            };
            match db.drop_index_for_account(acc, &file, &field) {
                Ok(()) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        // With a `file`, one file's indexes. Without one, every index in the
        // account - the view that comes to you, so index health is visible
        // without walking file by file through three columns of navigation.
        "LIST.INDEXES" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            match req.file.as_deref().map(str::trim).filter(|file| !file.is_empty()) {
                Some(file) => match db.index_statistics(acc, file) {
                    Ok(indexes) => index_listing(indexes),
                    Err(e) => db_error(e),
                },
                None => match db.index_statistics_for_account(acc) {
                    Ok(indexes) => account_index_listing(indexes),
                    Err(e) => db_error(e),
                },
            }
        }
        "AUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let thumbprint = match req.thumbprint {
                Some(t) => t,
                None => {
                    return error(ErrorCode::MissingField, "Thumbprint not specified");
                }
            };
            let name = match req.name {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Name not specified");
                }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            let is_admin = req.is_admin.unwrap_or(false);
            match db.add_authorized_client(&name, &thumbprint, accounts, is_admin) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "DEAUTHORIZE.CONN" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.name {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Name not specified");
                }
            };
            match db.remove_authorized_client(&name) {
                Ok(true) => Response {
                    status: "OK".to_string(),
                    ..Default::default()
                },
                Ok(false) => error(ErrorCode::ClientNotFound, "Client not found"),
                Err(e) => db_error(e),
            }
        }
        "ADD.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.name {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Name not specified");
                }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.add_client_account(&name, &acc) {
                    return db_error_in(&format!("Error adding account {}", acc), e);
                }
            }
            Response {
                status: "OK".to_string(),
                ..Default::default()
            }
        }
        "REMOVE.CLIENT.ACCOUNT" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let name = match req.name {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Name not specified");
                }
            };
            let accounts = req.accounts_list.unwrap_or_default();
            for acc in accounts {
                if let Err(e) = db.remove_client_account(&name, &acc) {
                    return db_error_in(&format!("Error removing account {}", acc), e);
                }
            }
            Response {
                status: "OK".to_string(),
                ..Default::default()
            }
        }
        "LIST.CONNS" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            // Re-read first: another process (a CLI beside this server) may have
            // authorized or revoked a client since the last request.
            let _ = db.refresh_clients_if_stale();
            let results = db
                .authorized_clients()
                .into_iter()
                .map(|info| {
                    (
                        info.name.clone(),
                        serde_json::json!({
                            "thumbprint": info.thumbprint,
                            "accounts": info.allowed_accounts,
                            "is_admin": info.is_admin,
                        }),
                    )
                })
                .collect::<Vec<_>>();
            let count = results.len();
            Response {
                status: "OK".to_string(),
                results: Some(results),
                count: Some(count),
                ..Default::default()
            }
        }
        "LIST.ACCOUNTS" => {
            // A client sees the accounts it may reach; an admin sees them all.
            let stats: Vec<crate::db::AccountStats> = db
                .account_statistics()
                .into_iter()
                .filter(|account| client_info.is_admin || client_info.allowed_accounts.contains(&account.name))
                .collect();
            let results = stats
                .into_iter()
                .map(|account| {
                    let name = account.name.clone();
                    (name, serde_json::to_value(account).unwrap_or(serde_json::Value::Null))
                })
                .collect::<Vec<_>>();
            let count = results.len();
            Response {
                status: "OK".to_string(),
                results: Some(results),
                count: Some(count),
                ..Default::default()
            }
        }
        "LIST.FILES" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            // `keys` is the plain listing every client already reads; `results`
            // carries what is worth knowing about a file beside its name, so
            // durability is answerable without reading the account's DIR file.
            let files = db.list_tables_with_durability_for_account(acc);
            let count = files.len();
            let keys = files.iter().map(|(name, _)| name.clone()).collect();
            let results = files
                .into_iter()
                .map(|(name, durable)| {
                    // The cheap verdict - section metadata and index `state`
                    // files, no group trailers and no records - so a problem
                    // file is findable without opening every file in turn.
                    let health = db.file_health_summary(acc, &name);
                    let value = serde_json::json!({
                        "durable": durable,
                        "health": health.verdict.as_str(),
                        "health_reasons": health.reasons,
                    });
                    (name, value)
                })
                .collect::<Vec<_>>();
            Response {
                status: "OK".to_string(),
                keys: Some(keys),
                results: Some(results),
                count: Some(count),
                ..Default::default()
            }
        }
        "FILE.STATS" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File not specified");
                }
            };
            match db.file_statistics(acc, &name) {
                Ok(stats) => Response {
                    status: "OK".to_string(),
                    record: Some(serde_json::to_value(stats).unwrap_or(serde_json::Value::Null)),
                    ..Default::default()
                },
                Err(e) => db_error(e),
            }
        }
        "LIST.DICT" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File not specified");
                }
            };
            let handle = match db.get_table_mut_for_account(acc, &name) {
                Ok(handle) => handle,
                Err(e) => return db_error(e),
            };
            let table = handle.read();
            // Ordered by attribute number, then by name: the order the file's
            // records are laid out in, which is the order a dictionary is read.
            let mut entries: Vec<(&String, &Record)> = table.dictionary.iter().collect();
            entries.sort_by(|(left_name, left), (right_name, right)| {
                let position = |record: &Record| {
                    record
                        .get_field_display_string(crate::db::DICT_FIELD_IDX)
                        .trim()
                        .parse::<i64>()
                        .unwrap_or(i64::MAX)
                };
                position(left)
                    .cmp(&position(right))
                    .then_with(|| left_name.cmp(right_name))
            });
            let keys: Vec<String> = entries.iter().map(|(name, _)| (*name).clone()).collect();
            let results: Vec<(String, serde_json::Value)> = entries
                .into_iter()
                .map(|(name, record)| (name.clone(), dictionary_entry(record)))
                .collect();
            let count = results.len();
            Response {
                status: "OK".to_string(),
                keys: Some(keys),
                results: Some(results),
                count: Some(count),
                ..Default::default()
            }
        }
        "SET.DICT" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let name = match req.file {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File not specified");
                }
            };
            let key = match req.key.as_deref().map(str::trim) {
                Some(k) if !k.is_empty() => k.to_string(),
                _ => {
                    return error(ErrorCode::MissingField, "Key not specified");
                }
            };
            let spec = match req.structured_data {
                Some(spec @ serde_json::Value::Object(_)) => spec,
                _ => {
                    return error(ErrorCode::MissingField, "Dictionary attributes not specified");
                }
            };
            let record = match dictionary_record(&key, &spec) {
                Ok(record) => record,
                Err(message) => return error(ErrorCode::InvalidData, message),
            };
            // Read back what was stored rather than echoing what was asked for,
            // so a caller sees the defaults this filled in.
            let entry = dictionary_entry(&record);

            {
                let handle = match db.get_table_mut_for_account(acc, &name) {
                    Ok(handle) => handle,
                    Err(e) => return db_error(e),
                };
                let mut table = handle.write();
                table.dictionary.insert(key, record);
                table.mark_dict_dirty();
            }
            match db.note_write_for(acc, &name) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    record: Some(entry),
                    ..Default::default()
                },
                Err(e) => db_error_in("Save error", e),
            }
        }
        "SERVER.STATS" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let mut snapshot =
                serde_json::to_value(crate::server::stats::snapshot()).unwrap_or(serde_json::Value::Null);
            // The engine side of "how busy is it": what is still only in memory.
            if let Some(object) = snapshot.as_object_mut() {
                object.insert(
                    "pending_writes".to_string(),
                    serde_json::json!(db.pending_write_count()),
                );
                object.insert("loaded_tables".to_string(), serde_json::json!(db.loaded_table_count()));
                object.insert(
                    "authorized_clients".to_string(),
                    serde_json::json!(db.authorized_client_count()),
                );
            }
            Response {
                status: "OK".to_string(),
                record: Some(snapshot),
                ..Default::default()
            }
        }
        "GENERATE.CERT" => {
            if !client_info.is_admin {
                return error(ErrorCode::AdminRequired, "Admin privileges required");
            }
            let common_name = match req.name {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "Name not specified");
                }
            };
            let config = match crate::server::active_config() {
                Some(config) => config,
                None => {
                    return error(
                        ErrorCode::Unavailable,
                        "Certificate generation is unavailable: no server configuration",
                    );
                }
            };
            // A generated certificate is useless until it is authorized, and a
            // caller that has to send a second command can leave orphaned keys
            // behind. Both happen here, or neither does.
            match crate::server::certs::generate_client_cert(&config, &common_name, CLIENT_CERT_DAYS, true) {
                Ok(generated) => {
                    let accounts = req.accounts_list.unwrap_or_default();
                    let is_admin = req.is_admin.unwrap_or(false);
                    if !is_admin && accounts.is_empty() {
                        return error(
                            ErrorCode::InvalidRequest,
                            "A non-admin certificate needs at least one allowed account",
                        );
                    }
                    if let Err(e) = db.add_authorized_client(&common_name, &generated.thumbprint, accounts, is_admin) {
                        return db_error_in("Certificate generated but authorization failed", e);
                    }
                    Response {
                        status: "OK".to_string(),
                        record: Some(serde_json::to_value(&generated).unwrap_or(serde_json::Value::Null)),
                        ..Default::default()
                    }
                }
                Err(e) => db_error(DbError::Io(e)),
            }
        }
        _ => error(ErrorCode::UnknownCommand, "Unknown command"),
    }
}
