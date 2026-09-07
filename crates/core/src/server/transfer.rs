//! Raw byte transfers: the one place the protocol is not a line of JSON.
//!
//! A [directory file](crate::db::directory)'s record is a host file, and
//! `max_directory_record_bytes` lets it be 64 MiB. The ordinary protocol cannot
//! carry one: it is line-delimited JSON bounded by `max_request_bytes`, and a
//! record travels inside that line base64-encoded, which inflates by 4/3 and
//! leaves a ceiling just under 768 KiB. `PUT.BYTES` and `GET.BYTES` are how the
//! other 63 MiB get across.
//!
//! ```text
//! {"command":"PUT.BYTES","file":"SCANS","key":"scan.pdf","length":3145728}\n
//! <3145728 raw bytes>
//! {"status":"OK","length":3145728}\n
//! ```
//!
//! and `GET.BYTES` the same the other way: a JSON line announcing the length,
//! then exactly that many bytes. The connection is already authenticated by the
//! client certificate, so a transfer is a bounded interlude in a session that
//! carries on afterwards, and needs nothing new to authorise it.
//!
//! # Why the body is announced rather than delimited
//!
//! Every other request ends at a newline. A record is arbitrary bytes and holds
//! newlines like any other byte, so there is no terminator to look for - the
//! length has to be said in advance. Which is also what lets an oversized
//! transfer be refused before a byte of it is read.
//!
//! # The three things that make this safe
//!
//! **The body is read through the same [`BufReader`](tokio::io::BufReader) the
//! request line was.** By the time the JSON line has been parsed, the first
//! bytes of the body are almost certainly sitting in that reader's buffer
//! already - they arrived in the same TCP segment. Reading the body from the
//! underlying stream instead would silently drop exactly those bytes, on
//! exactly the transfers whose body straddles a buffer boundary, which is most
//! of them. So every read here goes through the caller's reader, and
//! [`a_body_is_read_through_the_buffer_the_request_line_was`] pins it with a
//! reader too small to hold the body whole.
//!
//! **A body is always accounted for.** After the request line is parsed the
//! socket holds `length` bytes that are not JSON, and the line reader must not
//! be allowed near them. Either they are consumed - into the record, or into
//! nothing when the request is refused - or the connection is closed. There is
//! no third option, and [`Outcome`] is that choice made explicit rather than
//! left to a `return` somewhere. The rule the codebase already had for an
//! over-long line ("unread bytes may still be sitting on the socket, so the
//! only safe response is to close the connection") is this rule; a body makes
//! it routine rather than exceptional.
//!
//! **Nothing is held in memory.** The body goes to a temporary file inside the
//! directory file's own root, in fixed-size chunks, and is renamed into place
//! when it has all arrived. A 64 MiB record costs a 64 KiB buffer. The
//! temporary is inside the file's root rather than the system temporary
//! directory precisely so an abandoned transfer is swept by the ordinary read
//! path - debris somewhere nothing looks at again is a leak with extra steps.
//!
//! # A stalled transfer
//!
//! `idle_timeout_ms` bounds a connection with no request in flight. A client
//! that announces 64 MiB and sends a byte a second is neither idle nor
//! finished, so it needs a bound of its own: every read of a body chunk is
//! given `transfer_stall_timeout_ms`, which catches *no progress* rather than
//! capping the total duration. A slow link moving a large record is not a
//! stalled one and must not be treated as one.

use super::handler::{self, SharedDb, StagedWrite};
use super::models::{ErrorCode, Request, Response};
use crate::db::ClientInfo;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bytes moved per read. Large enough that a 64 MiB record is a thousand reads
/// rather than a million, small enough that it is not what bounds memory.
const CHUNK: usize = 64 * 1024;

/// True when the command is one this module owns, and the connection loop must
/// not hand to the ordinary dispatch.
pub fn is_transfer_command(command: &str) -> bool {
    matches!(command, "PUT.BYTES" | "GET.BYTES")
}

/// What the connection is to do next, and what to record about it.
///
/// The whole point of returning this rather than a `Response` is `close`: after
/// a body has been announced, "carry on reading lines" is only safe if the body
/// was accounted for, and the caller must not be able to assume it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// The socket cannot be trusted to be at a request boundary. The response,
    /// if any, has been written; the connection must now be closed.
    pub close: bool,
    /// The request was answered with an error, for the request counters
    /// `SERVER.STATS` reports. A transfer that fails is a failed request like
    /// any other, and counting it as a success would make the one command most
    /// likely to fail on a bad network the one that never shows it.
    pub failed: bool,
}

impl Outcome {
    /// Stored or sent, and the connection is at a request boundary.
    fn done() -> Self {
        Outcome {
            close: false,
            failed: false,
        }
    }

