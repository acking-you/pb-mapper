# Map: dependencies and new capabilities (remote desktop overhaul)

Repo: `<repo>`
(branch `claude/remote-desktop-overhaul`, head `a41eaa7`). All paths below are
relative to the repo unless absolute. Line numbers refer to that head.

Scope: what the 11 "new capabilities" in the brief need. For each one: what
exists today, what is missing, which library to use (version, platforms,
licence, cost), and whether it can be installed offline. I checked the pub
cache, the cargo registry cache and the Gradle cache on this machine, and read
pub.dev for current versions on 2026-11-07.

---

## 1. Overview

- **New third-party packages are needed for three capabilities only**:
  rendering the invite QR, scanning a QR with the camera, and encrypting
  the remembered-computer list on each platform. Everything else (online
  status, host viewer info, quality overlay, quality preference, toolbar pin,
  shortcuts and modifier bar, gestures, thumbnail) is built from existing
  Rust/FFI/Dart pieces plus a few new fields and ops.
- **None of the candidate pub packages are in the local pub cache**
  (`%LOCALAPPDATA%\Pub\Cache\hosted\pub.dev`, 489 packages; there is no
  `qr`, `qr_flutter`, `mobile_scanner` or `flutter_secure_storage*`). Their
  Android Maven artifacts (CameraX, ML Kit, tink-android) are not in
  `~/.gradle/caches` either. So **offline install is impossible**: you need
  network access to pub.dev, and to Maven through the Gradle proxy set in
  `~/.gradle/gradle.properties` (`127.0.0.1:10812`).
- **A remembered remote-computer list does not exist at all today.**
  `docs/remote-desktop.md:67-68` says "The controller must retain its private
  invitation; automatic mobile invitation storage is not implemented." The
  only encrypted store is the *host-side* trust file
  `ui/native/pb_mapper_ffi/src/desktop_trust.rs` (Windows current-user DPAPI,
  `desktop-trust.dpapi`). It holds the host's own TLS identity, not
  invitations to other computers.
- Two **hard compatibility hazards** cut across several capabilities. Both
  `wire::Command` (`crates/pb-mapper-desktop/src/wire.rs:9-37`) and
  `Input`/`Key` (`crates/pb-mapper-desktop/src/lib.rs:246-287`) are closed
  serde types with `deny_unknown_fields`. If a new viewer sends an older host
  a new command, a new key or a new `Authenticate` field, the host fails to
  parse it and **ends the session** (`host.rs:180`, where `??` propagates
  "invalid desktop message"). Every new viewer-to-host message must therefore
  be gated on a capability the host advertises. `SessionStatus` (lib.rs:125)
  has no `deny_unknown_fields`, so the host→viewer direction can grow freely.

---

## 2. Components and responsibilities (current baseline)

### 2.1 Flutter dependencies (`ui/pubspec.yaml`, `ui/pubspec.lock`)

- Toolchain: Flutter 3.44.9 bundles **Dart 3.12.2**
  (`<flutter 3.44.9>\bin\cache\dart-sdk\version`).
  `pubspec.yaml:24-25` sets `sdk: ^3.9.0`, and the lock's `sdks` asks for
  `dart >=3.10.0`, `flutter >=3.38.1`.
- Direct deps and their locked versions (`pubspec.yaml:34-56`): `ffi` 2.2.0,
  `lucide_icons_flutter` 3.1.25 (exact pin), `url_launcher` 6.3.2,
  `shared_preferences` 2.5.3, `path_provider` 2.1.5, `tray_manager` 0.5.3
  (path override `packages/tray_manager`, `pubspec.yaml:62-64`),
  `window_manager` 0.5.1, `toastification` 3.2.0, `package_info_plus`
  10.2.2, `crypto` 3.0.7, `intl` 0.20.2, `cupertino_icons` 1.0.8.
- Relevant transitive deps: `image` 4.5.4 (through dev dep `icons_launcher`,
  so not usable from `lib/`), `uuid` 4.6.0, `win32` 5.x via path_provider.
  None of them provide QR, camera or secure-storage features.
- Persisted UI choices go through `AppPrefs` on `shared_preferences`
  (`ui/lib/src/shell/app_prefs.dart:10-40`, keys such as `theme_mode` and
  `last_page`). That is the natural home for toolbar-pin and
  quality-preference choices.

### 2.2 Android build (`ui/android`)

- `settings.gradle.kts:20-24`: AGP **8.9.1**, Kotlin Gradle plugin
  **2.1.0**. The wrapper is Gradle 8.12
  (`gradle/wrapper/gradle-wrapper.properties`).
- `app/build.gradle.kts`: `compileSdk`/`minSdk`/`targetSdk` come from the
  Flutter SDK defaults, which for 3.44.9 are **36 / 24 / 36**, NDK
  28.2.13676358 (`flutter_tools/gradle/.../FlutterExtension.kt:23-42`). The
  app module uses Java/Kotlin **11** (lines 18-25). jniLibs come from
  `../../native/android` (lines 27-31). Release is signed with the pinned
  key, checked by SHA-256 (lines 51-84). The release block sets no
  `minifyEnabled`, so the Flutter Gradle plugin's default R8 shrinking
  applies.
- `gradle.properties`: `android.useAndroidX=true`, `enableJetifier=true`.
  There is no `dev.steenbakker.mobile_scanner.useUnbundled`.
- `AndroidManifest.xml`:
  - Permissions (lines 3-7): `INTERNET`, `REQUEST_INSTALL_PACKAGES`,
    `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE`. There is **no `CAMERA`**.
  - `<application>` (lines 9-13) sets **no `android:allowBackup`**, so the
    default `true` applies. Android Auto Backup therefore copies app-private
    files (Rust config in `getApplicationDocumentsDirectory()`, see
    `ui/lib/main.dart:93-104`, and shared_preferences) to Google Drive.
  - Impeller is disabled (lines 19-21). This is a performance invariant from
    the brief.
  - `MainActivity` is `FlutterActivity`, not `FlutterFragmentActivity`
    (`kotlin/com/pb_mapper/ui/MainActivity.kt:7`). That is fine for
    mobile_scanner and flutter_secure_storage; only biometric prompts would
    need a FragmentActivity. It registers two channels:
    `UpdateInstaller` and `DesktopVideo` (lines 11-15).
- Gradle cache: only `androidx.datastore` (1.1.3/1.1.7/1.2.1),
  `com.google.crypto.tink:tink` 1.7.0 and `org.jetbrains.kotlinx` are
  present. There is no `androidx.camera`, `com.google.mlkit`,
  `com.google.android.gms` or `tink-android` 1.23.

### 2.3 Apple config basics

- iOS deployment target **13.0** (`ios/Runner.xcodeproj/project.pbxproj:349,475,526`).
  `ios/Runner/Info.plist` has **no `NSCameraUsageDescription`** (only
  orientation keys, lines 31-38).
- macOS deployment target **10.15** (`macos/Runner.xcodeproj/project.pbxproj:464,546,596`).
  Both entitlement files enable app-sandbox, network client/server and
  Downloads read-write. There is **no `keychain-access-groups` and no
  `com.apple.security.device.camera`**.
- macOS ships as a Developer ID–signed, notarised DMG outside the App Store
  (`docs/macos-signing.zh-CN.md`). This matters because
  `keychain-access-groups` requires an embedded provisioning profile.
- Neither `ios/` nor `macos/` has a Podfile or a SwiftPM package reference
  in the checked-in pbxproj. The plugin integration is regenerated on the
  first macOS build, so CocoaPods/SwiftPM handling of new plugins can only be
  checked on a Mac or in CI.

