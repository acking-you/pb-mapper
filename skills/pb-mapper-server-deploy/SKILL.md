---
name: pb-mapper-server-deploy
description: Deploy or upgrade `pb-mapper-server` on a remote Linux host through SSH, install it as a managed `systemd` service, and verify the server is listening. Use when users ask to set up the central pb-mapper server (download locally, upload remotely, run as service, and health-check). This avoids requiring GitHub access on the remote host.
---

# Pb Mapper Server Deploy

## Overview

Use this workflow to deploy `pb-mapper-server` on a remote host without requiring the remote host to have GitHub access.
All artifacts are downloaded on the local machine first, then uploaded via SCP.
Execute from the operator machine that has SSH access to the target host.

## Pre-flight: SSH Access

Before starting, verify SSH connectivity to the target host:

1. Confirm passwordless SSH login is configured. Test with:
   ```bash
   ssh <user>@<host> "echo ok"
   ```
   If this fails or prompts for a password, configure key-based auth first:
   ```bash
   ssh-copy-id <user>@<host>
   ```

2. Ask the user: **Is the SSH port the default 22?** If not, collect the custom port number.
   All subsequent `ssh`/`scp` commands use `${SSH_PORT_OPT}` / `${SCP_PORT_OPT}` which expand to `-p <port>` / `-P <port>` when non-default.

## Required Inputs (Interactive Collection)

Prompt the user for each value below. Do NOT assume or hardcode any value.

| Variable | Prompt | Default | Notes |
|---|---|---|---|
| `SSH_USER` | Remote host username | — | Required |
| `SSH_HOST` | Remote host IP or domain | — | Required |
| `SSH_PORT` | SSH port | `22` | Only ask if non-default |
| `SERVER_PORT` | pb-mapper-server listening port | `7666` | User can override |
| `USE_IPV6` | Listen on IPv6 (`::`) instead of IPv4? | `false` | Optional |
| `ENABLE_KEEP_ALIVE` | Enable TCP keep-alive? | `false` | Optional |
| `USE_MACHINE_KEY` | Use machine-derived msg header key? | `true` | Recommended; generates and persists a 32-char key |
| `VERSION` | Release version (without `v` prefix) | Latest release | Auto-detect or user-specified |
| `TARGET_TRIPLE` | Build target | `x86_64-unknown-linux-musl` | User can override |

### Derived Variables

After collection, compute:

```bash
export SSH_TARGET="${SSH_USER}@${SSH_HOST}"
export SSH_PORT_OPT=""
export SCP_PORT_OPT=""
if [ "${SSH_PORT}" != "22" ]; then
  export SSH_PORT_OPT="-p ${SSH_PORT}"
  export SCP_PORT_OPT="-P ${SSH_PORT}"
fi
```

Build the `ExecStart` flags dynamically:

```bash
EXTRA_FLAGS=""
if [ "${USE_IPV6}" = "true" ]; then
  EXTRA_FLAGS="${EXTRA_FLAGS} --use-ipv6"
fi
if [ "${ENABLE_KEEP_ALIVE}" = "true" ]; then
  EXTRA_FLAGS="${EXTRA_FLAGS} --keep-alive"
fi
if [ "${USE_MACHINE_KEY}" = "true" ]; then
  EXTRA_FLAGS="${EXTRA_FLAGS} --use-machine-msg-header-key"
fi
export EXTRA_FLAGS
```

## Deployment Workflow

### 1. Prepare local artifact

Download the release tarball on the local machine:

```bash
export ASSET_FILE="pb-mapper-server-v${VERSION}-${TARGET_TRIPLE}.tar.gz"
export ASSET_URL="https://github.com/acking-you/pb-mapper/releases/download/v${VERSION}/${ASSET_FILE}"

curl -fL "${ASSET_URL}" -o "/tmp/${ASSET_FILE}"
test -s "/tmp/${ASSET_FILE}"
```

