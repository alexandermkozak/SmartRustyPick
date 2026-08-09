"""Covers the headless server binary and a CLI attaching to the same storage directory."""

import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TEST_ACC"
FILE = "USERS"


def main():
    suite = harness.Suite("Headless", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("headless") as workspace:
        certs = harness.Certificates(workspace.path)
        client_crt, client_key, thumbprint = certs.client("client")
        port = harness.free_port()

        storage = os.path.join(workspace.path, "db_storage")
        account_dir = os.path.join(workspace.path, "TEST_ACC_DIR")
        os.makedirs(account_dir, exist_ok=True)

        server = None
        try:
            # Seed without TLS paths in the config so the CLI does not auto-start a
            # background server and hold on to the port the headless service needs.
            harness.write_config(port, certs=None)
            setup_output = harness.run_cli(
                [
                    f"AUTHORIZE.CONN {thumbprint} test_client ADMIN",
                    f"CREATE.ACCOUNT {ACCOUNT} {account_dir}",
                    f"LOGTO {ACCOUNT}",
                    "Y",  # answer the "DIR file missing. Create and populate?" prompt
                    f"CREATE.FILE {FILE}",
                    "SAVE",
                    "EXIT",
                ],
                args=["--account", "SYSTEM"],
            )
            suite.check(
                "CLI seeds the account and file",
                f"Account '{ACCOUNT}' created" in setup_output and f"[{FILE}] created" in setup_output,
                "see CLI output" if "Error" in setup_output else "",
            )

            harness.write_config(port, certs)
            server = harness.start_server()
            conn = harness.wait_for_client(port, client_crt, client_key, certs.ca_crt, process=server)

            with conn:
                # An admin client with no default account must name one explicitly.
                resp = conn.request(command="READ", file=FILE, key="K1")
                suite.check_eq(
                    "Headless server is reachable and answers the protocol",
                    resp.get("message"),
                    "Account not specified",
                )

                resp = conn.request(command="READ", file=FILE, key="K1", account=ACCOUNT)
                suite.check_eq(
                    "Headless server resolves the seeded account",
                    resp.get("message"),
                    "Record not found",
                )

                # A CLI started inside the account directory auto-logs into that account.
                cli_output = harness.run_cli(
                    [f"SET {FILE} K1 Hello", "SAVE", "EXIT"],
                    args=["-d", storage],
                    cwd=account_dir,
                )
                suite.check(
                    "CLI auto-logs in based on the current directory",
                    f"Auto-logged into account '{ACCOUNT}'" in cli_output,
                    "" if f"Auto-logged into account '{ACCOUNT}'" in cli_output else cli_output.strip().replace("\n", " / "),
                )

                # The running headless server must observe what the CLI just persisted.
                resp = conn.request(command="READ", file=FILE, key="K1", account=ACCOUNT)
                suite.check_eq(
                    "Headless server picks up records written by the CLI",
                    resp["status"],
                    "OK",
                )
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Headless suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
