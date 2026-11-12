#!/usr/bin/env bash
# Build the discovery server as a static musl binary, install it on a host over ssh,
# restart its systemd unit and check that it answers. The unit is stopped while the
# database files are copied, so a new binary that migrates the database can be rolled
# back. If the unit does not start or the check fails, the previous binary and the copied
# database are put back and the unit is started again.
#
# Required environment:
#   DEPLOY_HOST        ssh destination, for example deploy@server.example
#   DEPLOY_HEALTH_URL  URL of the server's /api/v1/info, for example
#                      https://server.example/api/v1/info
# Optional environment:
#   DEPLOY_SSH_KEY     ssh private key file (default: ssh's own key selection)
#   DEPLOY_DIR         install directory on the host (default: /opt/proscenium)
#   DEPLOY_DATA_DIR    server data directory on the host (default: $DEPLOY_DIR/data)
#   DEPLOY_SERVICE     systemd unit name (default: proscenium)
#
# The ssh user needs sudo for cp, install, mv, rm, test and systemctl on the host.
set -euo pipefail

: "${DEPLOY_HOST:?set DEPLOY_HOST to the ssh destination (user@host)}"
: "${DEPLOY_HEALTH_URL:?set DEPLOY_HEALTH_URL to the URL of /api/v1/info on the deployed server}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/proscenium}"
DEPLOY_DATA_DIR="${DEPLOY_DATA_DIR:-$DEPLOY_DIR/data}"
DEPLOY_SERVICE="${DEPLOY_SERVICE:-proscenium}"

HEALTH_ATTEMPTS=15
HEALTH_INTERVAL_SECONDS=2

SSH_OPTS=()
if [[ -n "${DEPLOY_SSH_KEY:-}" ]]; then
    if [[ ! -r "$DEPLOY_SSH_KEY" ]]; then
        echo "DEPLOY_SSH_KEY is not a readable file: $DEPLOY_SSH_KEY" >&2
        exit 1
    fi
    SSH_OPTS=(-o IdentitiesOnly=yes -i "$DEPLOY_SSH_KEY")
fi

if [[ "$DEPLOY_HEALTH_URL" != https://* ]]; then
    echo "warning: DEPLOY_HEALTH_URL is not an https URL; the check is not authenticated" >&2
fi

remote() {
    ssh ${SSH_OPTS[@]+"${SSH_OPTS[@]}"} "$DEPLOY_HOST" "$@"
}

# Quote arguments for the remote shell, which splits the ssh command line again.
quote() {
    printf '%q ' "$@"
}

cd "$(dirname "${BASH_SOURCE[0]}")/.."
BINARY="server/target/x86_64-unknown-linux-musl/release/proscenium-server"

echo "Building server (musl)..."
cargo build --release --target x86_64-unknown-linux-musl --manifest-path server/Cargo.toml
CHECKSUM="$(sha256sum "$BINARY" | cut -d' ' -f1)"

echo "Uploading binary..."
# mktemp -d creates a directory with mode 0700 owned by the ssh user, so no other
# local user of the host can replace the file between upload and install.
REMOTE_TMP="$(remote mktemp -d)"
if [[ "$REMOTE_TMP" != /* ]]; then
    echo "unexpected mktemp output from host: $REMOTE_TMP" >&2
    exit 1
fi
cleanup() {
    remote "rm -rf -- $(quote "$REMOTE_TMP")" || true
}
trap cleanup EXIT

scp ${SSH_OPTS[@]+"${SSH_OPTS[@]}"} "$BINARY" "$DEPLOY_HOST:$REMOTE_TMP/proscenium-server"

echo "Installing and restarting $DEPLOY_SERVICE..."
remote "bash -s -- $(quote "$REMOTE_TMP" "$DEPLOY_DIR" "$DEPLOY_DATA_DIR" "$DEPLOY_SERVICE" "$CHECKSUM")" <<'REMOTE'
set -euo pipefail
tmp=$1
dir=$2
data=$3
service=$4
checksum=$5

echo "$checksum  $tmp/proscenium-server" | sha256sum --check --status -

# Keep the installed binary and, with the unit stopped, the database files, so a failed
# start or health check can restore both.
if sudo test -f "$dir/proscenium-server"; then
    sudo cp -p -- "$dir/proscenium-server" "$dir/proscenium-server.previous"
else
    sudo rm -f -- "$dir/proscenium-server.previous"
fi
sudo systemctl stop "$service"
for file in server.db server.db-wal server.db-shm; do
    sudo rm -f -- "$data/$file.previous"
    if sudo test -f "$data/$file"; then
        sudo cp -p -- "$data/$file" "$data/$file.previous"
    fi
done
# Write next to the target, then rename: the target path never holds a partial file.
sudo install -o root -g root -m 0755 -- "$tmp/proscenium-server" "$dir/proscenium-server.new"
sudo mv -f -- "$dir/proscenium-server.new" "$dir/proscenium-server"
# A failed start is detected by the health check, which then restores the copies.
sudo systemctl start "$service" || echo "systemctl start $service failed" >&2
REMOTE

echo "Checking $DEPLOY_HEALTH_URL..."
healthy=0
for ((attempt = 1; attempt <= HEALTH_ATTEMPTS; attempt++)); do
    sleep "$HEALTH_INTERVAL_SECONDS"
    if remote "sudo systemctl is-active --quiet $(quote "$DEPLOY_SERVICE")" &&
        body="$(curl --fail --silent --max-time 5 "$DEPLOY_HEALTH_URL")"; then
        echo "$body"
        healthy=1
        break
    fi
done

if [[ "$healthy" -ne 1 ]]; then
    echo "Health check failed after $((HEALTH_ATTEMPTS * HEALTH_INTERVAL_SECONDS)) seconds." >&2
    remote "bash -s -- $(quote "$DEPLOY_DIR" "$DEPLOY_DATA_DIR" "$DEPLOY_SERVICE")" <<'REMOTE'
set -euo pipefail
dir=$1
data=$2
service=$3
if ! sudo test -f "$dir/proscenium-server.previous"; then
    echo "No previous binary to restore; the new binary stays installed." >&2
    exit 0
fi
sudo systemctl stop "$service" || true
# The copies were taken with the unit stopped. A file without a copy did not exist then.
for file in server.db server.db-wal server.db-shm; do
    if sudo test -f "$data/$file.previous"; then
        sudo mv -f -- "$data/$file.previous" "$data/$file"
    else
        sudo rm -f -- "$data/$file"
    fi
done
sudo mv -f -- "$dir/proscenium-server.previous" "$dir/proscenium-server"
sudo systemctl start "$service"
echo "Restored the previous binary and database and started $service." >&2
REMOTE
    exit 1
fi

echo "Deploy complete."
