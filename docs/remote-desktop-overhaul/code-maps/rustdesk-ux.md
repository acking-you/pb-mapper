# Map: RustDesk reference — UI/UX patterns and video QoS the overhaul borrows

Source: `<rustdesk checkout>` at upstream `5f015c9`. Paths below are
relative to that root unless they start with `pb-mapper:` (=
`<repo>`).
`libs/scrap` is NOT vendored in this checkout (only `libs/hbb_common`), so the
`BR_*` ratio constants and the bitrate formula in section 4.3 come from upstream
`libs/scrap/src/common/codec.rs` and are marked "unverified here".

Flutter framework constants used by RustDesk's recognizers (not RustDesk code):
`kDoubleTapTimeout` = 300 ms, `kDoubleTapMinTime` = 40 ms, `kDoubleTapSlop` =
100 px, `kTouchSlop` = `kDoubleTapTouchSlop` = 18 px, `kLongPressTimeout` = 500 ms.

---

## 1. Overview

RustDesk splits the remote-desktop experience into:

- **Desktop viewer chrome**: one floating toolbar overlaid on the picture
  (`flutter/lib/desktop/widgets/remote_toolbar.dart`), with pin, collapse to a
  handle, auto-collapse, drag-docking, and submenus (Display, Keyboard,
  Control...). The menus' content is shared with mobile through
  `flutter/lib/common/widgets/toolbar.dart`.
- **Mobile viewer**: `flutter/lib/mobile/pages/remote_page.dart` — picture,
  quality monitor, modifier/key bar (`KeyHelpTools`), hidden text field for the
  soft keyboard, painted cursor, and a virtual mouse
  (`mobile/widgets/floating_mouse.dart` in touch mode,
  `mobile/widgets/floating_mouse_widgets.dart` in mouse mode).
- **Touch input**: custom gesture recognizers
  (`common/widgets/gestures.dart`) wired to mouse semantics in
  `common/widgets/remote_input.dart`; cursor/canvas math in
  `models/model.dart` (`CursorModel`, `CanvasModel`); wire calls in
  `models/input_model.dart`.
- **Quality**: client presets/custom fps+quality →
  `OptionMessage` (`src/client.rs`), host-side adaptive fps/bitrate
  (`src/server/video_qos.rs`) fed by a host-initiated `TestDelay` round-trip
  (`src/server/connection.rs`) and by a client-side decode-capacity fps cap
  (`src/client/io_loop.rs::fps_control`). The quality monitor overlay is
  `common/widgets/overlay.dart::QualityMonitor` fed by `update_quality_status`
  events.

Threading in the client: one async `io_loop` task owns the socket and answers
`TestDelay` itself; each display has a std::thread decoder fed by a lock-free
`ArrayQueue` (capacity 120); rendering hand-off on mobile is a single
"latest-only" slot that drops frames when Flutter has not consumed the previous
one. Nothing on the network path waits for decode or presentation.

---

## 2. Components and responsibilities

| Component | File:lines | Responsibility |
| --- | --- | --- |
| `ToolbarState` | `flutter/lib/desktop/widgets/remote_toolbar.dart:245-327` | `pin` (global, persisted), `collapse` and `hide` (per peer, persisted toggles), `initialized` gate |
| `RemoteToolbar` / `_RemoteToolbarState` | `remote_toolbar.dart:442-935` | Layout, docking, auto-collapse debouncer, item list |
| `_DraggableShowHide` (the handle) | `remote_toolbar.dart:3066-3438` | Drag grip, fullscreen, minimize, collapse/expand button, edge snapping |
| `_PinMenu` | `remote_toolbar.dart:937-956` | Pin toggle button |
| `_DisplayMenu` | `remote_toolbar.dart:1476-1820` | View style, scroll style, Image Quality, Codec, Resolution, cursor toggles, display toggles (incl. "Show quality monitor"), privacy |
| `_KeyboardMenu` | `remote_toolbar.dart:2311-2580` | Keyboard mode, input source, view mode, swap keys, relative mouse, reverse wheel, trackpad speed, Android actions |
| `_ControlMenu` → `toolbarControls` | `remote_toolbar.dart:1304-1332`, `common/widgets/toolbar.dart:332-648` | Ctrl+Alt+Del (gated), Insert Lock, send clipboard keystrokes, Refresh, restart, screenshot... |
| `_IconMenuButton` / `_IconSubmenuButton` | `remote_toolbar.dart:2785-2943` | 32 px square button, hover colour, MenuBar/SubmenuButton |
| `toolbarImageQuality` | `common/widgets/toolbar.dart:680-715` | Best / Balanced / Low / Custom radio list |
| `customImageQualityDialog` / `customImageQualityWidget` | `common/widgets/dialog.dart:1834-1906`, `common/widgets/setting_widgets.dart:9-148` | Custom bitrate % and fps sliders |
| `LoginConfigHandler` quality API | `src/client.rs:2237-2341, 2416-2483` | Builds `OptionMessage` for quality/fps |
| `Session` quality API | `src/ui_session_interface.rs:477-508` | Sends option messages; presets force fps 30 |
| `VideoQoS` | `src/server/video_qos.rs:105-595` | Host fps + bitrate ratio adaptation, RTT baseline |
| TestDelay host side | `src/server/connection.rs:443, 632-633, 1082-1103, 2790-2805` | 1 s probe, single outstanding, unresponsive detection |
| TestDelay client side | `src/client.rs:3126-3132`, `src/ui_session_interface.rs:1904-1913`, `src/client/io_loop.rs:2016-2018` | Echo from the network task + update monitor |
| Capture pacing | `src/server/video_service.rs:875-895, 90-117, 149-180`; `connection.rs:982-992` | Wait until each connection *dequeued* the frame, then sleep to `spf` |
| `fps_control` | `src/client/io_loop.rs:1170-1281` | Cap host fps from measured decode capacity; IDR on backlog |
| Decoder thread / `fps_calculate` | `src/client.rs:2855-3005, 3034-3064` | Decode loop, decode-capacity fps |
| Mobile render slot | `src/flutter.rs:1093-1108, 1196-1255` | Latest-only RGBA slot, drop when busy |
| `QualityMonitorModel` / `QualityMonitor` | `flutter/lib/models/model.dart:3541-3614`, `common/widgets/overlay.dart:561-613` | Overlay data and widget |
| Mobile `RemotePage` | `flutter/lib/mobile/pages/remote_page.dart:43-916` | Mobile viewer composition, soft keyboard, bottom bar |
| `KeyHelpTools` | `mobile/pages/remote_page.dart:918-1139` | Modifier bar, Fn row, extra keys |
| Mobile `CursorPaint` | `mobile/pages/remote_page.dart:1164-1213` | Painted remote cursor |
| `GestureHelp` | `flutter/lib/mobile/widgets/gesture_help.dart:39-391` | Mouse/touch mode switch, virtual mouse options, gesture sheet |
| `CustomTouchGestureRecognizer`, `HoldTapMoveGestureRecognizer`, `DoubleFinerTapGestureRecognizer` | `flutter/lib/common/widgets/gestures.dart:13-194, 196-462, 464-658` | 1/2/3-finger classification, double-tap-hold-drag, two-finger tap |
| `RawTouchGestureDetectorRegion` | `flutter/lib/common/widgets/remote_input.dart:61-589` | Maps gestures to mouse actions per mode |
| `CursorModel` | `flutter/lib/models/model.dart:2966-3535` | Local cursor position (remote coords), blocked rects, trackpad pan, touch move |
| `CanvasModel` | `flutter/lib/models/model.dart:2202-2807` | Local zoom/pan, soft-keyboard save/restore, keep cursor centered |
| `VirtualMouseMode` | `flutter/lib/models/model.dart:1834-1898` | Show virtual mouse / joystick / scale (local options) |
| `FloatingMouse` | `flutter/lib/mobile/widgets/floating_mouse.dart:30-1209` | Touch-mode virtual mouse |
| `FloatingMouseWidgets` | `flutter/lib/mobile/widgets/floating_mouse_widgets.dart:34-905` | Mouse-mode wheel, L/R buttons, joystick |
| `InputModel` | `flutter/lib/models/input_model.dart:333-2145` | Sticky modifiers, key/mouse/wheel wire calls |
| Cursor echo exclusion | `src/server/input_service.rs:356-379` | Host does not send cursor position to the connection that moved it within 300 ms |

---

## 3. Data / control flow

### 3.1 Desktop toolbar: pin, collapse, auto-collapse, docking

1. **Creation**: each remote tab gets a fresh `ToolbarState()`
   (`desktop/pages/remote_tab_page.dart:100,473`). Its constructor reads
   `pin` from the *global* local Flutter option `remoteMenubarState`
   (`kOptionRemoteMenubarState`, `consts.dart:154`) as JSON `{"pin": bool}`
   (`remote_toolbar.dart:255-270`). `init(sessionId)` loads `collapse` =
   per-peer toggle `collapse_toolbar` (`kOptionCollapseToolbar`,
   `consts.dart:130`) and `hide` = per-peer toggle `hide-toolbar`
   (`consts.dart:131`) (`remote_toolbar.dart:276-295`). Until `initialized` and
   docking options are loaded the toolbar renders `SizedBox.shrink()` to avoid
   flicker (`:691-694`); `hide == true` also renders nothing (`:696-698`).
