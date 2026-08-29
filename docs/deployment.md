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
```

To keep the data in a host directory instead of a named volume, replace the volume entry in `compose.yaml`:

```yaml
    volumes:
      - ./data:/data:z
```

The `:z` suffix is required on SELinux-enabled hosts (common with podman) and is harmless with docker.

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

To pre-provision your own certificates, drop `ca.crt`, `server.crt` and
`server.key` into the data volume before the first start; existing files are never overwritten.

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
