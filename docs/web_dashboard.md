# Web Management Dashboard

A browser interface for running the database: who may connect, what certificates they hold, what the server is doing
right now, and what the accounts and files look like. It starts with the database server, so there is nothing extra to
run.

It is a *management* interface, not a data browser. It reports how many records a file holds and how they are laid out;
it never returns one.

## Starting it

The dashboard comes up with the protocol server, whichever way that server is started —
`make run-server`, the CLI's background service, or `START.SERVER`. On startup it prints the address to open:

```
Server listening on TLS 127.0.0.1:8443
Web dashboard on http://127.0.0.1:8080/?token=6f1c…
  authorized as WEB.DASHBOARD (thumbprint 5bfc…), reissued on every start
```

Open that URL. The token in it is stored in a `HttpOnly`, `SameSite=Strict` cookie, so the page's own requests carry it
without any script being able to read it.

### Configuration

| Setting       | Default     | Description                                                           |
|---------------|-------------|-----------------------------------------------------------------------|
| `web_enabled` | `true`      | Set `false` for a server that should expose nothing but the protocol. |
| `web_addr`    | `127.0.0.1` | Interface to bind. May carry its own port (`"0.0.0.0:9000"`).         |
| `web_port`    | `8080`      | Port to listen on.                                                    |
| `web_token`   | *generated* | A fixed access token. Unset, a new one is generated on every boot.    |

A failure to start the dashboard — a port already in use, a missing CA — is reported and does not stop the database from
serving.

## How it talks to the database

The dashboard holds a client certificate and speaks the ordinary
[remote protocol](protocol.md) over the same TLS listener as every other client. It has no private path into the engine,
so it can do exactly what its authorization allows and nothing more, and its work shows up in `LIST.CONNS` and
`SERVER.STATS` like anyone else's.

```
browser ──HTTP(localhost)──▶ dashboard ──TLS + client cert──▶ protocol server ──▶ engine
```

Its certificate is **issued fresh on every boot** and authorized under the fixed name
`WEB.DASHBOARD`, which replaces the previous entry. Two things follow:

- A dashboard certificate from an earlier run stops working the moment the server restarts. It is valid for a day at
  most in any case.
- `DEAUTHORIZE.CONN WEB.DASHBOARD` locks the dashboard out until the next restart, the same way it would lock out any
  other client.

The certificate and its key are written next to the CA (`.local/certs/web-dashboard.crt`
by default), so they follow `ca_path` rather than littering the working directory.

## What it shows

| Tab            | Contents                                                                                                                         |
|----------------|----------------------------------------------------------------------------------------------------------------------------------|
| Overview       | Uptime, listener, connection and request totals, pending writes, tables in memory, and every connection open right now.          |
| Authorizations | Every authorized client: name, thumbprint, allowed accounts, admin flag. Authorize a thumbprint, add or remove accounts, revoke. |
| Certificates   | Issue a certificate signed by the server's CA, authorized in the same step, with its key downloadable once.                      |
| Accounts       | Every account with its file count, record count and size on disk; drill into an account's files and one file's statistics.       |

File statistics cover the record and dictionary counts, the hash modulus and group distribution, bytes on disk, the
durability flag and whether the file is currently held in the server's cache. Record counts come from each file's
section metadata, so opening the view does not load the file.

## Security

The dashboard can authorize clients and hand out private keys, so treat reaching it as equivalent to holding an admin
certificate.

- It **binds to `127.0.0.1` by default**. Point `web_addr` elsewhere only behind a reverse proxy that terminates TLS;
  the dashboard itself serves plain HTTP and says so at startup when it is bound to a non-loopback address.
- Every request needs the token, in the session cookie, an `Authorization: Bearer` header or a `?token=` parameter. The
  only exception is `/health`, which returns nothing but liveness. Tokens are compared in constant time.
- The page is served under `Content-Security-Policy: default-src 'none'` with only same-origin scripts and styles:
  nothing is fetched from anywhere else, and there is no inline script to smuggle anything into.
- Values from the database are written into the page as text, never as markup.

## HTTP API

Each endpoint is one protocol command. Responses are the protocol's own JSON; failures are
`{"error": "..."}` with a status code — `401` without a token, `403` when the protocol refused for lack of privileges,
`404` for something that is not there, `502` when the database itself cannot be reached.

| Method   | Path                                   | Command                                                           |
|----------|----------------------------------------|-------------------------------------------------------------------|
| `GET`    | `/health`                              | none (liveness, no token required)                                |
| `GET`    | `/api/stats`                           | `SERVER.STATS`                                                    |
| `GET`    | `/api/clients`                         | `LIST.CONNS`                                                      |
| `POST`   | `/api/clients`                         | `AUTHORIZE.CONN`                                                  |
| `DELETE` | `/api/clients/{name}`                  | `DEAUTHORIZE.CONN`                                                |
| `POST`   | `/api/clients/{name}/accounts`         | `ADD.CLIENT.ACCOUNT` / `REMOVE.CLIENT.ACCOUNT` (`"remove": true`) |
| `POST`   | `/api/certificates`                    | `GENERATE.CERT`                                                   |
| `GET`    | `/api/accounts`                        | `LIST.ACCOUNTS`                                                   |
| `GET`    | `/api/accounts/{account}/files`        | `LIST.FILES`                                                      |
| `GET`    | `/api/accounts/{account}/files/{file}` | `FILE.STATS`                                                      |

```sh
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/stats
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"common_name":"reporting-bot","accounts":["SALES"]}' \
     http://127.0.0.1:8080/api/certificates
```

## Implementation

Everything lives in `crates/core/src/web/`:

| File        | Role                                                                             |
|-------------|----------------------------------------------------------------------------------|
| `mod.rs`    | Boot: issue and authorize the certificate, mint the token, accept connections.   |
| `http.rs`   | The HTTP/1.1 subset the dashboard needs, with every read bounded.                |
| `client.rs` | The pooled TLS connection to the protocol server.                                |
| `api.rs`    | Routes: one HTTP shape in, one protocol command out.                             |
| `assets/`   | The page, its stylesheet and its script, embedded in the binary at compile time. |

There is no HTTP framework and no JavaScript framework. The protocol server was written the same way, and a dependency
tree larger than the database itself is a poor trade for a handful of routes.

`test/integration/test_web.py` drives the real binaries: the bootstrap, the token checks, every endpoint, certificate
issuing and revocation, and the certificate rotation across a restart.
