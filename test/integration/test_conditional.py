"""Conditional writes and server-minted keys over the remote protocol.

The unit tests cover the conditions, the counter and what two racing threads do
to each other inside one process. This suite covers what only exists once there
is a real server on the far side of a TLS connection: that several *clients*
racing over the wire produce one winner and a `PRECONDITION_FAILED` for
everybody else, that a minted key comes back on the reply that stored the
record, and that both survive a restart of the server.
"""

import concurrent.futures
import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "COND_ACC"
LEDGER = "LEDGER"
EVENTS = "EVENTS"

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "SAVE",
]

RACERS = 6


def main():
    suite = harness.Suite("Conditional writes", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("conditional") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        port = harness.free_port()

        server = None
        clients = []
        try:
            harness.write_config(port, certs=None)
            harness.run_cli(
                [f"AUTHORIZE.CONN {admin_tp} admin ADMIN", *SETUP_COMMANDS, "EXIT"],
                args=["--account", "SYSTEM"],
            )

            harness.write_config(port, certs)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            clients.append(admin)

            resp = admin.request(command="CREATE.FILE", file=LEDGER, account=ACCOUNT)
            suite.check_eq(f"CREATE.FILE {LEDGER}", resp.get("status"), "OK")
            resp = admin.request(command="CREATE.FILE", file=EVENTS, account=ACCOUNT, autokey=True)
            suite.check_eq(f"CREATE.FILE {EVENTS} AUTOKEY", resp.get("status"), "OK")
            suite.check_eq("and the reply says it mints keys", (resp.get("record") or {}).get("autokey"), True)

            # --- if_absent -----------------------------------------------------
            resp = admin.request(
                command="WRITE", file=LEDGER, account=ACCOUNT, key="L-1", data="OPENED", if_absent=True
            )
            suite.check_eq("if_absent creates a key that is free", resp.get("status"), "OK")
            suite.check("and the reply carries a version", bool(resp.get("version")), str(resp.get("version")))
            opened = resp.get("version")

            resp = admin.request(
                command="WRITE", file=LEDGER, account=ACCOUNT, key="L-1", data="CLOBBERED", if_absent=True
            )
            suite.check_eq("if_absent refuses a key that is taken", resp.get("code"), "PRECONDITION_FAILED")
            # The version is how a client sees this without a dictionary on the
            # file: unchanged means the record is byte for byte the one that was
            # there, so the refused write really did write nothing.
            resp = admin.request(command="READ", file=LEDGER, account=ACCOUNT, key="L-1")
            suite.check_eq("and the refused write did not land", resp.get("version"), opened)

            # --- if_match ------------------------------------------------------
            stale = resp.get("version")
            resp = admin.request(command="WRITE", file=LEDGER, account=ACCOUNT, key="L-1", data="MOVED")
            suite.check_eq("An unconditional write still overwrites", resp.get("status"), "OK")
            current = resp.get("version")
            suite.check("and the version moved with the record", stale != current, f"{stale} -> {current}")

            resp = admin.request(
                command="WRITE", file=LEDGER, account=ACCOUNT, key="L-1", data="LOST UPDATE", if_match=stale
            )
            suite.check_eq("if_match refuses a stale version", resp.get("code"), "PRECONDITION_FAILED")
            resp = admin.request(
                command="WRITE", file=LEDGER, account=ACCOUNT, key="L-1", data="RETRIED", if_match=current
            )
            suite.check_eq("and accepts the one a READ just reported", resp.get("status"), "OK")

            resp = admin.request(command="DELETE", file=LEDGER, account=ACCOUNT, key="L-1", if_match=stale)
            suite.check_eq("DELETE takes the same condition", resp.get("code"), "PRECONDITION_FAILED")

            # --- minted keys ---------------------------------------------------
            minted = []
            for n in range(4):
                resp = admin.request(command="WRITE", file=EVENTS, account=ACCOUNT, data=f"EVENT^{n}")
                if resp.get("status") != "OK" or not resp.get("key"):
                    suite.check(f"Append {n} to {EVENTS}", False, str(resp))
                    break
                minted.append(resp["key"])
            suite.check_eq("Four keyless writes were all given a key", len(minted), 4)
            suite.check(
                "The minted keys are twenty digits and already in order",
                all(len(key) == 20 and key.isdigit() for key in minted) and minted == sorted(minted),
                str(minted),
            )
            resp = admin.request(command="WRITE", file=LEDGER, account=ACCOUNT, data="NO KEY")
            suite.check_eq("A keyless write to an ordinary file is refused", resp.get("code"), "INVALID_REQUEST")

            # --- several clients racing over the wire ---------------------------
            # One connection each, so this is real contention at the server
            # rather than one client's requests taking turns.
            racers = [
                harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
                for _ in range(RACERS)
            ]
            clients.extend(racers)

            def create(index):
                return racers[index].request(
                    command="WRITE",
                    file=LEDGER,
                    account=ACCOUNT,
                    key="CONTESTED",
                    data=f"RACER^{index}",
                    if_absent=True,
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=RACERS) as pool:
                outcomes = list(pool.map(create, range(RACERS)))
            won = [r for r in outcomes if r.get("status") == "OK"]
            collided = [r for r in outcomes if r.get("code") == "PRECONDITION_FAILED"]
            suite.check_eq("Exactly one client created the contested key", len(won), 1)
            suite.check_eq("and every other one was told it collided", len(collided), RACERS - 1)

            def append(index):
                return racers[index].request(
                    command="WRITE", file=EVENTS, account=ACCOUNT, data=f"CONCURRENT^{index}"
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=RACERS) as pool:
                appended = list(pool.map(append, range(RACERS)))
            keys = [r.get("key") for r in appended if r.get("status") == "OK"]
            suite.check_eq("Every concurrent append succeeded", len(keys), RACERS)
            suite.check_eq("with a distinct key each, and no record lost", len(set(keys)), RACERS)
            minted.extend(keys)

            # --- a restart keeps the flag and does not reuse a key ---------------
            for client in clients:
                client.close()
            clients = []
            harness.stop(server)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            clients.append(admin)

            resp = admin.request(command="FILE.STATS", file=EVENTS, account=ACCOUNT)
            suite.check_eq(
                "The autokey flag survived the restart",
                (resp.get("record") or {}).get("autokey"),
                True,
            )
            resp = admin.request(command="WRITE", file=EVENTS, account=ACCOUNT, data="AFTER RESTART")
            after = resp.get("key")
            suite.check_eq("A keyless write still works after the restart", resp.get("status"), "OK")
            suite.check(
                "and its key is one no record already has",
                after not in minted and after > max(minted),
                f"{after} against {max(minted)}",
            )

            resp = admin.request(command="READ", file=LEDGER, account=ACCOUNT, key="CONTESTED")
            suite.check_eq("The contested record survived too", resp.get("status"), "OK")
            suite.check(
                "and a version read after a restart is still usable",
                admin.request(
                    command="WRITE",
                    file=LEDGER,
                    account=ACCOUNT,
                    key="CONTESTED",
                    data="SETTLED",
                    if_match=resp.get("version"),
                ).get("status")
                == "OK",
                "",
            )

            for client in clients:
                client.close()
            clients = []
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Conditional writes suite", exc)
        finally:
            for client in clients:
                try:
                    client.close()
                except Exception:  # noqa: BLE001
                    pass
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
