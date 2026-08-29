"""Behaviour and cost of the server under concurrent clients.

Reads run under a shared lock and the blocking engine runs off the async runtime, so
readers scale with the client count while writers still take the database exclusively.
That mix is why this suite asserts properties rather than a fixed speedup:

* correctness under contention - no lost updates, no cross-talk between connections;
* no throughput collapse - N clients must still get at least a fraction of the
  single-client throughput, which is what convoy effects and lock thrashing destroy;
* bounded tail latency - the p99 a client sees must stay within a fair-queueing
  multiple of the single-client p95;
* connection setup is independent of database work - a client hammering the disk must
  not delay the handshakes of unrelated connections;
* bounded per-connection cost - mutual-TLS handshakes and idle connections must not
  cost unreasonable memory, so a connection leak shows up here.

The observed scaling factor is recorded as a metric on every run, so a regression in
the locking granularity is visible immediately.
"""

import os
import sys
import threading
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "CONC_ACC"
FILE = "CONC"
SEED_RECORDS = int(os.environ.get("SRP_CONC_RECORDS", "1000"))
CLIENTS = int(os.environ.get("SRP_CONC_CLIENTS", "8"))
OPS_PER_CLIENT = int(os.environ.get("SRP_CONC_OPS", "200"))
HANDSHAKES = int(os.environ.get("SRP_CONC_HANDSHAKES", "20"))

# Fraction of the single-client throughput that N clients must still achieve in
# aggregate. Serialised execution alone costs nothing here; only pathological
# contention (convoying, repeated lock hand-off, per-request rebuilds) does.
MIN_THROUGHPUT_RATIO = float(os.environ.get("SRP_CONC_MIN_THROUGHPUT_RATIO", "0.5"))
# Under perfect fair queueing a client waits for the other N-1, so its tail latency
# grows about N-fold. Anything beyond this multiple means unfair or degrading queueing.
TAIL_LATENCY_SLACK = float(os.environ.get("SRP_CONC_TAIL_SLACK", "3"))
# A handshake shares nothing with the database, so a flush may cost it scheduling
# noise but not a multiple of its idle cost. The comparison is worst case against
# worst case: a blocking flush delays the one handshake it overlaps, which is an
# outlier the percentiles would hide.
HANDSHAKE_LOAD_SLACK = float(os.environ.get("SRP_CONC_HANDSHAKE_LOAD_SLACK", "3"))
# A buffered burst large enough that writing it out is measurable disk work rather
# than a memcpy: the point is to have one long flush in flight, not to load the CPU.
BULK_PAYLOAD = "X" * int(os.environ.get("SRP_CONC_PAYLOAD_BYTES", "4096"))
BULK_RECORDS = int(os.environ.get("SRP_CONC_BULK_RECORDS", "2000"))
BUDGET_HANDSHAKE_MS = float(os.environ.get("SRP_BUDGET_HANDSHAKE_MS", "150"))
BUDGET_KB_PER_CONNECTION = float(os.environ.get("SRP_BUDGET_KB_PER_CONNECTION", "1024"))

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "Y",  # answer the "DIR file missing. Create and populate?" prompt
    f"CREATE.FILE {FILE}",
    f"SET DICT {FILE} VAL1 1",
    f"SET DICT {FILE} SEQ 2",
    "SAVE",
]


def read_op(conn, index):
    return conn.request(
        command="READ", file=FILE, key=f"REC{index % SEED_RECORDS}", account=ACCOUNT
    )


def run_workload(conn, ops, op, offset=0):
    """Run `ops` operations on one connection, returning (samples, failures)."""
    samples = []
    failures = []
    for i in range(ops):
        start = time.perf_counter()
        resp = op(conn, offset + i)
        samples.append(time.perf_counter() - start)
        if resp.get("status") != "OK":
            failures.append(resp.get("message"))
    return samples, failures


