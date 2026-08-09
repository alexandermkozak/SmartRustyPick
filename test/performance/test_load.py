"""Load profile for the remote protocol: bulk writes followed by representative queries.

Every operation is measured over many iterations and reported as a latency
distribution (p50/p95/p99) plus throughput, because a single timing on a shared
machine says more about the host than about the code.

Three kinds of guard run here, in increasing order of trustworthiness:

* correctness  - every measured operation also asserts its result count, so a change
                 that is fast only because it stopped doing the work still fails;
* budgets      - absolute p95 ceilings, deliberately generous, scalable per host via
                 `SRP_PERF_BUDGET_SCALE`;
* ratios       - how cost grows as the file grows. These are host independent and are
                 what actually catches an accidental O(n^2), so they are the tightest.

Resource usage of the server process (peak RSS, CPU seconds) is sampled throughout,
which turns the suite into a leak detector as well as a benchmark.
"""

import os
import random
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "PERF_ACC"
FILE = "PERF"
NUM_RECORDS = int(os.environ.get("SRP_PERF_RECORDS", "10000"))
QUERY_ITERATIONS = int(os.environ.get("SRP_PERF_QUERY_ITERS", "20"))
READ_SAMPLES = min(int(os.environ.get("SRP_PERF_READ_SAMPLES", "1000")), NUM_RECORDS)

