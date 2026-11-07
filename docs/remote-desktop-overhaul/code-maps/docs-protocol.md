# Map: documentation, documented protocol, and acceptance tooling (remote desktop, PR #51)

All paths are relative to the repo root
`<repo>` (branch
`claude/remote-desktop-overhaul`, head `a41eaa7`). Line numbers are for that head.
The design canvas paths are relative to the scratchpad `design/project/` folder.

---

## 1. Overview

The remote-desktop feature is documented in one long design/usage document plus a
set of per-build validation records. The UI design doc, the UI README, and
CLAUDE.md barely mention it, or not at all. Acceptance is done with six Rust
examples, one Flutter acceptance entry, a Node loopback bootstrap, a PowerShell
evidence collector, and opt-in (ignored) Rust tests.

| Artifact | Role | State |
| --- | --- | --- |
| `docs/remote-desktop.md` (571 lines) | Main doc: usage, trust model, helper/UAC, SDK, media/perf model, measurements, support matrix, build, licensing | Detailed but describes the presentation-ACK credit model the overhaul removes |
| `docs/validation/desktop-input-backpressure.md` (227) | Latest record: installed-input fix, encoder drain, startup probe, **merge block + performance handoff (zh-CN)** | Merge blocked: "Do not merge PR #51 until the performance issue is resolved and real-device acceptance passes" (`:3-8`, `:160-161`, `:221-226`) |
| `docs/validation/desktop-922.md` (163) | Build 922 record: measurements, installed host checks, rejected experiments | Historical. Has a "superseded" note at `:3-6` |
| `docs/validation/desktop-recovery-914.md`, `windows-helper-915.md`, `windows-tray-916.md`, `windows-session-918.md`, `windows-restart-919.md` | Per-build repair records | Historical. No performance model |
| `docs/validation/*.json` (7 files: `desktop-922.json`, `desktop-922-installed.json`, `desktop-925-installed-input.json`, `desktop-video-backpressure.json`, `android-cellular-927.json`, `windows-direct-1080p90*.json`) | Evidence counters and hashes | Historical evidence |
| `scripts/test/desktop-ui-acceptance.md` (124) | Native UI performance acceptance procedure | The only documented FPS procedure. Windows-centric |
| `scripts/test/desktop-bootstrap.cjs` (26) | Loopback HTTP server for `controller.json` | Works for Windows. Android needs ADB reverse forwarding |
| `scripts/test/windows-helper-acceptance.md` + `.ps1` | UAC/secure-desktop acceptance matrix and read-only evidence collector | Rows NOT RUN |
| `ui/tool/desktop_viewer_acceptance.dart` (226) | Flutter acceptance entry: real store, viewer and native decoder; 1 Hz samples written to a JSON report | Depends on Dart-side presentation path |
| `crates/pb-mapper-desktop/examples/*.rs` (6) | Probes: `capture_probe`, `deployed`, `viewer_probe`, `video_bench`, `transport_probe`, `input_acceptance` | Development probes |
| `docs/ui-design.md` (371) | POCOX UI rules | **No remote-desktop content at all**. Five places, Lucide at 300 |
| `ui/README.md` (151) | UI README | Remote-desktop paragraph at `:3-12`. The page count is already stale |
| `CLAUDE.md` (484) | Repo guide | **No mention of `pb-mapper-desktop`, desktop pages, desktop FFI, or `DesktopStore`** |
| `docs/nat-traversal-validation.zh-CN.md:93-98` | Cross-reference to the merge block | Must be updated when the block lifts |
| `docs/ui-updates.md:84-87` | Build counter | Stale ("next distribution must use at least 920"; builds up to 927 already used) |
| `CHANGELOG.md` | Release notes | **No remote-desktop entry**. The feature is unreleased; the gesture change needs to go here |
| `ui/lib/l10n/app_{en,zh}.arb` | User-visible gesture help (`desktopGestureHelp`, `:4`) | Describes the old gestures |

---

## 2. Components and responsibilities

### 2.1 `docs/remote-desktop.md`, section by section

- `:1-21` Status and milestone. Issue #50 is incomplete: UAC acceptance is outstanding and Android was validated in a VM only (`:9-14`). The milestone is "1080p direct viewing at or near 90 presented FPS"; the UI requests 1080p / 90 FPS / 12 Mbit/s and Android caps at the display refresh rate (`:16-21`).
- `:23-60` Use the UI: the host enable flow, invitation, controller flow, gestures (`:44-50`), single display and single viewer, input geometry, rotated displays rejected, generic cursor indicator.
- `:62-132` Remembered authorization and unattended use: bearer invitation, relay/namespace binding, the action table (`:77-83`), DPAPI storage, the service host, proxy/firewall notes, the service log, owner-session waiting.
- `:134-222` Windows helper and UAC boundary: protected install, the `PbMapperDesktop` LocalSystem service, the SYSTEM worker in the user session, desktop generation, SCM recovery, unsupported SAS.
- `:224-262` SDK ownership and transport: `Host`/`Viewer` APIs, backend threading, `ControlAdmission`, TLS 1.3 pinning, input binding.
- `:264-376` Media and rendering. This is the **documented performance model** (§3.3).
- `:378-535` Measurements and remaining acceptance, plus the support matrix (`:487-500`).
- `:537-571` Components, builds, licensing. The RustDesk reference is cited as `../rustdesk` at `49516c9` (`:539-540`).

### 2.2 Validation records

- `desktop-input-backpressure.md` covers the installed-worker `SendInput` failure (Win32 5), the cause (`DESKTOP_JOURNALPLAYBACK` missing; access mask `0x181` changed to `0x1a1`, `:33-41`), input failure ending the grant (`:51-54`), the reconnection and invitation-retention fix (`:56-68`), the async encoder drain (`:70-84`), and the startup probe (`:86-93`). It includes a delay-proxy table (`:95-110`) and acceptance status (`:112-149`). The handoff (`:153-227`, Chinese) lists the required evidence: a same-condition baseline against RustDesk, a per-frame trace correlated across stages, and real-device frame intervals, stalls and latency. It says 30 fps is not an automatic pass (`:224-225`).
- `desktop-922.md` shows the final Windows result of 84.75 presented FPS (`:118-121`) and the Android VM table (`:129-134`). It records two rejected experiments: a two-Surface slot and a reference mean (`:141-148`).

### 2.3 Acceptance tooling

| Tool | Measures | Does not measure |
| --- | --- | --- |
| `examples/capture_probe.rs:1-19` | `capabilities()`, `displays()`, and the time for 10 JPEG `backend.capture()` calls | H.264, network, presentation |
| `examples/video_bench.rs` | Capture and encode only. Args: `seconds` (1..300, default 10), `fps` (default 120), `--max-width/--max-height/--bitrate` (default 24 Mbps), `--backend` (full SDK backend; `--poll-us`, default 8333), `--output FILE` (raw Annex-B), `--pause-ms 300..1000` (an expired GPU output must recover on an IDR, `:119-132`, `:177-184`), and a forced IDR at 1.5 s that must arrive (`:134-138`, `:218-220`). **`--pattern-only`** runs a 960×540 topmost window that changes colour every `DwmFlush` and draws "Unique desktop presentation N" (`:255-357`). This moving source is used by the UI acceptance | Network, viewer |
| `examples/deployed.rs` | Against a deployed relay: issues an expiring temporary key (`issue_key(seconds+300)`, `:86-91`), starts `Host::start` on display 0 (`--control` sets `allow_control`), writes `controller.json` `{server, credential, transport, invitation}` (`:96-97`), and prints `host_ready`. In `serve` mode it prints per-second host `state/codec/unique_capture_fps/frames/dropped/message` and stops on `<output>/stop`. In `selftest` mode it connects `Viewer::connect_with_video_preferences` (no presentation feedback) over Auto and RelayOnly and prints `unique_receive_fps/receive_mbps/dropped`. `--feedback-log` forces the filter `off,pb_mapper_desktop::feedback=debug` and ignores `RUST_LOG` (`:42-52`). `--tcp`, `--max-width/--max-height/--fps/--bitrate`. It always revokes the key and deletes `controller.json` (`:127-129`) | Presentation |
| `examples/viewer_probe.rs` | Reads `controller.json`. Args: `seconds` (5..120), `delay` (simulated presentation delay, 0..1000 ms), `ack` interval (10..500 ms), `auto\|relay`, and an optional raw H.264 output file. Uses `connect_with_presentation_feedback` at `max_fps: 60`, then ACKs the newest due frame on each tick (`:228-234`). Prints `unique_fps`, `keyframes`, `mbps`, `bitrate`, `dropped`, `ack_rtt_ms`, and **receive-gap p50/p95/max after a 5 s warmup** (`:240-243`). This is the **only existing tool that reports an interval distribution**, and it measures receive time, not presentation | Real decoder or display |
| `examples/transport_probe.rs` | Throughput and 64 B ping RTT for the cases raw, codec, tls-split and tls-combined. `--local` (testkit relay), `--auto` (direct; prints direct diagnostics), `--consumer-server`, `--mib 1..64`, `--nagle`, `--host` (registration check only). Revokes its temporary key (`:207-210`) | Video |
| `examples/input_acceptance.rs` | Windows only. Creates an owned window with an EDIT control, guards foreground and hit-testing on every input (`:99-195`), then checks Unicode text `"远控验证 ✓ 🙂"`, move, left down/up, and **held-button release on disconnect** (prints `disconnect_release_ms`, `:545-549`). Uses a local relay by default, or `--relay-config`, `--tcp`, `--consumer-server`. Video is `1280x720@30, 2 Mbps`, RelayOnly (`:519`) | Video latency |
| `ui/tool/desktop_viewer_acceptance.dart` | See §3.4 | Interval distribution, latency, input-to-display |
| `scripts/test/windows-helper-acceptance.ps1` | Read-only evidence for the stages HostInventory, Baseline, Installed, Disabled and Uninstalled. Never installs, elevates or injects (`windows-helper-acceptance.md:10-13`) | Pass/fail |
| Ignored Rust tests (`src/slow_link.rs`) | `delayed_feedback_does_not_starve_async_encoder` (`:158-167`; cases low_delay 20 ms, fixed_wan 350 ms, slow_consumer 67 ms/frame), `sustained_slow_tls_reader` (`:380-382`), `presentation_feedback_recovers_after_slow_tls_and_renderer` (`:624-627`). Uses a synthetic source over real TLS and relay with a simulated renderer | Real GPU or phone |
| Opt-in Windows test `helper/worker.rs:466` | Production desktop access mask plus a zero-distance `SendInput` | — |
| Host feedback trace | `tracing` target `pb_mapper_desktop::feedback` at DEBUG, event `desktop_feedback_interval` (`congestion.rs:430-457`) | — |
| Android native stats | MethodChannel `pb_mapper/desktop_video` method `stats` (`DesktopVideo.kt:48`, `:254-272`). **Called only by the acceptance entry**, which is the only `.stats()` call site in `ui/` (`tool/desktop_viewer_acceptance.dart:85`) | — |

