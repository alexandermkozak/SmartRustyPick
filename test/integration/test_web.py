"""Covers the web management dashboard: its bootstrap, its API and its access control.

The dashboard is only trustworthy if two things hold. It must be an ordinary
remote client - so anything it does is subject to the same authorization as any
other certificate holder - and its own certificate must not outlive the boot
that issued it. Both are checked here against the real binaries.
"""

import json
import os
import re
import shutil
import ssl
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")))

import harness

ACCOUNT = "TEST_ACC"
FILE = "USERS"
TOKEN = "integration-token-0123456789abcdef"
DASHBOARD_CLIENT = "WEB.DASHBOARD"


def seed_database(thumbprint):
    """Create the account, a file and two records through the CLI."""
    output = harness.run_cli(
        [
            f"AUTHORIZE.CONN {thumbprint} admin ADMIN",
            f"CREATE.ACCOUNT {ACCOUNT}",
            f"LOGTO {ACCOUNT}",
            f"CREATE.FILE {FILE}",
            f"SET {FILE} K1 Alice]alice@example.com",
            f"SET {FILE} K2 Bob]bob@example.com",
            "SAVE",
            "EXIT",
        ],
        args=["--account", "SYSTEM"],
    )
    if "Error" in output:
        raise RuntimeError(f"CLI setup failed:\n{output}")


def client_entry(dashboard, name):
    """The `[name, info]` pair for one authorized client, or None."""
    _, payload, _ = dashboard.call("/api/clients")
    for entry_name, info in (payload or {}).get("results") or []:
        if entry_name == name:
            return info
    return None


def protocol_call(port, certificate, private_key, ca, request):
    """One request over the remote protocol with a specific certificate."""
    with harness.Client(port, certificate, private_key, ca) as client:
        return client.request(**request)