    /// Refused, and the body accounted for.
    fn refused() -> Self {
        Outcome {
            close: false,
            failed: true,
        }
    }

    /// The socket is at an unknown offset. Always a failure: every path that
    /// reaches it has either refused the request or lost the body part way.
    fn torn() -> Self {
        Outcome {
            close: true,
            failed: true,
        }
    }
}

/// Reads `length` bytes of body and stores them as one record.
///
/// The refusal path is where the care is. A request refused *before* the body
/// has been read still has that body on the socket, and what to do with it
/// depends on how big it is: a body within the record limit is drained, so the
/// client keeps its connection and gets a useful error; one that is over the
/// limit - or was never given a length at all - is not, because draining ten
/// gigabytes to report "that is too large" is the same denial of service the
/// limit exists to prevent. Then the only answer is to close.
// The `Err` variant is a ready-to-send `Response`, the same trade the rest of
// the handler makes: boxing it would add an allocation on the error path and an
// unboxing at each call site, to shrink a value that is written to a socket
// immediately either way.
#[allow(clippy::result_large_err)]
pub async fn put_bytes<R, W>(
    reader: &mut R,
    writer: &mut W,
    req: &Request,
    db: &SharedDb,
    client: &ClientInfo,
    max_record_bytes: u64,
    stall: Duration,
) -> Outcome
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let announced = req.length;
    let staged = {
        let (req, db, client) = (clone_request(req), db.clone(), client.clone());
        match blocking(move || handler::stage_bytes(&req, &db, &client)).await {
            Ok(staged) => staged,
            Err(response) => {
                // Refused with the body still to come. Drain it only when the
                // client told us how much there is and that much is affordable.
                let drainable = announced.is_some_and(|length| length <= max_record_bytes);
                let _ = write_response(writer, &response).await;
                if !drainable {
                    return Outcome::torn();
                }
                return match drain(reader, announced.unwrap_or(0), stall).await {
                    Ok(()) => Outcome::refused(),
                    Err(_) => Outcome::torn(),
                };
            }
        }
    };

    // From here the body is being consumed whatever happens, so a failure is a
    // failure of the transfer rather than of the connection - except an I/O
    // error on the socket itself, which leaves it at an unknown offset.
    let received = receive(reader, &staged, stall).await;
    let (arrived, torn) = match received {
        Ok(arrived) => (arrived, false),
        Err(TransferError::Body(arrived)) => (arrived, false),
        Err(TransferError::Socket(arrived)) => (arrived, true),
    };

    let response = {
        let (db, staged_owned) = (db.clone(), staged);
        blocking(move || Ok::<_, Response>(handler::commit_bytes(&staged_owned, arrived, &db)))
            .await
            .unwrap_or_else(|response| response)
    };
    let written = write_response(writer, &response).await;
    if torn || written.is_err() {
        Outcome::torn()
    } else if response.status == "OK" {
        Outcome::done()
    } else {
        Outcome::refused()
    }
}

/// Announces a record's length and then sends exactly that many bytes.
///
/// Nothing is on the socket to resynchronise against here - the client sent an
/// ordinary request line and is waiting - so a refusal is just a response and
/// the connection carries on. Only a failure part way through the body is
/// fatal: the client is by then reading a fixed number of bytes and cannot be
/// told the count has changed.
#[allow(clippy::result_large_err)]
pub async fn get_bytes<W>(writer: &mut W, req: &Request, db: &SharedDb, client: &ClientInfo) -> Outcome
where
    W: AsyncWrite + Unpin,
{
    let opened = {
        let (req, db, client) = (clone_request(req), db.clone(), client.clone());
        match blocking(move || handler::open_bytes(&req, &db, &client)).await {
            Ok(opened) => opened,
            Err(response) => {
                return match write_response(writer, &response).await {
                    Ok(()) => Outcome::refused(),
                    Err(_) => Outcome::torn(),
                };
            }
        }
    };

    let response = Response {
        status: "OK".to_string(),
        length: Some(opened.length),
        ..Default::default()
    };
    if write_response(writer, &response).await.is_err() {
        return Outcome::torn();
    }

    // Read on a blocking thread a chunk at a time, because the engine's files
    // are synchronous and a 64 MiB read on an async worker would stall every
    // other connection that worker is carrying.
    let mut file = opened.file;
    let mut sent = 0u64;
    while sent < opened.length {
        let want = ((opened.length - sent) as usize).min(CHUNK);
        let read = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut buffer = vec![0u8; want];
            let mut filled = 0;
            while filled < want {
                match file.read(&mut buffer[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => return (file, Err(e)),
                }
            }
            buffer.truncate(filled);
            (file, Ok(buffer))
        })
        .await;
        let (handle, chunk) = match read {
            Ok(result) => result,
            // The blocking pool failed; the client is mid-body and cannot be
            // told, so the only honest thing left is to close.
            Err(_) => return Outcome::torn(),
        };
        file = handle;
        let chunk = match chunk {
            Ok(chunk) if chunk.is_empty() => return Outcome::torn(),
            Ok(chunk) => chunk,
            Err(_) => return Outcome::torn(),
        };
        if writer.write_all(&chunk).await.is_err() {
            return Outcome::torn();
        }
        sent += chunk.len() as u64;
    }
    Outcome::done()
}