Not in the repo: the "independent Python/FFI probe" and the Tk input fixture used for the build 922/925 evidence (`desktop-922.md:99-102`, `desktop-input-backpressure.md:17-20`, `docs/validation/desktop-925-installed-input.json` "dedicated owned Tk input fixture"). No `.py` file in the repo calls `pb_mapper_desktop_call_json`. Those numbers cannot be reproduced from repo sources.

---

## 3. Data and control flow

### 3.1 Documented host flow (matches the current code)

1. Navigation. Tunnels page action `IconButton(key: 'open-desktop', icon: Icons.desktop_windows_outlined)` calls `openDesktop` (`ui/lib/src/pages/tunnels_page.dart:33-38`), which pushes a `MaterialPageRoute(DesktopPage)` (`desktop_page.dart:9-14`). Startup with `--desktop` (`ui/lib/main.dart:39`) pushes the same route (`ui/lib/src/app.dart:124-133`). The docs say "monitor button on Tunnels" (`remote-desktop.md:28`, `ui/README.md:3`).
2. **Enable remote control** (`desktop_page.dart:114-138`) calls `store.enableHost(service, display)`, then FFI op `host_enable {service, display_id}` (`ui/native/pb_mapper_ffi/src/desktop.rs:200-203`). The background settings are hard-coded to `allow_control: true, relay_only: false, privileged: true, auto_start: true, background: true` (`ui/native/pb_mapper_ffi/src/desktop_trust.rs:73-79`). One action covers the helper, the service, input and auto-restore, as documented at `remote-desktop.md:29-31` and `:79`.
3. The helper installer runs with UAC consent, the app restarts with `--desktop`, and the pending handoff resumes (`remote-desktop.md:142-155`; `windows-restart-919.md:7-14`).
4. The invitation is copied to the clipboard (`desktop_page.dart:96-104`). Advanced settings hold the service name and display (before sharing), the input switch (`desktopAllowControl`, `:228-234`), revoke (`:236-241`), helper install/disable/uninstall (`:242-269`), and relay-only for connecting (`:271-278`).
5. `DesktopSharingBanner` (`desktop_page.dart:305-338`) provides the persistent Stop documented at `remote-desktop.md:51-52`.

### 3.2 Documented session protocol (verified against code)

1. The viewer validates `VideoPreferences` and its codec list (1..2 entries), then round-trips `Invitation::decode` (`viewer.rs:136-141`).
2. `client.connect_tcp(invitation.service, options)`: the normal pb-mapper connection, direct first with relay fallback before commit (`viewer.rs:143-146`; `remote-desktop.md:244-248`).
3. TLS 1.3 client handshake over that stream. The connection pins the exact DER certificate from the invitation and rejects any chain (`security.rs:153-158`), then runs normal WebPKI validation with that certificate as the only root (`security.rs:113-122`). SNI is `desktop.pb-mapper.invalid` (`security.rs:11`), ALPN `pb-mapper/desktop/1` (`:12`), early data off (`:135`, server `max_early_data_size = 0`, `:99`). Timeout 15 s (`viewer.rs:147-155`).
4. The viewer sends `Command::Authenticate {token, codecs, video, presentation_feedback}` (`viewer.rs:156-169`).
5. The host reads it within 3 s (`IO_TIMEOUT`, `wire.rs:7`). It allows at most 2 codecs and picks H.264 if offered (`host.rs:158-159`). It compares SHA-256(token) in constant time (`host.rs:160-162`), validates preferences (`:163-164`), and takes the single-viewer semaphore, so a second viewer gets "a viewer is already connected" (`:140`, `:165`). Up to 8 concurrent handshakes are allowed (`:146`). A failed invitation never starts a backend: the worker is created only after authentication (`host.rs:174`).
6. Host to viewer: pairs of `[u32 BE len][SessionStatus JSON ≤ 8192]` and `[u32 BE len][bytes ≤ 4 MiB]` (`wire.rs:65-92`, `host.rs:266-269`). An empty-bytes pair means a status-only, cursor or heartbeat update (`host.rs:213-226`, `:246-257`). When feedback is enabled the first pair is a probe: `SessionStatus.roundtrip_probe = Some(1)` with empty bytes (`host.rs:193-200`, `congestion.rs:121-124`).
7. Viewer to host: `Command` JSON frames. A heartbeat `Ping` goes every 1 s (`viewer.rs:294`, `:313`). Others are `Control{enabled}`, `Release` (coalesced through the priority mailbox, `viewer.rs:28-79`), `Keyframe`, `Probe{nonce}` (echoed **from the network reader**, `viewer.rs:225-235`, `:306-311`), `Presented{generation, frame_id}` (`:312`), and `Input{event}` (bounded mpsc of 32, `:202`).
8. The host network receive loop answers `Probe` without going through the worker (`host.rs:181-184`). Other commands go to `worker.command`; a malformed frame terminates the transport (`host.rs:180`, `:185-187`).
9. Host send loop (`host.rs:190-273`):
   - It first checks for a stall: 4 s without ACK progress sends a status of `"stopped"` with the message "Renderer stopped acknowledging presented frames; reconnect the viewer" and ends the session (`:202-211`, `congestion.rs:375-380`).
   - A changed cursor is sent as a status-only pair, bypassing media admission (`:213-226`).
   - It pops media and runs `feedback.admit` to decide Send, Wait or Discard (Discard calls `media.resynchronize()`) (`:236-245`).
   - While waiting, it sends a 1 s liveness status (`:246-257`).
   - It writes the frame and calls `worker.sent` (`:264-271`).
10. Viewer status becomes `"stale"` when streaming has seen no video for more than 2 s (`viewer.rs:355-363`). `Viewer::input` requires `streaming`, `controlling`, and the same generation (`viewer.rs:402-409`). A full input channel triggers a revoke barrier and the message "input congestion; control released while video recovers" (`:410-419`).
11. The viewer computes `path` **once at connect** from `connection.diagnostics()` as direct or relay (`viewer.rs:170-188`) and stamps it on every status (`:236-238`).

### 3.3 Documented performance model (current code; this is what the overhaul replaces)

The pipeline from the docs (`remote-desktop.md:289-376`), with code references:

1. **Capture and encode (Windows).** DXGI Desktop Duplication, D3D11, GPU NV12, then a low-latency Media Foundation hardware H.264 encoder (`remote-desktop.md:289-295`).
   - `CODECAPI_AVLowLatencyMode = true` (`video/windows.rs:598`).
   - B-frames 0 (`:601`).
   - **`CODECAPI_AVEncMPVGOPSize = fps`** (`:607`), meaning an IDR every second. This is **not documented** anywhere in the docs.
   - "The capture schedule is independent of encoding duration. Static-screen refreshes at about 250 ms are marked duplicates and excluded from unique-frame FPS" (`remote-desktop.md:324-325`; `video/windows.rs:381`).
