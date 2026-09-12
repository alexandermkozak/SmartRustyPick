use crate::db::engine::dictionary::DEFAULT_FIELD_WIDTH;
use crate::db::{
    Change, ChangeOp, Condition, Database, DbError, ExplodeSpec, IndexStats, QueryNode, Record, SortSpec, Table,
    TableHandle,
};
use crate::server::models::{ChangeSpec, ErrorCode, Request, Response};
use std::collections::HashMap;
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
///
/// Derived from the width `LIST` renders an entry at when it carries no width,
/// rather than declared as a second `10`, because the two are one rule: were
/// they different, "no width given" would mean one width for an entry created
/// over the protocol and another for one written by hand. See
/// [`crate::db::engine::dictionary`].
const DEFAULT_DICT_WIDTH: i64 = DEFAULT_FIELD_WIDTH as i64;
/// The justifications a dictionary entry may carry, as `LIST` understands them.
const DICT_JUSTIFICATIONS: [&str; 2] = ["L", "R"];
/// The tiers an association may pair on, as the engine reads them.
const DICT_ASSOC_DEPTHS: [&str; 2] = [crate::db::ASSOC_VALUE, crate::db::ASSOC_SUB_VALUE];

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
        "association": attribute(crate::db::DICT_ASSOC_IDX),
        "associationDepth": attribute(crate::db::DICT_ASSOC_DEPTH_IDX),
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

    // The association is recorded on the dependent, naming its controller, so
    // this is the field that says "my values pair with those of that one". A
    // controller carries nothing: it is found by the entries that name it.
    let association = dict_text(spec, "association").unwrap_or_default();
    if association == key {
        return Err("A dictionary entry cannot be associated with itself".to_string());
    }
    let depth = match dict_text(spec, "associationDepth") {
        Some(text) if !text.is_empty() => text.to_uppercase(),
        _ => String::new(),
    };
    if !depth.is_empty() && !DICT_ASSOC_DEPTHS.contains(&depth.as_str()) {
        return Err(format!(
            "Association depth must be {} (value) or {} (sub-value)",
            DICT_ASSOC_DEPTHS[0], DICT_ASSOC_DEPTHS[1]
        ));
    }
    if association.is_empty() && !depth.is_empty() {
        return Err("Association depth given without a controlling field".to_string());
    }
    // An association with no tier named pairs value for value, and says so
    // rather than leaving the attribute blank for a reader to guess at.
    let depth = match (association.is_empty(), depth.is_empty()) {
        (false, true) => DICT_ASSOC_DEPTHS[0].to_string(),
        _ => depth,
    };

    // The conversion sits at attribute 8, so the positions between it and the
    // association are filled and then trimmed back off when nothing occupies
    // them - an entry with neither is `1^NAME^L^20`, as the CLI writes it.
    let mut attributes = vec![
        field.to_string(),
        heading,
        justification,
        width.to_string(),
        association,
        depth,
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
    matches!(
        command,
        // The queue commands belong here for the reason the record commands do,
        // and rather more so: a queue is the most contended file in any system
        // that has one, and taking the database exclusively to claim from it
        // would serialise every consumer against every other connection in the
        // server rather than against the other consumers of that one queue.
        // TRANSACT belongs here too: it takes the files it names, in name
        // order, and nothing else. Sending it down the exclusive path would
        // make every transaction stop the whole server rather than the files it
        // touches, which is the opposite of what per-file locking bought.
        "READ" | "WRITE" | "DELETE" | "QUERY" | "TRANSACT" | "ENQUEUE" | "DEQUEUE" | "ACK" | "NACK" | "PEEK"
    )
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
            return record_command(&command, req, &db, &acc, &client_info.name);
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
fn record_command(command: &str, req: Request, db: &Database, acc: &str, owner: &str) -> Response {
    match command {
        "READ" => read_record(db, acc, &req),
        "WRITE" => write_record(db, acc, req),
        "DELETE" => delete_record(db, acc, req),
        "TRANSACT" => transact(db, acc, req),
        "ENQUEUE" => enqueue_record(db, acc, req),
        "DEQUEUE" => dequeue_record(db, acc, &req, owner),
        "ACK" => settle_claim(db, acc, &req, owner, Settle::Ack),
        "NACK" => settle_claim(db, acc, &req, owner, Settle::Nack),
        "PEEK" => peek_record(db, acc, &req),
        _ => query_records(db, acc, &req),
    }
}

/// Which way a claim is being settled. The two commands differ only in this, so
/// they share [`settle_claim`] rather than duplicating the ownership check.
#[derive(Clone, Copy)]
enum Settle {
    /// The work succeeded: the record leaves the queue.
    Ack,
    /// The work failed: the record goes back, or to the dead-letter file if it
    /// has used up its deliveries.
    Nack,
}

/// The bookkeeping a queue command reports beside the record, as the `claim`
/// field of the response.
///
/// Times are milliseconds since the epoch, which is what the record's sequence
/// key already carries, so a client that wants an age subtracts rather than
/// parsing a format. `expires` and `owner` are populated only by `DEQUEUE`:
/// `ENQUEUE` and `PEEK` take no claim, and saying nothing is more honest than
/// reporting a claim of zero length.
fn claim_json(file: &str, delivery: &crate::db::QueueDelivery) -> serde_json::Value {
    let mut claim = serde_json::json!({
        "queue": file,
        "key": delivery.key,
        "deliveries": delivery.deliveries,
    });
    let object = claim.as_object_mut().expect("built as an object");
    if let Some(enqueued) = delivery.enqueued_millis {
        object.insert("enqueued".to_string(), enqueued.into());
    }
    if let Some(expires) = delivery.expires_millis {
        object.insert("expires".to_string(), expires.into());
    }
    if let Some(owner) = &delivery.owner {
        object.insert("owner".to_string(), owner.as_str().into());
    }
    claim
}

/// The response a queue command that found nothing sends.
///
/// A distinct status rather than an error, exactly as `GET.NEXT` answers a list
/// it has walked to the end of: an empty queue is the ordinary state of a queue
/// that is keeping up, and a consumer polling one should not have to tell a
/// drained queue from a broken request by reading prose.
fn queue_empty() -> Response {
    Response {
        status: "EMPTY".to_string(),
        count: Some(0),
        ..Default::default()
    }
}

/// The queue file and the record a queue command carries, or the error to send.
#[allow(clippy::result_large_err)]
fn queued_record(db: &Database, acc: &str, req: Request) -> Result<(String, Record), Response> {
    let file = requested_file(&req)?.to_string();
    // Deserialized against this queue's dictionary, so an object payload maps
    // to attributes the same way `WRITE` maps one.
    let handle = resolve_file(db, acc, &file)?;
    let record = match (req.structured_data, req.data) {
        (Some(structured), _) => db
            .deserialize_record_in(&handle.read(), &structured)
            .ok_or_else(|| error(ErrorCode::InvalidData, "Invalid structured data"))?,
        (None, Some(serde_json::Value::String(text))) => Record::from_display_string(&text),
        (None, Some(object @ serde_json::Value::Object(_))) => db
            .deserialize_record_in(&handle.read(), &object)
            .ok_or_else(|| error(ErrorCode::InvalidData, "Invalid structured data in data field"))?,
        (None, Some(_)) => {
            return Err(error(
                ErrorCode::InvalidData,
                "Invalid data type in data field: expected string or object",
            ));
        }
        (None, None) => return Err(error(ErrorCode::MissingField, "Data not specified")),
    };
    Ok((file, record))
}

fn enqueue_record(db: &Database, acc: &str, req: Request) -> Response {
    let (file, record) = match queued_record(db, acc, req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match db.enqueue(acc, &file, record) {
        Ok(key) => {
            let delivery = crate::db::QueueDelivery {
                enqueued_millis: crate::db::queue::key_enqueued_millis(&key),
                key,
                record: Record::new(),
                deliveries: 0,
                expires_millis: None,
                owner: None,
            };
            Response {
                status: "OK".to_string(),
                claim: Some(claim_json(&file, &delivery)),
                ..Default::default()
            }
        }
        Err(e) => db_error_in("Enqueue error", e),
    }
}

fn dequeue_record(db: &Database, acc: &str, req: &Request, owner: &str) -> Response {
    let file = match requested_file(req) {
        Ok(name) => name,
        Err(resp) => return resp,
    };
    let visibility = match req.visibility_timeout {
        Some(seconds) if seconds == 0 || seconds > crate::db::queue::MAX_VISIBILITY_SECONDS => {
            return error(
                ErrorCode::InvalidData,
                format!(
                    "visibility_timeout must be between 1 and {} seconds",
                    crate::db::queue::MAX_VISIBILITY_SECONDS
                ),
            );
        }
        Some(seconds) => Some(std::time::Duration::from_secs(seconds)),
        None => None,
    };
    match db.dequeue(acc, file, owner, visibility) {
        Ok(Some(delivery)) => Response {
            status: "OK".to_string(),
            record: Some(db.serialize_record_for_account(acc, file, &delivery.record)),
            claim: Some(claim_json(file, &delivery)),
            ..Default::default()
        },
        Ok(None) => queue_empty(),
        Err(e) => db_error_in("Dequeue error", e),
    }
}

fn settle_claim(db: &Database, acc: &str, req: &Request, owner: &str, how: Settle) -> Response {
    let file = match requested_file(req) {
        Ok(name) => name,
        Err(resp) => return resp,
    };
    let key = match req.key.as_deref() {
        Some(key) => key,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    let settled = match how {
        Settle::Ack => db.ack(acc, file, key, owner),
        Settle::Nack => db.nack(acc, file, key, owner),
    };
    match settled {
        Ok(()) => Response {
            status: "OK".to_string(),
            ..Default::default()
        },
        Err(e) => db_error(e),
    }
}

fn peek_record(db: &Database, acc: &str, req: &Request) -> Response {
    let file = match requested_file(req) {
        Ok(name) => name,
        Err(resp) => return resp,
    };
    match db.peek(acc, file, req.key.as_deref()) {
        Ok(Some(delivery)) => Response {
            status: "OK".to_string(),
            record: Some(db.serialize_record_for_account(acc, file, &delivery.record)),
            claim: Some(claim_json(file, &delivery)),
            ..Default::default()
        },
        // A named key that is not there is a client mistake; an empty queue is
        // not, so the two do not share an answer.
        Ok(None) if req.key.is_some() => error(ErrorCode::RecordNotFound, "Record not found"),
        Ok(None) => queue_empty(),
        Err(e) => db_error_in("Peek error", e),
    }
}

/// Resolves the file a request names, loading it if it is not in memory.
// The `Err` variant is a ready-to-send `Response`, which every caller returns as
// its own value. Boxing it to shrink the `Result` would only add an allocation on
// the error path and an unboxing at each call site.
#[allow(clippy::result_large_err)]
fn resolve_file(db: &Database, acc: &str, name: &str) -> Result<TableHandle, Response> {
    db.get_table_mut_for_account(acc, name).map_err(db_error)
}

/// The `DIR` attributes a `CREATE.FILE` or `SET.FILE` request asks for, applied
/// over what the file carries now.
///
/// `current` is the default attributes on a create and the file's own on a set,
/// which is what makes an omitted field mean "leave it" rather than "clear it".
/// The two policy numbers are validated here rather than clamped in the engine:
/// a queue given a timeout of zero would hand every record to two consumers at
/// once, and finding that out from the behaviour is far worse than being told.
///
/// A file's *type* is the one thing this will not change. A directory file's
/// records are host files and an ordinary file's are framed inside a hashed
/// section, so turning one into the other is a conversion of every record and
/// not a flag - and a `SET.FILE` that flipped the flag alone would leave a file
/// whose entry says one thing and whose records are somewhere else entirely.
#[allow(clippy::result_large_err)]
fn file_attributes(req: &Request, current: crate::db::FileAttributes) -> Result<crate::db::FileAttributes, Response> {
    use crate::db::queue::{MAX_DELIVERY_LIMIT, MAX_VISIBILITY_SECONDS, QueuePolicy};

    let wants_directory = req.directory.unwrap_or(current.is_directory() || req.path.is_some());
    if wants_directory != current.is_directory() && !current_is_new(&current) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "A file's type is fixed when it is created: create a new file of the type you want and move the records",
        ));
    }
    let wants_autokey = req.autokey.unwrap_or(current.autokey);
    if wants_directory {
        if req.queue == Some(true) || req.visibility_timeout.is_some() || req.max_deliveries.is_some() {
            return Err(error(
                ErrorCode::InvalidRequest,
                "A directory file cannot be a queue: its records are host files, with no order to claim from",
            ));
        }
        if wants_autokey {
            return Err(error(
                ErrorCode::InvalidRequest,
                "A directory file cannot mint keys: its keys are the names of host files, which is what a caller \
                 opens them by",
            ));
        }
        if req.durable == Some(false) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "A directory file has no buffered writes to make durable: a record is on disk when the write returns",
            ));
        }
        let path = req
            .path
            .clone()
            .or_else(|| current.directory.as_ref().map(|policy| policy.path.clone()))
            .unwrap_or_default();
        return Ok(crate::db::FileAttributes {
            durable: false,
            queue: None,
            autokey: false,
            directory: Some(crate::db::DirectoryPolicy { path }),
        });
    }
    if req.path.is_some() {
        return Err(error(
            ErrorCode::InvalidRequest,
            "path names where a directory file's records live; give directory as well, or leave both out",
        ));
    }

    let durable = req.durable.unwrap_or(current.durable);
    let wants_queue = req
        .queue
        .unwrap_or(current.queue.is_some() || req.visibility_timeout.is_some() || req.max_deliveries.is_some());
    if !wants_queue {
        return Ok(crate::db::FileAttributes {
            durable,
            queue: None,
            autokey: wants_autokey,
            directory: None,
        });
    }
    if wants_autokey {
        // Refused rather than settled either way: a queue already mints the key
        // of every record it stores, so a file asked for both has been asked
        // for two counters, and which one a `WRITE` should draw from is a
        // question only the caller can answer. `ENQUEUE` is what appends to a
        // queue; `autokey` is what gives an ordinary file the same thing.
        return Err(error(
            ErrorCode::InvalidRequest,
            "A queue file already mints the key of every record it stores: use ENQUEUE, or create a separate \
             autokey file for records that are not claimed",
        ));
    }

    let existing = current.queue.unwrap_or_default();
    let visibility = match req.visibility_timeout {
        None => existing.visibility,
        Some(seconds) if (1..=MAX_VISIBILITY_SECONDS).contains(&seconds) => std::time::Duration::from_secs(seconds),
        Some(_) => {
            return Err(error(
                ErrorCode::InvalidData,
                format!(
                    "visibility_timeout must be between 1 and {} seconds",
                    MAX_VISIBILITY_SECONDS
                ),
            ));
        }
    };
    let max_deliveries = match req.max_deliveries {
        None => existing.max_deliveries,
        Some(count) if (1..=MAX_DELIVERY_LIMIT).contains(&count) => count,
        Some(_) => {
            return Err(error(
                ErrorCode::InvalidData,
                format!("max_deliveries must be between 1 and {}", MAX_DELIVERY_LIMIT),
            ));
        }
    };
    Ok(crate::db::FileAttributes {
        // A file *becoming* a queue defaults to durable, because acknowledging
        // a claim that a crash then loses is the failure a queue exists to
        // prevent. An explicit `durable: false` is still honoured - the caller
        // has said so - and a file that is already a queue keeps the durability
        // it has, so a request about the claim policy does not silently undo a
        // deliberate demotion.
        durable: req
            .durable
            .unwrap_or_else(|| if current.queue.is_some() { current.durable } else { true }),
        queue: Some(QueuePolicy {
            visibility,
            max_deliveries,
        }),
        autokey: false,
        directory: None,
    })
}

