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
# check `TARGET_NAME`
if [ -z "$TARGET_NAME" ]; then
  echo "Error: TARGET_NAME is not set."
  exit 1
fi

# check cross command
if ! command -v cross > /dev/null 2>&1; then
  echo "Error：`cross` not install." >&2
  exit 1
fi

cd $PROJECT_DIR

echo "Build bin:$BIN_NAME target:$TARGET_NAME(Release)..."
cross build --bin $BIN_NAME --target $TARGET_NAME --release
echo "Build bin:$BIN_NAME target:$TARGET_NAME success!"