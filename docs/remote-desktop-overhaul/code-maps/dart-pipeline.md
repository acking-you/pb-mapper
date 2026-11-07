# Map: Dart desktop state and FFI client (`DesktopStore` and the video/input pipeline)

Repo root for every path below:
`<repo>` (branch
`claude/remote-desktop-overhaul`, head `a41eaa7`). Line numbers are for that head.

Files in scope (all read in full):

| File | Lines | Role |
| --- | --- | --- |
| `ui/lib/src/state/desktop_store.dart` | 1142 | `DesktopStore` (ChangeNotifier): session, host, recovery, media, input |
| `ui/lib/src/ffi/desktop_api.dart` | 492 | Models, `DesktopApiClient` interface, `DesktopApi` (JSON ops) |
| `ui/lib/src/ffi/pb_mapper_service.dart` | 813 | Frame worker isolate, `DesktopFrameStream`, `desktopCall` routing, `compute` calls |
| `ui/lib/src/ffi/desktop_video.dart` | 118 | `DesktopVideoTexture` / `DesktopVideoEvent` over MethodChannel `pb_mapper/desktop_video` |
| `ui/lib/src/ffi/pb_mapper_ffi.dart` | 442 | Desktop symbol lookups (L200-226) |
| `ui/lib/src/state/app_store.dart` | 516 | Lazy `DesktopStore` ownership (L38-42, L192, L510) |
| `ui/test/fake_desktop_api.dart` | 209 | `FakeDesktopApi` |
| `ui/test/desktop_test.dart`, `desktop_lifecycle_test.dart`, `desktop_recovery_test.dart`, `desktop_host_readiness_test.dart`, `desktop_viewer_navigation_test.dart`, `desktop_gesture_test.dart` | | Tests |
| `ui/tool/desktop_viewer_acceptance.dart` | 226 | On-device acceptance harness |

Read for contracts only (owned by other maps): `ui/native/pb_mapper_ffi/src/desktop.rs`,
`crates/pb-mapper-desktop/src/{lib.rs,viewer.rs,media.rs,worker.rs}`,
`ui/android/app/src/main/kotlin/com/pb_mapper/ui/DesktopVideo.kt`,
`ui/windows/runner/desktop_video.cpp`, `ui/lib/src/pages/desktop_*.dart`.

---

## 1. Overview

The Flutter side of remote desktop is one long-lived `DesktopStore`
(`desktop_store.dart:20`) owned by `AppStore` (`app_store.dart:40-42`, created
lazily, `restoreOnStartup()` called at app start `app_store.dart:192`, disposed
`app_store.dart:510`). It owns, independently of navigation:

- **Host role** (this computer shared): capabilities, remembered trust, background
  (Windows service) state, invitation, start/stop/revoke/permission, durable
  resume after helper restarts.
- **Viewer role** (one outgoing session): target `(invitation, relayOnly)`,
  connect/reconnect with exponential backoff, media epoch, the compressed-frame
  pipeline into a native H.264 texture (Android/Windows) or JPEG bytes (other
  platforms), presentation feedback to the host, keyframe requests, control
  grant, and the serialized input queue.

Media crosses five execution contexts per frame, strictly one frame at a time:

```
Rust tokio network task ─push─▶ media::Queue (≤8 frames / 8 MiB, status coalesced)
        │ (pb_mapper_desktop_wait_frame, blocking ≤16 ms, runtime.block_on)
        ▼
Dart frame-worker isolate  (_desktopFramesWorker, ONE credit)
        │ SendPort (copy)                        ▲ credit `true` only after
        ▼                                        │ _acceptFrame() completes
Dart main isolate: DesktopFrameStream → .map → StreamIterator → _consumeFrames
        │ await _acceptFrame → await texture.submit (MethodChannel)
        ▼
Android platform main thread → HandlerThread "pb-desktop-video" → MediaCodec
   (reply only when MediaCodec accepted the input; decoder admits ≤4 outputs,
    outputs released one per SurfaceTexture consumption by Flutter raster)
        │ onFrameConsumed → channel.invokeMethod('event', presented)
        ▼
Dart main: _presentVideo → _flushFeedback (≥33 ms apart) → `presented` op
        │ via the SAME frame-worker isolate mailbox (blocked in wait_frame)
        ▼
Rust Viewer::presented → watch → network send task → host credit + input lease
```

This is the stop-and-wait chain named in brief P1-1, and the
presentation-controlled credit named in P0-1. Input, control, release, keyframe
and presented calls all share the frame-worker isolate (P1-3).

---

## 2. Components and responsibilities

### 2.1 `DesktopStore` (`ui/lib/src/state/desktop_store.dart`)

`class DesktopStore extends ChangeNotifier` (L20). Constructor
`DesktopStore(this.api, {DateTime Function()? now, this.onRestartReady})`
(L21-23); `now` is an injectable clock used by every freshness/backoff decision
(timers still use real time).

State fields (L24-89), grouped:

| Group | Fields |
| --- | --- |
| Host | `capabilities` (L28), `trust` (L29), `_trustRestored` (L30), `_hostResumeTimer`, `_hostResumeSerial`, `_hostResumeAttempt` (L31-32), `invitation` (L63), `restarting` (L27) |
| Snapshot | `snapshot: DesktopSnapshot` (L33) = {host, viewer, background} |
| Media | `frame` (L34, last accepted `DesktopFrame` incl. bytes), `videoTexture` (L35), `videoPresentation` ValueNotifier (L36, getter/setter `videoPresented` L38-39), `cursorStatus` ValueNotifier (L37), `_frames` StreamIterator (L40), `_videoEvents` (L41), `_videoStatuses: Map<int frameId,(DesktopStatus, DateTime receivedAt)>` (L42, capped 256 at L541-543), `_videoGeneration` (L43), `_mediaEpoch` + `mediaEpoch` getter + `_current(epoch)` (L44-46), `_frameAt` (L71), `_videoPresentedAt` (L60), `_videoError` (L65), `_mediaFailed` (L51) |
| Feedback | `_feedback` future (L48), `_presentationTimer` (L49), `_pendingPresentation` (L50), `_pendingKeyframe` (L51), `_lastAck` (L61), `_lastMediaNotify` (L62) |
| Rates | `_presentSample`, `_presentCount`, `_presentFps`, `presentFps` getter (L52-59) |
| Input | `_sequence` (L70), `_inFlight` (L72), `_inputEpoch` (L73), `_inputTail` (L74), `_pendingMove: _PendingMove?` (L75), `_endingControl` (L76), `_releasing` (L77) |
| Session | `_target: (String invite, bool relayOnly)?` (L78), `_sessionSerial` (L79), `_attemptSerial` + `_attemptRunning` (L80-81), `_controlWanted`, `_viewerForeground` (L82), `_restoringControl` (L83), `_retryTimer` (L84), `_retryAt`, `_recoveryStarted`, `_lastKeyframeRequest`, `_stableSince` (L85), `_controlRequestedAt` (L86), `reconnectAttempt` (L87), `recoveryReason` (L88), `_identityMismatch` (L89) |
| Misc | `error` (L64, public mutable), `busy` (L66, public mutable), `_disposed` (L67), `_polling` (L68), `_timer` 250 ms poll (L69), `_disconnecting` (L47) |

Derived getters:

- `pictureAge` (L90-93): `now - (_videoPresentedAt ?? _frameAt)`. Note
  `_videoPresentedAt` is the **receipt** time of the presented frame
  (`received.$2`, L584), so decode/present latency is excluded.
- `retryIn` (L95-99): remaining time to `_retryAt`, floored at zero.
- `connectionPhase` (L100-120), first match wins:
  `identityMismatch` (if `_identityMismatch`) → `retryWaiting` (`_retryTimer != null`)
  → `connecting` / `reconnecting` (`_attemptRunning`, by `reconnectAttempt == 0`)
  → `waitingForDesktop` (`snapshot.viewer.state == 'capture_unavailable'`)
  → `idle` (`!connected`) → if `fresh`: `delayed` when
  `snapshot.viewer.frameAckMs > 500`, else `live`
  → `recoveringVideo` if `_recoveryStarted != null`, else `connecting`.
  Enum `DesktopConnectionPhase` has 9 values (L6-16).
- `sharing` (L122-123): `snapshot.host != null || snapshot.background?.enabled == true`.
- `connected` (L124): `_target != null || snapshot.viewer != null`.
- `fresh` (L125-135): `frame` non-null with non-empty bytes, `_frameAt` < 2 s ago,
  and for `codec == 'h264'` additionally `videoPresented.generation ==
  frame.generation` and `_videoPresentedAt` < 2 s ago; and
  `snapshot.viewer.streaming`. JPEG freshness does not require presentation.
- `controlling` (L136-140): `fresh && snapshot.viewer.controlling && !_endingControl && !_releasing`.

`_PendingMove` (L1136-1142): mutable `rendered`, `event`, final `epoch`,
`late Future<void> done`.

### 2.2 `DesktopApiClient` / `DesktopApi` (`ui/lib/src/ffi/desktop_api.dart`)

Interface `abstract interface class DesktopApiClient` (L240-277). Members:
`enableHost(service, displayId)`, `backgroundAction(action)`, `trustStatus()`,
`restoreHost()`, `revokeTrust()`, `frames()` (Stream<DesktopFrame>),
`keyframe()`, `helperAction(action)` (true = restart handoff ready),
`presented(generation, frameId)`, `capabilities()`, `status()`,
`startHost({service, displayId, allowControl, relayOnly, privileged, remember, autoStart, background})`,
`stopHost({onExit})`, `allowControl(allow)`, `connect(invitation, {relayOnly})`,
`disconnect()`, `control(enabled)`, `release()`,
`input({generation, frameId, sequence, input})`, `frame(after, generation)`.

`DesktopApi implements DesktopApiClient` (L279-492). Every JSON op goes through
`_call(op, args)` (L346-358) → `PbMapperService().desktopCall({'op': op, ...args})`;
`success != true` throws `DesktopCallException(code ?? 'INTERNAL', message ?? 'Desktop operation failed')`
(L6-11, `toString()` is the message only). `frames()` (L325-331) maps
`PbMapperService().desktopFrames()`; `frame()` (L482-491) maps
`PbMapperService().desktopFrame(after, generation)`. `disconnect()` (L449-453)
first calls `PbMapperService().stopDesktopFrames()` synchronously, then the
`disconnect` op.

