#!/usr/bin/env bash
set -euo pipefail

# check `PB_MAPPER_PORT`
if [ -z "${PB_MAPPER_PORT:-}" ]; then
  echo "Error: PB_MAPPER_PORT is not set."
  exit 1
fi

USE_MACHINE_MSG_HEADER_KEY=${USE_MACHINE_MSG_HEADER_KEY:-true}

ARGS=(-p "$PB_MAPPER_PORT")

if [ "${USE_IPV6:-false}" = "true" ]; then
  echo "USE_IPV6 is set to true"
  ARGS+=(--ipv6)
else
  echo "USE_IPV6 is set to false or is not set"
fi

if [ "$USE_MACHINE_MSG_HEADER_KEY" = "true" ]; then
  echo "USE_MACHINE_MSG_HEADER_KEY is set to true"
  ARGS+=(--use-machine-msg-header-key)
else
  echo "USE_MACHINE_MSG_HEADER_KEY is set to false"
fi

exec ./pb-mapper server "${ARGS[@]}"
