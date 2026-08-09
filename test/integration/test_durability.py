"""Per-file durable writes: a file created with the DURABLE flag must reach disk
on every write, while the rest of the database keeps batching."""

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
    "Y",  # answer the "DIR file missing. Create and populate?" prompt
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
        port = harness.free_port()
        harness.write_config(port, certs, extra=NO_AUTO_FLUSH)

        cli = harness.start_cli(["--account", "SYSTEM"])
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} test_client ADMIN\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with conn:
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