**What `connect` requests** (L427-447):

```dart
'invitation': invitation,
'relay_only': relayOnly,
'native_video': Platform.isWindows || Platform.isAndroid,
'video': {
  'max_width': 1920, 'max_height': 1080,
  'max_fps': fps,          // Android: display refreshRate.round().clamp(30, 90); else 90
  'bitrate': 12000000,
}
```

`refreshRate` is `ui.PlatformDispatcher.instance.views.first.display.refreshRate`
(60.0 if no view, L429-430). Rust `VideoPreferences::default()` is the same
1920×1080/90/12 Mbps (`crates/pb-mapper-desktop/src/lib.rs:186-195`) and
`validate()` bounds are width 2..=3840, height 2..=2160, fps 30..=120, bitrate
1 000 000..=40 000 000 (`lib.rs:198-207`). With `native_video: true` Rust offers
`[H264, Jpeg]` with presentation feedback (`desktop.rs:748-756`); otherwise
JPEG-only without feedback (`desktop.rs:757-765`).

Models (all `const`, no `==` overrides):
`DesktopDisplay(id, name, width, height)` (L13-25; Rust also sends `x`, `y`,
dropped here), `DesktopCapabilities{host, displays, message, helper}` (L27-38),
`DesktopHelper{installed, running, authorized, identityVerified, message}` (L40-57),
`DesktopStatus` (L59-132; `streaming` getter L131), `DesktopCursor{x, y, visible}`
(L134-147), `DesktopSnapshot{host, viewer, background}` + `withViewer/withHost/withBackground`
(L149-159), `DesktopBackground{configured, enabled, service, server, invitation, autoStart, message}`
(L161-184), `DesktopTrustedHost` (L186-211), `DesktopHostEnable{restart, invitation, background}`
(L213-222), `DesktopTrust{supported, saved, matchesRelay}` (L224-232),
`DesktopFrame(status, bytes)` (L234-238).

### 2.3 `PbMapperService` desktop parts (`ui/lib/src/ffi/pb_mapper_service.dart`)

- `_desktopFrameIsolate((handleAddress, after, generation), {wait})` (L27-53):
  calls `pbMapperDesktopWaitFrame` (wait) or `pbMapperDesktopFrame`; null pointer
  → null; validates `len >= 4` and metadata length; returns
  `{'status': jsonDecode(utf8 metadata), 'bytes': Uint8List.fromList(payload)}`
  (copy at L48); always frees with `pbMapperDesktopFreeFrame` (L51).
- `_desktopFramesWorker((handleAddress, SendPort))` (L57-114): the frame isolate.
  See §3.2 and §4.4.
- `DesktopFrameStream` (`@visibleForTesting`, L118-246): owns one worker until its
  exit acknowledgement. `StreamController` with `onListen: _start`,
  `onPause: _paused = true`, `onResume: scheduleMicrotask(_credit)`,
  `onCancel: stop(); return _closed.future` (L120-134).
  `call(request)` (L153-165) multiplexes session-bound JSON calls over the worker
  mailbox: error if stopped/finished, error if `_calls.length >= 16`
  ("Desktop worker request limit reached"), queued in `_calls` until the
  worker's `SendPort` arrives (replayed at L196-198). `stop()` (L167-171) sets
  `_stopped`, fails pending calls, sends `false`. `_credit()` (L180-184) sends
  `true` only when not stopped, not paused, not `_outstanding`, and the port is
  known. `_start()` (L186-232) opens `_messages`/`_exited`/`_errors` ports and
  spawns via the injected `spawn`. `_finish()` (L234-245) runs once on isolate
  exit (or spawn failure): closes ports, `onClosed()`, completes `_closed`,
  closes the controller.
- `PbMapperService.stopDesktopFrames()` (L251-256): `_desktopFramesEpoch++` and
  `stop()` every live worker.
- `PbMapperService.desktopFrames()` (L258-286): creates a `DesktopFrameStream`
  whose `spawn` pins the native handle (`_activeCalls++`, `pinned = true`),
  registers the reader in `_desktopFrameWorkers`, stops it immediately if a
  `stopDesktopFrames()` happened since creation (epoch check L268), and
  `Isolate.spawn(_desktopFramesWorker, (handle.address, messages), onExit, onError, errorsAreFatal: true)`.
  `onClosed` unregisters and unpins (`_activeCalls--; _destroyIfIdle()`).
- `PbMapperService.desktopCall(request)` (L288-301): ops in
  `{'presented','keyframe','input','control','release'}` go to the first worker
  with `acceptsCalls` (the frame isolate); everything else, and these ops when no
  worker is live, go to `_runJsonOnWorker('desktop', {'request': jsonEncode(request)})`.
- `PbMapperService.desktopFrame(after, generation)` (L303-316): one-shot
  `compute(_desktopFrameIsolate, ...)` (non-blocking pull).
- `_runJsonOnWorker` (L470-486): `compute(_callJsonIsolate, ...)` = a **new
  isolate per call** (`Isolate.run`), pinning the handle. `_callJsonIsolate`
  case `'desktop'` (L696-699) calls `pbMapperDesktopCall`; result decoded and
  freed by `_decodeJsonStatic` (L637-657).
- `dispose()` (L443-450) calls `stopDesktopFrames()`; the handle is destroyed only
  when `_activeCalls == 0` (`_destroyIfIdle` L454-462), so a live frame worker
  keeps the handle alive until it exits.

### 2.4 `pb_mapper_ffi.dart` desktop parts

`final class DesktopBuffer extends Struct { Pointer<Uint8> data; @UintPtr() int len; }` (L200-204).
Lookups on `PbMapperFFI` (L207-226):

| Dart member | Symbol | Native signature |
| --- | --- | --- |
| `pbMapperDesktopCall` | `pb_mapper_desktop_call_json` | `Pointer<Utf8>(Pointer<Void> handle, Pointer<Utf8> request)` |
| `pbMapperDesktopFrame` | `pb_mapper_desktop_frame` | `Pointer<DesktopBuffer>(Pointer<Void>, Uint64 after, Uint64 generation)` |
| `pbMapperDesktopWaitFrame` | `pb_mapper_desktop_wait_frame` | same |
| `pbMapperDesktopFreeFrame` | `pb_mapper_desktop_free_frame` | `Void(Pointer<DesktopBuffer>)` |

### 2.5 `DesktopVideoTexture` (`ui/lib/src/ffi/desktop_video.dart`)

- `supported` = `Platform.isAndroid || Platform.isWindows` (L42).
- Static `MethodChannel('pb_mapper/desktop_video')` (L39) and static
  `_textures: Map<int, DesktopVideoTexture>` (L40). The method-call handler is
  installed once on first `create()` and never removed (L53-64); it routes
  `event` calls by `textureId` to the texture's broadcast `_events` controller.
- `create()` (L49-70): throws `UnsupportedError` if unsupported; `invokeMethod<int>('create')`;
  id < 0 or null → `StateError`. Android caps live textures at 2
  (`DesktopVideo.kt:31`).
- `submit({bytes, generation, frameId, ptsUs, width, height, keyframe})` (L72-92),
  `reset()` (L94-98, a barrier: Android replies after the worker released the old
  codec, `DesktopVideo.kt:281-302`), `stats()` (L100-106, Android enables
  per-stage timing diagnostics on first call because `timings` defaults to true,
  `DesktopVideo.kt:48,254-273`), `dispose()` (L108-117).
- `DesktopVideoEvent{generation, frameId, width, height, presented, needKeyframe, message}`
  (L7-33). Doc comment: "Acknowledges consumption by the native texture, never
  queue acceptance" (L6).

### 2.6 `AppStore` desktop parts (`ui/lib/src/state/app_store.dart`)

```dart
DesktopStore? _desktop;                                  // L38
DesktopStore? get activeDesktop => _desktop;             // L39
DesktopStore get desktop =>                               // L40-42
    _desktop ??= DesktopStore(api.desktop, onRestartReady: onRestartReady)
      ..addListener(_notify);
```

`start()` ends with `unawaited(desktop.restoreOnStartup())` (L192), so every app
launch creates the store. `dispose()` disposes it (L510). **Every DesktopStore
notification is re-broadcast as an AppStore notification** (L42), reaching every
AppStore listener: `ListenableBuilder`s in `overview_page.dart:89-90`,
`tunnels_page.dart:52-53`, `navigation.dart:180-181`, the sharing banner
`app.dart:324-331`, and the tray `_store.addListener(_refreshTray)` on desktop
platforms (`app.dart:217`, `TrayService.refreshStatus` dedups the actual render).
`api.desktop` is `PbMapperApiClient.desktop` (`pb_mapper_api.dart:498`), real
`final DesktopApiClient desktop = DesktopApi()` (`pb_mapper_api.dart:621`), fake
`final FakeDesktopApi desktop = FakeDesktopApi()` (`test/fake_pb_mapper_api.dart:14`).

### 2.7 Native counterparts (contract only)

- Android `DesktopVideo.kt`: handler on the platform main thread (L28-56). `submit`
  validates `bytes.size <= 4 MiB`, `width 2..7680`, `height 2..4320`,
  `w*h <= 16 777 216` (L43). Per texture: queue of `Submission` ≤ 4 (overflow
  clears queue, bumps revision, requests key, L231-237); non-key while awaiting key
  is dropped with a `needKeyframe` event (L238-241); stale generation/frameId
  replies immediately (L221, L229). The submit reply is sent **only when
  MediaCodec accepted the input or it was retired** (L66-71, L501, L518-522).
  The pump admits input only while `pending < 16 && waitingOutputs + outputs < 4`
  (L492). One output is on the Surface at a time; the next is released only after
  Flutter consumed the previous SurfaceTexture timestamp
  (`setOnFrameConsumedListener` L168-177, `consumeSurface` L180-215), which also
  emits the `presented` event (L198-201). Events are coalesced per kind and posted
  to the main looper (L317-336). `ptsUs` from Dart is ignored (decoder uses its
  own `pts += 16667`, L508). Not consumed for > 2 s → `fail("Native video texture
  was not consumed")` → `needKeyframe` (L525-527).