/// Whether `current` is the placeholder a `CREATE.FILE` passes in rather than a
/// file that already exists. A create may settle on any type; a set may not
/// change the one a file has.
fn current_is_new(current: &crate::db::FileAttributes) -> bool {
    *current == crate::db::FileAttributes::default()
}

/// What `CREATE.FILE` and `SET.FILE` report back about the file they settled.
///
/// `path` is the host directory a directory file's records are the files of.
/// It is reported resolved rather than as the entry spells it, because an
/// entry that says nothing means "the default place", and an operator asking
/// where the records went wants the answer and not the rule.
fn file_attributes_json(db: &Database, account: &str, name: &str) -> serde_json::Value {
    // Read back rather than echoed: this reports what the file now carries,
    // which is the only answer that stays right when the engine settles an
    // attribute the request did not name.
    let attributes = db.file_attributes_for_account(account, name);
    serde_json::json!({
        "account": account,
        "name": name,
        "durable": attributes.durable,
        "queue": attributes.queue.is_some(),
        "visibility_timeout_seconds": attributes.queue.map(|policy| policy.visibility_seconds()),
        "max_deliveries": attributes.queue.map(|policy| policy.max_deliveries),
        "autokey": attributes.autokey,
        "directory": attributes.is_directory(),
        "path": db
            .directory_root(account, name)
            .ok()
            .map(|root| root.to_string_lossy().into_owned()),
    })
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
    if db.is_table_directory_for_account(acc, table_name) {
        return directory_read(db, acc, table_name, req);
    }
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
    if db.is_table_directory_for_account(acc, table_name) {
        return match directory_listing(db, acc, table_name, req) {
            Ok(records) => Response {
                status: "OK".to_string(),
                results: Some(records.into_iter().map(directory_row).collect()),
                ..Default::default()
            },
            Err(resp) => resp,
        };
    }
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
    let condition = match requested_condition(&req) {
        Ok(condition) => condition,
        Err(resp) => return resp,
    };
    if db.is_table_directory_for_account(acc, &table_name) {
        if let Err(resp) = directory_takes_no_condition(&table_name, &condition) {
            return resp;
        }
        return directory_write(db, acc, &table_name, req);
    }
    // Resolved once, and held: deserialization needs the dictionary and the
    // write needs the records. Resolving a second time would take this file's
    // lock again - and on a file several connections are writing at once, that
    // is the contended one.
    let handle = match resolve_file(db, acc, &table_name) {
        Ok(handle) => handle,
        Err(resp) => return resp,
    };

    let is_dict = req.is_dict.unwrap_or(false);
    // An empty key is a missing one rather than a request to mint: a client
    // that meant to ask the server for a key omits the field, and one that
    // built the key and got an empty string has a bug this should report.
    if req.key.as_deref().is_some_and(str::is_empty) {
        return error(ErrorCode::MissingField, "Key not specified");
    }

    let record = match record_from(db, &handle, req.data, req.structured_data) {
        Ok(record) => record,
        Err(resp) => return resp,
    };

    match db.write_record_in(
        acc,
        &table_name,
        &handle,
        req.key.as_deref(),
        record,
        is_dict,
        &condition,
    ) {
        // The key is reported only when the server chose it. Echoing one the
        // client already sent would make every reply carry a field that means
        // something on one write in a hundred.
        Ok(written) => Response {
            status: "OK".to_string(),
            key: written.minted.then_some(written.key),
            version: Some(written.version),
            ..Default::default()
        },
        Err(e) => write_error(e),
    }
}

