#!/bin/bash
SCRIPT_DIR=$(cd `dirname $0`; pwd)
export PROJECT_DIR="$SCRIPT_DIR/../.."
export BIN_NAME="pb-mapper-server"

bash $SCRIPT_DIR/build.sh