- Windows `desktop_video.cpp`: handler on platform thread (L535-570); submit reply
  when the decoder consumed/retired the packet (L79-85, L144-178, L476-496);
  `presented` is emitted when the engine acquires the GPU surface for a new frame
  (`AcquireSurface` L196-221).

---

## 3. Data and control flow (step by step)

### 3.1 Connect (`connect` L874-891 → `_openViewer` L893-950)

1. Page calls `store.connect(invite, relayOnly)` (`desktop_page.dart:179`).
2. `await _disconnecting` (L875); return silently if disposed, `busy`, or an
   attempt is running (L876).
3. `_viewerForeground = true`; cancel `_retryTimer`/`_retryAt`; `_target = (invite, relayOnly)`;
   `_controlWanted = true`; `_identityMismatch = false`; `reconnectAttempt = 0`;
   clear `error`, `recoveryReason`; `serial = ++_sessionSerial` (L877-887).
4. Start the 250 ms poll timer if absent (L888). `await _openViewer(serial)`, then `await poll()`.
5. `_openViewer(serial)`: guard (disposed, background, no target, stale serial,
   attempt running, L895-901). `_attemptSerial = serial`; `_notify()` (L902-903).
6. `needsDisconnect = snapshot.viewer != null || reconnectAttempt > 0` (L905).
   `cleanup = _clearVideo()` — synchronously bumps `_mediaEpoch` and tears down
   media (§3.9) — capture `epoch = _mediaEpoch` (L906-907).
7. If needed `await api.disconnect()` (L908); `await cleanup` (L909); re-check
   epoch/serial/foreground (L910-912).
8. `await api.connect(invite, relayOnly: ...)` (L913) — a `compute` isolate
   blocking in Rust `connect_viewer` until the session (incl. direct/relay path
   selection and certificate pin) is established; cancellable from Rust by a
   later `disconnect` (`desktop.rs:109-166`).
9. On return (if still current, L914): `_sequence = 0`; `_inputEpoch++`;
   `frame = null`; `_endingControl = !_viewerForeground`; `_releasing = false`;
   `_mediaFailed = false`; `_recoveryStarted = now`; clear
   `_lastKeyframeRequest`, `_controlRequestedAt`, `error`; viewer snapshot =
   `DesktopStatus(state: 'connecting')` (L915-925).
10. `_frames = StreamIterator(api.frames())`; `unawaited(_consumeFrames(frames, epoch))` (L926-928).
11. `catch` (L929-942): `DesktopCallException` with `code == 'INVALID_ARGUMENT'`
    is terminal: `_target = null`, `_controlWanted = false`, viewer snapshot null
    (back to the input page, no retries). Any other error → `_mediaFailed = true`.
12. `finally` (L943-949): clear `_attemptSerial`; if `_mediaFailed`,
    `_scheduleReconnect(error)`; `_notify()`.

### 3.2 Frame worker isolate and its single credit

Main side (`DesktopFrameStream`) and worker side (`_desktopFramesWorker`):

1. `_consumeFrames` calls `frames.moveNext()` → StreamIterator listens →
   `onListen: _start` (L121) → ports opened, `spawn` called (L186-231).
2. Worker starts, creates `requests` port, sends its `SendPort` (L83).
3. Main receives the `SendPort` (L191-200): replays queued `_calls`, then `_credit()`
   → `_outstanding = true; send(true)`.
4. Worker listener (L61-82): `false` → `stopped = true`; `true` → `credit = true`;
   `(int id, Map request)` → synchronous `_callJsonIsolate({'op':'desktop', 'handleAddress', 'request': jsonEncode(request)})`
   and reply `(id, result)` (errors replied as `{'success': false, 'message'}`).
   Every message completes the `ready` completer (L81).
5. Worker loop (L85-107): if no credit, `await ready.future` (event loop runs →
   requests are serviced here); with credit, call
   `_desktopFrameIsolate((handle, after, generation), wait: true)` — a
   **synchronous FFI call that blocks the isolate** for up to 16 ms in Rust
   `DesktopState::wait_frame` (`desktop.rs:167-190`).
   - `null` (timeout or no viewer) → `await Future.delayed(2 ms)` (requests are
     serviced here too), loop.
   - frame → `after/generation` updated from status (Rust ignores both arguments,
     `desktop.rs:1221-1224,1245-1248,1258-1261`), `credit = false`,
     `send(frame)` (the `Uint8List` is copied across isolates).
6. Main receives the `Map` (L204-209): `_outstanding = false`;
   `_controller.add(Map.from(message))`; `scheduleMicrotask(_credit)`.
7. The controller delivers to the `.map` in `DesktopApi.frames()`
   (`DesktopStatus.fromJson`) and then to the StreamIterator, which **pauses** its
   subscription until the next `moveNext()`. `onPause` sets `_paused = true`
   before the scheduled `_credit` microtask runs, so no new credit is sent
   (comment at L126).
8. `_consumeFrames` awaits `_acceptFrame` (L667); only when it returns does the
   loop call `moveNext()` → resume → `onResume` → microtask `_credit()` → worker
   gets `true` → back to step 5.

Result: exactly one frame is owned between Rust's `media::Queue` and the native
decoder. Errors: worker exceptions are sent as a `String` (L108-110) → main
`addError(StateError(message))` + `stop()` (L210-213); isolate errors arrive on
`_errors` (L215-222); on exit the worker closes `requests` and sends `null`
(L111-113, ignored by main); `_exited` → `_finish()` (L223).

Idle cost with credit held: one ≤16 ms blocking wait + 2 ms timer per loop,
about 55 wakeups/s on a static desktop.

### 3.3 `_consumeFrames` (L660-686)

1. `while (await frames.moveNext())`: break if disposed or epoch changed; else
   `await _acceptFrame(frames.current, epoch)` (L665-668).
2. `catch`: if current, `error = e.toString(); frame = null; _notify()` (L669-674).
   Any exception thrown by `_acceptFrame` (invalid dimensions L514-516, texture
   create/submit/reset `PlatformException`, `UnsupportedError`) lands here — it
   ends the whole stream, not just the frame.
3. `finally`: `await frames.cancel()` (waits for worker exit through `onCancel` →
   `_closed`); if still current and still the active iterator: `_frames = null;
   _mediaFailed = true; frame = null; videoPresented = null; error ??= 'Desktop
   video stream ended'; _notify()` (L675-685). The next `poll()` →
   `_maintainViewer` → `_scheduleReconnect` (L369-371).

### 3.4 `_acceptFrame(next, epoch)` (L471-560)

1. Return if not current (L472). `cursorStatus.value = next.status` (L473) —
   fires the viewer's `_updateCursor` on every frame and every heartbeat (no `==`
   on `DesktopStatus`, so every new object notifies).
2. **Liveness/status heartbeat** (streaming, empty bytes, same generation as
   `frame`, L474-489): only `snapshot = snapshot.withViewer(next.status)`; does not
   touch `frame`, `_frameAt`, presentation or the texture. Notify if
   `controlling` or `controlAllowed` changed or ≥100 ms since `_lastMediaNotify`.
   Return. (Rust `media::Queue::pop` returns a coalesced pending status **before**
   any queued frame, `media.rs:70-76`, so cursor/status updates consume the same
   single credit as video.)
3. `frame = next; snapshot = snapshot.withViewer(next.status)` (L490-491).
4. **Non-streaming or empty bytes** (other generation, L493-502): clear
   `videoPresented`, `_videoPresentedAt`, `_videoStatuses`, `_videoGeneration =
   -1`; `_notify()`; return. Texture is kept; decoder reset happens on the next
   real frame of the new generation.
5. `_frameAt = now` (L503).
6. **JPEG frame** (L504-512): clear presentation state; if a texture was used
   (`_videoGeneration != -1`) `await texture.reset()`; `_videoGeneration = -1`.
7. **H.264 frame** (L513-554):
   a. `videoWidth/Height < 2` → throw `StateError('Invalid desktop video dimensions')`.
   b. Lazily `await DesktopVideoTexture.create()`; if no longer current or not
      `connected`, dispose it and return; else store it and subscribe
      `texture.events` → `_presentVideo(event, epoch)` guarded by
      `_current(epoch) && identical(texture, videoTexture)` (L517-529).
   c. Generation change (L531-539): `_videoGeneration = generation`; clear
      presentation state; `_notify()`; `await texture.reset()` (barrier before the
      new IDR); return if superseded.
   d. `_videoStatuses[frameId] = (status, _frameAt)`; evict oldest beyond 256 (L540-543).
   e. `await texture.submit(bytes, generation, frameId, ptsUs, width, height, keyframe)`
      (L544-552). On Android this completes only when MediaCodec accepted the
      access unit (§2.7) — the stop-and-wait point.
8. Notify: JPEG always; H.264 at most every 100 ms (L555-559).

### 3.5 `_presentVideo(event, epoch)` (L562-605)

1. Ignore if not current or `event.generation != _videoGeneration` (L563).
2. `wasFresh = fresh` (L564).
3. `needKeyframe` (L565-574): `videoPresented = null`, `_videoPresentedAt = null`
   (→ not fresh → not controlling); non-empty message → `error = message`,
   `_videoError = (generation, message)`; `_pendingKeyframe = true`;
   `_flushFeedback(epoch)`.
4. `presented` (L575-598) accepted only if: the frameId is in `_videoStatuses`,
   its status generation equals `frame.status.generation`, `frameId >
   videoPresented.frameId` (monotonic), viewer snapshot is streaming, and its
   receipt time is < 2 s ago. Then `videoPresented = status`;
   `_videoPresentedAt = receivedAt`; clear a matching `_videoError` (and `error`
   if it is that message); `_presentCount++`, recompute `_presentFps =
   count / elapsed` when ≥1 s since `_presentSample`; `_pendingPresentation =
   status`; `_flushFeedback(epoch)`.
5. Notify if `needKeyframe`, or it just became fresh, or ≥100 ms since
   `_lastMediaNotify` (L599-604).

`presentFps` (L55-59) is reported as 0 when `_videoPresentedAt` is ≥2 s old. It is
never updated on the JPEG path.

### 3.6 `_flushFeedback(epoch)` (L607-647) and `acknowledgePresented` (L649-658)