2. **Worker pause.** The worker calls `feedback.budget()` and sets `paused = waiting`, then `engine.capture_video(!paused)` (`worker.rs:222-254`). Capture stops while credit is exhausted. The encoder drain continues (`desktop-input-backpressure.md:70-77`).
3. **Admission** (`congestion.rs`):
   - `INITIAL_WINDOW` 64 KiB, `MIN_WINDOW` 16 KiB, `MAX_WINDOW` 512 KiB (`:11-13`).
   - `MAX_FRAMES = media::MAX_QUEUED_FRAMES = 8` (`:16`, `media.rs:10`).
   - `MAX_INDEPENDENT_FRAME` 256 KiB; above that the frame is discarded and the bitrate halved (`:17`, `:191-195`).
   - `MAX_READY_AGE` 250 ms: discard and IDR (`:19`, `:179-181`).
   - The first unit may exceed the window if nothing is in flight (`:196-205`).
   - While `recovering`, an IDR exclusively holds credit until its ACK (`:184-190`, `:199`, `:227-228`, `:234-249`).
   - Credit returns **only** on `Presented`, which the Dart store sends after native texture consumption (`:254-319`).
   - Documented at `remote-desktop.md:326-336`.
4. **Congestion and quality** (`congestion.rs:320-494`; documented at `remote-desktop.md:338-351`):
   - Delay is send-to-presentation latency (`:270`).
   - Baseline: the minimum of small predictive flights, or the startup probe RTT (`:128-144`, `:286-302`).
   - `congested_after = max(250 ms, baseline + 100 ms)` (`:465-467`).
   - The delay is averaged over 1 s (`OBSERVATION_INTERVAL`, `:383-394`). Two bad intervals halve the bitrate (`:400-406`).
   - After 4 s healthy (`RECOVERY_HOLD`): `+25% + 64 kbps`, up to the ceiling (`:407-419`).
   - An ACK stall of at least `max(1 s, congested_after + 250 ms)` multiplies the bitrate by 2/3 (`:325-335`).
   - `desired_window = bitrate × max(baseline + 50 ms, 125 ms)` clamped to 16–512 KiB (`:459-464`).
   - **The quality ladder changes resolution** (`:336-369`). Bitrate bands are under 0.5, under 1, under 2, under 4 Mbps, and above (`:477-485`). They map to 640×360@30, 640×360@60, 960×540@60, 1280×720@60, and the ceiling (`:360-366`). Going up needs 25% headroom and 5 s (`RESOLUTION_HOLD`); going down happens when the bitrate falls below 80% of the tier minimum. `MIN_BITRATE` is 256 kbps (`:18`).
5. **Viewer SDK queue.** At most 8 units and 8 MiB (`media.rs:10-11`). Overflow drops everything and waits for an IDR (`media.rs:47-62`). Documented at `remote-desktop.md:317-320`.
6. **FFI.** `wait_frame` waits up to **16 ms** (`ui/native/pb_mapper_ffi/src/desktop.rs:181`). Binary buffer format: a LE u32 metadata length, JSON status, then bytes (`desktop.rs:1269`); free with `pb_mapper_desktop_free_frame` (`:1281`). The Dart frame isolate has a single `credit` (`ui/lib/src/ffi/pb_mapper_service.dart:57-105`).
7. **Android decoder.** The input queue holds 4 (`DesktopVideo.kt:231`). Pending metadata is limited to 16, decoding plus outputs to 4 (`:492`). One SurfaceTexture slot (`:180-200`). A 2 s consumption stall triggers a reset (`:525-529`). Documented at `remote-desktop.md:353-364`.
8. **ACKs.** The Dart store coalesces ACKs at **33 ms** (`ui/lib/src/state/desktop_store.dart:620`). A presentation counts only if its status is under 2 s old (`desktop_store.dart:576-583`). It feeds `presentFps` (`:586-593`).
9. **UI requests.** `max_width 1920, max_height 1080, max_fps = Android ? refreshRate.round().clamp(30,90) : 90, bitrate 12000000` (`ui/lib/src/ffi/desktop_api.dart:428-447`). The SDK default is the same (`crates/pb-mapper-desktop/src/lib.rs:186-195`). Bounds are `max_fps 30..=120` and `bitrate 1M..=40M` (`lib.rs:198-207`).

### 3.4 Documented UI acceptance procedure (`scripts/test/desktop-ui-acceptance.md`)

1. Build the Windows FFI and helper (`scripts/build/pb-mapper-ffi-windows.ps1`), then build the examples `deployed` and `video_bench` (`:13-19`).
2. Build the acceptance entry with `flutter build windows --release --target tool/desktop_viewer_acceptance.dart` and the defines `setupUrl=http://127.0.0.1:18747/controller.json`, `reportPath`, `seconds`, `relayOnly` (`:26-31`).
3. Host: `deployed.exe APP_CONFIG_PATH target/desktop-acceptance serve 600 --tcp` (`:39-41`). It is view-only unless `--control` is passed.
4. Run `node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance`. This serves `GET /controller.json` on `127.0.0.1:18747`, at most 16 KiB, with `no-store` (`desktop-bootstrap.cjs:10-26`).
5. Run `video_bench.exe 60 90 --pattern-only` for moving content (`:49-52`), then launch the UI.
6. The entry (`desktop_viewer_acceptance.dart`):
   - It refuses a non-loopback `setupUrl` and enforces `seconds` 15..1800 (`:19-31`). It fetches the bootstrap with a 16 KiB bound (`:32-49`); the config may override `seconds` and `relay_only` (`:50-51`).
   - It initializes `PbMapperService`, using `<support>/desktop-acceptance` as the app directory (`:53-56`). It calls `updateConfig(server, keepAlive: true, credential, transport)` (`:62-70`).
   - It runs `DesktopViewer(store: DesktopStore(_AcceptanceApi()))`, then `store.load()` and `store.connect(invitation, relayOnly)` (`:71-79`).
   - It takes one sample per second (`:82-117`). The sample fields are listed in §4.7.
   - It computes `native_presented_fps` as the difference in the native `presented` counter over the samples with `elapsed_us ≥ 5 s` (`:121-140`). It writes the report (`schema: 1`) to `reportPath`, then disconnects and disposes (`:142-155`).
   - `_AcceptanceApi` counts `presented()` and `keyframe()` calls and their errors. These are **Dart-side calls** (`:159-196`).
   - `_RenderTimings` holds Flutter `FrameTiming` build, raster and total means and maxima (`:199-226`).
7. Check criteria (`desktop-ui-acceptance.md:57-65`). **Every** steady-state sample must show `state=streaming`, `path=direct`, H.264, 1920×1080, and no error, plus the drop counts. Decoder submissions and received frames do not count as presentation. Record the source and receiver refresh rates and the build.
8. Relay is checked separately with `relay_only=true`, and lower fps is allowed (`:77-79`). The Android VM is driven through "ADB's loopback bootstrap forwarding", meaning `adb reverse tcp:18747 tcp:18747`. This is not spelled out (`:79-82`).
9. Clean up with `target/desktop-acceptance/stop`: the host revokes its scope and deletes `controller.json`. Then rebuild the normal `lib/main.dart` entry (`:84-92`).

---

## 4. Contracts and wire, FFI and channel formats (exact names)

### 4.1 Invitation (bearer capability)

`crates/pb-mapper-desktop/src/lib.rs:89-123`, `#[serde(deny_unknown_fields)]`:

```json
{"version": 1, "service": "desktop:...", "certificate": "<base64url DER, ≤4096 chars>", "token": "<43 chars = base64url(32 random bytes)>"}
```

- Decode rules: text ≤ 8192 bytes, `version == 1`, `valid_service`, `token.len() == 43`, `certificate.len() ≤ 4096` (`lib.rs:108-122`).
- Service name: `desktop:` plus 1..120 chars from `[A-Za-z0-9._:-]`, total ≤ 128 (`lib.rs:49-56`). A slash is invalid (`remote-desktop.md:34-35`).
- It contains no relay credential. The UI binds the saved host grant to the relay address and namespace (`desktop_trust.rs:15-20`, `:36-43`; error text at `desktop.rs:986`).
- **Design impact.** The artboards show a compact invite such as `pbd1·k7Hq…Zx2Q` (`Main.dc.html`, `AddComputer.dc.html`) and a recognised "permission: view and control". Neither fits the current JSON. Because of `deny_unknown_fields` plus `version == 1`, any new field or encoding is **rejected by every existing viewer** (see §7).

### 4.2 HostIdentity (host secret)

`security.rs:24-30`: `{certificate, private_key, token}`, `deny_unknown_fields`, ≤ 16384 bytes (`:51-59`). The app stores it with current-user DPAPI in `desktop-trust.dpapi` (`remote-desktop.md:85-89`); the service uses machine DPAPI (`:94-96`). `Saved {schema, relay, namespace, settings, identity}` with `Settings {service, display_id, allow_control, relay_only, privileged, auto_start, background}` (`desktop_trust.rs:21-43`).

