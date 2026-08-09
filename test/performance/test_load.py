"""Load profile for the remote protocol: bulk writes followed by representative queries.

Timings are reported rather than asserted, because they depend heavily on the host.
The correctness of each query (its result count) *is* asserted, so a performance
regression that also breaks behaviour still fails the suite.
"""

import os
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "PERF_ACC"
FILE = "PERF"
NUM_RECORDS = int(os.environ.get("SRP_PERF_RECORDS", "10000"))
# Derived so the suite stays meaningful when the record count is overridden.
MIDDLE_SEQ = NUM_RECORDS // 2
BATCH_SIZE = max(1, min(500, NUM_RECORDS // 10))

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "Y",  # answer the "DIR file missing. Create and populate?" prompt
    f"CREATE.FILE {FILE}",
    f"SET DICT {FILE} VAL1 1",
    f"SET DICT {FILE} VAL2 2",
    f"SET DICT {FILE} SEQ 3",
    "SAVE",
]


def timed(func):
    start = time.perf_counter()
    result = func()
    return result, (time.perf_counter() - start)


def main():
    suite = harness.Suite(
        "Performance",
        "performance_results.md",
        title="Performance Test Results",
        detail_header="Performance Data",
    )
    harness.require_binaries(harness.CLI_BIN)

    with harness.Workspace("performance") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        port = harness.free_port()
        harness.write_config(port, certs)

        cli = harness.start_cli(["--account", "SYSTEM"])
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} perf_client ADMIN\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with conn:
                print(f"Writing {NUM_RECORDS} records...")

                def write_all():
                    failed = 0
                    for i in range(NUM_RECORDS):
                        resp = conn.request(
                            command="WRITE",
                            file=FILE,
                            key=f"REC{i}",
                            data=f"Val{i % 10}^Data{i % 100}^{i}",
                            account=ACCOUNT,
                        )
                        if resp["status"] != "OK":
                            failed += 1
                    return failed

                failed, write_time = timed(write_all)
                suite.check(
                    f"Write {NUM_RECORDS} records",
                    failed == 0,
                    f"{write_time:.2f}s total, {write_time / NUM_RECORDS * 1000:.2f}ms/record"
                    if failed == 0
                    else f"{failed} writes failed",
                )

                def query(query_string):
                    return conn.request(
                        command="QUERY", file=FILE, query_string=query_string, account=ACCOUNT
                    )

                resp, elapsed = timed(lambda: query(f"WITH SEQ = {MIDDLE_SEQ}"))
                count = len(resp.get("results") or [])
                suite.check(
                    f"Unique-match query (WITH SEQ = {MIDDLE_SEQ})",
                    count == 1,
                    f"{elapsed * 1000:.2f}ms, {count} result(s)",
                )

                resp, elapsed = timed(lambda: query("WITH VAL1 = Val5"))
                count = len(resp.get("results") or [])
                suite.check(
                    "Attribute query (WITH VAL1 = Val5)",
                    count == NUM_RECORDS // 10,
                    f"{elapsed * 1000:.2f}ms, {count} result(s)",
                )

                resp, elapsed = timed(lambda: query("WITH VAL1 = Val5 AND VAL2 = Data55"))
                count = len(resp.get("results") or [])
                suite.check(
                    "Compound query (WITH VAL1 = Val5 AND VAL2 = Data55)",
                    count == NUM_RECORDS // 100,
                    f"{elapsed * 1000:.2f}ms, {count} result(s)",
                )

                resp, elapsed = timed(lambda: query(""))
                count = len(resp.get("results") or [])
                suite.check(
                    "Full scan",
                    count == NUM_RECORDS,
                    f"{elapsed * 1000:.2f}ms, {count} result(s)",
                )

                resp, elapsed = timed(
                    lambda: conn.request(
                        command="SELECT",
                        file=FILE,
                        query_string="WITH VAL1 = Val5",
                        list_name="PERFLIST",
                        account=ACCOUNT,
                    )
                )
                suite.check(
                    "SELECT into a named list",
                    resp.get("count") == NUM_RECORDS // 10,
                    f"{elapsed * 1000:.2f}ms, {resp.get('count')} key(s)",
                )

                resp, elapsed = timed(
                    lambda: conn.request(
                        command="GET.NEXT",
                        list_name="PERFLIST",
                        batch_size=BATCH_SIZE,
                        account=ACCOUNT,
                    )
                )
                count = len(resp.get("results") or [])
                suite.check(
                    f"GET.NEXT batch of {BATCH_SIZE}",
                    count == BATCH_SIZE,
                    f"{elapsed * 1000:.2f}ms, {count} record(s)",
                )
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Performance suite", exc)
        finally:
            output = harness.stop(cli)
            if suite.failures:
                print("--- CLI output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
