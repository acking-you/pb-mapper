# Remote desktop overhaul — shared brief

Everything an agent on this effort needs before it reads code. The user (project
owner) approved both source documents summarised below; they are the
requirements. Write all user-facing replies/summaries in English unless told
otherwise; code comments follow the surrounding code.

## Where things are

- Repo (git worktree, branch `claude/remote-desktop-overhaul`, based on PR #51
  head `a41eaa7` = `origin/codex/issue-50-remote-desktop`):
  `<repo>`
  PR #51 is one commit behind `origin/master` (`badcad7`, namespaces #52).
- Do NOT touch `<codex worktrees>\issue-50-desktop\pb-mapper`
  (another tool's checkout of the same branch) or `<main checkout>`.
- RustDesk reference source (upstream `5f015c9`): `<rustdesk checkout>`
- Flutter 3.44.9 (project pin): `<flutter 3.44.9>\bin\flutter.bat`
  (not on PATH; call it by full path from PowerShell, cwd `ui/`).
- Android SDK: `<Android SDK>`; JDK 17 on PATH.
- Rust 1.99.1. Dependency submodules under `deps/` are initialised.
- Approved design canvas (HTML artboards, read them as the visual spec):
  `<scratchpad>\design\project\`
  - Desktop (1440x900 unless noted): `Main.dc.html` (remote desktop page: host
    card + remote computer list), `HostSetup.dc.html` (first enable),
    `AddComputer.dc.html` (paste-invite dialog), `Viewer.dc.html` (full-window
    viewer, floating toolbar, Display/Shortcuts menus), `ViewerStates.dc.html`
    (connection state overlays, 1440x1400)
  - Mobile: `MobileHome.dc.html` (Remote tab), `MobileAdd.dc.html` (scan QR /
    paste), `MobileViewer.dc.html` (844x390 landscape viewer, controls in the
    side gutters of the 16:9 picture, minimap top-right),
    `MobileKeyboard.dc.html` (modifier key bar above the soft keyboard),
    `MobileGestures.dc.html` (gesture sheet)
  - Components: `AppRail.dc.html`, `RemoteScreen.dc.html`, `MobileTabBar.dc.html`
  - `Icons.dc.html` (icon system), `TitleBarFix.dc.html` (caption-button fix)
  - `canvas.json` lists artboards; `support.js` is canvas runtime, ignore it.

## Requirement 1 — performance (merge blocker of PR #51)

Symptom: on a physical Android phone the remote desktop stutters ("burst,
stall, burst") even on a direct path. Root-cause hypotheses from reading both
codebases (not yet measured on device):

- P0-1 Decouple send credit from presentation. Today
  `crates/pb-mapper-desktop/src/congestion.rs` admits frames only while
  <= 8 frames (`MAX_FRAMES = media::MAX_QUEUED_FRAMES`) / a byte window are
  outstanding, and ONLY the phone's "presented" ACK returns credit;
  `worker.rs` sets `paused = waiting` and stops capturing while waiting, then
  bursts at fps. RustDesk: capture thread waits only until the connection task
  takes the frame and starts the TCP write; native clients send no
  presentation ACK. Fix: credit returns on RECEIPT — the viewer's network read
  task sends a cumulative receive ack as soon as a frame arrives, without
  going through FFI/Dart/decoder. Window = fps x RTT + margin, not a fixed 8.
  Presentation lag is handled locally by the viewer (keep only the newest
  decodable frame; when the backlog exceeds a threshold, ask the host to lower
  fps or send an IDR, like RustDesk). Capture paces evenly at the target fps,
  never pause/resume as flow control. Congestion is judged only from
  transport delay minus a baseline RTT (extend the startup probe to a periodic
  ~1 s probe), never including phone processing.
  SECURITY BOUNDARY STAYS: input control is still authorised by a fresh
  presented frame. Presentation ACKs may remain for input authorisation; they
  just stop controlling sending.
- P0-2 Even pacing with delay-based adaptation (RustDesk video_qos.rs): queue
  delay < 50 ms -> step fps up (+5 after 3 good samples); 50-150 ms -> only
  slow increase; >= 150 ms -> fps / (delay/150); client unresponsive > 2 s ->
  2 fps. Bitrate ratio adjusted every 3 s (+5-15% up to +150 kbps when delay
  low and picture changing; -5-20% when high). Resolution fixed; the current
  quality ladder that drops resolution down to 640x360@30 when "presentation
  delay" > 250 ms must not fire from phone processing time. Automatic
  adaptation changes fps and bitrate only; resolution changes only on user
  choice, display switch, or long-term low bandwidth, and does not flap within
  a session.
- P0-3 Infinite GOP: `video/windows.rs` ~L598-607 sets
  `CODECAPI_AVEncMPVGOPSize = fps` (an IDR every second). Set GOP to max; IDR
  only on resync, resolution change, new session/subscriber, decoder request.
  `congestion.rs::admit` makes an over-window IDR wait for all in-flight
  frames — reserve headroom for one IDR instead.
- P0-4 Targets: `ui/lib/src/ffi/desktop_api.dart` ~L430-444 requests
  `max_fps` = screen refresh (30-90) and 12 Mbps. Default for phones: 1080p,
  ~4 Mbps, <= 60 fps (not above measured Flutter fps), adaptation probes
  upward. Map to quality preferences: Quality / Balanced / Smooth.
- P1-1 Phone pipeline: `desktop_store.dart` `_consumeFrames`/`_acceptFrame`
  awaits `texture.submit` per frame (stop-and-wait across frame isolate ->
  main isolate -> platform thread -> decoder thread);
  `pb_mapper_service.dart` frame isolate has a single credit. Preferred fix:
  hand compressed frames from Rust straight to the Kotlin decoder (JNI or a
  shared queue), Dart only gets status/geometry. Fallback: allow 2-3 submits
  in flight and move the MethodChannel to `makeBackgroundTaskQueue()`.
  `DesktopVideo.kt` keeps 1 frame on the Surface and skips superseded frames —
  that part is fine. Measure decode/present fps in the viewer and cap host fps
  at 90% of it (RustDesk); on backlog drop + request IDR instead of stalling
  the host.
- P1-2 Delay feedback must not ride the display path (TestDelay-style probe
  answered by the network task).
- P1-3 Input and cursor independent of video: host input injection on its own
  thread (keep authorisation checks, desktop-handle binding, fail-closed
  semantics); cursor status must not enter the video queue — FFI offers a
  non-blocking "latest cursor" read polled by Dart per rendered frame; the
  host does not echo the cursor position to the operator who is currently
  moving it (RustDesk excludes connections that sent input within 300 ms);
  input/feedback FFI calls must not route through the isolate blocked in
  `wait_frame` (up to 16 ms, `ui/native/pb_mapper_ffi/src/desktop.rs` ~L181).
- P1-4 Zoom and pan stay local: already a local `Transform` over
  `RepaintBoundary(Texture)` in `ui/lib/src/pages/desktop_gesture_surface.dart`
  (tested in `ui/test/desktop_gesture_test.dart`). Keep it an invariant: zoom,
  pan, rotation, soft keyboard and window resize never message the host,
  never change encode params, never rebuild decoder/texture. Keep scale and
  offset across soft-keyboard show/hide. Android keeps Impeller disabled
  (`AndroidManifest.xml`) because Impeller's Vulkan path copies the
  SurfaceTexture on every paint.
- P2-1 Transport: QUIC single stream is fine; do not switch to unreliable
  datagrams before P0 is done.
- Regression guards: queues stay bounded for slow consumers; reconnect never
  replays input; input authorisation still requires a fresh presented frame.
- Existing instrumentation: host `RUST_LOG=pb_mapper_desktop::feedback=debug`
  logs `desktop_feedback_interval` (`waiting`, `outstanding_frames`, `action`,
  `bitrate`, `quality`, `delay_ms`, `baseline_ms`); Android video channel
  `stats` (`receive_to_present`, `input_wait`, `decode_callback`). Performance
  handoff notes: `docs/validation/desktop-input-backpressure.md`.

## Requirement 2 — remote desktop UI/UX redesign (approved)

Desktop keeps management on one page and the viewer fills the window; mobile
puts the picture first with controls in the side gutters. Interaction follows
RustDesk, visuals follow POCOX (`ui/lib/src/theme/`, `docs/ui-design.md`).

| Area | Design | Artboards |
| --- | --- | --- |
| Information architecture | Desktop rail gains "Remote desktop" (6 entries); mobile bottom bar gains "Remote", Logs moves into Settings | Main, MobileHome |
| Host sharing | One "Enable remote control" button + 3-step explanation; two permission levels (view only / view and control); invite can be copied or shown as QR; helper service, service name, revoke live under "More" | HostSetup, Main |
| Remote computer list | Remembered computers with online status and last path; the one being viewed shows "Back to picture" | Main, MobileHome |
| Add computer | Desktop pastes an invite and shows the recognised host; mobile scans QR first, paste second; "Remember this computer" on by default | AddComputer, MobileAdd |
| Desktop viewer | Hide the rail; floating toolbar that can be pinned or collapsed into a top handle (unpinned: collapses 2 s after the mouse leaves); "Display" and "Shortcuts" menus; connection state overlaid on the picture (six states) | Viewer, ViewerStates |
| Mobile viewer | Landscape: controls in the gutters beside the 16:9 picture; trackpad mode by default; modifier key bar above the keyboard (sticky Ctrl/Alt/Shift/Win, Fn row, Ctrl+C/V); minimap top-right | MobileViewer, MobileKeyboard |
| Gestures | Same as RustDesk: tap = left click, long press = right click, double-tap-and-hold = drag, three-finger vertical = scroll, two fingers move/pinch = navigate picture; direct-touch mode can show a virtual mouse | MobileGestures |

Gesture changes vs PR #51: right click moves from "two-finger tap" to "long
press"; drag moves from "long press then move" to "double-tap and hold then
move" — must be in release notes / docs. "Ctrl+Alt+Del" keeps showing as not
supported (existing security boundary).

New capabilities the design needs (absent in PR #51):

| Capability | Where | Notes |
| --- | --- | --- |
| Invite QR code | host card | shown only after tapping "QR code", hidden when leaving the page; only on this screen |
| Camera QR scan | mobile add computer | camera permission + scanning library; frames never stored or uploaded |
| Encrypted invite storage on mobile | remote list | only Windows DPAPI today; Android uses Keystore, controlled by "Remember this computer" |
| Remote computer online status | remote list | ask the relay whether that service is registered |
| Last-picture thumbnail | mobile remote list | capture one frame on disconnect, local only, deleted with the computer |
| Host viewer info | host card | whether someone is watching / controlling, direct or relay, connected for how long |
| Connection quality overlay | both viewers | path, network RTT, picture latency, display fps, bitrate, resolution, codec; same metrics as the perf doc |
| Quality preference | desktop Display menu | Quality / Balanced / Smooth, mapping to P0-2/P0-4 bitrate+fps targets |
| Toolbar pin / collapse | desktop viewer | unpinned collapses to a top handle 2 s after the mouse leaves |
| Shortcut menu + modifier bar | both viewers | today only some keys are mapped (`desktop_viewer.dart` `_keyNames`); add F1-F12, Home/End, Win combos, lockable modifiers |
| New gestures + virtual mouse | mobile viewer | long-press right click, double-tap-hold drag, three-finger scroll; optional virtual mouse in direct-touch mode |

## Requirement 3 — icons (approved)

- Lucide only, through `ui/lib/src/theme/app_icons.dart` (package
  `lucide_icons_flutter` 3.1.25, weights 100-600). Replace every Material
  `Icons.*` (caption buttons `remove`/`crop_square`/`filter_none`/`close`;
  PR #51 desktop pages `screen_share_outlined`, `keyboard_outlined`,
  `ads_click`, `mouse_outlined`, `pan_tool_outlined`,
  `desktop_windows_outlined`, ...).
- Weight follows size: <= 20 px uses 400, >= 24 px uses 300. AppIcons default
  becomes 400; add separate 300 constants for the few >= 24 px uses (empty
  states, large tiles).
- Sizes: 16 labels/inline, 18 buttons/menus, 20 rail/bottom bar, 24 empty
  states/tiles. Colour follows text; status uses coloured dots; blue only for
  "this computer" and actively transferring tunnels.
- Swaps: tunnels `cable` -> `route`; relay online `wifi` -> `cloud`; relay
  offline `wifi-off` -> `cloud-off`; new rail/bottom-bar entry `monitor`.
- 19 new remote-desktop icons: screen-share, screen-share-off,
  monitor-smartphone, eye, mouse-pointer-2, keyboard, command, mouse,
  touchpad, pointer, qr-code, scan-qr-code, maximize, pin, power,
  shield-check, lock-keyhole, gauge, smartphone.
- Window caption glyphs are self-drawn (10 px, 1 px stroke), not icons.
- Launcher and tray icons are out of scope.

## Requirement 4 — title-bar caption-button crowding fix (approved)

On Windows the close button crowds the content sheet's rounded corner: 8 px
from the sheet edge, the sheet 8 px from the window edge, 44x36 buttons with
radius 10 and Material 15 px icons.

| Item | Now | Fix | Code |
| --- | --- | --- | --- |
| Right padding after caption buttons | 8 px | 16 px (when caption buttons are shown) | `ui/lib/src/shell/page_frame.dart` `_TitleBar` |
| Gap to the primary action | 10 px | 16 px | same |
| Button size / radius | 44x36, radius 10 | 46x32, radius 6, vertically centred in the 58 px title bar | `ui/lib/src/shell/window_chrome.dart` `_CaptionButton` |
| Glyph | Material Icons 15 px | self-drawn 10 px, 1 px stroke, pixel-snapped | `window_chrome.dart` `CaptionButtons` |

Acceptance: at 100/125/150% scaling, maximised and restored, the hovered close
button's red background stays >= 16 px from the sheet edge.

## Docs to update at the end

`docs/ui-design.md` (six entries, icon rules, caption sizes),
`docs/remote-desktop.md` (gestures, performance model), `CLAUDE.md` page list
if pages change, release notes for the gesture change.

## Engineering rules that matter here (from the user's CLAUDE.md)

- Understand the whole flow before editing; fix the structure, delete what the
  change makes obsolete (code and tests). A small diff is not a goal.
- Tests minimal and deliberate: core capabilities, real failure/state/security
  boundaries. No wording tests, no repetitive matrices.
- `unwrap_used`/`expect_used` are denied in production Rust.
- Verify with the narrowest relevant checks.
