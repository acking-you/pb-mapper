#!/bin/bash
TAG ?= dev

build-pb-mapper-server:
	bash ./scripts/build/pb-mapper-server.sh

build-pb-mapper-server-x86_64_musl:
	bash ./scripts/build/pb-mapper-server-x86_64_linux_musl.sh

# Build the Linux FFI library and stage it for Flutter.
build-pb-mapper-ffi-linux:
	bash ./scripts/build/pb-mapper-ffi-linux.sh

# Build the macOS FFI library and stage it for Flutter.
build-pb-mapper-ffi-macos:
	bash ./scripts/build/pb-mapper-ffi-macos.sh

# Build the Windows FFI library and stage it for Flutter (requires pwsh).
build-pb-mapper-ffi-windows:
	pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build/pb-mapper-ffi-windows.ps1

# Build the Android FFI libraries and stage them for Flutter.
build-pb-mapper-ffi-android:
	bash ./scripts/build/pb-mapper-ffi-android.sh

# Build the iOS FFI library and stage it for Flutter.
build-pb-mapper-ffi-ios:
	bash ./scripts/build/pb-mapper-ffi-ios.sh

# Build the Flutter Linux release app with the FFI library staged.
build-ui-linux-release-no-ffi:
	cd ui && flutter clean
	cd ui && flutter pub get
	cd ui && flutter build linux --release
	# Ensure the bundle has the FFI library even if it already existed.
	bash ./scripts/build/pb-mapper-ffi-linux.sh
build-ui-linux-release: build-pb-mapper-ffi-linux build-ui-linux-release-no-ffi

# Build the Flutter macOS release app with the FFI library staged.
build-ui-macos-release-no-ffi:
	cd ui && flutter clean
	cd ui && flutter pub get
	cd ui && flutter build macos --release
	# Ensure the .app bundle has the FFI library even if it already existed.
	bash ./scripts/build/pb-mapper-ffi-macos.sh
build-ui-macos-release: build-pb-mapper-ffi-macos build-ui-macos-release-no-ffi

# Build the Flutter Windows release app with the FFI library staged.
build-ui-windows-release-no-ffi:
	cd ui && flutter clean
	cd ui && flutter pub get
	cd ui && flutter build windows --release
	# Ensure the build output has the FFI DLL even if it already existed.
	pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build/pb-mapper-ffi-windows.ps1
build-ui-windows-release: build-pb-mapper-ffi-windows build-ui-windows-release-no-ffi

# Build the Flutter Android release APKs with the FFI library staged.
build-ui-android-release-no-ffi:
	cd ui && flutter clean
	cd ui && flutter pub get
	cd ui && flutter build apk --split-per-abi --release
build-ui-android-release: build-pb-mapper-ffi-android build-ui-android-release-no-ffi

# Build the Flutter iOS release app with the FFI library staged.
build-ui-ios-release-no-ffi:
	cd ui && flutter clean
	cd ui && flutter pub get
	cd ui && flutter build ios --release --no-codesign
build-ui-ios-release: build-pb-mapper-ffi-ios build-ui-ios-release-no-ffi

# Package Windows release via fastforge (requires Dart/Flutter and fastforge).
release-ui-windows-no-ffi:
	cd ui && flutter clean
	cd ui && dart pub global activate fastforge
	cd ui && flutter pub get
	cd ui && dart pub global run fastforge:main release --name production
release-ui-windows: build-pb-mapper-ffi-windows release-ui-windows-no-ffi

release-pb-mapper-server-docker-image: build-pb-mapper-server
	bash ./scripts/release/pb-mapper-server-docker-image.sh

release-pb-mapper-server-x86-64-musl-docker-image: build-pb-mapper-server-x86_64_musl
	bash ./scripts/release/pb-mapper-server-x86-64-linux-musl-docker-image.sh

.PHONY: \
	build-pb-mapper-server \
	build-pb-mapper-server-x86_64_musl \
	build-pb-mapper-ffi-linux \
	build-pb-mapper-ffi-macos \
	build-pb-mapper-ffi-windows \
	build-pb-mapper-ffi-android \
	build-pb-mapper-ffi-ios \
	build-ui-linux-release-no-ffi \
	build-ui-linux-release \
	build-ui-macos-release-no-ffi \
	build-ui-macos-release \
	build-ui-windows-release-no-ffi \
	build-ui-windows-release \
	build-ui-android-release-no-ffi \
	build-ui-android-release \
	build-ui-ios-release-no-ffi \
	build-ui-ios-release \
	release-ui-windows-no-ffi \
	release-ui-windows \
	release-pb-mapper-server-docker-image \
	release-pb-mapper-server-x86-64-musl-docker-image
