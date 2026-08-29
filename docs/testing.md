# Testing

The project has five layers of tests, all runnable from the `Makefile` and all executed by the
`Build and Test` GitHub workflow on every push to `main` and every pull request.

| Layer       | Command                 | What it covers                                                                                                                                  |
|-------------|-------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------|
| Unit        | `make test-unit`        | `cargo test --workspace` — the engine, query parser, dictionaries and the request handler.                                                      |
| Integration | `make test-integration` | The real binaries over the TLS protocol: CRUD, queries, select lists, headless mode, access control, per-file durability and the web dashboard. |
| Performance | `make test-performance` | End-to-end latency distributions, throughput, scaling ratios, concurrency and resource usage.                                                   |
| Benchmarks  | `make bench`            | Criterion micro-benchmarks of the engine: record codec, query execution, sorting, persistence.                                                  |
| Front end   | `make ui-test`          | The dashboard's Vue slices under jsdom, plus the architecture test that keeps features from importing each other.                               |

`make test-all` runs the first three; `make ui-test` covers the dashboard's front end and needs node. Everything below
the unit layer requires `cargo build` first; the Make targets
take care of it.

## Requirements

- A Rust toolchain (for `cargo build` / `cargo test`).
- `python3` (standard library only — no packages to install).
- `openssl` on the `PATH`, used to mint throwaway certificates.
- Node 22+ **only** for the dashboard front end (`make ui-test`, `make ui-build`). The built bundle is committed, so
  building and testing the database itself never needs it.

## How the Python suites work

The suites live in `test/` and share `test/harness.py`:

- **Isolation.** Each suite runs inside its own temporary directory. Because both binaries resolve
  `config.toml` and the storage directory relative to the working directory, this keeps a run from ever touching your
  real `db_storage/` or `config.toml`. Nothing is left behind in the repository.
- **Ports.** Every suite asks the OS for a free port instead of hardcoding one, so runs never collide with each other or
  with a database you already have running. The web dashboard binds a fixed port by default, so `harness.write_config`
  disables it unless a suite asks for it with a free port of its own.
- **Certificates.** A fresh CA, server certificate and client certificates are generated per run. The client thumbprints
  are authorised through the real `AUTHORIZE.CONN` command.
- **Fixtures.** State is built by driving the actual CLI commands rather than by writing storage files by hand, so the
  suites do not depend on the on-disk byte layout.
- **Reporting.** A failing check does not abort the suite; every check is recorded and the process exits non-zero at the
  end. Results are written to `target/test-results/`: `integration_results.md`, `performance_results.md` and the
  machine readable `performance_metrics.json`. The whole directory is gitignored and uploaded as a CI artifact, so a
  test run never dirties the working copy. Override the location with `SRP_RESULTS_DIR` (or `make RESULTS_DIR=...`).
- **Measurement.** `harness.benchmark` times repeated operations into a `Stats` object (percentiles, throughput) and
  `harness.ResourceMonitor` samples the server's RSS and CPU time from `/proc` while a suite runs.

## Performance testing

`make test-performance` runs two suites. Neither reports a single stopwatch reading: every operation is repeated and
reported as a latency distribution (p50/p95/p99/max) plus throughput, because one timing on a shared machine says more
about the host than about the code.

- **`test/performance/test_load.py`** — bulk writes, random point reads, four query shapes, `SELECT` and `GET.NEXT`
  against a 10 000-record file, plus the resident memory and CPU time of the server process throughout the run.
- **`test/performance/test_concurrency.py`** — mutual-TLS handshake cost, single-client vs. 8-client read throughput,
  tail latency under contention, handshake cost while a large buffered burst is flushed to disk, concurrent writers
  (including a lost-update check), and per-connection memory.

Each measurement is guarded in up to three ways, in increasing order of trustworthiness:

1. **Correctness.** Every measured operation also asserts its result count, so a change that is fast only because it
   stopped doing the work still fails.
2. **Budgets.** Absolute p95 ceilings. Deliberately generous, and multiplied by `SRP_PERF_BUDGET_SCALE` for slow or
   noisy hosts — CI uses `4`. Set `SRP_PERF_ENFORCE=0` to downgrade budget violations to informational rows.
