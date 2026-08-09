# Container Deployment

SmartRustyPick ships with a container definition that runs the headless server (`smart-rusty-pick-server`). It is
written to the OCI/Dockerfile format, so the exact same build works with **podman** and **docker**.

| File                                 | Purpose                                                                                   |
|--------------------------------------|-------------------------------------------------------------------------------------------|
| `Containerfile`                      | Multi-stage build (Rust builder + slim Debian runtime).                                   |
| `Dockerfile`                         | Symlink to `Containerfile` for docker's default lookup.                                   |
| `.containerignore` / `.dockerignore` | Keeps `target/`, local certificates and the local `db_storage/` out of the build context. |
| `compose.yaml`                       | Single-service compose stack with a persistent data volume.                               |
| `deploy/entrypoint.sh`               | Seeds `/data/config.toml` on first start, then runs the server.                           |
| `deploy/config.toml`                 | Default config baked into the image (binds to `0.0.0.0:8443`).                            |

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

| Variable          | Default   | Description                                    |
|-------------------|-----------|------------------------------------------------|
| `SRP_SERVER_ADDR` | `0.0.0.0` | Listen address written into the seeded config. |
| `SRP_SERVER_PORT` | `8443`    | Listen port written into the seeded config.    |
| `SRP_DATA_DIR`    | `/data`   | Working directory used by the entrypoint.      |

Afterwards, edit `/data/config.toml` directly and restart the container. Keep
`server_addr` at `0.0.0.0` — binding to `127.0.0.1` inside the container makes the published port unreachable.

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