def main():
    suite = harness.Suite("Web Dashboard", "integration_results.md")
    harness.require_binaries(harness.CLI_BIN, harness.SERVER_BIN)

    with harness.Workspace("web") as workspace:
        certs = harness.Certificates(workspace.path)
        admin_crt, admin_key, admin_tp = certs.client("admin")
        port = harness.free_port()
        web_port = harness.free_port()
        dashboard = harness.Dashboard(web_port, TOKEN)

        server = None
        restarted = None
        try:
            harness.write_config(port, certs=None)
            seed_database(admin_tp)
            harness.write_config(port, certs, web_port=web_port, web_token=TOKEN)

            server = harness.start_server()
            harness.wait_for_dashboard(web_port, process=server)

            # --- bootstrap -------------------------------------------------
            status, payload, _ = dashboard.call("/health", token=None)
            suite.check("Health check answers without a token", status == 200 and payload.get("status") == "ok")

            entry = client_entry(dashboard, DASHBOARD_CLIENT)
            suite.check(
                "Dashboard authorized itself on startup",
                entry is not None and entry.get("is_admin") is True,
                "" if entry else "WEB.DASHBOARD is not in the authorized client list",
            )
            first_thumbprint = (entry or {}).get("thumbprint")

            # --- access control --------------------------------------------
            status, _, _ = dashboard.call("/api/stats", token=None)
            suite.check_eq("API refuses a request with no token", status, 401)

            status, _, _ = dashboard.call("/api/stats", token="wrong-token")
            suite.check_eq("API refuses a wrong token", status, 401)

            status, _, headers = dashboard.call(f"/?token={TOKEN}", token=None)
            cookie_set = status == 200 and "srp_token" in headers.get("Set-Cookie", "")
            suite.check(
                "The page hands the browser a session cookie",
                cookie_set,
                "" if cookie_set else headers.get("Set-Cookie", "no Set-Cookie header"),
            )

            status, _, _ = dashboard.call("/", token=None)
            suite.check_eq("The page itself needs a token too", status, 401)

            # The page is a built Vue bundle: if the server and the bundle
            # disagree about a filename, the browser gets a blank screen and
            # nothing else in this suite would notice.
            _, page, _ = dashboard.call("/")
            referenced = sorted(set(re.findall(r'/dist/[A-Za-z0-9._-]+', page if isinstance(page, str) else "")))
            served = {}
            for asset in referenced:
                status, _, headers = dashboard.call(asset)
                served[asset] = (status, headers.get("Content-Type", ""))
            suite.check(
                "Every asset the page references is served",
                bool(referenced) and all(status == 200 for status, _ in served.values()),
                "" if referenced else "the page references no bundle assets",
            )
            suite.check(
                "The bundle is served with usable content types",
                served.get("/dist/app.js", (0, ""))[1].startswith("application/javascript")
                and served.get("/dist/app.css", (0, ""))[1].startswith("text/css"),
                json.dumps(served),
            )

            # --- server view -----------------------------------------------
            status, payload, _ = dashboard.call("/api/stats")
            stats = (payload or {}).get("record") or {}
            suite.check_eq("Statistics report the protocol listener", stats.get("listen_addr"), f"127.0.0.1:{port}")
            connections = stats.get("active_connections", [])
            visible = any(c.get("client_name") == DASHBOARD_CLIENT for c in connections)
            suite.check(
                "The dashboard's own session is visible as an active connection",
                visible,
                "" if visible else json.dumps(connections),
            )

            # --- accounts and files ----------------------------------------
            status, payload, _ = dashboard.call("/api/accounts")
            accounts = {name: info for name, info in (payload or {}).get("results") or []}
            listed = ACCOUNT in accounts and accounts[ACCOUNT]["file_count"] > 0
            suite.check(
                "Every account is listed with its file count",
                listed,
                "" if listed else json.dumps(sorted(accounts)),
            )
            rollup = accounts.get(ACCOUNT, {})
            suite.check(
                "...and a health roll-up, so a problem account is findable from the list",
                (rollup.get("health") or {}).get("verdict") in ("good", "watch", "act")
                and isinstance(rollup.get("index_count"), int)
                and isinstance(rollup.get("unhealthy_files"), int),
                json.dumps(rollup),
            )

            status, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files")
            files = (payload or {}).get("keys") or []
            suite.check(
                "Files of an account are listed",
                FILE in files,
                "" if FILE in files else json.dumps(files),
            )
            per_file = {name: info for name, info in (payload or {}).get("results") or []}
            suite.check(
                "...each with a verdict, without opening any of them",
                per_file.get(FILE, {}).get("health") in ("good", "watch", "act")
                and isinstance(per_file.get(FILE, {}).get("health_reasons"), list),
                json.dumps(per_file.get(FILE)),
            )

            status, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files/{FILE}")
            file_stats = (payload or {}).get("record") or {}
            suite.check_eq("File statistics count the records", file_stats.get("record_count"), 2)

            # The derived measures: what the format already knew and never said.
            groups = file_stats.get("group_records") or {}
            counted = groups.get("groups", 0) - groups.get("unreadable", 0)
            suite.check(
                "Records per group are reported, read from the group trailers",
                groups.get("groups") == file_stats.get("modulus")
                and sum(bucket["groups"] for bucket in groups.get("buckets") or []) == counted
                and groups.get("mean", -1) * groups.get("groups", 0) == file_stats.get("record_count"),
                json.dumps(groups),
            )
            suite.check(
                "Layout and headroom say what the file will do next",
                isinstance(file_stats.get("load_factor"), (int, float))
                and isinstance(file_stats.get("records_until_growth"), int)
                and isinstance(file_stats.get("skew"), (int, float))
                and isinstance(file_stats.get("group_bytes"), int),
                json.dumps({k: file_stats.get(k) for k in
                            ("load_factor", "records_until_growth", "skew", "group_bytes")}),
            )

            health = file_stats.get("health") or {}
            measures = {m.get("id"): m for m in health.get("measures") or []}
            suite.check(
                "Every measure carries a verdict and the rule that produced it",
                health.get("verdict") in ("good", "watch", "act")
                and {"format", "checksums", "skew", "load_factor"} <= set(measures)
                and all(
                    m.get("verdict") in ("good", "watch", "act") and m.get("threshold") and m.get("detail")
                    for m in measures.values()
                ),
                json.dumps(sorted(measures)),
            )
            # `legacy` and missing checksums used to render as neutral rows
            # reading "no"; both mean the file is not yet protected by the
            # current format's guarantees, so both are judged.
            suite.check_eq(
                "A current file's format and checksums are reported as healthy",
                [measures["format"]["verdict"], measures["checksums"]["verdict"]],
                ["good", "good"],
            )

            leaked = "Alice" in json.dumps(payload)
            suite.check(
                "File statistics carry no record contents",
                not leaked,
                "a record value leaked into the statistics payload" if leaked else "",
            )

            status, _, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files/NO_SUCH_FILE")
            suite.check_eq("An unknown file is a 404", status, 404)

            # Durability is the one thing about a file the dashboard changes.
            status, payload, _ = dashboard.call(
                f"/api/accounts/{ACCOUNT}/files/{FILE}", method="POST", payload={"durable": True}
            )
            suite.check_eq("A file can be promoted to durable writes", status, 200)
            suite.check_eq(
                "The promotion reports the new setting",
                ((payload or {}).get("record") or {}).get("durable"),
                True,
            )

            status, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files")
            durability = {name: info.get("durable") for name, info in (payload or {}).get("results") or []}
            suite.check_eq("The file listing shows the file as durable", durability.get(FILE), True)

            status, payload, _ = dashboard.call(
                f"/api/accounts/{ACCOUNT}/files/{FILE}", method="POST", payload={"durable": False}
            )
            suite.check_eq("A file can be returned to buffered writes", status, 200)
            status, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files")
            durability = {name: info.get("durable") for name, info in (payload or {}).get("results") or []}
            suite.check_eq("The listing follows it back", durability.get(FILE), False)

            status, payload, _ = dashboard.call(
                f"/api/accounts/{ACCOUNT}/files/{FILE}", method="POST", payload={}
            )
            suite.check_eq("A change with no flag is refused", status, 400)

            # --- creating and dropping accounts and files -------------------
            status, _, _ = dashboard.call("/api/accounts", method="POST", payload={"name": "WEB_MADE"})
            suite.check_eq("An account can be created", status, 200)
            _, payload, _ = dashboard.call("/api/accounts")
            made = [name for name, _ in (payload or {}).get("results") or []]
            suite.check(
                "The new account is listed straight away",
                "WEB_MADE" in made,
                "" if "WEB_MADE" in made else json.dumps(made),
            )

            # An account describes its own files in DIR. Created over the
            # protocol it used to have none until somebody logged into it from
            # the CLI and answered a prompt.
            _, payload, _ = dashboard.call("/api/accounts/WEB_MADE/files")
            suite.check_eq(
                "A new account comes with its DIR file", (payload or {}).get("keys"), ["DIR"]
            )

            status, _, _ = dashboard.call(
                "/api/accounts/WEB_MADE/files", method="POST", payload={"name": "LEDGER", "durable": True}
            )
            suite.check_eq("A file can be created in it", status, 200)
            _, payload, _ = dashboard.call("/api/accounts/WEB_MADE/files")
            durability = {name: info.get("durable") for name, info in (payload or {}).get("results") or []}
            suite.check_eq("The file is created durable when asked", durability.get("LEDGER"), True)
            suite.check_eq(
                "The new file joins the account's listing", (payload or {}).get("keys"), ["DIR", "LEDGER"]
            )

            status, _, _ = dashboard.call("/api/accounts/WEB_MADE/files/LEDGER", method="DELETE")
            _, payload, _ = dashboard.call("/api/accounts/WEB_MADE/files")
            gone = "LEDGER" not in ((payload or {}).get("keys") or [])
            suite.check("A file can be dropped", status == 200 and gone)

            status, _, _ = dashboard.call("/api/accounts/WEB_MADE", method="DELETE")
            _, payload, _ = dashboard.call("/api/accounts")
            remaining = [name for name, _ in (payload or {}).get("results") or []]
            suite.check(
                "An account can be dropped",
                status == 200 and "WEB_MADE" not in remaining,
                "" if "WEB_MADE" not in remaining else json.dumps(remaining),
            )

            # The demo fixture is the same endpoint with a flag, and the point
            # of it is that the account arrives with something in it.
            status, payload, _ = dashboard.call(
                "/api/accounts", method="POST", payload={"name": "WEB_DEMO", "demo": True}
            )
            created = (payload or {}).get("record") or {}
            suite.check(
                "A demo account can be created",
                status == 200 and created.get("files") == ["DIR", "JOBS", "PRODUCTS", "USERS"],
                json.dumps(created),
            )

            _, payload, _ = dashboard.call("/api/accounts")
            demo = {name: info for name, info in (payload or {}).get("results") or []}
            suite.check(
                "The demo account is listed with the records it came with",
                demo.get("WEB_DEMO", {}).get("record_count", 0) > 0,
                json.dumps(demo.get("WEB_DEMO", {})),
            )

            # Populated means dictionaries too, which is what makes it worth
            # opening: the dashboard can show them the moment it is created.
            _, payload, _ = dashboard.call("/api/accounts/WEB_DEMO/files/PRODUCTS/dictionary")
            entries = {name: info for name, info in (payload or {}).get("results") or []}
            suite.check_eq(
                "Its files come with their dictionaries", entries.get("PRICE", {}).get("conversion"), "MD2"
            )

            dashboard.call("/api/accounts/WEB_DEMO", method="DELETE")

            # SYSTEM holds the account registry and the authorized clients, so
            # the database refuses to drop it however it is asked.
            status, payload, _ = dashboard.call("/api/accounts/SYSTEM", method="DELETE")
            suite.check(
                "Dropping SYSTEM is refused",
                status >= 400 and "SYSTEM" in json.dumps(payload),
                json.dumps(payload),
            )

            # --- dictionary maintenance -------------------------------------
            dict_path = f"/api/accounts/{ACCOUNT}/files/{FILE}/dictionary"

            status, payload, _ = dashboard.call(
                dict_path, method="POST", payload={"name": "NAME", "field": 1, "width": 20}
            )
            stored = (payload or {}).get("record") or {}
            suite.check(
                "A dictionary entry can be created, with the defaults filled in",
                status == 200 and stored.get("definition") == "1^NAME^L^20",
                json.dumps(stored),
            )

            status, payload, _ = dashboard.call(
                dict_path,
                method="POST",
                payload={"name": "PRICE", "field": 3, "justification": "R", "width": 12, "conversion": "MD2"},
            )
            stored = (payload or {}).get("record") or {}
            suite.check(
                "An entry carries its justification and conversion",
                status == 200 and stored.get("definition") == "3^PRICE^R^12^^^^MD2",
                json.dumps(stored),
            )

            # An association is recorded on the dependent, naming its controller,
            # and a group that names no tier pairs value for value.
            status, payload, _ = dashboard.call(
                dict_path,
                method="POST",
                payload={"name": "PRICE.DATE", "field": 4, "association": "PRICE"},
            )
            stored = (payload or {}).get("record") or {}
            suite.check(
                "An entry can be associated with a controlling field",
                status == 200 and stored.get("definition") == "4^PRICE.DATE^L^10^PRICE^V",
                json.dumps(stored),
            )

            status, _, _ = dashboard.call(
                dict_path,
                method="POST",
                payload={"name": "PRICE.NOTE", "field": 5, "associationDepth": "S"},
            )
            suite.check_eq("A tier without a controlling field is refused", status, 400)

            status, payload, _ = dashboard.call(dict_path)
            listed = [name for name, _ in (payload or {}).get("results") or []]
            entries = {name: info for name, info in (payload or {}).get("results") or []}
            suite.check_eq("Entries come back in attribute order", listed, ["NAME", "PRICE", "PRICE.DATE"])
            suite.check_eq(
                "...and an associated entry names its controller",
                [
                    entries.get("PRICE.DATE", {}).get("association"),
                    entries.get("PRICE.DATE", {}).get("associationDepth"),
                ],
                ["PRICE", "V"],
            )
            suite.check_eq("An entry is listed with its attributes", entries.get("PRICE", {}).get("field"), 3)
            suite.check_eq("...and its conversion", entries.get("PRICE", {}).get("conversion"), "MD2")

            # The dictionary the dashboard wrote is the one the engine reads
            # with: a record that came back as nothing but a key now has a field.
            response = protocol_call(
                port,
                admin_crt,
                admin_key,
                certs.ca_crt,
                {"command": "READ", "account": ACCOUNT, "file": FILE, "key": "K1"},
            )
            record = response.get("record") or {}
            suite.check(
                "A record read afterwards is named by the new dictionary",
                "name" in record,
                json.dumps(record),
            )

            status, _, _ = dashboard.call(dict_path, method="POST", payload={"name": "PRICE", "field": 0})
            suite.check_eq("A definition no query could use is refused", status, 400)

            dashboard.call(f"{dict_path}/PRICE.DATE", method="DELETE")
            status, _, _ = dashboard.call(f"{dict_path}/PRICE", method="DELETE")
            _, payload, _ = dashboard.call(dict_path)
            left = [name for name, _ in (payload or {}).get("results") or []]
            suite.check(
                "A dictionary entry can be deleted, leaving the rest",
                status == 200 and left == ["NAME"],
                json.dumps(left),
            )

            # --- index maintenance ------------------------------------------
            # NAME is attribute 1, and both records hold two values there, so an
            # index on it has four values over two records - which is also what
            # proves a multivalued field indexes each of its values.
            index_path = f"/api/accounts/{ACCOUNT}/files/{FILE}/indexes"

            status, payload, _ = dashboard.call(index_path)
            suite.check_eq("A file starts with no indexes", (payload or {}).get("count"), 0)

            status, payload, _ = dashboard.call(index_path, method="POST", payload={"field": "NAME"})
            built = (payload or {}).get("record") or {}
            suite.check(
                "An index can be created on a dictionary field",
                status == 200
                and built.get("field") == "NAME"
                and built.get("attribute") == 1
                and built.get("values") == 4
                and built.get("postings") == 4
                and built.get("stale") is False,
                json.dumps(built),
            )

            status, payload, _ = dashboard.call(index_path)
            listed = {name: info for name, info in (payload or {}).get("results") or []}
            suite.check_eq("It is listed with its statistics", listed.get("NAME", {}).get("values"), 4)
            suite.check_eq("...and the file it belongs to", listed.get("NAME", {}).get("file"), FILE)
            suite.check(
                "...and a verdict on all of it, rather than counts to interpret",
                (listed.get("NAME", {}).get("health") or {}).get("verdict")
                in ("good", "watch", "act"),
                json.dumps(listed.get("NAME", {}).get("health")),
            )
            usage = listed.get("NAME", {}).get("usage") or {}
            suite.check(
                "...and what the read path has asked of it",
                set(usage) == {"lookups", "candidates", "matched", "measured_lookups", "excluded_lookups"},
                json.dumps(usage),
            )

            # Every index in the account, without naming a file: the view that
            # makes a badly shaped index findable without opening each file.
            status, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/indexes")
            keys = (payload or {}).get("keys") or []
            suite.check(
                "Every index in the account is listed by file and field",
                status == 200 and f"{FILE}/NAME" in keys,
                json.dumps(keys),
            )

            _, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files/{FILE}")
            record = (payload or {}).get("record") or {}
            reported = record.get("indexes") or []
            suite.check_eq(
                "File statistics report the indexes too",
                [entry.get("field") for entry in reported],
                ["NAME"],
            )

            # The index is an optimisation, so the answer must not change.
            response = protocol_call(
                port,
                admin_crt,
                admin_key,
                certs.ca_crt,
                {
                    "command": "QUERY",
                    "account": ACCOUNT,
                    "file": FILE,
                    "query_string": 'WITH NAME = "Alice"',
                },
            )
            matched = [key for key, _ in response.get("results") or []]
            suite.check_eq("A query resolves through the index to the same answer", matched, ["K1"])

            status, payload, _ = dashboard.call(index_path, method="POST", payload={"field": "NOSUCH"})
            suite.check_eq("A field the dictionary does not define is refused", status, 400)

            # --- the value histogram, and acting on it -----------------------
            status, payload, _ = dashboard.call(f"{index_path}/NAME?limit=5")
            report = (payload or {}).get("record") or {}
            values = report.get("top_values") or []
            suite.check(
                "One index's values can be read, largest first",
                status == 200
                and report.get("values_available") is True
                and len(values) > 0
                and all(values[i]["keys"] >= values[i + 1]["keys"] for i in range(len(values) - 1)),
                json.dumps(report)[:300],
            )

            # The answers must not change when a value stops being indexed. The
            # baseline is taken first, because it is the thing being preserved.
            def names_matching(value):
                answer = protocol_call(
                    port,
                    admin_crt,
                    admin_key,
                    certs.ca_crt,
                    {
                        "command": "QUERY",
                        "account": ACCOUNT,
                        "file": FILE,
                        "query_string": f'WITH NAME = "{value}"',
                    },
                )
                return [key for key, _ in answer.get("results") or []]

            dominant = values[0]["value"] if values else "Alice"
            before = names_matching(dominant)

            status, payload, _ = dashboard.call(
                f"{index_path}/NAME/exclude", method="POST", payload={"values": [dominant]}
            )
            trimmed = (payload or {}).get("record") or {}
            suite.check(
                "A value can be excluded from an index, which rebuilds it",
                status == 200 and trimmed.get("excluded") == [dominant] and trimmed.get("stale") is False,
                json.dumps(trimmed)[:300],
            )
            suite.check_eq(
                "...and the index no longer holds it",
                trimmed.get("values"),
                built.get("values") - 1,
            )
            # The whole risk of the feature: a lookup on an excluded value must
            # fall back to a scan rather than trusting an empty posting list.
            suite.check_eq(
                "A query for an excluded value answers exactly as it did before",
                names_matching(dominant),
                before,
            )

            status, payload, _ = dashboard.call(
                f"{index_path}/NAME/exclude", method="POST", payload={"values": []}
            )
            restored = (payload or {}).get("record") or {}
            suite.check(
                "Clearing the exclusions puts the index back",
                status == 200 and restored.get("excluded") == [] and restored.get("values") == built.get("values"),
                json.dumps(restored)[:200],
            )

            status, payload, _ = dashboard.call(f"{index_path}/NAME/rebuild", method="POST")
            rebuilt = (payload or {}).get("record") or {}
            suite.check(
                "An index can be rebuilt",
                status == 200 and rebuilt.get("postings") == 4 and rebuilt.get("stale") is False,
                json.dumps(rebuilt),
            )

            status, _, _ = dashboard.call(f"{index_path}/NAME", method="DELETE")
            _, payload, _ = dashboard.call(index_path)
            suite.check(
                "An index can be dropped",
                status == 200 and (payload or {}).get("count") == 0,
                json.dumps(payload),
            )

            # And the records it described are still there afterwards.
            _, payload, _ = dashboard.call(f"/api/accounts/{ACCOUNT}/files/{FILE}")
            suite.check_eq(
                "Dropping an index leaves the records alone",
                ((payload or {}).get("record") or {}).get("record_count"),
                2,
            )

            # --- certificates ----------------------------------------------
            status, payload, _ = dashboard.call(
                "/api/certificates", method="POST", payload={"common_name": "dash-issued", "accounts": [ACCOUNT]}
            )
            issued = (payload or {}).get("record") or {}
            issued_ok = (
                status == 200
                and issued.get("private_key_pem", "").startswith("-----BEGIN")
                and len(issued.get("thumbprint", "")) == 64
            )
            suite.check(
                "A certificate is issued with its key and thumbprint",
                issued_ok,
                "" if issued_ok else json.dumps(payload)[:200],
            )

            status, _, _ = dashboard.call(
                "/api/certificates", method="POST", payload={"common_name": "no-accounts"}
            )
            suite.check_eq("A certificate with no accounts and no admin rights is refused", status, 400)

            issued_crt = os.path.join(workspace.path, "dash-issued.crt")
            issued_key = os.path.join(workspace.path, "dash-issued.key")
            response = protocol_call(
                port, issued_crt, issued_key, certs.ca_crt, {"command": "READ", "file": FILE, "key": "K1"}
            )
            suite.check_eq("The issued certificate can connect and read", response["status"], "OK")

            # --- authorization management -----------------------------------
            status, _, _ = dashboard.call(
                "/api/clients",
                method="POST",
                payload={"name": "manual", "thumbprint": "a" * 64, "accounts": [ACCOUNT]},
            )
            suite.check_eq("A known thumbprint can be authorized", status, 200)
            entry = client_entry(dashboard, "manual")
            suite.check_eq("The new client carries its allowed account", (entry or {}).get("accounts"), [ACCOUNT])

            dashboard.call("/api/clients/manual/accounts", method="POST", payload={"accounts": ["SYSTEM"]})
            entry = client_entry(dashboard, "manual")
            suite.check_eq("An account can be added to a client", (entry or {}).get("accounts"), [ACCOUNT, "SYSTEM"])

            dashboard.call(
                "/api/clients/manual/accounts", method="POST", payload={"accounts": ["SYSTEM"], "remove": True}
            )
            entry = client_entry(dashboard, "manual")
            suite.check_eq("An account can be removed again", (entry or {}).get("accounts"), [ACCOUNT])

            status, _, _ = dashboard.call("/api/clients/manual", method="DELETE")
            suite.check(
                "A client can be revoked",
                status == 200 and client_entry(dashboard, "manual") is None,
                "the revoked client is still listed" if client_entry(dashboard, "manual") else "",
            )

            # Revoking is not bookkeeping: the certificate must stop working.
            status, _, _ = dashboard.call("/api/clients/dash-issued", method="DELETE")
            rejected = False
            try:
                protocol_call(
                    port, issued_crt, issued_key, certs.ca_crt, {"command": "READ", "file": FILE, "key": "K1"}
                )
            except (ssl.SSLError, OSError, ConnectionError):
                rejected = True
            suite.check("A revoked certificate is refused by the server", rejected)

            # --- certificate rotation across a restart ----------------------
            old_crt = os.path.join(workspace.path, "old-dashboard.crt")
            old_key = os.path.join(workspace.path, "old-dashboard.key")
            shutil.copyfile(os.path.join(workspace.path, "web-dashboard.crt"), old_crt)
            shutil.copyfile(os.path.join(workspace.path, "web-dashboard.key"), old_key)

            first_output = harness.stop(server)
            server = None
            suite.check(
                "Startup prints the dashboard address and its token",
                f"http://127.0.0.1:{web_port}/?token={TOKEN}" in first_output,
                "" if TOKEN in first_output else first_output.strip().replace("\n", " / ")[-200:],
            )

            restarted = harness.start_server()
            harness.wait_for_dashboard(web_port, process=restarted)

            entry = client_entry(dashboard, DASHBOARD_CLIENT)
            rotated = entry is not None and entry.get("thumbprint") != first_thumbprint
            suite.check(
                "The dashboard certificate is reissued on the next boot",
                rotated,
                "" if rotated else f"thumbprint unchanged: {first_thumbprint}",
            )

            stale_rejected = False
            try:
                protocol_call(port, old_crt, old_key, certs.ca_crt, {"command": "SERVER.STATS"})
            except (ssl.SSLError, OSError, ConnectionError):
                stale_rejected = True
            suite.check("The previous boot's dashboard certificate no longer connects", stale_rejected)
        except Exception as exc:  # noqa: BLE001 - report instead of aborting the whole run
            suite.error("Web dashboard suite", exc)
        finally:
            output = harness.stop(server) + harness.stop(restarted)
            if suite.failures:
                print("--- server output ---")
                print(output)

    return suite.finish()


if __name__ == "__main__":
    sys.exit(main())