def run_parallel(make_client, clients, ops, op):
    """Drive `clients` connections in parallel and return (Stats, failures, wall time)."""
    samples = [None] * clients
    failures = [None] * clients
    errors = []

    def worker(slot):
        try:
            with make_client() as conn:
                samples[slot], failures[slot] = run_workload(conn, ops, op, offset=slot * ops)
        except Exception as exc:  # noqa: BLE001 - surfaced as a suite failure
            errors.append(f"client {slot}: {exc}")
            samples[slot], failures[slot] = [], []

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(clients)]
    start = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    wall = time.perf_counter() - start

    merged = [s for group in samples for s in (group or [])]
    problems = errors + [f for group in failures for f in (group or [])]
    return harness.Stats(merged), problems, wall


def handshakes_during_slow_flush(make_client, handshake_stats):
    """Measure handshake latency while the server writes out a large buffered burst.

    The engine is synchronous and file backed, so a flush must not run on an async
    worker thread: it would block every other task scheduled there, and an unrelated
    client would wait for the disk before its TLS handshake even starts. A burst is
    buffered and then released in one go, which puts a single long flush in flight
    without loading the CPU, so what is measured is blocking and not contention.
    """
    errors = []
    try:
        conn = make_client()
        for index in range(BULK_RECORDS):
            conn.request(
                command="WRITE",
                file=FILE,
                key=f"BULK{index}",
                data=f"{BULK_PAYLOAD}^{index}",
                account=ACCOUNT,
            )
        # Disconnecting makes the server persist the burst immediately instead of
        # waiting for the flush ticker.
        conn.close()
    except Exception as exc:  # noqa: BLE001 - surfaced as a suite failure
        errors.append(str(exc))

    # No warmup: the flush is in flight now and a warmup handshake would spend it.
    loaded, _ = harness.benchmark(lambda _: make_client().close(), HANDSHAKES)
    ratio = loaded.max / handshake_stats.max if handshake_stats.max else 0.0
    return loaded, ratio, errors


