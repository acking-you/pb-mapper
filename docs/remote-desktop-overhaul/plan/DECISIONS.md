# Decisions in force (set by the lead; designers must follow them)

These settle the cross-cutting questions the subsystem maps raised. If a
design finds one of them unworkable, say so explicitly with evidence instead
of silently deviating.

## Protocol and compatibility

- D1. PR #51 was never released. Do NOT keep wire compatibility with PR #51
  builds. Bump the desktop ALPN to `pb-mapper/desktop/2` (security.rs) so an
  old peer fails cleanly at the handshake with a clear "update pb-mapper"
  message instead of mid-session parse errors. Within v2, keep
  `deny_unknown_fields` (strictness is a security property) and add a
  host-advertised capability list in SessionStatus only where a future v2.x
  extension is plausible.
- D2. The installed Windows helper/background service runs the same host
  code; the design must state how the helper is upgraded (version check +
  reinstall prompt on mismatch is acceptable).
- D3. Invitation format: emit a compact text invite `pbd1.` +
  base64url-no-pad(binary v2 payload: service name, SHA-256 fingerprint of the
  host certificate DER, bearer token). The viewer pins the host certificate by
  fingerprint (constant-time compare). Paste/scan also accept the old v1 JSON.
  A new FFI op `inspect_invitation` returns the recognised host (service,
  short fingerprint) without connecting, and rejects pb-mapper setup strings
  with a specific error. Persisted host trust (`desktop-trust.dpapi`) is
  re-encoded on load; no user action needed.

## Performance architecture

- D4. Send credit returns on RECEIPT (cumulative receive ack from the viewer's
  network task, never through FFI/Dart/decoders). Presentation receipts stay
  ONLY as input authorisation (fresh presented frame, 2 s window, sequence and
  desktop generation unchanged). Document and test that receipt acks grant no
  input rights.
- D5. Capture paces evenly at the target fps; no pause/resume flow control.
  Delay-based adaptation (TestDelay-style 1 s probe answered by the viewer's
  network task) adjusts fps and bitrate only. The encoder resolution stays
  fixed for the session except on explicit user choice or display switch.
  Delete the automatic resolution ladder. A long-term low-bandwidth
  resolution step is NOT part of this cut.
- D6. Changing fps at runtime must not rebuild the encoder: run the encoder at
  the session's max fps and drop/skip capture ticks (or use a variable frame
  rate timestamp model) — the design must pick one and verify CBR budgets.
- D7. Infinite GOP with keyframes only on demand (new session, resync, decoder
  request, display/resolution change), SPS/PPS in every IDR, keyframe
  requests rate-limited on both sides, and recovery must not depend on
  presentation acks. Verify vendor behaviour of the GOP setting with
  `video_bench` keyframe counts (the setting's result is currently ignored).
- D8. Viewer-side backlog policy: keep only the newest decodable frame
  downstream of the network; on backlog, drop to the next IDR and request one
  (rate-limited); measure presented fps and send an fps cap (90% direct /
  80% relay) to the host as a runtime limit separate from VideoPreferences.
- D9. Phone pipeline: H.264 frames bypass Dart. Android: a Kotlin feeder
  thread pulls frames from Rust through JNI (`jni` crate already in
  Cargo.lock) and reports presentation/keyframe needs straight to Rust; Kotlin
  calls `System.loadLibrary("pb_mapper_ffi")` (verify single instance with a
  static counter in a debug log). Windows: the runner's decoder thread pulls
  frames through plain C exports looked up from the already-loaded DLL. Dart
  receives only status, geometry, stats and the latest cursor. The JPEG
  fallback path (other platforms) stays in Dart. Exactly one consumer per
  session pops video.
- D10. Input must never wait behind capture/encode or acks. Whether that is a
  dedicated injection thread or priority handling on the desktop-bound worker
  thread is the design's call, but it must keep: desktop binding per
  injecting thread, fail-closed authorisation, generation checks, and the
  helper IPC's request/response discipline. The receive task must not block on
  worker replies for input.
- D11. Cursor: host never echoes the cursor position to the operator who sent
  input within the last 300 ms; cursor status leaves the video queue; Dart
  reads the latest cursor via a non-blocking FFI call per rendered frame.
- D12. Quality preference Quality / Balanced / Smooth maps to bitrate + fps
  targets (Balanced default: 1080p, ~4 Mbps, <= 60 fps). Changing it mid
  session is a live command (no reconnect). The design specifies the numbers.
- D13. Backgrounded viewer (Android app paused / desktop window minimised):
  viewer sends a pause (fps cap 0 or an explicit pause command) and resumes
  with a keyframe request; never let the host stream full rate to a hidden
  viewer, and never trip a recovery loop.
- D14. Every viewer/store notification fan-out to AppStore must stop: the
  desktop store notifies only its own listeners; the 250 ms status poll stops
  when nothing needs it.

## UI decisions

- D15. Remove the global sharing banner. Show a compact, always-on-top
  "being viewed / being controlled" bar ONLY while a remote viewer is
  connected, with Disconnect and Stop sharing. Sharing state otherwise lives on
  the rail entry (status dot), the host card, and the tray menu.
- D16. Modifier keys: tap = one-shot (applies to the next key or click), tap
  again while armed = lock, tap a locked key = release (RustDesk-like). All
  modifiers release on reconnect, viewer close, or focus loss. Every key/click
  carrying modifiers is sent as explicit down/up wrapping so nothing can stay
  held on the host.
- D17. Extend the host Key vocabulary to cover the Shortcuts menu and modifier
  bar (letters A-Z, digits, F1-F12, Home/End/PageUp/PageDown/Insert/Delete,
  arrows, Tab/Esc/Enter/Backspace/Space, Win). Win+L is a separate
  control-gated host command `lock` (LockWorkStation in the interactive
  session; return a clear error if it fails). Ctrl+Alt+Del stays unsupported.
- D18. Gestures (mobile, trackpad mode default): tap = left click (send
  immediately, do not delay single taps by ~300 ms; resolve double-tap-hold by
  converting the already-sent click into a drag only when the second touch
  arrives within the double-tap window and holds), long press = right click,
  double-tap-hold = drag, three-finger vertical = scroll, two fingers =
  local pan/pinch (P1-4, never sent to the host). Two-finger tap as right
  click is dropped. Direct-touch mode with an optional virtual mouse.
- D19. Mobile keeps the shared PageFrame header (no large title). Keep
  GlassSegmented's capsule corners. Undrawn screens follow the design-spec
  map's proposals.
- D20. Remembered computers (viewer side): stored by Rust in an encrypted file
  — Windows via DPAPI (reuse desktop_trust.rs code), Android via an Android
  Keystore AES-GCM key exposed by a small Kotlin method channel (Rust/Dart
  pass the wrapped blob). Other platforms: "Remember this computer" is
  disabled with an explanation. Do NOT add flutter_secure_storage (its
  Windows plugin needs ATL, missing on this machine). Set Android
  `allowBackup` exclusions so the remembered list and credentials are not
  backed up.
- D21. New Dart packages allowed: `qr` 3.0.3 (QR rendering with our own
  painter) and `mobile_scanner` 7.5.0 (Android/iOS camera scan,
  `returnImage: false`, bundled ML Kit model). Bump the Android Kotlin plugin
  only if the build requires it.
- D22. Online status for remembered computers comes from the relay key list
  AppStore already polls; no new FFI.
- D23. Last-picture thumbnail: on viewer close, the Android/Windows native
  decoder writes a small JPEG/PNG of the last presented frame to the app's
  private storage (native side has the pixels; Dart does not). Deleted with
  the remembered computer. If a platform cannot snapshot, show the
  placeholder — never block closing on it.
- D24. Host viewer info: host status gains session start time, path (direct /
  relay), and whether the viewer is controlling. "Make view-only" revokes
  control live.
- D25. Icons and title bar exactly as BRIEF requirements 3 and 4. The desktop
  viewer's dark toolbar uses a dark variant of CaptionButtons (same 46x32
  geometry). Tests must load the weight-400 Lucide font too; regenerate
  previews/posters.
