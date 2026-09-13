"""Verifies that the headless server enforces admin privileges and per-account access,
and that it enforces its request-size, handshake, idle and connection-count limits."""

import datetime
import json
import os
import socket
import ssl
import subprocess
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TEST_ACC"
# The protocol's error code for an admin-only command, asserted rather than the
# message beside it: the wording may change, the code may not.
ADMIN_REQUIRED = "ADMIN_REQUIRED"


def as_client(port, certificate, private_key, ca, request):
    """One request over the protocol on a connection of its own.

    A capability is a property of the certificate, so the only honest way to
    test one is to connect with that certificate rather than to ask the admin
    connection what it thinks would happen.
    """
    with harness.Client(port, certificate, private_key, ca) as client:
        return client.request(**request)


def check_certificate_lifetimes(suite, admin):
    """A caller can ask for a shorter certificate, within the deployment's cap.

    There is no CRL and no OCSP, so removing a thumbprint is the only withdrawal
    there is - and it only helps if somebody notices a leak. A short lifetime is
    the withdrawal that happens whether or not anyone notices (issue #112). This
    suite runs with max_client_cert_days = 30.
    """
    resp = admin.request(command="GENERATE.CERT", name="week-long", is_admin=True, days=7)
    suite.check_eq("A certificate can be issued for fewer days", resp.get("status"), "OK")
    issued = resp.get("record") or {}
    expires = issued.get("expires_at") or ""
    suite.check(
        "and it reports when it expires, in UTC",
        expires.endswith("Z") and len(expires) == 20,
        f"expires_at={expires!r}",
    )

    # The reported date must be the certificate's own, not a guess.
    on_disk = subprocess.run(
        ["openssl", "x509", "-enddate", "-noout", "-in", issued.get("cert_path", "")],
        capture_output=True,
        text=True,
    )
    parsed = ""
    if on_disk.returncode == 0:
        raw = on_disk.stdout.strip().removeprefix("notAfter=")
        parsed = datetime.datetime.strptime(raw, "%b %d %H:%M:%S %Y %Z").strftime("%Y-%m-%dT%H:%M:%SZ")
    suite.check_eq("and the date is the certificate's own", parsed, expires)

    # About seven days out, which is the property the whole feature is for.
    not_after = datetime.datetime.strptime(expires, "%Y-%m-%dT%H:%M:%SZ").replace(
        tzinfo=datetime.timezone.utc
    )
    days_out = (not_after - datetime.datetime.now(datetime.timezone.utc)).days
    suite.check("and it really is a week, not a year", 6 <= days_out <= 7, f"{days_out} days")

    # Above the deployment's ceiling: refused, never shortened.
    resp = admin.request(command="GENERATE.CERT", name="too-long", is_admin=True, days=365)
    suite.check_eq("A lifetime above the cap is refused", resp.get("code"), "INVALID_REQUEST")
    suite.check(
        "and the refusal says what the cap is",
        "30" in (resp.get("message") or ""),
        resp.get("message", ""),
    )

    resp = admin.request(command="GENERATE.CERT", name="zero-days", is_admin=True, days=0)
    suite.check_eq("A lifetime of zero is refused", resp.get("code"), "INVALID_REQUEST")

    # And nothing was issued for either refusal - a refusal that still wrote a
    # key would leave a private key on disk for a client nobody authorized.
    listing = dict(admin.request(command="LIST.CONNS").get("results") or [])
    suite.check(
        "A refused request authorizes nothing",
        "too-long" not in listing and "zero-days" not in listing,
        ", ".join(sorted(listing)),
    )

    # The expiry survives into $CLIENTS and is reported per client, with a day
    # count - an expiry nobody can see is an outage waiting to happen.
    entry = listing.get("week-long") or {}
    suite.check_eq("LIST.CONNS reports the expiry", entry.get("expires_at"), expires)
    # Seven, not six: the count is calendar days between the dates, so a
    # certificate issued moments ago for seven days does not read as six.
    suite.check_eq("and how many days are left", entry.get("expires_in_days"), 7)

    # A client authorized by thumbprint alone has no certificate to read, so it
    # reports no expiry rather than an invented one.
    admin.request(command="AUTHORIZE.CONN", thumbprint="feed1234", name="blind-auth", is_admin=True)
    listing = dict(admin.request(command="LIST.CONNS").get("results") or [])
    blind = listing.get("blind-auth") or {}
    suite.check_eq("An AUTHORIZE.CONN records no expiry", blind.get("expires_at"), None)
    suite.check_eq("nor a day count", blind.get("expires_in_days"), None)