/// A write that has been authorised and reserved, waiting for its bytes.
///
/// Handed to [`crate::server::transfer`], which moves the body off the socket
/// and hands it back to [`commit_bytes`]. It carries the resolved account
/// rather than the request, because by the time the body has arrived the
/// authorisation is a decision already made and must not be made again against
/// a client whose permissions changed mid-transfer.
pub struct StagedWrite {
    pub account: String,
    pub file: String,
    pub key: String,
    /// Bytes the client said it would send. The body is read to exactly this.
    pub length: u64,
    /// The temporary the body is written into, inside the file's own directory
    /// so an abandoned transfer is swept by the ordinary read path.
    pub staged: std::path::PathBuf,
}

/// A record opened for streaming out, with the length that same handle carries.
pub struct OpenedRecord {
    pub file: std::fs::File,
    pub length: u64,
}

/// The account a transfer runs in, or the refusal to send back.
///
/// The same rules the ordinary commands use, through the same helper, so a
/// transfer cannot become a way to reach an account a `READ` could not.
#[allow(clippy::result_large_err)]
fn transfer_account<'a>(req: &'a Request, client_info: &'a crate::db::ClientInfo) -> Result<&'a str, Response> {
    match allowed_account(req, client_info) {
        Some(account) => Ok(account),
        None => match req.account.as_deref() {
            Some(account) => Err(error(
                ErrorCode::AccessDenied,
                format!("Access denied for account {}: Not in allowed list", account),
            )),
            None => Err(error(ErrorCode::AccountNotSpecified, "Account not specified")),
        },
    }
}

