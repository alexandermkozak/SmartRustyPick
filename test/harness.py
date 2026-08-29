"""Shared helpers for the SmartRustyPick integration and performance suites.

Every suite runs inside its own temporary working directory. Both binaries resolve
`config.toml` and the storage directory relative to the current working directory,
so this is enough to keep a run fully isolated from the developer's real
`db_storage/` and `config.toml`, and to let suites run in any order.
"""

import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
PROFILE = os.environ.get("SRP_PROFILE", "debug")
TARGET_DIR = os.path.abspath(os.environ.get("CARGO_TARGET_DIR", os.path.join(REPO_ROOT, "target")))

CLI_BIN = os.path.join(TARGET_DIR, PROFILE, "smart-rusty-pick-cli")
SERVER_BIN = os.path.join(TARGET_DIR, PROFILE, "smart-rusty-pick-server")

# Every suite writes its report and its metrics here rather than into the repository
# root, so a test run never dirties the working copy. `target/` is already ignored,
# which means one rule covers all present and future result files.
RESULTS_DIR = os.path.abspath(os.environ.get("SRP_RESULTS_DIR", os.path.join(TARGET_DIR, "test-results")))

STARTUP_TIMEOUT = float(os.environ.get("SRP_STARTUP_TIMEOUT", "30"))

# Latency budgets are wall-clock and therefore host dependent. They are deliberately
# generous so they only fire on real regressions, and every budget can be stretched
# for slow or noisy machines (CI containers, emulated CPUs) with a single knob.
BUDGET_SCALE = float(os.environ.get("SRP_PERF_BUDGET_SCALE", "1"))
# Set SRP_PERF_ENFORCE=0 to downgrade budget violations to informational rows.
ENFORCE_BUDGETS = os.environ.get("SRP_PERF_ENFORCE", "1") != "0"


def require_binaries(*binaries):
    """Fail fast with an actionable message instead of a confusing FileNotFoundError."""
    missing = [b for b in binaries if not os.path.exists(b)]
    if missing:
        names = ", ".join(os.path.basename(b) for b in missing)
        raise SystemExit(f"Missing binaries ({names}). Run `cargo build` first.")