### 2.4 Rust manifests

- Workspace `Cargo.toml`: version 0.10.2, edition 2024, `rust-version`
  1.99.1, `[workspace.lints.clippy] unwrap_used/expect_used = "deny"`.
  Relevant pinned deps: `windows` 0.62.2 (Win32 Foundation, StationsAndDesktops,
  Threading, WindowsAndMessaging, HiDpi, Gdi), `enigo` 0.6.1, `image` 0.25.9
  (jpeg), `ring` 0.17.14, `base64` 0.23.1, `xcap` 0.9.8, `windows-capture`
  2.0.1, `quinn`/`rcgen` forks.
- `ui/native/pb_mapper_ffi/Cargo.toml`: `crate-type = ["cdylib","staticlib"]`,
  `pb-mapper` with feature `desktop`, `pb-mapper-client` with features
  `p2p, relay-quic, port-mapping`, `ring` for Ed25519. On Windows it adds
  `windows` with `Win32_Security_Cryptography`, `Win32_System_Memory` and
  `Win32_Storage_FileSystem`, which DPAPI and `MoveFileExW` need.
- `Cargo.lock` already contains `jni` 0.21.1/0.22.4 and `ndk-context` 0.1.1
  (pulled by `hickory-resolver` and `netdev`). Nothing initialises the
  Android context: grep finds no `ndk_context::initialize_android_context`
  and no `JNI_OnLoad`. The cargo registry cache also holds `keyring` 3.6.3
  and `security-framework` 2.11.1/3.7.0, but none of these are dependencies.

### 2.5 Invitation (the payload a QR must carry)

- Type: `pb_mapper::desktop::Invitation`
  (`crates/pb-mapper-desktop/src/lib.rs:86-123`), with
  `#[serde(deny_unknown_fields)]` and fields
  `{version: u32, service: String, certificate: String, token: String}`.
- `encode()` is plain `serde_json::to_string`. **There is no prefix, no
  base64 wrapper and no URI scheme**: the invite is literally
  `{"version":1,"service":"desktop:…","certificate":"<b64url DER>","token":"<43 chars>"}`.
- `decode()` (lines 108-122) rejects text over 8192 bytes, `version != 1`,
  an invalid service (`valid_service`, lines 49-56: `desktop:` prefix, 9-128
  bytes, ASCII alphanumerics plus `._:-`), `token.len() != 43` and
  `certificate.len() > 4096`.
- The certificate comes from `HostIdentity::generate()`
  (`security.rs:37-45`), which is `rcgen::generate_simple_self_signed(["desktop.pb-mapper.invalid"])`.
  That gives P-256 ECDSA, CN "rcgen self signed cert", validity 1975-4096,
  serial = SHA-256(pubkey)[..20], and SAN + SKI extensions
  (`deps/rcgen/rcgen/src/certificate.rs:90-112,335-412`). I estimate the DER
  at **about 400 bytes**, which is **about 535 base64url characters**. The
  token is 32 random bytes as base64url = 43 characters (`security.rs:14-19`).
- **Estimated invite length**: about 650 characters for a 19-character
  service name such as `desktop:my-computer`, and up to about 760 for a
  128-byte service. The relay address and credential are *deliberately
  absent* (lib.rs:86-88). A viewer always connects through its own
  configured relay and namespace (`desktop.rs:727-746`).
- The verifier pins the exact certificate bytes (`security.rs:139-158`); a
  mismatch yields "desktop identity changed".
- Contrast with the **setup string** (`ui/lib/src/common/setup_string.dart:24-37`):
  base64 of a JSON object holding
  `{version, serverAddress, keepAliveEnabled, msgHeaderKey, relayTransport, exportedAt}`.
  It **carries the relay credential**. A temporary credential has the prefix
  `pbmt1_` (`crates/pb-mapper-core/src/checksum.rs:24`). A QR scanner for
  "Add computer" must never accept or apply a setup string by accident.

### 2.6 Host trust store (the only encrypted store today)

`ui/native/pb_mapper_ffi/src/desktop_trust.rs`:

- `supported()` returns `cfg!(windows)` (lines 12-14).
- `Saved {schema: 1, relay, namespace, settings: Settings, identity: Vec<u8>}`
  with `deny_unknown_fields` (lines 34-42). `Settings` carries `service,
  display_id, allow_control, relay_only, privileged, auto_start, background`
  (lines 21-33).
- `load`/`save`/`revoke` work on `<config_dir>/desktop-trust.dpapi`, capped
  at 64 KiB (`LIMIT`, line 11). `save` writes a random-named temp file with
  `create_new`, `sync_all`, then
  `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)` (lines 141-171, 221-239).
- `protect()` calls DPAPI `CryptProtectData`/`CryptUnprotectData` with
  entropy `b"pb-mapper/desktop-trust/v1"` and `CRYPTPROTECT_UI_FORBIDDEN`
  (lines 179-214). On non-Windows it returns the error "remembered desktop
  hosting currently requires Windows DPAPI" (lines 215-220).
- The background service keeps its own machine-DPAPI file
  `background-host.dpapi` (`crates/pb-mapper-desktop/src/helper/background.rs:10`).
- This store is **host-side only**: there is no viewer-side equivalent.

### 2.7 Relay listing APIs (for "online status")

- Wire: `PbConnStatusReq::Keys`. The relay answers with the service names
  **in the caller credential's namespace only**
  (`crates/pb-mapper-server/src/runtime.rs:530-538`, which filters by
  `split_scoped_service_key(key).0 == namespace`).
- SDK: `Client::list_keys()` returns "Service keys visible to this
  credential's namespace" (`crates/pb-mapper-client/src/sdk/client.rs:510-518`).
  `Client::service_status(key)` returns the connections the relay holds for
  one key (lines 530-547).
- FFI:
  - `fetch_real_status` (`ui/native/pb_mapper_ffi/src/state.rs:59-106`)
    calls `client.list_keys()` with a 5 s timeout
    (`FORCE_REFRESH_TIMEOUT`, line 49).
  - `refresh_server_status` (`state/status.rs:332-405`) packs the result
    into `ServerStatusDetail.registered_services`
    (`state.rs:415-435`, camelCase `registeredServices`). It is exported as
    `pb_mapper_get_server_status_detail_json` and
    `pb_mapper_force_refresh_server_status_json` (`server.rs:122-164`).
  - `pb_mapper_get_service_conns_json(service_key)` (`server.rs:95-120`,
    implemented in `status.rs:296-322`) is the per-key variant.
- Admin-only: the `services` op of `pb_mapper_relay_call_json` (`relay.rs:262-270`)
  lists every namespace, as `{keyId, namespace, name, transport, encrypted, connections}`.
- Dart: `AppStore` polls the relay every **12 s** (`relayInterval`,
  `ui/lib/src/state/app_store.dart:46,189`) through `checkRelay()`
  (lines 225-280).
  - It exposes `relayServices` (lines 122-135). With the admin key that list
    covers every namespace, from `_everyService`; otherwise it covers the
    own namespace from `registeredServices`.
  - `ownNamespace` (lines 115-116) is the temporary credential's key id, or 0
    for the admin key.
  - `remoteServices` (lines 139-148) currently includes `desktop:*`
    services, so they show up as ordinary services in Tunnels and the galaxy.
- `RelayStore` (`ui/lib/src/state/relay_store.dart:111-121`) polls only
  while the Relay page watches it, so it is not suitable for a list that is
  always visible.

### 2.8 Desktop FFI control surface (what new ops would extend)