def main():
    suite = harness.Suite(
        "Concurrency",
        "performance_results.md",
        title="Performance Test Results",
        detail_header="Measurement",
        metrics_file="performance_metrics.json",
    )
    harness.require_binaries(harness.CLI_BIN)

    with harness.Workspace("concurrency") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        port = harness.free_port()
        harness.write_config(port, certs)

        def make_client():
            return harness.Client(port, client_crt, client_key, certs.ca_crt)

        cli = harness.start_cli(["--account", "SYSTEM"])
        monitor = harness.ResourceMonitor(cli.pid)
        monitor.start()
        try:
            cli.stdin.write(f"AUTHORIZE.CONN {thumbprint} conc_client ADMIN\n")
            for command in SETUP_COMMANDS:
                cli.stdin.write(command + "\n")
            cli.stdin.flush()

            seeder = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=cli)
            with seeder:
                print(f"Seeding {SEED_RECORDS} records...")
                for i in range(SEED_RECORDS):
                    seeder.request(
                        command="WRITE",
                        file=FILE,
                        key=f"REC{i}",
                        data=f"Val{i % 10}^{i}",
                        account=ACCOUNT,
                    )

                handshake_stats, _ = harness.benchmark(
                    lambda _: make_client().close(), HANDSHAKES, warmup=1
                )
                suite.measure(
                    f"Mutual-TLS connection setup ({HANDSHAKES} handshakes)",
                    handshake_stats,
                    budget_ms=BUDGET_HANDSHAKE_MS,
                )

                single, single_failures = run_workload(seeder, OPS_PER_CLIENT, read_op)
                single = harness.Stats(single)
                suite.measure(
                    f"Single-client reads ({OPS_PER_CLIENT} ops)",
                    single,
                    passed=not single_failures,
                    extra="baseline for the scaling checks",
                )

            parallel, problems, wall = run_parallel(make_client, CLIENTS, OPS_PER_CLIENT, read_op)
            total_ops = CLIENTS * OPS_PER_CLIENT
            aggregate = total_ops / wall if wall else 0.0
            suite.measure(
                f"{CLIENTS} concurrent clients, {OPS_PER_CLIENT} reads each",
                parallel,
                passed=not problems,
                extra=f"{aggregate:.0f} ops/s aggregate over {wall:.2f}s"
                if not problems
                else f"{len(problems)} failures ({problems[0]})",
            )

            ratio = aggregate / single.ops_per_second if single.ops_per_second else 0.0
            suite.record(
                "Concurrent throughput scaling",
                {
                    "clients": CLIENTS,
                    "single_ops_per_second": round(single.ops_per_second, 2),
                    "aggregate_ops_per_second": round(aggregate, 2),
                    "scaling": round(ratio, 3),
                    "minimum": MIN_THROUGHPUT_RATIO,
                },
            )
            suite.check(
                "Throughput does not collapse under contention",
                ratio >= MIN_THROUGHPUT_RATIO,
                f"{CLIENTS} clients reach {ratio:.2f}x the single-client throughput "
                f"({aggregate:.0f} vs {single.ops_per_second:.0f} ops/s, "
                f"minimum {MIN_THROUGHPUT_RATIO:.2f}x)",
            )
            suite.check_ratio(
                "Tail latency degrades no worse than fair queueing",
                parallel.p99 / single.p95 if single.p95 else 0.0,
                CLIENTS * TAIL_LATENCY_SLACK,
                detail=f"p99 {parallel.p99:.2f}ms under {CLIENTS} clients "
                f"vs p95 {single.p95:.2f}ms alone",
            )

            # Concurrent writers to disjoint keys: the lock must make every write land.
            def write_op(conn, index):
                return conn.request(
                    command="WRITE",
                    file=FILE,
                    key=f"W{index}",
                    data=f"Written^{index}",
                    account=ACCOUNT,
                )

            writes_per_client = max(1, OPS_PER_CLIENT // 10)
            written, problems, wall = run_parallel(
                make_client, CLIENTS, writes_per_client, write_op
            )
            expected = CLIENTS * writes_per_client
            suite.measure(
                f"{CLIENTS} concurrent writers, {writes_per_client} writes each",
                written,
                passed=not problems,
                extra=f"{expected / wall:.0f} ops/s aggregate"
                if not problems
                else f"{len(problems)} failures ({problems[0]})",
            )

            with make_client() as verifier:
                resp = verifier.request(
                    command="QUERY", file=FILE, query_string="WITH VAL1 = Written", account=ACCOUNT
                )
                found = len(resp.get("results") or [])
                suite.check_eq("No writes are lost under contention", found, expected)

            loaded, handshake_ratio, load_errors = handshakes_during_slow_flush(
                make_client, handshake_stats
            )
            suite.measure(
                f"Mutual-TLS connection setup during a flush ({HANDSHAKES} handshakes)",
                loaded,
                passed=not load_errors,
                extra=f"while {BULK_RECORDS} buffered records are written out"
                if not load_errors
                else f"{len(load_errors)} failures ({load_errors[0]})",
            )
            suite.check_ratio(
                "A slow flush does not delay the handshakes of other connections",
                handshake_ratio,
                HANDSHAKE_LOAD_SLACK,
                detail=f"slowest handshake {loaded.max:.2f}ms during the flush "
                f"vs {handshake_stats.max:.2f}ms idle",
            )

            monitor.stop()
            if monitor.available:
                suite.record("Server resources", monitor.as_dict())
                growth_kb = max(monitor.peak_rss_kb - monitor.last_rss_kb, 0)
                suite.check(
                    "Connections are released without leaking memory",
                    growth_kb <= BUDGET_KB_PER_CONNECTION * CLIENTS * harness.BUDGET_SCALE
                    or not harness.ENFORCE_BUDGETS,
                    f"{growth_kb}KB above the final RSS at peak for {CLIENTS} connections; "
                    f"{monitor.summary()}",
                )
            else:
                print("  [Skipped] resource monitoring is unavailable on this platform")
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Concurrency suite", exc)
        finally:
            monitor.stop()
            output = harness.stop(cli)
            if suite.failures:
                print("--- CLI output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
