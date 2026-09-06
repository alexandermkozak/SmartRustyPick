"""Transactions over the remote protocol: a set of writes and deletes that
lands whole or not at all.

The unit tests cover the format, the refusals and a process killed between the
two halves of a set. This suite covers what only exists once there is a real
server on the far side of a TLS connection: that `TRANSACT` puts a set across
two files on disk *before it answers*, with nothing else in the configuration
able to have flushed them, and that a `SIGKILL` immediately afterwards takes
nothing with it.
"""

import os
import signal
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TXN_ACC"
ORDERS = "ORDERS"
BASKETS = "BASKETS"
QUEUE = "JOBS"

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "SAVE",
]

# Nothing may flush because of time or batch size, and neither file is durable,
# so anything on disk after a TRANSACT was written by the transaction itself.
NO_AUTO_FLUSH = "flush_interval_ms = 3600000\nflush_max_pending = 1000000\n"


def records_on_disk(workspace, table):
    """Bytes in the table's hashfile groups. Zero means nothing has flushed."""
    section = os.path.join(workspace.path, "db_storage", ACCOUNT, table, "data.hf")
    if not os.path.isdir(section):
        return 0
    return sum(
        os.path.getsize(os.path.join(section, name))
        for name in os.listdir(section)
        if name.startswith("g")
    )


def pending_intents(workspace):
    log = os.path.join(workspace.path, "db_storage", ".txn")
    if not os.path.isdir(log):
        return []
    return [name for name in os.listdir(log) if name.endswith(".intent")]


def main():
    suite = harness.Suite("Transaction", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("transaction") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        port = harness.free_port()

        server = None
        try:
            # Seeded without TLS paths so the CLI does not auto-start a server
            # on the port the headless one needs.
            harness.write_config(port, certs=None, extra=NO_AUTO_FLUSH)
            harness.run_cli(
                [f"AUTHORIZE.CONN {admin_tp} admin ADMIN", *SETUP_COMMANDS, "EXIT"],
                args=["--account", "SYSTEM"],
            )

            harness.write_config(port, certs, extra=NO_AUTO_FLUSH)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)

            for name in (ORDERS, BASKETS):
                resp = admin.request(command="CREATE.FILE", file=name, account=ACCOUNT)
                suite.check_eq(f"CREATE.FILE {name}", resp.get("status"), "OK")
            resp = admin.request(command="CREATE.FILE", file=QUEUE, account=ACCOUNT, queue=True)
            suite.check_eq(f"CREATE.FILE {QUEUE} QUEUE", resp.get("status"), "OK")

            # --- A set across two files ---------------------------------------
            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[
                    {"op": "WRITE", "file": ORDERS, "key": "O-1", "data": "PLACED^129.50"},
                    {"op": "WRITE", "file": BASKETS, "key": "B-1", "data": "EMPTIED"},
                ],
            )
            suite.check_eq("TRANSACT across two files", resp.get("status"), "OK")
            suite.check_eq("and reports how many changes it applied", resp.get("count"), 2)

            for name in (ORDERS, BASKETS):
                resp = admin.request(command="READ", file=name, account=ACCOUNT, key="O-1" if name == ORDERS else "B-1")
                suite.check_eq(f"The {name} record reads back", resp.get("status"), "OK")

            # Nothing here is durable and no timer can have fired, so this is the
            # transaction's own promise: on disk before the reply was sent.
            suite.check(
                "Both files are on disk before the reply, though neither is DURABLE",
                records_on_disk(workspace, ORDERS) > 0 and records_on_disk(workspace, BASKETS) > 0,
                f"{records_on_disk(workspace, ORDERS)} and {records_on_disk(workspace, BASKETS)} bytes",
            )
            suite.check_eq("The intent is retired once the set is durable", pending_intents(workspace), [])

            # --- A refused set writes nothing ---------------------------------
            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[
                    {"op": "WRITE", "file": ORDERS, "key": "O-2", "data": "PLACED"},
                    {"op": "WRITE", "file": "NOWHERE", "key": "X", "data": "X"},
                ],
            )
            suite.check_eq("A file that is not there refuses the set", resp.get("code"), "FILE_NOT_FOUND")
            suite.check_eq(
                "and the change to the file that does exist was not applied",
                admin.request(command="READ", file=ORDERS, account=ACCOUNT, key="O-2").get("code"),
                "RECORD_NOT_FOUND",
            )

            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[
                    {"op": "WRITE", "file": ORDERS, "key": "O-3", "data": "PLACED"},
                    {"op": "WRITE", "file": QUEUE, "key": "anything", "data": "X"},
                ],
            )
            suite.check_eq("A queue file is out of scope, with its own code", resp.get("code"), "TRANSACTION_SCOPE")
            suite.check_eq(
                "and again nothing was applied",
                admin.request(command="READ", file=ORDERS, account=ACCOUNT, key="O-3").get("code"),
                "RECORD_NOT_FOUND",
            )

            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[
                    {"op": "WRITE", "file": ORDERS, "key": "O-4", "data": "PLACED"},
                    {"op": "DELETE", "file": ORDERS, "key": "O-4"},
                ],
            )
            suite.check_eq("One key changed twice is refused", resp.get("code"), "INVALID_REQUEST")

            # --- Deletes travel with writes ------------------------------------
            resp = admin.request(
                command="TRANSACT",
                account=ACCOUNT,
                changes=[
                    {"op": "DELETE", "file": BASKETS, "key": "B-1"},
                    {"op": "WRITE", "file": ORDERS, "key": "O-9", "data": "SHIPPED"},
                ],
            )
            suite.check_eq("A set mixing a delete and a write", resp.get("status"), "OK")
            suite.check_eq(
                "The delete landed",
                admin.request(command="READ", file=BASKETS, account=ACCOUNT, key="B-1").get("code"),
                "RECORD_NOT_FOUND",
            )
            suite.check_eq(
                "and so did the write beside it",
                admin.request(command="READ", file=ORDERS, account=ACCOUNT, key="O-9").get("status"),
                "OK",
            )

            # --- SIGKILL takes nothing acknowledged with it ---------------------
            admin.close()
            os.kill(server.pid, signal.SIGKILL)
            server.wait(timeout=10)

            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            survived = {
                (ORDERS, "O-1"): "OK",
                (ORDERS, "O-9"): "OK",
                (BASKETS, "B-1"): "RECORD_NOT_FOUND",
                (ORDERS, "O-2"): "RECORD_NOT_FOUND",
                (ORDERS, "O-3"): "RECORD_NOT_FOUND",
                (ORDERS, "O-4"): "RECORD_NOT_FOUND",
            }
            for (name, key), expected in survived.items():
                resp = admin.request(command="READ", file=name, account=ACCOUNT, key=key)
                found = resp.get("status") if resp.get("status") == "OK" else resp.get("code")
                suite.check_eq(f"After a hard kill, {name}/{key}", found, expected)

            admin.close()
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Transaction suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
