"""Directory files end to end: the wire, the CLI, and the bytes in between.

The unit tests cover the engine directly. This suite covers what only exists
once there is a server and a real filesystem under it: that content an ordinary
record cannot hold survives a round trip over TLS in the `{"$base64": ...}`
envelope, that `STORE` and `EXTRACT` move a file far larger than one request
line, and that the commands a directory file cannot answer come back as
refusals a client can branch on rather than as empty results.
"""

import base64
import json
import os
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "DIR_ACC"
FILE = "SCANS"

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "SAVE",
]

# All three marks, an embedded NUL and a byte that is not valid UTF-8. Every one
# of these is something an ordinary record either splits on or cannot carry, so
# a round trip of exactly these bytes is the property being tested.
HOSTILE = bytes([0xFE, 0x61, 0xFD, 0x62, 0xFC, 0x00, 0xFF, 0xC3, 0x7A, 0x0A])

# Larger than `max_request_bytes`, so it could not have crossed the wire in one
# request even base64-free. STORE streams it instead.
BIG_BYTES = 3 * 1024 * 1024

# Short enough that the suite can wait out a stalled transfer, long enough that
# it is never tripped by a body that is merely arriving slowly - it bounds a gap
# with no bytes at all, not the total duration.
STALL_MS = 1500
STALL_CONFIG = f"transfer_stall_timeout_ms = {STALL_MS}\n"


