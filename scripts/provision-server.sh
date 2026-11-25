#!/usr/bin/env bash
# Prepare a fresh Debian or Ubuntu host for the discovery server, once: install Caddy from
# its official package repository as the HTTPS reverse proxy, open its ports and UDP in
# ufw if ufw is active, create the service user, the install directory, the config and
# the systemd unit, install and start the server binary, and check that it answers on
# 127.0.0.1. Later updates use deploy-server.sh.
#
# Running it again changes nothing that exists: an existing config, binary or database is
# kept. The Caddyfile and the unit file are written again.
#
# Required environment:
#   PROVISION_HOST     ssh destination with root or sudo, for example root@203.0.113.7
#   PUBLIC_HOSTNAME    DNS name of the server, for example discovery.example.org. Caddy
#                      obtains its certificate once the name resolves to the host.
# Optional environment:
#   PROVISION_SSH_KEY  ssh private key file (default: ssh's own key selection)
#   SERVER_NAME        name shown to clients (default: the host name)
#   SERVER_DESCRIPTION description shown to clients (default: empty)
#   DEPLOY_DIR         install directory (default: /opt/proscenium)
#   DEPLOY_SERVICE     systemd unit name (default: proscenium)
#   SERVER_PORT        loopback port of the server behind Caddy (default: 3000)
set -euo pipefail

: "${PROVISION_HOST:?set PROVISION_HOST to the ssh destination (user@host)}"
: "${PUBLIC_HOSTNAME:?set PUBLIC_HOSTNAME to the DNS name of the server}"
SERVER_NAME="${SERVER_NAME:-$PUBLIC_HOSTNAME}"
SERVER_DESCRIPTION="${SERVER_DESCRIPTION:-}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/proscenium}"
DEPLOY_SERVICE="${DEPLOY_SERVICE:-proscenium}"
SERVER_PORT="${SERVER_PORT:-3000}"

if [[ ! "$PUBLIC_HOSTNAME" =~ ^[A-Za-z0-9.-]+$ ]]; then
    echo "PUBLIC_HOSTNAME is not a host name: $PUBLIC_HOSTNAME" >&2
    exit 1
fi
if [[ ! "$SERVER_PORT" =~ ^[0-9]+$ ]]; then
    echo "SERVER_PORT is not a number: $SERVER_PORT" >&2
    exit 1
fi

SSH_OPTS=()
if [[ -n "${PROVISION_SSH_KEY:-}" ]]; then
    SSH_OPTS=(-o IdentitiesOnly=yes -i "$PROVISION_SSH_KEY")
fi
remote() {
    ssh ${SSH_OPTS[@]+"${SSH_OPTS[@]}"} "$PROVISION_HOST" "$@"
}
quote() {
    printf '%q ' "$@"
}

cd "$(dirname "${BASH_SOURCE[0]}")/.."
BINARY="server/target/x86_64-unknown-linux-musl/release/proscenium-server"

echo "Building server (musl)..."
cargo build --release --target x86_64-unknown-linux-musl --manifest-path server/Cargo.toml
CHECKSUM="$(sha256sum "$BINARY" | cut -d' ' -f1)"