/// Where a transfer stopped, and whether the socket survived it.
enum TransferError {
    /// The bytes could not be stored - a full disk, a permission error. The
    /// body was still consumed, so the connection is fine.
    Body(u64),
    /// The socket failed, timed out, or ended early. Its offset is unknown.
    Socket(u64),
}

/// Moves `staged.length` bytes from the reader into the staged file.
///
/// Every read goes through the caller's reader, which is the same buffered
/// reader the request line came from - see the module documentation for why
/// that is the whole ballgame.
async fn receive<R>(reader: &mut R, staged: &StagedWrite, stall: Duration) -> Result<u64, TransferError>
where
    R: AsyncRead + Unpin,
{
    let path = staged.staged.clone();
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(file) => file,
        // Nowhere to put it, but the body is still coming: drain it so the
        // connection survives to carry the error.
        Err(_) => {
            return match drain(reader, staged.length, stall).await {
                Ok(()) => Err(TransferError::Body(0)),
                Err(taken) => Err(TransferError::Socket(taken)),
            };
        }
    };

    let mut buffer = vec![0u8; CHUNK];
    let mut taken = 0u64;
    while taken < staged.length {
        let want = ((staged.length - taken) as usize).min(CHUNK);
        let read = match tokio::time::timeout(stall, reader.read(&mut buffer[..want])).await {
            Ok(Ok(0)) => return Err(TransferError::Socket(taken)),
            Ok(Ok(n)) => n,
            Ok(Err(_)) | Err(_) => return Err(TransferError::Socket(taken)),
        };
        if file.write_all(&buffer[..read]).await.is_err() {
            // The disk gave out. The body is still owed to the socket, so it is
            // drained rather than abandoned - the alternative is a connection
            // that cannot be reused for a failure that was not its fault.
            let owed = staged.length - taken - read as u64;
            return match drain(reader, owed, stall).await {
                Ok(()) => Err(TransferError::Body(taken + read as u64)),
                Err(further) => Err(TransferError::Socket(taken + read as u64 + further)),
            };
        }
        taken += read as u64;
    }
    match file.flush().await {
        Ok(()) => Ok(taken),
        Err(_) => Err(TransferError::Body(taken)),
    }
}

/// Reads and discards `length` bytes, so the socket is back at a request
/// boundary. `Err` carries how far it got before giving up.
async fn drain<R>(reader: &mut R, length: u64, stall: Duration) -> Result<(), u64>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; CHUNK];
    let mut taken = 0u64;
    while taken < length {
        let want = ((length - taken) as usize).min(CHUNK);
        match tokio::time::timeout(stall, reader.read(&mut buffer[..want])).await {
            Ok(Ok(0)) => return Err(taken),
            Ok(Ok(n)) => taken += n as u64,
            Ok(Err(_)) | Err(_) => return Err(taken),
        }
    }
    Ok(())
}

async fn write_response<W>(writer: &mut W, response: &Response) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(response)
        .unwrap_or_else(|_| r#"{"status":"ERROR","message":"Response could not be encoded"}"#.to_string());
    writer.write_all(format!("{}\n", line).as_bytes()).await
}

/// Runs one engine call off the async worker, turning a pool failure into a
/// response rather than a panic.
#[allow(clippy::result_large_err)]
async fn blocking<T, F>(work: F) -> Result<T, Response>
where
    F: FnOnce() -> Result<T, Response> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(e) => Err(Response {
            status: "ERROR".to_string(),
            message: Some(format!("Request failed: {}", e)),
            code: Some(ErrorCode::IoError),
            ..Default::default()
        }),
    }
}

/// The fields a transfer needs, as an owned request for the blocking half.
///
/// `Request` is not `Clone` - it carries the `TRANSACT` change set, which there
/// is no reason to copy - and a transfer needs five scalars out of it, so this
/// takes those rather than making the whole thing cloneable for one caller.
fn clone_request(req: &Request) -> Request {
    Request {
        command: req.command.clone(),
        account: req.account.clone(),
        file: req.file.clone(),
        key: req.key.clone(),
        length: req.length,
        ..Default::default()
    }
}