### 4.3 Desktop wire protocol (inside TLS)

- Framing: `u32` **big-endian** length, then the payload. JSON ≤ `MAX_JSON` 8192, bytes ≤ `MAX_FRAME` 4 MiB, `IO_TIMEOUT` 3 s (`wire.rs:5-7`, `:65-92`).
- Viewer to host: `Command`, `#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]` (`wire.rs:9-37`):
  - `{"op":"authenticate","token":…,"codecs":["h264"|"jpeg"],"video":{max_width,max_height,max_fps,bitrate}?,"presentation_feedback":bool?}`
  - `{"op":"control","enabled":bool}`, `{"op":"input","event":InputEvent}`, `{"op":"release"}`, `{"op":"ping"}`, `{"op":"probe","nonce":u64}`, `{"op":"keyframe"}`, `{"op":"presented","generation":u64,"frame_id":u64}`
  - **An unknown `op` or field fails to parse, and the host then ends the session** (`host.rs:180`, `:185-186`). Any new viewer command, such as a receive ACK, needs a host-advertised capability first. The precedent is `roundtrip_probe`, which the viewer echoes only after the host advertises it (`desktop-input-backpressure.md:91-93`, `viewer.rs:225-235`).
- Host to viewer: `SessionStatus` (`lib.rs:125-165`): `state, message, control_allowed, controlling, generation, frame_id, display{id,name,x,y,width,height}, path, direct_stage?, direct_failure?, frames, bytes, desktop, video{codec,width,height,keyframe,pts_us,duplicate}, capture_fps, receive_fps, dropped_frames, bitrate, cursor{x,y,visible}?, frame_ack_ms, roundtrip_probe?`. No `deny_unknown_fields`, so it is additive-safe for old viewers. States seen in docs and code include `waiting`, `streaming`, `stale` (viewer-local), `stopped`, `capture_unavailable`, `connecting`, and `waiting_for_owner_session` (service).
- `InputEvent {generation, frame_id, sequence, input}` (`lib.rs:289-295`). `Input` is `#[serde(tag="kind", rename_all="snake_case", deny_unknown_fields)]` with `move{x,y}`, `button{button:left|right|middle, down}`, `wheel{horizontal,vertical}` (|v| ≤ 100), `key{key, down}`, `text{text}` (≤ 1024 bytes, no NUL) (`lib.rs:244-254`, `:377-395`).
- **`Key` vocabulary is closed** (`lib.rs:262-287`): `shift, control, alt, meta, enter, escape, tab, backspace, delete, left, right, up, down, home, end, page_up, page_down, a, c, v, x, z`. There are no F1–F12 keys and no letters other than A/C/V/X/Z, so Win+D, Win+L, Ctrl+S and similar cannot be expressed. The Dart mapping `_keyNames` (`desktop_viewer.dart:541-568`) mirrors this list. Other characters go as `text` only when Ctrl, Alt and Meta are **not** held (`desktop_viewer.dart:244-251`).
- `VideoPreferences` (`lib.rs:178-208`), `deny_unknown_fields`: defaults 1920×1080, 90, 12 Mbps. Bounds: width 2..3840, height 2..2160, fps 30..120, bitrate 1M..40M.

### 4.4 FFI (`ui/native/pb_mapper_ffi/src/desktop.rs`)

- `pb_mapper_desktop_call_json` (`:510-511`). The request is `#[serde(tag="op", rename_all="snake_case", deny_unknown_fields)]` (`:193-255`). Ops: `capabilities, status, trust_status, trust_revoke, host_restore, host_enable{service,display_id}, helper_action{action}, background_action{action}, host_start{service,display_id,allow_control,relay_only?,privileged?,remember?,auto_start?,background?}, host_stop{on_exit?}, host_permission{allow_control}, connect{invitation,relay_only?,native_video?,video?}, disconnect, control{enabled}, release, keyframe, presented{generation,frame_id}, input{event}`.
- `pb_mapper_desktop_frame` (`:1221`), `pb_mapper_desktop_wait_frame` (`:1245`, 16 ms wait at `:181`), `pb_mapper_desktop_free_frame` (`:1281`). Buffer: `[u32 LE metadata length][JSON status][encoded bytes]` (`:1269`).
- CLAUDE.md says every export returns a JSON envelope except three integer calls (`CLAUDE.md:218-226`). That is **already false** because of these binary exports.

### 4.5 Native video MethodChannel `pb_mapper/desktop_video`

- Dart side `ui/lib/src/ffi/desktop_video.dart:39`. Methods: `create` → textureId, `submit` (packet), `reset{textureId}`, `stats{textureId}`, `dispose{textureId}` (`:65-115`).
- Events go out through `channel.invokeMethod("event", {textureId, generation, frameId, width, height, presented, needKeyframe, message})` (`DesktopVideo.kt:333`).
- Android `stats` returns `submitted, submittedKeyframes, decoded, presented, lastPresentedGeneration, lastPresentedFrameId, dropped, decoder, configurations, recoveries, lastRecovery, queued, decoding, outputs, surfacePending, timings` (`DesktopVideo.kt:258-269`).
  - `timings` contains `elapsed_ms, input_wait, decode_callback, ready_to_release, receive_to_present, surface_consume, consume_dispatch, pump_dispatch, pump_work` (each `{samples, mean_ms, max_ms}`), plus `peak_pending` and `peak_outputs` (`:85-112`).
  - **Timings are enabled whenever `stats` is called without `timings:false`.** Dart never passes `timings`, so the acceptance run always has them on (`DesktopVideo.kt:48`, `:255-257`).
  - There are no percentiles and no inter-present interval histogram.
- Windows `stats` returns only `submitted, decoded, presented, dropped, decoder` (`ui/windows/runner/desktop_video.cpp:181-187`).

### 4.6 Host feedback trace fields

Event `desktop_feedback_interval` (`congestion.rs:433-457`) has the fields `action` (hold, congestion_pending, decrease, probe, ack_stall), `samples, bitrate, quality, baseline_ms, baseline_calibrated, delay_ms, congested_after_ms, window_bytes, desired_window_bytes, outstanding_frames, outstanding_bytes, oldest_frame_id, newest_frame_id, recovering, waiting, last_ack_generation, last_ack_frame_id, last_ack_age_ms, unacknowledged_age_ms, healthy_ms`.

### 4.7 Acceptance report schema (`desktop_viewer_acceptance.dart:131-141`)

```text
{schema:1, platform, relay_only, warmup_seconds:5, elapsed_us, native_presented_fps, samples:[
  {elapsed_us, state, codec, path, direct_stage, direct_failure, capture_fps, receive_fps,
   present_fps (DesktopStore.presentFps), dropped, frame_id, bytes, bitrate, width, height,
   frame_ack_ms, has_error, error, recovery (recoveryReason), phase (connectionPhase.name),
   reconnects (reconnectAttempt), controlling, picture_age_ms, texture_id,
   presented_generation, presented_frame_id,
   feedback:{acknowledgments, ack_errors, last_generation, last_frame_id, keyframes, keyframe_errors},
   native:<stats map §4.5>, flutter:{frames, build_mean_ms, raster_mean_ms, total_mean_ms, build_max_ms, raster_max_ms, total_max_ms}}]}
```

`controller.json` (written by `deployed.rs:96-97`, read by the entry and by `viewer_probe.rs:144-150`): `{server, credential, transport, invitation}`. It may also carry `seconds` and `relay_only` (`desktop_viewer_acceptance.dart:50-51`).

---

## 5. Invariants and security boundaries (as documented, with enforcing code)