- Export `pb_mapper_desktop_call_json(handle, request)`
  (`ui/native/pb_mapper_ffi/src/desktop.rs:510-533`). Requests are capped at
  16384 bytes, and `Request` is `#[serde(tag="op", rename_all="snake_case",
  deny_unknown_fields)]` (lines 193-255).
- The ops are `capabilities, status, trust_status, trust_revoke,
  host_restore, host_enable{service,display_id},
  helper_action{action}, background_action{action},
  host_start{service,display_id,allow_control,relay_only?,privileged?,remember?,auto_start?,background?},
  host_stop{on_exit?}, host_permission{allow_control},
  connect{invitation,relay_only?,native_video?,video?},
  disconnect, control{enabled}, release, keyframe,
  presented{generation,frame_id}, input{event}`.
- The reply envelope is `{"success", "message", "data"}`. `status` returns
  `{"host": SessionStatus|null, "viewer": SessionStatus|null, "background": Snapshot|null}`
  (lines 614-626).
- Dart wrapper: `DesktopApiClient`/`DesktopApi`
  (`ui/lib/src/ffi/desktop_api.dart:240-492`).
  `PbMapperService.desktopCall` routes `presented, keyframe, input, control,
  release` to the frame isolate when one is running; it sends everything
  else to the JSON worker (`ui/lib/src/ffi/pb_mapper_service.dart:288-301`).
  The fake for tests is `ui/test/fake_desktop_api.dart` (`FakeDesktopApi`).

### 2.9 Status vocabulary (`SessionStatus`, lib.rs:125-165)

- Fields: `state, message, control_allowed, controlling, generation,
  frame_id, display{id,name,x,y,width,height}, path,
  direct_stage?, direct_failure?, frames, bytes, desktop,
  video{codec,width,height,keyframe,pts_us,duplicate}, capture_fps,
  receive_fps, dropped_frames, bitrate, cursor?{x,y,visible}, frame_ack_ms,
  roundtrip_probe?`.
- Dart `DesktopStatus.fromJson` (`desktop_api.dart:86-119`) reads all of
  these except `roundtrip_probe`.
- The **viewer** sets `path` once at connect: "direct" if
  `connection.diagnostics().direct.active_direct > 0`, else "relay"
  (`viewer.rs:170-188`, written per frame at line 236). It never updates
  after a later upgrade or downgrade.
- The **host** never sets `path`. Host `state` moves between `waiting`,
  `connecting`, `streaming`, `capture_unavailable` and `stopped`
  (`worker.rs:124,280,484,492,516,548,573`, `host.rs:204,331`).
  `Host::status()` is at `host.rs:308-315`.
- The host admits at most one viewer: `Semaphore::new(1)` at `host.rs:140`,
  and `try_acquire_owned` at line 165 fails with "a viewer is already
  connected". The session start point is `host.rs:172`.
- `VideoPreferences {max_width,max_height,max_fps,bitrate}`
  (lib.rs:178-208) defaults to 1920×1080, 90 fps, 12 Mbit/s. `validate()`
  bounds are width 2-3840, height 2-2160, **fps 30-120**, **bitrate 1-40 Mbit/s**.
  The value only travels inside `Command::Authenticate.video` and **cannot
  change mid-session**.

### 2.10 Native video and picture widgets (for the thumbnail)

- `DesktopVideoTexture` uses channel `pb_mapper/desktop_video` with methods
  `create, submit, reset, stats, dispose` and event `event`
  (`ui/lib/src/ffi/desktop_video.dart:37-117`). It is supported **only on
  Android and Windows** (line 42).
  - Android: `DesktopVideo.kt` decodes with MediaCodec into a
    `SurfaceTextureEntry` (lines 28-48, 115, 381-418).
  - Windows: `windows/runner/desktop_video.cpp` uses the
    `CLSID_CMSH264DecoderMFT` decoder into a `GpuSurfaceTexture` of type
    `kFlutterDesktopGpuSurfaceTypeDxgiSharedHandle` (lines 91-119, 256-293,
    537-569).
- Viewer picture (`ui/lib/src/pages/desktop_viewer.dart:462-478`):
  `Texture(textureId)` for H.264, otherwise `RawImage(image: ui.Image)` for
  JPEG (iOS, macOS, Linux). Both sit inside `RepaintBoundary(child: widget.child)`
  in `desktop_gesture_surface.dart:382`.
- Teardown: `DesktopStore._disconnect` (`desktop_store.dart:964-987`) calls
  `_clearVideo()` first (line 976), which disposes the texture (lines 688-726).

### 2.11 Input key vocabulary

- Rust `Key` (lib.rs:262-287) covers Shift, Control, Alt, Meta, Enter,
  Escape, Tab, Backspace, Delete, arrows, Home, End, PageUp, PageDown, and
  A/C/V/X/Z. It maps to enigo in `windows.rs:538-563`; letters map to
  `enigo::Key::Unicode`.
- Dart `_keyNames` (`desktop_viewer.dart:541-568`) has the same set.
  Unmapped printable characters go out as `{'kind':'text'}` when no
  Ctrl/Alt/Meta modifier is held (lines 244-251).
- enigo 0.6.1 (registry source `keycodes.rs`) has `F1`..`F12`+, `Insert`,
  `CapsLock`, `PrintScr`, `Snapshot`, `Pause`, `ScrollLock`, `Numlock`,
  `LWin` and `Meta`, so there is no library gap.

---

## 3. Data and control flow (today)

1. **Invite creation**:
   1. The host UI calls `host_enable` (Windows), handled by `enable_host`
      (`desktop.rs:1064-1204`). The background service then owns the
      identity and returns `invitation` from `background::Snapshot.invitation`.
   2. Alternatively the UI calls `host_start`, handled by `start_host`
      (lines 820-1058), which returns `host.invitation().encode()`
      (line 1054).
   3. Dart stores the string in `DesktopStore.invitation`
      (`desktop_store.dart:63`, set at 210, 220, 241, 287, 329, 754, 806).
   4. `DesktopPage` copies it to the clipboard
      (`desktop_page.dart:96-104`). That is the only way to share it today.
2. **Invite use**:
   1. A `TextField` with key `desktop-invitation` (`desktop_page.dart:161-171`)
      feeds `DesktopStore.connect(invite, relayOnly)` (`desktop_store.dart:874-891`).
   2. `_target = (invite, relayOnly)` holds the invite **in memory only**
      while reconnecting (line 881). It is cleared on `_disconnect`
      (line 965) or on `INVALID_ARGUMENT` (lines 932-938).
   3. `DesktopApi.connect` (`desktop_api.dart:428-447`) sends `{op:"connect", invitation, relay_only,
      native_video: Platform.isWindows||Platform.isAndroid, video:{max_width:1920,max_height:1080,max_fps:<refresh 30..90 on Android else 90>,bitrate:12000000}}`.
   4. The FFI `Request::Connect` (`desktop.rs:718-770`) runs
      `Invitation::decode`, which maps to `INVALID_ARGUMENT` on failure, then
      `Client::from_credential(config.server_address, process credential, ...)`
      and `Viewer::connect_with_presentation_feedback` or
      `connect_with_video_preferences`.
   5. `Viewer::connect_inner` (`viewer.rs:128-354`) re-validates the invite,
      pins TLS, calls `client.connect_tcp(invitation.service, options)` and
      sends `Command::Authenticate{token, codecs, video, presentation_feedback}`.
