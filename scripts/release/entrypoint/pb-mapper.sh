#!/usr/bin/env bash
set -euo pipefail

# check `PB_MAPPER_PORT`
if [ -z "${PB_MAPPER_PORT:-}" ]; then
  echo "Error: PB_MAPPER_PORT is not set."
  exit 1
fi

USE_MACHINE_MSG_HEADER_KEY=${USE_MACHINE_MSG_HEADER_KEY:-false}

ARGS=(-p "$PB_MAPPER_PORT")
AUTH_DIR="${PB_MAPPER_AUTH_STATE_DIR:-/var/lib/pb-mapper/auth}"
ADMIN_KEY_PATH="$AUTH_DIR/admin.key"
LEGACY_KEY_PATH="/var/lib/pb-mapper-server/msg_header_key"

install -d -m 0700 "$AUTH_DIR"
if [ -z "${MSG_HEADER_KEY:-}" ] && [ ! -s "$ADMIN_KEY_PATH" ] && [ -s "$LEGACY_KEY_PATH" ]; then
  if [ -s "$AUTH_DIR/auth.snapshot" ] || [ -s "$AUTH_DIR/auth.wal" ]; then
    echo "Leaving $ADMIN_KEY_PATH unset so the service can verify the legacy key against existing authentication state"
  else
    install -m 0600 "$LEGACY_KEY_PATH" "$ADMIN_KEY_PATH"
    echo "Migrated the legacy machine-derived key into $ADMIN_KEY_PATH"
  fi
fi

if [ "${USE_IPV6:-false}" = "true" ]; then
  echo "USE_IPV6 is set to true"
  ARGS+=(--ipv6)
else
  echo "USE_IPV6 is set to false or is not set"
fi

if [ "$USE_MACHINE_MSG_HEADER_KEY" = "true" ]; then
  if [ -s "$ADMIN_KEY_PATH" ]; then
    echo "admin.key already exists; skipping --use-machine-msg-header-key"
  else
    echo "WARNING: USE_MACHINE_MSG_HEADER_KEY is a legacy compatibility mode"
    ARGS+=(--use-machine-msg-header-key)
  fi
else
  echo "USE_MACHINE_MSG_HEADER_KEY is set to false"
fi

if [ -n "${MSG_HEADER_KEY:-}" ]; then
  echo "Using the configured administrator credential"
else
  echo "Using or initializing $ADMIN_KEY_PATH"
fi

exec ./pb-mapper server "${ARGS[@]}"
