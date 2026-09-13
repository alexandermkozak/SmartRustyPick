//! Routes dashboard requests onto remote-protocol commands.
//!
//! Every endpoint here is a thin translation: an HTTP shape in, one protocol
//! command out, the protocol's own answer back. Nothing reaches into the engine
//! directly, so the dashboard cannot do anything a certificate holder on the
//! TCP listener could not, and a permission decision is made in exactly one
//! place - the command handler.

use crate::server::models::ErrorCode;
use crate::web::client::ProtocolClient;
use crate::web::http::{Request, Response};
use serde_json::{Value, json};
use std::sync::Arc;

/// Maps a protocol error onto the status code that describes it.
///
/// The protocol has one error status; a browser client wants to tell "you may
/// not" apart from "that does not exist", and the error code is what says
/// which. This used to read the message instead, which made the wording of
/// every refusal part of the dashboard's interface. A response with no code -
/// from a server older than the codes - is a plain bad request, as it was
/// before.
fn status_for(code: Option<ErrorCode>) -> u16 {
    match code {
        Some(ErrorCode::AdminRequired | ErrorCode::AccessDenied | ErrorCode::Deauthorized) => 403,
        Some(ErrorCode::PermissionDenied | ErrorCode::AccountProtected) => 403,
        Some(
            ErrorCode::AccountNotFound
            | ErrorCode::FileNotFound
            | ErrorCode::RecordNotFound
            | ErrorCode::IndexNotFound
            | ErrorCode::SelectListNotFound
            | ErrorCode::ClientNotFound,
        ) => 404,
        Some(ErrorCode::AccountExists | ErrorCode::FileExists | ErrorCode::IndexExists) => 409,
        // The database failed rather than the request: a corrupt file or a disk
        // that will not take the write is not something the browser can fix.
        Some(ErrorCode::CorruptData | ErrorCode::IoError) => 500,
        Some(ErrorCode::Unavailable) => 503,
        _ => 400,
    }
}

/// Runs one command and turns its response into an HTTP response.
async fn run(client: &ProtocolClient, payload: Value) -> Response {
    match client.request(payload).await {
        Ok(response) => reply(response),
        // The database is the dashboard's upstream, so an unreachable one is a
        // gateway failure rather than the browser's fault.
        Err(e) => Response::error(502, format!("Database unreachable: {}", e)),
    }
}

/// One protocol response as an HTTP one: the envelope when it succeeded, and
/// the engine's own error code mapped to a status when it did not.
///
/// Shared by the ordinary commands and by the archive upload, which cannot go
/// through [`run`] - its request carries a body - but must answer a refusal
/// identically. A second copy of this would be a second place for the status
/// mapping to drift.
fn reply(response: Value) -> Response {
    let status = response.get("status").and_then(Value::as_str).unwrap_or("ERROR");
    if status == "OK" {
        return Response::json(200, &response);
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Request failed");
    let code = response
        .get("code")
        .and_then(Value::as_str)
        .and_then(ErrorCode::from_wire);
    Response::error(status_for(code), message)
}

/// A required string field of a JSON request body.
fn field<'a>(body: &'a Value, name: &str) -> Option<&'a str> {
    body.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// A list of names - accounts or capabilities - accepting both `["A","B"]` and