3. **Relay service list**:
   1. `AppStore.checkRelay`, every 12 s or on config/server change events,
      calls `api.getServerStatusDetail()` or `forceRefreshServerStatus()`.
   2. The FFI `refresh_server_status` runs `client.list_keys()` alongside
      `remote_id()`.
   3. The result is `relay.registeredServices`; with the admin key the
      store also calls `adminServices()` (`app_store.dart:265-273`).
   4. Pages read `relayServices` and `remoteServices`.
4. **Host and viewer status**:
   1. `DesktopStore.poll` runs every 250 ms (`desktop_store.dart:149,316-365`)
      and calls `api.status()`.
   2. The FFI `Request::Status` returns `host.status()` through `try_lock`,
      falling back to the background snapshot, plus `viewer.status()`.
   3. Frames reach Dart through the frame isolate
      (`pb_mapper_desktop_wait_frame`), then `_consumeFrames` and
      `_acceptFrame` (lines 471-560, 660-686).
5. **Host trust persistence** (Windows only): every `trust::load/save/revoke`
   runs in `tokio::task::spawn_blocking` under the host lifecycle lock
   (`DesktopState.host`, `desktop.rs:59`).

Threading: each `pb_mapper_desktop_call_json` blocks a Dart worker isolate on
`handle.runtime.block_on` (`desktop.rs:529`), and file and DPAPI I/O goes
through `spawn_blocking`. A new viewer-side store done in Rust should follow
the same pattern.

---

## 4. Contracts and formats that the new capabilities touch

| Contract | Location | Format / constraints |
| --- | --- | --- |
| Invitation text | lib.rs:91-122 | Raw JSON `{"version":1,"service":"desktop:<1-120 chars [A-Za-z0-9._:-]>","certificate":"<b64url DER ≤4096>","token":"<43 b64url>"}`; ≤8192 bytes; no prefix |
| Setup string (must not be confused with an invite) | setup_string.dart:26-37 | base64(JSON `{version, serverAddress, keepAliveEnabled, msgHeaderKey, relayTransport, exportedAt}`) |
| Desktop FFI request | desktop.rs:193-255 | `{"op": snake_case, ...}`, `deny_unknown_fields`, ≤16384 bytes |
| Desktop status | desktop.rs:614-626 | `{"host", "viewer", "background"}`; `background` = helper `Snapshot{configured, enabled, host, invitation, settings{service,display_id,allow_control,allow_direct,auto_start}, server, namespace, message}` (background.rs:42-53) plus FFI-added `supported`, `service_disabled` |
| SessionStatus | lib.rs:125-165 | snake_case JSON; **open for new optional fields** (no `deny_unknown_fields`); use `#[serde(default, skip_serializing_if = "Option::is_none")]` like `direct_stage` |
| Wire Command (viewer→host) | wire.rs:9-37 | **closed** (`deny_unknown_fields`, closed tag set): new ops/fields break old hosts |
| Input / Key | lib.rs:246-287 | **closed** enums; a new `Key` variant breaks old hosts' parse |
| Helper IPC Command | background.rs:55-74 | **closed**; Snapshot/Response open (`Response` has `deny_unknown_fields`, background.rs:75-80) |
| Relay key list | runtime.rs:530-538 | names only, own namespace only |
| ServerStatusDetail | state.rs:415-435 | camelCase `{serverAvailable, registeredServices[], serverMap, activeConnections, idleConnections, error?, latencyMs?, quicPort?, quicKnown}` |
| Relay op `services` (admin) | relay.rs:262-270, 520-529 | `[{keyId, namespace, name, transport, encrypted, connections}]` |
| VideoPreferences | lib.rs:178-208 | `{max_width, max_height, max_fps (30-120), bitrate (1e6-4e7)}`, `deny_unknown_fields` |
| Video texture channel | desktop_video.dart:39, DesktopVideo.kt:28-48, desktop_video.cpp:537-569 | `pb_mapper/desktop_video`: `create→id`, `submit{textureId,bytes,generation,frameId,ptsUs,width,height,keyframe}`, `reset`, `stats{textureId,timings?}`, `dispose`; events `{textureId,generation,frameId,width,height,presented,needKeyframe,message}` |
| Host trust file | desktop_trust.rs:111-113 | `<config_dir>/desktop-trust.dpapi`, DPAPI(JSON `Saved`), ≤64 KiB |

---

## 5. Invariants and security boundaries relevant here

1. **The invitation is a bearer capability** (lib.rs:86-88; docs/remote-desktop.md:70).
   Never log it, publish it or put it in a service name. `Debug` prints only
   the service (lib.rs:97-103). Any new store, QR or scan path must keep it
   out of logs, analytics, crash reports and the clipboard history beyond
   the existing Copy action.
2. **Invites carry no relay credential.** A remembered computer is only
   meaningful together with the relay address and namespace that were in
   effect when it was added. Host trust uses the same rule (`Saved::matches`,
   desktop_trust.rs:98-100, and `matches_relay`, desktop.rs:595-608).
3. **The certificate pin never changes silently.**
   `_scheduleReconnect` stops retries on "desktop identity changed"
   (`desktop_store.dart:428-443`). A remembered entry must not "heal" a pin
   by replacing the certificate without an explicit new invite.
4. **Reconnect never replays input, and control needs a fresh presented
   frame** (brief regression guards; `DesktopStore.send`/`_interruptInput`).
   Lockable modifiers from the new modifier bar must be released on every
   barrier (`release`, end control, disconnect, background).
5. **Host-side remembering is Windows-only and fail-closed.** On any other
   platform `trust::supported()` is false and `start_host` rejects
   `remember` (desktop.rs:993-995).
6. **Ctrl+Alt+Del stays unsupported** (docs/remote-desktop.md:49). No
   Secure Attention Sequence injection is to be added.
7. **Compatibility**: viewer→host messages are closed types (section 4).
   Each new viewer→host message needs a host-advertised feature first.
8. **Privacy promises in the brief**:
   - Camera frames are never stored or uploaded.
   - The thumbnail is local only and is deleted with the computer.
   - The QR is visible only on the host card, after an explicit tap.
   - Android Auto Backup (on by default, see 2.2) currently contradicts the
     "local only" intent for anything kept in app-private storage.

---

## 6. Tests covering this scope today

- Rust, `ui/native/pb_mapper_ffi/src/desktop_trust.rs`:
  - `dpapi_round_trip_tampering_and_revocation` (Windows; lines 249-296)
    covers round trip, scope mismatch, no plaintext service in the
    ciphertext, tamper detection and revocation.
  - `background_handoff_keeps_the_live_link_through_restart` (lines 298-338).
- Rust, `ui/native/pb_mapper_ffi/src/desktop.rs`:
  - `status_and_video_with_the_same_id_both_cross_the_binary_boundary`
    (1318-1345) covers the binary frame layout.
  - `background_consent_and_ui_ownership_boundaries` (1347-1403).
  - `late_background_discovery_cannot_hide_a_new_grant` (1405-1425).
  - `revocation_waits_for_handoff_and_uses_the_actual_owner` (1427-1481).
  - `disabled_service_allows_local_host_but_unknown_background_stays_blocked` (1483-1573).
  - `disconnect_cancels_configuration_wait_without_cancelling_reconnect`
    (1575-1652) covers invalid invite → `INVALID_ARGUMENT`.
  - Two cancel tests (1654-1704).
  - `binary_frame_is_owned_until_explicit_release_after_disconnect` (1725-1812).
- Rust, `crates/pb-mapper-desktop/src/host.rs`:
  `remembered_invitation_survives_restart_but_not_revocation` (374-446).
