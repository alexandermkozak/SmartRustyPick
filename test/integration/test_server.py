"""End-to-end coverage of the remote protocol: WRITE, READ, QUERY, SELECT, GET.NEXT, DELETE."""

import base64
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
    # ACCT.DATES is associated with ACCOUNTS, so the two explode in lockstep and
    # the protocol has a group to answer for as well as a lone field. They are
    # their own pair rather than hung off ROLES, which stays unassociated so the
    # single-field checks above keep describing a single field.
    "SET DICT %s ACCOUNTS 5" % FILE,
    "SET DICT %s ACCT.DATES 6^ACCT.DATES^L^10^ACCOUNTS^V" % FILE,
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
                    lambda resp: sorted(resp.get("keys") or [])
                    == ["ACCOUNTS", "ACCT.DATES", "AGE", "NAME", "ROLES", "SURNAME"],
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
                    {
                        "name": "John",
                        "surname": "Doe",
                        "age": "30",
                        "roles": "",
                        "accounts": "",
                        "acctDates": "",
                    },
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

                # --- association groups -----------------------------------
                # USER3 holds three accounts and two dates. They are one group,
                # so exploding either explodes both, and the third account keeps
                # its row with an empty date rather than being dropped.
                resp = conn.request(
                    command="WRITE",
                    file=FILE,
                    key="USER3",
                    data="Ann^Roe^41^ADMIN^TEST]PAYROLL]LAB^2019]2021",
                    account=ACCOUNT,
                )
                suite.check_eq("WRITE a record with an association group", resp["status"], "OK")

                for named in (["ACCOUNTS"], ["ACCT.DATES"], ["ACCOUNTS", "ACCT.DATES"]):
                    resp = conn.request(
                        command="QUERY",
                        file=FILE,
                        query_string="WITH ID = USER3",
                        explode=named,
                        account=ACCOUNT,
                    )
                    suite.check_eq(
                        "QUERY explodes the group named by %s" % "+".join(named),
                        resp.get("positions"),
                        [
                            {"value": 0, "sub_value": None},
                            {"value": 1, "sub_value": None},
                            {"value": 2, "sub_value": None},
                        ],
                    )

                # A criterion on one member positions the whole group.
                resp = conn.request(
                    command="QUERY",
                    file=FILE,
                    query_string="WITH ID = USER3 AND ACCT.DATES = 2021",
                    explode=["ACCOUNTS"],
                    account=ACCOUNT,
                )
                suite.check_eq(
                    "a criterion on one member positions the group",
                    resp.get("positions"),
                    [{"value": 1, "sub_value": None}],
                )

                # Two fields no association pairs still have no defined pairing.
                resp = conn.request(
                    command="QUERY", file=FILE, explode=["ACCOUNTS", "NAME"], account=ACCOUNT
                )
                suite.check_eq(
                    "unassociated explode fields are refused",
                    (resp.get("status"), resp.get("code")),
                    ("ERROR", "INVALID_QUERY"),
                )

                resp = conn.request(command="DELETE", file=FILE, key="USER3", account=ACCOUNT)
                suite.check_eq("DELETE the associated record", resp["status"], "OK")

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

                # A record is a byte container. Bytes that are not valid UTF-8
                # used to be replaced with U+FFFD on the way in and were gone
                # for good; they travel in a tagged envelope now. The mark bytes
                # (0xFC-0xFE) are excluded because they are the record's
                # structure, not content - see docs/data_structures.md.
                raw = bytes(b for b in range(256) if b not in (0xFC, 0xFD, 0xFE))
                encoded = base64.b64encode(raw).decode("ascii")
                resp = conn.request(
                    command="WRITE",
                    file=FILE,
                    key="BINARY",
                    account=ACCOUNT,
                    structured_data={"name": {"$base64": encoded}},
                )
                suite.check_eq("A record of raw bytes is accepted", resp["status"], "OK")

                resp = conn.request(command="READ", file=FILE, key="BINARY", account=ACCOUNT)
                returned = resp.get("record", {}).get("name")
                suite.check(
                    "...and every byte comes back exactly as it went in",
                    isinstance(returned, dict)
                    and base64.b64decode(returned.get("$base64", "")) == raw,
                    "got %r" % (returned,),
                )

                resp = conn.request(
                    command="WRITE",
                    file=FILE,
                    key="BADBINARY",
                    account=ACCOUNT,
                    structured_data={"name": {"$base64": "not base64!"}},
                )
                suite.check_eq(
                    "A binary envelope that does not decode is refused",
                    resp.get("code"),
                    "INVALID_DATA",
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