3. **Ratios.** How cost grows with data size or client count. These are host independent and are what actually catches
   an accidental O (n²), so they are the tightest checks in the suite.

Every measurement is also written to `target/test-results/performance_metrics.json` (gitignored, uploaded as a CI
artifact). To check a change for regressions, run the suite on both revisions on the same machine and diff the two
files:

```
make test-performance && cp target/test-results/performance_metrics.json /tmp/base.json
# ... apply your change ...
make test-performance
make perf-compare BASE=/tmp/base.json
```

`perf-compare` exits non-zero if any metric worsened by more than 25% (`--tolerance` to change it).

### Hashed Storage Performance

The storage engine uses a hashed layout to ensure that write cost remains flat as the table grows. A single write only
rewrites its corresponding hash group (averaging 16 records) rather than the entire table.

The performance suite enforces this with two specific checks:

- **Write cost growth**: Measures the ratio between the p50 write latency of the first 2,500 records and the remaining
  7,500. This must stay below 2.0x (currently measured at ~0.99x).
- **Write amplification**: Ensures that no single group file exceeds 5% of the total table size, verifying that records
  are evenly distributed and the dynamic modulus is working correctly.

## Benchmarking and profiling

`make bench` runs the Criterion micro-benchmarks in `crates/core/benches`. They measure the engine directly, without TLS
or JSON in the way, and Criterion compares each run against the previous one stored in `target/criterion`:

| Group          | What it measures                                                                                |
|----------------|-------------------------------------------------------------------------------------------------|
| `record_codec` | `Record` encode/decode for a small (3 field) and a wide (~50 field, multi-valued) record.       |
| `query`        | `parse_query` and `query_for_account` for unique-match, 10%-match and full-scan shapes.         |
| `sort`         | `sort_results_for_account` over 10 000 records, single and compound sort specs.                 |
| `storage`      | Saving 5 000 records, loading them back from disk, JSON serialisation, and `incremental_write`. |

`storage/incremental_write/{1000,10000}_records` is the sharpest guard on write amplification: it updates a single
record and flushes, on a small and on a large table. The two figures must match - 63 us and 61 us when this was written.
A gap between them means a write has started scaling with the size of the table again.

`make bench-smoke` (also a CI step) runs one iteration of each. It is not a timing gate; it keeps the benchmarks
compiling and running, which is how benchmark suites usually rot.

`make profile` produces a flamegraph of the benchmarks with `cargo flamegraph`, falling back to `perf record`, and tells
you what to install if neither is present. Narrow it down with `make profile FILTER=query/`.

## Useful environment variables

| Variable                 | Default  | Purpose                                                           |
|--------------------------|----------|-------------------------------------------------------------------|
| `SRP_PERF_RECORDS`       | `10000`  | Record count for the load suite. CI uses a smaller value.         |
| `SRP_PERF_BUDGET_SCALE`  | `1`      | Multiplies every latency budget. Raise it on slow or noisy hosts. |
| `SRP_PERF_ENFORCE`       | `1`      | Set to `0` to report budget violations without failing the suite. |
| `SRP_CONC_CLIENTS`       | `8`      | Parallel clients in the concurrency suite.                        |
| `SRP_CONC_OPS`           | `200`    | Operations each concurrent client performs.                       |
| `SRP_STARTUP_TIMEOUT`    | `30`     | Seconds to wait for a server to accept connections.               |
| `SRP_PROFILE`            | `debug`  | Which `target/<profile>` directory to take the binaries from.     |
| `CARGO_TARGET_DIR`       | `target` | Where to look for the built binaries.                             |
| `SRP_KEEP_WORKSPACE`     | unset    | Keep the temporary directory after the run, for debugging.        |
| `SRP_LIMIT_WRITE_GROWTH` | `2.0`    | Max allowed growth ratio for write cost as the table grows.       |
| `SRP_LIMIT_GROUP_SHARE`  | `0.05`   | Max allowed fraction of the table for a single hash group.        |

Individual budgets and ratio limits have their own variables (`SRP_BUDGET_*`, `SRP_LIMIT_*`, `SRP_CONC_*`); they are
listed at the top of each performance suite.

Each suite is a plain script, so an individual one can be run directly:

```
cargo build
python3 test/integration/test_security.py
```
