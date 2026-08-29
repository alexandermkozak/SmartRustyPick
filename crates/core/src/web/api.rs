//! Routes dashboard requests onto remote-protocol commands.
//!
//! Every endpoint here is a thin translation: an HTTP shape in, one protocol
//! command out, the protocol's own answer back. Nothing reaches into the engine
//! directly, so the dashboard cannot do anything a certificate holder on the
//! TCP listener could not, and a permission decision is made in exactly one
//! place - the command handler.

use crate::web::client::ProtocolClient;
use crate::web::http::{Request, Response};
use serde_json::{Value, json};
use std::sync::Arc;

/// Maps a protocol error onto the status code that describes it.
///
/// The protocol has one error status and a human-readable message; a browser
/// client wants to tell "you may not" apart from "that does not exist" without
/// parsing prose, so the few messages that carry a distinct meaning are
/// classified here and everything else is a plain bad request.
fn status_for(message: &str) -> u16 {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("admin privileges required") || lowered.contains("access denied") {
        403
    } else if lowered.contains("not found") {
        404
    } else {
        400
    }
}

/// Runs one command and turns its response into an HTTP response.
async fn run(client: &ProtocolClient, payload: Value) -> Response {
    match client.request(payload).await {
        Ok(response) => {
            let status = response.get("status").and_then(Value::as_str).unwrap_or("ERROR");
            if status == "OK" {
                Response::json(200, &response)
            } else {
                let message = response.get("message").and_then(Value::as_str).unwrap_or("Request failed");
                Response::error(status_for(message), message)
            }
        }
        // The database is the dashboard's upstream, so an unreachable one is a
        // gateway failure rather than the browser's fault.
        Err(e) => Response::error(502, format!("Database unreachable: {}", e)),
    }
}

/// A required string field of a JSON request body.
fn field<'a>(body: &'a Value, name: &str) -> Option<&'a str> {
    body.get(name).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
}

/// An account list, accepting both `["A","B"]` and `"A, B"` so the page can send
/// whichever it has.
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

fn flag(body: &Value, name: &str) -> bool {
    match body.get(name) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => matches!(text.to_ascii_uppercase().as_str(), "Y" | "YES" | "TRUE" | "1" | "ADMIN"),
        _ => false,
    }
}

/// Dispatches an authenticated `/api/...` request.
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
            if !is_admin && allowed.is_empty() {
                return Response::error(400, "A non-admin client needs at least one allowed account");
            }
            run(
                client,
                json!({
                    "command": "AUTHORIZE.CONN",
                    "name": name,
                    "thumbprint": thumbprint,
                    "accounts_list": allowed,
                    "is_admin": is_admin,
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
            let command = if flag(&body, "remove") { "REMOVE.CLIENT.ACCOUNT" } else { "ADD.CLIENT.ACCOUNT" };
            run(client, json!({ "command": command, "name": name, "accounts_list": allowed })).await
        }

        // Certificates: issued, authorized and handed back in one step.
        ("POST", ["api", "certificates"]) => {
            let common_name = match field(&body, "common_name") {
                Some(name) => name,
                None => return Response::error(400, "A common name is required"),
            };
            let is_admin = flag(&body, "is_admin");
            let allowed = accounts(&body, "accounts");
            if !is_admin && allowed.is_empty() {
                return Response::error(400, "A non-admin certificate needs at least one allowed account");
            }
            run(
                client,
                json!({
                    "command": "GENERATE.CERT",
                    "name": common_name,
                    "accounts_list": allowed,
                    "is_admin": is_admin,
                }),
            )
                .await
        }

        // Accounts and their files. Statistics only - no endpoint here returns a
        // record, which is the point: the dashboard manages the database, it is
        // not a second way to read it.
        ("GET", ["api", "accounts"]) => run(client, json!({ "command": "LIST.ACCOUNTS" })).await,
        ("GET", ["api", "accounts", account, "files"]) => {
            run(client, json!({ "command": "LIST.FILES", "account": account })).await
        }
        ("GET", ["api", "accounts", account, "files", file]) => {
            run(client, json!({ "command": "FILE.STATS", "account": account, "file": file })).await
        }

        ("GET", _) | ("HEAD", _) => Response::error(404, "No such endpoint"),
        _ => Response::error(405, "Method not allowed for this endpoint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_errors_keep_their_meaning_in_the_status_code() {
        assert_eq!(status_for("Admin privileges required"), 403);
        assert_eq!(status_for("Access denied for account PAYROLL: Not in allowed list"), 403);
        assert_eq!(status_for("Table 'ORDERS' not found in account 'SALES'"), 404);
        assert_eq!(status_for("File not specified"), 400);
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
    fn blank_fields_read_as_missing() {
        assert_eq!(field(&json!({ "name": "  REPORTS " }), "name"), Some("REPORTS"));
        assert_eq!(field(&json!({ "name": "   " }), "name"), None);
        assert_eq!(field(&json!({}), "name"), None);
    }
}