1. **The invitation is a bearer capability for viewing.** Input needs host permission plus an explicit request (`remote-desktop.md:36-38`, `:70-75`; `lib.rs:86-88`). A relay credential alone cannot authorise capture or input. A failed invitation does not start a backend (`remote-desktop.md:249-254`; `host.rs:157-174`).
2. **The certificate is pinned and the token is revealed only after the host is verified.** TLS 1.3 only, early data off. A certificate mismatch stops automatic retries and never replaces the pin (`remote-desktop.md:74-75`, `:249-254`). Tested by `ui/test/desktop_recovery_test.dart:226` and `src/tests.rs:16`.
3. **The grant is bound to relay and namespace.** Changing either does not silently reuse it (`remote-desktop.md:66-72`; `desktop_trust.rs:36-43`, `desktop.rs:986`).
4. **Every connection starts without control.** The UI requests a grant only after a fresh frame is **presented** (`remote-desktop.md:40-42`, `:72-74`). Input binds to generation, frame and a two-second freshness limit (`worker.rs:409`, `:453`, `:600`). Sequence numbers reject duplicates and reordering (`worker.rs:588`). Only a current rendered frame renews held input (`remote-desktop.md:192-194`, `:255-258`, `:334-336`). Cursor status never renews input (`:369-370`). Probe receipts never retire debt or grant credit (`congestion.rs:140-143`).
5. **Reconnect never replays input.** Queued clicks and keys are dropped (`remote-desktop.md:73-74`, `:275-277`, `:321-322`). The viewer's input channel is bounded at 32; overflow revokes control (`viewer.rs:410-419`).
6. **Input failure ends the grant, and the UI does not reacquire it automatically** (`desktop-input-backpressure.md:51-54`; test `desktop_recovery_test.dart:106`).
7. **Desktop generation.** Desktop switches and locks advance a generation, and the final input injection checks it. Failed releases are retried (`remote-desktop.md:184-191`). The helper is bound to the console owner's session and login (`:184-188`, `:121-131`).
8. **Helper and service.** The UI stays unprivileged. LocalSystem service, named-pipe ACLs, kernel PID and image checks, one restricted SYSTEM worker with a kill-on-close job, and only `PROCESS_QUERY_LIMITED_INFORMATION` added for the owner (`remote-desktop.md:162-194`; `windows-helper-915.md:20-26`). The worker desktop-handle access mask is `0x1a1`, adding `DESKTOP_JOURNALPLAYBACK`; UIAccess is not used (`desktop-input-backpressure.md:33-49`).
9. **No auto-consent and no policy change.** Nothing auto-approves UAC, changes policy, or stores a password. SAS / Ctrl+Alt+Del is unsupported (SendInput cannot do it). No `SoftwareSASGeneration` (`remote-desktop.md:157-160`, `:216-222`). Pre-login boot is unsupported.
10. **Stop vs revoke.** Stop keeps the invitation. Revoke deletes trust and needs a new invitation (`remote-desktop.md:77-83`).
11. **Bounded queues everywhere** (`remote-desktop.md:317-322`): 8 units / 8 MiB, 4 MiB unit, 8 KiB JSON, native 4 / 16, a 3 s blocked write. The regression guard is that queues stay bounded for slow consumers (brief).
12. **Secrets stay out of logs and evidence.** Never log or publish invitations or credentials. Acceptance reports exclude images and credentials (`desktop_viewer_acceptance.dart:147-149`). `deployed`, `transport_probe` and `input_acceptance` revoke their temporary keys. "No invitations, relay credentials or screen contents are included here" (`desktop-input-backpressure.md:151`).
13. **The invitation is never persisted on the controller** today. It is kept in page memory only (`desktop-input-backpressure.md:58-63`; `remote-desktop.md:67-68`, `:277`). This changes with "Remember this computer" (§7).
14. **Zoom and pan are local.** Pinch and pan keep the same mounted texture and decoder. An opaque stale overlay must not block consumption (`remote-desktop.md:286-287`, `:363-364`; `desktop_viewer.dart:491-493`). Android uses OpenGL with Impeller disabled (`ui/android/app/src/main/AndroidManifest.xml:15-20`).

---

## 6. Tests covering this area

Rust (`crates/pb-mapper-desktop`):

- `src/congestion.rs` tests:
  - `admission_bounds_startup_and_rejects_unearned_acknowledgments` (`:524`)
  - `an_unpresented_flight_cannot_overflow_the_receiver_queue` (`:548`)
  - `keyframe_recovery_retires_old_frames_without_resetting_unacknowledged_limit` (`:577`)
  - `replacement_idr_exclusively_holds_credit_until_its_actual_ack` (`:627`)
  - `unavailable_capture_retires_debt_but_new_video_still_requires_acks` (`:656`)
  - `renderer_baseline_and_congestion_recovery_use_real_ack_progress` (`:684`)
  - `startup_probe_measures_path_without_retiring_presentation_debt` (`:730`)
  - `slow_first_idr_does_not_normalize_persistent_video_delay` (`:757`)
  - `cumulative_small_frame_acks_calibrate_wan_delay_and_recover_through_jitter` (`:777`)
  - `resolution_recovers_with_headroom_without_chattering_at_rate_boundaries` (`:816`)
- `src/worker.rs`:
  - `input_requires_control_current_geometry_and_unique_sequence` (`:678`)
  - `native_input_failure_ends_control_and_survives_new_video` (`:742`)
  - `observation_never_acquires_write_admission_and_revocation_releases_it` (`:826`)
  - `unavailable_backend_reports_failure_and_stops` (`:872`)
  - `presentation_backpressure_drains_encoder_without_new_capture` (`:898`)
  - `unavailable_desktop_retires_old_ack_debt_and_resumes_control` (`:1022`)
  - `decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture` (`:1243`)
- `src/media.rs`: `overflow_discards_dependent_frames_until_idr` (`:133`), `status_is_coalesced_without_reordering_or_invalidating_video` (`:153`)
- `src/tests.rs`: `tls_rejects_an_unpaired_host_identity` (`:16`), `blocked_video_does_not_block_local_stop` (`:77`), `older_authentication_defaults_video_limits_and_rejects_unbounded_preferences` (`:176`)
- `src/viewer.rs:443` `repeated_releases_are_bounded_and_revocation_precedes_a_new_grant`
- `src/host.rs:375` `remembered_invitation_survives_restart_but_not_revocation`
- `src/wire.rs:117` `clean_tls_close_is_eof_only_between_messages`
- `src/scheduler.rs:152`
- `video/windows.rs:1112`, `:1119`
- Integration tests in `tests/session.rs`: `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown` (`:74`), `cursor_crosses_a_full_video_window_without_refreshing_the_picture` (`:207`)
- Ignored or opt-in tests: the three `slow_link.rs` tests (§2.3) and `helper/worker.rs:466`. Key assertions that encode the current model:
  - `slow_link.rs:283-287` asserts that a slow **consumer** must lower host bitrate ("actual consumer backlog must still apply backpressure").
  - `slow_link.rs:289` asserts `peak <= MAX_QUEUED_FRAMES`.
  - `slow_link.rs:780-785` asserts recovered `bitrate >= 4 Mbps`, `video.width >= 960`, and `frame_ack_ms < 350`.
  - `slow_link.rs:787-807` asserts that a renderer which stops ACKing ends the session within 6 s.

FFI (`ui/native/pb_mapper_ffi/src/desktop.rs`): tests at `:1319` (status and video with the same id cross the binary boundary), `:1348`, `:1406`, `:1429`, `:1485`, `:1576`, `:1655`, `:1678`, and `:1726` (a binary frame is owned until its explicit release).

Flutter (`ui/test/`):

- `desktop_gesture_test.dart`:
  - `:37` "touch taps wait for the gesture and two fingers click only right"
  - `:77` "pinch keeps the texture mounted and maps later taps through zoom"
  - `:125` "trackpad moves without dragging and releases a held drag on pinch"
- `desktop_test.dart`: `:19`, `:98` (touch/hover mapping; blocks ungranted or stale input), `:225` (ordering; view-only; release), `:280` (compact host flow)
- `desktop_lifecycle_test.dart`: `:98`, `:144`, `:199`, `:243`
- `desktop_recovery_test.dart`: `:106`, `:132`, `:158`, `:226`, `:251`, `:322`, `:351`
- `desktop_host_readiness_test.dart`: `:70`, `:121`, `:148`
- `desktop_viewer_navigation_test.dart:32`
- Fake API: `fake_desktop_api.dart`

Android unit tests: `ui/android/app/src/test/kotlin/com/pb_mapper/ui/VideoPresentationQueueTest.kt` (`decodedPicturesAreImmediatelyEligible…`, `pausedSurfaceDiscardsOnlySuperseded…`, `resetRetiresOldRevision…`).

Not covered by any automated test: physical-phone FPS, frame-interval stalls, end-to-end latency, UAC/secure desktop, and service restart or sleep. The docs say so explicitly (`remote-desktop.md:512-517`, `desktop-922.md:155-163`).

---

## 7. Impact on the planned overhaul

### 7.1 Per requirement

**P0-1 (credit on receipt; window = fps × RTT + margin; capture never paused for flow control).**
- Code coupling, for the media mappers: `congestion.rs` admit/presented, `host.rs:236-245` and `:202-211`, `worker.rs:222-254`, and the viewer network reader (`viewer.rs:212-292`, which must send the cumulative receive ACK from the reader task).
- Wire: a new `Command` such as `received {generation, frame_id}` is fatal to an old host (§4.3). It must be gated on a host advertisement, for example a new optional `SessionStatus` field like `roundtrip_probe`, or an `authenticate` field the host already ignores. `Authenticate` is `deny_unknown_fields`, so **adding a viewer→host Authenticate field breaks old hosts**. Use the host→viewer advert direction.
- Since nothing has shipped, an explicit decision may simply drop compatibility with dev builds 914–927. Record that decision in `remote-desktop.md`.
- Docs that become false: `remote-desktop.md:326-336`, `:338-351`, `:281-283`, `:357-360` (the wording "presentation credit"), `:410-414`, `:437-438`; `desktop-input-backpressure.md:106-110` and `:184-186`; the `desktop-922.md:25-27` "Changes retained" bullet (add a superseded note).
- The 4 s presentation-stall teardown (`remote-desktop.md:346-349`; `host.rs:202-211`; test `slow_link.rs:787-807`) needs an explicit new rule. A missing receive ACK means transport death or an unresponsive client (RustDesk drops to 2 fps after 2 s). A missing presentation must stay viewer-local.
- **Security:** keep `Presented` for input authorisation (`worker.rs:409/453/600`). The docs must still state that receipt ACKs never grant or renew input, the same way `congestion.rs:250-253` does today.