- Rust relay ops, `ui/native/pb_mapper_ffi/src/relay.rs:594-742`: admin
  lifecycle; `services`/`connections`/`sessions` return arrays; an
  unreachable relay; QUIC link.
- Dart desktop tests (`ui/test/`):
  - `desktop_test.dart`: background sharing, viewer input mapping,
    input ordering, compact host flow.
  - `desktop_lifecycle_test.dart`: final-frame ack, worker cancellation,
    texture disposal, freshness.
  - `desktop_host_readiness_test.dart`: one-action upgrade, IPC failure
    resume, restart handoff.
  - `desktop_recovery_test.dart`: input failure, invalid invite, retries,
    identity pin, IDR before reconnect, cancelled completion, move
    coalescing.
  - `desktop_viewer_navigation_test.dart`.
  - `desktop_gesture_test.dart`: tap and two-finger right click, pinch,
    trackpad.
  - Fake: `fake_desktop_api.dart` (`FakeDesktopApi`, 209 lines).
- **No tests exist** for QR, scanning, a viewer-side store, online status of
  desktop services, thumbnails, quality preference, or extended keys. None
  of these features exist yet.

---

## 7. Impact on the planned overhaul (per capability)

### 7.0 Dependency decisions (summary table)

| Need | Recommended | Version (exact) | Platforms | Licence | In local cache? | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| QR encode | `qr` (pure Dart) + own `CustomPainter` | **3.0.3** (published 2026-10-29, SDK `^3.4.0`, dep `meta ^1.7.0`) | all 5 + web | BSD-3-Clause | no | **`qr` 4.0.x needs Dart `^3.13.0`; Flutter 3.44.9 ships Dart 3.12.2, so it is incompatible.** Pin `qr: 3.0.3`. |
| QR encode, alternative | `qr_flutter` | 4.1.0 (2023-05-14; deps `qr ^3.0.1`) | all | BSD-3-Clause | no | Unmaintained since 2023, and it keeps you on qr 3.x. Only worth taking for its ready widget. |
| Camera scan | `mobile_scanner` | **7.5.0** (2026-11-01; Dart `^3.7.0`, Flutter `>=3.29.0`). 7.4.2 (2026-09-10) is the most recent build older than two weeks if a soak period is wanted | Android, iOS, macOS, web. **Not Windows or Linux** | BSD-3-Clause | no | Android: CameraX `camera-lifecycle`/`camera-camera2` 1.6.1, ML Kit `com.google.mlkit:barcode-scanning:17.3.0` **bundled by default** (unbundled = Play-services 18.3.1, chosen with `dev.steenbakker.mobile_scanner.useUnbundled=true`), kotlinx-coroutines-android 1.11.0, minSdk 23, Java 17. iOS ≥13, macOS ≥10.15 (AVFoundation + Vision). Plugin manifest adds `CAMERA` and `uses-feature camera required=false`. Bundled model adds about 3-10 MB. |
| Encrypted remembered list | `flutter_secure_storage` | **11.2.0** (2026-09-16; Dart `>=3.8.0`, Flutter `>=3.19.0`) → darwin `^0.4.0` (0.4.3), linux `>=3.0.1 <5` (3.0.3), windows `^4.2.2` (4.2.2), web `^2.1.1`, platform_interface `^2.1.0` | all 5 | BSD-3-Clause | no | Android: Keystore-backed (RSA-OAEP key-wrapping key in AndroidKeyStore + AES-GCM data; `tink-android:1.23.0`; minSdk 24 per its gradle). iOS/macOS: Keychain. Windows: DPAPI-encrypted `flutter_secure_storage.dat` under `getApplicationSupportDirectory()` (written by Dart FFI; the legacy C++ plugin is still compiled). Linux: libsecret plus a running keyring daemon. |
| (rejected) Rust keyring | `keyring` 3.6.3 (cached) / `keyring-core` + `android-native-keyring-store` 1.0.0 | — | keyring 3.x has no Android; the Android store needs `ndk-context` initialised with JavaVM + Context (nothing does that today) | MIT/Apache | partly | More glue (JNI_OnLoad, Kotlin context hand-off) and less proven; it would also duplicate DPAPI code we already have. |

**Offline install is impossible.** None of the above is in the pub cache. You
need network access to pub.dev and to Maven (google/mavenCentral through the
Gradle proxy) for CameraX 1.6.1, ML Kit 17.3.0, kotlinx-coroutines 1.11.0 and
tink-android 1.23.0.

Cross-cutting build risks:

1. **Windows ATL.** `flutter_secure_storage_windows` 4.2.2 still compiles
   `flutter_secure_storage_windows_plugin.cpp`, which includes **`<atlstr.h>`**.
   This machine has only "VS 2022 BuildTools" **without** the
   `Microsoft.VisualStudio.Component.VC.ATL` component: `vswhere -requires`
   returns nothing. Install "C++ ATL for latest v143 build tools" before the
   first Windows build. GitHub `windows-latest` (VS 2022 Enterprise) is
   expected to have it, but check `release-ui.yml:77` once.
   - Alternative if ATL is unacceptable: on Windows keep the store in Rust
     with the existing `trust::protect` (DPAPI) behind a Dart `SecretStore`
     interface. That is more code and two code paths, so it is not
     recommended.
2. **Linux libsecret.** Add `libsecret-1-dev` to the apt line at
   `.github/workflows/release-ui.yml:162`. At runtime, if no Secret Service
   is running (headless or minimal WMs), writes throw `PlatformException`.
   The UI must then turn "Remember this computer" off and say why.
3. **macOS keychain.** Use `MacOsOptions(usesDataProtectionKeychain: false)`
   (the legacy keychain). The data-protection keychain needs
   `keychain-access-groups`, which needs a provisioning profile. That does
   not fit Developer ID DMG distribution (docs/macos-signing.zh-CN.md). On
   iOS, add an empty `keychain-access-groups` entitlement as the package
   requires, and use
   `IOSOptions(accessibility: KeychainAccessibility.first_unlock_this_device)`
   so items do not migrate to other devices.
4. **Kotlin.** mobile_scanner 7.5.0 pulls kotlinx-coroutines 1.11.0 (built
   with Kotlin 2.2.20) and CameraX 1.6.1. KGP 2.1.0 (`settings.gradle.kts:23`)
   can read metadata one minor version ahead, so 2.2 should work. If the
   build fails with "incompatible version of Kotlin … metadata", bump to
   ≥ 2.2.x. JDK 17 is already on PATH, as both plugins require.
5. **Android Auto Backup.** `allowBackup` defaults to true
   (AndroidManifest.xml:9-13). flutter_secure_storage documents
   "Failed to unwrap key" after a restore, because Keystore keys are not
   backed up. It also means thumbnails and the Rust config (relay
   credential) leave the device. Recommended: set
   `android:allowBackup="false"`, or add `android:dataExtractionRules`
   (API 31+) plus `android:fullBackupContent` that exclude the
   FlutterSecureStorage prefs, the thumbnail directory and Rust's config
   dir. This is a behaviour change for existing users' cloud restore and
   needs a release note.
6. **iOS/macOS plugin integration.** Both new plugins have Darwin code.
   Neither project checks in a Podfile or SwiftPM refs, so verify on a Mac
   or in CI that CocoaPods or SwiftPM resolution works (mobile_scanner
   supports SwiftPM for iOS since 7.0.0-beta.2 and for macOS since 5.2.2;
   SwiftPM support in flutter_secure_storage_darwin 0.4.3 is not
   documented).
