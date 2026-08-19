#!/usr/bin/env bash
set -euo pipefail

# Configuration
VERSION="${PB_MAPPER_VERSION:-0.4.0}"
ARCH="${PB_MAPPER_ARCH:-x86_64-unknown-linux-musl}"
TARBALL="pb-mapper-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/acking-you/pb-mapper/releases/download/v${VERSION}/${TARBALL}"
INSTALL_DIR="/usr/local/bin"
SERVICE_NAME="pb-mapper-server"
SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
PORT="${PB_MAPPER_PORT:-7666}"
AUTH_DIR="/var/lib/pb-mapper/auth"
ADMIN_KEY_PATH="${AUTH_DIR}/admin.key"
LEGACY_KEY_PATH="/var/lib/pb-mapper-server/msg_header_key"
SERVER_ENV_FILE="/etc/pb-mapper/server.env"

admin_key_is_env_safe() {
  local key="$1"
  local bytes
  bytes=$(printf '%s' "$key" | wc -c)
  [ "$bytes" -eq 32 ] || return 1
  printf '%s' "$key" | LC_ALL=C grep -qx '[[:graph:]]\{32\}'
}

configured_msg_header_key() {
  if [ -n "${MSG_HEADER_KEY:-}" ]; then
    printf '%s' "$MSG_HEADER_KEY"
    return 0
  fi
  if [ ! -f "$SERVER_ENV_FILE" ]; then
    return 0
  fi
  awk -F= '
    $1 ~ /^[[:space:]]*#/ { next }
    $1 ~ /^[[:space:]]*MSG_HEADER_KEY[[:space:]]*$/ {
      val = substr($0, index($0, "=") + 1)
      sub(/\r$/, "", val)
      key = val
    }
    END { printf "%s", key }
  ' "$SERVER_ENV_FILE"
}

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
BIN_PATH=$(find "$TMP_DIR/extract" -type f -name "pb-mapper" -perm -u+x -print -quit)
if [ -z "$BIN_PATH" ]; then
  echo "pb-mapper binary not found in archive." >&2
  exit 1
fi

# Install binary
mkdir -p "$INSTALL_DIR"
install -m 0755 "$BIN_PATH" "${INSTALL_DIR}/pb-mapper"

# Preserve the former machine-derived credential on upgrade only when neither
# admin.key nor an explicit MSG_HEADER_KEY is already configured. An explicit
# key in the environment or /etc/pb-mapper/server.env must win; otherwise the
# runtime would prefer the newly copied admin.key and lock operators out.
install -d -m 0700 "$AUTH_DIR"
INSTALLER_KEY="$(configured_msg_header_key)"
if [ -n "$INSTALLER_KEY" ] && [ ! -s "$ADMIN_KEY_PATH" ]; then
  case "$INSTALLER_KEY" in
    pbmt1_*)
      echo "MSG_HEADER_KEY is a temporary credential; write a 32-character administrator key to $ADMIN_KEY_PATH" >&2
      exit 1
      ;;
  esac
  if ! admin_key_is_env_safe "$INSTALLER_KEY"; then
    echo "MSG_HEADER_KEY must be exactly 32 printable ASCII bytes without whitespace or NUL" >&2
    exit 1
  fi
  if [ -s "${AUTH_DIR}/auth.snapshot" ] || [ -s "${AUTH_DIR}/auth.wal" ]; then
    echo "Leaving $ADMIN_KEY_PATH unset so the service can verify MSG_HEADER_KEY against existing authentication state"
  else
    printf '%s\n' "$INSTALLER_KEY" > "$ADMIN_KEY_PATH"
    chmod 0600 "$ADMIN_KEY_PATH"
    echo "Persisted installer MSG_HEADER_KEY to $ADMIN_KEY_PATH"
  fi
elif [ -z "$INSTALLER_KEY" ] && [ ! -s "$ADMIN_KEY_PATH" ] && [ -s "$LEGACY_KEY_PATH" ]; then
  if [ -s "${AUTH_DIR}/auth.snapshot" ] || [ -s "${AUTH_DIR}/auth.wal" ]; then
    echo "Leaving $ADMIN_KEY_PATH unset so the service can verify the legacy key against existing authentication state"
  else
    install -m 0600 "$LEGACY_KEY_PATH" "$ADMIN_KEY_PATH"
    echo "Migrated the legacy machine-derived key into $ADMIN_KEY_PATH"
  fi
fi

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
ExecStart=${INSTALL_DIR}/pb-mapper server --port ${PORT}
Environment=RUST_LOG=info
EnvironmentFile=-/etc/pb-mapper/server.env
StateDirectory=pb-mapper
StateDirectoryMode=0700
Restart=on-failure
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
UNIT

# Reload systemd and start service
systemctl daemon-reload
systemctl enable --now "${SERVICE_NAME}.service"

echo "pb-mapper server is installed and running."
echo "Service name: ${SERVICE_NAME}.service"
echo "Administrator key file: /var/lib/pb-mapper/auth/admin.key"
echo "Read it locally as root and issue temporary credentials for register/connect clients."