def free_port():
    """Reserve an ephemeral port from the OS so parallel/CI runs never collide."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _run(cmd):
    subprocess.run(cmd, shell=True, check=True, capture_output=True)


def percentile(sorted_samples, fraction):
    """Nearest-rank percentile over an already sorted sample list."""
    if not sorted_samples:
        return 0.0
    index = int(round(fraction * (len(sorted_samples) - 1)))
    return sorted_samples[index]


class Stats:
    """Latency distribution of a repeated operation, in milliseconds.

    A single timing says very little on a shared machine: it is dominated by whatever
    else the host was doing. Percentiles over many iterations are what make a
    performance signal usable as a regression guard.
    """

    def __init__(self, samples_seconds):
        self.samples = sorted(s * 1000.0 for s in samples_seconds)
        self.count = len(self.samples)
        self.total_ms = sum(self.samples)

    @property
    def mean(self):
        return self.total_ms / self.count if self.count else 0.0

    @property
    def min(self):
        return self.samples[0] if self.count else 0.0

    @property
    def max(self):
        return self.samples[-1] if self.count else 0.0

    @property
    def p50(self):
        return percentile(self.samples, 0.50)

    @property
    def p95(self):
        return percentile(self.samples, 0.95)

    @property
    def p99(self):
        return percentile(self.samples, 0.99)

    @property
    def ops_per_second(self):
        return self.count / (self.total_ms / 1000.0) if self.total_ms else 0.0

    def summary(self):
        return (
            f"n={self.count}, p50 {self.p50:.2f}ms, p95 {self.p95:.2f}ms, "
            f"p99 {self.p99:.2f}ms, max {self.max:.2f}ms, {self.ops_per_second:.0f} ops/s"
        )

    def as_dict(self):
        return {
            "count": self.count,
            "mean_ms": round(self.mean, 4),
            "min_ms": round(self.min, 4),
            "p50_ms": round(self.p50, 4),
            "p95_ms": round(self.p95, 4),
            "p99_ms": round(self.p99, 4),
            "max_ms": round(self.max, 4),
            "total_ms": round(self.total_ms, 4),
            "ops_per_second": round(self.ops_per_second, 2),
        }


def benchmark(func, iterations, warmup=0):
    """Time `func(i)` over `iterations` runs and return (Stats, last result).

    Warmup iterations are executed but not measured, so first-call effects such as
    lazily loading a table from disk do not distort the distribution.
    """
    for i in range(warmup):
        func(i)
    samples = []
    result = None
    for i in range(iterations):
        start = time.perf_counter()
        result = func(i)
        samples.append(time.perf_counter() - start)
    return Stats(samples), result


class ResourceMonitor:
    """Samples a child process's memory and CPU usage while a suite runs.

    Reads `/proc`, so it degrades gracefully (`available` is False) on platforms that
    do not have it instead of failing the suite.

    The sampling thread is held rather than inherited from: subclassing
    `threading.Thread` puts every attribute in the same namespace as the class's own
    private members, and `self._stop` used to shadow `Thread._stop()`, which `join()`
    calls on CPython 3.11 - breaking `stop()` with "'Event' object is not callable".
    """

    def __init__(self, pid, interval=0.1):
        self.pid = pid
        self.interval = interval
        self.available = os.path.exists(f"/proc/{pid}/status")
        self.peak_rss_kb = 0
        self.first_rss_kb = None
        self.last_rss_kb = 0
        self.cpu_seconds = 0.0
        self.samples = 0
        self._stop_event = threading.Event()
        self._thread = threading.Thread(target=self._sample_loop, daemon=True)
        self._ticks = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100

    def _read_rss_kb(self):
        with open(f"/proc/{self.pid}/status") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
        return 0

    def _read_cpu_seconds(self):
        with open(f"/proc/{self.pid}/stat") as handle:
            fields = handle.read().rsplit(") ", 1)[1].split()
        # utime and stime are fields 14 and 15 of /proc/pid/stat (1-based).
        return (int(fields[11]) + int(fields[12])) / self._ticks

    def start(self):
        self._thread.start()
        return self

    def _sample_loop(self):
        if not self.available:
            return
        while not self._stop_event.is_set():
            try:
                rss = self._read_rss_kb()
                self.cpu_seconds = self._read_cpu_seconds()
            except (OSError, IndexError, ValueError):
                break  # the process exited
            if self.first_rss_kb is None:
                self.first_rss_kb = rss
            self.last_rss_kb = rss
            self.peak_rss_kb = max(self.peak_rss_kb, rss)
            self.samples += 1
            self._stop_event.wait(self.interval)

    def stop(self):
        """Idempotent, so a suite can stop the monitor and still stop it in `finally`."""
        self._stop_event.set()
        if self._thread.is_alive():
            self._thread.join(timeout=5)
        return self

    def as_dict(self):
        return {
            "peak_rss_mb": round(self.peak_rss_kb / 1024.0, 2),
            "final_rss_mb": round(self.last_rss_kb / 1024.0, 2),
            "start_rss_mb": round((self.first_rss_kb or 0) / 1024.0, 2),
            "cpu_seconds": round(self.cpu_seconds, 2),
            "samples": self.samples,
        }

    def summary(self):
        if not self.available:
            return "resource monitoring unavailable on this platform"
        return (
            f"peak RSS {self.peak_rss_kb / 1024.0:.1f}MB, "
            f"final RSS {self.last_rss_kb / 1024.0:.1f}MB, "
            f"CPU {self.cpu_seconds:.2f}s over {self.samples} samples"
        )


class Certificates:
    """A throwaway CA plus a server certificate and any number of client certificates."""

    def __init__(self, directory):
        self.dir = directory
        self.ca_crt = os.path.join(directory, "ca.crt")
        self.ca_key = os.path.join(directory, "ca.key")
        self.server_crt = os.path.join(directory, "server.crt")
        self.server_key = os.path.join(directory, "server.key")
        self._create_ca()
        self._create_server()

    def _path(self, *parts):
        return os.path.join(self.dir, *parts)

    def _write_ext(self, name, contents):
        path = self._path(name)
        with open(path, "w") as handle:
            handle.write(contents)
        return path

    def _create_ca(self):
        _run(f"openssl genrsa -out {self.ca_key} 2048")
        _run(
            f"openssl req -x509 -new -nodes -key {self.ca_key} -sha256 -days 365 -out {self.ca_crt} "
            "-subj '/CN=SmartRustyPick Test CA' "
            "-addext 'basicConstraints=critical,CA:TRUE' "
            "-addext 'keyUsage=critical,keyCertSign,cRLSign'"
        )

    def _sign(self, name, common_name, ext_contents):
        key = self._path(f"{name}.key")
        csr = self._path(f"{name}.csr")
        crt = self._path(f"{name}.crt")
        ext = self._write_ext(f"{name}.ext", ext_contents)
        _run(f"openssl genrsa -out {key} 2048")
        _run(f"openssl req -new -key {key} -out {csr} -subj '/CN={common_name}'")
        _run(
            f"openssl x509 -req -in {csr} -CA {self.ca_crt} -CAkey {self.ca_key} -CAcreateserial "
            f"-out {crt} -days 365 -sha256 -extfile {ext}"
        )
        os.remove(ext)
        os.remove(csr)
        return crt, key

    def _create_server(self):
        self._sign(
            "server",
            "localhost",
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            "extendedKeyUsage=serverAuth\n"
            "subjectAltName=DNS:localhost,IP:127.0.0.1\n",
        )

    def client(self, name):
        """Issue a client certificate and return a (crt, key, sha256 thumbprint) triple."""
        crt, key = self._sign(
            name,
            f"SmartRustyPick {name}",
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature\n"
            "extendedKeyUsage=clientAuth\n",
        )
        fingerprint = subprocess.check_output(
            f"openssl x509 -in {crt} -fingerprint -noout -sha256", shell=True
        ).decode()
        thumbprint = fingerprint.split("=", 1)[1].replace(":", "").strip().lower()
        return crt, key, thumbprint


class Client:
    """A mutual-TLS client speaking the newline-delimited JSON protocol."""

    def __init__(self, port, certfile, keyfile, cafile):
        context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=cafile)
        context.load_cert_chain(certfile=certfile, keyfile=keyfile)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_REQUIRED

        sock = socket.create_connection(("127.0.0.1", port), timeout=30)
        self.sock = context.wrap_socket(sock, server_hostname="localhost")
        self.sock.settimeout(60)
        self._buffer = b""

    def request(self, **payload):
        """Send one request and read exactly one newline-terminated response."""
        self.sock.sendall(json.dumps(payload).encode() + b"\n")
        while b"\n" not in self._buffer:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("Server closed the connection before responding")
            self._buffer += chunk
        line, self._buffer = self._buffer.split(b"\n", 1)
        return json.loads(line.decode())

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        finally:
            self.sock.close()

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        self.close()


def wait_for_port(port, process=None, timeout=STARTUP_TIMEOUT):
    """Poll until the server accepts TCP connections, failing early if it died."""
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"Process exited with code {process.returncode} before opening port {port}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return
        except OSError as exc:
            last_error = exc
            time.sleep(0.1)
    raise TimeoutError(f"Port {port} did not open within {timeout}s (last error: {last_error})")


def wait_for_client(port, certfile, keyfile, cafile, timeout=STARTUP_TIMEOUT, process=None):
    """Wait for the port, then retry the TLS handshake until the acceptor is ready."""
    wait_for_port(port, process=process, timeout=timeout)
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            return Client(port, certfile, keyfile, cafile)
        except (ssl.SSLError, OSError) as exc:
            last_error = exc
            time.sleep(0.2)
    raise TimeoutError(f"Could not establish a TLS session on port {port} (last error: {last_error})")


def write_config(port, certs=None, extra="", web_port=None, web_token=None):
    """Write a config.toml into the current working directory.

    When `certs` is None no TLS paths are emitted, which keeps the CLI from
    auto-starting its background server (useful when the suite starts one itself).

    The web dashboard is off unless a suite asks for it: it defaults to a fixed
    port, which two suites - or a suite and the developer's own server - would
    otherwise fight over. A suite that wants it passes a `free_port()` and a
    token, so it knows the token without having to scrape the server's output.
    """
    lines = [f'server_addr = "127.0.0.1"', f"server_port = {port}", 'editor = "true"']
    if certs is not None:
        lines += [
            f'cert_path = "{certs.server_crt}"',
            f'key_path = "{certs.server_key}"',
            f'ca_path = "{certs.ca_crt}"',
        ]
    if web_port is None:
        lines.append("web_enabled = false")
    else:
        lines += ['web_addr = "127.0.0.1"', f"web_port = {web_port}"]
        if web_token is not None:
            lines.append(f'web_token = "{web_token}"')
    with open("config.toml", "w") as handle:
        handle.write("\n".join(lines) + "\n" + extra)


class _Unset:
    """Distinguishes "use the stored token" from "send no token at all"."""


_UNSET = _Unset()


class Dashboard:
    """HTTP client for the web management dashboard.

    Returns `(status, payload)` rather than raising on 4xx: the suites check
    refusals as often as they check successes, and an exception for "401 as
    intended" would read backwards.
    """

    def __init__(self, port, token=None):
        self.base = f"http://127.0.0.1:{port}"
        self.token = token

    def call(self, path, method="GET", payload=None, token=_UNSET, headers=None):
        import urllib.error
        import urllib.request

        body = json.dumps(payload).encode() if payload is not None else None
        request = urllib.request.Request(f"{self.base}{path}", data=body, method=method)
        if body is not None:
            request.add_header("Content-Type", "application/json")
        bearer = self.token if token is _UNSET else token
        if bearer:
            request.add_header("Authorization", f"Bearer {bearer}")
        for name, value in (headers or {}).items():
            request.add_header(name, value)

        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.status, _decode(response.read()), dict(response.headers)
        except urllib.error.HTTPError as error:
            return error.code, _decode(error.read()), dict(error.headers)


def _decode(raw):
    text = raw.decode("utf-8", "replace")
    try:
        return json.loads(text)
    except ValueError:
        return text


def wait_for_dashboard(port, process=None, timeout=STARTUP_TIMEOUT):
    """Poll the dashboard's unauthenticated health endpoint until it answers."""
    import urllib.error
    import urllib.request

    wait_for_port(port, process=process, timeout=timeout)
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=5) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, OSError) as exc:
            last_error = exc
        time.sleep(0.2)
    raise TimeoutError(f"Dashboard on port {port} never became healthy (last error: {last_error})")