def check_capabilities(suite, admin, certs, port, user_crt, user_key):
    """A capability grants a command without granting an account (issue #111).

    `ADMIN` used to be one flag doing two jobs, so anything that could create an
    account could also read every record in the database. These checks are the
    split: a provisioning credential does its job and can do nothing else.
    """
    # Issued with a capability and no accounts at all - the shape that was not
    # expressible before.
    resp = admin.request(
        command="GENERATE.CERT",
        name="provisioner",
        capabilities=["accounts:manage"],
    )
    if resp.get("status") != "OK":
        suite.check("A capability-only certificate is issued", False, resp.get("message", ""))
        return
    issued = resp.get("record") or {}
    suite.check(
        "A certificate can be issued with a capability and no accounts",
        issued.get("cert_path") and issued.get("key_path"),
    )

    # LIST.CONNS reports it as what it is, rather than as an unexplained flag.
    listing = dict(admin.request(command="LIST.CONNS").get("results") or [])
    entry = listing.get("provisioner") or {}
    suite.check_eq("and it is listed with its capability", entry.get("capabilities"), ["accounts:manage"])
    suite.check_eq("without being an admin", entry.get("is_admin"), False)
    suite.check_eq("and with no accounts", entry.get("accounts"), [])

    # Now connect as that credential and find out what it can actually do.
    prov_crt, prov_key = issued["cert_path"], issued["key_path"]

    resp = as_client(port, prov_crt, prov_key, certs.ca_crt,
                     {"command": "CREATE.ACCOUNT", "target_account": "PROVISIONED"})
    suite.check_eq("The provisioner can create an account", resp.get("status"), "OK")

    # ...and nothing else. Creating an account does not grant access to it.
    for command, extra in [
        ("READ", {"file": "DIR", "key": "X"}),
        ("WRITE", {"file": "DIR", "key": "X", "data": "V"}),
        ("LIST.FILES", {}),
        ("CREATE.FILE", {"file": "SNEAKY"}),
    ]:
        resp = as_client(port, prov_crt, prov_key, certs.ca_crt,
                             {"command": command, "account": "PROVISIONED", **extra})
        suite.check_eq(
            f"and cannot {command} in the account it just created",
            resp.get("code"),
            "ACCESS_DENIED",
        )

    # Nor into somebody else's.
    resp = as_client(port, prov_crt, prov_key, certs.ca_crt,
                     {"command": "READ", "account": ACCOUNT, "file": "GOOD_FILE", "key": "K1"})
    suite.check_eq("nor read another account", resp.get("code"), "ACCESS_DENIED")

    # Nor hand itself the access, which is the escalation the split exists to
    # prevent: granting accounts is `clients:manage`, a different capability.
    resp = as_client(port, prov_crt, prov_key, certs.ca_crt,
                     {"command": "ADD.CLIENT.ACCOUNT", "name": "provisioner",
                          "accounts_list": ["PROVISIONED"]})
    suite.check_eq("nor grant itself an account", resp.get("code"), "ADMIN_REQUIRED")
    suite.check(
        "and the refusal names the capability it lacked",
        "clients:manage" in (resp.get("message") or ""),
        resp.get("message", ""),
    )

    # A client allowed an account may now shape it, which is the other half of
    # the split: it could already rewrite every record in the file.
    resp = as_client(port, user_crt, user_key, certs.ca_crt,
                         {"command": "CREATE.FILE", "account": ACCOUNT, "file": "SHAPED_BY_USER"})
    suite.check_eq("A client allowed an account may create a file in it", resp.get("status"), "OK")

    resp = as_client(port, user_crt, user_key, certs.ca_crt,
                         {"command": "CREATE.FILE", "account": "PROVISIONED", "file": "NOPE"})
    suite.check_eq("but not in an account it is not allowed", resp.get("code"), "ACCESS_DENIED")

    # A typo must not read as a grant that quietly does nothing.
    resp = admin.request(command="AUTHORIZE.CONN", thumbprint="abc123", name="typo",
                         capabilities=["accounts:mange"])
    suite.check_eq("An unknown capability is refused, not ignored", resp.get("code"), "INVALID_DATA")


