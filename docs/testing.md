# Testing

The project has four layers of tests, all runnable from the `Makefile` and all executed by the
`Build and Test` GitHub workflow on every push to `main` and every pull request.

| Layer       | Command                 | What it covers                                                                                          |
|-------------|-------------------------|---------------------------------------------------------------------------------------------------------|
| Unit        | `make test-unit`        | `cargo test --workspace` — the engine, query parser, dictionaries and the request handler.              |
| Integration | `make test-integration` | The real binaries over the TLS protocol: CRUD, queries, select lists, headless mode and access control. |
| Performance | `make test-performance` | End-to-end latency distributions, throughput, scaling ratios, concurrency and resource usage.           |
| Benchmarks  | `make bench`            | Criterion micro-benchmarks of the engine: record codec, query execution, sorting, persistence.          |

`make test-all` runs the first three. Everything below the unit layer requires `cargo build` first; the Make targets
take care of it.

## Requirements

- A Rust toolchain (for `cargo build` / `cargo test`).
- `python3` (standard library only — no packages to install).
- `openssl` on the `PATH`, used to mint throwaway certificates.

## How the Python suites work

The suites live in `test/` and share `test/harness.py`:

- **Isolation.** Each suite runs inside its own temporary directory. Because both binaries resolve
  `config.toml` and the storage directory relative to the working directory, this keeps a run from ever touching your
  real `db_storage/` or `config.toml`. Nothing is left behind in the repository.
- **Ports.** Every suite asks the OS for a free port instead of hardcoding one, so runs never collide with each other or
  with a database you already have running.
- **Certificates.** A fresh CA, server certificate and client certificates are generated per run. The client thumbprints
  are authorised through the real `AUTHORIZE.CONN` command.
- **Fixtures.** State is built by driving the actual CLI commands rather than by writing storage files by hand, so the
  suites do not depend on the on-disk byte layout.
- **Reporting.** A failing check does not abort the suite; every check is recorded and the process exits non-zero at the
  end. Results are written to `integration_results.md` and `performance_results.md` in the repository root, machine
  readable measurements to `performance_metrics.json`, and all three are uploaded as CI artifacts.
- **Measurement.** `harness.benchmark` times repeated operations into a `Stats` object (percentiles, throughput) and
  `harness.ResourceMonitor` samples the server's RSS and CPU time from `/proc` while a suite runs.

## Performance testing

`make test-performance` runs two suites. Neither reports a single stopwatch reading: every operation is repeated and
reported as a latency distribution (p50/p95/p99/max) plus throughput, because one timing on a shared machine says more
about the host than about the code.

- **`test/performance/test_load.py`** — bulk writes, random point reads, four query shapes, `SELECT` and `GET.NEXT`
  against a 10 000-record file, plus the resident memory and CPU time of the server process throughout the run.
- **`test/performance/test_concurrency.py`** — mutual-TLS handshake cost, single-client vs. 8-client read throughput,
  tail latency under contention, concurrent writers (including a lost-update check), and per-connection memory.

Each measurement is guarded in up to three ways, in increasing order of trustworthiness:

1. **Correctness.** Every measured operation also asserts its result count, so a change that is fast only because it
   stopped doing the work still fails.
2. **Budgets.** Absolute p95 ceilings. Deliberately generous, and multiplied by `SRP_PERF_BUDGET_SCALE` for slow or
   noisy hosts — CI uses `4`. Set `SRP_PERF_ENFORCE=0` to downgrade budget violations to informational rows.
3. **Ratios.** How cost grows with data size or client count. These are host independent and are what actually catches
   an accidental O (n²), so they are the tightest checks in the suite.

Every measurement is also written to `performance_metrics.json` (gitignored, uploaded as a CI artifact). To check a
change for regressions, run the suite on both revisions on the same machine and diff the two files:

```
make test-performance && cp performance_metrics.json /tmp/base.json
# ... apply your change ...
make test-performance
make perf-compare BASE=/tmp/base.json
```

`perf-compare` exits non-zero if any metric worsened by more than 25% (`--tolerance` to change it).

### Known cost: writes rewrite the whole file

The remote `WRITE` and `DELETE` handlers call `Database::save()`, which rewrites the entire table file. A single write
is therefore O (file size) and a bulk load is O (n²): at 10 000 records the suite measures p50 1.4 ms per write over the
first quarter of the file and 7.1 ms over the rest, and concurrent writers see a p99 of ~95 ms against a p50 of 5 ms.
This is a design property, not a regression, so the suite pins it at *linear* — the check fails only if the per-write
cost ever starts growing faster than the file does.

## Benchmarking and profiling

`make bench` runs the Criterion micro-benchmarks in `crates/core/benches`. They measure the engine directly, without TLS
or JSON in the way, and Criterion compares each run against the previous one stored in `target/criterion`:

| Group          | What it measures                                                                               |
|----------------|------------------------------------------------------------------------------------------------|
| `record_codec` | `Record` encode/decode for a small (3 field) and a wide (~50 field, multi-valued) record.      |
| `query`        | `parse_query` and `query_for_account` for unique-match, 10%-match and full-scan shapes.        |
| `sort`         | `sort_results_for_account` over 10 000 records, single and compound sort specs.                |
| `storage`      | Building and saving 5 000 records, loading them back from disk, and JSON record serialisation. |

`make bench-smoke` (also a CI step) runs one iteration of each. It is not a timing gate; it keeps the benchmarks
compiling and running, which is how benchmark suites usually rot.

`make profile` produces a flamegraph of the benchmarks with `cargo flamegraph`, falling back to `perf record`, and tells
you what to install if neither is present. Narrow it down with `make profile FILTER=query/`.

## Useful environment variables

| Variable                | Default  | Purpose                                                           |
|-------------------------|----------|-------------------------------------------------------------------|
| `SRP_PERF_RECORDS`      | `10000`  | Record count for the load suite. CI uses a smaller value.         |
| `SRP_PERF_BUDGET_SCALE` | `1`      | Multiplies every latency budget. Raise it on slow or noisy hosts. |
| `SRP_PERF_ENFORCE`      | `1`      | Set to `0` to report budget violations without failing the suite. |
| `SRP_CONC_CLIENTS`      | `8`      | Parallel clients in the concurrency suite.                        |
| `SRP_CONC_OPS`          | `200`    | Operations each concurrent client performs.                       |
| `SRP_STARTUP_TIMEOUT`   | `30`     | Seconds to wait for a server to accept connections.               |
| `SRP_PROFILE`           | `debug`  | Which `target/<profile>` directory to take the binaries from.     |
| `CARGO_TARGET_DIR`      | `target` | Where to look for the built binaries.                             |
| `SRP_KEEP_WORKSPACE`    | unset    | Keep the temporary directory after the run, for debugging.        |

Individual budgets and ratio limits have their own variables (`SRP_BUDGET_*`, `SRP_LIMIT_*`, `SRP_CONC_*`); they are
listed at the top of each performance suite.

Each suite is a plain script, so an individual one can be run directly:

```
cargo build
python3 test/integration/test_security.py
```