/// Everything a `PUT.BYTES` can be refused for before a byte of it is read.
///
/// The point of doing it all here is that a transfer the server will not accept
/// costs nothing on the wire: the account, the file's type, the key and the
/// announced length are all decided from the request line alone.
#[allow(clippy::result_large_err)]
pub fn stage_bytes(req: &Request, db: &SharedDb, client_info: &crate::db::ClientInfo) -> Result<StagedWrite, Response> {
    let account = transfer_account(req, client_info)?.to_string();
    let file = requested_file(req)?.to_string();
    let key = match req.key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => key.to_string(),
        None => return Err(error(ErrorCode::MissingField, "Key not specified")),
    };
    let length = match req.length {
        Some(length) => length,
        None => {
            return Err(error(
                ErrorCode::MissingField,
                "length not specified: PUT.BYTES announces how many bytes of body follow it",
            ));
        }
    };
    let db = read_lock(db);
    if !db.is_table_directory_for_account(&account, &file) {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!(
                "'{}' is not a directory file: PUT.BYTES stores a record that is its bytes, and an ordinary \
                 file's records are fields inside a hashed section",
                file
            ),
        ));
    }
    match db.stage_directory_record(&account, &file, &key, length) {
        Ok(staged) => Ok(StagedWrite {
            account,
            file,
            key,
            length,
            staged,
        }),
        Err(e) => Err(db_error(e)),
    }
}

/// Puts a staged body in place once it has all arrived.
pub fn commit_bytes(staged: &StagedWrite, arrived: u64, db: &SharedDb) -> Response {
    let result = read_lock(db).commit_directory_record(
        &staged.account,
        &staged.file,
        &staged.key,
        &staged.staged,
        arrived,
        staged.length,
    );
    match result {
        Ok(()) => Response {
            status: "OK".to_string(),
            length: Some(arrived),
            ..Default::default()
        },
        Err(e) => db_error(e),
    }
}

/// Opens the record a `GET.BYTES` names, having checked everything a `READ` of
/// it would be checked for.
#[allow(clippy::result_large_err)]
pub fn open_bytes(req: &Request, db: &SharedDb, client_info: &crate::db::ClientInfo) -> Result<OpenedRecord, Response> {
    let account = transfer_account(req, client_info)?;
    let file = requested_file(req)?;
    let key = match req.key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => key,
        None => return Err(error(ErrorCode::MissingField, "Key not specified")),
    };
    let db = read_lock(db);
    if !db.is_table_directory_for_account(account, file) {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!(
                "'{}' is not a directory file: GET.BYTES sends a record that is its bytes, and an ordinary \
                 file's records are fields inside a hashed section",
                file
            ),
        ));
    }
    match db.open_directory_record(account, file, key) {
        Ok(Some((file, length))) => Ok(OpenedRecord { file, length }),
        Ok(None) => Err(error(ErrorCode::RecordNotFound, "Record not found")),
        Err(e) => Err(db_error(e)),
    }
}

/// What a byte-transfer command answers when it arrives anywhere but on a
/// connection that can carry its body.
///
/// `PUT.BYTES` and `GET.BYTES` are intercepted by the connection loop, which is
/// the only place with the socket in hand. Reaching the ordinary dispatch means
/// an in-process caller or a client library that sent the line and nothing
/// else, and saying so beats `UNKNOWN_COMMAND` - the command exists, and this
/// is not where it works.
fn transfer_command_elsewhere(command: &str) -> Response {
    error(
        ErrorCode::InvalidRequest,
        format!(
            "{} carries raw bytes on the connection itself, so it is only available over the remote protocol \
             and only to a client that reads or writes the body it announces",
            command
        ),
    )
}