**P0-2 (delay-based fps/bitrate; resolution fixed; ~1 s periodic probe).**
- Remove the quality ladder (`congestion.rs:336-369`) and its test `resolution_recovers_with_headroom…` (`:816`).
- Rewrite `slow_link.rs:783-785`, which asserts `width >= 960` after recovery and so presumes resolution adaptation.
- Rewrite `slow_link.rs:283-287` (consumer backlog must lower host bitrate), which contradicts the requirement.
- The startup-only probe (`congestion.rs:119-144`, nonce fixed at `1`, `:121-129`) becomes periodic, so nonces must be unique and the stale-probe rule (`> IO_TIMEOUT`) kept.
- Feedback trace fields `quality`, `window_bytes`, `desired_window_bytes` and `recovering` (`congestion.rs:433-457`) will change. The brief and `desktop-input-backpressure.md` name `waiting`, `outstanding_frames`, `action`, `bitrate`, `quality`, `delay_ms` and `baseline_ms` as the existing instrumentation, so keep or rename them deliberately and document the new set.
- Docs: rewrite `remote-desktop.md:338-351` and `:20-21` (relay may reduce "frame rate and resolution").

**P0-3 (GOP max, IDR on demand, IDR headroom).** `video/windows.rs:607` and `congestion.rs:184-205`, `:234-249`. The docs never mention GOP size, so add it to "Media and rendering". Rewrite `remote-desktop.md:330-333` (exclusive recovery-IDR hold) and test `replacement_idr_exclusively_holds_credit_until_its_actual_ack` (`congestion.rs:627`). The `video_bench --pause-ms` probe (`video_bench.rs:119-132`) still checks independent recovery and stays valid. The forced-IDR check at 1.5 s (`:134-138`) also stays.

**P0-4 (targets 1080p / ~4 Mbps / ≤60 fps; Quality/Balanced/Smooth).**
- Change `desktop_api.dart:428-447`. SDK defaults (`lib.rs:186-195`) can stay; state that in the docs.
- Docs: `remote-desktop.md:16-19` (milestone and requested 90/12 Mbps), `:297-301`, `:353` ("up to 90 FPS"); `desktop-ui-acceptance.md:9-11` ("at least 90 Hz … 1080p direct-viewing target"); `ui/README.md:10-11` ("sustained 60+ FPS UI acceptance").
- The acceptance procedure must record the selected quality preference.
- `VideoPreferences.validate` keeps `max_fps ≥ 30` (`lib.rs:201`). If Smooth or adaptation needs a ceiling below 30, or adaptation runs fps down to 2 (RustDesk), adaptation must run below the negotiated ceiling, not through `VideoPreferences`.

**P1-1 (frames Rust→Kotlin directly; Dart gets status and geometry).**
- Contract changes: the binary FFI frame bridge (`desktop.rs:1221-1281`), the Dart frame isolate credit (`pb_mapper_service.dart:57-105`), and the Dart-side `presented` path (`desktop_store.dart:562-600`).
- Docs: `remote-desktop.md:303-315`, `:353-360`; `ui/README.md:124-128`; `CLAUDE.md:218-226`. CLAUDE.md is already wrong; add a new JNI or export entry there and in the "When adding a call" guidance (`CLAUDE.md:316-320`).
- **Acceptance tool breakage:**
  - `_AcceptanceApi.presented` and `keyframe` (`desktop_viewer_acceptance.dart:159-196`) only see Dart-originated calls. Once Kotlin or Rust ACK natively, `feedback.acknowledgments` will read 0. Replace it with native or Rust counters exposed through stats.
  - `store.presentFps`, `store.videoPresented` and `store.videoTexture?.stats()` (`:95`, `:110-112`, `:85`) must still exist, or the tool must be rewritten against the new surface.
  - `native_presented_fps` relies on the `presented` key in both platforms' stats (`DesktopVideo.kt:260`, `desktop_video.cpp:184`). Keep that key.
- Windows keeps the Dart path unless it is changed too. Document the per-platform difference.

**P1-2 (delay feedback off the display path).** This is already true for the startup probe (`viewer.rs:225-235`; `host.rs:181-184`). Extend it to periodic probes and update `desktop-input-backpressure.md:86-93` ("A single optional startup status probe").

**P1-3 (input and cursor independent of video; latest-cursor FFI; no echo to the active operator within 300 ms; input not behind `wait_frame`).**
- Docs: `remote-desktop.md:366-370` (cursor over the status channel; same-generation empty-bytes events). Cursor moving out of `SessionStatus` delivery, or adding a suppression rule, changes `host.rs:213-226`, where cursor updates currently go to every viewer. With a single viewer, "no echo" means suppressing echo to that viewer while it moves the pointer.
- Keep, and say so in the docs: "Cursor observation never renews held-input authorization".
- The worker/scheduler threading (`remote-desktop.md:235-238`: backend built, used and dropped on its dedicated worker; no `Send`) constrains moving input injection "to its own thread". COM and desktop binding are thread-affine (`remote-desktop.md:184-191`, `:519-521`: the helper binds the capture/input thread with `SetThreadDesktop`). Splitting input from capture means **two threads bound to the same desktop with consistent generation checks**. This is a correctness and security risk; record the new threading model in the docs.

**P1-4 (zoom/pan local invariant).**
- Already documented at `remote-desktop.md:286-287`, `:363-364`; tested at `desktop_gesture_test.dart:77`.
- Add the explicit invariant list to the docs: zoom, pan, rotation, soft keyboard and window resize never message the host, change encode parameters, or rebuild the decoder or texture.
- Keep the note that Impeller stays off (`AndroidManifest.xml:15-20`); the docs mention only "OpenGL renderer" (`remote-desktop.md:362-364`).
- Note: `_layout` (`desktop_gesture_surface.dart:275-293`) preserves the centre point and scale on viewport change but recomputes the offset. The requirement "keep scale and offset across soft-keyboard show/hide" needs a test.

**P2-1.** No doc change beyond restating that QUIC carries a single reliable stream.

**Regression guards (bounded queues, no input replay, fresh-presented-frame authorisation).** These are documented at `remote-desktop.md:317-322`, `:73-74` and `:255-258`. Keep the sentences and update the numbers.

**Requirement 2 (UI/UX redesign).**
- IA: a rail "Remote desktop" entry, sixth of six, in the order Overview, Tunnels, Remote desktop, Relay, Logs, Settings (`AppRail.dc.html`). Mobile tabs: Overview, Tunnels, Remote, Relay, Settings, with Logs inside Settings (`MobileTabBar.dc.html`). This invalidates `remote-desktop.md:28`, `ui/README.md:3`, `:14-17`, `:52-54`, `ui-design.md:10-11`, `:22-28`, `:197-199`, and `CLAUDE.md:165`, `:167-187`, `:322-351`. Remove the Tunnels `open-desktop` action (`tunnels_page.dart:33-38`). The startup `--desktop` push (`app.dart:124-133`) should select the page instead.
- Host card: permission level chosen at enable (view only / view and control). Today one-click forces `allow_control: true` (`desktop_trust.rs:73-79`). The host card has a "Change to view only" toggle. HostSetup offers "share ordinary desktop only (no helper)" and "keep sharing after closing the window". These invalidate `remote-desktop.md:29-31`, `:36-38`, `:79`. The `ui/README.md:6-7` line "prepares … in one flow" needs rewording.
- Invite QR: the QR string must carry the full invitation, about 1 KB of JSON with a base64 certificate. Any compact `pbd1·…` encoding or added permission field is a format change (§4.1). Keep `decode` accepting the v1 JSON, or bump the version deliberately. The invitation must never be logged, cached or screenshotted; the QR is shown only on demand and only on this screen (brief).
- Encrypted invite storage (Remember this computer, default on): invalidates `remote-desktop.md:67-68`, `:276-277` (reword: not persisted unless Remember is on), and `desktop-input-backpressure.md:58-63`. Document the storage: Windows DPAPI exists for the **host** grant (`desktop_trust.rs:1-3`). A **controller-side** store is new on both Windows and Android (Keystore), and deleting a computer must delete its invitation and thumbnail.
- Online status: this means asking the relay whether `desktop:<name>` is registered. Note the namespace scoping (`ui-design.md:77-85`; `remote-desktop.md:25-27`). A scoped controller credential can see only its namespace.
- Thumbnail: local only, deleted with the computer. Docs must say it is never uploaded.
- Host viewer info (who is watching or controlling, direct or relay, connected for how long): host status has `controlling`, but `path` is computed **viewer-side** (`viewer.rs:170-188`, `:236`). The host needs its own session path and start time. Document the new host status fields.
- Quality overlay (path, network RTT, picture latency, display fps, bitrate, resolution, codec): the product currently never reads native stats (only the acceptance tool calls `.stats()`), and `frame_ack_ms` is ACK RTT including presentation (`lib.rs:156-160`; `remote-desktop.md:372-376`). Define metrics once and reuse them in the overlay, the acceptance report, and the docs (the brief says "same metrics as the perf doc").
- Six viewer states (`ViewerStates.dc.html`: connecting with steps, slow picture plus quality, secure desktop paused, reconnecting with countdown, host identity changed (stop retries), view-only collapsed toolbar) replace the status bar described at `remote-desktop.md:266-269`. "Host identity changed" corresponds to the pin-mismatch boundary (`remote-desktop.md:74-75`).
- Shortcuts and modifier bar: the `Key` enum is closed (§4.3). Adding F1–F12, letters and Fn changes the wire `Key` enum, which old hosts reject (`deny_unknown_fields` on `Input`; any `Key` deserialize error ends the session per `host.rs:180`). Gate this on a host capability.
  - **"Lock remote computer Win+L"** (`Viewer.dc.html`) cannot be synthesised with `SendInput`. RustDesk uses a dedicated `lock_screen()` that calls `LockWorkStation` (`<rustdesk checkout>\src\platform\windows.rs:1266-1271`). It needs a new control-gated host command and a security note in the docs.
  - "Ctrl+Alt+Del: not supported" stays (`remote-desktop.md:50`, `:220-222`).
  - "Refresh picture" maps to `keyframe`; "Release all keys" maps to `release`.
  - Ctrl+C/V on the modifier bar are key chords, not clipboard sync. The support matrix line "clipboard … Not implemented" (`remote-desktop.md:500`) stays true; word it so nobody reads it as clipboard support.
