"""Backup and restore against the real binaries.

The unit tests cover the format and the engine. What only exists once there is
a process is the part this suite is for: an archive taken from one **server**
and restored into a **second one** with its own storage directory, which is the
whole claim a backup makes and the one thing an in-process test cannot show.

Also here because they need two processes or a socket: an archive moved over the
connection by a client with no filesystem access to the host, and a damaged
archive proving it restores nothing rather than restoring some of itself.
"""

import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "ARC_ACC"
FILE = "LEDGER"
SCANS = "SCANS"

# Every mark byte and an embedded NUL: content an ordinary record cannot carry,
# so a directory record that survives this has survived the interesting case.
HOSTILE = bytes([0xFE, 0xFD, 0xFC, 0x00]) + b"PNG\x0a" + bytes(range(250, 256))

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    f"CREATE.FILE {FILE}",
    f"CREATE.FILE {SCANS} DIRECTORY",
    f"SET DICT {FILE} STATE 2^State^L^10",
    f"SET {FILE} L-1 OPENED^129.50",
    f"SET {FILE} L-2 CLOSED^88.00",
    f"SET {FILE} L-3 OPENED^12.25",
    f"CREATE.INDEX {FILE} STATE",
    "SAVE",
]


def ledger_rows(client, account):
    """Every record of the ledger, as `{key: display string}`."""
    response = client.request(command="QUERY", account=account, file=FILE)
    return {key: value for key, value in (response.get("results") or [])}


