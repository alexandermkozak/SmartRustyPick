//! Just enough HTTP/1.1 to serve a single-page dashboard and a JSON API.
//!
//! The dashboard needs request lines, headers, a bounded body, cookies and
//! keep-alive - and nothing else. A framework would bring a dependency tree
//! larger than the database itself for that, so the subset is written out here,
//! in the same hand-rolled spirit as the line-delimited protocol it fronts.
//!
//! Everything is bounded: a request that exceeds a limit is answered with a
//! status code rather than read into memory.

use std::collections::HashMap;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Longest request line accepted (method, target and version).
const MAX_REQUEST_LINE: usize = 8 * 1024;
/// Longest single header line accepted.
const MAX_HEADER_LINE: usize = 8 * 1024;
/// Most headers accepted in one request.
const MAX_HEADERS: usize = 64;
/// Largest body accepted. The API exchanges small JSON objects only.
pub const MAX_BODY: usize = 256 * 1024;

/// A parsed request. Header names are lowercased on the way in, so lookups do
/// not have to care how the client capitalised them.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// Path with percent escapes decoded, query string removed.
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub keep_alive: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    /// One cookie value from the `Cookie` header, or `None` when it is absent.
    pub fn cookie(&self, name: &str) -> Option<String> {
        let header = self.header("cookie")?;
        header.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_string())
        })
    }

    /// The body parsed as JSON, or `None` when it is absent or malformed.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// Path split into non-empty segments: `/api/accounts/X` -> `["api", "accounts", "X"]`.
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }
}

/// A response ready to be written. The body is bytes, so the same type carries
/// JSON, HTML and downloads.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Response {
            status,
            content_type: content_type.to_string(),
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
        Response::new(status, "application/json; charset=utf-8", body)
    }

    /// The API's error shape: every failure is `{"error": "..."}` so the page
    /// has one thing to render.
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Response::json(status, &serde_json::json!({ "error": message.into() }))
    }

    pub fn html(body: impl Into<Vec<u8>>) -> Self {
        Response::new(200, "text/html; charset=utf-8", body)
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Unknown",
    }
}

/// Reads one line, without ever buffering more than `max` bytes of it.
///
/// `read_line` would happily grow its buffer until the process ran out of
/// memory, which is not a promise a network-facing parser can make. Returns the
/// line without its terminator, or `None` at end of input.
async fn read_line_limited<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(finish_line(line)))
            };
        }
        match available.iter().position(|b| *b == b'\n') {
            Some(index) => {
                if line.len() + index > max {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
                }
                line.extend_from_slice(&available[..index]);
                reader.consume(index + 1);
                return Ok(Some(finish_line(line)));
            }
            None => {
                let taken = available.len();
                if line.len() + taken > max {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
                }
                line.extend_from_slice(available);
                reader.consume(taken);
            }
        }
    }
}

fn finish_line(mut line: Vec<u8>) -> String {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8_lossy(&line).into_owned()
}

/// Percent-decoding, with `+` treated as a space so query values behave the way
/// a form-encoded value does.
pub fn percent_decode(value: &str, plus_is_space: bool) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key, true), percent_decode(value, true)),
            None => (percent_decode(pair, true), String::new()),
        })
        .collect()
}

/// What one read attempt produced.
pub enum Incoming {
    /// A well-formed request.
    Request(Box<Request>),
    /// The client closed the connection cleanly.
    Eof,
    /// The request was rejected before it was fully parsed; the response says
    /// why and the connection must close after it is written.
    Rejected(Response),
}

/// Reads one request from a connection.
pub async fn read_request<R>(reader: &mut R) -> std::io::Result<Incoming>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let request_line = match read_line_limited(reader, MAX_REQUEST_LINE).await {
        Ok(Some(line)) => line,
        Ok(None) => return Ok(Incoming::Eof),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Ok(Incoming::Rejected(Response::error(414, "Request target too long")));
        }
        Err(e) => return Err(e),
    };
    // A stray empty line before the request line is legal and worth ignoring.
    if request_line.trim().is_empty() {
        return Ok(Incoming::Eof);
    }

    let mut parts = request_line.split_whitespace();
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(method), Some(target), version) => (
            method.to_string(),
            target.to_string(),
            version.unwrap_or("HTTP/1.1").to_string(),
        ),
        _ => return Ok(Incoming::Rejected(Response::error(400, "Malformed request line"))),
    };

    let mut headers = HashMap::new();
    loop {
        let line = match read_line_limited(reader, MAX_HEADER_LINE).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(Incoming::Rejected(Response::error(400, "Unexpected end of headers"))),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(Incoming::Rejected(Response::error(431, "Header line too long")));
            }
            Err(e) => return Err(e),
        };
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Ok(Incoming::Rejected(Response::error(431, "Too many headers")));
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length: usize = match headers.get("content-length").map(|v| v.parse::<usize>()) {
        Some(Ok(length)) => length,
        Some(Err(_)) => return Ok(Incoming::Rejected(Response::error(400, "Invalid Content-Length"))),
        None => 0,
    };
    if content_length > MAX_BODY {
        return Ok(Incoming::Rejected(Response::error(413, "Request body too large")));
    }
    // Chunked bodies would need a second framing to be implemented; nothing the
    // dashboard sends uses one, so it is refused rather than mis-read.
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return Ok(Incoming::Rejected(Response::error(
            400,
            "Chunked request bodies are not supported",
        )));
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let connection = headers.get("connection").map(|v| v.to_ascii_lowercase());
    let keep_alive = match connection.as_deref() {
        Some(value) if value.contains("close") => false,
        Some(value) if value.contains("keep-alive") => true,
        _ => version != "HTTP/1.0",
    };

    Ok(Incoming::Request(Box::new(Request {
        method: method.to_ascii_uppercase(),
        path: percent_decode(raw_path, false),
        query: parse_query(raw_query),
        headers,
        body,
        keep_alive,
    })))
}