1. Return if a flush is already running (`_feedback != null`) or not current.
2. Async loop while current and (`_pendingKeyframe` or `_pendingPresentation`):
   - Keyframe first: clear flag, `await api.keyframe()`.
   - Else presentation: `delay = 33 ms - (now - _lastAck)`. If positive, arm
     `_presentationTimer` (once) to re-run `_flushFeedback(epoch)` and break —
     guarantees the final presented frame is acknowledged even if the host
     pauses (comment L622-623). Otherwise cancel the timer, take
     `_pendingPresentation`, `_lastAck = now`, `await api.presented(generation, frameId)`.
   - All errors swallowed (teardown).
3. `_feedback` is cleared in `whenComplete` if still identical (L642-646).
4. Comment L617-618 records why 33 ms: "Waiting 100 ms can fill the bounded
   receiver window even on a fast link" — i.e. host send credit is gated on these
   ACKs today.

`acknowledgePresented(status, epoch)` is the JPEG path's entry, called by the
viewer after a decoded image was painted (`desktop_viewer.dart:144-154`); it
requires current epoch, `connected`, matching generation and streaming, then sets
`_pendingPresentation` and flushes.

Downstream (Rust): `presented` op → `Viewer::presented` → `watch::send_replace`
(`viewer.rs:366-368`) → network send loop `Command::Presented` (biased before
input, `viewer.rs:312`) → host `presentation()` (`worker.rs:443-472`) sets
`frame_ack_ms`, the input lease, and the congestion credit; host stops the session
with "Renderer stopped acknowledging presented frames; reconnect the viewer" when
ACKs stall (`host.rs:202-211`).

### 3.7 `poll()` (L316-365) and `_maintainViewer()` (L367-426)

The 250 ms `Timer.periodic` is created by `load()` (L149), `_restoreStartupHost`
(L183, L191), `_resumePendingHost` (L249) and `connect()` (L888), and is cancelled
only by `dispose()` (L1125) or a restart handoff (L801, L844). It is **not**
stopped when the desktop page or viewer closes or the session ends.

`poll()`:
1. Return if `_polling`, disposed, or disconnecting. If `busy`, attempt running
   or retry waiting: just `_notify()` (drives the retry countdown) and return
   (L317-321).
2. `status = await api.status()` (a fresh `compute` isolate each time);
   `snapshot = status` (whole snapshot replaced; Rust reports a viewer as
   `stale`/`controlling: false` when no frame arrived for 2 s, `viewer.rs:355-363`);
   update `invitation` from background (L325-330).
3. Viewer gone or `stopped` (L331-341): `_mediaFailed = viewer != null`; with a
   target → `_scheduleReconnect(viewer.message ?? error)` and return; else clear
   video if anything is held.
4. If `connected && _frames == null && !_mediaFailed` → **pull path**
   `api.frame(frameId, generation)` and `_acceptFrame` (L342-350). In production
   `_frames` is always set while a store-opened session is live, so this path is
   effectively exercised only by tests that never call `connect()`
   (`desktop_test.dart`, the coalescing test in `desktop_recovery_test.dart`).
5. `if (!viewer.controlling) _endingControl = false` (L354); `await _maintainViewer()`.
6. `catch`: `error`, and with a target `_scheduleReconnect(error)`. `finally`:
   `_polling = false; _notify()` — **every poll notifies** (L361-364).

`_maintainViewer()` (only with a target and foreground, L368):
- `_mediaFailed` → `_scheduleReconnect(error)` (L369-372).
- Viewer message starts with `input_failed:` → `_controlWanted = false;
  _endingControl = true` (L374-377) — host input failure never auto-regains control.
- `capture_unavailable` → reset recovery timers and wait (no network retry,
  L378-385).
- `fresh` → clear recovery; `_stableSince ??= now`; after 3 s stable
  `reconnectAttempt = 0`; drop `_controlWanted` if `controlAllowed` is false;
  if `_controlWanted && !controlling && !_restoringControl` and ≥1 s since
  `_controlRequestedAt` → `api.control(true)` (L386-411).
- Not fresh → `_recoveryStarted ??= now`; ≥3 s → `_scheduleReconnect`; ≥700 ms and
  ≥1 s since last request → `_pendingKeyframe = true; _flushFeedback` (L413-425).

### 3.8 Reconnect (`_scheduleReconnect` L428-469)

- Reason containing `desktop identity changed` (certificate pin mismatch):
  `_identityMismatch = true`, cancel retry, `_controlWanted = false`,
  `_endingControl = true`, `_inputEpoch++`, notify; no automatic retry until a
  deliberate `connect()` (L432-443).
- Otherwise skip if disposed, mismatch, background, no target, already waiting,
  or an attempt is running (L444-451).
- `reconnectAttempt++`; delay = `(1 << (attempt-1).clamp(0,4)).clamp(1,15)` s
  → 1, 2, 4, 8, 15 s; `recoveryReason`; `_retryAt`; `_endingControl = true`;
  `_inputEpoch++`; one-shot `Timer` → `_openViewer(serial)` if the session serial
  is unchanged (L452-468).

Triggers: `poll` viewer missing/stopped with target (L333-335), `poll` exception
(L359), `_maintainViewer` media failure or ≥3 s without fresh picture (L370,
L417), `_openViewer` non-terminal failure (L945-947). Successful recovery reuses
the same `_target` (same invitation and path policy), restarts `_sequence` at 0,
and re-requests control only after a fresh frame (L394-410).

Background (`setViewerForeground(false)`, L1024-1042): cancels retries, sets
`_endingControl`, `_inputEpoch++`, fire-and-forget `api.control(false)`; keeps
`_controlWanted`. Note `_consumeFrames` keeps submitting frames while no viewer is
mounted; on Android the texture is then not consumed, presented events stop, the
native decoder fails after 2 s and the host ends the session after the ACK stall;
reconnect waits for foreground. Foreground: `_recoveryStarted = null` and a
microtask `poll()`.

### 3.9 Teardown (`_clearVideo` L688-726, `disconnect` L952-987, `dispose` L1119-1133)

`_clearVideo()` is the only place `_mediaEpoch` increments (L689). Synchronously:
`cursorStatus = null`, cancel `_presentationTimer`, detach `_frames`,
`_videoEvents`, `videoTexture`, `_feedback`; clear pending feedback, `frame`,
`_frameAt`, presentation, `_videoGeneration`, `_videoStatuses`, `_videoError`,
fps counters, `_lastAck`, `_lastMediaNotify`. Then awaits `frames.cancel()`
(worker exit), `events.cancel()`, `texture.dispose()`, and the old feedback future.
Callers: `poll` (viewer gone, L338), `_openViewer` (L906), `_disconnect` (L976),
`dispose` (L1127). Consequence: every reconnect destroys the texture and the last
picture.

`disconnect()` is idempotent through `_disconnecting` (L952-962). `_disconnect()`:
`_target = null`, `_sessionSerial++`, cancel retry, clear desired control,
mismatch, recovery; `_endingControl = true`; `_inputEpoch++`; start `_clearVideo`;
viewer snapshot null; notify; `api.disconnect()` (which first stops frame workers,
then cancels a pending Rust connect or stops the viewer); await cleanup and
`_inputTail`; notify.

`dispose()`: cancel host resume, `_disposed = true`, `_target = null`, cancel
timers, `_inputEpoch++`, `_clearVideo()`, `api.disconnect()` and
`api.stopHost(onExit: true)` (stops only an ephemeral local host; a service-owned
background host keeps running, `desktop.rs:663-668`), dispose both ValueNotifiers.

### 3.10 Input path (`send` L1044-1100, `_interruptInput` L1102-1113)

Producer: `DesktopViewer._send` (`desktop_viewer.dart:202-207`) passes `_rendered`
(for H.264 = `store.videoPresented` as of the last store notification,
`desktop_viewer.dart:77`) and `_renderedEpoch`, only when `_interactive`
(`desktop_viewer.dart:193-201`). Event maps (`kind`): `move{x,y}`,
`button{button,down}`, `wheel{horizontal,vertical}` (clamped ±100,
`desktop_viewer.dart:456-460`), `key{key,down}`, `text{text}`.

`send(rendered, event, {mediaEpoch})`:
1. Drop if `mediaEpoch` given and stale (L1049); drop if `!controlling` or
   `rendered.generation != frame.generation` (L1050).
2. Motion coalescing: if this is a `move`, and the pending (not yet dispatched)
   move belongs to the current `_inputEpoch`, overwrite its `rendered`/`event`
   and return its `done` future (L1051-1057).
3. Any other kind is an ordering barrier: `_pendingMove = null` (L1060).
4. Bound: if `_inFlight >= 16` → `_interruptInput()` (end control rather than drop
   a release or replay a click) and return (L1063-1066).
5. `_inFlight++`; capture `epoch = _inputEpoch`; create `_PendingMove` for moves;
   chain on `_inputTail` (L1067-1071).
6. When its turn comes: clear `_pendingMove` if it is this one; re-check disposed,
   epoch, `controlling`, generation; `await api.input(generation, frameId,
   sequence: ++_sequence, input)` (L1072-1087).
7. On error (if epoch still current): `error = e.toString()`; `_interruptInput()`
   (L1088-1093). `finally _inFlight--`.
8. `_inputTail = operation; await operation` (L1097-1099).

Only one `api.input` is in flight at a time (serial chain); it travels through
the frame-worker isolate mailbox (§3.2), so each event can wait up to one
`wait_frame` timeout (16 ms) plus the queue ahead of it.

`_interruptInput()`: `_endingControl = true`, `_inputEpoch++`, `_pendingMove =
null`, `api.control(false)` (errors ignored), notify.

`release()` (L1009-1022): if connected and not releasing: `_inputEpoch++`,
`_releasing = true` (so `controlling` is false), `await _inputTail` (queued items
see the epoch change and skip), then `api.release()` if still current; finally
`_releasing = false`.

`control(enabled)` (L989-1007): sets `_controlWanted`, `_controlRequestedAt`,
`_endingControl = !enabled`, `_inputEpoch++`, notify; enabling goes through
`run()` (`busy`), disabling bypasses `busy` so focus loss/end-control is never
skipped.

`_inputEpoch` increments (each drops all queued, unsent input): L439, L459, L916,
L975, L994, L1011, L1034, L1104, L1126.

Host side (contract): `sequence` must strictly increase per session
(`worker.rs:586-589`), the input's `frame_id` must be one the host sent < 2 s ago
in the current generation (`worker.rs:594-603`), and held input is released when
the last presented frame is stale (`release_if_stale`, `worker.rs:402-414`).
Rust `Viewer::input` additionally requires `streaming && controlling &&
generation` match and turns a full 32-slot input channel into a control release
(`viewer.rs:402-421`).