7. **China and no Google services.** Keep the **bundled** ML Kit model,
   i.e. do not set `useUnbundled`. The unbundled model downloads through
   Google Play services, which many phones in China lack. ML Kit may send
   usage metrics, but never images. If a strict no-Google build is ever
   required, `flutter_zxing` (zxing-cpp) is the fallback.

### 7.1 Invite QR code (host card)

- **Exists**: the invite string is in `DesktopStore.invitation`
  (`desktop_store.dart:63`). Copy is at `desktop_page.dart:96-104`. There is
  no rendering.
- **Missing**: a QR widget in the new host card, shown only after tapping
  "QR code" and hidden when leaving the page.
- **Do**: add `qr: 3.0.3` and a `QrInvite` painter in `ui/lib/src/widgets/`.
  - Use `QrCode.fromData(data: invitation, errorCorrectLevel: QrErrorCorrectLevel.M)`
    and `QrImage(qr).isDark(r, c)`. It picks the minimal version
    automatically.
  - Draw dark modules in a POCOX token colour on a light square, with a
    4-module quiet zone, even in dark mode: scanners need dark-on-light
    contrast.
  - Size: an invite of about 650 bytes in byte mode needs **version 20 at
    ECC M (97×97 modules)**, or version 18 at ECC L (89×89). A 128-byte
    service pushes that to about v22-M (105×105). At ≥4 px per module plus
    the quiet zone, render at **≥ about 420 logical px**. That is fine
    on a 1440×900 card; scale down only with care.
  - Encode the **exact invitation string** (raw JSON). Adding a prefix or
    base64 grows the payload and would need decoding changes; raw JSON is
    already self-validating through `Invitation::decode`.
  - Make QR visibility local widget state (`bool _showQr`). Reset it in
    `deactivate`/`dispose` and on `AppLifecycleState.inactive/paused`, so
    the code does not remain in OS task-switcher snapshots. Optional on
    Android: `FLAG_SECURE` while shown, via a two-method channel in
    `MainActivity.kt`; no plugin is needed.
- **Optional shrink** (not recommended now): a v2 invite that carries
  SHA-256(cert) instead of the DER (~140 characters, about v7-M) means
  changing `security::client`/`Pinned` (security.rs:109-158) to hash-pin,
  and `Invitation::decode` to accept version 2. Older apps reject version 2
  invites, and this couples with the security review.
- **Risks**: the QR shows on the *host's* own screen, which the host
  captures and which a connected viewer can see. That viewer already holds
  the same invite, so this is not an escalation, but other screen-sharing
  apps can see it. Keep it opt-in and transient.

### 7.2 Camera QR scan (mobile "Add computer")

- **Exists**: only paste (`desktop_page.dart:161-191`). There is no camera,
  no `CAMERA` permission and no `NSCameraUsageDescription`.
- **Do**:
  1. Add `mobile_scanner: 7.5.0` (or 7.4.2).
  2. Show the scanner only when
     `Platform.isAndroid || Platform.isIOS`, matching MobileAdd. On
     Windows/Linux the plugin has no implementation and would throw
     `MissingPluginException`.
  3. Create the controller with `formats: [BarcodeFormat.qrCode]`,
     `detectionSpeed: DetectionSpeed.noDuplicates` and
     `returnImage: false` (the default), so no frame bytes reach Dart. Never
     persist anything. Stop and dispose the controller when the sheet
     closes or the app is paused.
  4. On detection, take `barcodes.first.rawValue` and pass it through the
     same validation as paste: the new `inspect_invitation` op (see 7.3),
     then show the recognised host and confirm.
  5. Reject setup strings and anything else with a specific message. Never
     auto-apply a scanned relay credential, and never auto-connect without
     the recognised-host step.
  6. iOS: add `NSCameraUsageDescription` to `ios/Runner/Info.plist`, in
     English with a zh translation via InfoPlist.strings.
  7. Android: the plugin merges `CAMERA` and the camera feature with
     `required=false`. Runtime permission is requested when the controller
     starts. Handle `MobileScannerException` for permission denied by
     falling back to paste.
  8. macOS: no entitlement is needed unless scanning is enabled there; the
     design is mobile-only.
- **Cost**: medium. One plugin, one page section, permission and error
  states, l10n strings.
- **Coupling**: the IA/mobile-pages subsystem (MobileAdd layout), Icons
  (`scan-qr-code`), and l10n.

### 7.3 Encrypted invite storage on mobile, i.e. the remembered computer list (all platforms)

- **Exists**: nothing on the viewer side. The invite lives only in
  `_target` memory (`desktop_store.dart:78,881`).
  - The host side has a Windows DPAPI store (`desktop_trust.rs`) with good
    patterns worth copying: schema version, size limits, atomic replace,
    scope match.
  - There is no op to decode or inspect an invite without connecting;
    decode happens inside `Request::Connect` (`desktop.rs:724-725`).
- **Recommended design**:
  - **Secret plus metadata in `flutter_secure_storage`** under a single key,
    e.g. `pb_mapper.desktop.computers.v1`, holding a JSON array. That keeps
    updates atomic and avoids divergence between an index and its secrets.
  - Record:
    `{id (random 128-bit hex), name, service, relay, namespace, fingerprint (SHA-256 hex of cert DER, short form for UI), invitation, relayOnly, lastPath ('direct'|'relay'|null), lastConnectedAt (ms), thumbnail (bool)}`.
  - Bound it at about 50 entries and about 64 KiB, mirroring
    `desktop_trust::LIMIT`.
  - Options:
    - `AndroidOptions()`: the v10+ default ciphers, keeping
      `migrateOnAlgorithmChange`.
    - `IOSOptions(accessibility: first_unlock_this_device)` plus the
      entitlement.
    - `MacOsOptions(usesDataProtectionKeychain: false)`.
    - Windows and Linux: defaults.
  - Put it in a new `RemoteComputerStore` under `ui/lib/src/state/`, owned by
    `AppStore` next to `desktop`. Inject a `SecretStore` interface so tests
    use an in-memory fake, the same pattern as `PbMapperApiClient`.
  - Dedupe on `(relay, namespace, service, fingerprint)`. The same
    `(relay, namespace, service)` with a different fingerprint is a *new
    identity*: ask the user, never overwrite silently (invariant 3).
  - "Remember this computer" (on by default) writes only after a successful
    first connect, i.e. after the TLS pin succeeded. Turning it off deletes
    the entry and its thumbnail.
  - On `PlatformException` from the store (Linux without a keyring, a
    damaged keystore after restore), turn "Remember" off with a reason.
    Never fall back to plaintext.
- **New FFI op** (Rust, `desktop.rs` `Request`):
  `InspectInvitation { invitation: String }` returns
  `{service, fingerprint, version}`, using `Invitation::decode` and a SHA-256
  via `ring::digest`, which is already a dependency.
  - It mirrors the existing `INVALID_ARGUMENT` mapping, and is used for
    paste ("recognised host" in AddComputer), for scan, and for dedupe.
  - Wire it through all four layers as CLAUDE.md requires:
    `DesktopApiClient.inspectInvitation`, `DesktopApi`, `FakeDesktopApi`.
    No new export symbol is needed.
  - Avoid re-implementing the validation in Dart, because the two would
    drift.
- **Connect with a remembered entry**: compare `entry.relay`/`entry.namespace`
  with the current `AppStore.config.serverAddress` and `ownNamespace`. If
  they differ, show "belongs to another relay or namespace" and do not
  connect. That mirrors host `matches_relay`, because the invite carries no
  relay. After a successful session, update `lastPath`, `lastConnectedAt`
  and the thumbnail.