def check_no_secret_leakage(suite, admin, workspace_path):
    """Issuing a certificate must not leave its key material anywhere (issue #55).

    The rule is that key material, passphrases and tokens never reach `$LOGS`,
    stdout, a protocol response or an HTTP body, with one deliberate exception:
    the issuance path, which exists to deliver exactly that. So this issues a
    certificate the documented way and then looks for its secrets everywhere
    *except* the response that carried them.
    """
    resp = admin.request(command="GENERATE.CERT", name="leak-probe", accounts_list=[ACCOUNT])
    if resp.get("status") != "OK":
        suite.check("A certificate is issued for the leak probe", False, resp.get("message", ""))
        return
    issued = resp.get("record") or {}

    # The body of the key, not its header - the PEM armour is a constant and
    # would match text that carries no key at all.
    key_body = "".join(issued.get("private_key_pem", "").splitlines()[1:-1])
    passphrase = issued.get("pfx_passphrase") or ""
    suite.check(
        "The issuance response carries the key and passphrase, as it must",
        len(key_body) > 100 and len(passphrase) >= 32,
    )

    # Everything the database has written. `$LOGS` and `$SAVEDLISTS` are in
    # here, so this covers the acceptance criterion without depending on how
    # either is queried.
    found = []
    for root, _dirs, files in os.walk(os.path.join(workspace_path, "db_storage")):
        for name in files:
            path = os.path.join(root, name)
            with open(path, "rb") as handle:
                blob = handle.read()
            if key_body.encode() in blob or passphrase.encode() in blob:
                found.append(os.path.relpath(path, workspace_path))
    suite.check(
        "No issued key or passphrase is written into the database",
        not found,
        ", ".join(found),
    )

    # And a second read of the same material through an ordinary command must
    # not produce it: `LIST.CONNS` names clients, it does not hand back keys.
    resp = admin.request(command="LIST.CONNS")
    listing = json.dumps(resp)
    suite.check(
        "LIST.CONNS reports the client without its key material",
        key_body not in listing and passphrase not in listing and "leak-probe" in listing,
    )


def tls_handshake(port, certificate, private_key, ca, maximum=None, minimum=None):
    """Attempts one handshake at a bounded TLS version, reporting what happened.

    Returns the negotiated version on success, or the exception text on failure.
    The client offers a valid certificate either way, so a refusal is about the
    protocol version and nothing else.
    """
    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=ca)
    context.load_cert_chain(certfile=certificate, keyfile=private_key)
    if maximum is not None:
        context.maximum_version = maximum
    if minimum is not None:
        context.minimum_version = minimum
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
            with context.wrap_socket(raw, server_hostname="localhost") as tls:
                return tls.version(), None
    except (ssl.SSLError, OSError) as exc:
        return None, str(exc)


def check_tls_floor(suite, port, user_crt, user_key, ca):
    """The listener speaks TLS 1.3 and refuses anything older (issue #52).

    A client capped at 1.2 is the whole test: it holds a certificate this CA
    issued, so the only reason the handshake can fail is the version floor.
    """
    version, failure = tls_handshake(port, user_crt, user_key, ca)
    suite.check_eq("An ordinary client negotiates TLS 1.3", version, "TLSv1.3")

    version, failure = tls_handshake(port, user_crt, user_key, ca, maximum=ssl.TLSVersion.TLSv1_2)
    suite.check_eq("A client capped at TLS 1.2 is refused", version, None)
    # The refusal must come from the version negotiation rather than from a
    # certificate problem, or the test would pass for the wrong reason.
    suite.check(
        "and refused over the protocol version, not the certificate",
        failure is not None and "version" in failure.lower(),
        failure or "the handshake succeeded",
    )


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


