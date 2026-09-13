# Security Posture and Threat Model

What SmartRustyPick protects, what it does not, and which of those two lists a given statement belongs to. It is the
document the encryption work is designed against: the decisions recorded here are the ones every other part of that
design depends on, and they are cheaper to make now than to unpick from a format that already holds data.

> **Read the labels.** Everything under [The posture today](#the-posture-today) is true of the code as it stands, and
> nothing else in this document is. [Decisions](#decisions) records design decisions taken *before* the code, so that
> the sub-issues of #47 have a settled answer to build on. A decision is not a feature. Nothing here claims the database
> encrypts anything at rest — today it does not.

## What is worth protecting

- **Records and dictionaries** — every account's data, in `db_storage/<ACCOUNT>/<FILE>/`.
- **The system account** — `$LOGS` (thumbprints, peer addresses, denied-account messages), `$SAVEDLISTS` (record keys
  from real queries), `$CLIENTS` (who may connect), `$ACCOUNTS`.
- **Private key material** — the CA key, the server key, and every client key the CA has issued, in `.local/certs/`.
- **The dashboard token**, which is a credential for a fully privileged management client.

## In scope

The threats the design is meant to answer:

- **A stolen `db_storage/` directory.** A disk, a VM image, a decommissioned drive, a copied folder.
- **A stolen backup or snapshot.** The same bytes, moved somewhere with weaker custody than the host they came from.
- **A stolen `.local/certs/` directory.** Every key the CA has issued, plus the CA that can issue more.
- **A passive network observer** between a client and the protocol listener.

## Out of scope

Not because they do not matter, but because nothing in this design defends against them, and a threat model that
implies otherwise is worse than none:

- **Read access to the memory of an unlocked server process.** Once the database is unlocked, the keys and the
  plaintext are in that process. A debugger, a core dump or a memory-scraping root is game over.
- **The logged-in operator, while the database is unlocked.** Someone who can run commands as the service user can ask
  the database for its data through the front door.
- **Traffic analysis and metadata.** Record counts, file sizes, group distribution, request timing and connection
  patterns are not hidden, and encryption at rest will not hide them.
- **Side channels** — timing, cache, power.
- **A misbehaving but authorized client.** A client holding an authorized certificate is trusted within its allowed
  accounts, and within the capabilities it holds. Authorization is the control there, not encryption — which is why
  what a credential is authorized *for* is worth bounding, and why capabilities exist (below).
- **Availability.** Connections are bounded (`max_connections`) and handshakes time out (`handshake_timeout_ms`), which
  keeps a flood from building unbounded backlog. That is resource hygiene, not a denial-of-service defence.

## The posture today

| Surface | What protects it | What does not |
| --- | --- | --- |
| Protocol listener | **TLS 1.3 only**, pinned by this project on both the listener and the dashboard's client rather than inherited from rustls, with **mutual** authentication: the client certificate is verified against `ca_path`, then its SHA-256 thumbprint must appear in `$CLIENTS`. An unknown thumbprint is logged and the connection is dropped with no response. | Cipher suites are rustls' TLS 1.3 set, deliberately not overridden. Traffic analysis is unaffected — see the scope above. |
| Records, dictionaries, saved lists, `$LOGS` | Nothing. | Written as **plaintext** frames (`[key_len][key][data_len][data]`, see [Storage Engine](storage.md)). The CRC32C trailer is integrity against a torn write, not authentication: it is keyless, so anyone who can edit a group file can recompute it. |
| Web dashboard | Bound to `127.0.0.1:8080` by default. Its token is compared in constant time and stored in an `HttpOnly; SameSite=Strict` cookie. It is an ordinary protocol client with a certificate reissued every boot and valid for a day. | **Plain HTTP.** The cookie has no `Secure` attribute, the startup URL carries the token in a query string, and `POST /api/certificates` returns a freshly generated **private key** in the response body. Defensible on loopback; not once `web_addr` points anywhere else. |
| CA, server and client keys | Filesystem permissions: `.local/certs/` is `0700` and every key, certificate and PKCS#12 bundle in it is `0600`, on Unix. The mode is set *before* `openssl` writes the key, so there is no instant at which a private key is readable by anyone else. PKCS#12 bundles carry a per-issuance passphrase, delivered once to the caller and stored nowhere. | The PEM key files themselves are **unencrypted** (`openssl req -nodes`, `openssl genrsa`), so on the host the mode is all that protects them. The passphrase protects the bundle only once it leaves — anyone who can read `.local/certs/` has the `.key` beside it. |
| Certificate lifetime | Client certificates last 365 days, the CA 3650. Deauthorization by name takes effect on the client's next request. | There is **no revocation path** — no CRL, no OCSP, no CA rotation. Removing a thumbprint from `$CLIENTS` is the only revocation, and it works only for this database. |
| Files on disk | Every file this project writes — group files, `meta`, dictionaries, index state, queue books, the transaction intent log, directory-file records, archives and their staging siblings — is created `0600` on Unix, and a file written by an earlier build is tightened the next time it is rewritten. | **Directories** under `db_storage/` are left at the umask, so account and file names remain listable by anyone who can read the volume — which changes nothing, since those names are directory names either way (see below). **Windows sets no mode at all**: `PermissionsExt` is Unix-only, so there the files land at whatever the default ACL grants. |
| `config.toml` | — | It is **committed to the repository** and has a `web_token` field. Treat it as a non-secret file; a token set there is a token in git history. |

`$LOGS` is capped at `max_log_records` (default 100) and holds the message plus, in `detailed` mode, a UTC timestamp.
Its **record keys** embed a timestamp and the account name, which is why it is the clearest case for the key-encryption
option below.

## Trust boundaries

Extending the diagram in [Web Dashboard](web_dashboard.md), with what is protected on each hop:

```
                        ┌─ trusted host ──────────────────────────────────────────────┐
                        │                                                             │
  browser ──HTTP──────▶ │ dashboard ──TLS 1.3, mutual auth──────▶ protocol server     │
   (plaintext,          │  (ordinary client,                        │                 │
    loopback only)      │   1-day certificate)                      ▼                 │
                        │                                         engine              │
  remote client ────────┼──TLS 1.3, mutual auth───────────────────▶ │                 │
   (thumbprint          │                                           ▼                 │
    authorized)         │                       db_storage/  ──── PLAINTEXT today     │
                        │                       .local/certs/ ──── PLAINTEXT keys     │
                        └─────────────────────────────────────────────────────────────┘
                                                     │
                                    a stolen disk, backup or snapshot
                                    crosses this line with everything in it
```

The line the encryption work is about is the bottom one: today, everything that leaves the host on a disk or in a backup
leaves in the clear.

## Decisions

Taken now so the sub-issues of #47 are not each free to answer them differently. **None of this is implemented.**

### 1. The key-encryption key is supplied at start, and never stored beside the data

A per-database data-encryption key (DEK) is wrapped by a key-encryption key (KEK). The KEK is **supplied by the operator
when the database starts** — a passphrase through a memory-hard KDF, a key file the operator points at, or an
environment variable for headless and container runs. A KEK sitting inside `db_storage/`, or derivable from it, is not
encryption at rest and is rejected as an option.

- **What it buys:** a stolen directory, backup or snapshot is ciphertext to whoever took it, because the key was never
  in it.
- **What it costs:** every start needs the key, including the CLI's automatic background server and `make run-server`,
  neither of which has anyone to prompt. An encrypted database that cannot be unlocked must **refuse to start**.
  Falling back to plaintext, or starting up degraded, is not an option — it turns a loud failure into a silent one.
- **What it does not buy:** anything against the operator of a running, unlocked host. That is the out-of-scope list,
  and no key-custody scheme moves it.

### 2. Encrypting record keys is configurable; bodies only is the default

Both postures are supported, because they answer different questions:

| | Bodies only (default) | Bodies and keys |
| --- | --- | --- |
| Record placement | `fnv1a64(key) % modulus` unchanged | Needs a **keyed** hash under a DEK-derived subkey, so a stolen directory cannot be located in without the key |
| What a thief reads | Every record key: `$LOGS` timestamps and account names, `DIR`'s file names, whatever your keys mean | Sizes and counts only |
| Cost | The AEAD over record bodies | Above, plus decrypting a key per frame scanned, and a re-derivation on every modulus change |

Chosen as a **database default in `config.toml`, overridable per file** in the account's `DIR` entry — the same shape
as `DURABLE`, so a sensitive file can pay for encrypted keys while hot files do not. Changing the flag on an existing
file rewrites its sections.

Two constraints fall out of this and are not negotiable in the implementation:

- **The mode is recorded in the section's own `meta`**, beside `checksums`. A reader must never have to be *told* out of
  band how to read what is in front of it, and `DIR` in particular cannot be the answer for `DIR` itself.
- **Key ciphertext is randomized, not deterministic.** Equality lookup is served by the keyed hash and then by
  decrypting within the group, so nothing needs identical keys to produce identical ciphertext — and deterministic
  encryption would leak exactly the equality relation the option exists to hide.

Neither posture hides **account or file names**: those are directory names on disk (`db_storage/SALES/USERS/`). Hiding
them is a separate change to the storage layout and is not in this design.

### 3. The SYSTEM account is in scope

`$LOGS`, `$SAVEDLISTS`, `$CLIENTS` and `$ACCOUNTS` encrypt like any other file, with no carve-out. `$LOGS` records
thumbprints, peer addresses and denied-account names, and `$SAVEDLISTS` holds record keys from real queries; leaving the
audit trail readable in a stolen directory would undo much of what encrypting the data achieved.

This is the case that forces the ordering: the engine cannot list an account's files without reading `DIR`, so an
encrypted database is unreadable until it is unlocked — consistent with decision 1's refusal to start without a key.

### 4. Authorization is two questions, not one

`ADMIN` used to be one flag doing two jobs: it bypassed the account allowlist on the data plane **and** it gated every
administrative command. The consequence was that anything which had to create an account or a file was thereby
authorized to read and overwrite every record in the database. A deployment script, a CI job, a service that onboards
accounts — each needed a credential that was also a master key.

The two jobs are now separate, and neither implies the other:

- **Which accounts may this connection touch?** The allowed-account list. Everything that names a target account is
  checked against it, including `CREATE.FILE`, `SET.FILE`, `DELETE.FILE` and the index commands — a file, a dictionary
  and an index live inside one account and affect nothing outside it, so they are authorized the way the records in
  them already were. A client that may rewrite every record in a file was never restrained by being unable to index it.
- **What may this connection do that is not about one account?** A capability: `accounts:manage`, `clients:manage` or
  `server:observe`. The table of which commands each covers is in [the protocol reference](protocol.md#authorization).

**Creating an account does not grant access to it.** That is the property that makes a provisioning credential worth
having: it can create the account and cannot read a record in it, and it cannot grant itself the access either, because
granting is `clients:manage` — deliberately a different capability, since a client that may authorize clients may
authorize an admin.

`ADMIN` still means every capability and every account, so an authorization written before capabilities existed behaves
exactly as it did and nothing has to be migrated. `EXPORT.*` and `IMPORT*` stay on `ADMIN` rather than moving behind a
capability: an export reads every record of whatever it names, so a capability granting it would grant reading every
account — the conflation this decision exists to undo.

A secondary benefit worth stating, because it is what an operator actually sees: `LIST.CONNS` can now distinguish a
credential that exists to run backups from one that exists to provision accounts. Both used to read `ADMIN`.

### 5. What may never be logged

The rule, so that the encryption work has something to check itself against — a key that leaks into a log line defeats
all of it:

> **Key material, passphrases and tokens never reach `$LOGS`, stdout, stderr, a protocol response, or an HTTP response
> body.**

There are exactly two deliberate exceptions, and both exist to *deliver* the thing they carry:

1. **Certificate issuance.** `GENERATE.CERT` returns `private_key_pem` and `pfx_passphrase` to the caller that asked
   for the certificate. That is the entire purpose of the command; there is no other way to get a key to a client.
2. **The dashboard startup URL**, which prints the session token because it is how the operator reaches the dashboard
   at all. Its lifecycle — a URL valid for the life of the process — is a known gap, tracked in #54.

How the rule is held rather than remembered:

- A `Secret` is the only type a passphrase, key or token is held in. It has no `Display`, no `Serialize` and no
  `Clone`; its `Debug` prints `[redacted]` and it zeroizes on drop. `expose()` is the single way out, so every
  legitimate disclosure is visible in review as the exception it is.
- `GeneratedCert` deliberately derives neither `Debug` nor `Serialize`. It is the one struct holding a private key and
  a passphrase at once, so a derive would carry both into any response or log line that ever touched it; `record()`
  names each field it emits instead.
- `Config` has a hand-written `Debug` that redacts `web_token`. Nothing prints a `Config` today — the point is that the
  next thing to do it cannot leak the dashboard credential by accident.
- A PKCS#12 passphrase reaches `openssl` through the child's environment, never `-passout pass:<value>`, which `ps`
  shows to every user on the host.
- The integration suite issues a certificate and then searches every byte the database wrote for its key and
  passphrase. `$LOGS` and `$SAVEDLISTS` are inside that search, so the assertion does not depend on how either is
  queried.

**Certificate thumbprints are logged in full, on purpose.** A thumbprint is a public fingerprint — holding one grants
nothing without the private key beside it — it is already stored in full in `$CLIENTS` for every authorized client, and
the rejected-connection log line is how an operator discovers what to authorize next. Truncating it would cost a real
workflow to hide an identifier that is not a secret. Peer addresses and denied-account names are logged for the same
reason: they are the access-control record, and `$LOGS` is in scope for encryption at rest (decision 3) rather than
being thinned out.

## What the operator is responsible for

- **Key custody.** The KEK is yours to store, deliver and rotate. It must not live in the backup it protects, and a
  passphrase in a shell history or a CI log is a disclosed key.
- **Backups.** An encrypted `db_storage/` is only as private as the key that is *not* in the archive beside it. See
  the backup work in #37.
- **The dashboard's exposure.** It is loopback and plain HTTP by design. Put it behind a TLS-terminating reverse proxy
  before binding it anywhere else, and expect the token cookie to need `Secure` once you do. The startup URL contains
  the token: treat it like a password, not like a bookmark.
- **File modes on Windows.** On Unix the code sets them: `0700` on `.local/certs/`, `0600` on every key and on every
  file under `db_storage/`. On Windows nothing is set, and the inherited ACL is yours to get right. One deliberate
  exception on both: `EXTRACT` writes to a path you named, outside the database, so it follows your umask like any
  other export rather than landing unreadable to everyone but the service user.
- **`config.toml`.** It is committed. Keep secrets out of it and source them from the environment or a gitignored
  override.
- **Certificate hygiene.** There is no revocation but the thumbprint list. Issue narrowly, keep `LIST.CONNS` short, and
  deauthorize what you no longer recognise. "Narrowly" is now expressible: grant the capability a credential needs
  instead of `ADMIN`, and check `LIST.CONNS` for entries still holding the flag that do not need it.

## Still open

Recorded here so they are not mistaken for settled:

- Whether Windows gets real ACL tightening, or stays documented as an operator responsibility.
- Whether the dashboard gets native TLS, refuses a non-loopback bind without it, or keeps key-bearing endpoints
  loopback-only regardless of bind address.
- CA rotation and a real revocation path.
- Whether the PEM key files themselves should be encrypted at rest, now that the bundle made from them is.
- Encryption granularity (per group file or per record) and nonce management, which are storage-engine questions rather
  than threat-model ones.

The work is tracked in #47 and its sub-issues; this document is Phase 0 of it (#48).