def main():
    suite = harness.Suite("Directory", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("directory") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        port = harness.free_port()

        server = None
        try:
            harness.write_config(port, certs=None, extra=STALL_CONFIG)
            harness.run_cli(
                [
                    f"AUTHORIZE.CONN {admin_tp} admin ADMIN",
                    *SETUP_COMMANDS,
                    "EXIT",
                ],
                args=["--account", "SYSTEM"],
            )

            harness.write_config(port, certs, extra=STALL_CONFIG)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)

            # --- Creation -------------------------------------------------------
            resp = admin.request(command="CREATE.FILE", file=FILE, account=ACCOUNT, directory=True)
            suite.check_eq("CREATE.FILE ... DIRECTORY", resp.get("status"), "OK")
            record = resp.get("record") or {}
            records_dir = record.get("path") or ""
            suite.check(
                "The reply says where the records actually are",
                record.get("directory") is True and records_dir.endswith(os.path.join(FILE, "records")),
                str(record),
            )
            suite.check(
                "And that directory exists before a record is written",
                os.path.isdir(records_dir),
                records_dir,
            )

            # An ordinary file, so the refusals have something to be refused
            # against rather than only a missing one.
            admin.request(command="CREATE.FILE", file="USERS_ORDINARY", account=ACCOUNT)

            listed = admin.request(command="LIST.FILES", account=ACCOUNT)
            flags = dict(listed["results"])
            suite.check(
                "LIST.FILES reports the type",
                flags[FILE]["directory"] is True and flags["DIR"]["directory"] is False,
                str(flags.get(FILE)),
            )

            # --- The round trip that an ordinary record cannot make -------------
            encoded = base64.b64encode(HOSTILE).decode()
            resp = admin.request(
                command="WRITE", file=FILE, account=ACCOUNT, key="marks.bin", data={"$base64": encoded}
            )
            suite.check_eq("WRITE bytes that hold every mark", resp.get("status"), "OK")

            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="marks.bin")
            suite.check(
                "READ gives back exactly those bytes",
                (resp.get("record") or {}).get("$base64") == encoded,
                str(resp.get("record")),
            )
            suite.check(
                "And the record on disk is the file, byte for byte",
                open(os.path.join(records_dir, "marks.bin"), "rb").read() == HOSTILE,
            )

            resp = admin.request(
                command="WRITE", file=FILE, account=ACCOUNT, key="note.txt", data="plain text"
            )
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="note.txt")
            suite.check_eq("Text needs no envelope in either direction", resp.get("record"), "plain text")

            # --- Listing shows sizes, never content -----------------------------
            resp = admin.request(command="QUERY", file=FILE, account=ACCOUNT)
            rows = dict(resp.get("results") or [])
            suite.check(
                "QUERY gives one row per record, with its size",
                rows == {"marks.bin": {"size": len(HOSTILE)}, "note.txt": {"size": 10}},
                str(rows),
            )

            resp = admin.request(command="SELECT", file=FILE, account=ACCOUNT)
            suite.check_eq("SELECT builds a list of the keys", resp.get("count"), 2)
            resp = admin.request(command="GET.NEXT", account=ACCOUNT, batch_size=10)
            suite.check_eq("GET.NEXT pages it", resp.get("count"), 2)

            # --- Refusals, each with the code a client branches on ---------------
            refusals = [
                (
                    "A criterion, which would read a field the file has not got",
                    dict(command="QUERY", file=FILE, account=ACCOUNT, query_string='WITH NAME = "x"'),
                ),
                (
                    "The dictionary section, which governs nothing here",
                    dict(command="READ", file=FILE, account=ACCOUNT, key="note.txt", is_dict=True),
                ),
                (
                    "Changing the file's type after it was created",
                    dict(command="SET.FILE", file=FILE, account=ACCOUNT, directory=False),
                ),
                (
                    "Making a directory file a queue",
                    dict(command="CREATE.FILE", file="SPOOL", account=ACCOUNT, directory=True, queue=True),
                ),
                (
                    "A key that is not a usable file name",
                    dict(command="WRITE", file=FILE, account=ACCOUNT, key="../escape", data="no"),
                ),
                (
                    "An index on a file with no fields",
                    dict(command="CREATE.INDEX", file=FILE, account=ACCOUNT, field="ANY"),
                ),
                (
                    "Enqueueing onto a file with no order",
                    dict(command="ENQUEUE", file=FILE, account=ACCOUNT, data="work"),
                ),
            ]
            for what, request in refusals:
                resp = admin.request(**request)
                suite.check_eq(what, resp.get("code"), "INVALID_REQUEST")

            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[{"op": "WRITE", "file": FILE, "key": "in-a-set", "data": "x"}],
            )
            suite.check_eq(
                "A transaction naming a directory file", resp.get("code"), "TRANSACTION_SCOPE"
            )
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="in-a-set")
            suite.check_eq("And nothing in it was applied", resp.get("code"), "RECORD_NOT_FOUND")

            resp = admin.request(
                command="WRITE",
                file=FILE,
                account=ACCOUNT,
                key="note.txt",
                structured_data={"name": "Alice"},
            )
            suite.check_eq("Fields, sent to a file that has none", resp.get("code"), "INVALID_DATA")

            # --- FILE.STATS ------------------------------------------------------
            resp = admin.request(command="FILE.STATS", file=FILE, account=ACCOUNT)
            stats = resp.get("record") or {}
            held = stats.get("directory") or {}
            suite.check(
                "FILE.STATS counts the records and their bytes",
                held.get("record_count") == 2 and held.get("bytes") == len(HOSTILE) + 10,
                str(held),
            )
            suite.check(
                "And judges the file as the directory file it is",
                [m["id"] for m in stats["health"]["measures"]] == ["format", "largest_record"],
                str(stats.get("health")),
            )

            # --- PUT.BYTES and GET.BYTES ----------------------------------------
            # The point of the whole transfer path: a record far larger than a
            # request line, moved by a client with no filesystem access to the
            # server. 3 MiB against a 1 MiB line, and it holds newlines, NULs
            # and every mark byte - so anything that framed it, split it on a
            # terminator or ran it through base64 would come back different.
            big = (HOSTILE + b"\n\r\n" + bytes(range(256))) * 16384
            resp = admin.put_bytes(file=FILE, key="huge.bin", payload=big, account=ACCOUNT)
            suite.check(
                "PUT.BYTES stores a record far larger than one request line",
                resp.get("status") == "OK" and resp.get("length") == len(big),
                str(resp),
            )
            suite.check(
                "And the record on disk is those bytes exactly",
                open(os.path.join(records_dir, "huge.bin"), "rb").read() == big,
            )

            resp, body = admin.get_bytes(file=FILE, key="huge.bin", account=ACCOUNT)
            suite.check(
                "GET.BYTES announces the length and sends exactly that many",
                resp.get("length") == len(big) and body == big,
                f"announced {resp.get('length')}, got {len(body) if body is not None else 'nothing'}",
            )

            # The session is still usable afterwards. This is the assertion that
            # would fail if either side lost track of the body - one byte either
            # way and the next line read is the middle of a PDF.
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="note.txt")
            suite.check_eq("The connection is still in sync after a transfer", resp.get("record"), "plain text")

            # A refusal within the limit drains the body and keeps going.
            resp = admin.put_bytes(file=FILE, key="../escape", payload=b"x" * 4096, account=ACCOUNT)
            suite.check_eq("PUT.BYTES refuses a key that is not a file name", resp.get("code"), "INVALID_REQUEST")
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="note.txt")
            suite.check_eq(
                "And drains the body, so the next request is still read", resp.get("record"), "plain text"
            )

            resp = admin.put_bytes(file="USERS_ORDINARY", key="k", payload=b"x" * 16, account=ACCOUNT)
            suite.check_eq(
                "PUT.BYTES refuses an ordinary file", resp.get("code"), "INVALID_REQUEST"
            )

            resp, body = admin.get_bytes(file=FILE, key="absent.bin", account=ACCOUNT)
            suite.check(
                "GET.BYTES on a missing record is an ordinary refusal",
                resp.get("code") == "RECORD_NOT_FOUND" and body is None,
                str(resp),
            )

            # A body that runs short is refused rather than stored truncated,
            # and the connection closes because the socket is mid-body. A fresh
            # one shows the record was never written.
            short = harness.Client(port, admin_crt, admin_key, certs.ca_crt)
            try:
                resp = short.put_bytes(
                    file=FILE, key="truncated.bin", payload=b"x" * 40, length=4096, account=ACCOUNT
                )
                truncated_refused = resp.get("status") == "ERROR"
            except ConnectionError:
                # Equally correct: the server closed on a socket it could not
                # trust rather than answering.
                truncated_refused = True
            finally:
                short.close()
            suite.check("A body that runs short is refused", truncated_refused)
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="truncated.bin")
            suite.check_eq(
                "And nothing truncated was stored under its key", resp.get("code"), "RECORD_NOT_FOUND"
            )

            resp = admin.request(command="FILE.STATS", file=FILE, account=ACCOUNT)
            held = (resp.get("record") or {}).get("directory") or {}
            suite.check_eq(
                "An abandoned transfer leaves no record behind",
                held.get("record_count"),
                3,
            )

            # A client that announces a body and then stops sending is neither
            # idle - a request is in flight - nor finished, which is the case
            # `idle_timeout_ms` cannot see. Its own bound catches it.
            stalled = harness.Client(port, admin_crt, admin_key, certs.ca_crt)
            stalled.sock.settimeout(STALL_MS / 1000 * 8)
            started = time.time()
            try:
                header = {
                    "command": "PUT.BYTES",
                    "file": FILE,
                    "key": "stalled.bin",
                    "account": ACCOUNT,
                    "length": 1 << 20,
                }
                stalled.sock.sendall(json.dumps(header).encode() + b"\n" + b"x" * 64)
                # Nothing more is sent. The server gives up on its own, says so,
                # and then closes - the error line first, because a client that
                # is told why can act on it, and the close after, because the
                # socket is sitting in the middle of a body nobody will finish.
                first = stalled.sock.recv(65536)
                said_why = first != b"" and b"ERROR" in first
                after = stalled.sock.recv(65536)
                ended = said_why and after == b""
            except (ConnectionError, OSError):
                # Equally acceptable: the server dropped it without a word.
                ended = True
            finally:
                elapsed = time.time() - started
                stalled.close()
            suite.check(
                "A stalled transfer is ended rather than held open",
                ended and elapsed < STALL_MS / 1000 * 6,
                f"ended={ended} after {elapsed:.1f}s",
            )
            resp = admin.request(command="READ", file=FILE, account=ACCOUNT, key="stalled.bin")
            suite.check_eq(
                "And it stored nothing", resp.get("code"), "RECORD_NOT_FOUND"
            )

            # --- STORE and EXTRACT, past what a request line can carry -----------
            big_path = os.path.join(workspace.path, "big.bin")
            with open(big_path, "wb") as handle:
                handle.write(HOSTILE * (BIG_BYTES // len(HOSTILE)))
            expected = open(big_path, "rb").read()
            out_path = os.path.join(workspace.path, "big-out.bin")

            output = harness.run_cli(
                [
                    f"STORE {FILE} big.bin {big_path}",
                    f"LIST {FILE}",
                    f"EXTRACT {FILE} big.bin {out_path}",
                    "EXIT",
                ],
                args=["--account", ACCOUNT],
            )
            suite.check(
                "STORE streams a file far larger than one request line",
                f"{len(expected)} bytes" in output,
                output.strip().splitlines()[-6:],
            )
            suite.check(
                "EXTRACT brings it back byte for byte",
                os.path.exists(out_path) and open(out_path, "rb").read() == expected,
                f"{os.path.getsize(out_path) if os.path.exists(out_path) else 'missing'} bytes",
            )
            suite.check(
                "LIST shows the key and its size, not its content",
                "big.bin" in output and str(len(expected)) in output,
                output,
            )

            admin.close()
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Directory suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
