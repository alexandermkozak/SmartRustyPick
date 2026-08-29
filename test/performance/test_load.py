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
# Exploding is measured on its own file so every measurement above keeps the
# record shape its published numbers were taken against: adding a multivalued
# attribute to PERF would move every write, read and scan number at once and
# make the run-to-run comparison meaningless for one run.
MV_FILE = "PERFMV"
# Values per multivalued record. A bare explode yields this many rows per
# record, so the file is kept to a slice of the main one and still produces more
# rows than PERF has records.
MV_VALUES = 8
NUM_RECORDS = int(os.environ.get("SRP_PERF_RECORDS", "10000"))
QUERY_ITERATIONS = int(os.environ.get("SRP_PERF_QUERY_ITERS", "20"))
READ_SAMPLES = min(int(os.environ.get("SRP_PERF_READ_SAMPLES", "1000")), NUM_RECORDS)

# The first slice is written and measured on its own so the same measurements can be
# repeated on the full file; the two sets of numbers give the growth ratios below.
FIRST_SLICE = max(1, NUM_RECORDS // 4)
GROWTH = NUM_RECORDS / FIRST_SLICE
# Records are stored in a hashfile whose modulus grows with the table, so a write
# rewrites one group rather than the whole file: its cost must stay *constant* as
# the table grows. This is the allowed drift between the two write phases, which
# see the file at very different sizes.
WRITE_GROWTH = float(os.environ.get("SRP_LIMIT_WRITE_GROWTH", "2.0"))

MV_RECORDS = max(1, NUM_RECORDS // 4)

MIDDLE_SEQ = NUM_RECORDS // 2
BATCH_SIZE = max(1, min(500, NUM_RECORDS // 10))

# Sorting is O(n log n), so 4x the records is a little under 5x the comparisons -
# nothing like the 16x an accidental O(n^2) would cost. That gap is what this limit
# separates, which is why it can be well below 16 without being fragile.
SORT_GROWTH = float(os.environ.get("SRP_LIMIT_SORT_GROWTH", "8.0"))
# How much of a scan's cost sorting it is allowed to add. End to end a full scan is
# dominated by serialising and shipping the records, so this ratio is coarse by
# construction: it fails when sorting becomes the dominant cost of a query, not when
# the comparator merely gets slower. The sharp guard on the comparator itself is the
# `sort` group in `cargo bench`, which measures it without TLS or JSON in the way.
SORT_OVERHEAD = float(os.environ.get("SRP_LIMIT_SORT_OVERHEAD", "2.5"))

# Budgets are p95 milliseconds. Scan-bound operations are budgeted per 10k records so
# the suite stays meaningful when SRP_PERF_RECORDS is overridden.
SCAN_SCALE = max(NUM_RECORDS / 10000.0, 0.2)
# A write touches one hashfile group and is batched with its neighbours, so unlike the
# scan-bound budgets below it does not scale with the size of the file.
BUDGET_WRITE_MS = float(os.environ.get("SRP_BUDGET_WRITE_MS", "10"))
BUDGET_READ_MS = float(os.environ.get("SRP_BUDGET_READ_MS", "25"))
BUDGET_UNIQUE_MS = float(os.environ.get("SRP_BUDGET_UNIQUE_MS", "60")) * SCAN_SCALE
BUDGET_ATTRIBUTE_MS = float(os.environ.get("SRP_BUDGET_ATTRIBUTE_MS", "120")) * SCAN_SCALE
BUDGET_COMPOUND_MS = float(os.environ.get("SRP_BUDGET_COMPOUND_MS", "80")) * SCAN_SCALE
BUDGET_SCAN_MS = float(os.environ.get("SRP_BUDGET_SCAN_MS", "500")) * SCAN_SCALE
BUDGET_SORTED_SCAN_MS = float(os.environ.get("SRP_BUDGET_SORTED_SCAN_MS", "600")) * SCAN_SCALE
BUDGET_SELECT_MS = float(os.environ.get("SRP_BUDGET_SELECT_MS", "120")) * SCAN_SCALE
BUDGET_GET_NEXT_MS = float(os.environ.get("SRP_BUDGET_GET_NEXT_MS", "60"))
# Exploding scans MV_FILE and builds one row per matching value. Scaled off the
# MV file rather than the main one, since that is what it walks.
MV_SCAN_SCALE = max(MV_RECORDS / 10000.0, 0.2)
BUDGET_EXPLODE_MS = float(os.environ.get("SRP_BUDGET_EXPLODE_MS", "200")) * MV_SCAN_SCALE
BUDGET_EXPLODE_BARE_MS = float(os.environ.get("SRP_BUDGET_EXPLODE_BARE_MS", "1600")) * MV_SCAN_SCALE
# A bare explode walks the same records as a selective one but returns far more
# rows. Cost must therefore grow no faster than the row count does: at or below
# 1.0x the row ratio the per-row work is linear, which is the property that
# separates it from an accidentally quadratic row builder.
EXPLODE_ROW_GROWTH = float(os.environ.get("SRP_LIMIT_EXPLODE_ROW_GROWTH", "1.0"))
# Resident memory attributable to the data set, which spans both files: dividing
# by PERF's records alone would charge MV_FILE's records to them. A record is
# ~30 bytes on the wire, so this ceiling is very loose; it exists to catch a leak
# or a per-record blow-up.
BUDGET_BYTES_PER_RECORD = float(os.environ.get("SRP_BUDGET_BYTES_PER_RECORD", "8192"))
TOTAL_RECORDS = NUM_RECORDS + MV_RECORDS

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "Y",  # answer the "DIR file missing. Create and populate?" prompt
    f"CREATE.FILE {FILE}",
    f"SET DICT {FILE} VAL1 1",
    f"SET DICT {FILE} VAL2 2",
    f"SET DICT {FILE} SEQ 3",
    f"CREATE.FILE {MV_FILE}",
    f"SET DICT {MV_FILE} VAL1 1",
    f"SET DICT {MV_FILE} TAGS 2",
    "SAVE",
]


def record_data(i):
    return f"Val{i % 10}^Data{i % 100}^{i}"


# TAGS holds MV_VALUES values drawn from a hundred, so `TAGS = Tag42` matches
# MV_VALUES records in every hundred, with one matching value each.
def mv_record_data(i):
    tags = "]".join(f"Tag{(i + k) % 100}" for k in range(MV_VALUES))
    return f"Val{i % 10}^{tags}"


def find_section_dir(root, table):
    '''Locate the hashed record section of `table` inside a workspace.'''
    for path, dirs, _files in os.walk(root):
        if os.path.basename(path) == table and 'data.hf' in dirs:
            return os.path.join(path, 'data.hf')
    return None


def group_file_sizes(section_dir):
    return sorted(
        os.path.getsize(os.path.join(section_dir, name))
        for name in os.listdir(section_dir)
        if name.startswith('g') and not name.endswith('.tmp')
    )


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


def sort_field_of(key, field):
    """The value the engine sorts on, derived from the record's key.

    `record_data` is deterministic, so the expected ordering can be computed from
    the keys alone. That keeps the check independent of how a record is spelled on
    the wire, and means it verifies the order rather than re-deriving it from the
    very response it is meant to be checking.
    """
    i = int(key[len("REC"):])
    if field == "SEQ":
        return i  # numeric: 9 sorts before 10, not after it
    if field == "VAL1":
        return f"Val{i % 10}"
    return f"Data{i % 100}"


def expected_sorted_keys(count, specs):
    """The key order a correct sort must produce for `specs`.

    Built by stable sorts applied from the least significant spec to the most, over
    a list that starts in record-ID order - which is exactly how the engine breaks a
    tie once every sort key compares equal.
    """
    keys = sorted(f"REC{i}" for i in range(count))
    for field, descending in reversed(specs):
        keys.sort(key=lambda k: sort_field_of(k, field), reverse=descending)
    return keys


def sorted_query_stats(conn, query_string, iterations=QUERY_ITERATIONS):
    """Run a sorted query repeatedly and return (Stats, the keys it returned)."""
    stats, resp = harness.benchmark(
        lambda _: conn.request(
            command="QUERY", file=FILE, query_string=query_string, account=ACCOUNT
        ),
        iterations,
        warmup=1,
    )
    return stats, [pair[0] for pair in (resp.get("results") or [])]


def order_verdict(returned, expected):
    """(passed, detail) for a returned key order against the expected one."""
    if returned == expected:
        return True, f"{len(returned)} records in order"
    if len(returned) != len(expected):
        return False, f"{len(returned)} records returned, expected {len(expected)}"
    first = next(i for i, (a, b) in enumerate(zip(returned, expected)) if a != b)
    return False, f"order differs at position {first}: {returned[first]}, expected {expected[first]}"


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
                # More iterations than the scans above: this pair feeds the growth
                # ratio, and a p50 taken from a handful of samples is a noisy divisor.
                small_sorted, _ = sorted_query_stats(
                    conn, "BY SEQ", iterations=max(5, QUERY_ITERATIONS // 2)
                )

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

                # The headline guarantee of the hashfile format. Before it, the WRITE
                # handler rewrote the entire table on every request, so a write was
                # O(file size) and a bulk load O(n^2). Now a write rewrites a single
                # group, and this check fails if that ever regresses: the later phase
                # sees the file 5x larger yet must cost the same.
                suite.check_ratio(
                    "Write cost stays flat as the file grows",
                    late.p50 / early.p50 if early.p50 else 0.0,
                    WRITE_GROWTH * float(os.environ.get("SRP_LIMIT_WRITE_SLACK", "1.0")),
                    detail=f"p50 {early.p50:.2f}ms -> {late.p50:.2f}ms while the file "
                    f"grew {GROWTH:.0f}x",
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

                # Sorting had no end-to-end coverage at all before this: every query
                # above is unsorted, so a regression in the comparator - the cost that
                # `SortValue` exists to keep out of it - was invisible here.
                # `SEQ` is the numeric path, which is the one that historically parsed
                # each value again on every comparison.
                sorted_scan, sorted_keys = sorted_query_stats(
                    conn, "BY SEQ", iterations=max(5, QUERY_ITERATIONS // 2)
                )
                passed, detail = order_verdict(sorted_keys, expected_sorted_keys(NUM_RECORDS, [("SEQ", False)]))
                suite.measure(
                    "Sorted scan, numeric key (BY SEQ)",
                    sorted_scan,
                    budget_ms=BUDGET_SORTED_SCAN_MS,
                    passed=passed,
                    extra=detail,
                )

                text_sorted, text_keys = sorted_query_stats(
                    conn, "BY VAL2", iterations=max(3, QUERY_ITERATIONS // 4)
                )
                passed, detail = order_verdict(text_keys, expected_sorted_keys(NUM_RECORDS, [("VAL2", False)]))
                suite.measure(
                    "Sorted scan, text key with ties (BY VAL2)",
                    text_sorted,
                    budget_ms=BUDGET_SORTED_SCAN_MS,
                    passed=passed,
                    extra=detail,
                )

                # Text primary with heavy ties, numeric secondary descending: the shape
                # that exercises every branch of the comparison in one query.
                compound_sorted, compound_keys = sorted_query_stats(
                    conn, "BY VAL1 BY.DSND SEQ", iterations=max(3, QUERY_ITERATIONS // 4)
                )
                passed, detail = order_verdict(
                    compound_keys,
                    expected_sorted_keys(NUM_RECORDS, [("VAL1", False), ("SEQ", True)]),
                )
                suite.measure(
                    "Sorted scan, compound (BY VAL1 BY.DSND SEQ)",
                    compound_sorted,
                    budget_ms=BUDGET_SORTED_SCAN_MS,
                    passed=passed,
                    extra=detail,
                )

                # The regression signal for the sort itself: 4x the records must cost
                # about 4.7x (n log n), not 16x. Host independent, like the scan ratio.
                suite.check_ratio(
                    "Sort cost grows no worse than n log n",
                    sorted_scan.p50 / small_sorted.p50 if small_sorted.p50 else 0.0,
                    SORT_GROWTH,
                    detail=f"{small_count} -> {len(sorted_keys)} records, "
                    f"p50 {small_sorted.p50:.2f}ms -> {sorted_scan.p50:.2f}ms",
                )

                # And what ordering adds to the same scan. Coarse - serialising the
                # records dominates both sides - but it fails outright if sorting ever
                # becomes the dominant cost of a query again.
                suite.check_ratio(
                    "Sorting adds a bounded share of a scan's cost",
                    compound_sorted.p50 / full_scan.p50 if full_scan.p50 else 0.0,
                    SORT_OVERHEAD,
                    detail=f"unsorted p50 {full_scan.p50:.2f}ms -> compound-sorted "
                    f"p50 {compound_sorted.p50:.2f}ms",
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

                # Exploding multivalues. The work is bounded by values rather
                # than by records, which is the one shape none of the
                # measurements above can see.
                print(f"Writing {MV_RECORDS} multivalued records...")
                for i in range(MV_RECORDS):
                    conn.request(
                        command="WRITE",
                        file=MV_FILE,
                        key=f"MV{i}",
                        data=mv_record_data(i),
                        account=ACCOUNT,
                    )

                selective_hits = (MV_RECORDS // 100) * MV_VALUES
                stats, resp = harness.benchmark(
                    lambda _i: conn.request(
                        command="QUERY",
                        file=MV_FILE,
                        query_string="WITH TAGS = Tag42",
                        explode=["TAGS"],
                        account=ACCOUNT,
                    ),
                    QUERY_ITERATIONS,
                    warmup=1,
                )
                rows = len(resp.get("results") or [])
                positions = resp.get("positions") or []
                explode_selective = stats
                suite.measure(
                    "QUERY exploding a matched value",
                    stats,
                    budget_ms=BUDGET_EXPLODE_MS,
                    # A fast answer that lost the positions is not the answer.
                    passed=rows == selective_hits and len(positions) == rows,
                    extra=f"{rows} row(s), {len(positions)} position(s)",
                )

                stats, resp = harness.benchmark(
                    lambda _i: conn.request(
                        command="QUERY",
                        file=MV_FILE,
                        query_string="BY.EXP TAGS",
                        account=ACCOUNT,
                    ),
                    max(3, QUERY_ITERATIONS // 4),
                    warmup=1,
                )
                bare_rows = len(resp.get("results") or [])
                suite.measure(
                    "QUERY exploding every value",
                    stats,
                    budget_ms=BUDGET_EXPLODE_BARE_MS,
                    passed=bare_rows == MV_RECORDS * MV_VALUES,
                    extra=f"{bare_rows} row(s) from {MV_RECORDS} records",
                )

                # Time ratio against row ratio: returning a hundred times the
                # rows must not cost more than a hundred times as much.
                row_growth = bare_rows / selective_hits if selective_hits else 0.0
                time_growth = stats.p50 / explode_selective.p50 if explode_selective.p50 else 0.0
                suite.check_ratio(
                    "Exploding costs no more than the rows it returns",
                    time_growth / row_growth if row_growth else 0.0,
                    EXPLODE_ROW_GROWTH,
                    detail=f"{selective_hits} rows p50 {explode_selective.p50:.2f}ms -> "
                    f"{bare_rows} rows p50 {stats.p50:.2f}ms "
                    f"({time_growth:.1f}x time for {row_growth:.0f}x rows)",
                )

                stats, resp = harness.benchmark(
                    lambda i: conn.request(
                        command="SELECT",
                        file=MV_FILE,
                        query_string="WITH TAGS = Tag42",
                        explode=["TAGS"],
                        list_name=f"MVLIST{i}",
                        account=ACCOUNT,
                    ),
                    QUERY_ITERATIONS,
                    warmup=1,
                )
                suite.measure(
                    "SELECT into an exploded list",
                    stats,
                    budget_ms=BUDGET_EXPLODE_MS,
                    passed=resp.get("count") == selective_hits,
                    extra=f"{resp.get('count')} row(s)",
                )

            # Write amplification, measured directly on disk rather than inferred from
            # timings: a write rewrites one group, so the largest group is the real
            # per-write I/O cost and must stay a small fraction of the whole table.
            # Writes are batched, and the connection was only just closed, so give
            # the server a moment to finish its final flush before reading the files.
            time.sleep(0.5)
            section_dir = find_section_dir(workspace.path, FILE)
            if section_dir:
                sizes = group_file_sizes(section_dir)
                total = sum(sizes)
                largest = sizes[-1] if sizes else 0
                share = largest / total if total else 1.0
                suite.record(
                    'Hashfile layout',
                    {
                        'groups': len(sizes),
                        'total_bytes': total,
                        'largest_group_bytes': largest,
                        'largest_group_share': round(share, 5),
                        'records_per_group': round(NUM_RECORDS / len(sizes), 2) if sizes else 0,
                    },
                )
                suite.check(
                    'A write rewrites a small fraction of the file',
                    share <= float(os.environ.get('SRP_LIMIT_GROUP_SHARE', '0.05')),
                    f'{len(sizes)} groups, {NUM_RECORDS / max(len(sizes), 1):.1f} records each; '
                    f'largest group {largest}B of {total}B ({share * 100:.2f}%)',
                )
            else:
                suite.check(
                    'A write rewrites a small fraction of the file',
                    False,
                    'no hashed section found on disk',
                )

            elapsed = time.perf_counter() - started
            monitor.stop()
            if monitor.available:
                growth_kb = max(monitor.peak_rss_kb - (monitor.first_rss_kb or 0), 0)
                bytes_per_record = growth_kb * 1024.0 / TOTAL_RECORDS
                suite.record("Server resources", dict(monitor.as_dict(), wall_seconds=round(elapsed, 2)))
                suite.check(
                    "Resident memory per record",
                    bytes_per_record <= BUDGET_BYTES_PER_RECORD * harness.BUDGET_SCALE
                    or not harness.ENFORCE_BUDGETS,
                    f"{bytes_per_record:.0f} B/record over {TOTAL_RECORDS} records "
                    f"(budget {BUDGET_BYTES_PER_RECORD * harness.BUDGET_SCALE:.0f}); "
                    f"{monitor.summary()}",
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
