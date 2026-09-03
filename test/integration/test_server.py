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
    "CREATE.FILE " + FILE,
    # The dictionary maps attribute numbers onto names; the remote protocol serialises
    # records as JSON objects keyed by the camelCased dictionary entries.
    "SET DICT %s NAME 1" % FILE,
    "SET DICT %s SURNAME 2" % FILE,
    "SET DICT %s AGE 3" % FILE,
    "SET DICT %s ROLES 4" % FILE,
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
                # The listener comes up while the CLI is still reading the setup
                # script, so the dictionary the assertions below expect is what
                # is waited for - not merely a connection.
                harness.wait_for_seed(
                    conn,
                    lambda resp: sorted(resp.get("keys") or []) == ["AGE", "NAME", "ROLES", "SURNAME"],
                    process=cli,
                    command="LIST.DICT",
                    file=FILE,
                    account=ACCOUNT,
                )

                resp = conn.request(
                    command="WRITE", file=FILE, key="USER1", data="John^Doe^30", account=ACCOUNT
                )
                suite.check_eq("WRITE", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="USER1", account=ACCOUNT)
                suite.check_eq(
                    "READ returns the structured record",
                    resp.get("record"),
                    {"name": "John", "surname": "Doe", "age": "30", "roles": ""},
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

                # Multivalue: a record whose ROLES field holds three values, the
                # last of them sub-valued.
                resp = conn.request(
                    command="WRITE",
                    file=FILE,
                    key="USER2",
                    data="Jane^Smith^41^ADMIN]DEV]TEST\\LAB",
                    account=ACCOUNT,
                )
                suite.check_eq("WRITE a multivalued record", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="USER2", account=ACCOUNT)
                record = resp.get("record") or {}
                suite.check_eq(
                    "READ shapes a multivalued field as an array",
                    record.get("roles"),
                    ["ADMIN", "DEV", ["TEST", "LAB"]],
                )
                suite.check_eq(
                    "READ leaves a single-valued field a string", record.get("name"), "Jane"
                )

                # Writing the record straight back must not flatten it.
                resp = conn.request(
                    command="WRITE",
                    file=FILE,
                    key="USER2",
                    structured_data=record,
                    account=ACCOUNT,
                )
                suite.check_eq("WRITE the record back unchanged", resp["status"], "OK")
                resp = conn.request(command="READ", file=FILE, key="USER2", account=ACCOUNT)
                suite.check_eq(
                    "the multivalue structure survives the round trip",
                    (resp.get("record") or {}).get("roles"),
                    ["ADMIN", "DEV", ["TEST", "LAB"]],
                )

                resp = conn.request(
                    command="QUERY",
                    file=FILE,
                    query_string="WITH ROLES = [TEST]",
                    explode=["ROLES"],
                    account=ACCOUNT,
                )
                keys = [item[0] for item in resp.get("results") or []]
                suite.check_eq("QUERY explodes on the matching value", keys, ["USER2"])
                suite.check_eq(
                    "QUERY reports which position matched",
                    resp.get("positions"),
                    [{"value": 2, "sub_value": 0}],
                )

                resp = conn.request(
                    command="SELECT",
                    file=FILE,
                    query_string="BY.EXP ROLES",
                    list_name="MVLIST",
                    account=ACCOUNT,
                )
                # Four rows, not three: USER1 has no ROLES at all, and a record
                # the explode clause cannot expand stays as one unexploded row.
                suite.check_eq("SELECT counts exploded rows, not records", resp.get("count"), 4)

                resp = conn.request(
                    command="GET.NEXT", list_name="MVLIST", batch_size=10, account=ACCOUNT
                )
                suite.check_eq(
                    "GET.NEXT carries the exploded positions",
                    resp.get("positions"),
                    [
                        None,
                        {"value": 0, "sub_value": None},
                        {"value": 1, "sub_value": None},
                        {"value": 2, "sub_value": None},
                    ],
                )

                resp = conn.request(command="DELETE", file=FILE, key="USER2", account=ACCOUNT)
                suite.check_eq("DELETE the multivalued record", resp["status"], "OK")

                resp = conn.request(command="DELETE", file=FILE, key="USER1", account=ACCOUNT)
                suite.check_eq("DELETE", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="USER1", account=ACCOUNT)
                suite.check(
                    "READ after DELETE reports a missing record",
                    resp["status"] == "ERROR" and resp.get("code") == "RECORD_NOT_FOUND",
                    resp.get("message", ""),
                )

                resp = conn.request(command="READ", key="USER1", account=ACCOUNT)
                suite.check_eq(
                    "READ without a file is rejected", resp.get("code"), "MISSING_FIELD"
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
