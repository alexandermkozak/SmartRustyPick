#!/bin/sh
# Bootstraps the data directory before handing control to the requested binary.
set -e

DATA_DIR="${SRP_DATA_DIR:-/data}"

mkdir -p "$DATA_DIR/db_storage"
cd "$DATA_DIR"

# Seed a default config on first run. The server reads ./config.toml from the
# working directory, and generates its certificates next to it.
if [ ! -f "$DATA_DIR/config.toml" ]; then
    echo "No config.toml found in $DATA_DIR, installing the default one..."
    cp /usr/local/share/smart-rusty-pick/config.toml "$DATA_DIR/config.toml"

    if [ -n "$SRP_SERVER_ADDR" ]; then
        sed -i "s|^server_addr = .*|server_addr = \"$SRP_SERVER_ADDR\"|" "$DATA_DIR/config.toml"
    fi
    if [ -n "$SRP_SERVER_PORT" ]; then
        sed -i "s|^server_port = .*|server_port = $SRP_SERVER_PORT|" "$DATA_DIR/config.toml"
    fi
fi

exec "$@"