/// `READ` against a directory file: the record's bytes, exactly as they are on
/// disk.
///
/// The reply's `record` is the *value* rather than an object of field names,
/// because a directory file has no field names to key it by - the whole record
/// is its bytes. It is a JSON string when those bytes are text and the
/// `{"$base64": "..."}` envelope when they are not, which is the same pair of
/// shapes a sub-value already travels in, so a client that learned the envelope
/// for step 1 needs nothing new for this.
fn directory_read(db: &Database, acc: &str, name: &str, req: &Request) -> Response {
    if let Err(resp) = directory_has_no_dictionary(name, req.is_dict.unwrap_or(false)) {
        return resp;
    }
    let key = match req.key.as_deref() {
        Some(key) => key,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    match db.read_directory_record(acc, name, key) {
        Ok(Some(bytes)) => Response {
            status: "OK".to_string(),
            record: Some(Database::bytes_to_json(&bytes)),
            ..Default::default()
        },
        Ok(None) => error(ErrorCode::RecordNotFound, "Record not found"),
        Err(e) => db_error(e),
    }
}

/// `WRITE` against a directory file: exactly these bytes become the file.
///
/// `structured_data` is refused rather than flattened. It describes a record in
/// fields, and a directory file has none; writing the flattened form would
/// store marks the caller never asked for and read back as something else.
fn directory_write(db: &Database, acc: &str, name: &str, req: Request) -> Response {
    if let Err(resp) = directory_has_no_dictionary(name, req.is_dict.unwrap_or(false)) {
        return resp;
    }
    let key = match req.key.as_deref() {
        Some(key) => key,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    if req.structured_data.is_some() {
        return error(
            ErrorCode::InvalidData,
            "A directory file's record is its bytes, not fields: send them in data, as a string or a \
             {\"$base64\": \"...\"} envelope",
        );
    }
    let bytes: Vec<u8> = match req.data.as_ref() {
        None => return error(ErrorCode::MissingField, "Data not specified"),
        Some(value) => match Database::bytes_from_json(value) {
            Some(bytes) => bytes,
            None => {
                return error(
                    ErrorCode::InvalidData,
                    "A directory file's record is its bytes: send a string, or a {\"$base64\": \"...\"} \
                     envelope holding valid base64",
                );
            }
        },
    };
    match db.write_directory_record(acc, name, key, &bytes) {
        Ok(()) => Response {
            status: "OK".to_string(),
            ..Default::default()
        },
        Err(e) => db_error(e),
    }
}

/// The keys of a directory file, for the commands that enumerate rather than
/// fetch.
///
/// A criterion, a sort and an explode are each refused rather than ignored. All
/// three are read against a dictionary field, a directory file has none, and a
/// `WITH` clause that quietly matched everything would be a wrong answer
/// delivered with `status: "OK"`.
#[allow(clippy::result_large_err)]
fn directory_listing(
    db: &Database,
    acc: &str,
    name: &str,
    req: &Request,
) -> Result<Vec<crate::db::DirectoryRecord>, Response> {
    directory_has_no_dictionary(name, req.is_dict.unwrap_or(false))?;
    let named = [
        req.query_node.is_some().then_some("query_node"),
        req.query_string
            .as_deref()
            .filter(|q| !q.trim().is_empty())
            .map(|_| "query_string"),
        req.sort_specs
            .as_ref()
            .filter(|specs| !specs.is_empty())
            .map(|_| "sort_specs"),
        req.explode
            .as_ref()
            .filter(|fields| !fields.is_empty())
            .map(|_| "explode"),
    ];
    if let Some(clause) = named.into_iter().flatten().next() {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!(
                "'{}' is a directory file: its records are host files with no fields, so {} has nothing to \
                 read - the keys come back in name order",
                name, clause
            ),
        ));
    }
    db.directory_records(acc, name).map_err(db_error)
}

/// One enumerated record as a result row: the key and how large it is, never
/// its bytes.
///
/// Listing a file of scans must not cost the scans, which it would if a row
/// carried the content. `READ` fetches one record and `EXTRACT` streams one to
/// a file; those are the two ways bytes leave a directory file, and both name
/// the record they are about.
fn directory_row(record: crate::db::DirectoryRecord) -> (String, serde_json::Value) {
    (record.key, serde_json::json!({ "size": record.bytes }))
}

/// Refuses `is_dict` against a directory file.
///
/// A dictionary describes fields, and a directory file's records have none, so
/// `is_dict` there names a section that exists on disk and governs nothing. A
/// caller acting on what it read back would be acting on a promise that is not
/// kept.
#[allow(clippy::result_large_err)]
fn directory_has_no_dictionary(name: &str, is_dict: bool) -> Result<(), Response> {
    if !is_dict {
        return Ok(());
    }
    Err(error(
        ErrorCode::InvalidRequest,
        format!(
            "'{}' is a directory file: its records are host files with no fields, so it has no dictionary \
             to read or write",
            name
        ),
    ))
}

/// The record a `WRITE`-shaped request describes, in the file's own terms.
///
/// Shared by `WRITE` and by each write of a `TRANSACT` set, so the two accept
/// exactly the same shapes: a display string, an object of field names, or the
/// `structured_data` spelling of the second. It takes the handle the caller has
/// already resolved rather than the file's name, because resolving it a second
/// time would take the lock of the very file several connections are writing to
/// at once - and it takes the handle rather than a locked table because a
/// display string needs no dictionary and so should cost no lock at all.
#[allow(clippy::result_large_err)]
fn record_from(
    db: &Database,
    handle: &TableHandle,
    data: Option<serde_json::Value>,
    structured_data: Option<serde_json::Value>,
) -> Result<Record, Response> {
    if let Some(structured) = structured_data {
        return db
            .deserialize_record_in(&handle.read(), &structured)
            .ok_or_else(|| error(ErrorCode::InvalidData, "Invalid structured data"));
    }
    match data {
        Some(serde_json::Value::String(text)) => Ok(Record::from_display_string(&text)),
        Some(object @ serde_json::Value::Object(_)) => db
            .deserialize_record_in(&handle.read(), &object)
            .ok_or_else(|| error(ErrorCode::InvalidData, "Invalid structured data in data field")),
        Some(_) => Err(error(
            ErrorCode::InvalidData,
            "Invalid data type in data field: expected string or object",
        )),
        None => Err(error(ErrorCode::MissingField, "Data not specified")),
    }
}