**Proxy fallback:** If the download fails due to network issues (timeout, connection refused), ask the user for their local proxy port and retry:

```bash
export PROXY_PORT="<user-provided>"
curl -fL -x "http://127.0.0.1:${PROXY_PORT}" "${ASSET_URL}" -o "/tmp/${ASSET_FILE}"
test -s "/tmp/${ASSET_FILE}"
```

### 2. Upload and install binary on remote host

```bash
scp ${SCP_PORT_OPT} "/tmp/${ASSET_FILE}" "${SSH_TARGET}:/tmp/${ASSET_FILE}"

ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "ASSET_FILE='${ASSET_FILE}' sudo bash -s" <<'REMOTE_INSTALL'
set -euo pipefail
TMP_DIR="/tmp/pb-mapper-server-install"
INSTALL_DIR="/opt/pb-mapper-server"

rm -rf "${TMP_DIR}"
mkdir -p "${TMP_DIR}"
tar -xzf "/tmp/${ASSET_FILE}" -C "${TMP_DIR}"

BIN_PATH="$(find "${TMP_DIR}" -type f -name pb-mapper-server | head -n 1)"
if [ -z "${BIN_PATH}" ]; then
  echo "pb-mapper-server binary not found in archive" >&2
  exit 1
fi

sudo install -d "${INSTALL_DIR}"
sudo install -m 0755 "${BIN_PATH}" "${INSTALL_DIR}/pb-mapper-server"
sudo rm -rf "${TMP_DIR}" "/tmp/${ASSET_FILE}"
echo "Installed: $(${INSTALL_DIR}/pb-mapper-server --version 2>/dev/null || echo 'ok')"
REMOTE_INSTALL
```

### 3. Create or update systemd unit

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" \
  "SERVER_PORT='${SERVER_PORT}' EXTRA_FLAGS='${EXTRA_FLAGS}' sudo bash -s" <<'REMOTE_SYSTEMD'
set -euo pipefail

sudo tee /etc/systemd/system/pb-mapper-server.service >/dev/null <<UNIT
[Unit]
Description=pb-mapper server
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/pb-mapper-server
ExecStart=/opt/pb-mapper-server/pb-mapper-server --pb-mapper-port ${SERVER_PORT} ${EXTRA_FLAGS}
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
UNIT

sudo systemctl daemon-reload
REMOTE_SYSTEMD
```

### 4. Enable and start service

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "sudo systemctl enable --now pb-mapper-server.service"
```

### 5. Validate deployment

Run all checks:

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "sudo systemctl --no-pager --full status pb-mapper-server.service"
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "ss -tlnp | grep ':${SERVER_PORT}'"
```

If `USE_MACHINE_KEY=true`, retrieve the generated key for use with server-cli / client-cli:

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "sudo cat /var/lib/pb-mapper-server/msg_header_key"
```

Store this key securely — it is needed by `pb-mapper-server-cli` and `pb-mapper-client-cli` when `--codec` or `MSG_HEADER_KEY` is used.

## Troubleshooting Checklist

Use this quick triage when the server fails to start or accept connections:

- Port already in use: check with `ss -tlnp | grep :${SERVER_PORT}` and stop the conflicting process.
- Firewall blocking: ensure the server port is open (`ufw allow ${SERVER_PORT}/tcp` or equivalent).
- Service crashes on start: inspect logs with `journalctl -u pb-mapper-server -n 200 --no-pager`.
- Machine key not generated: verify `/var/lib/pb-mapper-server/` directory exists and is writable; check logs for key derivation errors.
- Permission denied: binary must be owned by root with `0755` permissions.

## Safe Update Procedure

Upgrade in place without changing the unit:

1. Download/upload new `vX.Y.Z` artifact.
2. Re-run install step to overwrite `/opt/pb-mapper-server/pb-mapper-server`.
3. Restart service: `sudo systemctl restart pb-mapper-server.service`.
4. Re-run validation checks.
