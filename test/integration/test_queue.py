"""Queue files over the remote protocol: order, exclusive claims, redelivery,
dead letters, and what a hard kill of the server does to all of it.

The unit tests cover the engine directly. This suite covers the parts that only
exist once there is a server and more than one connection: that a claim is held
against the *client name a certificate is authorised as*, that a record survives
`SIGKILL` on the side of the line it was on, and that the dashboard's numbers -
depth, in flight, oldest age, dead letters - come back over the wire.
"""

import os
import signal
import sys
import time

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "QUEUE_ACC"
QUEUE = "JOBS"
DEAD = f"{QUEUE}.DEAD"

SETUP_COMMANDS = [
    f"CREATE.ACCOUNT {ACCOUNT}",
    f"LOGTO {ACCOUNT}",
    "SAVE",
]

# Nothing may flush because of time or batch size, so anything on disk after the
# kill was written because the queue is durable and not because a timer fired.
NO_AUTO_FLUSH = "flush_interval_ms = 3600000\nflush_max_pending = 1000000\n"

# The queue's own timeout, long enough that a claim outlives the several round
# trips the checks below make while holding it. It was one second, and the suite
# then raced its own timeout: on a slow or loaded machine a claim lapsed between
# being taken and being asked about, and a `PEEK` that expected to see the holder
# saw an unclaimed record instead. Nothing here is waiting for it to pass.
VISIBILITY = 30
# The one claim that is meant to lapse takes this as a per-claim override, at the
# point the redelivery is being tested and nowhere else.
LAPSE_SECONDS = 1
# Deliveries a record gets. Three, so the redelivery below has room to happen
# before the dead-lettering that follows it.
MAX_DELIVERIES = 3


def claim_of(resp):
    """The claim a queue command reports, or an empty one.

    Never indexed into directly: a key that is absent because a claim lapsed
    would raise, and the suite would report `'owner'` rather than what it saw.
    """
    return resp.get("claim") or {}


def claim_key(resp):
    return claim_of(resp).get("key")


def queue_stats(conn, file=QUEUE):
    resp = conn.request(command="FILE.STATS", file=file, account=ACCOUNT)
    return (resp.get("record") or {}).get("queue") or {}


