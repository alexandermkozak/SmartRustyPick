"""Behaviour and cost of the server under concurrent clients.

Reads run under a shared lock, writers lock only the file they name, and the blocking
engine runs off the async runtime, so both readers and writers to different files
scale with the client count. That is why this suite asserts properties rather than a
fixed speedup:

* correctness under contention - no lost updates, no cross-talk between connections;
* exclusive queue claims - N consumers draining one queue file must between them receive
  every record exactly once, which is the property a queue exists to provide;
* per-file locking - writers to *different* files must beat a single writer, and must
  see a shorter tail than the same writers all queueing on one file;
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
QUEUE = "CONC_Q"
# An ordinary file the queue's statistics are measured against, so the group
# trailers both of them read are on both sides of the comparison.
PLAIN = "CONC_P"
SEED_RECORDS = int(os.environ.get("SRP_CONC_RECORDS", "1000"))
CLIENTS = int(os.environ.get("SRP_CONC_CLIENTS", "8"))
OPS_PER_CLIENT = int(os.environ.get("SRP_CONC_OPS", "200"))
HANDSHAKES = int(os.environ.get("SRP_CONC_HANDSHAKES", "20"))
# Records pushed through the queue by the concurrent consumers. Enough that the
# consumers overlap on the file many times over rather than each taking a turn.
QUEUE_RECORDS = int(os.environ.get("SRP_CONC_QUEUE_RECORDS", "600"))
# `FILE.STATS` on a queue sweeps the lapsed claims and reads the depth, in-flight
# count and oldest age, and it does that under the file's own write lock - the
# lock every consumer of that queue is waiting on. So being a queue must not add
# a cost that grows with the backlog, or a dashboard polling a deep queue would
# stall the consumers draining it.
#
# The comparison is a queue against an *ordinary* file holding the same records,
# rather than a shallow queue against a deep one. `FILE.STATS` reads one group
# trailer per group whatever the file is, so it grows with the group count on its
# own; measuring a queue against itself at two depths would report that growth
# and say nothing about the queue. Against a plain file of the same size, that
# shared cost is on both sides and what is left is the queue's own.
QUEUE_STATS_RECORDS = int(os.environ.get("SRP_CONC_QUEUE_STATS_RECORDS", "8000"))
QUEUE_STATS_OVERHEAD = float(os.environ.get("SRP_CONC_QUEUE_STATS_OVERHEAD", "1.6"))

# Fraction of the single-client throughput that N clients must still achieve in
# aggregate. Serialised execution alone costs nothing here; only pathological
# contention (convoying, repeated lock hand-off, per-request rebuilds) does.
MIN_THROUGHPUT_RATIO = float(os.environ.get("SRP_CONC_MIN_THROUGHPUT_RATIO", "0.5"))
# Writers on distinct files hold distinct locks, so N of them must get more work done
# than one. Above 1x rather than near N: the ceiling here is the test client, not the
# server - N Python threads share a GIL and a machine with fewer cores than clients,
# which is also why the read-scaling number above is nowhere near N.
MIN_WRITE_SCALING = float(os.environ.get("SRP_CONC_MIN_WRITE_SCALING", "1.25"))
# The same N writers, on N files versus all on one. Same clients, same requests and
# the same work for the server: the only difference is whether the writes queue for
# one lock, which isolates the lock granularity itself. Compared as tail latency
# rather than throughput, because aggregate throughput here is capped by the test
# client while what a shared lock does is make a request wait its turn.
MIN_WRITE_SPREAD = float(os.environ.get("SRP_CONC_MIN_WRITE_SPREAD", "1.15"))
# ...but only asserted with enough writers to make them collide. Below this, each
# client largely gets a core to itself, requests rarely overlap on the file at all,
# and the ratio measures scheduling noise rather than the lock: at four writers it
# sits around 1.1x whether the lock is per file or database-wide. The ratio is
# always recorded; a run with fewer writers than this reports it without asserting
# on it, and leans on the scaling check above, which does hold at four.
SPREAD_MIN_CLIENTS = int(os.environ.get("SRP_CONC_SPREAD_MIN_CLIENTS", "8"))
# Writes per connection in the distinct-file comparison. Long enough to span several
# flush intervals - a run shorter than that measures whichever flush happened to land
# inside it - and short enough to stay a quick check.
DISTINCT_WRITES = int(os.environ.get("SRP_CONC_DISTINCT_WRITES", str(max(100, OPS_PER_CLIENT * 2))))
# Unmeasured writes that create and load a file before the baseline is timed.
WRITE_WARMUP = 20
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
    f"CREATE.FILE {FILE}",
    f"CREATE.FILE {QUEUE} QUEUE TIMEOUT 300",
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


def run_parallel(make_client, clients, ops, op=None, op_for_slot=None):
    """Drive `clients` connections in parallel and return (Stats, failures, wall time).

    `op_for_slot` gives each connection an operation of its own, which is how the
    distinct-file check points every writer at a different file; `op` is the shorthand
    for the common case where they all do the same thing.
    """
    if op_for_slot is None:
        def op_for_slot(_slot):
            return op

    samples = [None] * clients
    failures = [None] * clients
    errors = []

    def worker(slot):
        try:
            with make_client() as conn:
                samples[slot], failures[slot] = run_workload(
                    conn, ops, op_for_slot(slot), offset=slot * ops
                )
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


def drain_queue_concurrently(make_client, clients, expected):
    """Drain one queue with `clients` consumers and report exactly what each received.

    The whole point of a queue file is that this cannot double-deliver, so the check
    is a total across the consumers rather than a per-client one: every key handed
    out, counted, with duplicates kept rather than collapsed into a set. A record
    delivered twice shows up as a duplicate here even if both consumers succeed.

    Consumers acknowledge what they take, so nothing is redelivered by a lapsed
    claim - the queue is created with a 300 second visibility timeout, far longer
    than the run, so a redelivery here would mean a broken claim and not a slow test.
    """
    delivered = [None] * clients
    errors = []
    lock = threading.Lock()
    drained = {"count": 0}

    def worker(slot):
        keys = []
        try:
            with make_client() as conn:
                while True:
                    with lock:
                        if drained["count"] >= expected:
                            break
                    resp = conn.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT)
                    status = resp.get("status")
                    if status == "EMPTY":
                        # Another consumer may still be holding one it has not yet
                        # acknowledged, so an empty read is not the end of the queue.
                        with lock:
                            if drained["count"] >= expected:
                                break
                        continue
                    if status != "OK":
                        errors.append(f"client {slot}: DEQUEUE said {resp.get('message')}")
                        break
                    key = resp["claim"]["key"]
                    keys.append(key)
                    ack = conn.request(command="ACK", file=QUEUE, key=key, account=ACCOUNT)
                    if ack.get("status") != "OK":
                        errors.append(f"client {slot}: ACK said {ack.get('message')}")
                        break
                    with lock:
                        drained["count"] += 1
        except Exception as exc:  # noqa: BLE001 - surfaced as a suite failure
            errors.append(f"client {slot}: {exc}")
        delivered[slot] = keys

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(clients)]
    start = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    wall = time.perf_counter() - start

    every_key = [key for group in delivered for key in (group or [])]
    return every_key, [len(group or []) for group in delivered], wall, errors


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
                harness.wait_for_seed(
                    seeder,
                    lambda resp: sorted(resp.get("keys") or []) == ["SEQ", "VAL1"],
                    process=cli,
                    command="LIST.DICT",
                    file=FILE,
                    account=ACCOUNT,
                )

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

            # Writers on files of their own: with a lock per file rather than one
            # for the whole database, they proceed in parallel instead of taking
            # turns, and N connections get more done than one. The tail-latency
            # comparison below is the sharper of the two signals - this ratio is
            # measured against a baseline of one connection, and a single Python
            # client is fast enough to blur it.
            distinct_files = [f"{FILE}_W{slot}" for slot in range(CLIENTS)]
            shared_file = f"{FILE}_SHARED"
            baseline_file = f"{FILE}_BASE"
            with make_client() as builder:
                file_errors = [
                    builder.request(command="CREATE.FILE", file=name, account=ACCOUNT).get("message")
                    for name in distinct_files + [shared_file, baseline_file]
                ]
                file_errors = [message for message in file_errors if message]

            def write_to(file_name):
                def op(conn, index):
                    return conn.request(
                        command="WRITE",
                        file=file_name,
                        key=f"D{index}",
                        data=f"Distinct^{index}",
                        account=ACCOUNT,
                    )
                return op

            with make_client() as lone_writer:
                # The first writes to a fresh file create and load it. Timing them
                # would put a one-off cost into the baseline that every client of
                # the parallel run gets to spread over its whole workload.
                run_workload(lone_writer, WRITE_WARMUP, write_to(baseline_file))
                lone_samples, lone_failures = run_workload(
                    lone_writer, DISTINCT_WRITES, write_to(baseline_file), offset=WRITE_WARMUP
                )
            lone = harness.Stats(lone_samples)
            suite.measure(
                f"Single writer, {DISTINCT_WRITES} writes to one file",
                lone,
                passed=not (lone_failures or file_errors),
                extra="baseline for the per-file locking check"
                if not file_errors
                else f"{len(file_errors)} setup failures ({file_errors[0]})",
            )

            spread, problems, wall = run_parallel(
                make_client,
                CLIENTS,
                DISTINCT_WRITES,
                op_for_slot=lambda slot: write_to(distinct_files[slot]),
            )
            spread_aggregate = (CLIENTS * DISTINCT_WRITES) / wall if wall else 0.0
            suite.measure(
                f"{CLIENTS} concurrent writers, one file each, {DISTINCT_WRITES} writes",
                spread,
                passed=not problems,
                extra=f"{spread_aggregate:.0f} ops/s aggregate over {wall:.2f}s"
                if not problems
                else f"{len(problems)} failures ({problems[0]})",
            )

            # The same writers again, all on one file. Everything else is held
            # constant, so what separates this from the run above is only whether
            # the writes queue behind a single lock.
            shared, shared_problems, shared_wall = run_parallel(
                make_client, CLIENTS, DISTINCT_WRITES, write_to(shared_file)
            )
            shared_aggregate = (CLIENTS * DISTINCT_WRITES) / shared_wall if shared_wall else 0.0
            suite.measure(
                f"{CLIENTS} concurrent writers, all on one file, {DISTINCT_WRITES} writes",
                shared,
                passed=not shared_problems,
                extra=f"{shared_aggregate:.0f} ops/s aggregate over {shared_wall:.2f}s"
                if not shared_problems
                else f"{len(shared_problems)} failures ({shared_problems[0]})",
            )

            write_scaling = spread_aggregate / lone.ops_per_second if lone.ops_per_second else 0.0
            suite.record(
                "Concurrent write scaling, distinct files",
                {
                    "clients": CLIENTS,
                    "single_ops_per_second": round(lone.ops_per_second, 2),
                    "aggregate_ops_per_second": round(spread_aggregate, 2),
                    "scaling": round(write_scaling, 3),
                    "minimum": MIN_WRITE_SCALING,
                },
            )
            suite.check(
                "Writers to different files scale with the client count",
                write_scaling >= MIN_WRITE_SCALING or not harness.ENFORCE_BUDGETS,
                f"{CLIENTS} writers on {CLIENTS} files reach {write_scaling:.2f}x one writer's "
                f"throughput ({spread_aggregate:.0f} vs {lone.ops_per_second:.0f} ops/s, "
                f"minimum {MIN_WRITE_SCALING:.2f}x)",
            )

            spread_ratio = shared.p95 / spread.p95 if spread.p95 else 0.0
            suite.record(
                "Concurrent write spread, distinct files against one",
                {
                    "clients": CLIENTS,
                    "one_file_p95_ms": round(shared.p95, 3),
                    "distinct_files_p95_ms": round(spread.p95, 3),
                    "one_file_ops_per_second": round(shared_aggregate, 2),
                    "distinct_files_ops_per_second": round(spread_aggregate, 2),
                    "spread": round(spread_ratio, 3),
                    "minimum": MIN_WRITE_SPREAD,
                },
            )
            spread_detail = (
                f"{CLIENTS} writers sharing one file see a p95 of {shared.p95:.2f}ms against "
                f"{spread.p95:.2f}ms on {CLIENTS} files; {spread_ratio:.2f}x"
            )
            if CLIENTS >= SPREAD_MIN_CLIENTS:
                suite.check(
                    "A writer waits for the file it writes, not for the database",
                    spread_ratio >= MIN_WRITE_SPREAD or not harness.ENFORCE_BUDGETS,
                    f"{spread_detail} (minimum {MIN_WRITE_SPREAD:.2f}x)",
                )
            else:
                print(
                    f"  [Skipped] A writer waits for the file it writes, not for the database: "
                    f"{spread_detail}, not asserted below {SPREAD_MIN_CLIENTS} writers"
                )

            with make_client() as verifier:
                # A full scan rather than a criterion: these files were created
                # bare, and a selection would need a dictionary they have not got.
                landed = 0
                for name in distinct_files:
                    resp = verifier.request(command="QUERY", file=name, account=ACCOUNT)
                    landed += len(resp.get("results") or [])
                suite.check_eq(
                    "Every write to every file lands",
                    landed,
                    CLIENTS * DISTINCT_WRITES,
                )

            # N consumers against one queue: between them they must receive every
            # record exactly once. This is the acceptance property of a queue
            # file, and the only place in the suite where two clients are
            # deliberately racing for the *same* record rather than for the same
            # lock on different ones.
            with make_client() as producer:
                enqueue_failures = [
                    resp.get("message")
                    for resp in (
                        producer.request(
                            command="ENQUEUE",
                            file=QUEUE,
                            data=f"job^{index}",
                            account=ACCOUNT,
                        )
                        for index in range(QUEUE_RECORDS)
                    )
                    if resp.get("status") != "OK"
                ]
            suite.check(
                f"Every one of {QUEUE_RECORDS} records enqueues",
                not enqueue_failures,
                "as expected" if not enqueue_failures else f"{len(enqueue_failures)} failed ({enqueue_failures[0]})",
            )

            every_key, per_client, queue_wall, queue_errors = drain_queue_concurrently(
                make_client, CLIENTS, QUEUE_RECORDS
            )
            duplicates = len(every_key) - len(set(every_key))
            suite.record(
                "Queue drain",
                {
                    "clients": CLIENTS,
                    "records": QUEUE_RECORDS,
                    "delivered": len(every_key),
                    "distinct": len(set(every_key)),
                    "per_client": per_client,
                    "seconds": round(queue_wall, 3),
                    "records_per_second": round(QUEUE_RECORDS / queue_wall, 2) if queue_wall else 0.0,
                },
            )
            suite.check(
                f"{CLIENTS} consumers never receive the same record twice",
                not queue_errors and duplicates == 0 and len(every_key) == QUEUE_RECORDS,
                f"{len(every_key)} deliveries of {QUEUE_RECORDS} records, {duplicates} duplicated, "
                f"split {per_client} across {CLIENTS} consumers in {queue_wall:.2f}s"
                if not queue_errors
                else f"{len(queue_errors)} failures ({queue_errors[0]})",
            )
            # Every consumer has to have done some of the work: one client that
            # took all of it would satisfy the count above while proving nothing
            # about concurrent claims.
            suite.check(
                "Every consumer gets a share of the queue",
                bool(per_client) and min(per_client) > 0,
                f"smallest share {min(per_client) if per_client else 0} records of {QUEUE_RECORDS}",
            )
            with make_client() as checker:
                stats = checker.request(command="FILE.STATS", file=QUEUE, account=ACCOUNT)
                queue_stats = (stats.get("record") or {}).get("queue") or {}
            suite.check(
                "A fully drained queue reports nothing left",
                queue_stats.get("depth") == 0
                and queue_stats.get("in_flight") == 0
                and queue_stats.get("dead_letters") == 0,
                f"depth {queue_stats.get('depth')}, in flight {queue_stats.get('in_flight')}, "
                f"dead-lettered {queue_stats.get('dead_letters')}",
            )

            # What being a queue adds to a file's statistics, at a depth where a
            # scan of the backlog would be plainly visible.
            with make_client() as producer:
                producer.request(command="CREATE.FILE", file=PLAIN, account=ACCOUNT)
                for index in range(QUEUE_STATS_RECORDS):
                    producer.request(
                        command="ENQUEUE", file=QUEUE, data=f"depth^{index}", account=ACCOUNT
                    )
                    producer.request(
                        command="WRITE",
                        file=PLAIN,
                        key=f"D{index}",
                        data=f"depth^{index}",
                        account=ACCOUNT,
                    )

                def stats_cost(file):
                    return harness.benchmark(
                        lambda _: producer.request(
                            command="FILE.STATS", file=file, account=ACCOUNT
                        ),
                        30,
                        warmup=3,
                    )

                plain_stats, _ = stats_cost(PLAIN)
                queue_stats_timing, last = stats_cost(QUEUE)

            reported = ((last.get("record") or {}).get("queue") or {}).get("depth")
            suite.check_eq(
                "FILE.STATS reports the depth the queue was filled to",
                reported,
                QUEUE_STATS_RECORDS,
            )
            suite.check_ratio(
                "A queue's statistics cost no more than an ordinary file's",
                queue_stats_timing.p50 / plain_stats.p50 if plain_stats.p50 else 0.0,
                QUEUE_STATS_OVERHEAD,
                detail=f"{QUEUE_STATS_RECORDS} records, plain p50 {plain_stats.p50:.2f}ms "
                f"vs queue p50 {queue_stats_timing.p50:.2f}ms",
            )

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