- **Tests (minimal, boundary-focused)**:
  1. A store round trip through the fake `SecretStore`, covering dedupe and
     a fingerprint change that asks instead of overwriting.
  2. A store failure that disables Remember with no plaintext write.
  3. A Rust unit test for `InspectInvitation`: valid → service +
     fingerprint, a setup string → `INVALID_ARGUMENT`.

  Do not test platform crypto: it belongs to the library.
- **Risks / coupling**:
  - ATL, libsecret, macOS keychain mode and Android backup (7.0 items 1, 2,
    3, 5).
  - Desktop pages and IA (Main, MobileHome lists).
  - Docs: `docs/remote-desktop.md:67-68` must be rewritten ("automatic
    mobile invitation storage is not implemented").
  - `docs/ui-design.md` storage notes.

### 7.4 Remote computer online status

- **Exists**: the relay already answers "is `desktop:x` registered in my
  namespace?" through `list_keys`. `AppStore.relayServices` holds the result
  every 12 s (`app_store.dart:122-135`, `189`, `225-280`). An admin
  credential additionally sees every namespace via `_everyService`.
- **Do**: no new FFI or wire work. Derive the status in Dart, per remembered
  entry:
  - `unknown`: `entry.relay != config.serverAddress`, the relay is
    offline or checking (`relayState`), or a temporary credential's
    `entry.namespace != ownNamespace` (it cannot see other namespaces).
  - `online`: `relayServices.any((s) => s.name == entry.service && s.namespace == entry.namespace)`.
  - `offline`: otherwise.

  Trigger `checkRelay()` (not forced) when the remote list becomes visible,
  for freshness.
- **Optional precision**: `pb_mapper_get_service_conns_json(service)`
  (server.rs:95-120) gives the per-key connection count in the caller's own
  namespace, but it is not needed for online/offline.
- **Semantics to document**: "online" means a publisher holds that name in
  that namespace. It does not prove the invite is still valid (revoked or
  rotated tokens, a different publisher squatting the name). The TLS pin
  and token check at connect stay authoritative.
- **Coupling**:
  - `remoteServices` and the Tunnels page's "other services" count, plus
    the galaxy planets, currently show `desktop:*` services as ordinary
    tunnels (`app_store.dart:139-148`).
  - The IA subsystem should decide whether `desktop:` names are filtered
    out of Tunnels and the galaxy now that they have their own page.
  - The Rust `RelayStore` admin listing is unaffected.

### 7.5 Last-picture thumbnail (mobile remote list)

- **Exists**: the picture is a `Texture` (H.264 on Android/Windows) or a
  `RawImage` (JPEG elsewhere), inside a `RepaintBoundary`
  (`desktop_gesture_surface.dart:382`; `desktop_viewer.dart:462-478`).
  `_disconnect` disposes the texture immediately (`desktop_store.dart:976`).
- **Do** (no new dependency; `dart:ui` encodes PNG):
  1. Give the `RepaintBoundary` a key, or expose `snapshot()` from
     `DesktopGestureSurfaceState`. `widget.child` is drawn in its own
     coordinate space, so zoom and pan do not affect the capture.
  2. Before calling `store.disconnect()`, and only when the session is
     `fresh` and the computer is remembered:
     `boundary.toImage(pixelRatio: 320 / width)`, then
     `toByteData(format: ui.ImageByteFormat.png)`.
  3. Write the PNG to `getApplicationCacheDirectory()/desktop-thumbnails/<id>.png`.
     The cache directory is excluded from iOS backup and Android Auto Backup
     by default, and the OS may purge it, in which case show a
     placeholder.
  4. Delete the file when the entry is removed or "Remember" is turned off.
  5. For the JPEG path, use the `ui.Image` already held by the viewer and
     downscale it with a `PictureRecorder`; no readback is needed.
  6. Add a store hook (`beforeDisconnect`), or let the viewer page
     orchestrate. `DesktopStore.disconnect` must not await I/O on a slow
     path; capture must be best-effort and time-boxed (≤ about 300 ms).
- **Risk (verify on device)**: whether `RenderRepaintBoundary.toImage`
  includes **external textures**, i.e. the Android SurfaceTexture with
  Skia/GL (Impeller is off) and the Windows DXGI GPU surface. I could not
  confirm this from public sources.
  - Fallback per platform: a `snapshot` method on the `pb_mapper/desktop_video`
    channel.
    - Android: `PixelCopy.request(Surface, Bitmap, …)` (API 24+, and
      minSdk is 24), or decode the latest IDR into an `ImageReader`.
    - Windows: `CopyResource` into a D3D11 staging texture and `Map`,
      giving BGRA.
  - The perf subsystem may move frames off Dart (P1-1, direct Rust→Kotlin),
    which would make the native snapshot the only option on Android.
    **Coordinate with the phone-pipeline owner.**
- **Privacy**: never capture for unremembered computers, after an identity
  mismatch, or when the picture is stale. Never upload.

### 7.6 Host viewer info (host card)

- **Exists**:
  - `Host::status()` (host.rs:308-315) gives `state` (`waiting` = no viewer,
    `connecting`/`streaming`/`capture_unavailable` = a viewer session),
    `controlling` and `control_allowed`.
  - The background-service host status arrives via `Snapshot.host`
    (background.rs:46) and the FFI `background_host` (desktop.rs:286-303).
  - **Path is not set on the host side**, and there is **no session start
    time**.
- **Do (Rust)**:
  1. Add to `SessionStatus` (lib.rs:125-165)
     `#[serde(default, skip_serializing_if = "Option::is_none")] session_ms: Option<u64>`,
     the age of the current viewer session computed at status time.
     Monotonic, so no clock skew over IPC.
  2. Record `Instant::now()` in the shared status when the handshake
     succeeds (host.rs:172) and clear it when the session ends (worker
     stop, host.rs:276-289). There is only one viewer because of the
     semaphore (host.rs:140), so one slot is enough.
  3. In `Host::status()`, set `path` from `self.registration.diagnostics().direct`:
     `active_direct > 0` gives "direct", an active relay session gives
     "relay", otherwise empty. `Registration::diagnostics()` is already used
     at host.rs:113.
  4. Dart: add `sessionMs` to `DesktopStatus.fromJson`
     (desktop_api.dart:86-119). Show "watching" when
     `state ∈ {connecting, streaming, capture_unavailable}`, and
     "controlling" from `controlling`.
- **Compatibility**: the background helper is a separately installed
  binary. An older helper returns no `session_ms` or host `path`, so the UI
  must show "unknown" rather than "relay". The helper is upgraded through
  `helper_action install`.
- **Not possible without a protocol change**: *who* is watching, such as a
  device name. `Command::Authenticate` is closed (wire.rs:12-20), so a new
  field would break old hosts. Skip it, or gate it on a host feature.

### 7.7 Connection quality overlay (both viewers)

- **Exists** in the viewer `DesktopStatus`:
  - `path`: sampled once (viewer.rs:173-179), so it can be stale.
  - `receive_fps`, `capture_fps`, `dropped_frames`.
  - `bitrate`: the host's *target*, not a measurement.
  - `bytes`: a cumulative counter, so a measured bitrate is
    Δbytes/Δt in Dart.
  - `video.width/height/codec`.
  - `frame_ack_ms`: presentation round trip; its meaning changes under P0-1.
  - Dart `DesktopStore.presentFps` (desktop_store.dart:55-59, 589-595),
    **for H.264 only**: the JPEG path never updates it.
  - Android `stats` channel: `receive_to_present`, `input_wait`,
    `decode_callback`.
- **Missing**:
  - **Network RTT**. The host measures a transport probe baseline
    (`congestion.rs:121-143`, `baseline`), but it is not exported to the
    viewer.
  - A live path.
  - Display fps on the JPEG path.
- **Do**: no dependency.
  - Add an optional `rtt_ms` (transport-only, from the periodic probe that
    P0-1 introduces) to `SessionStatus` on the host. Coordinate the field
    name and semantics with the perf owner: "picture latency" must follow
    the new receipt-ack model.
  - Sample `connection.diagnostics()` in `Viewer::status()`, which is polled
    every 250 ms, instead of once at connect, so `path` stays live. Do not
    sample per frame; that cost is why it is sampled once today
    (viewer.rs:170-172).
  - Count presented `RawImage` updates for JPEG.
- **Coupling**: entirely with the perf map (P0-1, P0-2, P1-2, P1-3).

### 7.8 Quality preference (desktop Display menu)

- **Exists**:
  - `VideoPreferences` with validation (lib.rs:178-208).
  - The Dart request is hard-coded at `desktop_api.dart:436-446`:
    1920×1080, 12 Mbit/s, fps = Android refresh clamped to 30..90, else 90.
- **Do**:
  - Add `enum DesktopQuality {quality, balanced, smooth}`, persisted in
    `AppPrefs` (global), optionally overridden per remembered computer.
  - Map it to `{max_fps, bitrate}` targets defined by the perf work
    (P0-2/P0-4; phone default 1080p / about 4 Mbit/s / ≤60 fps). The bounds
    already allow this: fps ≥30 and bitrate ≥1 Mbit/s.
  - Pass the choice into `DesktopApiClient.connect(…, quality:)`.
- **Mid-session change**: there is no wire command, because
  `VideoPreferences` only travels in `Authenticate`. For now, apply a change
  by an explicit reconnect (`disconnect` + `connect` with the same target),
  which never replays input. A future
  `Command::Preferences { video }` must be **gated on a host-advertised
  feature** (wire.rs is closed), and must not change resolution
  automatically (P0-2).
- **No dependency.**

### 7.9 Toolbar pin / collapse (desktop viewer)

- **Exists**: nothing; the current viewer uses an `AppBar`.
- **Do**: pure Flutter. Use `MouseRegion` exit plus a 2 s `Timer` that
  collapses to a top handle. Persist the pinned state in `AppPrefs` (e.g.
  `desktop_toolbar_pinned`), and cancel the timer on dispose.
- **Invariant**: toolbar animation must not rebuild the `Texture` subtree or
  message the host (P1-4).
- **No dependency.**

### 7.10 Shortcut menu and modifier bar (both viewers)

- **Exists**: the `Key` subset listed in 2.11. Home and End **already
  exist** end to end; Ctrl+C/V work through `A/C/V/X/Z` plus `Control`.
  Text insertion is `Input::Text`.
- **Missing**: F1-F12, Insert, CapsLock, PrintScr, and letters other than
  a/c/v/x/z for Win+letter or Ctrl+letter combos (e.g. Win+D, Win+E, Win+R,
  Ctrl+S). There is no sticky-modifier state.
- **Do**:
  1. Rust (`crates/pb-mapper-desktop/src/lib.rs:262-287`): add variants
     `F1..F12, Insert, CapsLock, PrintScreen` and either letters `B..Y`
     plus digits, or one validated `Char { c }` restricted to `[a-z0-9]`.
  2. Map them in `windows.rs:538-563` to `enigo::Key::F1..F12`, `Insert`,
     `CapsLock`, `PrintScr` and `Unicode(c)`. enigo 0.6.1 has all of these,
     so no new crate is needed.
  3. Dart: extend `_keyNames` (desktop_viewer.dart:541-568) and add a
     shortcut catalogue for the Shortcuts menu and the Fn row.
  4. Make sticky Ctrl/Alt/Shift/Win viewer-side state that sends key-down
     on lock and key-up on unlock. It **must also be released** on
     `release`, end of control, `setViewerForeground(false)` and reconnect
     (invariant 4).
  5. Keep Ctrl+Alt+Del visible as "not supported".
- **Compatibility (hard)**: an old host parsing an unknown `Key` variant
  fails `read_json_or_eof` and **drops the session** (host.rs:180). The
  host must advertise support first, e.g. a new optional
  `SessionStatus.features: Vec<String>` containing `"keys_v2"`. That is
  safe because `SessionStatus` is open. The viewer FFI rejects or disables
  new keys when the feature is absent.
- **No dependency.** Coupling: the input-path map (P1-3, input thread),
  l10n, and Icons (`keyboard`, `command`).

### 7.11 New gestures and virtual mouse (mobile viewer)

- **Exists**: `desktop_gesture_surface.dart`, with tap = left, two-finger
  tap = right, long-press-move = drag, and two-finger pinch/pan local.
  Tests are in `desktop_gesture_test.dart`.
- **Do**:
  - Gestures as in RustDesk: long press = right click, double-tap-hold-move
    = drag, three-finger vertical = scroll, two fingers = local
    navigation.
  - Optional virtual mouse in direct-touch mode.
  - Use Flutter's built-in recognisers (`SerialTapGestureRecognizer` for
    double-tap-hold, `LongPressGestureRecognizer`, pointer counting for
    three fingers) or the existing raw `Listener` logic.
  - **No dependency.**
  - Update `desktop_gesture_test.dart`: replace the two-finger right-click
    case and keep the pinch invariant test.