/// Writes a response, including the headers every dashboard response carries.
///
/// The page is served from the same origin it talks to, so a strict policy
/// costs nothing and keeps an injected string from pulling in anything remote.
pub async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
    keep_alive: bool,
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    head.push_str(&format!("Content-Type: {}\r\n", response.content_type));
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    head.push_str(&format!(
        "Connection: {}\r\n",
        if keep_alive { "keep-alive" } else { "close" }
    ));
    head.push_str("Cache-Control: no-store\r\n");
    head.push_str("X-Content-Type-Options: nosniff\r\n");
    head.push_str("Referrer-Policy: no-referrer\r\n");
    head.push_str("Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'none'; frame-ancestors 'none'\r\n");
    for (name, value) in &response.headers {
        head.push_str(&format!("{}: {}\r\n", name, value));
    }
    head.push_str("\r\n");

    writer.write_all(head.as_bytes()).await?;
    writer.write_all(&response.body).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(raw: &str) -> Incoming {
        let mut reader = tokio::io::BufReader::new(raw.as_bytes());
        read_request(&mut reader).await.unwrap()
    }

    fn expect_request(incoming: Incoming) -> Request {
        match incoming {
            Incoming::Request(request) => *request,
            Incoming::Eof => panic!("expected a request, got EOF"),
            Incoming::Rejected(response) => panic!("expected a request, got {}", response.status),
        }
    }

    fn expect_rejected(incoming: Incoming) -> Response {
        match incoming {
            Incoming::Rejected(response) => response,
            _ => panic!("expected a rejection"),
        }
    }

    #[tokio::test]
    async fn parses_a_get_with_a_query_string() {
        let request =
            expect_request(parse("GET /api/accounts?token=abc&x=a%20b HTTP/1.1\r\nHost: localhost\r\n\r\n").await);
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/accounts");
        assert_eq!(request.query.get("token").unwrap(), "abc");
        assert_eq!(request.query.get("x").unwrap(), "a b");
        assert_eq!(request.header("host").unwrap(), "localhost");
        assert!(request.keep_alive);
    }

    #[tokio::test]
    async fn decodes_percent_escapes_in_the_path() {
        let request = expect_request(parse("GET /api/accounts/MY%20ACCOUNT/files HTTP/1.1\r\n\r\n").await);
        assert_eq!(request.segments(), vec!["api", "accounts", "MY ACCOUNT", "files"]);
    }

    #[tokio::test]
    async fn reads_a_json_body_of_the_declared_length() {
        let body = r#"{"name":"REPORTS"}"#;
        let raw = format!(
            "POST /api/clients HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let request = expect_request(parse(&raw).await);
        assert_eq!(request.json().unwrap()["name"], "REPORTS");
    }

    #[tokio::test]
    async fn reads_one_cookie_out_of_several() {
        let request =
            expect_request(parse("GET / HTTP/1.1\r\nCookie: theme=dark; srp_token=secret; other=1\r\n\r\n").await);
        assert_eq!(request.cookie("srp_token").unwrap(), "secret");
        assert_eq!(request.cookie("missing"), None);
    }

    #[tokio::test]
    async fn http_1_0_closes_unless_it_asks_otherwise() {
        assert!(!expect_request(parse("GET / HTTP/1.0\r\n\r\n").await).keep_alive);
        assert!(expect_request(parse("GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").await).keep_alive);
        assert!(!expect_request(parse("GET / HTTP/1.1\r\nConnection: close\r\n\r\n").await).keep_alive);
    }

    #[tokio::test]
    async fn an_empty_connection_is_end_of_input() {
        assert!(matches!(parse("").await, Incoming::Eof));
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_rather_than_read() {
        let raw = format!("POST /api/clients HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert_eq!(expect_rejected(parse(&raw).await).status, 413);
    }

    #[tokio::test]
    async fn an_endless_header_line_is_refused_rather_than_buffered() {
        let raw = format!("GET / HTTP/1.1\r\nX-Big: {}\r\n\r\n", "a".repeat(MAX_HEADER_LINE + 10));
        assert_eq!(expect_rejected(parse(&raw).await).status, 431);
    }

    #[tokio::test]
    async fn a_malformed_request_line_is_a_bad_request() {
        assert_eq!(expect_rejected(parse("GARBAGE\r\n\r\n").await).status, 400);
    }

    #[tokio::test]
    async fn two_requests_are_read_from_one_connection() {
        let raw = "GET /one HTTP/1.1\r\n\r\nGET /two HTTP/1.1\r\n\r\n";
        let mut reader = tokio::io::BufReader::new(raw.as_bytes());
        let first = expect_request(read_request(&mut reader).await.unwrap());
        let second = expect_request(read_request(&mut reader).await.unwrap());
        assert_eq!(first.path, "/one");
        assert_eq!(second.path, "/two");
        assert!(matches!(read_request(&mut reader).await.unwrap(), Incoming::Eof));
    }

    #[tokio::test]
    async fn a_response_carries_its_length_and_the_security_headers() {
        let mut out = Vec::new();
        let response =
            Response::json(200, &serde_json::json!({"status": "ok"})).with_header("Set-Cookie", "srp_token=x");
        write_response(&mut out, &response, false).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 15"));
        assert!(text.contains("Connection: close"));
        assert!(text.contains("X-Content-Type-Options: nosniff"));
        assert!(text.contains("Set-Cookie: srp_token=x"));
        assert!(text.ends_with(r#"{"status":"ok"}"#));
    }
}