2. **Placement**: the toolbar lives in an `Overlay` *over* the picture (the
   picture is not shrunk for it) — `desktop/pages/remote_page.dart:476-482`. It
   is removed entirely while relative-mouse (pointer lock) mode is on
   (`:476-477`). Inside, `Align(alignment: _alignmentForEdge(edge, fraction))`
   (`remote_toolbar.dart:713-721`, `:150-162`), default edge top, fraction 0.5.
3. **Expanded layout** (`_buildToolbar`, `:805-906`): items in order
   `_PinMenu`, [main-toolbar monitor switch], `_MobileActionMenu` (Android
   peers), `_MonitorMenu` (>1 display), `_ControlMenu`, `_DisplayMenu`,
   `_KeyboardMenu` (default conn), `_ChatMenu`, `_VoiceCallMenu`, `_RecordMenu`,
   `_CloseMenu` (`:807-856`), inside a `Material(elevation: 3, radius 4)` with a
   1 px border (`:857-893`). The **handle** (`_buildDraggableCollapse`) is
   stacked on the side facing away from the docked edge (`:894-905`), so the
   expanded toolbar is "toolbar + handle" and the collapsed toolbar is "handle
   only" — same position, same widget.
4. **Handle content** (`_DraggableShowHide.build`, `:3317-3437`): drag grip
   (`Icons.drag_indicator`, `:3291-3295`), [minimized monitor switch when
   collapsed, `:3349-3351`], fullscreen toggle (`:3352-3366`), minimize (only in
   fullscreen, not macOS, `:3367-3380`), collapse/expand button with
   direction-aware chevron (`_toolbarCollapseIcon`, `:192-203`; button
   `:3381-3393`), close (web only, collapsed only, `:3394-3412`). Strip is 20 px
   tall for horizontal edges (`:3430-3434`), radius 5 on the outer corners
   (`_collapseHandleBorderRadius`, `:166-178`). Icon size 20.
5. **Pin**: `_PinMenu` (`:937-956`) shows `assets/pinned.svg`/`unpinned.svg`,
   blue when pinned; `switchPin()` flips and persists globally (`:309-326`).
6. **Manual collapse**: the handle button calls `switchCollapse(sessionId)`
   (`:297-301`), which toggles the persisted per-peer `collapse_toolbar` option
   *and* the in-memory `collapse`. There is **no hover-to-expand**; the user
   clicks the expand chevron.
7. **Auto-collapse**: `_debouncerHide = Debouncer<int>(5000 ms)` (`:650-654`);
   `triggerAutoHide()` bumps its value (`:503`). Triggers: (a) the remote page
   calls the registered callback with `enter=true` when the pointer *enters the
   picture* (`remote_page.dart:538-549` → `remote_toolbar.dart:656-663`), which
   also sets `_isCursorOverImage`; `leaveView` sets it false
   (`remote_page.dart:560-575`); (b) every rebuild of the handle while expanded
   and not dragging (`remote_toolbar.dart:770-772`). When the debounce fires,
   `_debouncerHideProc` collapses only if `!pin && !collapse && cursorOverImage
   && !dragging` (`:674-678`). The auto-collapse sets `collapse.value = true`
   without persisting it. Net effect: unpinned toolbar collapses 5 s after the
   pointer goes back onto the picture and stays there; pinned never collapses.
8. **Drag-docking**: `Draggable` on the grip (`:3282-3315`), axis locked to
   horizontal unless multi-edge docking is enabled (`:3290`). Drag shows a ghost
   preview sized from the measured toolbar (`:702-711`, `:745-765`); nearest-edge
   with 50 px hysteresis (`_switchHysteresisPx`, `:3148-3178`). On drop,
   `_commitPreview` persists per-peer `remote-menubar-edge` and
   `remote-menubar-frac` (`consts.dart:149-150`; `remote_toolbar.dart:3244-3280`),
   bounded by global `remote-menubar-drag-left/right` (default 0/1, `:3124-3144`).
9. **Theme constants** (`_ToolbarTheme`, `:329-390`): button 32×32
   (`buttonSize`, `:341`), H margin 2, V margin 6 (`:342-343`), icon radius 8
   (`:344`), elevation 3 (`:345`), menu radius 5 (Windows)/7, menu padding
   `4,12,4,12` (Windows). Menu items dismiss through `_menuDismissCallback`,
   which also refreshes the remote mouse position (`:3447`).

### 3.2 Image quality selection → protocol

1. **Menu**: Display → "Image Quality" submenu (`remote_toolbar.dart:1744-1762`)
   built by `toolbarImageQuality` (`common/widgets/toolbar.dart:680-715`):
   "Good image quality" = `'best'`, "Balanced" = `'balanced'`, "Optimize
   reaction time" = `'low'`, "Custom" = `'custom'` (`consts.dart:397-407`).
   Selecting calls `bind.sessionSetImageQuality(value)`; "Custom" additionally
   opens `customImageQualityDialog` (`toolbar.dart:705-712`). Mobile uses the
   same list inside `showOptions` (`mobile/pages/remote_page.dart:1273-1323`).
   A user default exists in settings (`desktop/pages/desktop_setting_page.dart:1893-1914`).
2. **Preset path**: `Session::save_image_quality` (`ui_session_interface.rs:486-499`)
   → `LoginConfigHandler::save_image_quality` (`client.rs:2436-2452`) persists
   `config.image_quality` and, for a preset, sends
   `Misc{option: OptionMessage{image_quality: Low=2|Balanced=3|Best=4}}`
   (`get_image_quality_enum`, `client.rs:2327-2341`; proto
   `libs/hbb_common/protos/message.proto:645-650`). **Then, for every non-custom
   preset, it also sends `custom_fps = 30`** unless the client's own auto fps
   is already below 30 (`ui_session_interface.rs:491-498`). So all three presets
   are capped at 30 fps.
3. **Custom path**: the dialog (`dialog.dart:1834-1906`) shows a bitrate
   percentage slider 10..100 (step 5) or 10..2000 with "More" (step 10), and an
   fps slider 5..120 (step 5) (`setting_widgets.dart:47-145`; bounds
   `consts.dart:266-273`: `kMinFps 5`, `kDefaultFps 30`, `kMaxFps 120`,
   `kMinQuality 10`, `kDefaultQuality 50`, `kMaxQuality 100`,
   `kMaxMoreQuality 2000`). Slider changes are debounced 1000 ms
   (`setting_widgets.dart:27-36`). On a public relay (not direct), fps and >100%
   are hidden (`dialog.dart:1845-1849`) and clamped again when building the
   login options (`client.rs:2258-2277`: quality >100 → 50, fps >30 → 30).
   - quality → `save_custom_image_quality(q)` sends
     `OptionMessage.custom_image_quality = q << 8` and persists
     `image_quality="custom"`, `custom_image_quality=[q]` (`client.rs:2416-2429`).
   - fps → `set_custom_fps(fps, true)` sends `OptionMessage.custom_fps = fps`,
     persists option `custom-fps`, and updates `lc.custom_fps` (the ceiling
     `fps_control` uses) (`client.rs:2466-2483`).
4. **Login**: `get_option_message(ignore_default)` repeats the stored choice in
   the login `OptionMessage` (Balanced omitted because it is the host default)
   (`client.rs:2237-2309`).
5. **Host**: `Connection::update_options` (`server/connection.rs:4413-4438`)
   → `VideoQoS::user_image_quality(id, q)` (preset value, or the
   `custom_image_quality` field when `image_quality == NotSet`) and
   `VideoQoS::user_custom_fps(id, fps)` (accepted 1..=120). `user_image_quality`
   maps Low/Balanced/Best to `Quality::{Low,Balanced,Best}` and custom to
   `Quality::Custom(((q >> 8 & 0xFFF) * 2) as f32 / 100, clamped 0.2..40)`, i.e.
   ratio = percent/50, and resets the live ratio to that target immediately
   (`video_qos.rs:218-238`).
6. **Encoder**: the capture loop's `check_qos` reads `spf` and `ratio` every
   frame; if the ratio changed it calls `encoder.set_quality(ratio)` and stores
   `encoder.bitrate()` (kbps) for `TestDelay.target_bitrate`
   (`video_service.rs:1318-1353`). A codec that cannot change quality on the fly
   (VAAPI) restarts the service (`bail!("SWITCH")`, `:1331-1340`).

### 3.3 Host QoS: fps and bitrate ratio (`src/server/video_qos.rs`)

Design comment `:8-28`. Constants `:31-45`:

| Constant | Value | Meaning |
| --- | --- | --- |
| `FPS` | 30 | Default per-user ceiling when no custom fps |
| `MIN_FPS` / `MAX_FPS` | 1 / 120 | Absolute bounds |
| `INIT_FPS` | 15 | New users start here; first 1 s after any connect is capped to 15 (`:527-532`) |
| `BR_MAX` / `BR_MIN` / `BR_MIN_HIGH_RESOLUTION` | 40.0 / 0.2 / 0.1 | Ratio bounds |
| `MAX_BR_MULTIPLE` | 1.0 | Adaptation never exceeds the chosen quality's ratio |
| `HISTORY_DELAY_LEN` | 2 | Window keeps up to 3 samples (pop when `len > 2`, then push, `:58-64`) |
| `ADJUST_RATIO_INTERVAL` | 3 | Ratio adjusted every 3 s |
| `DYNAMIC_SCREEN_THRESHOLD` | 2 | ≥ 2 encodes/s (≥ 6 per 3 s) = "picture is changing" |
| `DELAY_THRESHOLD_150MS` | 150 | Good/bad network boundary |
| `RttCalculator::WINDOW_SAMPLES` / `MIN_SAMPLES` / `ALPHA` | 60 / 10 / 0.5 | Baseline RTT (`:547-551`) |

**Baseline RTT** (`RttCalculator`, `:540-595`): historical min; window min over
the last 60 samples; once 60 samples exist, smoothed = 0.5·min + 0.5·window_min;
before that, the historical min after ≥ 10 samples; `None` before 10 samples.
**Queue delay** `avg_delay()` = mean of the last ≤ 3 samples minus the baseline
(if larger), 150 when no samples (`:67-85`). Each sample is floored at 10 ms
(`:264`, `:268`).

**fps on each TestDelay sample** (`user_network_delay`, `:246-334`): per-mode
`(min_fps, normal_fps)` = Best (8, 16), Balanced (10, 20), Low/Custom-low
(12, 24) chosen from the latest target ratio (`:251-257`) — worse networks get a
*lower* fps floor in quality modes. Then with `d = avg_delay`:

- `d < 50`: `quick_increase_fps_count++`; step +1 while `fps < normal_fps`; every
  3rd consecutive good sample step +5; `fps = max(min_fps, fps + step)` (`:272-280`).
- `50 ≤ d < 100`: +1 only if delay improved and `fps < normal_fps` (`:281-291`).
- `100 ≤ d < 150`: hold (`max(min_fps, fps)`) (`:292-293`).
- `d ≥ 150`: `div = ceil(fps / (d/150))`; 150–200 → `max(min_fps, div)`;
  200–300 → `min(min_fps, div)`; 300–600 → `150·min_fps/d`; ≥ 600 →
  `min(150·min_fps/d, div)` (`:294-306`).
- Every 3 consecutive samples with `d < 150` add +1 more (`:308-317`);
  `d > 50` resets the quick counter (`:320-322`).
- Clamp to `[1, highest_fps]`, where `highest_fps` = min over users of
  `custom_fps` (default 30) and `auto_adjust_fps` (`:381-401`).
- First sample of a user triggers an immediate ratio adjustment (non-Linux)
  (`:326-333`).

**Unresponsive client** (`user_delay_response_elapsed`, `:336-344`): on every
1 s timer tick while a probe is outstanding, if elapsed > 2000 ms the user is
flagged `response_delayed`, the elapsed time is added as a delay sample, and
`adjust_fps` caps the global fps to `MIN_FPS + 1 = 2` (`:521-525`).

**Global fps** (`adjust_fps`, `:511-536`): min over users of their delay fps
(default `INIT_FPS`), 2 if anyone is unresponsive, ≤ 15 for 1 s after a new
connection, clamped to `[1, highest_fps]`. `spf()` = 1/fps (`:134-136`).

**Bitrate ratio** (`update_display_data` every 1 s from `check_qos`,
`:358-378`; `adjust_ratio`, `:416-508`): only when ABR is enabled
(`enable-abr != "N"`) and all displays support quality changes
(`in_vbr_state`, `:178-180`); otherwise ratio = target. Every 3 s, with
`max_delay` = the worst user's `avg_delay`:

| max_delay | Change |
| --- | --- |
| < 50 | ×1.15 if picture changing |
| < 100 | ×1.10 if changing |
| < 150 | ×1.05 if changing |
| < 200 | ×0.95 |
| < 300 | ×0.90 |
| < 500 | ×0.85 |
| ≥ 500 | ×0.80 |

Increases are limited to +150 kbps per step once `ratio ≥ BR_SPEED`
(`:438-443`, `:496-504`). Floor: Best `max(0.2, min(BR_BEST/2.5, ratio_for_1Mbps))`,
Balanced `max(0.1, min(min(BR_BALANCED/2, 0.4), ratio_for_1Mbps))`, Low/Custom 0.1
(`:446-468`). Ceiling = target ratio (`:469`). Resolution is never changed by
QoS.

### 3.4 TestDelay (network RTT probe) — host initiated, client network task echoes

1. Host connection task: `test_delay_timer` = interval 1 s
   (`TEST_DELAY_TIMEOUT`, `connection.rs:443`, `:632-633`). On each tick
   (`:1082-1103`): close if nothing received for 30 s; if **no probe is
   outstanding** (`last_test_delay.is_none()`), record `Instant::now()` and send
   `TestDelay{ last_delay: previous RTT ms, target_bitrate: encoder kbps,
   from_client: false }`; for authorised remote/camera connections, report the
   outstanding probe's elapsed time to `user_delay_response_elapsed` (> 2 s =
   unresponsive).
2. `conn.send` writes the probe directly to the stream from the connection
   task (`connection.rs:5118-5120`) — the same task that writes video frames
   dequeued from `rx_video` (`:982-992`) — so it travels in stream order behind
   bytes already in the socket buffer but never behind the capture loop; it
   measures downstream queueing, not encode/capture time.
3. Client: `io_loop` `select!` reads the socket (`io_loop.rs:242-271`) →
   `handle_msg_from_peer` → `TestDelay` (`:2016-2018`) →
   `Session::handle_test_delay` (`ui_session_interface.rs:1904-1913`): pushes
   `QualityStatus{delay: last_delay, target_bitrate}` to the UI and echoes the
   message unchanged (`client.rs:3126-3132`). This is the same task that owns
   the socket; video frames are only enqueued there (non-blocking), so the echo
   never waits for decoding or presentation.
4. Host receive (`connection.rs:2790-2805`): `from_client == true` probes are
   echoed back (client-initiated variant); otherwise `elapsed` since the
   recorded send → `VIDEO_QOS.user_network_delay(id, elapsed)` and stored as
   `network_delay` (sent as `last_delay` next time). `TestDelay` is a
   housekeeping message allowed regardless of connection type
   (`connection.rs:5467-5476`).

Constants: interval 1 s, single outstanding probe, unresponsive > 2000 ms → 2 fps,
idle close 30 s (`SEC30`, `connection.rs:444`).

### 3.5 Capture pacing on the host (no presentation ACK)

`video_service.rs:875-895`: after encoding and handing the frame to each
subscribed connection, the capture thread waits in 300 ms slices, up to 3 s,
until every connection reports the frame *fetched*
(`try_wait_next`, `:149-180`), then sleeps `spf - elapsed` (`:893-895`).
"Fetched" is signalled by the connection task when it **dequeues** the frame
from its unbounded `rx_video` channel, *before* `stream.send`
(`connection.rs:982-992` → `notify_video_frame_fetched`, `video_service.rs:90-94`).
The client's `Misc.video_received` ack (`connection.rs:3498-3503`) is used only
when the login sets `video_ack_required` (`connection.rs:983`, `:2469`); the
Flutter client does not. Back-pressure therefore comes from the connection task
blocking inside `stream.send` when the TCP socket buffer is full (so it cannot
dequeue the next frame), plus the delay-driven fps — never from presentation.

### 3.6 Client: cap host fps from measured decode capacity (`fps_control`)

1. **Per display**: `new_video_thread` (`io_loop.rs:2425-2468`) creates
   `ArrayQueue<VideoFrame>(VIDEO_QUEUE_SIZE = 120)` (`client.rs:101`), an mpsc
   channel, `decode_fps: Option<usize>`, `frame_count`.
2. **Enqueue** (`io_loop.rs:1323-1354`): key frames go straight through the
   channel (`MediaData::VideoFrame`); other frames `force_push` into the queue
   plus a `MediaData::VideoQueue` token; if the queue was full (oldest evicted),
   request an IDR via `refresh_video(display)` (`:1347-1349`).
3. **Decode thread** (`client.rs:2872-3004`): processes channel items in order
   (tokens pop FIFO from the queue). `fps_calculate` (`:3034-3064`) measures
   **decode capacity, not wall-clock throughput**: it times each successful
   `handle_frame` + render callback (`start` at `:2905`, `elapsed` at `:2941`),
   skips the first 3 frames after a format change, and every 10 frames sets
   `fps = count·1000 / Σms`, resetting the window at 30 frames. Decode errors
   request an IDR (`:2946-2960`).