/// `"A, B"` so the page can send whichever it has.
fn accounts(body: &Value, name: &str) -> Vec<String> {
    match body.get(name) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        Some(Value::String(text)) => text
            .split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// A list of strings from a JSON body, empty when there is none.
///
/// Unlike [`accounts`] this does not split a string on commas and does not drop
/// blanks: an index exclusion may legitimately be the empty value - a sparse
/// field most records do not carry - and it may hold a comma. The only thing
/// that can say what a value is, is the caller.
fn values(body: &Value, name: &str) -> Vec<String> {
    match body.get(name) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A flag the caller must actually have sent. `SET.FILE` reads an absent
/// durability flag as a mistake rather than as "off", so the endpoint has to
/// tell "false" and "not there" apart before it forwards anything.
/// A whole number from the body, accepting the string a form field holds as
/// well as a JSON number.
///
/// `None` for absent, null, empty or unparsable - the database is the one place
/// a queue's timeout and delivery limit are judged, and a value it would refuse
/// is better refused there, with its reason, than silently dropped here.
fn number(body: &Value, name: &str) -> Option<u64> {
    match body.get(name) {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

fn optional_flag(body: &Value, name: &str) -> Option<bool> {
    match body.get(name) {
        Some(Value::Null) | None => None,
        Some(_) => Some(flag(body, name)),
    }
}

/// The dictionary attributes of a `SET.DICT` body, without the entry's name.
///
/// The page sends one flat object; the protocol wants the name as the record's
/// key and the rest as the record. Copying the known attributes rather than
/// deleting `name` keeps anything else a caller sent out of the record.
fn dictionary_attributes(body: &Value) -> Value {
    let mut attributes = serde_json::Map::new();
    for name in [
        "field",
        "heading",
        "justification",
        "width",
        "association",
        "associationDepth",
        "conversion",
    ] {
        if let Some(value) = body.get(name) {
            attributes.insert(name.to_string(), value.clone());
        }
    }
    Value::Object(attributes)
}

fn flag(body: &Value, name: &str) -> bool {
    match body.get(name) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => matches!(text.to_ascii_uppercase().as_str(), "Y" | "YES" | "TRUE" | "1" | "ADMIN"),
        _ => false,
    }
}

/// Dispatches an authenticated `/api/...` request.
/// The largest archive the dashboard will move in either direction.
///
/// The dashboard holds a whole archive in memory while it is in flight - once
/// as it comes off the protocol connection and once as the HTTP body - because
/// neither this HTTP layer nor the protocol client streams. That is a fine
/// trade for the size of archive a person downloads through a browser, and a
/// bad one beyond it, so the bound is stated rather than discovered as a
/// dashboard that fell over.
///
/// Past it the archive is refused with the advice to use `EXPORT` / `IMPORT`
/// over the protocol or the CLI, which write to a path on the host and hold
/// nothing.
pub const MAX_DASHBOARD_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// `GET /api/archive` - an archive as a download.
///
/// The scope comes from the query, the same way the command takes it: `file`
/// for one file of `account`, `account` alone for that account, neither for
/// every account.
async fn export_archive(client: &Arc<ProtocolClient>, request: &Request) -> Response {
    let mut payload = json!({ "command": "EXPORT.BYTES" });
    let object = payload.as_object_mut().expect("built as an object");
    let account = request
        .query
        .get("account")
        .map(String::as_str)
        .filter(|a| !a.is_empty());
    let file = request.query.get("file").map(String::as_str).filter(|f| !f.is_empty());
    if let Some(account) = account {
        object.insert("target_account".to_string(), account.into());
    }
    if let Some(file) = file {
        object.insert("file".to_string(), file.into());
    }
    if file.is_some() && account.is_none() {
        return Response::error(400, "Exporting one file needs the account it belongs to");
    }

    let (response, body) = match client.request_with_body(payload, MAX_DASHBOARD_ARCHIVE_BYTES).await {
        Ok(answer) => answer,
        Err(e) => return Response::error(502, format!("Export failed: {}", e)),
    };
    if response.get("status").and_then(Value::as_str) != Some("OK") {
        // No body followed a refusal, so this is an ordinary JSON reply.
        return reply(response);
    }

    // `Cache-Control: no-store` is not set here: `write_response` puts it on
    // every dashboard response already, and setting it again would send the
    // header twice. It matters most for this endpoint - an archive is a
    // point-in-time copy of live data - which is why it is worth saying that it
    // is covered rather than leaving the absence to look like an oversight.
    Response::new(200, "application/octet-stream", body).with_header(
        "Content-Disposition",
        format!("attachment; filename=\"{}\"", archive_name(account, file)),
    )
}

/// `POST /api/archive` - a restore from an uploaded archive.
///
/// The options travel in the query rather than in the body, because the body is
/// the archive: there is nowhere else for them to go short of multipart, which
/// would be a parser this dashboard does not otherwise need.
async fn import_archive(client: &Arc<ProtocolClient>, request: &Request) -> Response {
    if request.body.is_empty() {
        return Response::error(400, "No archive was uploaded");
    }
    if request.body.len() as u64 > MAX_DASHBOARD_ARCHIVE_BYTES {
        return Response::error(
            413,
            format!(
                "The archive is larger than the {} MiB the dashboard will accept. Restore it with IMPORT over the                  protocol or the CLI, which reads it from a path on the server host",
                MAX_DASHBOARD_ARCHIVE_BYTES / (1024 * 1024)
            ),
        );
    }

    let mut payload = json!({ "command": "IMPORT.BYTES", "length": request.body.len() });
    let object = payload.as_object_mut().expect("built as an object");
    if let Some(into) = request.query.get("into").filter(|into| !into.is_empty()) {
        object.insert("target_account".to_string(), into.as_str().into());
    }
    // Both default to off, and absent means off. A restore that overwrote
    // because a query parameter was missing is the one failure the flag exists
    // to prevent.
    if request.query.get("overwrite").is_some_and(|value| flag_text(value)) {
        object.insert("overwrite".to_string(), true.into());
    }
    if request.query.get("verify").is_some_and(|value| flag_text(value)) {
        object.insert("dry_run".to_string(), true.into());
    }

    match client.request_sending_body(payload, &request.body).await {
        Ok(response) => reply(response),
        Err(e) => Response::error(502, format!("Import failed: {}", e)),
    }
}

/// A filename that says what is in the archive and when it was taken, so a
/// downloads folder with several of them is still readable.
fn archive_name(account: Option<&str>, file: Option<&str>) -> String {
    let safe = |text: &str| {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let stamp = {
        let now = time::OffsetDateTime::now_utc();
        format!(
            "{:04}{:02}{:02}-{:02}{:02}",
            now.year(),
            now.month() as u8,
            now.day(),
            now.hour(),
            now.minute()
        )
    };
    let what = match (account, file) {
        (Some(account), Some(file)) => format!("{}-{}", safe(account), safe(file)),
        (Some(account), None) => safe(account),
        _ => "database".to_string(),
    };
    format!("{}-{}.{}", what, stamp, crate::db::archive::EXTENSION)
}

/// A query parameter read as a flag. Present-and-true only: `?overwrite=false`
/// means what it says.
fn flag_text(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

pub async fn route(client: &Arc<ProtocolClient>, request: &Request) -> Response {
    let segments = request.segments();
    let method = request.method.as_str();
    let body = request.json().unwrap_or(Value::Null);

    match (method, segments.as_slice()) {
        // What the server is doing right now: uptime, totals and live sessions.
        ("GET", ["api", "stats"]) => run(client, json!({ "command": "SERVER.STATS" })).await,

        // Authorized clients.
        ("GET", ["api", "clients"]) => run(client, json!({ "command": "LIST.CONNS" })).await,
        ("POST", ["api", "clients"]) => {
            let name = match field(&body, "name") {
                Some(name) => name,
                None => return Response::error(400, "A client name is required"),
            };
            let thumbprint = match field(&body, "thumbprint") {
                Some(thumbprint) => thumbprint,
                None => return Response::error(400, "A certificate thumbprint is required"),
            };
            let is_admin = flag(&body, "is_admin");
            let allowed = accounts(&body, "accounts");
            // Unknown names are the server's to refuse, not this layer's: one
            // list of capabilities, checked in one place.
            let capabilities = accounts(&body, "capabilities");
            if !is_admin && allowed.is_empty() && capabilities.is_empty() {
                return Response::error(
                    400,
                    "A non-admin client needs at least one allowed account or capability",
                );
            }
            run(
                client,
                json!({
                    "command": "AUTHORIZE.CONN",
                    "name": name,
                    "thumbprint": thumbprint,
                    "accounts_list": allowed,
                    "is_admin": is_admin,
                    "capabilities": capabilities,
                }),
            )
            .await
        }
        ("DELETE", ["api", "clients", name]) => {
            run(client, json!({ "command": "DEAUTHORIZE.CONN", "name": name })).await
        }
        ("POST", ["api", "clients", name, "accounts"]) => {
            let allowed = accounts(&body, "accounts");
            if allowed.is_empty() {
                return Response::error(400, "At least one account is required");
            }
            let command = if flag(&body, "remove") {
                "REMOVE.CLIENT.ACCOUNT"
            } else {
                "ADD.CLIENT.ACCOUNT"
            };
            run(
                client,
                json!({ "command": command, "name": name, "accounts_list": allowed }),
            )
            .await
        }

        // Certificates: issued, authorized and handed back in one step.
        ("POST", ["api", "certificates"]) => {
            let common_name = match field(&body, "common_name") {
                Some(name) => name,
                None => return Response::error(400, "A common name is required"),
            };
            let is_admin = flag(&body, "is_admin");
            let allowed = accounts(&body, "accounts");
            let capabilities = accounts(&body, "capabilities");
            if !is_admin && allowed.is_empty() && capabilities.is_empty() {
                return Response::error(
                    400,
                    "A non-admin certificate needs at least one allowed account or capability",
                );
            }
            run(
                client,
                json!({
                    "command": "GENERATE.CERT",
                    "name": common_name,
                    "accounts_list": allowed,
                    "is_admin": is_admin,
                    "capabilities": capabilities,
                }),
            )
            .await
        }

        // Accounts and their files: what exists, how big it is, and how it is
        // stored. No endpoint here returns a stored *record*, which is the
        // point - the dashboard manages the database, it is not a second way to
        // read it. A file's dictionary is the exception that proves the rule:
        // it is the file's shape rather than its contents, and maintaining it
        // is why an operator opens a management interface at all.
        ("GET", ["api", "accounts"]) => run(client, json!({ "command": "LIST.ACCOUNTS" })).await,
        // One endpoint for both kinds of account, because the page is asking
        // for the same thing either way: an empty one, or the demo fixture the
        // CLI's CREATE.TEST.ACCOUNT populates.
        ("POST", ["api", "accounts"]) => {
            let name = match field(&body, "name") {
                Some(name) => name,
                None => return Response::error(400, "An account name is required"),
            };
            let command = if flag(&body, "demo") {
                "CREATE.TEST.ACCOUNT"
            } else {
                "CREATE.ACCOUNT"
            };
            run(client, json!({ "command": command, "target_account": name })).await
        }
        // Dropping an account deletes every file in it. The confirmation is the
        // page's job; the database refuses SYSTEM whatever is asked here.
        ("DELETE", ["api", "accounts", account]) => {
            run(
                client,
                json!({ "command": "DELETE.ACCOUNT", "target_account": account }),
            )
            .await
        }
        ("GET", ["api", "accounts", account, "files"]) => {
            run(client, json!({ "command": "LIST.FILES", "account": account })).await
        }
        ("POST", ["api", "accounts", account, "files"]) => {
            let name = match field(&body, "name") {
                Some(name) => name,
                None => return Response::error(400, "A file name is required"),
            };
            let mut request = json!({
                "command": "CREATE.FILE",
                "account": account,
                "file": name,
                "durable": flag(&body, "durable"),
            });
            // A directory file, whose records are the files of a host
            // directory. Sent instead of the durability flag rather than
            // beside it: a directory file has no buffered writes to make
            // durable, and the database refuses a request that asks for both.
            if flag(&body, "directory") {
                let object = request.as_object_mut().expect("built as an object");
                object.remove("durable");
                object.insert("directory".to_string(), json!(true));
                if let Some(path) = field(&body, "path") {
                    object.insert("path".to_string(), json!(path));
                }
                return run(client, request).await;
            }
            // A file that mints its own keys. Exclusive with `queue`, which
            // already mints one per record, so the two are never sent
            // together - the page offers them as one choice for that reason.
            if flag(&body, "autokey") {
                let object = request.as_object_mut().expect("built as an object");
                object.insert("autokey".to_string(), json!(true));
                return run(client, request).await;
            }
            // A queue is created with its claim policy, so the file is never
            // briefly a queue running on defaults nobody asked for.
            if flag(&body, "queue") {
                let object = request.as_object_mut().expect("built as an object");
                object.insert("queue".to_string(), json!(true));
                if let Some(seconds) = number(&body, "visibility_timeout") {
                    object.insert("visibility_timeout".to_string(), json!(seconds));
                }
                if let Some(count) = number(&body, "max_deliveries") {
                    object.insert("max_deliveries".to_string(), json!(count));
                }
            }
            run(client, request).await
        }
        ("GET", ["api", "accounts", account, "files", file]) => {
            run(
                client,
                json!({ "command": "FILE.STATS", "account": account, "file": file }),
            )
            .await
        }
        // What the dashboard changes about an existing file rather than
        // reports: whether its writes are flushed before they are
        // acknowledged, whether it is a queue, and that queue's claim policy.
        // Only the fields the request carries are sent on, so the database's
        // "an omitted attribute is left alone" rule reaches the page unchanged.
        ("POST", ["api", "accounts", account, "files", file]) => {
            let mut request = json!({ "command": "SET.FILE", "account": account, "file": file });
            let object = request.as_object_mut().expect("built as an object");
            if let Some(durable) = optional_flag(&body, "durable") {
                object.insert("durable".to_string(), json!(durable));
            }
            if let Some(queue) = optional_flag(&body, "queue") {
                object.insert("queue".to_string(), json!(queue));
            }
            if let Some(autokey) = optional_flag(&body, "autokey") {
                object.insert("autokey".to_string(), json!(autokey));
            }
            if let Some(seconds) = number(&body, "visibility_timeout") {
                object.insert("visibility_timeout".to_string(), json!(seconds));
            }
            if let Some(count) = number(&body, "max_deliveries") {
                object.insert("max_deliveries".to_string(), json!(count));
            }
            if object.len() <= 3 {
                return Response::error(
                    400,
                    "Nothing to set: name durable, autokey, queue, visibility_timeout or max_deliveries",
                );
            }
            run(client, request).await
        }
        ("DELETE", ["api", "accounts", account, "files", file]) => {
            run(
                client,
                json!({ "command": "DELETE.FILE", "account": account, "file": file }),
            )
            .await
        }

        // A file's indexes: which fields resolve a `WITH <field> = ...` through
        // an index instead of a scan, what each one costs, and how selective it
        // is. Creating, rebuilding and dropping one are storage decisions about
        // the file, which is why they are admin-only in the database; listing
        // them is not.
        ("GET", ["api", "accounts", account, "files", file, "indexes"]) => {
            run(
                client,
                json!({ "command": "LIST.INDEXES", "account": account, "file": file }),
            )
            .await
        }
        // Every index in the account, so index health is visible without
        // walking file by file. Same command, without the file.
        ("GET", ["api", "accounts", account, "indexes"]) => {
            run(client, json!({ "command": "LIST.INDEXES", "account": account })).await
        }
        ("POST", ["api", "accounts", account, "files", file, "indexes"]) => {
            let field = match field(&body, "field") {
                Some(field) => field,
                None => return Response::error(400, "A dictionary field is required"),
            };
            run(
                client,
                json!({
                    "command": "CREATE.INDEX",
                    "account": account,
                    "file": file,
                    "field": field,
                    "values": values(&body, "values"),
                }),
            )
            .await
        }
        // One index in full, with the values that dominate it. The page asks
        // for this deliberately, which is why it is not folded into the listing
        // above: that one is read on every navigation and stays cheap.
        ("GET", ["api", "accounts", account, "files", file, "indexes", field]) => {
            let limit = request.query.get("limit").and_then(|limit| limit.parse::<usize>().ok());
            run(
                client,
                json!({
                    "command": "INDEX.STATS",
                    "account": account,
                    "file": file,
                    "field": field,
                    "limit": limit,
                }),
            )
            .await
        }
        // Acting on the diagnosis the histogram above shows. Its own endpoint
        // rather than a flag, for the reason `rebuild` is: it changes what an
        // existing index holds, and confusing the two would let a mistyped
        // field name quietly create a second index.
        ("POST", ["api", "accounts", account, "files", file, "indexes", field, "exclude"]) => {
            // An absent list clears the exclusions, which is what the command
            // means by replacing the set - so nothing is refused for being empty.
            run(
                client,
                json!({
                    "command": "SET.INDEX.EXCLUDE",
                    "account": account,
                    "file": file,
                    "field": field,
                    "values": values(&body, "values"),
                }),
            )
            .await
        }
        // Rebuilding is its own endpoint rather than a flag on the one above:
        // it acts on an index that already exists, and confusing the two would
        // let a mistyped field name quietly create a second index.
        ("POST", ["api", "accounts", account, "files", file, "indexes", field, "rebuild"]) => {
            run(
                client,
                json!({ "command": "REBUILD.INDEX", "account": account, "file": file, "field": field }),
            )
            .await
        }
        ("DELETE", ["api", "accounts", account, "files", file, "indexes", field]) => {
            run(
                client,
                json!({ "command": "DELETE.INDEX", "account": account, "file": file, "field": field }),
            )
            .await
        }

        // A file's dictionary: the definitions that decide what its fields are
        // called, how they are laid out and how they convert.
        ("GET", ["api", "accounts", account, "files", file, "dictionary"]) => {
            run(
                client,
                json!({ "command": "LIST.DICT", "account": account, "file": file }),
            )
            .await
        }
        ("POST", ["api", "accounts", account, "files", file, "dictionary"]) => {
            let name = match field(&body, "name") {
                Some(name) => name,
                None => return Response::error(400, "A dictionary entry name is required"),
            };
            // Forwarded whole rather than picked apart: `SET.DICT` is where an
            // attribute number or a justification is judged, so a rule lives in
            // one place and this endpoint cannot drift from it.
            run(
                client,
                json!({
                    "command": "SET.DICT",
                    "account": account,
                    "file": file,
                    "key": name,
                    "structured_data": dictionary_attributes(&body),
                }),
            )
            .await
        }
        // `DELETE` with `is_dict` already removes one entry correctly, so there
        // is no separate command for it to duplicate.
        ("DELETE", ["api", "accounts", account, "files", file, "dictionary", name]) => {
            run(
                client,
                json!({ "command": "DELETE", "account": account, "file": file, "key": name, "is_dict": true }),
            )
            .await
        }

        // Backup and restore. The dashboard is a browser, so an archive is a
        // download and a restore is an upload - the two things a browser is
        // actually good for, and the reason `EXPORT.BYTES` / `IMPORT.BYTES`
        // exist. Writing an archive to a path on the server host is the CLI's
        // job and is deliberately not offered here: a path typed into a web
        // form names a directory on a machine the person at the keyboard
        // usually cannot see.
        ("GET", ["api", "archive"]) => export_archive(client, request).await,
        ("POST", ["api", "archive"]) => import_archive(client, request).await,

        ("GET", _) | ("HEAD", _) => Response::error(404, "No such endpoint"),
        _ => Response::error(405, "Method not allowed for this endpoint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_errors_keep_their_meaning_in_the_status_code() {
        assert_eq!(status_for(Some(ErrorCode::AdminRequired)), 403);
        assert_eq!(status_for(Some(ErrorCode::AccessDenied)), 403);
        assert_eq!(status_for(Some(ErrorCode::FileNotFound)), 404);
        assert_eq!(status_for(Some(ErrorCode::FileExists)), 409);
        assert_eq!(status_for(Some(ErrorCode::IoError)), 500);
        assert_eq!(status_for(Some(ErrorCode::MissingField)), 400);
        // A code this build does not know, and one that is not there at all.
        assert_eq!(status_for(ErrorCode::from_wire("SOMETHING_NEW")), 400);
        assert_eq!(status_for(None), 400);
    }

    #[test]
    fn account_lists_are_accepted_as_an_array_or_a_string() {
        let from_array = json!({ "accounts": ["SALES", " REPORTS ", ""] });
        assert_eq!(accounts(&from_array, "accounts"), vec!["SALES", "REPORTS"]);

        let from_string = json!({ "accounts": "SALES, REPORTS," });
        assert_eq!(accounts(&from_string, "accounts"), vec!["SALES", "REPORTS"]);

        assert!(accounts(&json!({}), "accounts").is_empty());
    }

    #[test]
    fn flags_accept_what_a_form_actually_sends() {
        assert!(flag(&json!({ "is_admin": true }), "is_admin"));
        assert!(flag(&json!({ "is_admin": "Y" }), "is_admin"));
        assert!(flag(&json!({ "is_admin": "admin" }), "is_admin"));
        assert!(!flag(&json!({ "is_admin": "N" }), "is_admin"));
        assert!(!flag(&json!({}), "is_admin"));
    }

    #[test]
    fn a_missing_flag_is_not_a_false_one() {
        // The difference decides whether an incomplete request demotes a file or
        // is refused, so it is asserted rather than left to `flag`'s default.
        assert_eq!(optional_flag(&json!({ "durable": true }), "durable"), Some(true));
        assert_eq!(optional_flag(&json!({ "durable": false }), "durable"), Some(false));
        assert_eq!(optional_flag(&json!({ "durable": "Y" }), "durable"), Some(true));
        assert_eq!(optional_flag(&json!({ "durable": null }), "durable"), None);
        assert_eq!(optional_flag(&json!({}), "durable"), None);
    }

    #[test]
    fn dictionary_attributes_carry_only_what_a_definition_is_made_of() {
        // The entry's name is the record's key, not one of its attributes, and
        // anything else a caller sent is not part of the definition either.
        let body = json!({
            "name": "PRICE",
            "field": 2,
            "heading": "Unit price",
            "justification": "R",
            "width": "12",
            "conversion": "MD2",
            "durable": true,
        });
        assert_eq!(
            dictionary_attributes(&body),
            json!({"field": 2, "heading": "Unit price", "justification": "R", "width": "12", "conversion": "MD2"})
        );

        // An attribute left out stays out, so SET.DICT applies its own default
        // rather than being handed a null to interpret.
        assert_eq!(
            dictionary_attributes(&json!({ "name": "NAME", "field": 1 })),
            json!({ "field": 1 })
        );
    }

    #[test]
    fn index_exclusions_keep_every_value_the_caller_sent() {
        // Not `accounts`: an exclusion may be the empty value, which is the
        // commonest one there is - a sparse field most records do not carry -
        // and it may hold a comma or edge whitespace. Dropping either would be
        // this endpoint deciding what a value is, which only the caller knows.
        let body = json!({ "values": ["ACTIVE", "", "a,b", " padded "] });
        assert_eq!(values(&body, "values"), vec!["ACTIVE", "", "a,b", " padded "]);

        // An absent or non-array list is no exclusions, which is what clearing
        // them looks like on the wire.
        assert!(values(&json!({}), "values").is_empty());
        assert!(values(&json!({ "values": "ACTIVE" }), "values").is_empty());
    }

    /// A downloaded archive lands in a folder beside other downloads, so its
    /// name has to say what it holds and when it was taken.
    #[test]
    fn an_archive_is_named_for_what_it_holds() {
        assert!(archive_name(Some("SALES"), None).starts_with("SALES-"));
        assert!(archive_name(Some("SALES"), Some("ORDERS")).starts_with("SALES-ORDERS-"));
        assert!(archive_name(None, None).starts_with("database-"));
        assert!(archive_name(Some("SALES"), None).ends_with(".srp"));
    }

    /// An account name is user data and reaches a `Content-Disposition` header,
    /// so anything that could end the quoted string - or climb a directory on
    /// the way to the viewer's disk - is replaced rather than passed through.
    #[test]
    fn an_archive_name_cannot_carry_anything_but_a_name() {
        let hostile = archive_name(Some("../../etc\"; rm -rf /"), None);
        assert!(!hostile.contains('/'), "{}", hostile);
        assert!(!hostile.contains('"'), "{}", hostile);
        assert!(!hostile.contains(' '), "{}", hostile);
        assert!(hostile.starts_with(".._.._etc"), "{}", hostile);
    }

    /// A restore's two dangerous options are read from the query, and absent
    /// has to mean off. `?overwrite=false` meaning "overwrite" would be the one
    /// failure the flag exists to prevent.
    #[test]
    fn a_restore_flag_is_only_set_when_it_says_so() {
        for yes in ["1", "true", "TRUE", "yes", "on"] {
            assert!(flag_text(yes), "{}", yes);
        }
        for no in ["", "0", "false", "no", "off", "maybe"] {
            assert!(!flag_text(no), "{}", no);
        }
    }

    #[test]
    fn blank_fields_read_as_missing() {
        assert_eq!(field(&json!({ "name": "  REPORTS " }), "name"), Some("REPORTS"));
        assert_eq!(field(&json!({ "name": "   " }), "name"), None);
        assert_eq!(field(&json!({}), "name"), None);
    }
}
