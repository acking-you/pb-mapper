#!/usr/bin/env bash
set -euo pipefail

# Configuration
VERSION="0.2.1"
ARCH="x86_64-unknown-linux-musl"
TARBALL="pb-mapper-server-v${VERSION}-${ARCH}.tar.gz"
DOWNLOAD_URL="https://gitee.com/acking-you/pb-mapper/releases/download/v${VERSION}/${TARBALL}"
INSTALL_DIR="/opt/pb-mapper-server"
SERVICE_NAME="pb-mapper-server"
SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
PORT="7666"

# Re-run with sudo if needed
if [ "${EUID:-$(id -u)}" -ne 0 ]; then
  if command -v sudo >/dev/null 2>&1; then
    exec sudo -E bash "$0" "$@"
  fi
  echo "This script must be run as root." >&2
  exit 1
fi

# Verify required tools exist
for cmd in tar systemctl; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing required command: $cmd" >&2
    exit 1
  fi
done

# Choose a downloader
if command -v curl >/dev/null 2>&1; then
  DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER="wget"
else
  echo "Missing required command: curl or wget" >&2
  exit 1
fi

# Prepare temp workspace
TMP_DIR=$(mktemp -d)
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

ARCHIVE_PATH="${TMP_DIR}/${TARBALL}"

# Download release archive
if [ "$DOWNLOADER" = "curl" ]; then
  curl -fL --retry 3 --connect-timeout 10 --max-time 300 -o "$ARCHIVE_PATH" "$DOWNLOAD_URL"
else
  wget -O "$ARCHIVE_PATH" "$DOWNLOAD_URL"
fi

# Extract and locate binary
mkdir -p "$TMP_DIR/extract"
tar -xzf "$ARCHIVE_PATH" -C "$TMP_DIR/extract"
BIN_PATH=$(find "$TMP_DIR/extract" -type f -name "pb-mapper-server" -perm -u+x | head -n 1)
if [ -z "$BIN_PATH" ]; then
  echo "pb-mapper-server binary not found in archive." >&2
  exit 1
fi

# Install binary
mkdir -p "$INSTALL_DIR"
install -m 0755 "$BIN_PATH" "${INSTALL_DIR}/pb-mapper-server"

# Stop and remove existing service if present
if systemctl is-active --quiet "${SERVICE_NAME}.service"; then
  systemctl stop "${SERVICE_NAME}.service"
fi
if systemctl is-enabled --quiet "${SERVICE_NAME}.service"; then
  systemctl disable "${SERVICE_NAME}.service"
fi
if [ -f "$SERVICE_PATH" ]; then
  rm -f "$SERVICE_PATH"
fi

# Write systemd unit
cat > "$SERVICE_PATH" <<UNIT
[Unit]
Description=pb-mapper server
After=network.target

[Service]
Type=simple
WorkingDirectory=${INSTALL_DIR}
ExecStart=${INSTALL_DIR}/pb-mapper-server --pb-mapper-port ${PORT} --use-machine-msg-header-key
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
UNIT

# Reload systemd and start service
systemctl daemon-reload
systemctl enable --now "${SERVICE_NAME}.service"

echo "pb-mapper-server is installed and running."
echo "Service name: ${SERVICE_NAME}.service"
echo "Machine-derived key file: /var/lib/pb-mapper-server/msg_header_key"
