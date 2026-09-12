"""The storage format stamp, against the real binaries.

The unit tests cover the policy - what each version on disk leads to. This suite
covers the part that only exists once there is a process: that a directory the
build cannot open makes the **server exit non-zero having written nothing**,
with a message naming both versions, rather than starting and misreading it.
That refusal is the whole mechanism; a version number nothing enforces would be
a comment.
"""

import os
import subprocess
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "FMT_ACC"
FILE = "LEDGER"
STAMP = ".format"


def crc32c(data):
    """The checksum the state files carry, so a stamp written here is one the
    server accepts. Same polynomial as `hashfile::crc32c` - a stamp that failed
    its checksum would test the *unreadable* path instead of the version one."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
    return crc ^ 0xFFFFFFFF


def write_stamp(storage, version):
    body = f"version={version}\n".encode()
    with open(os.path.join(storage, STAMP), "wb") as handle:
        handle.write(b"checksum=%08x\n" % crc32c(body) + body)


def read_stamp(storage):
    path = os.path.join(storage, STAMP)
    if not os.path.exists(path):
        return None
    for line in open(path).read().splitlines():
        if line.startswith("version="):
            return int(line.split("=", 1)[1])
    return None


def start_and_wait(timeout=20):
    """Start the server and wait for it to exit, returning (code, output).

    Used for the runs that are *expected* to fail: a server that starts anyway
    would hang here, and the timeout is what turns that into a failed check
    rather than a suite that never finishes.
    """
    process = harness.start_server()
    try:
        output, _ = process.communicate(timeout=timeout)
        return process.returncode, output
    except subprocess.TimeoutExpired:
        process.kill()
        output, _ = process.communicate()
        return None, output


def main():
    suite = harness.Suite("Storage format", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("format") as workspace:
        certs = harness.Certificates(workspace.path)
        # Issued once: a second `certs.client("admin")` would mint a different
        # certificate with a different thumbprint, and only the first is the one
        # authorized below.
        admin_crt, admin_key, thumbprint = certs.client("admin")
        port = harness.free_port()
        storage = os.path.join(workspace.path, "db_storage")

        server = None
        try:
            # --- a fresh directory is created and stamped -------------------
            harness.write_config(port, certs=None)
            output = harness.run_cli(
                [
                    f"AUTHORIZE.CONN {thumbprint} admin ADMIN",
                    f"CREATE.ACCOUNT {ACCOUNT}",
                    f"LOGTO {ACCOUNT}",
                    f"CREATE.FILE {FILE}",
                    f"SET {FILE} L-1 OPENED^129.50",
                    "SAVE",
                    "EXIT",
                ],
                args=["--account", "SYSTEM"],
            )
            suite.check(
                "A fresh storage directory is created and says so",
                "created at storage format" in output,
                output.strip().splitlines()[0] if output.strip() else "",
            )
            suite.check_eq("and carries the stamp", read_stamp(storage), 1)

            # --- an ordinary start says nothing about the format ------------
            output = harness.run_cli(["EXIT"], args=["--account", "SYSTEM"])
            suite.check(
                "An ordinary start is silent about the format",
                "storage format" not in output,
                output.strip(),
            )

            # --- an unstamped directory is adopted, and the data survives ----
            # This is every deployment that exists today: the stamp is removed
            # to make one, and what matters is that the records still read back
            # afterwards rather than only that the server started.
            os.remove(os.path.join(storage, STAMP))
            output = harness.run_cli(
                [f"LOGTO {ACCOUNT}", f"GET {FILE} L-1", "EXIT"],
                args=["--account", "SYSTEM"],
            )
            suite.check(
                "A directory with no stamp is adopted, and says so",
                "carried no format stamp" in output,
                output.strip().splitlines()[0] if output.strip() else "",
            )
            suite.check("and the records it already held still read back", "OPENED" in output, output.strip())
            suite.check_eq("and it is stamped from then on", read_stamp(storage), 1)

            # --- a directory from a newer build stops the server -------------
            harness.write_config(port, certs)
            write_stamp(storage, 99)
            code, output = start_and_wait()
            suite.check_eq("A directory from a newer build stops the server", code, 1)
            suite.check(
                "and the refusal names the version found and the one this build writes",
                "99" in output and "storage format" in output,
                output.strip().splitlines()[-1] if output.strip() else "",
            )
            suite.check_eq(
                "and it is left exactly as it was found",
                read_stamp(storage),
                99,
            )

            # The CLI refuses the same directory, for the same reason: both
            # binaries open the same database and neither may be the way in.
            output = harness.run_cli(["EXIT"], args=["--account", "SYSTEM"])
            suite.check(
                "The CLI refuses it too",
                "Cannot open the storage directory" in output,
                output.strip(),
            )

            # --- a damaged stamp is refused rather than adopted --------------
            # The dangerous case: if "cannot read this" were treated as "never
            # had one", a directory of any version would be adopted as 1.
            with open(os.path.join(storage, STAMP), "w") as handle:
                handle.write("checksum=00000000\nversion=99\n")
            code, output = start_and_wait()
            suite.check_eq("A damaged stamp stops the server", code, 1)
            suite.check(
                "and says the version could not be determined",
                "unreadable" in output,
                output.strip().splitlines()[-1] if output.strip() else "",
            )

            # --- and it starts again once the stamp is one it understands ----
            write_stamp(storage, 1)
            server = harness.start_server()
            client = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            resp = client.request(command="READ", account=ACCOUNT, file=FILE, key="L-1")
            suite.check_eq("A stamp it understands lets it start again", resp.get("status"), "OK")

            stats = client.request(command="SERVER.STATS").get("record") or {}
            suite.check_eq(
                "SERVER.STATS reports the format this build writes",
                stats.get("storage_format"),
                1,
            )
            suite.check_eq(
                "and the oldest it will open",
                stats.get("storage_format_oldest_supported"),
                1,
            )
            client.close()
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Storage format suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