- **Gestures (documented; must change in docs, l10n and release notes).** Current documentation (`remote-desktop.md:44-48`) and implementation:

  | Gesture | PR #51 touch mode (default, `desktop_viewer.dart:27`) | PR #51 trackpad mode | New (RustDesk, `MobileGestures.dc.html`) |
  | --- | --- | --- | --- |
  | One-finger tap | Left click at the point (`desktop_gesture_surface.dart:256-259`) | Left click at the cursor | Left click (trackpad: at the cursor; direct touch: where touched) |
  | Two-finger tap | **Right click** (`:260`) | Right click | Not right click (two fingers navigate the picture) |
  | Long press (`kLongPressTimeout`, 500 ms) | **Left down, then drag** (`:190-202`) | Same | **Right click** |
  | Drag | Hold, then move | Hold, then move | Trackpad: **double-tap and hold, then move**. Direct touch: **one-finger drag drags** ("单指拖动即拖拽") |
  | One-finger move | Moves the pointer, no button (`:234-238`) | Relative cursor move (`:224-233`) | Trackpad: relative move (default mode on mobile). Direct touch: drag |
  | Two-finger pan/pinch | Local pan/zoom 1–8× (`:299-320`) | Same | Same (move = pan, pinch = zoom) |
  | Three-finger vertical | — (scroll only via toolbar buttons, `desktop_viewer.dart:359-380`) | — | **Wheel scroll** |
  | Mouse wheel / Ctrl+wheel | Remote wheel / local zoom (`desktop_gesture_surface.dart:322-341`) (undocumented) | — | Keep; document |
  | Virtual mouse | — | — | Optional in direct touch (left/right/wheel) |

  Default mode changes from Touch to **Trackpad** on mobile. The text to replace is `desktopGestureHelp` (`ui/lib/l10n/app_en.arb:4`, `app_zh.arb:4`). Replace the tests at `desktop_gesture_test.dart:37` and `:125`. **Re-run gesture acceptance**: `remote-desktop.md:460-464` and `desktop-922.md:69-72` record the old gestures.

**Requirement 3 (icons).** `ui-design.md:204` ("Icons are Lucide at its 300 stroke") is invalidated. The new rules are: default weight 400, 300 for 24 px and above, sizes 16/18/20/24, colour follows text, status dots, and the blue rule. The rail icon `tunnels` becomes `route` (today `AppIcons.tunnels = LucideIcons.cable300`, `ui/lib/src/theme/app_icons.dart:10`). Relay online and offline icons become `cloud` and `cloud-off`. `ui-design.md:100-104` mentions the relay's "tower" (`AppIcons.relay = radioTower300`, `app_icons.dart:17`), so check whether the path chip keeps it. The Material icons in the desktop pages (`desktop_page.dart:102`, `:108`, `:136`, `:189`, `:324`; `desktop_viewer.dart:347-407`; `tunnels_page.dart:37`) go away.

**Requirement 4 (caption buttons).** `ui-design.md:193-195` has no sizes. Add: buttons 46×32, radius 6, vertically centred in the 58 px title bar; 16 px right padding when caption buttons are shown; 16 px gap to the primary action; self-drawn 10 px glyphs with a 1 px pixel-snapped stroke; acceptance at 100/125/150% scaling, maximised and restored.

### 7.2 Acceptance-tooling gaps the overhaul must close (merge gate)

`desktop-input-backpressure.md:190-226` requires real-device frame rate, frame intervals and latency, a stall explanation, and a RustDesk comparison. Gaps in the existing tools:

1. **No inter-present interval distribution or stall count.** Native stats keep only counters and mean/max. The tool samples at 1 Hz. Add a bounded histogram or percentiles of presentation intervals (p50/p95/p99/max, count of gaps over 100/250 ms) in Kotlin and C++ stats, plus per-second display fps. Today only `viewer_probe` gives receive-gap percentiles, and they are receive-side (`viewer_probe.rs:240-243`).
2. **No end-to-end or motion-to-photon latency.** `frame_ack_ms` is ACK RTT including presentation. The handoff asks for clock-skew handling or a camera (`desktop-input-backpressure.md:194-196`). The `video_bench --pattern-only` window draws "Unique desktop presentation N" each `DwmFlush` (`video_bench.rs:328-331`), so filming host and phone side by side with that counter gives a camera latency. Document this.
3. **No input-to-display latency on the phone.** The installed-input probe numbers come from an un-checked-in Python/Tk probe (§2.3), which is not reproducible.
4. **No transport RTT in the report.** After P0-2, add the probe RTT and baseline (network RTT), the delay above baseline, current target fps and bitrate, and the quality preference to `SessionStatus` or the report.
5. **Android procedure not written down.** It needs the `flutter build apk --target tool/desktop_viewer_acceptance.dart` command with defines and `adb reverse tcp:18747 tcp:18747`. The phone still needs USB for the loopback bootstrap while the video path uses cellular or Wi-Fi. A device-writable `reportPath` plus `adb pull`/`run-as` is needed. Never install the acceptance entry as the product entry (`desktop-922.md:11-12`).
6. **Host-side trace in service mode.** `--feedback-log` exists only in `deployed.rs:42-52`. For the installed service host, the docs do not say how to enable `pb_mapper_desktop::feedback=debug` (persisted logs default to INFO; `ui/README.md:100-102`). This is unverified for the helper service; document it or add an opt-in.
7. **No RustDesk comparison procedure.** It needs same host, phone, network, resolution, refresh rate and moving content, recording version, path, codec, bitrate, temperature and duration (`desktop-input-backpressure.md:192-196`).
8. **Observer effect.** Calling `stats` enables the timing accumulators (`DesktopVideo.kt:48`, `:255-257`) and makes a MethodChannel round trip each second on the main thread. That is small, but state it.

### 7.3 Every statement the overhaul makes false or stale (file:line)

`remote-desktop.md`:

