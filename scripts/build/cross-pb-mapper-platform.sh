#!/bin/bash
# check args num
if [ "$#" -ne 2 ]; then
    echo "Usage: $0 [BIN_NAME] [PLATFORM]"
    exit 1
fi

export BIN_NAME=$1
PLATFORM=$2
SCRIPT_DIR=$(cd `dirname $0`; pwd)
export PROJECT_DIR="$SCRIPT_DIR/../.."

case $PLATFORM in
    windows)
        echo "build for windows(x86-64)..."
        export TARGET_NAME="x86_64-pc-windows-gnu"
        ;;
    macos-x86)
        echo "build for macos(x86-64)..."
        export TARGET_NAME="x86_64-apple-darwin"
        ;;
    macos-arm)
        echo "build for macos(x86-64)..."
        export TARGET_NAME="aarch64-apple-darwin"
        ;;
    linux-x86)
        echo "build for linux(x86-64)..."
        export TARGET_NAME="x86_64-unknown-linux-musl"
        ;;
# Provides support for most Android phones as well as router devices
    linux-armv7)
        echo "build for linux(armv7)..."
        export TARGET_NAME="armv7-unknown-linux-musleabi"
        ;;
    *)
        echo "Error: Unsupported platform '$PLATFORM'."
        exit 2
        ;;
esac

bash $SCRIPT_DIR/cross-build.sh