4. **Once per second** (status timer `io_loop.rs:233-235`, tick `:298-341`):
   real per-display fps = frames delivered to the renderer during the elapsed
   interval (`frame_count`, incremented in the video callback `:2452`), then
   `fps_control(direct, fps)` (`:1173-1281`):
   - `custom_fps` = `lc.custom_fps` or 30 (invalid outside 5..120 → 30)
     (`:1182-1187`); displays with 0 fps for ≥ 15 consecutive seconds are
     ignored (`inactive_threshold`, `:1174-1181`, `:1188`).
   - `limited_fps = min_decode_fps × 9/10` on a direct path, `× 4/5` on relay;
     never above `custom_fps` (`:1205-1212`).
   - Per display trend (`:1215-1238`): **decrease** if
     `(queue_len > 1 && last_auto_fps > limited_fps) || queue_len > max(1, decode_fps/2)`;
     **increase** if `queue_len ≤ 1` for more than 3 consecutive seconds and
     `last_auto_fps + 3 ≤ limited_fps`.
   - New `auto_fps = limited_fps`, halved when decreasing and
     `limited_fps < max_queue_len`; min 1 (`:1242-1250`). If it changed, send
     `Misc{option: OptionMessage{custom_fps: auto_fps}}` and remember
     `last_auto_fps` (`:1251-1262`). On the host this is the same
     `user_custom_fps` ceiling (section 3.2 step 5), i.e. the cap rides the
     user-fps field.
   - **Backlog IDR** (`:1264-1280`): if `queue_len > min(min_decode_fps, 60)`
     request `refresh_video(display)` — at most 20 times per thread, the first
     immediately, later ones only if the previous was > 10 s ago. The queue is
     not flushed; the IDR helps when frames were evicted.
5. **Mobile rendering hand-off** (`flutter.rs:1196-1255`): the decoded RGBA goes
   into a per-display slot; if Flutter has not consumed the previous one
   (`valid == true`) the new frame is **dropped** (`:1206-1214`); otherwise the
   buffers are swapped and an `EventToUI::Rgba(display)` is posted. Dart
   decodes it to a `ui.Image` and calls `nextRgba` to free the slot
   (`models/model.dart:1946-1953`; `flutter.rs:1103-1108`). The decoder never
   waits for Flutter. Note: `frame_count` is incremented before this drop, so the
   monitor's FPS counts decoded frames, not presented ones.

### 3.7 Quality monitor overlay

- Toggle: Display menu → "Show quality monitor" (per-peer toggle
  `show-quality-monitor`, `toolbar.dart:895-904`; stored as
  `config.show_quality_monitor`, `client.rs:2367-2368`);
  `QualityMonitorModel.checkShowQualityMonitor` re-reads it
  (`model.dart:3560-3568`), called on page init (`desktop/pages/remote_page.dart:153`,
  `mobile/pages/remote_page.dart:111`).
- Data: `QualityStatus{speed, fps: HashMap<display,i32>, delay, target_bitrate,
  codec_format, chroma}` (`src/client/helper.rs:8-16`) → Flutter event
  `update_quality_status` with string fields `speed`, `fps` (JSON map),
  `delay`, `target_bitrate`, `codec_format`, `chroma` (`flutter.rs:716-739`) →
  `QualityMonitorModel.updateQualityStatus` keeps the last non-empty value of
  each (`model.dart:3570-3614`; dispatched at `:403-404`).
- Sources and cadence:
  - **Speed**: all bytes read from the socket per second, "x.xxkB/s"
    (`io_loop.rs:255`, `:310-312`), 1 s.
  - **FPS**: frames delivered to the renderer per second for the current
    display, or all displays space-separated (`io_loop.rs:314-320`;
    `model.dart:3575-3595`), 1 s.
  - **Delay**: the host's previous TestDelay RTT in ms (`last_delay`), each
    probe (~1 s); shown as 0 when fps is 0 (`overlay.dart:599-603`).
  - **Target Bitrate**: host `encoder.bitrate()` kbps, each probe.
  - **Codec**, **Chroma** (4:2:0 / 4:4:4): 1 s.
- Widget: `QualityMonitor` (`overlay.dart:561-613`): `maxWidth 200`, padding 8,
  `canvasColor` alpha 150, rows label right-aligned / value left
  (`AutoSizeText`), Delay value green. Placed `Positioned(top: 10, right: 10)` on
  both desktop (`desktop/pages/remote_page.dart:668-675`, wrapped so touches
  still reach the canvas) and mobile (`mobile/pages/remote_page.dart:664-668`).

### 3.8 Mobile remote page composition

`_RemotePageState.build` (`mobile/pages/remote_page.dart:442-535`):
- Immersive mode on entry (`SystemChrome.setEnabledSystemUIMode(manual, overlays: [])`,
  `:103-104`), wakelock (`:108`), restored on dispose (`:170-172`).
- Body: `RawPointerMouseRegion` (physical mouse/keyboard,
  `:537-551`) → `Overlay` → `SafeArea` → `OrientationBuilder` (on rotation, after
  200 ms, reset the actions overlay and `canvasModel.updateViewStyle()` — local
  only, `:509-517`) → `RawTouchGestureDetectorRegion` unless a physical mouse
  is in use (`:520-525`) → `getBodyForMobile` (`:657-717`): `ImagePaint`,
  `QualityMonitor`, `KeyHelpTools`, a zero-size hidden `TextFormField` for the
  soft keyboard, `CursorPaint` (if `showCursorPaint`, `:652-655`), and
  `FloatingMouse` (touch mode) or `FloatingMouseWidgets` (mouse mode)
  (`:706-714`).
- Bottom: `BottomAppBar` (`:553-650`): close, display options (`showOptions`,
  `:1215-1429`: displays, view style, image quality, codec, cursor/display
  toggles), keyboard (`openKeyboard`), gesture help / mode (`Icons.mouse` or
  `touch_app`; Android peers get a mobile-actions button instead), chat, more
  (`toolbarControls` + Android Back/Home/Apps/Volume/Power, `:733-806`), and a
  collapse chevron. A FAB re-shows the bar, hides the keyboard, or closes the
  gesture help (`:455-482`).
- Gesture help replaces the bar (`_bottomWidget`, `:436-440`; `getGestureHelp`,
  `:881-896`): `GestureHelp` toggles mode (persisted local option `touch-mode`
  `Y/N`, `consts.dart:143`) and virtual mouse options.

### 3.9 Gestures (mobile)

**Recognizers** (`gestures.dart`), registered together in `makeGestures`
(`remote_input.dart:530-588`), so they compete in one gesture arena:

- `TapGestureRecognizer` (onTapDown/onTapUp/onTap), `DoubleTapGestureRecognizer`,
  `LongPressGestureRecognizer` (onLongPressDown/Up/LongPress/MoveUpdate).
- `HoldTapMoveGestureRecognizer` (`:196-462`) — double-tap-and-hold-drag:
  first tap up holds the arena (`_registerFirstTap`, `:398-406`) and arms a
  300 ms reset (`:432-434`); a second down within `kDoubleTapSlop` (100 px)
  after `kDoubleTapMinTime` (40 ms) is tracked (`:242-262`) and arms a 300 ms
  timer (`_startSecondTapDownTimer`, `:436-438`). The drag *starts*
  (`_resolve` → `onHoldDragStart`, `:338-349`) when the second finger moves
  beyond 18 px (`:298-315`) or when it is held 300 ms; each move then calls
  `onHoldDragUpdate(delta)`; the second finger's up calls `onHoldDragEnd`
  (`:280-290`). A first-tap move beyond slop rejects (`:300-302`).
- `DoubleFinerTapGestureRecognizer` (`:464-658`) — two-finger tap: both pointers
  up within 300 ms without moving > 18 px (`_timeoutCheck`, `:627-635`).
- `CustomTouchGestureRecognizer` (`:13-194`, a `ScaleGestureRecognizer`):
  classifies by `pointerCount` into `oneFingerPan`, `twoFingerScale`,
  `threeFingerVerticalDrag` (`:46-84`); switching state is debounced 200 ms
  after an end or when moving from 3→2 fingers (`:117-119`, `:125-163`) so a
  lifted finger does not turn a pinch into a pan.

**Mode** (`handleTouch` = desktop or `ffiModel.touchMode`, `remote_input.dart:116`).
`touchMode` defaults to **false = "Mouse mode" (trackpad)** unless the local
option `touch-mode` is `Y`; Android peers are forced to touch mode
(`model.dart:1379-1397`, toggle `:215-220`).

**Mouse (trackpad) mode** (cursor stays where it is; the finger is a trackpad):