/// Turns the `changes` array into the engine's own change set.
///
/// Nothing is applied here: this only fails, and it fails on the whole set. A
/// change that cannot be read is a set that is refused entire, which is the
/// same promise the apply itself makes and the reason the two halves are
/// written this way round.
#[allow(clippy::result_large_err)]
fn transaction_changes(db: &Database, acc: &str, specs: Vec<ChangeSpec>) -> Result<Vec<Change>, Response> {
    // One handle per file however many changes name it: resolving is what loads
    // the file, and a set of fifty changes to one file should load it once.
    let mut handles: HashMap<String, TableHandle> = HashMap::new();
    let mut changes = Vec::with_capacity(specs.len());

    for (position, spec) in specs.into_iter().enumerate() {
        // The position, because a set is refused as a whole and "file not
        // specified" says nothing about which of thirty changes is at fault.
        let at = |what: &str| format!("Change {}: {}", position + 1, what);
        let file = spec
            .file
            .filter(|name| !name.is_empty())
            .ok_or_else(|| error(ErrorCode::MissingField, at("file not specified")))?;
        let key = spec
            .key
            .filter(|key| !key.is_empty())
            .ok_or_else(|| error(ErrorCode::MissingField, at("key not specified")))?;
        let is_dict = spec.is_dict.unwrap_or(false);

        let op = match spec.op.as_deref().unwrap_or_default().to_uppercase().as_str() {
            "DELETE" => ChangeOp::Delete,
            "WRITE" => {
                let handle = match handles.get(&file) {
                    Some(handle) => handle.clone(),
                    None => {
                        let handle = resolve_file(db, acc, &file)?;
                        handles.insert(file.clone(), handle.clone());
                        handle
                    }
                };
                let record = record_from(db, &handle, spec.data, spec.structured_data).map_err(|resp| {
                    error(
                        resp.code.unwrap_or(ErrorCode::InvalidData),
                        at(&resp.message.unwrap_or_default()),
                    )
                })?;
                ChangeOp::Write(record)
            }
            other => {
                return Err(error(
                    ErrorCode::InvalidData,
                    at(&format!("'{}' is not an operation; use WRITE or DELETE", other)),
                ));
            }
        };
        changes.push(Change { file, key, is_dict, op });
    }
    Ok(changes)
}

/// `TRANSACT`: a set of writes and deletes across the files of one account,
/// applied whole or not at all. See `docs/protocol.md` and
/// [`crate::db::engine::transaction`].
fn transact(db: &Database, acc: &str, req: Request) -> Response {
    let specs = match req.changes {
        Some(specs) if !specs.is_empty() => specs,
        _ => return error(ErrorCode::MissingField, "Changes not specified"),
    };
    // Checked before the files are resolved: a set the server will not apply is
    // not worth loading thirty files for.
    if specs.len() > crate::db::MAX_CHANGES {
        return error(
            ErrorCode::TransactionScope,
            format!(
                "A transaction may carry at most {} changes, and this one carries {}",
                crate::db::MAX_CHANGES,
                specs.len()
            ),
        );
    }
    let changes = match transaction_changes(db, acc, specs) {
        Ok(changes) => changes,
        Err(resp) => return resp,
    };
    match db.apply_transaction(acc, changes) {
        Ok(applied) => Response {
            status: "OK".to_string(),
            count: Some(applied),
            ..Default::default()
        },
        Err(e) => db_error_in("Transaction error", e),
    }
}