| Line(s) | Statement (abridged) | Why it becomes false |
| --- | --- | --- |
| 9-14 | Android validated only in the VM; physical phone not established | Physical-phone acceptance is now the gate (already stale vs `desktop-input-backpressure.md:171-173`) |
| 16-19 | Milestone 1080p ~90 presented FPS; UI requests 1080p/90/12 Mbps; Android caps at refresh | P0-4 targets and quality preferences |
| 20-21 | Relay may reduce frame rate **and resolution** | P0-2: resolution fixed under adaptation |
| 28-29 | Open the monitor button on Tunnels | Rail and tab entry |
| 29-31 | Single action prepares helper, service, **input permission**, restoration | Permission level choice; helper optional ("ordinary desktop only") |
| 32-33 | Advanced settings select the display and service name | Host card / More |
| 36-38 | Copy invitation; Advanced revokes input | Copy or QR; host-card "view only" toggle |
| 39-43 | Paste invitation; relay only in Advanced | Add-computer dialog, Remember, More options, QR scan |
| 44-48 | Touch: tap = left, two-finger tap = right, hold to drag; trackpad long-press drag; toolbar exposes buttons, scroll, text, keys | New gestures, floating toolbar, Display/Shortcuts menus, gutters, modifier bar |
| 51-52 | Persistent Stop sharing across navigation | Verify against the host card (`DesktopSharingBanner`, `desktop_page.dart:305-338`) |
| 54-55 | Stop and restart to select another display | Only if display switching is added (the brief mentions "display switch") |
| 67-68 | Controller must retain its invitation; mobile storage not implemented | Remember this computer (Keystore/DPAPI) |
| 79 | Enable starts the host **with input** | View-only option |
| 266-269 | Status bar reports …; "frame age", "frame confirmation time" | Six overlays and the quality overlay; metrics redefined |
| 276-277 | Invitation not persisted by this policy | Reword alongside Remember |
| 281-283 | Desktop transitions retire presentation debt; keyframe requests can't extend ACK timeout | Presentation debt no longer gates sending |
| 297-301 | `VideoPreferences` defaults 90 FPS / 12 Mbps (UI-relevant framing) | SDK default may stay; UI no longer requests it |
| 306-309 | Flutter binary FFI sends H.264 to native decoders | P1-1 (Android direct Rust→Kotlin) |
| 311-315 | Owned buffers through FFI; native consumption produces presentation ACKs | Bridge change; ACKs only for input authorisation |
| 317-320 | SDK queue 8 units / 8 MiB; native 4 / 16 | Revisit under P1-1 (newest-decodable, backlog threshold) |
| 326-327 | `connect_with_presentation_feedback` opts into renderer-driven admission | P0-1 |
| 328-333 | Window 64 KiB → 512 KiB / 8 frames; IDR ≤256 KiB; frame limit = receiver queue; recovery IDR holds the window | P0-1 / P0-3 |
| 333 | Unsent frames older than 250 ms discarded | Re-decide under even pacing |
| 334-336 | ACKs coalesced over 33 ms | Presentation ACKs are only for input auth; receipt ACKs are new |
| 338-345 | ACK delay lowers bitrate **and GPU output size** to 256 kbps and 640×360/30; baseline from predictive flights; admission on earned ACKs | P0-2 |
| 346-349 | 4 s without presentation progress ends the session | New unresponsive-client rule |
| 353 | Android negotiates ≤ refresh (up to 90) | ≤60 and ≤90% of measured decode/present fps |
| 354-356 | Media isolate waits ≤16 ms; native submit returns producer credit | P1-1 / P1-3 |
| 360 | Surface consumption "returns the next presentation credit" | No host credit; the local Surface slot stays |
| 366-370 | Cursor via the status channel / empty-bytes events | P1-3 latest-cursor FFI and echo suppression |
| 372-376 | Diagnostics: `frame_ack_ms` definition | New metric set |
| 410-414 | Runs used the 8-frame window and 33 ms ACK; reference retained in final code | Not retained |
| 437-438 | 1 s averaging, 8-frame bound, exclusive recovery-IDR credit retained | Not retained |
| 460-464 | VM gestures passed: hold-drag, two-finger right click | Old gestures |
| 480-482 | Rerun `presentation_feedback_recovers_…` and `sustained_slow_tls_reader` for the current congestion revision | Tests rewritten |
| 490, 492 | Matrix: touch mapping; "physical phone validation outstanding" | Update with new evidence |
| 539-540 | RustDesk reference `../rustdesk` @ `49516c9` | Overhaul follows `rustdesk-reference` @ `5f015c9` (`video_qos.rs`) |
| 545-550 | Dependency list | Add the QR generation/scan, camera and secure-storage packages that get chosen |
| 562-565 | Lists only `capture_probe`, `video_bench`, `deployed` | Also `viewer_probe`, `transport_probe`, `input_acceptance`, and the acceptance entry |

`docs/validation/desktop-input-backpressure.md`:

- `:3-8`, `:155-161`, `:221-226`: the merge-block status. Update only with real measured evidence and user approval.
- `:58-63`: the invitation is not persisted. False with Remember.
- `:86-93`: a single optional startup probe. Becomes periodic.
- `:106-110` and `:184-186`: the 8-frame / 512 KiB flight limits remain. False after P0-1.

`docs/validation/desktop-922.md:23-29` lists the "Changes retained" in build 922 (8-frame bound, exclusive IDR, 33 ms). Add a superseded note like the one at `:3-6`.

`scripts/test/desktop-ui-acceptance.md`:

- `:9-11`: the 90 Hz / 1080p90 target.
- `:57-60`: steady-state criteria. Add the quality preference, fps target, interval percentiles and stalls.
- `:71-75`: the report includes "transmitted-ACK frame IDs, ACK/keyframe errors". ACK semantics change.
- `:77-82`: the Android procedure is too vague.
- `:104-111`: the historical VM baselines remain; label them as pre-overhaul.

`docs/ui-design.md`:

- `:10-11`: five places. Six on desktop; the mobile tab set differs.
- `:22-28`: the IA table has no Remote desktop row.
- `:182-199`: layout. The viewer hides the rail (full window); the bottom-bar contents change; caption sizes are missing.
- `:204`: Lucide at 300 stroke.
- `:287-301`: the tests list has no desktop tests.
- There is no section describing the remote-desktop page, viewer, overlays, gestures or mobile gutters. A new section is required (brief: "six entries, icon rules, caption sizes").

`ui/README.md`:

- `:3-12`: entry point, "Android … currently uses a VM", the one-flow description.
- `:14-17`: "four pages" is already wrong; it becomes six.
- `:52-54`: the pages list.
- `:124-128`: the binary-buffer and presentation-ACK description.

`CLAUDE.md`:

- `:43-66`: the structure lacks `crates/pb-mapper-desktop` (and the `pb-mapper` facade, `pb-mapper-node`).
- `:69-85`: the DAG lacks the desktop crate. Per `remote-desktop.md:226-229` it sits above `pb-mapper-client` and is optional through `pb-mapper/desktop` (`crates/pb-mapper/Cargo.toml:19-22`, `crates/pb-mapper/src/lib.rs:70`).
- `:99-157`: no module list for the desktop crate.
- `:165`: "the five pages".
- `:167-179`: no desktop pages.
- `:181-187`: navigation has no Remote entry.
- `:189-191`: no `DesktopStore`.
- `:211-216`: no `desktop_api.dart` or `desktop_video.dart`.
- `:218-226`: "every call returns a JSON envelope except three". Already false.
- `:227-250`: no `desktop.rs` or `desktop_trust.rs`.
- `:322-351`: "Five pages", with no Remote desktop entry.

Other files:

- `docs/nat-traversal-validation.zh-CN.md:93-98`: the merge-block cross-reference. Update when resolved.
- `docs/ui-updates.md:84-87`: "next distribution must use at least 920". Already stale (927 consumed); set the next build counter.
- `CHANGELOG.md`: add an Unreleased remote-desktop entry, including the gesture change (right click becomes long press; drag becomes double-tap-hold in trackpad mode and one-finger drag in direct touch; three-finger scroll; trackpad is the mobile default).
- `ui/lib/l10n/app_en.arb:4` and `app_zh.arb:4` (`desktopGestureHelp`): the old gestures. `desktopHelperRestart` (`app_en.arb:661`, `app_zh.arb:559`) is unused (no reference in `lib/`) and describes the removed "open installed app" flow (`windows-restart-919.md:3-7`). Delete it.

### 7.4 Cross-subsystem coupling and risks

- **Wire compatibility.** Four contracts reject unknown input: `Command`, `Input`/`Key` and `Authenticate` (`deny_unknown_fields`), and `Invitation` (`version == 1`). Any of these changes makes a new viewer fail against an old host, or the reverse, by **ending the session**. The safe direction is host→viewer `SessionStatus` additions, which old viewers ignore (no `deny_unknown_fields`). Decide and document a capability-advert scheme once and use it for the receive ACK, the periodic probe, new keys, and lock-screen.
- **The security boundary must survive the credit change.** `Presented` must still be required for input renewal (`worker.rs:409/453/600`). Docs and tests must show that receipt ACKs cannot grant input; keep a test equivalent to `startup_probe_measures_path_without_retiring_presentation_debt` (`congestion.rs:730`) for receipt ACKs.
- **Input on its own thread versus the desktop-bound COM/SendInput thread** (`remote-desktop.md:235-238`, `:519-521`). This risks generation races at secure-desktop transitions, which the docs promise to handle (`:188-191`).
- **Acceptance comparability.** The old reports (`target/issue50-922/*.json`, not in the repo) used other semantics. New runs need new report schema fields; bump `schema` from 1 to 2 in `desktop_viewer_acceptance.dart:132`.
- **The doc-update timing rule.** Do not edit the merge-block statements (`desktop-input-backpressure.md`, `nat-traversal-validation.zh-CN.md`) until real-device evidence exists and the user accepts the fluidity. Unit tests, compilation and connectivity cannot lift the block (`desktop-input-backpressure.md:160-161`).
