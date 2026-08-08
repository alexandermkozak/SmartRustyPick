# Multi-stage build for the SmartRustyPick headless server.
# Works with both `podman build` and `docker build`.

FROM docker.io/library/rust:1.90-bookworm AS builder

WORKDIR /src

# Copy the manifests first so dependency compilation can be cached.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --package smart-rusty-pick-server \
    && cargo build --release --package smart-rusty-pick-cli

FROM docker.io/library/debian:bookworm-slim

# openssl is required at runtime: the server shells out to it to generate
# the CA and server certificates on first startup.
RUN apt-get update \
    && apt-get install -y --no-install-recommends openssl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/smart-rusty-pick-server /usr/local/bin/smart-rusty-pick-server
COPY --from=builder /src/target/release/smart-rusty-pick-cli /usr/local/bin/smart-rusty-pick-cli
COPY deploy/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY deploy/config.toml /usr/local/share/smart-rusty-pick/config.toml

RUN chmod +x /usr/local/bin/entrypoint.sh

# The server resolves `config.toml`, `db_storage` and the certificate paths
# relative to the working directory, so everything lives in one data volume.
ENV SRP_DATA_DIR=/data
WORKDIR /data
VOLUME ["/data"]

EXPOSE 8443

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["smart-rusty-pick-server"]