- D26. Desktop-tunnel services (`desktop:*`) are hidden from the Tunnels list
  and the galaxy's ordinary service planets; they appear in Remote desktop.

## Process

- D27. Each work package lands as its own commit(s) on
  `claude/remote-desktop-overhaul` with tests; title-bar and icon commits stay
  separable so they can be cherry-picked to master.
- D28. Merge-block statements in docs stay until real-device evidence exists
  and the user accepts fluidity.

## Added by the lead at Wave 0 (after the contracts and WORKPLAN)

- D29. Master #52 (merged into the integration branch at Wave 0) lets a
  temporary key be issued into a shared namespace, so a key's namespace need
  not equal its key id (`pb-mapper-auth/src/ids.rs` `KeyId::namespace`). The
  existing `AppStore.ownNamespace = keyId` predates that and is master's
  concern, not this overhaul's. Rules here: the desktop trust scope keeps
  `credential.key_id()` (trust binds to the credential). Remembered-computer
  presence must not add new reliance on `ownNamespace`: for a temporary
  credential, a computer is online when a service with its name is in
  `AppStore.relayServices` (that list is already confined to the
  credential's own namespace); for the administrator key, the service must
  also be in namespace 0 (the viewer connects without a namespace, i.e. in
  the administrator's own). `ComputerPresence.otherRelay` compares the relay
  server address and the credential key id the computer was remembered under.
- D30. Owner decisions OD-1..OD-4 (WORKPLAN 1.3): the defaults are taken and
  may be changed later by the owner. OD-1: tray active state + tooltip and
  `windowManager.show(inactive: true)` on a viewer's arrival; no
  always-on-top. OD-2: one immediate reconnect after an Android freeze is
  accepted. OD-3: "SDK: breaking" in the release notes; the version bump
  happens at release, not on this branch. OD-4: master was merged in Wave 0.
- D31. Machine facts for packages: worktrees live on `<package worktrees>\pN` (D: is
  short of space); each worktree builds into its own `target/`. 32 logical
  CPUs, so a package should pass `-j 8` to cargo when other packages build at
  the same time. The pinned Android signing key resolves on this machine
  (`~/.android/debug.keystore` matches
  `scripts/release/android_signing_certificate.sha256`), so release APKs
  build. No physical phone is attached; an x86_64 Android emulator
  (`emulator-5580`) is, and serves for functional on-device checks (its
  decoder is software, so it proves function, never fluidity). Real-phone
  measurement stays with the owner at the end.
- D32. At the owner's request, the design artboards and every development
  doc were published into PR #51 as commit `1919fe8` on
  `origin/codex/issue-50-remote-desktop` (on top of `a41eaa7`), under
  `docs/remote-desktop-overhaul/` (README, design/, perf-priorities.md,
  ui-redesign-checklist.md, plan/, proposals/, code-maps/), and the PR
  description gained a "Redesign and performance overhaul" section with the
  15 rendered artboards. The integration branch does not contain `1919fe8`
  yet: merge `origin/codex/issue-50-remote-desktop` into it before any push,
  and when the overhaul lands, update that folder's README status line
  (it says the implementation is not yet pushed).
  Done after the adversarial review: the integration branch merged
  `1919fe8`, and the README status line was updated in the same pass.