- **Docs**: the gesture change needs release notes and an update to
  `docs/remote-desktop.md:45-49`.

### 7.12 Files to touch, by capability (quick index)

| Capability | Rust | Dart | Native / config |
| --- | --- | --- | --- |
| QR | — | new `widgets/qr_invite.dart`, host card page | `pubspec.yaml` (`qr: 3.0.3`); optional `MainActivity.kt` FLAG_SECURE |
| Scan | — | add-computer mobile page | `pubspec.yaml` (`mobile_scanner`), `ios/Runner/Info.plist` (`NSCameraUsageDescription`), maybe `settings.gradle.kts` Kotlin bump |
| Remembered list | `desktop.rs` `Request::InspectInvitation` | new `state/remote_computer_store.dart`, `desktop_api.dart`, `fake_desktop_api.dart`, `app_store.dart` | `pubspec.yaml` (`flutter_secure_storage: 11.2.0`), `AndroidManifest.xml` backup rules, iOS entitlements, `release-ui.yml:162` (libsecret), local VS ATL component |
| Online status | — | `app_store.dart` (derive), remote list page | — |
| Thumbnail | — | `desktop_gesture_surface.dart` (key/snapshot), `desktop_store.dart` (pre-disconnect hook) | fallback: `DesktopVideo.kt`, `desktop_video.cpp` `snapshot` |
| Host viewer info | `lib.rs` (`session_ms`), `host.rs` (start/clear, path) | `desktop_api.dart` parse, host card | helper upgrade path |
| Quality overlay | `lib.rs`/`congestion.rs`/`viewer.rs` (rtt, live path) | overlay widget, `desktop_store.dart` (JPEG fps, Δbytes) | Android `stats` |
| Quality preference | (later) gated `wire::Command` | `app_prefs.dart`, `desktop_api.dart` `connect` | — |
| Toolbar pin | — | viewer page, `app_prefs.dart` | — |
| Shortcuts / modifiers | `lib.rs` `Key`, `windows.rs` `key_code`, `SessionStatus.features` | `desktop_viewer.dart` `_keyNames`, new modifier bar | — |
| Gestures | — | `desktop_gesture_surface.dart`, tests | — |
