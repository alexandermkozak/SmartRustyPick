"""Behaviour and cost of the server under concurrent clients.

The server serialises every request behind a single global `Mutex<Database>`, so this
suite deliberately does *not* assert linear scaling - that would fail by design. What
it does assert is the properties that matter for a lock-serialised server:

* correctness under contention - no lost updates, no cross-talk between connections;
* no throughput collapse - N clients must still get at least a fraction of the
  single-client throughput, which is what convoy effects and lock thrashing destroy;
* bounded tail latency - the p99 a client sees must stay within a fair-queueing
  multiple of the single-client p95;
* bounded per-connection cost - mutual-TLS handshakes and idle connections must not
  cost unreasonable memory, so a connection leak shows up here.

The observed scaling factor is recorded as a metric on every run, so if the global
lock is ever replaced by finer-grained locking the improvement is visible immediately.
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

                stats, _ = harness.benchmark(
                    lambda _: make_client().close(), HANDSHAKES, warmup=1
                )
                suite.measure(
                    f"Mutual-TLS connection setup ({HANDSHAKES} handshakes)",
                    stats,
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
