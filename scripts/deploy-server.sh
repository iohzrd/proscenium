#!/usr/bin/env bash
set -euo pipefail

HOST="ec2-user@54.201.25.68"
KEY="$HOME/.ssh/proscenium-server.pem"
SSH="ssh -o IdentitiesOnly=yes -i $KEY $HOST"
SCP="scp -o IdentitiesOnly=yes -i $KEY"
BINARY="server/target/x86_64-unknown-linux-musl/release/proscenium-server"

echo "Building server (musl)..."
cargo build --release --target x86_64-unknown-linux-musl --manifest-path server/Cargo.toml

echo "Uploading binary..."
$SCP "$BINARY" "$HOST:/tmp/proscenium-server"

echo "Replacing and restarting..."
$SSH "sudo mv /tmp/proscenium-server /opt/proscenium/proscenium-server && sudo chmod +x /opt/proscenium/proscenium-server && sudo systemctl restart proscenium"

echo "Verifying..."
sleep 2
$SSH "sudo systemctl is-active proscenium"
curl -sf "http://54.201.25.68:3000/api/v1/info" | python3 -m json.tool

echo "Deploy complete."