### 3.11 Host lifecycle (for the host card)

- `load()` (L142-151): under `run()`: `capabilities()`, `trustStatus()`,
  `_loadBackground()` (`background_action: status`); start poll timer; `poll()`.
  Called by `DesktopPage.initState` in a microtask (`desktop_page.dart:30-35`).
- `restoreOnStartup()` (L153-157) → `_restoreStartupHost(serial)` (L162-203): read
  trust first; if saved `background && autoStart && matchesRelay` →
  `_resumePendingHost`; else load background; if no background but `autoStart &&
  matchesRelay` → `restoreHost()`; start polling when anything is hosted. Errors →
  `_scheduleHostResume` backoff 1, 2, 4, 8, 15 s (L262-273).
- `_resumePendingHost(serial)` (L225-260): re-validates trust, `api.restoreHost()`,
  reloads background and trust, starts polling; retries on failure.
- `_hostResumeSerial` (`_cancelHostResume` L275-280) invalidates late replies of
  any host resume; used by `backgroundAction`, `enableHost`, `stopHost`,
  `revokeTrust`.
- `enableHost(service, display)` (L769-815): the page's single "share" action
  (`desktop_page.dart:120-128`). On IPC failure, if durable intent exists, schedule
  `_resumePendingHost` and rethrow. `restart: true` → `restarting = true`, cancel
  poll timer, notify, `await onRestartReady()`.
- `stopHost()` (L817-834) and `revokeTrust()` (L295-314) work outside `run()` so a
  pending outgoing connection cannot block a local stop.
- `allowControl(allow)` (L855-872): enabling under `run()`, disabling always; then
  refresh trust and poll. Op `host_permission`.
- `helperAction(action)` (L836-853): `install`/`disable`/`uninstall` (page
  `desktop_page.dart:249,257,264`); `true` → restart handoff.
- Unused by pages: `startHost()` (L743-767), `backgroundAction()` (L213-223);
  `restoreHost()` (L282-293) is used only internally (L181).

### 3.12 When `notifyListeners` fires (`_notify` L1115-1117)

| Source | Lines | Rate |
| --- | --- | --- |
| `poll()` finally / early return | L319, L363 | every 250 ms forever once the timer started |
| Heartbeat status (same generation) | L482-487 | on control change, else ≤10 Hz |
| Non-streaming / empty frame of other generation | L498 | per event |
| H.264 new generation | L536 | per generation |
| End of `_acceptFrame` | L555-559 | JPEG every frame; H.264 ≤10 Hz |
| `_presentVideo` | L599-604 | needKeyframe, became fresh, else ≤10 Hz |
| `_consumeFrames` error/end | L673, L683 | per stream end |
| `_scheduleReconnect` | L441, L468 | per scheduling |
| `run()` start/end | L732, L739 | per host/control op |
| `_openViewer` start/end | L903, L948 | per attempt |
| `_disconnect` | L979, L986 | per disconnect |
| `control()` | L995 | per toggle |
| `_interruptInput` | L1112 | per interrupt |
| Host flows | L201, L259, L312, L802, L832, L845, L864 | per op |

Plus the ValueNotifiers: `cursorStatus` on **every** accepted frame/heartbeat
(L473) and on clear (L690); `videoPresentation` on every presentation change (no
listeners anywhere). Each store notification is re-sent by AppStore (§2.6).
Consumers that rebuild on it: `DesktopViewer._update` → `setState` of the whole
viewer `Scaffold` (`desktop_viewer.dart:56-92`), `DesktopConnectionBanner`
(`desktop_connection_banner.dart:11-13`), `DesktopPage` (`desktop_page.dart:44-46`),
`DesktopSharingBanner` (`desktop_page.dart:310-312`), plus all AppStore listeners.

---

## 4. Contracts and formats

### 4.1 JSON envelope (`pb_mapper_desktop_call_json`)

Request: one JSON object `{"op": <snake_case>, ...}` ≤16 384 bytes, parsed into
Rust `enum Request` with `#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]`
(`desktop.rs:193-255`, size check L518-523). New Dart fields need Rust support
first or the call fails with `INVALID_ARGUMENT "invalid desktop request"`.
Response: `{"success": true, "data": {...}}` or
`{"success": false, "message": "...", "code": "INVALID_ARGUMENT"|"NOT_FOUND"|...}`
(`response.rs:32-52`). `Invitation::decode` failures are `INVALID_ARGUMENT`
(`desktop.rs:724-725`), which `_openViewer` treats as terminal.

| `op` | Dart caller | Args | `data` |
| --- | --- | --- | --- |
| `capabilities` | `capabilities()` L360 | — | `{capabilities:{host,viewer,secure_desktop,mode}, helper:{installed,running,authorized_application,identity_verified,message}|null, displays:[{id,name,x,y,width,height}]|null, message}` |
| `status` | `status()` L378 | — | `{host: SessionStatus|null, viewer: SessionStatus|null, background: Background|null}` |
| `trust_status` | `trustStatus()` L303 | — | `{supported, saved:{relay,service,display_id,allow_control,relay_only,privileged,auto_start,background}|null, matches_relay}` |
| `trust_revoke` | `revokeTrust()` L320 | — | `{}` |
| `host_restore` | `restoreHost()` L317 | — | `{invitation}` (or the `host_enable` shape) |
| `host_enable` | `enableHost()` L281 | `service`, `display_id` | `{restart, invitation, background}` |
| `helper_action` | `helperAction()` L337 | `action` | `{restart}` |
| `background_action` | `backgroundAction()` L297 | `action` (`status`/`start`/`stop`/`revoke`) | Background |
| `host_start` | `startHost()` L395 | `service, display_id, allow_control, relay_only, privileged, remember, auto_start, background` | `{invitation}` |
| `host_stop` | `stopHost()` L417 | `on_exit` | `{}` |
| `host_permission` | `allowControl()` L422 | `allow_control` | `{}` |
| `connect` | `connect()` L427 | `invitation, relay_only, native_video, video{max_width,max_height,max_fps,bitrate}` | `{}` |
| `disconnect` | `disconnect()` L449 | — | `{}` |
| `control` | `control()` L455 | `enabled` | `{}` |
| `release` | `release()` L460 | — | `{}` |
| `keyframe` | `keyframe()` L332 | — | `{}` |
| `presented` | `presented()` L341 | `generation`, `frame_id` | `{}` |
| `input` | `input()` L465 | `event:{generation, frame_id, sequence, input:{kind,...}}` | `{}` |

Background shape (read by `DesktopBackground.fromJson` L171-180): `configured`,
`enabled`, `settings.service`, `settings.auto_start`, `server`, `invitation`,
`message` (Rust also sends `host`, `namespace`, `supported`, `service_disabled`).

### 4.2 `SessionStatus` JSON → `DesktopStatus` (`desktop_api.dart:86-119`, Rust `lib.rs:125-165`)

| JSON | Dart | Notes |
| --- | --- | --- |
| `state` | `state` (default `stopped`) | `streaming`, `stale`, `connecting`, `waiting`, `capture_unavailable`, `stopped` |
| `message` | `message` | `input_failed: ...`, secure-desktop hints |
| `controlling`, `control_allowed` | `controlling`, `controlAllowed` | |
| `generation`, `frame_id` | `generation`, `frameId` | |
| `display{id,name,x,y,width,height}` | `display` (x, y dropped) | physical desktop pixels for input |
| `path` | `path` | set by the viewer network task |
| `direct_stage`, `direct_failure` | `directStage`, `directFailure` | optional |
| `frames`, `bytes` | `frames`, `bytes` | |
| `desktop` | `desktop` (default `ordinary`) | |
| `video{codec,width,height,keyframe,pts_us,duplicate}` | `codec` (default `jpeg`), `videoWidth`, `videoHeight`, `keyframe` (default true), `ptsUs` | `duplicate` ignored |
| `capture_fps`, `receive_fps` | `captureFps`, `receiveFps` | |
| `dropped_frames`, `bitrate` | `droppedFrames`, `bitrate` | |
| `cursor{x,y,visible}` | `cursor` | i32 coordinates |
| `frame_ack_ms` | `frameAckMs` | host frame-ready → renderer ACK round trip incl. presentation and coalescing (`lib.rs:156-160`); drives `delayed` |
| `roundtrip_probe` | — | consumed in the Rust receive task, never reaches Dart (`viewer.rs:225-233`) |

### 4.3 Frame buffer (`pb_mapper_desktop_frame` / `_wait_frame`)

One allocation: `u32` little-endian metadata length, UTF-8 JSON `SessionStatus`,
then raw encoded bytes (H.264 Annex-B access unit or JPEG); null = no newer frame
(`desktop.rs:1211-1278`). Every non-null pointer must be freed with
`pb_mapper_desktop_free_frame` (`desktop.rs:1280-1292`). `after`/`generation`
arguments are accepted but ignored; `Viewer::next_frame` pops each queued event
exactly once. Status heartbeats are frames with empty bytes.

### 4.4 Frame-worker isolate messages

| Direction | Message | Meaning |
| --- | --- | --- |
| worker → main | `SendPort` | handshake (L83) |
| main → worker | `true` | one frame credit (L183) |
| main → worker | `false` | stop (L170, L194) |
| main → worker | `(int id, Map<String,Object?> request)` | session-bound JSON call (L163, L197) |
| worker → main | `(int id, Map result)` | call reply (L69-78) |
| worker → main | `Map{'status': Map, 'bytes': Uint8List}` | frame (L106) |
| worker → main | `String` | fatal error (L109) |
| worker → main | `null` | exit (L112, ignored) |
| isolate | `onExit` / `onError` ports | `_finish()` / error + stop |

Limits: ≤16 pending calls per worker (L157); one outstanding frame.

### 4.5 MethodChannel `pb_mapper/desktop_video`