def check_ca_rotation(suite, certs, user_crt, user_key):
    """A CA can be rotated with an overlap, instead of as a flag day (issue #60).

    Every client certificate is signed by one CA, so replacing it invalidates
    all of them at once - unless the retiring CA stays trusted while clients are
    reissued one at a time. That overlap is the whole feature, so it is tested
    against a real listener with two genuinely different CAs rather than against
    the configuration that describes it.
    """
    incoming = certs.sibling_ca("ca-incoming")
    new_crt, new_key, new_tp = incoming.client("reissued")

    port = harness.free_port()
    # `certs` still signs the server certificate and any new issuance; the
    # incoming CA is trusted but does not issue. This is the shape of a
    # transition window.
    harness.write_config(port, certs, additional_cas=[incoming.ca_crt])
    output = harness.run_cli(
        [f"AUTHORIZE.CONN {new_tp} reissued-client ADMIN", "SAVE", "EXIT"],
        args=["--account", "SYSTEM"],
    )
    if "Error" in output:
        suite.check("The reissued client can be authorized", False, output)
        return

    server = harness.start_server()
    try:
        harness.wait_for_port(port, process=server)

        # The client signed by the CA that was there all along.
        resp = as_client(port, user_crt, user_key, certs.ca_crt, {"command": "LIST.FILES", "account": ACCOUNT})
        suite.check_eq(
            "A client signed by the original CA connects during the overlap",
            resp.get("status"),
            "OK",
        )

        # And one signed by a CA that did not exist when the server certificate
        # was made. It presents the trusted bundle as its root, because during a
        # rotation the server may be signed by either.
        bundle = os.path.join(os.path.dirname(incoming.ca_crt), "trusted-bundle.crt")
        with open(bundle, "w") as handle:
            for path in (certs.ca_crt, incoming.ca_crt):
                with open(path) as source:
                    handle.write(source.read())
        resp = as_client(port, new_crt, new_key, bundle, {"command": "LIST.ACCOUNTS"})
        suite.check_eq(
            "and one signed by the incoming CA connects too",
            resp.get("status"),
            "OK",
        )
    finally:
        harness.stop(server)

    # Rotation complete: the outgoing CA is dropped. The certificate signed by
    # it stops being accepted, which is the other half of the claim - an overlap
    # that never ends is not a rotation.
    harness.write_config(port, incoming, additional_cas=None)
    # The server certificate is still signed by the original CA, so the listener
    # re-signs it against the one now configured rather than presenting one no
    # current client can verify.
    server = harness.start_server()
    try:
        harness.wait_for_port(port, process=server)
        refused = False
        try:
            as_client(port, user_crt, user_key, certs.ca_crt, {"command": "LIST.ACCOUNTS"})
        except Exception:  # noqa: BLE001 - any handshake failure is the point
            refused = True
        suite.check("Once the old CA is retired, its client is refused", refused)

        resp = as_client(port, new_crt, new_key, incoming.ca_crt, {"command": "LIST.ACCOUNTS"})
        suite.check_eq(
            "while the reissued client keeps working",
            resp.get("status"),
            "OK",
        )
    finally:
        harness.stop(server)


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
            # A ceiling below the default, so the refusal path is exercised.
            harness.write_config(port, certs, extra="max_client_cert_days = 30\n")

            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            user = harness.Client(port, user_crt, user_key, certs.ca_crt)

            with admin, user:
                resp = user.request(command="CREATE.ACCOUNT", target_account="EVIL_ACC")
                suite.check_eq("Non-admin CREATE.ACCOUNT is blocked", resp.get("code"), ADMIN_REQUIRED)

                resp = admin.request(command="CREATE.ACCOUNT", target_account="NEW_ACC")
                suite.check_eq("Admin CREATE.ACCOUNT is allowed", resp["status"], "OK")

                # Since #111 a file is authorized against the account it lives in
                # rather than against an administrative rank: the user is allowed
                # TEST_ACC, and could already rewrite every record in it.
                resp = user.request(command="CREATE.FILE", file="USERS_OWN_FILE", account=ACCOUNT)
                suite.check_eq("Non-admin CREATE.FILE in its own account is allowed", resp["status"], "OK")

                resp = user.request(command="CREATE.FILE", file="EVIL_FILE", account="NEW_ACC")
                suite.check_eq(
                    "Non-admin CREATE.FILE in another account is blocked", resp.get("code"), "ACCESS_DENIED"
                )

                resp = admin.request(command="CREATE.FILE", file="GOOD_FILE", account=ACCOUNT)
                suite.check_eq("Admin CREATE.FILE is allowed", resp["status"], "OK")

                resp = user.request(
                    command="AUTHORIZE.CONN", thumbprint="1234", name="evil_client", is_admin=True
                )
                suite.check_eq("Non-admin AUTHORIZE.CONN is blocked", resp.get("code"), ADMIN_REQUIRED)

                resp = admin.request(
                    command="AUTHORIZE.CONN",
                    thumbprint="5678",
                    name="new_client",
                    accounts_list=[ACCOUNT],
                )
                suite.check_eq("Admin AUTHORIZE.CONN is allowed", resp["status"], "OK")

                resp = user.request(command="DELETE.ACCOUNT", target_account=ACCOUNT)
                suite.check_eq("Non-admin DELETE.ACCOUNT is blocked", resp.get("code"), ADMIN_REQUIRED)

                # The user client is only authorised for TEST_ACC.
                resp = user.request(command="READ", file="GOOD_FILE", key="K1", account="NEW_ACC")
                suite.check(
                    "Non-admin cannot reach an account outside its allow list",
                    resp["status"] == "ERROR" and resp.get("code") == "ACCESS_DENIED",
                    resp.get("message", ""),
                )

                # ...but it may reach its own account, where the record simply does not exist.
                resp = user.request(command="READ", file="GOOD_FILE", key="K1", account=ACCOUNT)
                suite.check_eq(
                    "Non-admin may reach its own account", resp.get("code"), "RECORD_NOT_FOUND"
                )

                check_no_secret_leakage(suite, admin, workspace.path)
                check_capabilities(suite, admin, certs, port, user_crt, user_key)
                check_certificate_lifetimes(suite, admin)

            check_tls_floor(suite, port, user_crt, user_key, certs.ca_crt)
            check_ca_rotation(suite, certs, user_crt, user_key)
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
