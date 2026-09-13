# Container Deployment

SmartRustyPick ships with a container definition that runs the headless server (`smart-rusty-pick-server`). It is
written to the OCI/Dockerfile format, so the exact same build works with **podman** and **docker**.

There is only one build definition. `Containerfile` holds it; `Dockerfile` and `.dockerignore` are symlinks to
`Containerfile` and `.containerignore`, so docker finds them under the names it looks for and there is nothing to keep
in sync by hand. Edit `Containerfile` and `.containerignore`.

| File                                 | Purpose                                                                                        |
|--------------------------------------|------------------------------------------------------------------------------------------------|
| `Containerfile`                      | Multi-stage build (Rust builder + slim Debian runtime). The single source of truth.            |
| `Dockerfile`                         | Symlink to `Containerfile` for docker's default lookup. Never edit it directly.                |
| `.containerignore` / `.dockerignore` | Same arrangement: `.dockerignore` is a symlink to `.containerignore`.                          |
| `compose.yaml`                       | Single-service compose stack with a persistent data volume.                                    |
| `deploy/entrypoint.sh`               | Seeds `/data/config.toml` on first start, then runs the server.                                |
| `deploy/config.toml`                 | Default config baked into the image (protocol on `0.0.0.0:8443`, dashboard on `0.0.0.0:8080`). |

## Quick start

```sh
# podman
podman compose up -d          # or: podman-compose up -d

# docker
docker compose up -d
```

The `Makefile` wraps the same commands (`CONTAINER_ENGINE` defaults to `podman`):

```sh
make container-build
make container-up
make container-logs
make container-down
make container-cli                       # interactive CLI inside the container
make container-up CONTAINER_ENGINE=docker
```

## How data is stored

The server resolves `config.toml`, `db_storage/` and the certificate paths relative to its working directory, so the
container puts all of them in a single directory: `/data`, exposed as the named volume `srp-data`.

```
/data
├── config.toml     # seeded from the image on first start, then yours to edit
├── ca.crt, ca.key  # CA generated on first start
├── server.crt/.key # server certificate, signed by that CA
└── db_storage/     # accounts and tables (containing `dict` and `data.hf/`)
    └── .format     # the storage format version, checked on every start
```

To keep the data in a host directory instead of a named volume, replace the volume entry in `compose.yaml`:

```yaml
    volumes:
      - ./data:/data:z
```

The `:z` suffix is required on SELinux-enabled hosts (common with podman) and is harmless with docker.

## Upgrading and rolling back