| Call | Args | Reply |
| --- | --- | --- |
| `create` | — | `int textureId` |
| `submit` | `textureId, bytes: Uint8List, generation, frameId, ptsUs, width, height, keyframe` | `null` once accepted/retired by the decoder |
| `reset` | `textureId` | `null` after the old codec is released (barrier) |
| `stats` | `textureId` (Android: `timings` default true) | Map: `submitted, submittedKeyframes, decoded, presented, lastPresentedGeneration, lastPresentedFrameId, dropped, decoder, configurations, recoveries, lastRecovery, queued, decoding, outputs, surfacePending, timings{input_wait, decode_callback, ready_to_release, receive_to_present, surface_consume, consume_dispatch, pump_dispatch, pump_work, peak_pending, peak_outputs}` (Android) |
| `dispose` | `textureId` | `null` |
| native → Dart `event` | `textureId, generation, frameId, width, height, presented, needKeyframe, message` | — |

### 4.6 Input event JSON (Rust `lib.rs:246-295`)

`{"generation": u64, "frame_id": u64, "sequence": u64, "input": {"kind": ...}}`
with `kind` ∈ `move{x:u32,y:u32}`, `button{button: left|right|middle, down}`,
`wheel{horizontal:i32, vertical:i32}` (|v| ≤ 100), `key{key, down}`,
`text{text}` (≤1024 bytes, no NUL). `key` ∈ `shift, control, alt, meta, enter,
escape, tab, backspace, delete, left, right, up, down, home, end, page_up,
page_down, a, c, v, x, z` — exactly the Dart `_keyNames` values
(`desktop_viewer.dart:541-568`). An unknown key fails deserialisation →
`api.input` throws → `_interruptInput()` ends control.

### 4.7 Public members used outside the store

| Member | Kind | Used at |
| --- | --- | --- |
| `DesktopStore(api, {now, onRestartReady})` | ctor | `app_store.dart:41`, tests, `tool/desktop_viewer_acceptance.dart:58` |
| `load()` | method | `desktop_page.dart:33` |
| `restoreOnStartup()` | method | `app_store.dart:192` |
| `capabilities` | field | `desktop_page.dart:51,76,118,127,199,211,216` |
| `trust` | field | `desktop_page.dart:122,126,237` |
| `snapshot` | field | `desktop_page.dart:50,55,87-90,125,232`; `desktop_viewer.dart:258`; `desktop_connection_banner.dart:16`; acceptance L84 |
| `invitation` | field | `desktop_page.dart:97,100` |
| `error` | mutable field | `desktop_page.dart:66,70`; banner L39; **written** by `desktop_viewer.dart:158` |
| `busy` | mutable field | `desktop_page.dart:118,129,175,239,247,255,262,275`; `desktop_viewer.dart:287`; written by `desktop_test.dart:303` |
| `sharing` | getter | `desktop_page.dart:86,200,228,312` |
| `connected` | getter | `desktop_page.dart:146,184` |
| `fresh` | getter | `desktop_viewer.dart:288,491` |
| `controlling` | getter | `desktop_viewer.dart:194` |
| `connectionPhase`, `retryIn`, `pictureAge`, `presentFps`, `reconnectAttempt`, `recoveryReason` | getters/fields | `desktop_connection_banner.dart:15,73,30,80,85-86,39`; acceptance |
| `frame` | field | `desktop_viewer.dart:71-72,82,129-130,163,197,201,443` |
| `videoTexture` | field | `desktop_viewer.dart:427`; acceptance L85,110 |
| `videoPresented` | getter | `desktop_viewer.dart:77`; acceptance L111-112 |
| `mediaEpoch` | getter | `desktop_viewer.dart:57-58,78,91,128,147,157,196,443` |
| `cursorStatus` | ValueNotifier | `desktop_viewer.dart:42,95,172,177,186` |
| `enableHost`, `stopHost`, `revokeTrust`, `helperAction`, `allowControl` | methods | `desktop_page.dart:120,107/328,239,249/257/264,233` |
| `connect`, `disconnect` | methods | `desktop_page.dart:179,155`; `desktop_viewer.dart:266` |
| `control`, `release`, `send`, `acknowledgePresented`, `setViewerForeground` | methods | `desktop_viewer.dart:286/291, 406/422/461, 205, 153, 40/53/173/175/187` |
| `restarting` | field | tests only |
| `poll()` | method | tests only |
| `videoPresentation`, `startHost`, `backgroundAction` | — | unused outside the store |
| `AppStore.desktop` / `activeDesktop` | getters | `desktop_page.dart:10`, `app.dart:129`; `app.dart:327` |

The gesture surface receives `inputEpoch: (store, store.mediaEpoch, frame?.status.generation)`
(`desktop_viewer.dart:443`); any change cancels held local input and calls
`onRelease` (`desktop_gesture_surface.dart:64-98`) but keeps the zoom transform
(`desktop_gesture_test.dart:115-121`).

---

## 5. Invariants and security boundaries

1. **Input requires a fresh presented frame.** Store: `controlling` needs `fresh`
   (H.264: native presentation of the current generation whose receipt is < 2 s
   old) and the host grant, and not ending/releasing (L125-140). Viewer: `_interactive`
   additionally needs `_rendered` from the same media epoch and generation, and
   for JPEG a decode < 2 s old (`desktop_viewer.dart:193-201`). Each queued input
   re-checks epoch/controlling/generation immediately before the FFI call
   (L1076-1081). Host: frame id sent < 2 s ago, current generation, strictly
   increasing sequence, held input released when presentation goes stale
   (`worker.rs:402-414, 585-603`). At store level, JPEG `controlling` does not
   require presentation; that boundary lives in the widget and the host.
2. **Presentation is monotonic and current.** `_presentVideo` accepts only a
   newer frame id of the current generation, received < 2 s ago, while streaming
   (L577-582); late native consumption cannot revive an old picture (tested,
   `desktop_lifecycle_test.dart:296-310`).
3. **Reconnect never replays input.** Every session boundary bumps `_inputEpoch`
   (`_openViewer` L916, `_scheduleReconnect` L459, `_disconnect` L975); queued
   operations bail on epoch mismatch; `_sequence` restarts at 0 per session
   (L915). Saturation ends control instead of dropping a key-up (L1063-1066).
   Rust drains queued inputs behind a priority command (`viewer.rs:304`).
4. **Ordering.** Inputs are serialized through `_inputTail`; only consecutive
   unsent moves coalesce; buttons/keys/wheel/text are barriers (L1051-1060).
5. **Bounded queues.** One frame credit; ≤16 worker calls; ≤16 Dart in-flight
   inputs; ≤256 `_videoStatuses`; Android ≤4 queued submits and ≤4 decoder
   outputs; Rust media ≤8 frames/8 MiB; Rust input channel 32.
6. **Epoch hygiene.** All media callbacks check `_current(epoch)`; texture events
   also check texture identity (L525); `_clearVideo` is the only epoch increment.
7. **Host identity pin.** `desktop identity changed` stops automatic retries
   until a deliberate new `connect` (L432-443).
8. **Terminal invitation errors.** `INVALID_ARGUMENT` from `connect` returns to
   the input page without retries (L932-938).
9. **Host input failure** (`input_failed:`) never auto-regains control (L374-377).
10. **Local stop always works**: `stopHost`, `revokeTrust`, `control(false)` and
    `allowControl(false)` bypass `busy` (L817-834, L295-314, L999-1006, L858-865).
11. **Host-resume serial** discards late host replies after stop/revoke (L159-160).
12. Invitations are never logged by this layer; `error` strings come from Rust
    messages.

---

## 6. Tests covering this scope

CI runs `flutter test` on `ubuntu-latest` (`.github/workflows/syntax-check.yml`,
job `flutter-syntax-check`), where `DesktopVideoTexture.supported` is false, so the
three tests marked `skip: !DesktopVideoTexture.supported` **do not run in CI**;
they run only on Windows (or Android) hosts.

| Test | File:line | Covers | Runs in CI |
| --- | --- | --- | --- |
| background sharing survives closing the app and remains explicitly stoppable | `desktop_test.dart:19-96` | page + store host flow through `FakePbMapperApi`, helper maintenance buttons | yes |
| viewer maps touch and mouse hover and blocks ungranted or stale input | `desktop_test.dart:98-224` | JPEG pull path via `poll()`/`api.frame`, `acknowledgePresented`, input mapping 1920×1080, stale blocks input | yes |
| input is ordered, view-only is read-only, and release cancels queued actions | `desktop_test.dart:225-278` | send gating, sequences 1,2, release drops queued, stale → not controlling | yes |
| compact host flow enables control and exposes a persistent stop | `desktop_test.dart:280-309` | `busy` does not block stop | yes |
| the final presented frame is acknowledged without another frame and teardown cancels it | `desktop_lifecycle_test.dart:98-143` | 33 ms coalescing, trailing ACK timer, no ACK after disconnect | **no** |
| worker cancellation releases its native borrow before handshake and while paused | `desktop_lifecycle_test.dart:144-197` | `DesktopFrameStream` cancel before handshake / while paused, call failure, `onClosed` once | yes |
| disconnect is effective while busy and disposes a late native texture | `desktop_lifecycle_test.dart:199-241` | late `create` disposed, disconnect during `busy` | **no** |
| native consumption must be current and recent before input becomes live | `desktop_lifecycle_test.dart:243-334` | freshness rules, heartbeats keep picture, 2 s windows, `capture_unavailable` keeps texture, `presentFps` decay | **no** |
| host input failure does not automatically regain control | `desktop_recovery_test.dart:106-130` | `input_failed:` | yes |
| invalid invitation returns to input without automatic retries | `desktop_recovery_test.dart:132-156` | terminal `INVALID_ARGUMENT` | yes |
| a failed stream retries the same target and path, restores only fresh control, and manual stop cancels retries | `desktop_recovery_test.dart:158-224` | backoff 1 s/2 s, same target, control restore only when fresh, no replay, attempt reset after 3 s, disconnect cancels | yes |
| identity failure keeps the pin and stops automatic retries | `desktop_recovery_test.dart:226-249` | identity mismatch | yes |
| stalls request an IDR before reconnect, while desktop transitions and backgrounding do not churn connections | `desktop_recovery_test.dart:251-320` | 700 ms keyframe, `capture_unavailable`, background suppression, foreground race during cleanup | yes |
| cancelled connection completion cannot replace a newer session | `desktop_recovery_test.dart:322-349` | session serial / media epoch | yes |
| pointer motion coalesces without crossing click barriers | `desktop_recovery_test.dart:351-385` | coalescing + barriers (pull path) | yes |
| one action upgrades local viewing and revoke reflects service ownership | `desktop_host_readiness_test.dart:70-119` | `enableHost`, failed revoke keeps sharing | yes |
| one-click setup resumes after IPC failure but stop rejects a late reply | `desktop_host_readiness_test.dart:121-146` | `_resumePendingHost`, host-resume serial | yes |
| restart waits for a successful handoff and cancellation keeps UI alive | `desktop_host_readiness_test.dart:148-182` | `restarting`, `onRestartReady` | yes |
| opening during a slow connection keeps one viewer route | `desktop_viewer_navigation_test.dart:32-124` | route guard, reconnect with kept invitation | yes |
| gesture tests (3) | `desktop_gesture_test.dart:37-181` | tap/two-finger right click, pinch keeps texture and transform across `inputEpoch` change, trackpad + long-press drag | yes |
| helper restart returns to the desktop page | `app_test.dart:24-36` | `openDesktopOnStartup` route | yes |

