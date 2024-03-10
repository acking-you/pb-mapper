#!/bin/bash
SCRIPT_DIR=$(cd `dirname $0`; pwd)
export PROJECT_DIR="$SCRIPT_DIR/../.."
export BIN_NAME="pb-mapper-server"
export TARGET_NAME="x86_64-unknown-linux-musl"

bash $SCRIPT_DIR/cross-build.sh