fn delete_record(db: &Database, acc: &str, req: Request) -> Response {
    let table_name = match requested_file(&req) {
        Ok(name) => name.to_string(),
        Err(resp) => return resp,
    };
    let condition = match requested_condition(&req) {
        Ok(condition) => condition,
        Err(resp) => return resp,
    };
    let key = match req.key {
        Some(k) => k,
        None => return error(ErrorCode::MissingField, "Key not specified"),
    };
    let is_dict = req.is_dict.unwrap_or(false);
    if db.is_table_directory_for_account(acc, &table_name) {
        if let Err(resp) = directory_has_no_dictionary(&table_name, is_dict) {
            return resp;
        }
        if let Err(resp) = directory_takes_no_condition(&table_name, &condition) {
            return resp;
        }
        return match db.delete_directory_record(acc, &table_name, &key) {
            Ok(true) => Response {
                status: "OK".to_string(),
                ..Default::default()
            },
            Ok(false) => error(ErrorCode::RecordNotFound, "Record not found"),
            Err(e) => db_error(e),
        };
    }

    let handle = match resolve_file(db, acc, &table_name) {
        Ok(handle) => handle,
        Err(resp) => return resp,
    };
    match db.delete_record_in(acc, &table_name, &handle, &key, is_dict, &condition) {
        // An unconditional delete of a key that is not there still answers OK,
        // as it always has: the caller asked for the record to be gone and it
        // is. A conditional one never reaches here on a missing record - the
        // condition refused it first, which is the difference between "already
        // done" and "not the record you read".
        Ok(_) => Response {
            status: "OK".to_string(),
            ..Default::default()
        },
        Err(e) => write_error(e),
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

/// The condition a `WRITE` or `DELETE` request attaches to itself.
///
/// Naming both is refused rather than settled: `if_absent` says the key holds
/// nothing and `if_match` says it holds one particular thing, so a request
/// carrying both has contradicted itself, and guessing which half was meant is
/// how a caller ends up trusting a check that never ran.
///
/// `if_absent: false` is the same as omitting it - the client asked for no
/// condition. Reading it as "the record must exist" would invent a third
/// condition nobody named.
#[allow(clippy::result_large_err)]
fn requested_condition(req: &Request) -> Result<Condition, Response> {
    match (req.if_absent.unwrap_or(false), req.if_match.as_deref()) {
        (true, Some(_)) => Err(error(
            ErrorCode::InvalidRequest,
            "if_absent and if_match contradict each other: one says the key holds nothing, the other says which \
             record it holds",
        )),
        (true, None) => Ok(Condition::IfAbsent),
        (false, Some("")) => Err(error(
            ErrorCode::InvalidData,
            "if_match needs the version a READ reported; an empty one matches nothing",
        )),
        (false, Some(version)) => Ok(Condition::IfMatch(version.to_string())),
        (false, None) => Ok(Condition::Always),
    }
}

/// The reply for an error out of the write path.
///
/// A write that was *refused* - a condition that did not hold, a keyless write
/// to a file that does not mint keys - saved nothing and failed at nothing, so
/// prefixing it with "Save error" would describe it to a person as the one
/// thing it is not. Only a genuine failure to store the record gets that
/// context; the code is right either way.
fn write_error(e: DbError) -> Response {
    match e {
        refused @ (DbError::PreconditionFailed(_) | DbError::InvalidRequest(_)) => db_error(refused),
        failed => db_error_in("Save error", failed),
    }
}

/// Refuses a condition on a directory file, whose records are host files.
///
/// A directory record is a file on the host, changed by whatever else has that
/// path open, so a version this server minted would be a promise about bytes it
/// does not control. Said plainly rather than checked and hoped for: the point
/// of a condition is that it holds.
#[allow(clippy::result_large_err)]
fn directory_takes_no_condition(name: &str, condition: &Condition) -> Result<(), Response> {
    if condition.is_unconditional() {
        return Ok(());
    }
    Err(error(
        ErrorCode::InvalidRequest,
        format!(
            "'{}' is a directory file: its records are host files that anything on the machine can change, so a \
             version taken from one promises nothing",
            name
        ),
    ))
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
            // Sent on every read rather than on request: it is a hash of the
            // bytes the read already had in hand and already serialized, so
            // asking for it would be a second round trip to save nothing, and a
            // client that only finds out it needed one after the fact would
            // have to read twice.
            version: Some(record.version()),
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

    let (query_node, sort_specs, explode_specs) = match resolve_clause(db, table_name, req) {
        Ok(clause) => clause,
        Err(response) => return response,
    };
    // What the clause named, resolved against this file's dictionary: a lone
    // field, or the association group it belongs to.
    let explode = match Database::resolve_explode_in(table, &explode_specs) {
        Ok(target) => target,
        Err(message) => return error(ErrorCode::InvalidQuery, message),
    };

    // Resolve the dictionary once for the whole result set rather than per record.
    let schema = db.record_schema(table);

    if query_node.is_none() && explode_specs.is_empty() {
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
    Database::sort_entries_in(table, &mut rows, &sort_specs, explode.as_ref());

    // A clause that named a field the dictionary does not know still asked for
    // an exploded result, and gets one - of `null` positions.
    let exploded = !explode_specs.is_empty();
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

/// What a QUERY or SELECT selects: the criteria, the ordering, and the fields
/// whose values become rows of their own.
///
/// The explode specs are left unresolved here because resolving them needs the
/// file's dictionary, which a caller holding the table has and this does not.
type Clause = (Option<QueryNode>, Vec<SortSpec>, Vec<ExplodeSpec>);

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
    let mut explode: Vec<ExplodeSpec> = req
        .explode
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|field_name| ExplodeSpec {
            field_name,
            condition: None,
        })
        .collect();

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
        if explode.is_empty() {
            for spec in &parsed_explodes {
                query_node = Database::and_condition(query_node, spec.condition.clone());
            }
            explode = parsed_explodes;
        }
    }

    Ok((query_node, sort_specs, explode))
}

/// Handles a request against an exclusively borrowed database. Commands that
/// only read still go through here whenever the shared path could not serve
/// them, for instance because the table had to be loaded first.
pub fn handle_request_locked(req: Request, db: &mut Database, client_info: &crate::db::ClientInfo) -> Response {
    let command = req.command.to_uppercase();

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
        } else if command == "GET.NEXT" {
            // GET.NEXT takes its account from the select list, so there is
            // nothing to resolve from the request and nothing to refuse. Any
            // client with more than one allowed account would otherwise be
            // turned away here for not naming what it does not need to name.
            None
        } else {
            return error(ErrorCode::AccountNotSpecified, "Account not specified");
        }
    };

    let acc = match target_account {
        Some(ref a) => a.as_str(),
        None => "", // Some commands might not need an account, or will fail later
    };

    match command.as_str() {
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
        "TRANSACT" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            transact(db, acc, req)
        }
        // Listed here so the command exists everywhere the protocol says it
        // does, and is documented like any other; the connection loop takes it
        // before this is reached, because only it has the socket.
        "PUT.BYTES" | "GET.BYTES" => transfer_command_elsewhere(&command),
        // The queue commands, for the same callers as the record arms above:
        // the shared path in [`handle_request`] serves them whenever it can
        // resolve the account on its own.
        "ENQUEUE" | "DEQUEUE" | "ACK" | "NACK" | "PEEK" => {
            if target_account.is_none() {
                return error(ErrorCode::AccountNotSpecified, "Account not specified");
            }
            let command = req.command.to_uppercase();
            let owner = client_info.name.clone();
            record_command(&command, req, db, acc, &owner)
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

            // A directory file's keys come from the host directory rather than
            // from a table, and the list holds nothing else: `GET.NEXT` reads
            // each record's size back off the disk when it pages them, so a
            // list taken now does not pin bytes that may be rewritten before it
            // is read.
            if db.is_table_directory_for_account(acc, &table_name) {
                let records = match directory_listing(db, acc, &table_name, &req) {
                    Ok(records) => records,
                    Err(resp) => return resp,
                };
                let list = crate::db::SelectList::from_keys(
                    table_name,
                    false,
                    records.into_iter().map(|record| record.key).collect(),
                );
                let count = list.len();
                db.remote_select_lists
                    .insert(list_name, crate::db::RemoteSelectList::new(acc.to_string(), list));
                return Response {
                    status: "OK".to_string(),
                    count: Some(count),
                    ..Default::default()
                };
            }

            let (query_node, sort_specs, explode_specs) = match resolve_clause(db, &table_name, &req) {
                Ok(clause) => clause,
                Err(response) => return response,
            };

            if let Err(e) = db.get_table_mut_for_account(acc, &table_name) {
                return db_error(e);
            }
            let entries = match db.get_table_read_only_for_account(acc, &table_name) {
                Some(handle) => {
                    let table = handle.read();
                    let explode = match Database::resolve_explode_in(&table, &explode_specs) {
                        Ok(target) => target,
                        Err(message) => return error(ErrorCode::InvalidQuery, message),
                    };
                    Database::select_entries_in(
                        &table,
                        is_dict,
                        query_node.as_ref(),
                        explode.as_ref(),
                        None,
                        &sort_specs,
                    )
                }
                None => return error(ErrorCode::FileNotFound, format!("File '{}' is not loaded", table_name)),
            };

            let list = crate::db::SelectList {
                table_name,
                is_dict,
                // One member names the whole group, and the group is re-resolved
                // from the dictionary when the list is read back - so a saved
                // list keeps its shape and follows a dictionary that has moved on.
                explode_field: explode_specs.into_iter().next().map(|spec| spec.field_name),
                entries,
            };
            let count = list.len();
            // The account is stored with the list, because it is what the list's
            // keys mean: they were chosen from this file in this account, and
            // reading them against another one answers a question nobody asked.
            db.remote_select_lists
                .insert(list_name, crate::db::RemoteSelectList::new(acc.to_string(), list));

            Response {
                status: "OK".to_string(),
                count: Some(count),
                ..Default::default()
            }
        }
        "GET.NEXT" => {
            let list_name = req.list_name.unwrap_or_else(|| "DEFAULT".to_string());
            let batch_size = req.batch_size.unwrap_or(1);

            // The account comes from the list rather than from this request: the
            // list's keys were chosen from one file in one account, and that is
            // the only account they mean anything in.
            let list_account = match db.remote_select_lists.get(&list_name) {
                Some(l) => l.account.clone(),
                None => {
                    return error(ErrorCode::SelectListNotFound, "Select list not found");
                }
            };

            // Which is why it is checked here rather than trusted. The lists are
            // held by name across every connection, so without this a client
            // could page one somebody else selected in an account it may not
            // reach - the account check at the top of this function never saw it,
            // because the request never named the account.
            if !client_info.is_admin && !client_info.allowed_accounts.contains(&list_account) {
                let msg = format!("Access denied for account {}: Not in allowed list", list_account);
                let _ = db.log_error("REMOTE", &msg);
                return error(ErrorCode::AccessDenied, msg);
            }

            // A request naming some other account is not describing this list.
            // Paging it anyway would answer from a file the caller did not ask
            // about, and silently, so it is refused instead.
            if let Some(named) = req.account.as_deref()
                && named != list_account
            {
                return error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "Select list '{}' was selected in account '{}', not '{}'",
                        list_name, list_account, named
                    ),
                );
            }

            let (entries_batch, table_name, is_dict) = {
                let held = db.remote_select_lists.get_mut(&list_name).expect("checked above");

                let list_len = held.list.len();
                let table_name = held.list.table_name.clone();
                let is_dict = held.list.is_dict;

                if held.cursor >= list_len {
                    return Response {
                        status: "EOF".to_string(),
                        ..Default::default()
                    };
                }

                let end = std::cmp::min(held.cursor + batch_size, list_len);
                let entries = held.list.entries[held.cursor..end].to_vec();
                held.cursor = end;
                (entries, table_name, is_dict)
            };

            let acc = list_account.as_str();
            // The same rule the listing commands follow: a row says how large a
            // record is and never what is in it, so paging a file of scans
            // costs a `stat` per row rather than the scans.
            if db.is_table_directory_for_account(acc, &table_name) {
                let mut results_processed = Vec::with_capacity(entries_batch.len());
                for entry in &entries_batch {
                    let size = match db.read_directory_size(acc, &table_name, &entry.key) {
                        Ok(Some(size)) => size,
                        // Deleted since the list was taken. Skipped rather than
                        // reported as zero bytes, which would read as an empty
                        // record that is still there.
                        Ok(None) => continue,
                        Err(e) => return db_error(e),
                    };
                    results_processed.push(directory_row(crate::db::DirectoryRecord {
                        key: entry.key.clone(),
                        bytes: size,
                    }));
                }
                let results_len = results_processed.len();
                return Response {
                    status: "OK".to_string(),
                    results: Some(results_processed),
                    count: Some(results_len),
                    ..Default::default()
                };
            }
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
            let name = match req.file.clone() {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File name not specified");
                }
            };
            let attributes = match file_attributes(&req, crate::db::FileAttributes::default()) {
                Ok(attributes) => attributes,
                Err(resp) => return resp,
            };
            match db.create_table_with(acc, &name, attributes) {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    record: Some(file_attributes_json(db, acc, &name)),
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
            let name = match req.file.clone() {
                Some(n) => n,
                None => {
                    return error(ErrorCode::MissingField, "File name not specified");
                }
            };
            // At least one of them, and only the ones named are changed: an
            // omitted flag would otherwise quietly demote a file the caller
            // only meant to promote, or turn a queue back into a plain file
            // because the request was about durability.
            if req.durable.is_none()
                && req.queue.is_none()
                && req.autokey.is_none()
                && req.visibility_timeout.is_none()
                && req.max_deliveries.is_none()
                && req.directory.is_none()
                && req.path.is_none()
            {
                return error(
                    ErrorCode::MissingField,
                    "Nothing to set: name durable, autokey, queue, visibility_timeout or max_deliveries",
                );
            }
            let current = db.file_attributes_for_account(acc, &name);
            let attributes = match file_attributes(&req, current) {
                Ok(attributes) => attributes,
                Err(resp) => return resp,
            };
            match db.set_file_attributes_for_account(acc, &name, attributes, "attributes") {
                Ok(_) => Response {
                    status: "OK".to_string(),
                    record: Some(file_attributes_json(db, acc, &name)),
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
            // durability and the queue flag are answerable without reading the
            // account's DIR file.
            let files = db.list_tables_with_attributes_for_account(acc);
            let count = files.len();
            let keys = files.iter().map(|(name, _)| name.clone()).collect();
            let results = files
                .into_iter()
                .map(|(name, attributes)| {
                    // The cheap verdict - section metadata and index `state`
                    // files, no group trailers and no records - so a problem
                    // file is findable without opening every file in turn. The
                    // queue's own numbers cost a load and belong to
                    // `FILE.STATS`; the flag is free and belongs here.
                    let health = db.file_health_summary(acc, &name);
                    let value = serde_json::json!({
                        "durable": attributes.durable,
                        "queue": attributes.queue.is_some(),
                        // Whether a keyless WRITE works on it, which is the one
                        // thing a client has to know before trying one - and
                        // free here for the same reason the queue flag is.
                        "autokey": attributes.autokey,
                        // The type, so a client knows which commands the file
                        // answers before it tries one. Free here: it is read
                        // off the same DIR entry the other flags are.
                        "directory": attributes.is_directory(),
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
                // What an operator needs before swapping the image over a
                // mounted volume: what the directory is at, and what this build
                // will open. Asking the running server beats reading a file
                // inside a container or trusting a tag to mean a version.
                object.insert(
                    "storage_format".to_string(),
                    serde_json::json!(crate::db::format::CURRENT),
                );
                object.insert(
                    "storage_format_oldest_supported".to_string(),
                    serde_json::json!(crate::db::format::OLDEST_SUPPORTED),
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