| Gesture | Handler (remote_input.dart) | Action |
| --- | --- | --- |
| Tap | `onTap` `:177-195` | `inputModel.tap(left)` at the current cursor (unless the tap-down point is in a blocked rect). Fires only after the double-tap recognizers give up, i.e. ~300 ms after release |
| Double tap | `onDoubleTap` `:210-229` | two left taps |
| Long press (500 ms) | `onLongPress` `:263-285` | `tap(right)` (blocked-rect check) |
| Two-finger tap | `onDoubleFinerTap` `:308-322` | `tap(right)` (mobile mouse mode) |
| Double-tap then hold/move | `onHoldDragStart/Update/End` `:324-352` | left down → `cursorModel.updatePan(delta)` → left up. Skipped while a virtual L/R button is held (`isSpecialHoldDragActive`) |
| One-finger move | `onOneFingerPanStart/Update` `:354-422` | start: re-centre cursor if it is off-screen (`:393-403`); update: `cursorModel.updatePan(delta)` (relative), or `sendMobileRelativeMouseMove` in relative mode |
| Two fingers move / pinch | `onTwoFingerScaleUpdate` `:460-493` | local `canvasModel.updateScale(d.scale/_scale, focal)` + `panX/panY(focalPointDelta)`; with a held virtual button, a pan ×2 instead (`:466-472`) |
| Two-finger end | `onTwoFingerScaleEnd` `:495-514` | `_scale = 1`; safety `sendMouse('up', left)` unless a virtual button holds |
| Three fingers vertical | `onThreeFingerVerticalDragUpdate` `:517-528` | `integral += dy/4`; > 1 → `scroll(+1)`, < −1 → `scroll(−1)`, reset integral (≈ one notch per 4 px). Disabled for Android peers. Wire sign: `+1` = wheel up (physical wheel-down sends `−1`, `input_model.dart:1695-1702`), so finger-down scrolls content like a touch screen |

**Touch mode** (the finger is the pointer):

| Gesture | Handler | Action |
| --- | --- | --- |
| Tap | `onTapUp` `:154-175` | `cursorModel.move(finger)` then left down/up |
| Double tap | `onDoubleTapDown` `:197-208` + `onDoubleTap` | move, then two left taps |
| Long press | `onLongPressDown` `:231-251` + `onLongPress` | move to the press point, `tap(right)` |
| One-finger move | `onOneFingerPanStart/Update/End` `:354-438` | move to tap-down point, left **down**, `updatePan(touchMode)` → absolute `_handleTouchMode` (`model.dart:3312-3374`), left **up** at end (direct drag) |
| Two fingers / three fingers | same as mouse mode | local zoom/pan; scroll |

**Blocked areas**: overlays (virtual mouse, wheel, buttons, key bar in touch
mode) register rects in `CursorModel._blockedRects` and a global
`blockEvents` flag; `shouldBlock(x, y)` gates taps/pans under them
(`model.dart:3006-3050`, `:3117-3137`).

### 3.10 Virtual mouse / joystick

**Visibility**: `VirtualMouseMode` (`model.dart:1834-1898`) local options
`show-virtual-mouse`, `virtual-mouse-scale` (0.8..1.8), `show-virtual-joystick`
(`consts.dart:173-175`); never for Android peers (`:1847`). Toggled in
`GestureHelp` (`gesture_help.dart:124-241`); relative-mouse checkbox only with
the joystick (`:243-274`).

**Touch mode → `FloatingMouse`** (`floating_mouse.dart`): a draggable mouse body
112×138 × scale (`:17-21`) with the remote cursor image drawn at its top-left
(`CursorPaint`, `:1181-1209`). Dragging the body (`_onMoveUpdateDelta`,
`:362-493`) moves the remote pointer to the drawn cursor's global position via
`inputModel.handleMouse(move, pos, moveCanvas: false)`; left/right/middle areas
send `tapDown/tapUp` (`MouseBody`, `:860-1067`). Pressing the wheel up/down
area enters a **scroll circle**: 60 dots, radius 48×scale; rotating the finger
snaps to dots and each step sends `scroll(±1)` (`:511-583`,
`DottedCirclePainter` `:1070-1112`). Near a screen edge (within 2.01 px) the
canvas auto-pans every 30 ms by `5 × canvas.scale × speed` (speed 3.0 while the
drag area is pressed) until the remote display edge is reached
(`_CanvasScrollState`, `:41-190`). Collapses to 30 % size after 7 s idle; tap
to expand (`:213-227`, `:646-670`). Its rect is a blocked rect (`:301-314`).

**Mouse mode → `FloatingMouseWidgets`** (`floating_mouse_widgets.dart`):
- `FloatingWheel` (50×162 at the right edge): up/down repeat `scroll(±1)` every
  100 ms while held, middle = wheel button (`:105-354`).
- `FloatingLeftRightButton` ×2 (55×40, `:356-654`): quick release (< 200 ms) =
  click (down, 50 ms, up); held ≥ 200 ms = button held and
  `isSpecialHoldDragActive = true`, so a second finger's pan becomes cursor
  movement ×2 = drag (`remote_input.dart:466-472`); dragging the button moves
  it; positions persisted per orientation in local Flutter option
  `{l|r}{l|p}-mouse-btn-pos` (`:429-458`).
- `VirtualJoystick` (radius 50, thumb 20, `:683-860`): immediate small move on
  touch, then after 120 ms a 20 ms periodic move of `offset/radius × 3`
  (absolute via `updatePan`) or relative `× 3.0` in relative mode.

### 3.11 Modifier bar (`KeyHelpTools`, `mobile/pages/remote_page.dart:918-1139`)

- **Visible** when the soft keyboard is up, the gesture sheet is open, the bar
  is pinned, or any modifier is latched (`:923`, `:983-987`).
- **Row 1**: `Ctrl`, `Alt`, `Shift`, `Cmd`/`Win` (by peer platform) as toggles
  bound to `inputModel.ctrl/alt/shift/command` (`:994-1007`); `Fn`, pin
  (`Icons.push_pin`), `...` (`:1008-1038`). `Fn` and `...` are mutually
  exclusive; `...` is on by default (`_more = true`, `:933`).
- **Fn row**: F1–F12 → `inputKey('VK_F1'..)` (`:1039-1047`).
- **More row**: Esc, Tab, Home, End, Ins, Del, PgUp, PgDn, [PrtScr, ScrollLock,
  Pause, Menu on Windows/Linux peers], Enter, ←↑↓→, Ctrl+C, Ctrl+V, Ctrl+S
  (Cmd on macOS peers) (`:1048-1118`).
- **Sticky semantics**: the four booleans are latched until tapped again; every
  `inputKey` passes them as flags (`input_model.dart:1029-1041`) and every
  mouse/wheel JSON gets `ctrl/shift/alt/command: 'true'` via `modify()`
  (`:1114-1129`). They reset when the pointer leaves the picture
  (`enterOrLeave(false)` → `resetModifiers()`, `:1152-1155`). `Ctrl+C/V/S`
  buttons use `sendPrompt` (`remote_page.dart:1498-1511`): set ctrl/command
  true, send the key, restore the previous latch.
- Button style: `TextButton`, padding 10×9.75, radius 5, active background
  `MyTheme.accent80`, text 11 px / icon 14 px white, bar `#AA000000`,
  `Wrap` spacing 4 (2 below 320 px width), top padding 24 when the keyboard is
  up (`:941-960`, `:1119-1137`).
- The bar's rect is published to `CursorModel.keyHelpToolsVisibilityChanged`
  500 ms after build (`:962-974`, `:1121-1123`) so the canvas can avoid it.

### 3.12 Soft keyboard handling (mobile)

- A zero-size `TextFormField` exists only while `_showEdit` (`:672-701`),
  `keyboardType: multiline`, `autocorrect: false`, controller pre-filled with
  `'1' * 1024` (`initText`, `:28`) so Backspace always produces a change.
- `openKeyboard` (`:393-413`, Wayland consent gate) → `_openKeyboardUnlocked`
  (`:415-434`): Android `invokeMethod("enable_soft_keyboard", true)`; reset
  text to `initText`; `_showEdit=false`; after 30 ms
  (`kMobileDelaySoftKeyboard`, `consts.dart:431`) `_showEdit=true`; after another
  30 ms show system overlays and focus the field.
- `onChanged` → `handleSoftKeyboardInput` (`:370-379`):
  - non-iOS (`:328-367`): shorter → `VK_BACK`; one new char → `inputKey(char)`
    (`'\n'`→`VK_RETURN`, `' '`→`VK_SPACE`, `:381-391`); several →
    `sessionInputString` (a `KeyEvent.seq`, `ui_session_interface.rs:915-921`);
    auto-paired brackets sent as one string.
  - iOS (`:278-326`): diff after the last `'1'`, respect the IME composing
    range, backspace the removed part, then send the new part.
- Keyboard visibility stream (`flutter_keyboard_visibility`) →
  `onSoftKeyboardChanged` (`:241-276`): hidden → re-enter immersive mode,
  Android disables the soft keyboard (so a physical keyboard is not hijacked,
  `:34-41`), iOS refocus workaround (100 + 50 ms); shown → focus the field
  after 30 ms. Physical keys go through `RawKeyFocusScope` on
  `_physicalFocusNode` (`:545-548`).
- Canvas: `keyHelpToolsVisibilityChanged` (`model.dart:3022-3042`) on show:
  `saveMobileOffsetBeforeSoftKeyboard` (offset + scale), then after 100 ms
  `mobileFocusCanvasCursor` re-centres the canvas on the cursor; on hide,
  `restoreMobileOffsetAfterSoftKeyboard` restores the saved offset and scale
  after 100 ms (`model.dart:2746-2779`). `getSize()` subtracts
  `viewInsets.bottom` and the key bar (`:2294-2329`); `getAdjustY()` shifts
  hit-testing by the key bar (`:2332-2336`). None of this messages the host.

