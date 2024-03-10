#!/bin/bash

# check `PROJECT_DIR`
if [ -z "$PROJECT_DIR" ]; then
  echo "Error: PROJECT_DIR is not set."
  exit 1
fi

# check `BIN_NAME`
if [ -z "$BIN_NAME" ]; then
  echo "Error: BIN_NAME is not set."
  exit 1
fi

# check cargo command
if ! command -v cargo > /dev/null 2>&1; then
  echo "Error：`cargo` not install." >&2
  exit 1
fi

cd $PROJECT_DIR

echo "Build bin:$BIN_NAME (Release)..."
cargo build --bin $BIN_NAME --release
echo "Build bin:$BIN_NAME success!"