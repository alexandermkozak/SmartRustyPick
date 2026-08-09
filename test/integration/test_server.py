"""End-to-end coverage of the remote protocol: WRITE, READ, QUERY, SELECT, GET.NEXT, DELETE."""

import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TEST_ACC"
FILE = "USERS"

SETUP_COMMANDS = [
    "CREATE.ACCOUNT " + ACCOUNT,
    "LOGTO " + ACCOUNT,
    "Y",  # answer the "DIR file missing. Create and populate?" prompt
    "CREATE.FILE " + FILE,
    # The dictionary maps attribute numbers onto names; the remote protocol serialises
    # records as JSON objects keyed by the camelCased dictionary entries.
    "SET DICT %s NAME 1" % FILE,
    "SET DICT %s SURNAME 2" % FILE,
    "SET DICT %s AGE 3" % FILE,
    "SAVE",
]


def main():
    suite = harness.Suite("Server protocol", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN)

    with harness.Workspace("server") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        port = harness.free_port()
        harness.write_config(port, certs)

        cli = harness.start_cli(["--account", "SYSTEM"])
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} test_client ADMIN\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with conn:
                resp = conn.request(
                    command="WRITE", file=FILE, key="USER1", data="John^Doe^30", account=ACCOUNT
                )
                suite.check_eq("WRITE", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="USER1", account=ACCOUNT)
                suite.check_eq(
                    "READ returns the structured record",
                    resp.get("record"),
                    {"name": "John", "surname": "Doe", "age": "30"},
                )

                resp = conn.request(
                    command="QUERY", file=FILE, query_string="WITH ID = USER1", account=ACCOUNT
                )
                keys = [item[0] for item in resp.get("results") or []]
                suite.check_eq("QUERY by ID", keys, ["USER1"])

                resp = conn.request(
                    command="QUERY", file=FILE, query_string="WITH NAME = John", account=ACCOUNT
                )
                keys = [item[0] for item in resp.get("results") or []]
                suite.check_eq("QUERY by dictionary name", keys, ["USER1"])

                resp = conn.request(
                    command="SELECT",
                    file=FILE,
                    query_string="WITH NAME = John",
                    list_name="MYLIST",
                    account=ACCOUNT,
                )
                suite.check_eq("SELECT builds a named list", resp.get("count"), 1)

                resp = conn.request(
                    command="GET.NEXT", list_name="MYLIST", batch_size=1, account=ACCOUNT
                )
                keys = [item[0] for item in resp.get("results") or []]
                suite.check_eq("GET.NEXT walks the list", keys, ["USER1"])

                resp = conn.request(
                    command="GET.NEXT", list_name="MYLIST", batch_size=1, account=ACCOUNT
                )
                suite.check_eq("GET.NEXT reports EOF at the end", resp["status"], "EOF")

                resp = conn.request(command="DELETE", file=FILE, key="USER1", account=ACCOUNT)
                suite.check_eq("DELETE", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="USER1", account=ACCOUNT)
                suite.check(
                    "READ after DELETE reports a missing record",
                    resp["status"] == "ERROR" and "Record not found" in (resp.get("message") or ""),
                    resp.get("message", ""),
                )

                resp = conn.request(command="READ", key="USER1", account=ACCOUNT)
                suite.check_eq(
                    "READ without a file is rejected", resp.get("message"), "File not specified"
                )
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Server protocol suite", exc)
        finally:
            output = harness.stop(cli)
            if suite.failures:
                print("--- CLI output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
