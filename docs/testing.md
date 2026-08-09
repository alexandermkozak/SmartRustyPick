# Testing

The project has three layers of tests, all runnable from the `Makefile` and all executed by the
`Build and Test` GitHub workflow on every push to `main` and every pull request.

| Layer       | Command                 | What it covers                                                                                          |
|-------------|-------------------------|---------------------------------------------------------------------------------------------------------|
| Unit        | `make test-unit`        | `cargo test --workspace` — the engine, query parser, dictionaries and the request handler.              |
| Integration | `make test-integration` | The real binaries over the TLS protocol: CRUD, queries, select lists, headless mode and access control. |
| Performance | `make test-performance` | A bulk write plus representative query shapes, reported as timings.                                     |

`make test-all` runs all three. Everything below the unit layer requires `cargo build` first; the Make targets take care
of it.

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
  end. Results are written to `integration_results.md` and
  `performance_results.md` in the repository root, and uploaded as CI artifacts.

## Useful environment variables

| Variable              | Default  | Purpose                                                          |
|-----------------------|----------|------------------------------------------------------------------|
| `SRP_PERF_RECORDS`    | `10000`  | Record count for the performance suite. CI uses a smaller value. |
| `SRP_STARTUP_TIMEOUT` | `30`     | Seconds to wait for a server to accept connections.              |
| `SRP_PROFILE`         | `debug`  | Which `target/<profile>` directory to take the binaries from.    |
| `CARGO_TARGET_DIR`    | `target` | Where to look for the built binaries.                            |
| `SRP_KEEP_WORKSPACE`  | unset    | Keep the temporary directory after the run, for debugging.       |

Each suite is a plain script, so an individual one can be run directly:

```
cargo build
python3 test/integration/test_security.py
```