`FakeDesktopApi` (`ui/test/fake_desktop_api.dart`): in-memory host/background/trust
state machine (`enableHost` → `startHost(background: true)` L6-18, `backgroundAction`
L22-40, `stopHost` keeps a service grant on `onExit` L135-160, `revokeTrust`
L60-64); viewer: `connect` sets `viewer = connecting` (L168-170), `control`
replaces `viewer` with a streaming status of `image`'s generation (L178-185),
`input` records `inputs`/`sequences` behind optional `inputGate` (L193-202),
`release` counts (L188-190), `frame()` returns `image` unless already delivered
(L205-208), `frames()` is **empty** (L67) so tests that call `connect()` override
it (`_StreamingApi` lifecycle L29-74, `_RecoveryApi` recovery L9-82,
`_SlowConnectionApi` navigation L13-29; `_HostProbe`/`_PendingHostProbe` host
readiness L10-67). The lifecycle tests mock the method channel with
`setMockMethodCallHandler` and inject native `event`s through
`ui.channelBuffers.push` (`desktop_lifecycle_test.dart:76-94`).

`ui/tool/desktop_viewer_acceptance.dart`: on-device harness (`--target`), loopback
bootstrap URL only (L19-31), real `DesktopApi` subclass `_AcceptanceApi` counting
`presented`/`keyframe` calls (L159-196), samples once per second for N seconds:
viewer status fields, `presentFps`, `pictureAge`, phase, native `stats()`, and
Flutter `FrameTiming` build/raster (L82-117, L199-226); report computes
`native_presented_fps` from the native `presented` counter after a 5 s warm-up
(L121-141).

---

## 7. Impact on the planned overhaul

### P0-1 — credit on receipt, presentation only for input authorisation

- Dart needs no part in credit any more. Today credit depends on
  `_presentVideo` → `_flushFeedback` (33 ms coalescing, trailing timer) →
  `presented` through the frame worker (L607-647, `pb_mapper_service.dart:288-301`).
  After the change the receive-side cumulative ack lives in Rust
  (`viewer.rs` receive loop, next to the `roundtrip_probe` echo at L225-233).
- Keep `presented` ACKs: host input authorisation (`worker.rs:443-472`) and
  `release_if_stale` still need them. The 33 ms cadence was chosen for the send
  window (comment L617-618); once credit is decoupled it may be relaxed (for
  example ≤ 4-10 Hz), as long as the ACK interval stays well inside the host's
  2 s `recent` window and the stall detector. Coordinate with the host-side map:
  `feedback.stalled()` (`host.rs:202-211`) currently stops a session when ACKs
  stop, which also fires when the viewer is merely backgrounded (§3.8). Decide
  whether stall detection moves to receipt ACKs. Otherwise "Back to picture"
  after > ~4 s away always reconnects.
- `frame_ack_ms` changes meaning or is replaced. `connectionPhase.delayed`
  (`frameAckMs > 500`, L113-115) and the banner's ACK latency
  (`desktop_connection_banner.dart:83-84`) must move to the new metrics
  (network RTT, picture latency).
- Rewrite `desktop_lifecycle_test.dart:98-143` for the new ACK policy. It is also
  CI-skipped; consider making the core of it platform-independent (see the test
  notes at the end).

### P0-2 / P0-3 — host pacing, GOP

No Dart logic, but Dart must stop interpreting phone-side delay as congestion
anywhere (only `delayed` today). Keyframe requests from Dart remain:
`_maintainViewer` stall (700 ms/1 s, L418-425) and native `needKeyframe`
(L565-574). With infinite GOP, any dropped access unit before the decoder needs an
IDR. Today a `submit` error ends the stream and forces a full reconnect
(`_consumeFrames` catch, L669-685). Prefer dropping the frame and requesting an
IDR for recoverable decoder errors.

### P0-4 — targets and quality preference

- `DesktopApi.connect` (L427-447) hardcodes `max_fps` = refresh rate (Android) or
  90, `bitrate: 12000000`, 1920×1080. Replace it with a preference-driven profile
  (Quality / Balanced / Smooth; phone default ≈1080p, ≈4 Mbps, ≤60 fps capped by
  the measured present rate). Rust bounds are fps 30..=120, bitrate 1..40 Mbps
  (`lib.rs:198-207`). A lower fps target than 30 is rejected today.
- `DesktopApiClient.connect` signature changes. Update `DesktopApi`,
  `FakeDesktopApi`, `_StreamingApi`, `_RecoveryApi`, `_SlowConnectionApi`, and the
  acceptance tool. `_target` (L78) must carry the preference so reconnects
  (`_openViewer` L913) reuse it.
- Changing the preference in the Display menu should not reconnect (P0-2:
  resolution changes only on user choice, without flapping). That needs a new
  session op (for example `video_preferences`) and a Rust `Request` variant
  (`deny_unknown_fields`). Add a store method for it.
- "Cap host fps at 90% of measured decode/present fps" needs the present rate to
  reach the host. Either add it to the `presented` op/new op from Dart
  (`presentFps`, L55-59, which is 0 on JPEG and only counts accepted events), or
  measure it natively (Android `presented` counter, `DesktopVideo.kt:200`) and
  report it from Rust if frames go Rust → Kotlin directly.

### P1-1 — phone pipeline (the core change in this scope)

Current serial chain: credit (`DesktopFrameStream._credit`, StreamIterator
pause/resume) → `_consumeFrames` → `await _acceptFrame` → `await texture.submit`
(reply only after MediaCodec accepted, which waits on Flutter consuming the
previous surface frame). Copies per frame: Rust builds the buffer
(`desktop.rs:1268-1271`), `Uint8List.fromList` (`pb_mapper_service.dart:48`),
isolate send, StandardMessageCodec encode, Java `ByteArray` decode, MediaCodec
`buffer.put` (`DesktopVideo.kt:507`). Also per frame: `jsonDecode` in the worker,
`Map.from` + `DesktopStatus.fromJson` on main, `cursorStatus` notify.

**Preferred: Rust → native decoder directly; Dart gets status and presentation only.**
- Delete from Dart: `_desktopFramesWorker`, `_desktopFrameIsolate` (wait path),
  `DesktopFrameStream` (and its test `desktop_lifecycle_test.dart:144-197`),
  `PbMapperService.desktopFrames`, `DesktopApiClient.frames`, `DesktopStore._frames`,
  `_consumeFrames`, the H.264 half of `_acceptFrame` (texture create/reset/submit),
  `DesktopVideoTexture.submit`, the FFI lookups `pbMapperDesktopWaitFrame`
  (and possibly `pbMapperDesktopFrame`, `DesktopBuffer`, `pbMapperDesktopFreeFrame`
  if JPEG also changes), and the test-only pull path in `poll()` (L342-350) with
  `DesktopApiClient.frame`.
- What Dart must still receive: the texture id; per presented frame
  `(generation, frameId)` plus the geometry used for input (`display`
  width/height, video size) — today that is reconstructed from `_videoStatuses`
  (L540-543, L575-584). Either the native `event` carries display geometry, or a
  low-rate status stream supplies `(generation → display)`. Keep the
  `_presentVideo` acceptance rules (monotonic, current generation, < 2 s)
  because they gate `fresh`/`controlling`.
- Where `presented` goes: if native → Rust directly, Dart `_flushFeedback` loses
  the presentation half and keeps keyframe requests. Dart still needs the
  presentation event for `fresh`. Use one source of truth so Dart's "fresh" and the
  host's lease cannot disagree.
- Texture lifetime: today the store creates/disposes the texture per media epoch
  (L517-529, L697-720). With a direct path, Rust needs the native sink handle
  (texture id / JNI object) at connect time. That couples `connect` (new arg or
  op) with `DesktopVideoTexture.create` ordering. The Windows runner path
  (`desktop_video.cpp`) must either get the same direct route or keep the Dart
  route. Decide per platform explicitly.

**Fallback: pipelined submits.**
- `DesktopFrameStream`: `_outstanding` bool → counter; worker `credit` bool →
  int (L59, L65, L105); replace StreamIterator pause coupling (`onPause`/`onResume`
  L123-128) with explicit credits returned when a submit **is issued**, not when it
  completes, bounded at 2-3.
- `_consumeFrames`/`_acceptFrame`: do not await `submit` inline. Keep order
  (single channel, or a serial background task queue on Android) and keep the
  `reset()` barrier before a new generation's IDR (L531-539). Track in-flight
  submits and abandon them on epoch change.
- Android: register the channel with `makeBackgroundTaskQueue()` (Kotlin side,
  `DesktopVideo.kt:24`). Dart is unchanged except for ordering assumptions.
- Use `TransferableTypedData` (or an FFI-owned buffer) for the isolate hop to
  remove one copy.
- Backlog policy (brief): when the native queue overflows today, Kotlin clears it
  and asks for a key (L231-237). Keep "newest decodable frame wins", and report
  backlog/decode fps to the host rather than stalling it.

Either way: keep `_videoStatuses` bounded, keep `_current(epoch)` guards, and
keep `fresh` requiring native consumption.

### P1-2 — delay feedback off the display path

Rust only (`roundtrip_probe` is already echoed in the network task). Dart:
surface the resulting network RTT as a new `SessionStatus` field (add to
`DesktopStatus.fromJson`, L86-119) for the overlay and the phase logic.

### P1-3 — input and cursor independent of video

