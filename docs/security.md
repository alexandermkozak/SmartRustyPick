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
  accounts. Authorization is the control there, not encryption.
- **Availability.** Connections are bounded (`max_connections`) and handshakes time out (`handshake_timeout_ms`), which
  keeps a flood from building unbounded backlog. That is resource hygiene, not a denial-of-service defence.

## The posture today

| Surface | What protects it | What does not |
| --- | --- | --- |
| Protocol listener | TLS with **mutual** authentication: the client certificate is verified against `ca_path`, then its SHA-256 thumbprint must appear in `$CLIENTS`. An unknown thumbprint is logged and the connection is dropped with no response. | The TLS floor is rustls' default, so **TLS 1.2 is still accepted**; version and cipher suites are not pinned by this project. |
| Records, dictionaries, saved lists, `$LOGS` | Nothing. | Written as **plaintext** frames (`[key_len][key][data_len][data]`, see [Storage Engine](storage.md)). The CRC32C trailer is integrity against a torn write, not authentication: it is keyless, so anyone who can edit a group file can recompute it. |
| Web dashboard | Bound to `127.0.0.1:8080` by default. Its token is compared in constant time and stored in an `HttpOnly; SameSite=Strict` cookie. It is an ordinary protocol client with a certificate reissued every boot and valid for a day. | **Plain HTTP.** The cookie has no `Secure` attribute, the startup URL carries the token in a query string, and `POST /api/certificates` returns a freshly generated **private key** in the response body. Defensible on loopback; not once `web_addr` points anywhere else. |
| CA, server and client keys | Nothing beyond the filesystem. | Unencrypted PEM (`openssl req -nodes`, `openssl genrsa`). PKCS#12 bundles are exported with an **empty password**. No explicit mode is set on any of them anywhere in the workspace, so the umask decides who can read them. |
| Certificate lifetime | Client certificates last 365 days, the CA 3650. Deauthorization by name takes effect on the client's next request. | There is **no revocation path** — no CRL, no OCSP, no CA rotation. Removing a thumbprint from `$CLIENTS` is the only revocation, and it works only for this database. |
| `config.toml` | — | It is **committed to the repository** and has a `web_token` field. Treat it as a non-secret file; a token set there is a token in git history. |

`$LOGS` is capped at `max_log_records` (default 100) and holds the message plus, in `detailed` mode, a UTC timestamp.
Its **record keys** embed a timestamp and the account name, which is why it is the clearest case for the key-encryption
option below.

## Trust boundaries

Extending the diagram in [Web Dashboard](web_dashboard.md), with what is protected on each hop:

```
                        ┌─ trusted host ──────────────────────────────────────────────┐
                        │                                                             │
  browser ──HTTP──────▶ │ dashboard ──TLS 1.3/1.2, mutual auth──▶ protocol server     │
   (plaintext,          │  (ordinary client,                        │                 │
    loopback only)      │   1-day certificate)                      ▼                 │
                        │                                         engine              │
  remote client ────────┼──TLS 1.3/1.2, mutual auth───────────────▶ │                 │
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

## What the operator is responsible for

- **Key custody.** The KEK is yours to store, deliver and rotate. It must not live in the backup it protects, and a
  passphrase in a shell history or a CI log is a disclosed key.
- **Backups.** An encrypted `db_storage/` is only as private as the key that is *not* in the archive beside it. See
  the backup work in #37.
- **The dashboard's exposure.** It is loopback and plain HTTP by design. Put it behind a TLS-terminating reverse proxy
  before binding it anywhere else, and expect the token cookie to need `Secure` once you do. The startup URL contains
  the token: treat it like a password, not like a bookmark.
- **File modes.** Until the code sets them, `.local/certs/` and its keys land at whatever the umask allows. `0700` on
  the directory and `0600` on the keys is the expectation.
- **`config.toml`.** It is committed. Keep secrets out of it and source them from the environment or a gitignored
  override.
- **Certificate hygiene.** There is no revocation but the thumbprint list. Issue narrowly, keep `LIST.CONNS` short, and
  deauthorize what you no longer recognise.

## Still open

Recorded here so they are not mistaken for settled:

- Pinning a TLS 1.3 floor and an explicit cipher suite list, rather than inheriting rustls' defaults.
- Whether the dashboard gets native TLS, refuses a non-loopback bind without it, or keeps key-bearing endpoints
  loopback-only regardless of bind address.
- CA rotation and a real revocation path.
- How a PKCS#12 passphrase is chosen and delivered, given the bundle exists precisely to be moved between machines.
- Encryption granularity (per group file or per record) and nonce management, which are storage-engine questions rather
  than threat-model ones.

The work is tracked in #47 and its sub-issues; this document is Phase 0 of it (#48).
