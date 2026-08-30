"""Per-file durable writes: a file marked durable must reach disk on every write
while the rest of the database keeps batching, and the flag must be readable and
changeable over the wire without recreating the file."""

import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "DUR_ACC"
DURABLE_FILE = "LEDGER"
BUFFERED_FILE = "SCRATCH"

SETUP_COMMANDS = [
    "CREATE.ACCOUNT " + ACCOUNT,
    "LOGTO " + ACCOUNT,
    "SAVE",
]

# Long enough that nothing flushes because of time or batch size during the run,
# so anything found on disk was written by the durability flag.
NO_AUTO_FLUSH = "flush_interval_ms = 3600000\nflush_max_pending = 1000000\n"


def records_on_disk(workspace, table):
    """Bytes stored in the table's hashfile groups. Zero means nothing flushed."""
    section = os.path.join(workspace.path, "db_storage", ACCOUNT, table, "data.hf")
    if not os.path.isdir(section):
        return 0
    return sum(
        os.path.getsize(os.path.join(section, name))
        for name in os.listdir(section)
        if name.startswith("g")
    )


def main():
    suite = harness.Suite("Durability", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN)

    with harness.Workspace("durability") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        reader_crt, reader_key, reader_tp = certs.client("reader")
        port = harness.free_port()
        harness.write_config(port, certs, extra=NO_AUTO_FLUSH)

        cli = harness.start_cli(["--account", "SYSTEM"])
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} test_client ADMIN\n")
            cli.stdin.write(f"AUTHORIZE.CONN {reader_tp} test_reader {ACCOUNT}\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with conn:
                # The account has to exist before a file can be created in it,
                # and the CLI is still working through the setup script.
                harness.wait_for_seed(
                    conn,
                    lambda resp: resp["status"] == "OK",
                    process=cli,
                    command="LIST.FILES",
                    account=ACCOUNT,
                )

                resp = conn.request(
                    command="CREATE.FILE", file=DURABLE_FILE, account=ACCOUNT, durable=True
                )
                suite.check_eq("CREATE.FILE with DURABLE is accepted", resp["status"], "OK")

                resp = conn.request(command="CREATE.FILE", file=BUFFERED_FILE, account=ACCOUNT)
                suite.check_eq("CREATE.FILE without DURABLE is accepted", resp["status"], "OK")

                resp = conn.request(
                    command="READ", file="DIR", key=DURABLE_FILE, account=ACCOUNT
                )
                record = resp.get("record") or {}
                suite.check_eq("DIR records the durability flag", record.get("durable"), "Y")

                resp = conn.request(command="READ", file="DIR", key=BUFFERED_FILE, account=ACCOUNT)
                record = resp.get("record") or {}
                suite.check_eq(
                    "DIR leaves other files buffered", record.get("durable", ""), ""
                )

                resp = conn.request(
                    command="WRITE", file=BUFFERED_FILE, key="K1", data="temp", account=ACCOUNT
                )
                suite.check_eq("WRITE to a buffered file", resp["status"], "OK")
                suite.check_eq(
                    "A buffered file is not on disk yet",
                    records_on_disk(workspace, BUFFERED_FILE),
                    0,
                )

                resp = conn.request(
                    command="WRITE", file=DURABLE_FILE, key="K1", data="critical", account=ACCOUNT
                )
                suite.check_eq("WRITE to a durable file", resp["status"], "OK")
                flushed = records_on_disk(workspace, DURABLE_FILE)
                suite.check(
                    "A durable file is on disk as soon as the write is acknowledged",
                    flushed > 0,
                    "%d bytes in %s/data.hf" % (flushed, DURABLE_FILE),
                )

                resp = conn.request(command="READ", file=DURABLE_FILE, key="K1", account=ACCOUNT)
                suite.check_eq("The durable record reads back", resp["status"], "OK")

                # The listing answers "which files are durable" without anyone
                # having to know that DIR is where the flag is kept.
                resp = conn.request(command="LIST.FILES", account=ACCOUNT)
                flags = {name: info.get("durable") for name, info in resp.get("results") or []}
                suite.check_eq("LIST.FILES marks the durable file", flags.get(DURABLE_FILE), True)
                suite.check_eq("LIST.FILES leaves the others alone", flags.get(BUFFERED_FILE), False)
                suite.check("LIST.FILES still returns plain names", DURABLE_FILE in (resp.get("keys") or []))

                # Promoting an existing file: the record it is already holding
                # must reach the disk with the flag, not after it.
                resp = conn.request(
                    command="SET.FILE", file=BUFFERED_FILE, account=ACCOUNT, durable=True
                )
                suite.check_eq("SET.FILE promotes a file", resp["status"], "OK")
                suite.check_eq(
                    "SET.FILE reports the new setting",
                    (resp.get("record") or {}).get("durable"),
                    True,
                )
                promoted = records_on_disk(workspace, BUFFERED_FILE)
                suite.check(
                    "Promoting flushes what the file had buffered",
                    promoted > 0,
                    "%d bytes in %s/data.hf" % (promoted, BUFFERED_FILE),
                )

                resp = conn.request(command="READ", file="DIR", key=BUFFERED_FILE, account=ACCOUNT)
                record = resp.get("record") or {}
                suite.check_eq("DIR carries the new flag", record.get("durable"), "Y")

                resp = conn.request(
                    command="WRITE", file=BUFFERED_FILE, key="K2", data="also critical", account=ACCOUNT
                )
                suite.check_eq("WRITE to the promoted file", resp["status"], "OK")
                suite.check(
                    "The promoted file flushes every later write too",
                    records_on_disk(workspace, BUFFERED_FILE) > promoted,
                )

                # And back the other way: a demoted file buffers like any other.
                resp = conn.request(
                    command="SET.FILE", file=DURABLE_FILE, account=ACCOUNT, durable=False
                )
                suite.check_eq("SET.FILE demotes a file", resp["status"], "OK")
                demoted = records_on_disk(workspace, DURABLE_FILE)
                resp = conn.request(
                    command="WRITE", file=DURABLE_FILE, key="K2", data="ordinary", account=ACCOUNT
                )
                suite.check_eq("WRITE to the demoted file", resp["status"], "OK")
                suite.check_eq(
                    "A demoted file goes back to being buffered",
                    records_on_disk(workspace, DURABLE_FILE),
                    demoted,
                )

                resp = conn.request(command="LIST.FILES", account=ACCOUNT)
                flags = {name: info.get("durable") for name, info in resp.get("results") or []}
                suite.check_eq("LIST.FILES follows the change", flags.get(DURABLE_FILE), False)
                suite.check_eq("LIST.FILES follows the promotion", flags.get(BUFFERED_FILE), True)

                resp = conn.request(command="FILE.STATS", file=BUFFERED_FILE, account=ACCOUNT)
                suite.check_eq(
                    "FILE.STATS reports the promoted file as durable",
                    (resp.get("record") or {}).get("durable"),
                    True,
                )

                # An omitted flag must not be read as "make it buffered".
                resp = conn.request(command="SET.FILE", file=BUFFERED_FILE, account=ACCOUNT)
                suite.check_eq("SET.FILE without a flag is refused", resp["status"], "ERROR")
                suite.check_eq(
                    "SET.FILE says which field is missing",
                    resp.get("message"),
                    "Durability flag not specified",
                )

                resp = conn.request(
                    command="SET.FILE", file="NO_SUCH_FILE", account=ACCOUNT, durable=True
                )
                suite.check_eq("SET.FILE on a missing file is refused", resp["status"], "ERROR")
                suite.check("SET.FILE says the file is not there", "not found" in (resp.get("message") or ""))

            # Durability is a storage decision, so it is admin only - a client
            # that may read and write the account still may not change it.
            with harness.Client(port, reader_crt, reader_key, certs.ca_crt) as reader:
                resp = reader.request(
                    command="SET.FILE", file=BUFFERED_FILE, account=ACCOUNT, durable=False
                )
                suite.check_eq("A non-admin client may not set durability", resp["status"], "ERROR")
                suite.check_eq(
                    "The refusal says why", resp.get("message"), "Admin privileges required"
                )
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Durability suite", exc)
        finally:
            output = harness.stop(cli)
            if suite.failures:
                print("--- CLI output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