def main():
    suite = harness.Suite("Archive", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("archive") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        app_crt, app_key, app_tp = certs.client("app")
        port = harness.free_port()
        backups = os.path.join(workspace.path, "backups")
        archive_path = os.path.join(backups, "account.srp")

        server = None
        try:
            harness.write_config(port, certs=None)
            harness.run_cli(
                [
                    f"AUTHORIZE.CONN {admin_tp} admin ADMIN",
                    f"AUTHORIZE.CONN {app_tp} app {ACCOUNT}",
                    *SETUP_COMMANDS,
                    "EXIT",
                ],
                args=["--account", "SYSTEM"],
            )

            harness.write_config(port, certs)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)

            # A directory record, so the archive carries bytes that no record
            # framing could survive.
            stored = admin.put_bytes(file=SCANS, key="scan.bin", payload=HOSTILE, account=ACCOUNT)
            suite.check_eq("A directory record is in place to be backed up", stored.get("status"), "OK")

            before = ledger_rows(admin, ACCOUNT)

            # --- the export ------------------------------------------------
            response = admin.request(
                command="EXPORT.ACCOUNT", target_account=ACCOUNT, path=archive_path
            )
            suite.check_eq("EXPORT.ACCOUNT succeeds", response.get("status"), "OK")
            report = response.get("archive") or {}
            suite.check(
                "and reports what it captured",
                report.get("source") == f"account {ACCOUNT}" and report.get("records", 0) >= 4,
                str(report),
            )
            suite.check("The archive is on disk", os.path.exists(archive_path), archive_path)

            # --- it is admin only ------------------------------------------
            app = harness.Client(port, app_crt, app_key, certs.ca_crt)
            refused = app.request(
                command="EXPORT.ACCOUNT",
                target_account=ACCOUNT,
                path=os.path.join(backups, "sneaky.srp"),
            )
            suite.check_eq(
                "A non-admin cannot export an account it can otherwise read",
                refused.get("code"),
                "ADMIN_REQUIRED",
            )
            app.close()

            # --- a damaged archive restores nothing ------------------------
            damaged = os.path.join(backups, "damaged.srp")
            raw = open(archive_path, "rb").read()
            open(damaged, "wb").write(raw[: len(raw) - 6])
            response = admin.request(command="IMPORT", path=damaged, target_account="TRUNCATED")
            suite.check_eq("A truncated archive is refused", response.get("code"), "INVALID_REQUEST")
            listed = admin.request(command="LIST.ACCOUNTS")
            names = [entry.get("name") for _, entry in (listed.get("results") or [])]
            suite.check(
                "and created no account on the way to refusing",
                "TRUNCATED" not in names,
                str(names),
            )

            flipped = os.path.join(backups, "flipped.srp")
            altered = bytearray(raw)
            altered[len(altered) // 2] ^= 0x20
            open(flipped, "wb").write(bytes(altered))
            response = admin.request(command="IMPORT", path=flipped, target_account="ALTERED")
            suite.check_eq("An altered archive is refused", response.get("code"), "INVALID_REQUEST")

            # --- verify, then restore beside the original ------------------
            response = admin.request(
                command="IMPORT", path=archive_path, target_account="ARC_COPY", dry_run=True
            )
            suite.check_eq("A dry run reports", response.get("status"), "OK")
            suite.check_eq("and says it wrote nothing", (response.get("archive") or {}).get("dryRun"), True)

            response = admin.request(command="IMPORT", path=archive_path, target_account="ARC_COPY")
            suite.check_eq("IMPORT ... AS restores beside the original", response.get("status"), "OK")
            suite.check_eq(
                "The restored ledger matches the source",
                ledger_rows(admin, "ARC_COPY"),
                before,
            )
            _, body = admin.get_bytes(file=SCANS, key="scan.bin", account="ARC_COPY")
            suite.check_eq("and the directory record came back byte for byte", body, HOSTILE)

            # --- an existing file needs OVERWRITE --------------------------
            response = admin.request(command="IMPORT", path=archive_path, target_account="ARC_COPY")
            suite.check_eq(
                "Restoring onto existing files is refused without OVERWRITE",
                response.get("code"),
                "INVALID_REQUEST",
            )
            response = admin.request(
                command="IMPORT", path=archive_path, target_account="ARC_COPY", overwrite=True
            )
            suite.check_eq("and accepted with it", response.get("status"), "OK")

            # --- over the connection, with no host access ------------------
            response, streamed = admin.export_bytes(account=ACCOUNT)
            suite.check_eq("EXPORT.BYTES announces a length", response.get("status"), "OK")
            suite.check(
                "and sends exactly that many bytes",
                streamed is not None and len(streamed) == response.get("length"),
                f"{response.get('length')} announced, {len(streamed or b'')} received",
            )
            response = admin.import_bytes(streamed, account="ARC_STREAMED")
            suite.check_eq("IMPORT.BYTES restores what EXPORT.BYTES sent", response.get("status"), "OK")
            suite.check_eq(
                "with the same records",
                ledger_rows(admin, "ARC_STREAMED"),
                before,
            )
            _, body = admin.get_bytes(file=SCANS, key="scan.bin", account="ARC_STREAMED")
            suite.check_eq("and the same bytes in the directory file", body, HOSTILE)

            # The session is still usable, which is the thing a mis-framed body
            # would break and no assertion above would notice.
            suite.check_eq(
                "The connection survives an archive transfer",
                admin.request(command="READ", account=ACCOUNT, file=FILE, key="L-1").get("status"),
                "OK",
            )
            admin.close()

        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Archive suite", exc)
        finally:
            if server is not None:
                output = harness.stop(server)
                if suite.failures:
                    print("--- server output ---")
                    print(output)

        # --- the claim a backup actually makes -----------------------------
        # A second server, with its own storage directory, restoring the archive
        # the first one wrote. Nothing in-process can show this: the hashfile
        # layout is not carried, so the records are rehashed into a deployment
        # that has never seen them.
        try:
            second = os.path.join(workspace.path, "second")
            os.makedirs(second, exist_ok=True)
            second_port = harness.free_port()

            previous = os.getcwd()
            os.chdir(second)
            try:
                second_certs = harness.Certificates(second)
                srv_crt, srv_key, srv_tp = second_certs.client("admin2")
                harness.write_config(second_port, certs=None)
                harness.run_cli(
                    [f"AUTHORIZE.CONN {srv_tp} admin2 ADMIN", "EXIT"],
                    args=["--account", "SYSTEM"],
                )
                harness.write_config(second_port, second_certs)
                other = harness.start_server()
                try:
                    client = harness.wait_for_client(
                        second_port, srv_crt, srv_key, second_certs.ca_crt, process=other
                    )
                    response = client.request(command="IMPORT", path=archive_path)
                    suite.check_eq(
                        "A second server with its own storage restores the archive",
                        response.get("status"),
                        "OK",
                    )
                    suite.check_eq(
                        "with every record intact",
                        ledger_rows(client, ACCOUNT),
                        before,
                    )
                    _, body = client.get_bytes(file=SCANS, key="scan.bin", account=ACCOUNT)
                    suite.check_eq(
                        "including the directory record it never held",
                        body,
                        HOSTILE,
                    )
                    client.close()
                finally:
                    output = harness.stop(other)
                    if suite.failures:
                        print("--- second server output ---")
                        print(output)
            finally:
                os.chdir(previous)
        except Exception as exc:  # noqa: BLE001
            suite.error("Archive restore into a second server", exc)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