echo "Uploading binary..."
REMOTE_TMP="$(remote mktemp -d)"
if [[ "$REMOTE_TMP" != /* ]]; then
    echo "unexpected mktemp output from host: $REMOTE_TMP" >&2
    exit 1
fi
cleanup() {
    remote "rm -rf -- $(quote "$REMOTE_TMP")" || true
}
trap cleanup EXIT
scp ${SSH_OPTS[@]+"${SSH_OPTS[@]}"} "$BINARY" "$PROVISION_HOST:$REMOTE_TMP/proscenium-server"

echo "Provisioning $PROVISION_HOST..."
remote "bash -s -- $(quote "$REMOTE_TMP" "$CHECKSUM" "$PUBLIC_HOSTNAME" "$SERVER_NAME" \
    "$SERVER_DESCRIPTION" "$DEPLOY_DIR" "$DEPLOY_SERVICE" "$SERVER_PORT")" <<'REMOTE'
set -euo pipefail
tmp=$1
checksum=$2
hostname=$3
name=$4
description=$5
dir=$6
service=$7
port=$8

# apt-get without questions; sudo does not pass the caller's environment.
apt_get() {
    sudo env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a apt-get -q "$@"
}

echo "$checksum  $tmp/proscenium-server" | sha256sum --check --status -

# TOML basic string: backslash and double quote escaped.
toml_string() {
    local value=${1//\\/\\\\}
    printf '"%s"' "${value//\"/\\\"}"
}

echo "Updating packages..."
apt_get update
apt_get full-upgrade -y

if ! command -v caddy >/dev/null; then
    echo "Installing Caddy from its package repository..."
    apt_get install -y debian-keyring debian-archive-keyring apt-transport-https curl gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
        | sudo gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
        | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
    sudo chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    sudo chmod o+r /etc/apt/sources.list.d/caddy-stable.list
    apt_get update
    apt_get install -y caddy
fi

# Some provider images enable ufw with only SSH allowed. Caddy needs 80 (ACME) and 443
# (TCP and UDP for HTTP/3); iroh binds a random UDP port.
if command -v ufw >/dev/null && sudo ufw status | grep -q "^Status: active"; then
    sudo ufw allow 80/tcp comment "http, ACME" >/dev/null
    sudo ufw allow 443/tcp comment "https" >/dev/null
    sudo ufw allow 1:65535/udp comment "iroh QUIC, HTTP/3" >/dev/null
fi

if ! id -u proscenium >/dev/null 2>&1; then
    sudo useradd --system --home-dir "$dir" --no-create-home --shell /usr/sbin/nologin proscenium
fi
sudo install -d -o root -g root -m 0755 "$dir"
sudo install -d -o proscenium -g proscenium -m 0750 "$dir/data"

if ! sudo test -f "$dir/config.toml"; then
    config=$(mktemp)
    cat >"$config" <<CONFIG
[server]
name = $(toml_string "$name")
description = $(toml_string "$description")
listen_addr = "127.0.0.1:$port"
data_dir = "$dir/data"
public_url = "https://$hostname"
registration_open = true
CONFIG
    sudo install -o root -g proscenium -m 0640 "$config" "$dir/config.toml"
    rm -f "$config"
fi

unit=$(mktemp)
cat >"$unit" <<UNIT
[Unit]
Description=Proscenium discovery server
After=network-online.target
Wants=network-online.target

[Service]
User=proscenium
Group=proscenium
WorkingDirectory=$dir
ExecStart=$dir/proscenium-server --config $dir/config.toml --data-dir $dir/data
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=$dir/data

[Install]
WantedBy=multi-user.target
UNIT
sudo install -o root -g root -m 0644 "$unit" "/etc/systemd/system/$service.service"
rm -f "$unit"
sudo systemctl daemon-reload
sudo systemctl enable "$service"

if ! sudo test -f "$dir/proscenium-server"; then
    sudo install -o root -g root -m 0755 -- "$tmp/proscenium-server" "$dir/proscenium-server.new"
    sudo mv -f -- "$dir/proscenium-server.new" "$dir/proscenium-server"
fi
sudo systemctl restart "$service"

echo "Checking http://127.0.0.1:$port/api/v1/info on the host..."
for _ in $(seq 1 15); do
    if body=$(curl --fail --silent --max-time 5 "http://127.0.0.1:$port/api/v1/info"); then
        echo "$body"
        break
    fi
    sleep 2
done
if [[ -z "${body:-}" ]]; then
    echo "The server does not answer; see: journalctl -u $service" >&2
    exit 1
fi

caddyfile=$(mktemp)
cat >"$caddyfile" <<CADDY
$hostname {
	reverse_proxy 127.0.0.1:$port
}
CADDY
sudo install -o root -g root -m 0644 "$caddyfile" /etc/caddy/Caddyfile
rm -f "$caddyfile"
sudo systemctl enable caddy
sudo systemctl reload-or-restart caddy
echo "Caddy serves https://$hostname once the name resolves to this host."
REMOTE

echo "Provisioning complete."
