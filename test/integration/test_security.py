"""Verifies that the headless server enforces admin privileges and per-account access."""

import os
import sys

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
