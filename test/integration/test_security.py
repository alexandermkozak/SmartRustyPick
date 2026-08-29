"""Verifies that the headless server enforces admin privileges and per-account access,
and that it enforces its request-size, handshake, idle and connection-count limits."""

import os
import socket
import ssl
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TEST_ACC"
ADMIN_REQUIRED = "Admin privileges required"


def seed_database(admin_thumbprint, user_thumbprint):
    """Create the account and the two client authorisations through the CLI.

    Building the state with the real commands keeps this suite honest: it no longer
    depends on the on-disk registry byte layout, which the previous version hardcoded.
    """
    output = harness.run_cli(
        [
            f"AUTHORIZE.CONN {admin_thumbprint} admin ADMIN",
            f"CREATE.ACCOUNT {ACCOUNT}",
            f"AUTHORIZE.CONN {user_thumbprint} user {ACCOUNT}",
            "SAVE",
            "EXIT",
        ],
        args=["--account", "SYSTEM"],
    )
    if "Error" in output:
        raise RuntimeError(f"CLI setup failed:\n{output}")
    return output


def check_connection_limits(suite, certs, user_crt, user_key):
    """Exercises the request-size, handshake, idle and connection-count limits
    (issue #13) against a dedicated server with tight settings, so the suite
    does not have to wait out the much larger production defaults.
    """
    port = harness.free_port()
    harness.write_config(
        port,
        certs,
        extra=(
            "max_request_bytes = 4096\n"
            "handshake_timeout_ms = 500\n"
            "idle_timeout_ms = 700\n"
            "max_connections = 2\n"
        ),
    )
    server = harness.start_server()
    try:
        harness.wait_for_port(port, process=server)

        # Oversized request: the read is bounded, and the connection is closed
        # with a clean error rather than left to grow its buffer forever.
        client = harness.wait_for_client(port, user_crt, user_key, certs.ca_crt, process=server)
        try:
            client.sock.sendall(b"x" * 8192)  # no trailing newline, over the 4096 byte cap
            client.sock.settimeout(5)
            buf = b""
            try:
                while b"\n" not in buf:
                    chunk = client.sock.recv(65536)
                    if not chunk:
                        break
                    buf += chunk
            except socket.timeout:
                pass
            suite.check(
                "Oversized request is rejected with a clean error",
                b'"ERROR"' in buf and b"too large" in buf.lower(),
                buf.decode("utf-8", "replace"),
            )
            client.sock.settimeout(2)
            try:
                closed = client.sock.recv(1) == b""
            except (socket.timeout, OSError):
                closed = False
            suite.check("Oversized request closes the connection", closed)
        finally:
            client.close()
        time.sleep(0.2)  # let the server notice the close and free its slot

        # Connection cap: the two allowed slots succeed; a third is rejected
        # before it ever reaches the TLS handshake.
        capped = [
            harness.wait_for_client(port, user_crt, user_key, certs.ca_crt, process=server)
            for _ in range(2)
        ]
        try:
            over_cap_connected = True
            try:
                extra_client = harness.Client(port, user_crt, user_key, certs.ca_crt)
                extra_client.close()
            except (ssl.SSLError, OSError):
                over_cap_connected = False
            suite.check("Connection beyond max_connections is rejected", not over_cap_connected)
        finally:
            for c in capped:
                c.close()
        time.sleep(0.2)  # let the server notice the closes and free their slots

        # Handshake timeout: a connection that never starts TLS is reaped
        # instead of held open forever.
        raw = socket.create_connection(("127.0.0.1", port), timeout=10)
        try:
            raw.settimeout(5)
            start = time.time()
            data = raw.recv(1)
            elapsed = time.time() - start
            suite.check(
                "Stalled TLS handshake is reaped",
                data == b"" and elapsed < 5,
                f"elapsed={elapsed:.2f}s data={data!r}",
            )
        finally:
            raw.close()

        # Idle timeout: an authenticated connection that goes quiet is closed
        # rather than held open indefinitely.
        idle_client = harness.wait_for_client(port, user_crt, user_key, certs.ca_crt, process=server)
        try:
            idle_client.sock.settimeout(5)
            time.sleep(1.0)  # past idle_timeout_ms
            data = idle_client.sock.recv(1)
            suite.check("Idle connection is reaped", data == b"", f"data={data!r}")
        finally:
            idle_client.close()
    finally:
        output = harness.stop(server)
        if suite.failures:
            print("--- limits server output ---")
            print(output)


def main():
    suite = harness.Suite("Security", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("security") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        user_crt, user_key, user_tp = certs.client("user")
        port = harness.free_port()

        server = None
        try:
            # The CLI auto-starts a background server when the config carries TLS paths,
            # so seed the database first and only then hand the port to the headless server.
            harness.write_config(port, certs=None)
            seed_database(admin_tp, user_tp)
            harness.write_config(port, certs)

            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            user = harness.Client(port, user_crt, user_key, certs.ca_crt)

            with admin, user:
                resp = user.request(command="CREATE.ACCOUNT", target_account="EVIL_ACC")
                suite.check_eq("Non-admin CREATE.ACCOUNT is blocked", resp.get("message"), ADMIN_REQUIRED)

                resp = admin.request(command="CREATE.ACCOUNT", target_account="NEW_ACC")
                suite.check_eq("Admin CREATE.ACCOUNT is allowed", resp["status"], "OK")

                resp = user.request(command="CREATE.FILE", file="EVIL_FILE", account=ACCOUNT)
                suite.check_eq("Non-admin CREATE.FILE is blocked", resp.get("message"), ADMIN_REQUIRED)

                resp = admin.request(command="CREATE.FILE", file="GOOD_FILE", account=ACCOUNT)
                suite.check_eq("Admin CREATE.FILE is allowed", resp["status"], "OK")

                resp = user.request(
                    command="AUTHORIZE.CONN", thumbprint="1234", name="evil_client", is_admin=True
                )
                suite.check_eq("Non-admin AUTHORIZE.CONN is blocked", resp.get("message"), ADMIN_REQUIRED)

                resp = admin.request(
                    command="AUTHORIZE.CONN",
                    thumbprint="5678",
                    name="new_client",
                    accounts_list=[ACCOUNT],
                )
                suite.check_eq("Admin AUTHORIZE.CONN is allowed", resp["status"], "OK")

                resp = user.request(command="DELETE.ACCOUNT", target_account=ACCOUNT)
                suite.check_eq("Non-admin DELETE.ACCOUNT is blocked", resp.get("message"), ADMIN_REQUIRED)

                # The user client is only authorised for TEST_ACC.
                resp = user.request(command="READ", file="GOOD_FILE", key="K1", account="NEW_ACC")
                suite.check(
                    "Non-admin cannot reach an account outside its allow list",
                    resp["status"] == "ERROR" and "Access denied" in (resp.get("message") or ""),
                    resp.get("message", ""),
                )

                # ...but it may reach its own account, where the record simply does not exist.
                resp = user.request(command="READ", file="GOOD_FILE", key="K1", account=ACCOUNT)
                suite.check_eq(
                    "Non-admin may reach its own account", resp.get("message"), "Record not found"
                )

            check_connection_limits(suite, certs, user_crt, user_key)
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Security suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
