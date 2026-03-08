#!/usr/bin/env bash
set -euo pipefail

HOST="ec2-user@54.201.25.68"
KEY="$HOME/.ssh/iroh-social-server.pem"
SSH="ssh -o IdentitiesOnly=yes -i $KEY $HOST"
SCP="scp -o IdentitiesOnly=yes -i $KEY"
BINARY="server/target/x86_64-unknown-linux-musl/release/iroh-social-server"

echo "Building server (musl)..."
cargo build --release --target x86_64-unknown-linux-musl --manifest-path server/Cargo.toml

echo "Uploading binary..."
$SCP "$BINARY" "$HOST:/tmp/iroh-social-server"

echo "Replacing and restarting..."
$SSH "sudo mv /tmp/iroh-social-server /opt/iroh-social/iroh-social-server && sudo chmod +x /opt/iroh-social/iroh-social-server && sudo systemctl restart iroh-social"

echo "Verifying..."
sleep 2
$SSH "sudo systemctl is-active iroh-social"
curl -sf "http://54.201.25.68:3000/api/v1/info" | python3 -m json.tool

echo "Deploy complete."