def main():
    suite = harness.Suite("Queue", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("queue") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        alice_crt, alice_key, alice_tp = certs.client("alice")
        bob_crt, bob_key, bob_tp = certs.client("bob")
        port = harness.free_port()

        server = None
        try:
            # Seeded without TLS paths so the CLI does not auto-start a server on
            # the port the headless one needs.
            harness.write_config(port, certs=None, extra=NO_AUTO_FLUSH)
            harness.run_cli(
                [
                    f"AUTHORIZE.CONN {admin_tp} admin ADMIN",
                    f"AUTHORIZE.CONN {alice_tp} alice {ACCOUNT}",
                    f"AUTHORIZE.CONN {bob_tp} bob {ACCOUNT}",
                    *SETUP_COMMANDS,
                    "EXIT",
                ],
                args=["--account", "SYSTEM"],
            )

            harness.write_config(port, certs, extra=NO_AUTO_FLUSH)
            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)
            alice = harness.Client(port, alice_crt, alice_key, certs.ca_crt)
            bob = harness.Client(port, bob_crt, bob_key, certs.ca_crt)

            resp = admin.request(
                command="CREATE.FILE",
                file=QUEUE,
                account=ACCOUNT,
                queue=True,
                visibility_timeout=VISIBILITY,
                max_deliveries=MAX_DELIVERIES,
            )
            suite.check_eq("CREATE.FILE ... QUEUE", resp.get("status"), "OK")
            suite.check(
                "A queue is durable and carries its policy",
                resp["record"]["queue"] is True
                and resp["record"]["durable"] is True
                and resp["record"]["visibility_timeout_seconds"] == VISIBILITY
                and resp["record"]["max_deliveries"] == MAX_DELIVERIES,
                str(resp.get("record")),
            )

            # A payload is serialized through the file's dictionary, exactly as
            # READ serializes one, so a queue gets a dictionary like any other
            # file before anything is put on it.
            for name, definition in (("KIND", {"field": 1, "heading": "KIND"}), ("REF", {"field": 2, "heading": "REF"})):
                resp = admin.request(
                    command="SET.DICT", file=QUEUE, account=ACCOUNT, key=name, structured_data=definition
                )
                suite.check_eq(f"SET.DICT {name}", resp.get("status"), "OK")

            listed = admin.request(command="LIST.FILES", account=ACCOUNT)
            flags = dict(listed["results"])
            suite.check(
                "LIST.FILES reports the queue flag",
                flags[QUEUE]["queue"] is True and flags["DIR"]["queue"] is False,
                str(flags.get(QUEUE)),
            )

            # --- Order and exclusive claims -----------------------------------
            keys = []
            for index in range(5):
                resp = alice.request(
                    command="ENQUEUE", file=QUEUE, account=ACCOUNT, data=f"job^{index}"
                )
                keys.append(claim_key(resp))
            suite.check(
                "ENQUEUE mints ordered sequence keys",
                keys == sorted(keys) and all(len(key) == 20 for key in keys),
                str(keys[:2]),
            )

            first = alice.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT)
            second = bob.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT)
            suite.check(
                "DEQUEUE hands out in arrival order, one consumer each",
                claim_key(first) == keys[0] and claim_key(second) == keys[1],
                f"{claim_key(first)} then {claim_key(second)}",
            )
            suite.check(
                "A claim names the certificate's authorised client",
                claim_of(first).get("owner") == "alice" and claim_of(second).get("owner") == "bob",
                f"{claim_of(first).get('owner')} and {claim_of(second).get('owner')}",
            )
            suite.check(
                "The payload comes back beside the claim, through the dictionary",
                first.get("record") == {"kind": "job", "ref": "0"},
                str(first.get("record")),
            )

            # An object payload maps to attributes the way WRITE's does.
            resp = alice.request(
                command="ENQUEUE",
                file=QUEUE,
                account=ACCOUNT,
                structured_data={"kind": "invoice", "ref": "4471"},
            )
            structured_key = claim_key(resp)
            peeked = alice.request(command="PEEK", file=QUEUE, account=ACCOUNT, key=structured_key)
            suite.check(
                "ENQUEUE takes the same object form WRITE takes",
                peeked.get("record") == {"kind": "invoice", "ref": "4471"},
                str(peeked.get("record")),
            )
            suite.check_eq(
                "and it is consumed like any other",
                alice.request(command="DELETE", file=QUEUE, account=ACCOUNT, key=structured_key).get("status"),
                "OK",
            )

            # Only the holder may settle it.
            stolen = bob.request(command="ACK", file=QUEUE, account=ACCOUNT, key=keys[0])
            suite.check(
                "Another client cannot acknowledge someone else's claim",
                stolen.get("code") == "INVALID_REQUEST" and "alice" in (stolen.get("message") or ""),
                stolen.get("message"),
            )

            # PEEK sees the holder without taking anything.
            peeked = bob.request(command="PEEK", file=QUEUE, account=ACCOUNT, key=keys[0])
            suite.check(
                "PEEK shows who is holding a record and claims nothing",
                claim_of(peeked).get("owner") == "alice" and claim_of(peeked).get("deliveries") == 1,
                str(peeked.get("claim")),
            )

            stats = queue_stats(admin)
            suite.check(
                "FILE.STATS reports depth, in flight and age",
                stats["depth"] == 3
                and stats["in_flight"] == 2
                and stats["dead_letters"] == 0
                and isinstance(stats["oldest_unacknowledged_seconds"], int),
                str(stats),
            )

            # --- Redelivery on a lapsed claim ---------------------------------
            # Both claims go back first, so what follows is the only thing in
            # flight and the only thing timing matters to.
            alice.request(command="NACK", file=QUEUE, account=ACCOUNT, key=keys[0])
            bob.request(command="NACK", file=QUEUE, account=ACCOUNT, key=keys[1])

            # This one claim is short lived, rather than the whole queue running
            # on a one-second timeout: only the sleep below waits for it, so
            # there is no other check racing the clock.
            brief = alice.request(
                command="DEQUEUE",
                file=QUEUE,
                account=ACCOUNT,
                visibility_timeout=LAPSE_SECONDS,
            )
            suite.check(
                "A per-claim visibility overrides the queue's own",
                claim_key(brief) == keys[0]
                and claim_of(brief).get("expires", 0) - claim_of(brief).get("enqueued", 0)
                < VISIBILITY * 1000,
                str(claim_of(brief)),
            )

            time.sleep(LAPSE_SECONDS + 1.5)
            back = bob.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT)
            suite.check(
                "A claim that lapses is redelivered with its count raised",
                claim_key(back) == keys[0] and claim_of(back).get("deliveries") == 3,
                f"{claim_key(back)} on delivery {claim_of(back).get('deliveries')}",
            )
            others = [
                claim_key(alice.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT))
                for _ in range(4)
            ]
            suite.check(
                "It is redelivered once, not to everyone at once",
                keys[0] not in others,
                f"redelivered to {others}",
            )

            # --- Dead letters --------------------------------------------------
            # keys[0] has now had all the deliveries it gets, so giving it back
            # kills it rather than returning it to the queue.
            resp = bob.request(command="NACK", file=QUEUE, account=ACCOUNT, key=keys[0])
            suite.check_eq("NACK a record out of retries", resp.get("status"), "OK")
            dead = admin.request(command="PEEK", file=DEAD, account=ACCOUNT, key=keys[0])
            suite.check(
                "It lands in the dead-letter file with its failure count intact",
                dead.get("status") == "OK" and claim_of(dead).get("deliveries") == MAX_DELIVERIES,
                str(dead.get("claim") or dead.get("message")),
            )
            suite.check_eq(
                "FILE.STATS counts the dead letters",
                queue_stats(admin).get("dead_letters"),
                1,
            )

            # A dead-letter file is the end of the line. Draining it and failing
            # again must leave the record where an operator can still find it.
            replay = admin.request(command="DEQUEUE", file=DEAD, account=ACCOUNT)
            suite.check(
                "The dead-letter file is drained with the same commands",
                claim_key(replay) == keys[0] and claim_of(replay).get("deliveries") == MAX_DELIVERIES + 1,
                str(replay.get("claim") or replay.get("message")),
            )
            admin.request(command="NACK", file=DEAD, account=ACCOUNT, key=keys[0])
            listed = admin.request(command="LIST.FILES", account=ACCOUNT).get("keys") or []
            suite.check(
                "It does not get a dead-letter file of its own",
                f"{DEAD}.DEAD" not in listed,
                f"account holds {listed}",
            )
            suite.check(
                "The record stays on it, still counted",
                queue_stats(admin, DEAD).get("depth") == 1
                and queue_stats(admin, DEAD).get("dead_letters") == 0,
                str(queue_stats(admin, DEAD)),
            )

            # --- Hard kill ------------------------------------------------------
            # One record acknowledged, one claimed and left in flight, and the
            # rest still waiting - then SIGKILL, with the flush timers disabled
            # so only the queue's own durability can have saved anything.
            acked, in_flight = others[0], others[1]
            resp = alice.request(command="ACK", file=QUEUE, account=ACCOUNT, key=acked)
            suite.check_eq("ACK before the kill", resp.get("status"), "OK")
            still_held = alice.request(command="PEEK", file=QUEUE, account=ACCOUNT, key=in_flight)
            suite.check(
                "A record is claimed and unacknowledged when the server dies",
                claim_of(still_held).get("owner") == "alice",
                str(still_held.get("claim")),
            )
            expected_left = sorted(others[1:])

            for conn in (alice, bob, admin):
                conn.close()
            os.kill(server.pid, signal.SIGKILL)
            server.wait(timeout=10)

            server = harness.start_server()
            admin = harness.wait_for_client(port, admin_crt, admin_key, certs.ca_crt, process=server)

            stats = queue_stats(admin)
            suite.check(
                "Nothing claimed-and-acknowledged reappears, nothing claimed-and-lost disappears",
                stats["depth"] == len(expected_left) and stats["in_flight"] == 0,
                f"{stats['depth']} waiting and {stats['in_flight']} in flight, "
                f"expected {len(expected_left)} and 0",
            )
            drained = []
            while True:
                resp = admin.request(command="DEQUEUE", file=QUEUE, account=ACCOUNT)
                if resp.get("status") == "EMPTY":
                    break
                drained.append((claim_key(resp), claim_of(resp).get("deliveries")))
                admin.request(command="ACK", file=QUEUE, account=ACCOUNT, key=drained[-1][0])
            suite.check(
                "Every surviving record comes back, in order",
                [key for key, _ in drained] == expected_left,
                f"{[key for key, _ in drained]} against {expected_left}",
            )
            redelivered = dict(drained).get(in_flight)
            suite.check(
                "The released claim keeps the deliveries it had used",
                redelivered == 2,
                f"delivery {redelivered} after a restart, expected 2",
            )
            suite.check_eq(
                "The dead-letter file survives the kill too",
                queue_stats(admin, DEAD).get("depth"),
                1,
            )

            admin.close()
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the run
            suite.error("Queue suite", exc)
        finally:
            output = harness.stop(server)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