- `PbMapperService.desktopCall` routes `presented/keyframe/input/control/release`
  into the frame isolate (L288-301), which is blocked in `wait_frame` for up to
  16 ms per loop. Move them to a dedicated long-lived control isolate (not
  `compute`, which spawns per call). Alternatively, add small non-blocking Rust
  exports (plain ints, like `pb_mapper_galaxy_frame_v1`). Do not call
  `pb_mapper_desktop_call_json` on the UI isolate: it `block_on`s a tokio mutex
  that `status`, `connect_viewer` and `disconnect` (during `viewer.stop()`) also
  hold (`desktop.rs:154-166, 614-626`).
- Keep the store-side ordering, epoch, coalescing and `_inFlight`/16 bound
  (L1044-1113). They are transport-independent. The worker's 16-call limit
  (L157) and the store's 16 in-flight limit must stay consistent with the new
  channel.
- Cursor: today `cursorStatus` is fed only from `_acceptFrame` (L473), so host
  cursor updates ride the media queue (`media.rs:70-76`, `push_status`) and
  consume the single credit. Add a non-blocking "latest cursor" FFI read and poll
  it from a viewer `Ticker`/per-frame callback. Then remove the cursor from the
  frame path and delete the `cursorStatus` notify per frame. The gesture surface
  already suppresses remote cursor updates for 500 ms after local motion
  (`desktop_gesture_surface.dart:76-85`). The host-side 300 ms exclusion is Rust.
  `DesktopDisplay` drops `x/y` (L15-20). Verify the cursor coordinate space when
  moving cursor reading.
- Status heartbeats should leave the media path too. The heartbeat branch of
  `_acceptFrame` (L474-489) and the `poll` overwrite (L327) then become the
  single status source.

### P1-4 — zoom/pan stay local

No store API touches the viewport today, and `DesktopApi.connect` reads the
refresh rate once (L429-435). Keep it so: do not add resize/keyboard/rotation
hooks to the store, do not rebuild the texture on layout changes, and keep the
`inputEpoch` record contract (`desktop_viewer.dart:443`) cancelling input without
resetting the transform (test `desktop_gesture_test.dart:115-121`). Re-check
`DesktopViewer._update`'s full-`Scaffold` `setState` per store notify
(`desktop_viewer.dart:80`). In the redesign, only overlays should rebuild at media
rate. The `Texture` subtree should not.

### Regression guards (brief)

- Bounded queues: preserve every bound listed in §5.5. If credits become
  counters, cap them.
- No input replay: keep `_inputEpoch` bumps at every session boundary, and
  `_sequence = 0` at session start (host rejects `sequence <= last`,
  `worker.rs:586-589`).
- Fresh presented frame for input: keep `fresh`/`controlling` semantics and the
  viewer's `_interactive`. If presentation events move native → Rust, Dart must
  still learn about them before enabling input.
- Keep `desktop_recovery_test.dart` intact (it is codec-agnostic and runs in CI).

### Performance issues in this scope beyond the brief (fix with the pipeline)

1. **Perpetual 4 Hz poll.** The 250 ms timer never stops after the first
   `load()`/`connect()`/host restore (L149, L888, L183, L191, L249). Each tick
   spawns a `compute` isolate for `status` (`pb_mapper_service.dart:470-486`)
   and calls `notifyListeners` (L363), which AppStore re-broadcasts
   (`app_store.dart:42`) to the galaxy, tunnels, navigation, banner and tray.
   Poll only while a page or session needs it, slow down when idle, use a
   long-lived worker for status, and notify only on change.
2. **AppStore re-broadcast.** `..addListener(_notify)` only exists so
   `app.dart:324-331` sees `activeDesktop` appear. Notify AppStore once on
   creation and let desktop widgets listen to `DesktopStore` directly.
3. **Viewer rebuild rate.** H.264 media notifies at ≤10 Hz plus 4 Hz polls,
   and each rebuilds the whole viewer (`desktop_viewer.dart:56-92`). JPEG notifies
   every frame.
4. **Frames continue while the viewer is not mounted** (§3.8). The overhaul's
   "Back to picture" needs an explicit policy (pause stream, keep the texture
   alive, or keep presenting offscreen).
5. **Reconnect destroys the texture** (`_clearVideo`, §3.9). The approved
   "Reconnecting" state shows the greyed last frame. That needs texture reuse
   across epochs (with `reset()`) or a captured still.
6. `after`/`generation` arguments of the frame exports are dead
   (`desktop.rs:1258-1261`). Drop them from Dart and the C ABI when touching it.
7. Dead public API to delete: `DesktopStore.startHost`, `DesktopStore.backgroundAction`,
   `videoPresentation` (unused ValueNotifier), and `DesktopApiClient.startHost`
   if no page needs the non-background start. Also delete the `poll()` pull path
   once tests move to the stream.

### Requirement 2 — UI/UX redesign: store/API changes it needs

| Design need | Today | Change |
| --- | --- | --- |
| Six viewer states (`ViewerStates.dc.html`: connecting with steps + Cancel; slow/quality; waiting for remote desktop; reconnecting with greyed last frame, countdown and "Retry now"; identity changed with Disconnect / Paste new invite; view-only) | 9 `DesktopConnectionPhase` values (L6-16); `connect` is one opaque blocking call | Map phases to states. Connecting steps need Rust progress (path found/direct, identity verified, waiting for first frame). `directStage`/`directFailure` exist only once a viewer exists, and the viewer is null during `connect_viewer`. Cancel = `disconnect()` (cancels the Rust connect). Add `retryNow()` (cancel `_retryTimer`, `_openViewer(serial)`). View-only = `snapshot.viewer.controlAllowed == false`. "Input paused" = `!controlling` |
| Connection quality overlay (path, network RTT, picture latency, display fps, bitrate, resolution, codec) | `path`, `directStage`, `bitrate`, `videoWidth/Height`, `codec`, `receiveFps`, `captureFps`, `droppedFrames`, `presentFps`, `frameAckMs`, `pictureAge` | Add RTT (Rust probe), a defined picture latency (for example native `receive_to_present` or host-ready → present), and hardware-decoder info (`stats().decoder`). `presentFps` should come from the native counter, not only accepted events. Overlay must rebuild alone, at ~1 Hz |
| Quality preference (Display menu) | none | §P0-4: preference in `_target`, new runtime op, persisted per computer |
| Shortcut menu (Win+D, Alt+Tab, Ctrl+Shift+Esc, Win, Win+L, refresh picture, release all keys; Ctrl+Alt+Del unsupported) and the mobile modifier bar (sticky Ctrl/Alt/Shift/Win, F1-F12, Ctrl+C/V) | Rust `Key` has no `d`, `l`, F-keys, other letters (`lib.rs:264-287`). An unknown key throws and ends control | Extend Rust `Key` (and host backends) first, then `_keyNames`. Add a store `requestKeyframe()` (today private via `_pendingKeyframe`). Sticky modifiers must be sent as explicit down/up through `send` (barriers) and cleared by `release()`. Combos should be issued as one ordered batch so coalescing never splits them |
| Remembered computers, online status, last path, "Back to picture" | One in-memory `_target` (private); no persistence | New persisted list (DPAPI/Keystore, outside this store or as a sibling store). Expose current target identity (service) publicly. Online status: match the invitation's `service` against `AppStore.relayServices` (namespaces). Last path: record `snapshot.viewer.path` at disconnect |
| Add computer: paste or scan, show recognised host | Validation only inside `connect` (Rust `Invitation::decode`, `INVALID_ARGUMENT`) | Add a cheap validate/describe op (service, version) so the dialog can show the host without connecting. The invitation JSON includes a secret `token`. Never display or log it |
| Invite QR code on the host card | `invitation` string (JSON, certificate up to 4096 bytes) | QR from `invitation`. The design shows a compact `pbd1·…` form, so check payload size or add a compact encoding (Rust-owned). Clear on leaving the page |
| Last-picture thumbnail | Texture has no readback; `frame.bytes` is only the last H.264 AU | Native `capture` method on the texture (Android PixelCopy/ImageReader, Windows D3D readback) before `_clearVideo`, stored locally and deleted with the computer |
| Host viewer info (watching/controlling, direct/relay, connected for) | `snapshot.host` has `state`, `controlling`, `path`, `display`. There is no viewer count or start time | Rust host status fields, then `DesktopStatus.fromJson`. Note `status` reads the host with `try_lock` (`desktop.rs:618-623`) |
| Two permission levels | `allowControl(bool)` → `host_permission` | Keep. Rename UI only |
| Desktop viewer fills the window, rail hidden; Remote desktop becomes a rail page | Viewer and page are pushed `MaterialPageRoute`s (`desktop_page.dart:9-14, 288-300`, `app.dart:124-133`, `tunnels_page.dart:34-37`) | Navigation rewrite in pages/shell. The store already survives navigation. Keep `setViewerForeground` driven by viewer visibility and app lifecycle. Rewrite `desktop_viewer_navigation_test.dart` and `app_test.dart:24-36` |
| Gestures (long-press right click, double-tap-hold drag, three-finger scroll) | Gesture surface (other map); input through `send` | No store change. Wheel stays within ±100 |

### Tests: what changes

- Delete or replace with the pipeline: `desktop_lifecycle_test.dart:144-197`
  (`DesktopFrameStream`) if the worker goes; rework `:98-143` (ACK cadence) and
  `:199-241`, `:243-334` around the new presentation source. They are skipped on
  Linux CI today, so give the freshness/security part (`:243-334`) a
  platform-independent seam (inject a texture/presenter factory instead of the
  static `DesktopVideoTexture.create`) so it runs in CI.
- `desktop_test.dart:98-278` and `desktop_recovery_test.dart:351-385` use the
  `poll()` pull path. Move them to the frames/stream fake when that path is
  deleted.
- Keep the recovery and host-readiness store tests. Their widget parts follow
  the redesign.
- `FakeDesktopApi` must implement every new `DesktopApiClient` member (the
  interface makes omissions compile errors). Give it a working default `frames()`
  (today an empty stream that immediately fails a connected session).
- Update `ui/tool/desktop_viewer_acceptance.dart` to the new viewer widget, new
  metrics (receipt ACKs, RTT, native decode/present fps, backlog drops) and the
  new preference argument. `_AcceptanceApi` counters disappear if feedback leaves
  Dart.