### 3.13 Cursor rendering on mobile and cursor echo

- **Painted cursor** (`CursorPaint`, `mobile/pages/remote_page.dart:1164-1213`):
  draws the remote cursor bitmap (`cursor_data` events → RGBA → `ui.Image`
  cached per id with hotspot, `model.dart:3381-3466`) at `CursorModel.x/y` (remote
  coordinates) transformed by canvas offset/scale; the cursor scales with zoom
  but never below 12 px on its long side (`:1196-1203`); default arrow if no
  bitmap; a "forbidden" cursor when control is not permitted (`:1184-1191`).
  Shown unless the peer is Android, the cursor is embedded in video, or relative
  mode is on (`:652-655`).
- **Local prediction**: `CursorModel._x/_y` is updated immediately by local
  input — `updatePan` (trackpad, `model.dart:3211-3304`) or `move` (touch,
  `:3140-3151`) — and the paint follows at once; `moveMouse` sends the absolute
  position (`input_model.dart:1167-1175`).
- **Camera follows cursor** (trackpad, zoomed in): `updatePan` divides the delta
  by the canvas scale, and once the cursor passes the centre of the visible rect
  toward an edge it pans the canvas by `−dx × scale` instead of letting the
  cursor reach the edge, until the remote display edge is visible
  (`model.dart:3216-3300`). Keyboard-up also re-centres on the cursor
  (`_moveToCenterCursor`, `:2783-2806`).
- **Host echo exclusion**: the host cursor service (`input_service.rs:356-379`)
  sends `CursorPosition` to all connections **except** the one whose input moved
  the cursor within the last 300 ms (`LATEST_PEER_INPUT_CURSOR`, `:369-378`),
  so a remote echo never fights the local prediction. The client applies
  `cursor_position` events directly (`model.dart:363-364`, `:3469-3482`).

### 3.14 Canvas zoom bounds (mobile)

`updateScale(v, focal)` multiplies the scale and keeps the focal point fixed
(`model.dart:2686-2705`); bounds `maxScale = max(1.5, fill)` and
`minScale = fit / 1.5` (`:1986-2002`) — zooming out below fit is allowed.
`panX/panY` just add (`:2660-2683`). Orientation change re-applies the view style
(local). The only host-affecting display action is the explicit "Resolution"
dialog (`mobile/pages/remote_page.dart:1454-1496`).

---

## 4. Contracts and wire / FFI / channel formats

### 4.1 Protobuf (`libs/hbb_common/protos/message.proto`)