def start_server(cwd=None, env=None):
    """Start the headless server binary."""
    require_binaries(SERVER_BIN)
    return subprocess.Popen(
        [SERVER_BIN],
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )


def start_cli(args=(), cwd=None):
    """Start the interactive CLI with its stdin/stdout attached to pipes."""
    require_binaries(CLI_BIN)
    return subprocess.Popen(
        [CLI_BIN, *args],
        cwd=cwd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )


def run_cli(commands, args=(), cwd=None, timeout=60):
    """Run the CLI over a fixed command script and return its combined output."""
    proc = start_cli(args, cwd=cwd)
    script = "".join(f"{line}\n" for line in commands)
    try:
        output, _ = proc.communicate(script, timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        output, _ = proc.communicate()
        raise TimeoutError(f"CLI did not exit within {timeout}s. Output:\n{output}")
    return output


def stop(process, timeout=10):
    """Terminate a child process and return whatever it printed."""
    if process is None:
        return ""
    if process.poll() is None:
        process.terminate()
    try:
        output, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        output, _ = process.communicate()
    return output or ""


class Workspace:
    """A temporary working directory that the suite runs inside."""

    def __init__(self, name):
        self.name = name
        self.path = None
        self._previous_cwd = None

    def __enter__(self):
        self._previous_cwd = os.getcwd()
        self.path = tempfile.mkdtemp(prefix=f"srp-{self.name}-")
        os.chdir(self.path)
        return self

    def __exit__(self, *_exc):
        os.chdir(self._previous_cwd)
        if os.environ.get("SRP_KEEP_WORKSPACE"):
            print(f"Workspace kept at {self.path}")
        else:
            shutil.rmtree(self.path, ignore_errors=True)
        return False


class Suite:
    """Collects results so a single failure does not hide the rest of the suite."""

    def __init__(
        self,
        name,
        results_file,
        title="Integration Test Results",
        detail_header="Details",
        metrics_file=None,
    ):
        self.name = name
        os.makedirs(RESULTS_DIR, exist_ok=True)
        self.results_file = os.path.join(RESULTS_DIR, results_file)
        self.title = title
        self.detail_header = detail_header
        self.metrics_file = os.path.join(RESULTS_DIR, metrics_file) if metrics_file else None
        self.failures = 0
        self.checks = 0
        self.metrics = {}

    def _log(self, test_name, status, detail):
        new_file = not os.path.exists(self.results_file)
        with open(self.results_file, "a") as handle:
            if new_file:
                handle.write(f"# {self.title}\n\n")
                handle.write(f"**Date:** {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
                handle.write(f"| Test Name | Status | {self.detail_header} |\n")
                handle.write("| --- | --- | --- |\n")
            handle.write(f"| {test_name} | {status} | {detail} |\n")

    def check(self, test_name, passed, detail=""):
        self.checks += 1
        status = "Success" if passed else "Failure"
        if not passed:
            self.failures += 1
        print(f"  [{status}] {test_name}{f': {detail}' if detail else ''}")
        self._log(f"{self.name}: {test_name}", status, detail or "-")
        return passed

    def check_eq(self, test_name, actual, expected):
        passed = actual == expected
        detail = "as expected" if passed else f"expected `{expected}`, got `{actual}`"
        return self.check(test_name, passed, detail)

    def record(self, metric_name, payload):
        """Store a machine-readable metric for trend tracking and baseline comparison."""
        self.metrics[f"{self.name}: {metric_name}"] = payload

    def measure(self, test_name, stats, budget_ms=None, extra="", passed=True):
        """Report a latency distribution and, when a budget is given, guard p95 against it.

        `passed` carries the correctness verdict of the measured operation, so a change
        that is fast only because it stopped doing the work still fails.
        """
        detail = stats.summary()
        if extra:
            detail = f"{extra}; {detail}"
        budget = None
        if budget_ms is not None:
            budget = budget_ms * BUDGET_SCALE
            within = stats.p95 <= budget
            detail += f"; budget p95 <= {budget:.2f}ms"
            if not within:
                detail += " (EXCEEDED)"
            if ENFORCE_BUDGETS:
                passed = passed and within
        self.record(test_name, dict(stats.as_dict(), budget_p95_ms=budget))
        return self.check(test_name, passed, detail)

    def check_ratio(self, test_name, ratio, limit, detail=""):
        """Guard a *relative* measurement, which stays meaningful on any host.

        Absolute timings vary by an order of magnitude between machines; the shape of
        the curve (how cost grows with data size or concurrency) does not, which makes
        ratios the reliable way to catch complexity regressions.
        """
        message = f"{ratio:.2f}x (limit {limit:.2f}x)"
        if detail:
            message = f"{detail}; {message}"
        self.record(test_name, {"ratio": round(ratio, 3), "limit": limit})
        return self.check(test_name, ratio <= limit, message)

    def error(self, test_name, exc):
        self.checks += 1
        self.failures += 1
        print(f"  [Failure] {test_name}: {exc}")
        self._log(f"{self.name}: {test_name}", "Failure", str(exc).replace("|", "/").replace("\n", " "))

    def _write_metrics(self):
        if not self.metrics_file:
            return
        document = {"generated": time.strftime("%Y-%m-%dT%H:%M:%S"), "metrics": {}}
        if os.path.exists(self.metrics_file):
            try:
                with open(self.metrics_file) as handle:
                    document = json.load(handle)
            except (OSError, ValueError):
                pass
        document.setdefault("metrics", {}).update(self.metrics)
        document["generated"] = time.strftime("%Y-%m-%dT%H:%M:%S")
        with open(self.metrics_file, "w") as handle:
            json.dump(document, handle, indent=2, sort_keys=True)
            handle.write("\n")

    def finish(self):
        self._write_metrics()
        print(f"\n{self.name}: {self.checks - self.failures}/{self.checks} checks passed")
        if self.failures:
            print(f"{self.name} FAILED", file=sys.stderr)
            return 1
        print(f"{self.name} PASSED")
        return 0
