---
name: pb-mapper-connect-deploy
description: Deploy or upgrade the unified `pb-mapper` binary on a remote Linux host, run its `connect` command as a managed `systemd` tunnel service, and verify the exposed path end to end.
---

# Pb Mapper Connect Deploy

## Overview

Use this workflow to perform reproducible remote deployment of `pb-mapper connect` with minimal manual edits.
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
| `SERVICE_KEY` | pb-mapper service name to subscribe | — | Required |
| `LISTEN_IP` | Listen on localhost only (`127.0.0.1`) or all interfaces (`0.0.0.0`)? | `127.0.0.1` | Required |
| `LISTEN_PORT` | Local listening port on remote host | — | Required |
| `MSG_HEADER_KEY` | Encryption key (exactly 32 chars) | *(empty)* | Optional, **confidential** — never log or echo |
| `PUBLIC_CHECK_URL` | URL for external validation | *(empty)* | Optional |
| `VERSION` | Release version (without `v` prefix) | Latest release | Auto-detect or user-specified |
| `TARGET_TRIPLE` | Build target | `x86_64-unknown-linux-musl` | User can override |
| `PB_SERVER` | pb-mapper server address | `${SSH_HOST}:7666` | Derived by default, user can override |

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
export LOCAL_ADDR="${LISTEN_IP}:${LISTEN_PORT}"
export PB_SERVER="${PB_SERVER:-${SSH_HOST}:7666}"
export INSTANCE_NAME="${SERVICE_KEY}"
```

## Deployment Workflow

### 1. Prepare local artifact

Use deterministic file names:

```bash
export ASSET_FILE="pb-mapper-${TARGET_TRIPLE}.tar.gz"
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
TMP_DIR="/tmp/pb-mapper-install"
INSTALL_DIR="/usr/local/bin"

rm -rf "${TMP_DIR}"
mkdir -p "${TMP_DIR}"
tar -xzf "/tmp/${ASSET_FILE}" -C "${TMP_DIR}"

BIN_PATH="$(find "${TMP_DIR}" -type f -name pb-mapper -print -quit)"
if [ -z "${BIN_PATH}" ]; then
  echo "pb-mapper binary not found in archive" >&2
  exit 1
fi

sudo install -d "${INSTALL_DIR}"
sudo install -m 0755 "${BIN_PATH}" "${INSTALL_DIR}/pb-mapper"
sudo rm -rf "${TMP_DIR}" "/tmp/${ASSET_FILE}"
REMOTE_INSTALL
```

### 3. Create or update systemd unit

Use a templated service so multiple tunnel instances can coexist:

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "sudo bash -s" <<'REMOTE_SYSTEMD'
set -euo pipefail
SERVICE_TEMPLATE="/etc/systemd/system/pb-mapper-connect@.service"
sudo tee "${SERVICE_TEMPLATE}" >/dev/null <<'UNIT'
[Unit]
Description=pb-mapper client tunnel (%i)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/pb-mapper/connect/%i.env
ExecStart=/usr/local/bin/pb-mapper connect tcp \
  --server ${PB_SERVER} \
  --key ${SERVICE_KEY} \
  --addr ${LOCAL_ADDR}
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
UNIT

sudo install -d /etc/pb-mapper/connect
REMOTE_SYSTEMD
```

### 4. Write instance env file and start service

`MSG_HEADER_KEY` must be omitted when empty; never write an empty value to env file.

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" \
  "INSTANCE_NAME='${INSTANCE_NAME}' PB_SERVER='${PB_SERVER}' SERVICE_KEY='${SERVICE_KEY}' LOCAL_ADDR='${LOCAL_ADDR}' MSG_HEADER_KEY='${MSG_HEADER_KEY}' sudo bash -s" <<'REMOTE_ENV'
set -euo pipefail

ENV_FILE="/etc/pb-mapper/connect/${INSTANCE_NAME}.env"
sudo tee "${ENV_FILE}" >/dev/null <<EOF
PB_SERVER=${PB_SERVER}
SERVICE_KEY=${SERVICE_KEY}
LOCAL_ADDR=${LOCAL_ADDR}
RUST_LOG=info
PB_MAPPER_KEEP_ALIVE=ON
EOF

if [ -n "${MSG_HEADER_KEY}" ]; then
  CLEAN_KEY="$(printf '%s' "${MSG_HEADER_KEY}" | tr -d '\r\n')"
  if [ "${#CLEAN_KEY}" -ne 32 ]; then
    echo "MSG_HEADER_KEY must be exactly 32 characters" >&2
    exit 1
  fi
  echo "MSG_HEADER_KEY=${CLEAN_KEY}" | sudo tee -a "${ENV_FILE}" >/dev/null
fi

sudo systemctl daemon-reload
sudo systemctl enable --now "pb-mapper-connect@${INSTANCE_NAME}.service"
REMOTE_ENV
```

### 5. Validate tunnel and external path

Run all checks:

```bash
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "sudo systemctl --no-pager --full status pb-mapper-connect@${INSTANCE_NAME}.service"
ssh ${SSH_PORT_OPT} "${SSH_TARGET}" "curl -fsS 'http://${LOCAL_ADDR}/' | head -c 512"
if [ -n "${PUBLIC_CHECK_URL}" ]; then
  curl -fsS "${PUBLIC_CHECK_URL}" | head -c 512
fi
```

If `jq` is available, pipe through `jq .` for formatted JSON output.

## Troubleshooting Checklist

Use this quick triage when startup or forwarding fails:

- `datalen not valid`: likely `MSG_HEADER_KEY` mismatch or hidden newline; verify both sides use the same 32-byte key.
- Service restarts immediately: inspect logs with `journalctl -u pb-mapper-connect@${INSTANCE_NAME} -n 200 --no-pager`.
- Remote port not listening: confirm `LOCAL_ADDR` host/port and no port conflict.
- Public URL fails but localhost works: investigate reverse proxy (for example, Caddy route/TLS config).

## Safe Update Procedure

Upgrade in place without changing the unit:

1. Download/upload new `vX.Y.Z` artifact.
2. Re-run install step to overwrite `/usr/local/bin/pb-mapper`.
3. Restart instance: `sudo systemctl restart pb-mapper-connect@${INSTANCE_NAME}.service`.
4. Re-run validation checks.