The volume outlives the container, so **replacing the image tag is the upgrade**. What makes that safe is
`db_storage/.format`: the directory records the [storage format version](storage.md#storage-format-versions) it was
written in, and a server checks it before reading a byte.

```sh
# Always, before an upgrade. Take a real backup, not a copy of the volume -
# see Backups below for why the difference matters.
make container-cli     # then: EXPORT.ALL TO /data/backups/pre-upgrade.srp

podman compose pull && podman compose up -d
podman compose logs | head
```

Three things can come out of that start, and only the first is silent:

| What you see | What happened |
|--------------|----------------|
| nothing about the format | The directory is already at the version this build writes. |
| `db_storage migrated from storage format 1 to 2. An older build can no longer open it.` | It was brought forward in place. **The rollback below no longer works.** |
| `Cannot open the storage directory 'db_storage': …` and the container exits | The build will not touch it. Nothing was read or written. |

The refusal names the version on disk and the range the build supports, and it is the point of the whole mechanism:
a container that will not start is recoverable in a minute, while one that started and misread the data may not be
recoverable at all.

**Rolling back.** Going back to a previous tag works as long as nothing migrated the directory:

```sh
podman compose down
# pin the previous tag in compose.yaml, then
podman compose up -d
```

Once a start has printed `migrated from storage format …`, the older image will refuse the volume, and that refusal is
correct — the older build genuinely cannot read what the newer one wrote. Start the old tag against an empty volume and
`IMPORT` the archive taken above into it. This is why the backup is not optional: a migration is the one step in an
upgrade that is not reversible by changing the tag back, and an archive is readable by builds on both sides of it —
the [archive format is versioned separately](storage.md#the-archive-format-version) from the storage format, precisely
so that a backup outlives the directory it came from.

**Checking before you move.** Ask the running server what it writes and what it will open, rather than reading a file
inside a container:

```sh
make container-cli     # then: SERVER.STATS
```

`storage_format` is the version the directory is at (the server would not have started otherwise) and
`storage_format_oldest_supported` is the oldest the build will open. An image whose `storage_format_oldest_supported`
is above your directory's version cannot take it.

One gap is worth knowing about: images built before this check existed do not look at `.format` at all, so they will
open a directory of any version. The protection starts with the first image that has it, and applies from there on.

## Backups

**Do not back up by copying the volume while the server is running.** It is the one thing that looks like a backup and
is not:

- writes are buffered in memory for up to `flush_interval_ms`, so a copy can miss acknowledged writes;
- a flush writes every changed group and *then* rewrites `meta`, so a copy landing between the two gets halves that
  disagree;
- nothing coordinates such a copy across the files of an account, so even a clean per-file copy is not a coherent
  account.

Use [`EXPORT`](admin_commands.md#exportfile--exportaccount--exportall) instead. It flushes, holds the files it names,
and writes a self-describing archive with a checksum over the whole of it, so an archive that restores at all restores
to a state the database actually passed through.

```sh
make container-cli
# then, inside the CLI:
#   EXPORT.ACCOUNT SALES TO /data/backups/sales-2026-09-12.srp    (nightly)
#   EXPORT.ALL TO /data/backups/all-2026-09-12.srp                (maintenance window)
```

`EXPORT.ACCOUNT` holds one account still and is the one to run routinely. `EXPORT.ALL` blocks writes to the whole
database for as long as it takes, so it belongs in a window where that is acceptable — and it leaves `SYSTEM` out,
because `$CLIENTS` holds the certificate thumbprints this deployment authorized and they have no business travelling
inside a routine backup.

Put the archives somewhere the volume is not. A backup on the disk you are protecting against is not one:

```yaml
# compose.yaml - a second volume, so a lost data volume does not take the backups with it
volumes:
  - srp-data:/data
  - srp-backups:/data/backups
```

**Restoring.** Verify first, always — it reads the archive through and reports what a real import would do without
writing anything:

```sh
make container-cli
# then:
#   IMPORT /data/backups/all-2026-09-12.srp VERIFY
#   IMPORT /data/backups/all-2026-09-12.srp OVERWRITE
```

An archive restores into a **different** deployment as readily as the one it came from — the hashfile layout is not
carried, so the target rehashes into its own `records_per_group`. `IMPORT … AS <account>` brings a production account
up beside the original for inspection without touching it.

A client with no filesystem access to the server host can move an archive over the connection instead, with
[`EXPORT.BYTES` / `IMPORT.BYTES`](protocol.md#exportbytes--importbytes--admin).

## Configuration

`deploy/config.toml` is only copied when `/data/config.toml` does not exist yet. Two environment variables can adjust
that first-run seed:

| Variable          | Default     | Description                                                        |
|-------------------|-------------|--------------------------------------------------------------------|
| `SRP_SERVER_ADDR` | `0.0.0.0`   | Listen address written into the seeded config.                     |
| `SRP_SERVER_PORT` | `8443`      | Listen port written into the seeded config.                        |
| `SRP_WEB_PORT`    | `8080`      | Port the web dashboard listens on.                                 |
| `SRP_WEB_ENABLED` | `true`      | Set to `false` to seed a config with no dashboard at all.          |
| `SRP_WEB_TOKEN`   | *generated* | Fixed dashboard token; unset, a new one is printed on every start. |
| `SRP_DATA_DIR`    | `/data`     | Working directory used by the entrypoint.                          |

Afterwards, edit `/data/config.toml` directly and restart the container. Keep
`server_addr` at `0.0.0.0` — binding to `127.0.0.1` inside the container makes the published port unreachable.

## The web dashboard

The [web management dashboard](web_dashboard.md) starts with the server. `compose.yaml`
publishes it to the host's loopback interface only (`127.0.0.1:8080:8080`): it can authorize clients and issue
certificates, and it speaks plain HTTP, so it should not be exposed directly. Put a TLS-terminating reverse proxy in
front of it if it has to be reachable from elsewhere.

Unless `SRP_WEB_TOKEN` was set, each start prints a URL carrying that boot's token:

```sh
podman compose logs smart-rusty-pick | grep 'Web dashboard'
# Web dashboard on http://0.0.0.0:8080/?token=6f1c...
```

Open it as `http://127.0.0.1:8080/?token=...` on the host. The dashboard's own client certificate is reissued and
re-authorized on every container start, so a restart invalidates the previous one.

## Certificates and clients

On the very first start the server generates its own CA and server certificate with `openssl` (which is installed in the
runtime image). The remote protocol requires client certificates signed by that same CA, so clients need `ca.crt`
and a client certificate issued from `ca.key`:

```sh
podman cp smart-rusty-pick:/data/ca.crt ./ca.crt
podman cp smart-rusty-pick:/data/ca.key ./ca.key
openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -subj '/CN=My Client'
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    -out client.crt -days 365 -sha256
```

Clients that want a PKCS#12 bundle rather than a PEM pair - .NET, and most GUI clients - need the CA in the bundle too,
which is what `-certfile` does. Without it such a client cannot build a chain for its own certificate, so it never
offers one and the server drops the connection as unauthenticated:

```sh
openssl pkcs12 -export -out client.pfx -inkey client.key -in client.crt \
    -certfile ca.crt -passout pass:
```

Register the client thumbprint with the server as described in
[Administration Commands](admin_commands.md), then connect as documented in the
[Remote Protocol](protocol.md).

Clients issued by `GENERATE.CERT` get a passphrase-protected bundle; the hand-rolled `-passout pass:` above is an
empty password and is only acceptable for a bundle you are about to import and delete.

To pre-provision your own certificates, drop `ca.crt`, `server.crt` and
`server.key` into the data volume before the first start. Existing files are never replaced, with one exception: if
the server certificate no longer chains to the configured `ca_path` — because the CA was rotated, or because the
certificate expired — it is re-signed against the current CA, keeping its key. Without that a rotation would leave the
listener presenting a certificate no current client can verify, and an expired server certificate could only be fixed
by hand.

### Rotating the CA

Every client certificate is signed by one CA, so replacing it invalidates all of them at once. `additional_ca_paths`
is what turns that flag day into a transition: CAs listed there are **trusted but not issued from**, so the outgoing
one can keep working while clients are reissued one at a time.

1. **Make the incoming CA.** Any CA will do; the shape the server generates is:

   ```sh
   openssl req -x509 -new -nodes -newkey rsa:2048 -days 3650 \
       -keyout ca-new.key -out ca-new.crt -subj '/CN=SmartRustyPick Root CA' \
       -addext 'basicConstraints=critical,CA:TRUE' \
       -addext 'keyUsage=critical,keyCertSign,cRLSign'
   ```

2. **Point `ca_path` at it and keep the old one trusted**, then restart:

   ```toml
   ca_path = ".local/certs/ca-new.crt"
   additional_ca_paths = [".local/certs/ca.crt"]
   ```

   On that restart the server re-signs its own certificate against `ca-new.crt`. Both CAs are now trusted for
   incoming clients, and every certificate `GENERATE.CERT` issues from here is signed by the new one and carries
   **both** CAs in its `ca_pem` — so a client can verify the server whichever CA signed it.

3. **Reissue clients** with `GENERATE.CERT`, at whatever pace suits. `LIST.CONNS` shows each certificate's expiry, so
   you can see what is left to do.

4. **Drop `additional_ca_paths`** once nothing is signed by the old CA, and restart. Anything still holding an old
   certificate stops connecting at that point, which is what completes the rotation — an overlap that never ends is
   not a rotation. Destroy `ca.key` afterwards.

The CA currently lasts 3650 days and the server certificate 365; neither lifetime is configurable (client certificate
lifetimes are, see `max_client_cert_days`).

## Building the image manually

```sh
podman build -f Containerfile -t localhost/smart-rusty-pick:latest .
docker build -t smart-rusty-pick:latest .

podman run -d --name smart-rusty-pick \
    -p 8443:8443 -v srp-data:/data \
    localhost/smart-rusty-pick:latest
```

The image also contains `smart-rusty-pick-cli`, so the interactive CLI can be run against the container's database:

```sh
podman exec -it smart-rusty-pick smart-rusty-pick-cli
```