- `ImageQuality { NotSet=0; Low=2; Balanced=3; Best=4 }` (`:645-650`).
- `OptionMessage` (`:672-699`): `image_quality=1`, `custom_image_quality=6`
  (int32; value = percent `<< 8`), `custom_fps=11` (int32; user fps *or* the
  client's decode-derived cap), `show_remote_cursor=3`, `disable_keyboard=12`,
  `show_my_cursor=19`, ...
- `TestDelay { int64 time=1; bool from_client=2; uint32 last_delay=3; uint32 target_bitrate=4; }` (`:701-706`).
- `Misc` (`:825-863`): `option=7`, `refresh_video=10` (IDR all),
  `video_received=12` (legacy ack), `auto_adjust_fps=28` (compat only),
  `refresh_video_display=31` (IDR one display).
- `MouseEvent { mask; x; y; repeated ControlKey modifiers }` (`:195-200`);
  `KeyEvent { down; press; oneof control_key|chr|unicode|seq; ... }` (`:296-...`);
  `CursorPosition { x; y }` (`:324-327`).

### 4.2 Flutter ↔ Rust (bridge names)

- `sessionSetImageQuality(value: 'best'|'balanced'|'low'|'custom')`,
  `sessionSetCustomImageQuality(value: int percent)`,
  `sessionSetCustomFps(fps: int)`, `sessionGetImageQuality`,
  `sessionGetCustomImageQuality`, `sessionGetOption(arg: 'custom-fps')`.
- `sessionToggleOption(value: 'show-quality-monitor'|'collapse_toolbar'|'hide-toolbar'|...)`,
  `sessionGetToggleOption(arg: ...)`.
- `sessionInputKey(name: 'VK_*'|single char, down, press, alt, ctrl, shift, command)`
  (`ui_session_interface.rs:894-913`), `sessionInputString(value)`,
  `sessionSendMouse(msg: JSON {type: down|up|wheel|move_relative, buttons, x, y, ctrl?, shift?, alt?, command?})`.
- Event `update_quality_status` fields: `speed`, `fps` (JSON object keyed by
  display index), `delay`, `target_bitrate`, `codec_format`, `chroma`
  (`flutter.rs:716-739`). Event `cursor_position {x, y}`, `cursor_data {id, hotx, hoty, width, height, colors}`.
- Mobile RGBA slot: `EventToUI::Rgba(display)`, `getRgba(display)` / `nextRgba(display)`.

### 4.3 Bitrate (unverified here; upstream `libs/scrap/src/common/codec.rs`)

`BR_BEST = 1.5`, `BR_BALANCED = 0.67`, `BR_SPEED = 0.5`;
`Quality::ratio()` maps Best/Balanced/Low to those and `Custom(r)` to `r`;
encoder bitrate ≈ `base_bitrate(w, h) × ratio` with
`base_bitrate = w·h/1000` kbps (1080p ≈ 2073 kbps → Best ≈ 3.1 Mbps,
Balanced ≈ 1.4 Mbps, Low ≈ 1.0 Mbps; Custom 50 % = ratio 1.0 ≈ 2.1 Mbps).
`BR_MAX = 40.0` is commented as `2000 * 2 / 100` (`video_qos.rs:37`), which
is consistent with `ratio = percent/50`.

### 4.4 Persisted option keys (Flutter)

| Key | Scope | Meaning | Source |
| --- | --- | --- | --- |
| `remoteMenubarState` (`{"pin":bool}`) | global local Flutter | toolbar pin | `consts.dart:154` |
| `collapse_toolbar` | per peer toggle | collapsed | `consts.dart:130` |
| `hide-toolbar` | per peer toggle | hidden | `consts.dart:131` |
| `remote-menubar-edge`, `remote-menubar-frac` | per peer | dock edge, fraction | `consts.dart:149-150` |
| `remote-menubar-drag-left/right` | global | fraction bounds | `consts.dart:147-148` |
| `show-quality-monitor` | per peer toggle | overlay | `toolbar.dart:896` |
| `image_quality`, `custom_image_quality`, `custom-fps` | per peer config / user default | quality | `client.rs:2425-2479`; `setting_widgets.dart:151-152` |
| `touch-mode` | global local | touch vs mouse mode | `consts.dart:143` |
| `show-virtual-mouse`, `virtual-mouse-scale`, `show-virtual-joystick` | global local | virtual mouse | `consts.dart:173-175` |
| `{l|r}{l|p}-mouse-btn-pos` | global local Flutter | virtual button positions | `floating_mouse_widgets.dart:429-433` |

---

## 5. Invariants and security boundaries (as RustDesk implements them)

1. The network read task never waits for decoding or presentation: video is
   enqueued into a bounded lock-free queue; `TestDelay` is echoed inline
   (`io_loop.rs:1323-1354`, `:2016-2018`).
2. The decoder never waits for Flutter on mobile: latest-only slot, drop when
   busy (`flutter.rs:1206-1214`).
3. The host's capture loop is paced by `spf` and by "dequeued for sending",
   not by any viewer acknowledgment (Flutter clients send none)
   (`video_service.rs:875-895`; `connection.rs:983`).
4. Delay feedback is RTT − baseline from host-initiated probes; one probe in
   flight; an unanswered probe > 2 s forces 2 fps (`connection.rs:1088-1101`,
   `video_qos.rs:336-344`, `:521-525`).
5. Automatic adaptation changes only fps and bitrate ratio, never resolution;
   the ratio never exceeds the user's chosen quality (`video_qos.rs:469`).
6. Zoom, pan, rotation, soft keyboard and toolbar never message the host
   (`CanvasModel`, `model.dart:2660-2806`); only "Resolution" does.
7. Cursor: local prediction is authoritative while the user moves; the host
   excludes that connection from cursor broadcasts for 300 ms
   (`input_service.rs:369-378`).
8. Modifier latches reset on leaving the picture (`input_model.dart:1152-1155`).
9. Ctrl+Alt+Del is offered only for Linux peers or when the host reports
   `sas_enabled` (`toolbar.dart:516-526`); view-only or no-keyboard permission
   hides input items (`:517-519`, `:541`).
10. Overlay widgets block touches beneath them via blocked rects so a virtual
    button press is never also a remote click (`model.dart:3006-3050`).
11. Custom quality > 100 % and fps > 30 are refused on public relays (client
    side, `client.rs:2258-2277`).

Security note for pb-mapper: RustDesk authorises input by connection
permissions only; pb-mapper additionally ties input to a fresh *presented*
frame (`pb-mapper: ui/lib/src/pages/desktop_viewer.dart:193-207`). That boundary
is pb-mapper's, not RustDesk's, and must be kept (brief P0-1).

---

## 6. Tests covering it

- RustDesk has **no Flutter tests** in this checkout (`flutter/test` absent) —
  nothing covers the toolbar, gestures, virtual mouse, key bar or quality
  monitor.
- No unit tests for `video_qos.rs`, `fps_control` (`io_loop.rs`) or
  `fps_calculate` (`client.rs`).
- `src/server/connection.rs:6686-7055` (`#[cfg(test)]`) covers per-connection-type
  message gating (e.g. `TestDelay` allowed on port-forward, `:6945`) and option
  scoping (`custom_fps` kept in scoped options, `:7031-7053`).
- pb-mapper's current coverage of the borrowed area:
  `pb-mapper: ui/test/desktop_gesture_test.dart` — taps/two-finger right click
  (`:37`), pinch keeps the texture and maps taps through zoom (`:77`), trackpad
  moves and pinch releases a held drag (`:125`). These encode the PR #51
  gestures (two-finger tap = right, long-press-then-move = drag) and must be
  rewritten for the RustDesk set. `pb-mapper: crates/pb-mapper-desktop/src/congestion.rs:730-745`
  tests the startup probe.

---

## 7. Impact on the planned overhaul

Each item: what to borrow, where in pb-mapper, risks and coupling.

### P0-1 Decouple send credit from presentation

- **Borrow**: RustDesk's host waits only until the *connection task dequeues*
  the frame for writing (`video_service.rs:875-895`, `connection.rs:982-992`),
  then sleeps to `spf`. Flutter clients send no presentation ack.
- **Where**: `pb-mapper: crates/pb-mapper-desktop/src/worker.rs` (stop
  `paused = waiting` pause/resume) and `congestion.rs` (credit). The host network
  writer in `host.rs:191-…` is the place to signal "taken for write".
- **Coupling/risk**: over QUIC a stream write completes as soon as quinn buffers
  it (large send window), so "dequeued" alone back-pressures much later than
  RustDesk's TCP socket buffer. This is why the brief's receipt-based
  cumulative ack (window = fps × RTT + margin) is needed on top; RustDesk has no
  direct equivalent. The receipt ack must be sent by the viewer network task
  (`pb-mapper: viewer.rs:212-291`, where the probe echo already lives), never via
  FFI/Dart. Keep `Command::Presented` (`wire.rs:33-36`) for input authorisation
  only. Regression guard: bounded queues (RustDesk bounds the client queue at
  120 and evicts + IDRs on overflow).

### P0-2 Delay-based adaptation

- **Borrow verbatim**: section 3.3 tables (thresholds 50/100/150/200/300/500/600,
  +1/+5 steps, 3-sample counters, per-mode `(min_fps, normal_fps)`, ratio
  ×1.15…×0.80 every 3 s only when the picture changes, +150 kbps cap, floor
  per mode, ceiling = chosen quality, 2 fps when unresponsive > 2 s, 15 fps for
  the first second, `RttCalculator` 60/10/0.5).
- **Where**: replace the ladder in `pb-mapper: congestion.rs` (the
  resolution-dropping "quality ladder" the brief forbids) with a `VideoQoS`-like
  struct; `scheduler.rs` consumes fps; the encoder (`video/windows.rs`) takes a
  bitrate. pb-mapper already logs `desktop_feedback_interval`; keep the fields.
- **Differences to decide**: RustDesk's default ceiling is 30 fps (`FPS`) and
  presets force 30 (`ui_session_interface.rs:491-498`); the brief wants ≤ 60 on
  phones and probing upward — use the per-mode table but raise `highest_fps` to
  the requested max. RustDesk keeps fps low in quality mode under congestion
  (min 8) — map that to "Quality" in pb-mapper.
- **Risk**: a single-viewer pb-mapper does not need RustDesk's multi-user min;
  keep one user's state but keep "unresponsive" separate from "slow".

### P0-3 IDR policy

- **Borrow**: IDRs only on demand — decode error (`client.rs:2946-2960`), queue
  overflow (`io_loop.rs:1347-1349`), backlog (`:1264-1280`: ≤ 20 per session,
  ≥ 10 s apart after the first), display/codec switch, new subscriber.
- **Where**: viewer already requests `Command::Keyframe`
  (`pb-mapper: wire.rs:32`, `viewer.rs:287-289`, `:305`); add the RustDesk rate
  limit for backlog-driven requests. GOP change is in `video/windows.rs`.
- **Risk**: RustDesk does not rate-limit decode-error refreshes; pb-mapper
  should, to avoid an IDR storm on a broken decoder.

### P0-4 Targets and the Quality / Balanced / Smooth preference

- **Borrow the semantics**: Best = high ratio, low fps floor (8/16);
  Balanced = default (10/20); "Optimize reaction time" = low ratio, high fps
  floor (12/24). Map pb-mapper "Quality" → Best, "Balanced" → Balanced,
  "Smooth" → Low. RustDesk's custom mode (bitrate % + fps) is not in the design;
  skip it.
- **Where**: `pb-mapper: ui/lib/src/ffi/desktop_api.dart:428-447` (today
  `max_fps` = refresh rate clamped 30..90, `bitrate: 12000000`);
  `crates/pb-mapper-desktop/src/lib.rs:180-208` (`VideoPreferences`,
  `validate` requires `max_fps` 30..=120 and bitrate 1–40 Mbps). The preference
  must be changeable mid-session (RustDesk sends `OptionMessage` at any time,
  `client.rs:2436-2452`) — add a `wire::Command` (e.g. quality preset) rather
  than reconnecting; changing it resets the ratio target immediately
  (`video_qos.rs:236`).
- **Risk**: changing the preset must not change resolution (brief).
  Persist per remote computer like RustDesk's per-peer `image_quality`.

### P1-1 Phone pipeline and decode-fps cap

- **Borrow**: (a) network task never awaits decode — RustDesk enqueues and
  returns; (b) decoder never awaits the UI — latest-only slot, drop when busy
  (`flutter.rs:1206-1214`); (c) cap host fps at 90 % (direct) / 80 % (relay) of
  measured decode capacity, ≤ user max, send only on change, once per second,
  with the decrease/increase/halve rules of section 3.6.
- **Where**: `pb-mapper: ui/lib/src/state/desktop_store.dart`
  (`_consumeFrames`/`_acceptFrame` await `texture.submit`),
  `ui/lib/src/ffi/pb_mapper_service.dart` (single-credit frame isolate),
  Android `DesktopVideo.kt` (measure decode time per frame:
  queue-input → output-release), host fps clamp in `scheduler.rs`.
- **Risk**: RustDesk's "decode fps" is capacity from per-frame decode time, not
  presented fps; on Android MediaCodec async decode the per-frame time is not
  meaningful — prefer "frames rendered to the Surface per second while backlog
  > 0" or the codec's output rate. A dynamic cap below 30 must not go through
  `VideoPreferences.validate` (30..=120); add a separate runtime limit
  (1..=120). The cap rides `custom_fps` in RustDesk, mixing user choice and
  auto cap; keep them separate in pb-mapper.

### P1-2 Delay feedback off the display path

- **Borrow**: host-initiated probe every 1 s, one outstanding, echo by the
  viewer's network task, unanswered > 2 s → fps floor 2, RTT baseline via
  `RttCalculator` (section 3.4).
- **Where**: `pb-mapper: host.rs:191-199` sends `roundtrip_probe` only once at
  start; move it into the send loop on a 1 s timer guarded by "none
  outstanding". The viewer echo already happens in the network task
  (`viewer.rs:225-235`, `:306-311`). `congestion.rs:121-140`
  (`start_probe`/`probe_received_at`) becomes the RTT sampler.
- **Risk**: the probe must be written by the host network writer in stream
  order (to measure queueing), not prioritised ahead of queued frames — but must
  not wait for the capture worker. The empty-body status frame used as the probe
  carrier must never refresh picture freshness (current code already guards
  this, `viewer.rs:228-234`).

### P1-3 Input and cursor independent of video

- **Borrow**: host-side cursor-echo exclusion for 300 ms after this
  connection's input (`input_service.rs:369-378`); viewer-side local cursor
  prediction updated synchronously on every local move (`CursorModel`).
- **Where**: host cursor publishing in `pb-mapper: worker.rs` / `host.rs`;
  viewer `desktop_gesture_surface.dart:76-84` currently suppresses remote
  cursor updates for 500 ms after a local move (keep as a second line of
  defence or delete once the host excludes). Brief: non-blocking "latest
  cursor" FFI read polled per rendered frame.
- **Coupling**: the input authorisation (fresh presented frame) stays; the
  cursor must not enter the video queue.

### P1-4 Zoom/pan local; soft keyboard

- **Borrow**: save offset+scale when the keyboard appears, keep the cursor
  visible above it (re-centre), restore exactly on hide (`model.dart:2746-2779`,
  `:3022-3042`); viewport size excludes `viewInsets.bottom` and the modifier bar;
  "camera follows cursor" in trackpad mode when zoomed (`updatePan`,
  `:3216-3300`).
- **Where**: `pb-mapper: ui/lib/src/pages/desktop_gesture_surface.dart`
  (`_layout` `:275-293` recentres on viewport change; `_scaleUpdate` clamps
  1.0..8.0 at `:312`; trackpad move `:224-233` has no follow).
- **Risk**: brief requires "keep scale and offset across keyboard show/hide";
  RustDesk temporarily moves the canvas while the keyboard is up and restores it
  after — compatible if the restore is exact. Never rebuild the `Texture` (the
  surface already keeps `RepaintBoundary(Texture)` as the same child,
  `:378-383`). RustDesk allows zoom out below fit (`fit/1.5`); the design's 16:9
  picture with gutters suggests keeping min = fit.

### Req 2 — Desktop viewer toolbar (pin / collapse / menus)

- **Borrow**: one `ToolbarState` with `pinned` (global, persisted), `collapsed`
  (per remote computer), handle always rendered at the docked position; expanded
  = toolbar + handle, collapsed = handle only; handle holds grip, fullscreen,
  expand/collapse chevron (section 3.1). Toolbar is an overlay over a
  full-window picture; it disappears while the pointer is locked.
- **Change from RustDesk**: the approved design collapses **2 s after the mouse
  leaves the toolbar** (RustDesk: 5 s after the pointer enters the picture,
  `Debouncer(5000)`). Implement with a `MouseRegion` on the toolbar: `onExit`
  starts a 2 s `Timer`, `onEnter` cancels it; do not collapse while a menu is
  open or a drag is in progress (RustDesk checks `_dragging`, `:675`); never
  when pinned. Auto-collapse should not overwrite the persisted manual state
  (RustDesk sets `collapse.value` only).
- **Where**: new widget under `pb-mapper: ui/lib/src/pages/` replacing the
  `AppBar` + `Wrap` + bottom icon row in `desktop_viewer.dart:260-416`; the
  shell must hide the rail for the viewer (Viewer.dc.html). Icons via
  `AppIcons` (Lucide `pin`, `maximize`, `monitor`, `keyboard`, `gauge`) — RustDesk
  uses SVG assets and Material icons, do not copy those.
- **Display menu**: borrow the grouping — view (fit/original), quality preset
  radio, "Show quality monitor" toggle, cursor toggles; drop codec/privacy/
  virtual display (no backend).
- **Shortcuts menu**: RustDesk desktop has no shortcuts menu (it grabs OS keys
  when the pointer is over the picture, `desktop/pages/remote_page.dart:624-630`);
  its Control menu offers Ctrl+Alt+Del (gated), Insert Lock, Refresh,
  "Send clipboard keystrokes" (`toolbar.dart:381-414`, `:516-583`). pb-mapper
  cannot grab Win/Alt+Tab from Flutter, hence a Shortcuts menu; reuse the
  `sendPrompt` pattern (press latched modifiers + key, then release).

### Req 2 — Connection quality overlay

- **Borrow**: a small top-right overlay (`Positioned(top: 10, right: 10)`,
  max width 200, translucent), toggled per remote computer, updated once per
  second, value retained when a field is missing (`model.dart:3570-3614`).
- **Metric mapping**: path ↔ RustDesk's direct/relay flag (shown elsewhere in
  RustDesk); network RTT ↔ `Delay` (`last_delay`); bitrate ↔ `Target Bitrate`
  (host encoder) — pb-mapper should also show received kbps (RustDesk "Speed");
  display fps ↔ RustDesk `FPS` but measure at presentation (RustDesk counts
  decoded frames before the drop-if-busy slot, `io_loop.rs:2452`); picture
  latency has no RustDesk equivalent (use pb-mapper's `receive_to_present`);
  resolution and codec as text.
- **Where**: viewer data from `DesktopStatus` (`pb-mapper: desktop_api.dart:60-131`:
  `receiveFps`, `bitrate`, `frameAckMs`, `path`, `codec`, video size) plus the
  new RTT field; Android `stats`.

### Req 2 — Mobile viewer: trackpad default, gestures, virtual mouse

- **Mode default**: RustDesk "Mouse mode" (trackpad) is the default
  (`model.dart:1389-1396`) — matches the brief. pb-mapper today defaults to
  touch (`desktop_viewer.dart:27`, `_trackpad = false`).
- **Gesture set** (sections 3.9 tables). Changes vs PR #51
  (`desktop_gesture_surface.dart:149-262`): right click becomes long press
  (500 ms) — RustDesk mouse mode *also* keeps two-finger tap as right click
  (`remote_input.dart:308-322`), decide whether to keep it; drag becomes
  double-tap-and-hold/move (300 ms window, 18 px slop, 100 px between taps);
  three-finger vertical scroll is new (≈ one notch per 4 px, finger-down =
  wheel up; pb-mapper `Input::Wheel.vertical` positive = down per
  `desktop_viewer.dart:362-378`, so negate); two-finger pan/pinch stays local.
  Pan-to-pinch transitions need RustDesk's 200 ms state debounce.
- **Latency trade-off**: RustDesk's single tap fires only after the
  double-tap window (~300 ms) because the hold-drag and double-tap recognizers
  hold the arena. pb-mapper's raw `Listener` state machine can instead click on
  the first tap-up and turn a second press within 300 ms into either a second
  click (quick) or a press-drag (held ≥ 300 ms or moved > 18 px). Choose
  explicitly; record it in `docs/remote-desktop.md` and the release notes.
- **Trackpad mapping**: RustDesk moves the cursor by `delta / canvasScale`
  remote pixels (1:1 on screen) and pans the canvas to follow when zoomed
  (`model.dart:3216-3300`); pb-mapper's normalised `delta / (scene × scale)`
  (`desktop_gesture_surface.dart:224-233`) is equivalent in on-screen terms but
  has no follow.
- **Virtual mouse (direct-touch mode)**: borrow `FloatingMouse`
  (section 3.10): drag the mouse body to move the pointer, L/R/middle buttons,
  scroll ring, edge auto-pan, 7 s auto-shrink, blocked rect under it. Mouse-mode
  widgets (joystick, floating L/R buttons) are not in the design.
- **Where**: `pb-mapper: desktop_gesture_surface.dart` (rewrite state machine),
  `ui/test/desktop_gesture_test.dart` (replace the three tests with: tap/long
  press/double-tap-drag/three-finger scroll/pinch-local).
- **Risk**: overlays (modifier bar, side-gutter controls, minimap, virtual
  mouse) must block touches beneath them (RustDesk blocked rects) so a button
  press is never also a remote click; `cancelInput` must release a held drag on
  pinch, focus loss, grant/generation change (existing pb-mapper behaviour,
  keep).

### Req 2 — Modifier key bar above the soft keyboard

- **Borrow**: `KeyHelpTools` layout and behaviour (section 3.11): Ctrl/Alt/
  Shift/Win latches, Fn row F1–F12, "more" row (Esc, Tab, Home, End, Ins, Del,
  PgUp, PgDn, arrows, Enter, Ctrl+C/V), pin, shown while keyboard is up or any
  modifier is latched; publish its rect so the canvas avoids it.
- **Wire difference**: RustDesk sends modifiers as flags on every key/mouse
  event; pb-mapper's `Input` has none (`lib.rs:248-254`). Recommended: keep
  latches viewer-local and wrap each key/click with `Key{mod, down:true}` …
  `Key{mod, down:false}` (RustDesk's `sendPrompt`), so a disconnect or stale
  generation can never leave a modifier held on the host (brief: reconnect
  never replays input). Reset latches on focus loss / leaving the viewer
  (RustDesk `resetModifiers`).
- **Key vocabulary**: `pb-mapper: lib.rs:264-287` `Key` lacks F1–F12, Insert,
  PrintScreen, Pause, ScrollLock, Menu, Space and most letters/digits; extend
  the enum, the Windows mapping (`windows.rs`), and `_keyNames`
  (`desktop_viewer.dart:541-568`). `Input` uses `deny_unknown_fields` and
  serde enums — an older host rejects new keys, so gate new keys on a host
  capability or accept the mismatch within one release.
- **Soft keyboard**: RustDesk's hidden 1024-char field and diff
  (section 3.12) handles IME/backspace; pb-mapper today uses a dialog
  (`desktop_viewer.dart:519-538`) and `Input::Text` (≤ 1024 bytes,
  `lib.rs:383-384`). Multi-char commits map to `Text`, single chars to `Text`
  or keys, deletions to `Key::Backspace`.
- **Ctrl+Alt+Del**: keep showing as not supported (existing boundary);
  RustDesk only offers it with `sas_enabled`.

### Regression guards (RustDesk equivalents to keep in mind)

- Bounded queues for slow consumers: client queue 120 with evict + IDR
  (`io_loop.rs:1347-1349`); host waits ≤ 3 s per frame.
- Unanswered probe > 2 s ⇒ 2 fps, not a stall (`video_qos.rs:521-525`).
- Input authorisation by a fresh presented frame is pb-mapper-only — RustDesk
  offers no pattern to copy; keep `Command::Presented` for it.