# The first slice is written and measured on its own so the same measurements can be
# repeated on the full file; the two sets of numbers give the growth ratios below.
FIRST_SLICE = max(1, NUM_RECORDS // 4)
GROWTH = NUM_RECORDS / FIRST_SLICE
# A write costs O(file size), so the fair comparison between the two write phases is
# the *average* file size during each phase, not the final one.
WRITE_GROWTH = (NUM_RECORDS + FIRST_SLICE) / FIRST_SLICE

MIDDLE_SEQ = NUM_RECORDS // 2
BATCH_SIZE = max(1, min(500, NUM_RECORDS // 10))

# Budgets are p95 milliseconds. Scan-bound operations are budgeted per 10k records so
# the suite stays meaningful when SRP_PERF_RECORDS is overridden.
SCAN_SCALE = max(NUM_RECORDS / 10000.0, 0.2)
# A remote write rewrites the whole table file, so its budget scales with the file too.
BUDGET_WRITE_MS = float(os.environ.get("SRP_BUDGET_WRITE_MS", "40")) * SCAN_SCALE
BUDGET_READ_MS = float(os.environ.get("SRP_BUDGET_READ_MS", "25"))
BUDGET_UNIQUE_MS = float(os.environ.get("SRP_BUDGET_UNIQUE_MS", "60")) * SCAN_SCALE
BUDGET_ATTRIBUTE_MS = float(os.environ.get("SRP_BUDGET_ATTRIBUTE_MS", "120")) * SCAN_SCALE
BUDGET_COMPOUND_MS = float(os.environ.get("SRP_BUDGET_COMPOUND_MS", "80")) * SCAN_SCALE
BUDGET_SCAN_MS = float(os.environ.get("SRP_BUDGET_SCAN_MS", "500")) * SCAN_SCALE
BUDGET_SELECT_MS = float(os.environ.get("SRP_BUDGET_SELECT_MS", "120")) * SCAN_SCALE
BUDGET_GET_NEXT_MS = float(os.environ.get("SRP_BUDGET_GET_NEXT_MS", "60"))
# Resident memory attributable to the data set. A record is ~30 bytes on the wire, so
# this ceiling is very loose; it exists to catch a leak or a per-record blow-up.
BUDGET_BYTES_PER_RECORD = float(os.environ.get("SRP_BUDGET_BYTES_PER_RECORD", "8192"))

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


def record_data(i):
    return f"Val{i % 10}^Data{i % 100}^{i}"


def write_range(conn, start, end):
    """Write records [start, end) and return their latency distribution."""
    failures = []

    def write_one(offset):
        i = start + offset
        resp = conn.request(
            command="WRITE", file=FILE, key=f"REC{i}", data=record_data(i), account=ACCOUNT
        )
        if resp["status"] != "OK":
            failures.append(resp.get("message"))

    stats, _ = harness.benchmark(write_one, end - start)
    return stats, failures


def query_stats(conn, query_string, iterations=QUERY_ITERATIONS):
    """Run the same query repeatedly and return (Stats, result count)."""
    stats, resp = harness.benchmark(
        lambda _: conn.request(
            command="QUERY", file=FILE, query_string=query_string, account=ACCOUNT
        ),
        iterations,
        warmup=1,
    )
    return stats, len(resp.get("results") or [])


def main():
    suite = harness.Suite(
        "Performance",
        "performance_results.md",
        title="Performance Test Results",
        detail_header="Measurement",
        metrics_file="performance_metrics.json",
    )
    harness.require_binaries(harness.CLI_BIN)

    with harness.Workspace("performance") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        port = harness.free_port()
        harness.write_config(port, certs)

        cli = harness.start_cli(["--account", "SYSTEM"])
        monitor = harness.ResourceMonitor(cli.pid)
        monitor.start()
        started = time.perf_counter()
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} perf_client ADMIN\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with conn:
                print(f"Writing {NUM_RECORDS} records in two phases...")

                early, early_failures = write_range(conn, 0, FIRST_SLICE)
                suite.measure(
                    f"Write first {FIRST_SLICE} records",
                    early,
                    budget_ms=BUDGET_WRITE_MS,
                    passed=not early_failures,
                    extra=f"{early.total_ms / 1000:.2f}s total"
                    if not early_failures
                    else f"{len(early_failures)} failed ({early_failures[0]})",
                )
                small_scan, small_count = query_stats(conn, "", iterations=max(3, QUERY_ITERATIONS // 4))

                late, late_failures = write_range(conn, FIRST_SLICE, NUM_RECORDS)
                suite.measure(
                    f"Write remaining {NUM_RECORDS - FIRST_SLICE} records",
                    late,
                    budget_ms=BUDGET_WRITE_MS,
                    passed=not late_failures,
                    extra=f"{late.total_ms / 1000:.2f}s total"
                    if not late_failures
                    else f"{len(late_failures)} failed ({late_failures[0]})",
                )

                # Known design cost: the remote WRITE handler calls `Database::save()`,
                # which rewrites the whole table file, so a single write is O(file size)
                # and a bulk load is O(n^2). This guard pins that at *linear* - it fails
                # if the per-write cost ever starts growing faster than the file does.
                suite.check_ratio(
                    "Write cost grows no worse than the file size",
                    late.p50 / early.p50 if early.p50 else 0.0,
                    WRITE_GROWTH * float(os.environ.get("SRP_LIMIT_WRITE_SLACK", "1.5")),
                    detail=f"p50 {early.p50:.2f}ms -> {late.p50:.2f}ms at "
                    f"{WRITE_GROWTH:.1f}x the average file size",
                )

                keys = [f"REC{i}" for i in range(NUM_RECORDS)]
                rng = random.Random(20240607)
                sample = [rng.choice(keys) for _ in range(READ_SAMPLES)]
                misses = []

                def read_one(i):
                    resp = conn.request(command="READ", file=FILE, key=sample[i], account=ACCOUNT)
                    if resp["status"] != "OK":
                        misses.append(sample[i])

                read, _ = harness.benchmark(read_one, READ_SAMPLES, warmup=1)
                suite.measure(
                    f"Random point reads ({READ_SAMPLES})",
                    read,
                    budget_ms=BUDGET_READ_MS,
                    passed=not misses,
                    extra="all records found" if not misses else f"{len(misses)} missing",
                )

                stats, count = query_stats(conn, f"WITH SEQ = {MIDDLE_SEQ}")
                suite.measure(
                    "Unique-match query",
                    stats,
                    budget_ms=BUDGET_UNIQUE_MS,
                    passed=count == 1,
                    extra=f"{count} result(s)",
                )

                stats, count = query_stats(conn, "WITH VAL1 = Val5")
                suite.measure(
                    "Attribute query (10% of the file)",
                    stats,
                    budget_ms=BUDGET_ATTRIBUTE_MS,
                    passed=count == NUM_RECORDS // 10,
                    extra=f"{count} result(s)",
                )

                stats, count = query_stats(conn, "WITH VAL1 = Val5 AND VAL2 = Data55")
                suite.measure(
                    "Compound query (1% of the file)",
                    stats,
                    budget_ms=BUDGET_COMPOUND_MS,
                    passed=count == NUM_RECORDS // 100,
                    extra=f"{count} result(s)",
                )

                full_scan, count = query_stats(conn, "", iterations=max(3, QUERY_ITERATIONS // 4))
                suite.measure(
                    "Full scan",
                    full_scan,
                    budget_ms=BUDGET_SCAN_MS,
                    passed=count == NUM_RECORDS,
                    extra=f"{count} result(s)",
                )

                # The strongest regression signal in this suite: scanning 4x the data
                # must cost roughly 4x, not 16x.
                suite.check_ratio(
                    "Full scan cost grows no worse than linearly",
                    full_scan.p50 / small_scan.p50 if small_scan.p50 else 0.0,
                    GROWTH * float(os.environ.get("SRP_LIMIT_SCAN_SLACK", "1.8")),
                    detail=f"{small_count} -> {count} records, "
                    f"p50 {small_scan.p50:.2f}ms -> {full_scan.p50:.2f}ms",
                )

                stats, resp = harness.benchmark(
                    lambda i: conn.request(
                        command="SELECT",
                        file=FILE,
                        query_string="WITH VAL1 = Val5",
                        list_name=f"PERFLIST{i}",
                        account=ACCOUNT,
                    ),
                    QUERY_ITERATIONS,
                    warmup=1,
                )
                suite.measure(
                    "SELECT into a named list",
                    stats,
                    budget_ms=BUDGET_SELECT_MS,
                    passed=resp.get("count") == NUM_RECORDS // 10,
                    extra=f"{resp.get('count')} key(s)",
                )

                list_name = f"PERFLIST{QUERY_ITERATIONS - 1}"
                batches = max(1, (NUM_RECORDS // 10) // BATCH_SIZE)
                short = []

                def get_next(_i):
                    resp = conn.request(
                        command="GET.NEXT",
                        list_name=list_name,
                        batch_size=BATCH_SIZE,
                        account=ACCOUNT,
                    )
                    if len(resp.get("results") or []) != BATCH_SIZE:
                        short.append(resp.get("status"))

                stats, _ = harness.benchmark(get_next, batches)
                suite.measure(
                    f"GET.NEXT, {batches} batches of {BATCH_SIZE}",
                    stats,
                    budget_ms=BUDGET_GET_NEXT_MS,
                    passed=not short,
                    extra=f"{batches * BATCH_SIZE} records drained"
                    if not short
                    else f"{len(short)} short batches",
                )

            elapsed = time.perf_counter() - started
            monitor.stop()
            if monitor.available:
                growth_kb = max(monitor.peak_rss_kb - (monitor.first_rss_kb or 0), 0)
                bytes_per_record = growth_kb * 1024.0 / NUM_RECORDS
                suite.record("Server resources", dict(monitor.as_dict(), wall_seconds=round(elapsed, 2)))
                suite.check(
                    "Resident memory per record",
                    bytes_per_record <= BUDGET_BYTES_PER_RECORD * harness.BUDGET_SCALE
                    or not harness.ENFORCE_BUDGETS,
                    f"{bytes_per_record:.0f} B/record over {NUM_RECORDS} records "
                    f"(budget {BUDGET_BYTES_PER_RECORD:.0f}); {monitor.summary()}",
                )
                suite.check(
                    "CPU time is accounted for",
                    monitor.cpu_seconds <= elapsed * (os.cpu_count() or 1),
                    f"{monitor.cpu_seconds:.2f}s CPU over {elapsed:.2f}s wall "
                    f"({monitor.cpu_seconds / elapsed * 100:.0f}% of one core)",
                )
            else:
                print("  [Skipped] resource monitoring is unavailable on this platform")
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Performance suite", exc)
        finally:
            monitor.stop()
            output = harness.stop(cli)
            if suite.failures:
                print("--- CLI output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
