# Map: remote desktop VIEWER side + desktop FFI (PR #51 head `a41eaa7`)

Path legend used in every citation:

- `D/` = `crates/pb-mapper-desktop/src/`
- `F/` = `ui/native/pb_mapper_ffi/src/`
- `U/` = `ui/lib/src/` (Dart, cited only where it is the other half of an FFI contract)
- `SDK/` = `crates/pb-mapper-client/src/`

Everything below was read from the code at this commit. Nothing was built or run.

---

## 1. Overview

The viewer is `pb_mapper_desktop::Viewer` (`D/viewer.rs`). It opens one owned TCP
session to a registered `desktop:*` service through the pb-mapper SDK
(`Client::connect_tcp`, direct QUIC path first or relay), runs TLS 1.3 pinned to
the certificate carried in the invitation, sends one `Authenticate` command with
the bearer token, codecs and video limits, and then runs ONE tokio task that
joins a receive future (status JSON + media bytes pairs from the host) and a
send future (commands to the host). Received media goes into a bounded
access-unit queue (`D/media.rs`); the embedding app pulls from it.

The FFI crate (`pb-mapper-ffi`, cdylib) owns at most one `Viewer` and one local
`Host` per FFI handle in `DesktopState` (`F/desktop.rs:55-67`). It exposes four C
symbols: one JSON dispatcher (`pb_mapper_desktop_call_json`, 18 ops covering
viewer, local host, Windows helper/background service and DPAPI-remembered host
trust) and three binary-frame functions (`pb_mapper_desktop_frame`,
`pb_mapper_desktop_wait_frame`, `pb_mapper_desktop_free_frame`). Every call is
synchronous: it `block_on`s the handle's multi-thread tokio runtime on the
calling Dart isolate thread.

Today the viewer's credit/feedback path to the host is presentation-driven:
Dart reports `presented(generation, frame_id)` after the platform decoder shows a
frame, the FFI pushes it into a `watch`, and the viewer's send loop writes
`Command::Presented`. The host's `congestion::Feedback` admits new frames only
against those ACKs. That coupling is what P0-1 removes.

There is NO viewer-side persistence today: no remembered computers, no stored
invitations, no thumbnails. The only persistence in this scope is the HOST's
single remembered grant `desktop-trust.dpapi` (Windows DPAPI, `F/desktop_trust.rs`).

---

## 2. Components and responsibilities

### 2.1 `pb-mapper-desktop` (viewer-relevant parts)

| Item | Where | Responsibility |
| --- | --- | --- |
| `Viewer` | `D/viewer.rs:15-26` | Owns: `presented: watch::Sender<(u64,u64)>`, `input: mpsc::Sender<Command>` (cap 32), `priority: Arc<Controls>`, `frames: watch::Receiver<Option<Frame>>`, `media: Arc<media::Queue>`, `keyframe: watch::Sender<u64>`, `status: Arc<parking_lot::Mutex<(SessionStatus, Instant)>>`, `cancel: CancellationToken`, `connection: Arc<Connection>` (SDK tunnel handle), `join: AsyncMutex<Option<JoinHandle>>`. |
| `Controls` / `PendingControls` | `D/viewer.rs:28-79` | Coalesced priority mailbox for control barriers: `revoke`, `release`, `grant` booleans + `Notify`. `push` (`:43-57`): `Control{enabled:false}` sets revoke and clears grant/release; `Control{true}` sets grant; `Release` sets release; any other command is `unreachable!`. `take` (`:58-69`) order: revoke > release > grant. `next` (`:70-78`) awaits. Bounded by construction (3 bits). |
| Constructors | `D/viewer.rs:81-127` | `connect` (JPEG only, no feedback), `connect_with_codecs`, `connect_with_video_preferences` (no feedback), `connect_with_presentation_feedback` (feedback on). All call `connect_inner(.., presentation_feedback)`. |
| `connect_inner` | `D/viewer.rs:128-354` | Validation, tunnel, TLS, `Authenticate`, path sampling, channel setup, spawns the session task. |
| `status()` | `D/viewer.rs:355-363` | Returns the last observed `SessionStatus`; if `state=="streaming"` and the observation is older than 2 s, reports `state="stale"`, `controlling=false`. |
| `presented()` | `D/viewer.rs:366-368` | `watch::send_replace((generation, frame_id))` - cumulative, latest wins. |
| `frames()` / `latest_frame()` | `D/viewer.rs:371-376` | Wake-up watch (latest `Frame`), used by FFI `wait_frame` for change notification and by SDK tests/examples. |
| `next_frame()` | `D/viewer.rs:377-379` | `media.pop()` - ordered, consume-once access units + coalesced status. Required for H.264 (doc `:88-89`). |
| `request_keyframe()` | `D/viewer.rs:381-383` | Bumps the keyframe watch counter. |
| `request_control()` / `release_input()` | `D/viewer.rs:384-401` | Push barrier into `Controls`; after cancel, `Release`/`Control{false}` are `Ok(())` no-ops, grant is an error. |
| `input()` | `D/viewer.rs:402-421` | Local gate: `status()` must be `streaming`, `controlling`, same `generation`. `try_send` into mpsc(32); on full: push revoke barrier, set local `controlling=false`, return "input congestion; control released while video recovers". |
| `stop()` / `Drop` | `D/viewer.rs:422-436` | Cancel, await task, `connection.stop()`. Drop only cancels. |
| `media::Queue` | `D/media.rs:13-110` | Bounded receiver queue. `MAX_QUEUED_FRAMES = 8` (`:10`), `MAX_QUEUED_BYTES = 8 MiB` (`:11`). Details in 3.2. |
| `wire` | `D/wire.rs` | Length-prefixed JSON / bytes framing, `Command` enum, limits `MAX_JSON=8192`, `MAX_FRAME=4 MiB`, `IO_TIMEOUT=3 s` (`:5-7`). |
| `security` | `D/security.rs` | `NAME="desktop.pb-mapper.invalid"` (`:11`), `ALPN=b"pb-mapper/desktop/1"` (`:12`), `HostIdentity` (cert, private key, token) with redacted `Debug` (`:24-102`), `client()` pinned TLS connector (`:109-137`), `Pinned` verifier (`:139-178`). |
| Shared types | `D/lib.rs` | `valid_service` (`:49-56`), `Display` (`:58-67`), `CursorPosition` (`:71-76`), `Capabilities` (`:78-84`), `Invitation` (`:89-123`), `SessionStatus` (`:125-165`), `VideoCodec` (`:167-173`), `VideoPreferences` + `validate` (`:178-208`), `VideoFormat` (`:210-219`), `Frame` (`:238-242`), `Input`/`Button`/`Key`/`InputEvent` (`:246-295`), `Input::validate` (`:377-394`). |

Host-side counterparts the viewer contract depends on (mapped in detail elsewhere):
`D/host.rs:139-295` (accept loop, handshake, per-session receive/send loops),
`D/worker.rs:159-199` (command execution on the capture thread),
`D/worker.rs:443-473` (`Engine::presentation`), `D/worker.rs:586-621`
(`Engine::input`), `D/congestion.rs` (`Feedback`: `start_probe :121`,
`probe_received_at :128-144`, `admit_at :163-230`, `request_keyframe :234-249`,
`presented_at :257-319`, `budget_at :323-371`, `stalled_at :375-380`).

### 2.2 FFI crate

| Item | Where | Responsibility |
| --- | --- | --- |
| `PbMapperHandle` | `F/handle.rs:17-25` | Fields `desktop: DesktopState`, `tunnel_events`, `runtime: tokio::runtime::Runtime`, `state: Arc<tokio::Mutex<PbMapperState>>`, `control_shutdown`. |
| Runtime | `F/handle.rs:39` | `Runtime::new()` = multi-thread runtime, default worker count (= logical CPUs). One per handle. |
| `pb_mapper_destroy` | `F/handle.rs:92-97` | Drops the handle (drops `DesktopState` -> `Viewer::drop`/`Host::drop` cancel). Safety: every call borrowing the handle must have returned. |
| Exports | `F/lib.rs:34-37` | Re-exports the 4 desktop symbols. |
| `DesktopState` | `F/desktop.rs:55-67` | `host: tokio::Mutex<Option<Host>>` (host lifecycle lock, also serialises background/helper management), `viewer: tokio::Mutex<Option<Viewer>>`, `connecting: parking_lot::Mutex<Option<(u64, CancellationToken)>>`, `viewer_epoch: AtomicU64`, `host_identity: tokio::Mutex<Option<LocalIdentity>>`, `background: Arc<parking_lot::Mutex<BackgroundCache>>`. |
| `LocalIdentity` | `F/desktop.rs:68-74` | `scope: trust::Scope`, Windows-only `identity: HostIdentity`, `remembered: bool`. Kept while a local host lives so `host_enable` can hand the same identity to the service. |
| `BackgroundCache` | `F/desktop.rs:24-53` | Last background-service snapshot + `checked` time + `refreshing` + `revision`. `store` preserves a configured snapshot when SCM says Disabled (`:33-47`). |
| `DesktopState` methods | `F/desktop.rs:76-190` | `cache_background`, `background_status` (lazy 2 s refresh via `tokio::spawn`, `:80-107`), `connect_viewer` (`:109-153`), `disconnect_viewer` (`:154-166`), `wait_frame` (`:167-190`). |
| `Request` | `F/desktop.rs:193-255` | serde enum, `tag="op"`, `snake_case`, `deny_unknown_fields`. 18 ops (section 4.3). |
| Background helpers | `F/desktop.rs:260-508` | `background_configured/enabled`, `unavailable_background`, `failed_background`, `background_host`, `query_background(_for_helper)`, `discover_background`, `disable_legacy_startup`, `background_action`, `stop_background`, `revoke_background`, `revoke_trust_with_background`. |
| `pb_mapper_desktop_call_json` | `F/desktop.rs:510-533` | C entry: null-handle check, request <= 16384 bytes, parse `Request` (any parse error -> `INVALID_ARGUMENT "invalid desktop request"`), `runtime.block_on(call(..))`, envelope. |
| `call` | `F/desktop.rs:535-818` | Dispatcher. |
| `start_host` | `F/desktop.rs:820-1058` | Local/remembered/privileged host start, and the `background:true` handoff to the Windows service. |
| `enable_host` | `F/desktop.rs:1063-1204` (non-Windows stub `:1206-1209`) | One-click Windows background enable incl. helper install + app restart. |
| Frame ABI | `F/desktop.rs:1211-1292` | `DesktopBuffer`, `pb_mapper_desktop_frame`, `pb_mapper_desktop_wait_frame`, `export_frame`, `pb_mapper_desktop_free_frame`. |
| `trust` (desktop_trust.rs) | `F/desktop_trust.rs` (included via `#[path]` at `F/desktop.rs:21-22`) | DPAPI-protected single remembered HOST grant: `Scope`, `Settings`, `Saved`, `load/save/revoke`, `protect`, `replace`. |
| Config dir | `F/state/configuration.rs:15-17, 99-127` | `desktop_config_dir()` = `config_dir` = `<app dir>/pb-mapper-ui` when Flutter passes an app dir (always on mobile), else `dirs::config_dir()/pb-mapper-ui` (Windows: `%APPDATA%\pb-mapper-ui`). |
| Envelope | `F/response.rs:32-38, 50-52`; `F/error.rs` | `{"success":true,"data":...}` / `{"success":false,"message":...,"code":...}`. Desktop uses `err()` = `CtlError::io` (`F/desktop.rs:256-258`) -> code `"IO"` for almost everything; `INVALID_ARGUMENT` only for request parse, invitation decode (`:724-725`), unknown background action (`:392`), background consent (`:828-832`), invalid service in `for_background` (`F/desktop_trust.rs:59`). |

### 2.3 Dart half of the FFI contract (for coupling only)

| Item | Where | Notes |
| --- | --- | --- |
| Symbol lookups | `U/ffi/pb_mapper_ffi.dart:200-226` | `DesktopBuffer` struct (`data`, `len`), the 4 symbols. |
| `_desktopFrameIsolate` | `U/ffi/pb_mapper_service.dart:27-53` | Calls `wait_frame` (or `frame`), parses u32 LE length + JSON, copies bytes with `Uint8List.fromList`, frees buffer. |
| `_desktopFramesWorker` | `U/ffi/pb_mapper_service.dart:57-114` | Long-lived isolate. One credit: blocks in `wait_frame` only when it has a credit; on `null` sleeps 2 ms (`:99`); sends frame map to main isolate; ALSO executes routed JSON calls synchronously in its `ReceivePort` listener (`:66-80`). |
| `DesktopFrameStream` | `U/ffi/pb_mapper_service.dart:119-246` | Main-isolate side: credit granted only when not paused and nothing outstanding (`:180-184`); <= 16 pending routed calls (`:157`). |
| Routing | `U/ffi/pb_mapper_service.dart:288-301` | Ops `presented`, `keyframe`, `input`, `control`, `release` go through the frame worker isolate if one accepts calls; everything else through `compute()` (a fresh short-lived isolate per call, `:470-486`). |
| `DesktopApi.connect` | `U/ffi/desktop_api.dart:428-447` | `native_video = Windows || Android`; `video = {max_width:1920, max_height:1080, max_fps: Android refresh clamp(30,90) else 90, bitrate: 12000000}`. |
| `DesktopApi.disconnect` | `U/ffi/desktop_api.dart:450-453` | `stopDesktopFrames()` then op `disconnect`. |
| Consumers | `U/state/desktop_store.dart` | `poll` every 250 ms (`:888`, `:316-365`), fallback `api.frame` via `compute` when no stream (`:342-350`), `_acceptFrame` awaits `texture.submit` per frame (`:544-552`), `_presentVideo` -> `_flushFeedback` throttles `presented` to >= 33 ms apart (`:607-647`, `:619-620`), recovery: keyframe after 700 ms not fresh, reconnect after 3 s (`:414-425`), reconnect backoff 1,2,4,8,15 s (`:452-455`), identity-changed detection by substring (`:432`), INVALID_ARGUMENT stops retry (`:932`), input <= 16 in flight else end control (`:1063-1066`). |

### 2.4 Threading and blocking (who runs where)

- Every FFI export runs `handle.runtime.block_on(...)` on the CALLING OS thread
  (a Dart isolate thread): `F/desktop.rs:529`, `:1230`, `:1254`. `block_on`
  futures run on that thread; anything they `tokio::spawn` runs on runtime
  workers.
- The viewer session is ONE task spawned in `connect_inner` (`D/viewer.rs:210`)
  on the FFI runtime's workers. Receive and send are two futures joined by
  `tokio::select!{biased; send, receive}` (`:322-323`) over
  `tokio::io::split(TlsStream)` (`:211`); they interleave cooperatively and share
  the split lock; there is no separate network-read task.
- Per-message work on that task: JSON parse of `SessionStatus`, a zero-filled
  `Vec` per media message (`D/wire.rs:89-90`), a second copy into `Arc<[u8]>`
  (`jpeg.into()`, `D/viewer.rs:276, 284`), queue push.
- `wait_frame` blocks the Dart frame-worker isolate thread up to 16 ms per call
  (`F/desktop.rs:181`) plus tokio-mutex waits.
- `connect` blocks a `compute` isolate thread for the whole setup: TLS handshake
  timeout 15 s (`D/viewer.rs:147-155`) + auth write 3 s.
- `disconnect` blocks for `Viewer::stop`: TLS `shutdown` (<= 3 s,
  `D/viewer.rs:302`) + drain of remaining bytes (<= 3 s, `:328-332`) +
  `Connection::stop`.
- Host-control blocking work goes to `spawn_blocking`: DPAPI load/save/revoke
  (`F/desktop.rs:376, 417, 487, 592, 638, 704, 887, 922, 945, 1046, 1081, 1114,
  1151, 1200`), `helper::status` SCM queries (`:308, :1126`), display enumeration
  (`:537-544`), and `helper::request_installation` which waits up to 300 s for
  the elevated helper (`D/helper/os.rs:818`) - while the `host` lifecycle lock is
  held (`F/desktop.rs:558-568`, `:1070-1146`).
- Background status refresh: `tokio::spawn` from `background_status` at most
  every 2 s and only when a configured snapshot exists (`F/desktop.rs:84-104`).
- Background-service IPC: named pipe `\\.\pipe\pb-mapper.desktop-background.v1`,
  20 s timeout (`D/helper/background.rs:8-9, 82-98`).

---

## 3. Data / control flow

### 3.1 Connect (FFI op `connect`)

1. Dart `DesktopApi.connect` sends `{"op":"connect","invitation":<json string>,
   "relay_only":bool,"native_video":bool,"video":{...}}` via `compute` isolate
   (`U/ffi/desktop_api.dart:436-446`).
2. `call` -> `Invitation::decode` (`F/desktop.rs:724-725`); failure is
   `INVALID_ARGUMENT` (Dart then drops the target and does not retry,
   `U/state/desktop_store.dart:932-938`). Decode rules (`D/lib.rs:108-122`):
   text <= 8192 bytes, `version == 1`, `valid_service` (`desktop:` prefix, 9..=128
   bytes, `[A-Za-z0-9._:-]`), `token.len() == 43`, `certificate.len() <= 4096`,
   `deny_unknown_fields`.
3. `connect_viewer` (`F/desktop.rs:109-153`): BEFORE any await, takes
   `connecting`; if another attempt is pending -> "desktop connection is already
   pending"; increments `viewer_epoch` and records `(epoch, cancel)`. Then
   `select!{biased; cancel, async { if viewer slot occupied -> "disconnect the
   current desktop first"; connect.await }}`.
4. Inside the attempt (`F/desktop.rs:727-768`): lock app state and read
   `AppConfig` (server address, keep-alive, `relay_transport`); build a NEW
   `Client::from_credential(server, process credential, keep_alive, None)
   .with_relay_transport(config.relay_transport)` (clients of the same relay and
   carrier share the process's relay connections, per `F/relay.rs:201-202`);
   `ConnectOptions{ path: RelayOnly if relay_only else Auto, port_mapping:
   Auto(default) }`; `preferences = video.unwrap_or_default()` (default
   1920x1080 / 90 fps / 12 Mbps, `D/lib.rs:186-195`).
   - `native_video == true` -> `Viewer::connect_with_presentation_feedback(..,
     [H264, Jpeg], preferences)`.
   - else -> `Viewer::connect_with_video_preferences(.., [Jpeg], preferences)`
     (no feedback; host JPEG cadence 125 ms, `D/worker.rs:270-272`).
5. `Viewer::connect_inner` (`D/viewer.rs:128-354`):
   1. `preferences.validate()` (`D/lib.rs:198-207`: width 2..=3840, height
      2..=2160, fps 30..=120, bitrate 1e6..=40e6); codecs 1..=2 entries.
   2. Re-encode/decode the invitation so SDK callers get the same validation
      (`:141`).
   3. `security::client(&invitation.certificate)` (`:142`).
   4. `client.connect_tcp(invitation.service, options)` (`:143-146`,
      `SDK/sdk/client.rs:351-447`): binds `127.0.0.1:0`, connects to itself,
      accepts exactly that peer, sets nodelay on both, spawns a tunnel worker
      that forwards the accepted loopback socket over the direct QUIC path
      (`PathPolicy::Auto`, with p2p) or the relay (TCP or QUIC carrier). Returns
      immediately with `(TcpStream, Connection)`; path setup happens as the first
      bytes (the TLS ClientHello) flow.
   5. TLS handshake over that loopback stream, SNI `desktop.pb-mapper.invalid`,
      15 s timeout (`:147-155`). The pinned verifier fails with
      `rustls::Error::General("desktop identity changed")` if the presented leaf
      differs from the invitation's certificate or any intermediate is present
      (`D/security.rs:153-155`). That text is what Dart matches
      (`U/state/desktop_store.dart:432`).
   6. Write `Command::Authenticate{token, codecs, video: Some(preferences),
      presentation_feedback}` with 3 s timeout (`:156-169`). The token is sent
      only after the pin succeeded.
   7. Sample transport diagnostics ONCE (`:173-188`): `path = "direct"` iff
      `diagnostics.direct.active_direct > 0` else `"relay"`;
      `direct_failure = direct.last_fallback`; `direct_stage` = the stage of the
      newest `TunnelEventKind::DirectFallback` event. These are stamped onto every
      received status (`:236-238`) and never re-sampled (the SDK never reconnects
      or re-paths an owned stream, `SDK/sdk/client.rs:347-350`).
   8. Create channels (`:197-209`): `frames` watch, `media` queue, `keyframe`
      watch (counter), `input` mpsc(32), `Controls`, `presented` watch `(0,0)`,
      optional `probe` watch (ONLY when `presentation_feedback`, `:206`),
      `CancellationToken`.
   9. Spawn the session task (`:210-341`) and return `Ok(Viewer)`.
      **`connect` succeeds before the host accepted the token**: a wrong token
      or a busy host (single-viewer semaphore, `D/host.rs:165`) just closes the
      stream later; the viewer task ends with a transport error and
      `status().state == "stopped"`. There is no "denied" vs "busy" signal on
      the wire (`D/host.rs:172` drops the socket silently).
6. Back in `connect_viewer` (`F/desktop.rs:135-152`): clear `connecting` if it
   is still our epoch; on success re-lock the viewer slot; if cancelled, epoch
   moved, or slot occupied -> `viewer.stop()` and "desktop connection
   cancelled"; else store the viewer.
7. Dart then sets `snapshot.viewer = connecting`, opens the frame stream
   (`U/state/desktop_store.dart:913-928`).

### 3.2 Receive loop (`D/viewer.rs:212-292`)

Per iteration, with ONE timeout `IO_TIMEOUT + 1 s = 4 s` covering both parts
(`:217-224`):

1. `read_json::<SessionStatus>` (<= 8192 bytes) then `read_bytes` (<= 4 MiB).
   Any error or timeout ends the session.
2. Probe echo (`:225-235`): if `bytes` is empty and `roundtrip_probe` is
   `Some(nonce)` -> if a probe watch exists, `send_replace(nonce)`; `continue`
   (never published, never a frame). Without feedback the probe is silently
   dropped.
3. Stamp local transport facts: `path`, `direct_stage`, `direct_failure`
   (`:236-238`) - host values are always overwritten.
4. Validate (`:239-251`): `display` width/height in 1..=32768; for H.264
   `video.width/height` non-zero and <= the preferences the viewer sent. Failure
   ends the session ("invalid display geometry" / "invalid video geometry").
5. Receive fps (`:252-260`): counts non-empty, non-`duplicate` frames; window
   >= 1 s; recomputed only when a message arrives; written into
   `state.receive_fps`.
6. `state.dropped_frames += incoming.dropped()` (`:261`) - host's own dropped
   count (host sets it from its outgoing queue, `D/worker.rs:252`) plus the
   viewer queue's cumulative drops.
7. `state == "stopped"` -> session error with host message or "Desktop host
   stopped the session" (`:262-268`).
8. Status-only streaming message (`bytes` empty, `state == "streaming"`,
   `:269-280`): `incoming.push_status(state)`; only if accepted (same generation
   as the queue, `D/media.rs:85-93`) update `observation.0` (NOT the freshness
   timestamp) and `frames_tx.send_replace(Frame{status, bytes: []})`. Otherwise
   discarded entirely.
9. Everything else (video, or any non-streaming status such as `connecting`,
   `capture_unavailable`) (`:281-290`): `observation = (state, now)` (refreshes
   freshness), build `Frame`, `incoming.push(frame.clone())`, if
   `incoming.take_keyframe_request()` bump the keyframe watch (the send loop
   turns it into `Command::Keyframe`), `frames_tx.send_replace(Some(frame))`.

`media::Queue::push` (`D/media.rs:30-69`):

- Generation change or EMPTY bytes (any non-streaming status) clears frames,
  status and bytes, adopts the generation, sets `wait_keyframe = predictive`
  (H.264 with bytes), clears `requested` (`:33-40`).
- Records `cursor` and copies it into a pending status (`:41-44`).
- Overflow: `frames.len() >= 8 || bytes + new > 8 MiB` -> counts all queued as
  dropped, clears, `wait_keyframe = predictive`, `requested = false` (`:47-54`).
- While waiting for an IDR, a predictive non-key frame is dropped (+1) and
  requests a keyframe once (`:55-62`).
- A keyframe or non-predictive frame clears the wait (`:63-66`); push.
- `pop` (`:70-82`): a pending coalesced status is returned FIRST (empty bytes),
  then the oldest frame with its cursor overwritten by the newest cursor (`:80`).
- `resynchronize` (`:100-109`) exists but the viewer never calls it (only the
  host's outgoing queue does).

### 3.3 Frame delivery to Dart

`pb_mapper_desktop_wait_frame(handle, _after, _generation)` ->
`DesktopState::wait_frame` (`F/desktop.rs:167-190`):

1. Lock the viewer slot (tokio mutex). No viewer -> `None` (null).
2. `changes = viewer.frames()`; `borrow_and_update()` marks the current value
   seen; `next_frame()`; if `Some` -> return it.
3. Remember `viewer_epoch`, RELEASE the lock, wait on `changes.changed()` for at
   most **16 ms** (`:181`). Timeout or sender dropped -> `None`.
4. Re-lock; if `viewer_epoch` changed -> `None`; else `next_frame()` once (may
   still be `None` if the change was consumed already).

`pb_mapper_desktop_frame` (`F/desktop.rs:1220-1240`) is the non-waiting variant
(lock + `next_frame`). `_after` and `_generation` are ignored by both
(`:1257-1261` explains: filtering by id would drop status events that share the
next picture's frame id or break the H.264 reference chain).

`export_frame` (`F/desktop.rs:1257-1278`) allocates ONE boxed slice:
`u32 LE metadata length | SessionStatus JSON | media bytes`, boxed
`DesktopBuffer{data,len}`; `pb_mapper_desktop_free_frame` (`:1280-1292`)
reconstructs both boxes. JSON serialisation failure -> null.

Dart side: the frame worker isolate calls `wait_frame` only while it holds a
credit; on null it sleeps 2 ms and loops; on a frame it copies the bytes
(`Uint8List.fromList`), frees the buffer, sends the map to the main isolate
(another copy across isolates) and drops its credit. The main isolate re-grants
the credit only after `_acceptFrame` returns (which awaits the MethodChannel
`texture.submit`), because `StreamIterator` pauses the subscription
(`U/ffi/pb_mapper_service.dart:120-133, 180-184`). Net: strict stop-and-wait per
frame across Rust -> frame isolate -> main isolate -> platform thread.

Copies of one access unit on the Android/Windows path today: TLS read into a
zero-filled `Vec` (1) -> `Arc<[u8]>` (2) -> `export_frame` boxed slice (3) ->
`Uint8List.fromList` in the frame isolate (4) -> isolate `SendPort.send` (5) ->
MethodChannel to platform (6+).

Fallback: when Dart has no frame stream, `poll()` calls `api.frame` ->
`pb_mapper_desktop_frame` via `compute` (a fresh isolate per call,
`U/ffi/pb_mapper_service.dart:303-316`, `U/state/desktop_store.dart:342-350`).

### 3.4 Calls flowing back to the host

All go: Dart -> (frame-worker isolate if present, else `compute` isolate) ->
`pb_mapper_desktop_call_json` -> `block_on(call)` -> lock viewer slot ->
synchronous `Viewer` method -> channel -> session send loop -> `write_json` +
flush with 3 s timeout (`D/viewer.rs:317-319`) -> host.

| FFI op | Viewer method | Channel | Wire `Command` | Host effect |
| --- | --- | --- | --- | --- |
| `presented{generation,frame_id}` | `presented` (`D/viewer.rs:366`) | `watch` latest | `Presented{generation,frame_id}` | `feedback.presented` (retire sent ledger, credit, delay sample) and `Engine::presentation` (frame must be current generation and captured < 2 s ago, else backend release + "stale presentation"; updates `frame_ack_ms`; while controlling calls `backend.presented`, which for the helper renews held input, `D/worker.rs:182-190, 443-473`, `D/helper/client.rs:170-175`, `D/helper/worker.rs:298-300, 365-369`). |
| `keyframe` | `request_keyframe` (`:381`) | `watch` counter | `Keyframe` | If `feedback.request_keyframe()` returns true (not already recovering) -> outgoing resynchronize + force IDR (`D/worker.rs:172-181`). Repeat requests during recovery are ignored (`D/congestion.rs:234-238`). |
| `control{enabled}` | `request_control` (`:384`) | `Controls` | `Control{enabled}` | `Engine::control` - grant requires host policy; grant/revoke invalidate generation (new IDR, new generation). |
| `release` | `release_input` (`:387`) | `Controls` | `Release` | `backend.release()`. |
| `input{event}` | `input` (`:402`) | mpsc(32) | `Input{event}` | `Engine::input`: strictly increasing `sequence`, view-only check, `streaming`, generation match, `frame_id` captured < 2 s ago, then `backend.input_in_context`; failure ends the grant with sticky `input_failed:` message (`D/worker.rs:586-621`). |
| (internal) heartbeat | - | 1 s interval (`:294`) | `Ping` | keeps the host's 3 s per-command read timeout alive (`D/host.rs:180`). |
| (internal) probe echo | - | probe `watch` | `Probe{nonce}` | `feedback.probe_received(nonce)` before the worker (`D/host.rs:181-184`); host accepts only nonce 1, once (`D/congestion.rs:121-144`). |
| (internal) queue overflow | - | keyframe `watch` | `Keyframe` | as above. |

Send loop priority (`D/viewer.rs:296-316`, `biased`): stop > control barrier
(and on ANY barrier, drain and discard every queued input: `while
inputs.try_recv().is_ok() {}`, `:304`) > keyframe > probe echo > presented >
heartbeat > input. Writes are serialised in this one future.

Dart-side throttles: `presented` at most every 33 ms, always flushing the last
one with a timer (`U/state/desktop_store.dart:617-634`); keyframe request after
700 ms without a fresh picture, at most 1/s (`:418-425`).

### 3.5 Cursor path (today)

Host worker samples the pointer every 16 ms (`D/worker.rs:206-220`) and, on
change, the host send loop writes a status-only message (empty bytes) in the
SAME ordered TLS stream as video (`D/host.rs:213-226`); it is also stamped into
queued pictures (`D/host.rs:227-235`). Viewer: status-only streaming message ->
`push_status` (coalesced) -> wake watch -> `wait_frame` -> `pop` returns the
status ahead of queued video -> Dart `_acceptFrame` sets `cursorStatus`
(`U/state/desktop_store.dart:473`). So cursor updates (a) are head-of-line
blocked behind any media message being transmitted, (b) wait behind the Dart
frame worker's single credit, i.e. behind `texture.submit` of the previous
frame. Covered by `tests/session.rs:207-347`.

### 3.6 Status poll and staleness

Op `status` (`F/desktop.rs:614-626`) returns `{host, viewer, background}`:
`viewer = viewer slot lock .await -> Viewer::status()`; `host` uses
`host.try_lock()` (never waits for a slow helper/background operation) and falls
back to the background snapshot's host (`background_host`, `:286-303`, which
synthesises `state:"waiting"` when the service owns an enabled grant without a
live session). Dart polls this every 250 ms through a fresh `compute` isolate.

Viewer states the app can observe: `connecting` (local placeholder,
`D/viewer.rs:190-196`), host-sent `connecting` / `streaming` /
`capture_unavailable` (`D/worker.rs:100-106, 484, 492, 548`), host-sent
`stopped` with message (renderer ACK stall: "Renderer stopped acknowledging
presented frames; reconnect the viewer", `D/host.rs:202-212`), derived `stale`
(streaming but no real frame or non-streaming status for > 2 s,
`D/viewer.rs:358-361`), local `stopped` after task end with the error text
(`:334-340`).

### 3.7 Disconnect, teardown, reconnect, generations

- Op `disconnect` -> `disconnect_viewer` (`F/desktop.rs:154-166`): bump
  `viewer_epoch` and cancel any pending attempt under the `connecting` lock, then
  `if let Some(viewer) = self.viewer.lock().await.take() { viewer.stop().await }`.
  Under edition 2024 `if let` scoping the guard lives through the then-block, so
  **the viewer slot stays locked for the whole `stop()`** (up to ~6 s worst
  case); `status`, `wait_frame`, `input`, `presented`, etc. queue behind it.
- `Viewer::stop` (`D/viewer.rs:422-430`): cancel -> send future completes
  `write.shutdown()` (TLS close_notify, <= 3 s) -> if clean, drain the read half
  to a sink for <= 3 s (`:324-333`) -> mark `stopped`, `controlling=false`,
  message on error -> `connection.stop()`.
- If the session fails on its own (host stop, timeout, validation), the task
  ends and marks `stopped`, but the SDK `Connection` stays alive in the slot
  until the app calls `disconnect`.
- Reconnect is entirely Dart's (`U/state/desktop_store.dart:428-469, 893-950`):
  always `disconnect` then `connect`, backoff 1,2,4,8,15 s, reset after 3 s of
  freshness; identity mismatch (substring "desktop identity changed") stops
  retries; `_sequence = 0`, `_inputEpoch++` per attempt.
- Input is never replayed across sessions: inputs live in the old `Viewer`'s
  mpsc which is dropped; Dart bumps `_inputEpoch`; the host's new `Worker`
  starts with `Engine.sequence = 0` and requires a fresh frame id.
- Generations are HOST-defined (`Engine::invalidate` on desktop context change,
  control grant/revoke and capture failure, `D/worker.rs:392-401`). Viewer
  queue resets on generation change and (H.264) waits for an IDR; Dart resets
  the texture on generation change (`U/state/desktop_store.dart:531-539`).
- `viewer_epoch` is only an FFI guard so a stale `wait_frame` never returns a
  replaced viewer's frame and a cancelled connect is not stored.

### 3.8 Host-control flows (FFI)

- `capabilities` (`F/desktop.rs:536-545`, `spawn_blocking`): `{capabilities,
  helper, displays, message}`. `capabilities.host` is `cfg!(windows)`
  (`D/platform.rs:3-15`); Android is viewer-only.
- `host_start` -> `start_host` (`F/desktop.rs:820-1058`):
  - `background:true` requires `privileged && remember` else INVALID_ARGUMENT
    (`:821-835`); non-Windows error.
  - Takes the host lifecycle lock first (`:838`); refuses if a host exists.
  - Queries the background service (fails closed when unknown) and refuses a
    local start if the service owns a configured desktop (`:842-859`).
  - Background handoff (Windows, `:866-926`): encode the process credential,
    `disable_legacy_startup`, reuse a matching legacy identity
    (`configure_with_identity`) or let the service generate one (`configure`),
    cache snapshot, clear `host_identity`, delete `desktop-trust.dpapi`, return
    `{invitation, background}`.
  - Local host (`:927-1057`): `Scope{dir, relay = server address, namespace =
    credential.key_id()}`; a new `Client`; load saved trust if remembering or
    restoring; `auto_start` requires `remember`; `remember` requires Windows;
    reuse the saved identity when scope and service match, else
    `HostIdentity::generate()`; `privileged` -> `helper::status().ensure_ready()`
    then `Host::start_with_backend_and_identity(.., helper::backend(display_id))`,
    else `Host::start_with_identity`; persist `Saved` (on failure stop the host
    and fail); return `{invitation}`.
- `host_restore` (`:636-651`): if the saved grant is a background handoff,
  `enable_host(.., allow_install=false)`; else `start_host(HostRestore)` which
  requires a saved grant in the same relay+namespace.
- `host_enable{service, display_id}` -> `enable_host` (Windows,
  `:1063-1204`): lock; load previous trust; on resume require a pending
  background+auto_start grant for this scope; build
  `Saved::for_background(..)` (reuses the live local identity if the running host
  publishes the same service, else the matching saved identity, else new) and save
  it as the pending handoff; stop the local host; if the helper is not ready:
  `request_installation("install"|"enable")` (elevated, <= 300 s); on failure
  roll the pending record back; after `install` -> `prepare_application_restart`
  and return `{"restart": true}`; else `configure_with_identity` with
  **hardcoded `allow_control: true, allow_direct: true, auto_start: true`**
  (`:1183-1189`; the pending record also hardcodes them,
  `F/desktop_trust.rs:72-80`), delete the legacy file, return
  `{restart:false, invitation, background}`.
- `host_stop{on_exit}` (`:655-674`): stop and drop the local host and
  `host_identity`; with `on_exit` stop there (UI exit never touches the
  service-owned host); otherwise also clear legacy `auto_start` and stop the
  background service if configured and not SCM-disabled.
- `host_permission{allow_control}` (`:675-717`): no local host -> forward to the
  background service if enabled, else "desktop is not shared". With a local host:
  revoke first (if lowering), update the remembered record when it is the same
  scope and service, then set.
- `helper_action{action}` (`:546-581`): `open` is rewritten to `install`;
  `enable`/`install` refused while a local host exists; runs
  `request_installation` (`install|enable|disable|uninstall`,
  `D/helper/os.rs:786-789`) under the host lock; `install` also prepares the
  restart; returns `{restart}`.
- `background_action{action}` (`:387-433`): `status` (discover, never blocks
  configure ordering thanks to `revision`), `start` (refused while a local host
  exists), `stop`, `revoke` (deletes legacy trust first, then service revoke).
- `trust_status` (`:584-610`): `{supported, saved: metadata|null,
  matches_relay}`.
- `trust_revoke` (`:611-613`) -> `revoke_trust_with_background`
  (`:474-508`): under the host lock: lower control, delete
  `desktop-trust.dpapi`, then `background::revoke_scope(server, namespace)`,
  stop the local host, clear identity. Invitations are revoked only by
  discarding the identity (cert + token): a new start generates a new identity,
  so every holder of the old invitation will hit "desktop identity changed".

---

## 4. Contracts

### 4.1 C ABI (all `#[unsafe(no_mangle)] extern "C"`)

| Symbol | Signature | Returns | Notes |
| --- | --- | --- | --- |
| `pb_mapper_desktop_call_json` | `(*mut PbMapperHandle, *const c_char) -> *mut c_char` | JSON envelope C string, free with `pb_mapper_free_string` | request <= 16384 bytes; null handle -> `INVALID_ARGUMENT "handle is null"`. |
| `pb_mapper_desktop_frame` | `(*mut PbMapperHandle, u64 _after, u64 _generation) -> *mut DesktopBuffer` | null = nothing queued | non-blocking (still `block_on` for the tokio lock). |
| `pb_mapper_desktop_wait_frame` | same | null after <= 16 ms without a frame | "never call this on the UI thread" (`F/desktop.rs:1243`). |
| `pb_mapper_desktop_free_frame` | `(*mut DesktopBuffer)` | - | null-safe. |

`#[repr(C)] struct DesktopBuffer { data: *const u8, len: usize }`
(`F/desktop.rs:1214-1218`); payload `u32 LE n | n bytes SessionStatus JSON |
media bytes` (JPEG image or one H.264 Annex-B access unit as the host's encoder
produced it). A returned buffer stays valid after disconnect until freed
(test `F/desktop.rs:1725-1812`).

### 4.2 JSON envelope

Success: `{"success": true, "data": <value>}` (`F/response.rs:50-52`); ops with
nothing to return answer `data: {}` (`F/desktop.rs:817`). Failure:
`{"success": false, "message": <sentence>, "code": "NOT_FOUND" |
"ALREADY_EXISTS" | "ALREADY_IN_PROGRESS" | "INVALID_ADDRESS" |
"INVALID_ARGUMENT" | "SERVER_UNREACHABLE" | "ADDRESS_IN_USE" | "TIMEOUT" |
"PROTOCOL" | "IO" | "INTERNAL"}` (`F/response.rs:32-38`, `F/error.rs`). Dart
throws `DesktopCallException(code, message)` (`U/ffi/desktop_api.dart:346-358`).

### 4.3 Ops of `pb_mapper_desktop_call_json` (`F/desktop.rs:193-255`)

`Request` is `deny_unknown_fields`; defaults noted.

| `op` | Request fields | `data` on success |
| --- | --- | --- |
| `capabilities` | - | `{capabilities:{host,viewer,secure_desktop,mode}, helper: HelperStatus|null, displays:[Display]|null, message:string|null}`; `HelperStatus` = `{installed, running, startup_disabled, authorized_application, identity_verified, installed_application, message}` (`D/helper/mod.rs:63-75`), Windows only. |
| `status` | - | `{host: SessionStatus|null, viewer: SessionStatus|null, background: BackgroundSnapshot|null}` |
| `trust_status` | - | `{supported: bool, saved: {relay, service, display_id, allow_control, relay_only, privileged, auto_start, background}|null, matches_relay: bool}` (`F/desktop_trust.rs:104-109`) |
| `trust_revoke` | - | `{}` |
| `host_restore` | - | as `host_enable` (background) or `{invitation}` (local) |
| `host_enable` | `service: string, display_id: u32` | `{restart: true}` or `{restart: false, invitation, background}`; non-Windows error |
| `helper_action` | `action: "install"|"enable"|"disable"|"uninstall"|"open"` | `{restart: bool}` |
| `background_action` | `action: "status"|"start"|"stop"|"revoke"` | `BackgroundSnapshot` |
| `host_start` | `service, display_id, allow_control`, `relay_only=false, privileged=false, remember=false, auto_start=false, background=false` | `{invitation}` or `{invitation, background}` |
| `host_stop` | `on_exit=false` | `{}` |
| `host_permission` | `allow_control: bool` | `{}` |
| `connect` | `invitation: string`, `relay_only=false`, `native_video=false`, `video: {max_width,max_height,max_fps,bitrate}|null` (`VideoPreferences` is `#[serde(default, deny_unknown_fields)]`) | `{}` |
| `disconnect` | - | `{}` |
| `control` | `enabled: bool` | `{}` |
| `release` | - | `{}` |
| `keyframe` | - | `{}` |
| `presented` | `generation: u64, frame_id: u64` | `{}` |
| `input` | `event: {generation, frame_id, sequence, input}` | `{}` |

`input.input` (`D/lib.rs:246-287`, `tag="kind"`, `deny_unknown_fields`):
`{"kind":"move","x":u32,"y":u32}`, `{"kind":"button","button":"left"|"right"|"middle","down":bool}`,
`{"kind":"wheel","horizontal":i32,"vertical":i32}`,
`{"kind":"key","key":<Key>,"down":bool}`, `{"kind":"text","text":string}`.
`Key` (snake_case) = `shift, control, alt, meta, enter, escape, tab, backspace,
delete, left, right, up, down, home, end, page_up, page_down, a, c, v, x, z`.
Host-side bounds (`D/lib.rs:377-394`): move inside the display's physical
pixels, text <= 1024 bytes without NUL, |wheel| <= 100 per axis.

`BackgroundSnapshot` (`D/helper/background.rs:42-53` + FFI additions):
`{configured, enabled, host: SessionStatus|null, invitation: string|null,
settings: {service, display_id, allow_control, allow_direct, auto_start}|null,
server, namespace, message}`; FFI may add `service_disabled: bool`
(`F/desktop.rs:45, 282, 335`) and `supported` in `unavailable_background`
(`:268-272`).

### 4.4 `SessionStatus` JSON (wire metadata, FFI metadata, `status` op)

`D/lib.rs:125-165`, NOT `deny_unknown_fields` (additive fields are safe):

```
state: string            message: string
control_allowed: bool    controlling: bool
generation: u64          frame_id: u64
display: {id,name,x,y,width,height}|null   (physical desktop pixels)
path: string             (viewer-local: "direct"|"relay")
direct_stage?: string    direct_failure?: string   (viewer-local)
frames: u64  bytes: u64  (host totals)
desktop: string          ("ordinary" or the helper's desktop name)
video: {codec:"jpeg"|"h264", width, height, keyframe, pts_us, duplicate}
capture_fps: f32  receive_fps: f32 (viewer-local)
dropped_frames: u64      (host + viewer queue)
bitrate: u32             (host's current target)
cursor?: {x:i32, y:i32, visible}   (relative to the selected display)
frame_ack_ms: f32        (host: frame-ready -> presented-ACK round trip)
roundtrip_probe?: u64    (host->viewer only; stripped by the viewer)
```

### 4.5 Wire protocol (inside TLS, inside the tunnel)

- Transport: SDK owned TCP session (loopback socketpair on both ends,
  `SDK/sdk/client.rs:373-386`, `D/host.rs:92`), carried over direct QUIC or the
  relay (TCP or QUIC carrier). One ordered byte stream per session.
- TLS: rustls, ring provider, TLS 1.3 only, ALPN `pb-mapper/desktop/1`, SNI
  `desktop.pb-mapper.invalid`, no client auth, early data disabled both ways
  (`D/security.rs:90-100, 123-136`). Client trusts exactly the invitation's DER
  certificate: equality check + empty chain + WebPKI verification against a root
  store containing only that self-signed cert (`:109-158`).
- Framing: `u32 BE length | payload` (`D/wire.rs:65-92`), JSON <= 8192,
  media <= 4 MiB, each write flushed.
- Viewer -> host: one JSON `Command` per frame (`D/wire.rs:9-37`, `tag="op"`,
  `snake_case`, `deny_unknown_fields`):
  - `{"op":"authenticate","token":s,"codecs":["h264","jpeg"],"video":{...},"presentation_feedback":true}`
    (`codecs` default `[]`; `video` omitted when `None`; `presentation_feedback`
    omitted when false)
  - `{"op":"control","enabled":b}`, `{"op":"input","event":{...}}`,
    `{"op":"release"}`, `{"op":"ping"}`, `{"op":"probe","nonce":u64}`,
    `{"op":"keyframe"}`, `{"op":"presented","generation":u64,"frame_id":u64}`.
- Host -> viewer: always a PAIR `JSON(SessionStatus)` + `bytes` (possibly empty).
  Empty bytes = status/cursor/liveness/probe; non-empty = one JPEG or one H.264
  access unit.
- Timeouts: viewer read 4 s per pair; viewer write 3 s per command; host read 3 s
  per command; host write 3 s per pair; host handshake 3 s; host liveness status
  every 1 s when idle/waiting with feedback (`D/host.rs:246-257`); viewer `Ping`
  every 1 s.
- Compatibility: every viewer->host type is `deny_unknown_fields`. A new
  `Command` variant, a new `Authenticate` field serialised to an old host, a new
  `Key`/`Input` variant, or a new `VideoPreferences` field makes an old host
  fail `read_json` with "invalid desktop message" and end the session
  (`D/host.rs:157, 180`). Only `SessionStatus` (host->viewer) is additive.

### 4.6 Invitation

`{"version":1,"service":"desktop:...","certificate":<base64url DER, no pad>,"token":<43-char base64url of 32 random bytes>}`
(`D/lib.rs:89-123`, `D/security.rs:14-19, 60-67`). It is the bearer view grant
and the host's pinned identity. It carries no relay address or namespace ("Both
devices need access to the same relay namespace"). Size is dominated by the
certificate (rcgen self-signed, roughly 0.5-0.7 KB of base64; estimate, not
measured).

### 4.7 Persistence (host trust only)

`<desktop_config_dir>/desktop-trust.dpapi` (`F/desktop_trust.rs:111-113`):
DPAPI CurrentUser blob (`CryptProtectData`, description `pb-mapper desktop
trust`, entropy `pb-mapper/desktop-trust/v1`, `CRYPTPROTECT_UI_FORBIDDEN`,
`:179-214`) of JSON `Saved{schema:1, relay, namespace, settings: Settings{service,
display_id, allow_control, relay_only, privileged, auto_start, background},
identity: [u8] (HostIdentity JSON {certificate, private_key, token})}`
(`:21-42`). Limits 64 KiB cipher and plain (`:11`); `load` validates schema,
relay <= 1024, service prefix/length, and decodes the identity
(`HostIdentity::decode_secret` <= 16384 bytes + builds the acceptor,
`D/security.rs:51-59`). `save` writes `desktop-trust-<32 hex>.tmp` with
`create_new` + `sync_all`, then `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)`
(`:141-171, 221-239`). Non-Windows: `protect`/`replace` always fail, so
`remember`/`host_enable` are Windows-only. The service-owned grant lives
elsewhere (`background-host.dpapi` and `background-host.revoked` under the
helper's install root, `D/helper/background.rs:10-11`).

Viewer-side storage: none.

---

## 5. Invariants and security boundaries

1. The bearer token leaves the viewer only after TLS verified the pinned
   certificate (`D/viewer.rs:142-169`); the host compares SHA-256 digests in
   constant time (`D/host.rs:160-162`). `Invitation` and `HostIdentity` `Debug`
   are redacted (`D/lib.rs:97-103`, `D/security.rs:31-35`); background
   `Options` redacts the credential (`D/helper/background.rs:31-40`).
2. Relay credentials are never written to `desktop-trust.dpapi`; only relay
   address and namespace id (`F/desktop_trust.rs:1-3, 90-96`). The background
   handoff passes the credential to the service over verified IPC
   (`D/helper/background.rs:82-87`).
3. Transport facts (`path`, `direct_stage`, `direct_failure`) are local
   observations, always overwritten on receipt (`D/viewer.rs:236-238`).
4. Input authority requires: host policy `allow_control`, a viewer `Control`
   grant, `streaming`, generation match, `frame_id` captured by the host within
   2 s, strictly increasing `sequence` (`D/worker.rs:586-604`). Held input is
   released when no presentation/input renewed it within 2 s
   (`D/worker.rs:402-414`; helper `D/helper/worker.rs:298-300`). The viewer adds
   a local pre-check (`D/viewer.rs:402-409`) and Dart only sends events against
   the PRESENTED frame (`U/state/desktop_store.dart:1044-1087`).
5. Control barriers are coalesced state, never a queue; a revoke always reaches
   the host before a later grant; every barrier discards queued input
   (`D/viewer.rs:28-79, 304`). Input congestion revokes control instead of
   dropping a key-up (`:410-418`). No input is replayed after reconnect (3.7).
6. Bounded memory: viewer media queue 8 frames / 8 MiB, input mpsc 32,
   `Controls` 3 bits, every other channel a `watch` (latest value); FFI request
   16 KiB; JSON 8 KiB; media 4 MiB; Dart frame worker 1 outstanding frame and
   <= 16 routed calls; Dart input <= 16 in flight.
7. H.264 integrity: a dropped predictive frame invalidates the rest of the GOP;
   the queue discards until an IDR and requests one (`D/media.rs:1-2, 47-62`).
   Only ONE consumer may pop video in order (`D/viewer.rs:88-89`; reason the FFI
   ignores `_after`).
8. One viewer slot and one pending connect per FFI handle; disconnect cancels a
   pending connect before any network work (`F/desktop.rs:113-124`); a cancelled
   or superseded connect is stopped, not stored (`:141-150`).
9. Host lifecycle (local host, background service, helper, DPAPI handoff) is
   serialised by `DesktopState.host`; `status` never waits on it
   (`F/desktop.rs:615-623`); stale background reads cannot overwrite a newer
   configure/stop/revoke (`revision`, `:352-372`); an unknown service state fails
   closed for local starts (`:338-347`).
10. Background hosting requires explicit `privileged && remember` consent
    (`F/desktop.rs:828-832`); UI exit (`on_exit`) never touches the service host.
11. Revoking trust deletes the local record before revoking the service scope
    so a restart cannot resurrect a grant (`F/desktop.rs:478-498`).
12. Ctrl+Alt+Del: no `Key`/`Input` can express SAS; stays unsupported.
13. Handle lifetime: any thread borrowing the raw handle must return before
    `pb_mapper_destroy` (`F/handle.rs:89-91`); Dart counts the frame worker as an
    active call (`U/ffi/pb_mapper_service.dart:264-283`).

---

## 6. Tests covering this scope

Rust, viewer/protocol (`cargo test -p pb-mapper-desktop`):

- `D/viewer.rs:442-462` `repeated_releases_are_bounded_and_revocation_precedes_a_new_grant`.
- `D/media.rs:132-151` `overflow_discards_dependent_frames_until_idr`;
  `D/media.rs:152-196` `status_is_coalesced_without_reordering_or_invalidating_video`
  (asserts cursor stamping on popped video).
- `D/wire.rs:116-154` `clean_tls_close_is_eof_only_between_messages`.
- `D/tests.rs:15-37` `tls_rejects_an_unpaired_host_identity`;
  `:76-173` `blocked_video_does_not_block_local_stop`;
  `:175-200` `older_authentication_defaults_video_limits_and_rejects_unbounded_preferences`.
- `D/host.rs:374-446` `remembered_invitation_survives_restart_but_not_revocation`.
- `crates/pb-mapper-desktop/tests/session.rs:73-204`
  `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown`
  (bad token sees no frames; path label; input gating; 1000 releases coalesce;
  revocation; direct survives relay loss);
  `:206-347` `cursor_crosses_a_full_video_window_without_refreshing_the_picture`
  (premise: the single IDR exceeds initial credit and is never ACKed; cursor
  still flows; viewer goes `stale`).
- Ignored acceptance (`D/slow_link.rs`): `:158-167`
  `delayed_feedback_does_not_starve_async_encoder` (asserts
  `slow_consumer` MUST lower bitrate, `:283-288`, and `peak <=
  MAX_QUEUED_FRAMES`, `:289`); `:380-575` `sustained_slow_tls_reader`;
  `:625-818` `presentation_feedback_recovers_after_slow_tls_and_renderer`
  (asserts a renderer that stops ACKing is stopped within 6 s, `:788-807`).
- Examples using the viewer API: `examples/viewer_probe.rs` (simulated renderer,
  `presented` cadence), `examples/deployed.rs:89-102`,
  `examples/input_acceptance.rs:463-552` (`latest_frame`).

Rust, FFI (`cargo test -p pb-mapper-ffi`):

- `F/desktop.rs:1318-1345` `status_and_video_with_the_same_id_both_cross_the_binary_boundary`.
- `:1347-1403` `background_consent_and_ui_ownership_boundaries`.
- `:1405-1425` `late_background_discovery_cannot_hide_a_new_grant`.
- `:1427-1481` (Windows) `revocation_waits_for_handoff_and_uses_the_actual_owner`.
- `:1483-1573` (Windows) `disabled_service_allows_local_host_but_unknown_background_stays_blocked`.
- `:1575-1652` `disconnect_cancels_configuration_wait_without_cancelling_reconnect`.
- `:1654-1675` `disconnect_cancels_setup_waiting_for_viewer_lock`.
- `:1677-1704` `disconnect_cancels_pending_setup_without_waiting_for_network`.
- `:1725-1812` `binary_frame_is_owned_until_explicit_release_after_disconnect`
  (live relay; buffer outlives disconnect; host stop not blocked by a held viewer
  lock).
- `F/desktop_trust.rs:249-296` (Windows) `dpapi_round_trip_tampering_and_revocation`;
  `:298-338` (Windows) `background_handoff_keeps_the_live_link_through_restart`.

Dart (FFI-adjacent): `ui/test/desktop_lifecycle_test.dart:98, 144, 199, 243`
(final presented frame ACKed without another frame; worker cancellation releases
its native borrow; disconnect while busy; input live only after current recent
consumption), plus `desktop_test.dart`, `desktop_recovery_test.dart`,
`desktop_host_readiness_test.dart`, `desktop_viewer_navigation_test.dart` over
`ui/test/fake_desktop_api.dart`.

Gaps: no test of `wait_frame` timeout behaviour, of the routed-call latency
while `wait_frame` blocks, of `connect` failure classification (busy vs denied
vs identity), of the `status` op blocking behind `disconnect`, or of any
viewer-side persistence (none exists).

---

## 7. Impact on the planned overhaul

### 7.0 Cross-cutting decisions this scope forces

- **Protocol versioning first.** Every planned wire change (receive ACK, periodic
  probe nonces, fps cap / quality / pause commands, new keys) breaks an older peer
  mid-session because all viewer->host types are `deny_unknown_fields`
  (4.5). Since PR #51 is unreleased, the clean option is to bump ALPN to
  `pb-mapper/desktop/2` (`D/security.rs:12`) so a mismatch fails during the TLS
  handshake (rustls `NoApplicationProtocol`), and map that to a structured
  "update the other computer" error. Then drop the opt-in booleans
  (`presentation_feedback`) instead of piling up flags. Risk: devices already
  installed from PR #51 builds (cellular/installed acceptance runs) stop
  interoperating with new builds; acceptable only if both ends are updated
  together - state it in release notes.
- **Split the FFI viewer slot into lifecycle vs hot path.** Today every viewer op
  `block_on`s a `tokio::Mutex` that `disconnect` holds for seconds
  (3.7). Keep `tokio::Mutex<Option<Viewer>>` for connect/disconnect only, and add
  a synchronous slot (e.g. `parking_lot::RwLock<Option<Arc<ViewerShared>>>` or
  `arc-swap`) holding cheap handles: input sender, control mailbox, presented /
  keyframe watches, latest cursor, stats, the video source. `Viewer::input`,
  `presented`, `request_keyframe`, `request_control`, `release_input` are already
  synchronous, so hot ops then need no runtime at all. Also fix
  `disconnect_viewer` to `take()` under a short `let` guard and call `stop()`
  after dropping it.
- **Structured error/state codes instead of prose.** Dart matches
  `"desktop identity changed"` (`U/state/desktop_store.dart:432`) and
  `"input_failed:"` (`:374`); the ViewerStates design needs distinct states for
  identity change, host busy, denied, version mismatch, capture unavailable
  (secure desktop). Add explicit codes (a new `ErrorCode` variant or a `reason`
  field in the connect error / viewer status) and stop string matching.

### 7.1 P0-1 credit on receipt (merge blocker)

Viewer (`D/viewer.rs`):
- In the receive loop, right after a non-empty media message is read (`:217-224`)
  and validated (`:239-251`) - before `incoming.push` and with no FFI/Dart/decoder
  involvement - publish a cumulative receipt `(generation, frame_id)` into a new
  `watch`. Acknowledge frames even if the local queue then drops them (they were
  received; credit is transport credit, not decodability).
- In the send loop (`:296-316`) add a branch writing a new
  `Command::Received{generation, frame_id}` (name TBD), placed after control
  barriers and before `Presented`/input. Coalescing via `watch` bounds it to one
  pending write. Consider combining with the probe echo to save TLS records.
- Remove the `presentation_feedback` gating of the probe watch (`:206, 231-233,
  306-311`): receipts and probe echoes must work for every renderer, JPEG
  included.
- Keep `presented()` / `Command::Presented`: it still authorises and renews input
  (`D/worker.rs:443-473`, helper hold renewal 2 s). It must keep flowing at least
  every < 2 s while controlling, independent of credit. `frame_ack_ms` keeps its
  meaning only if documented as "including phone processing"; the overlay's
  "picture latency" should come from it (or from a new receipt-based field) -
  decide and name it explicitly.
- Local presentation lag: the viewer, not the host, must handle backlog. With
  credit = fps x RTT + margin, more than 8 unpresented frames can legitimately
  arrive; the current overflow rule (`D/media.rs:47-54`) then drops the whole
  queue and asks for an IDR on every burst. Replace with: keep feeding the decoder
  in order (H.264 cannot skip P-frames), let the native side skip presentation of
  superseded frames (DesktopVideo.kt already does), and only when the decode
  backlog exceeds a threshold: drop to the next IDR and request one, and send a
  new viewer->host fps cap (P1-1 "cap at 90% of measured decode fps").
- Remove the coupling `congestion::MAX_FRAMES = media::MAX_QUEUED_FRAMES`
  (`D/congestion.rs:14-16`); the viewer queue bound becomes a memory guard, the
  host window becomes delay-based.
- Host coupling: `Feedback::presented_at` credit logic, `stalled()` (4 s without
  presentation ACK closes the session with "Renderer stopped acknowledging...",
  `D/host.rs:202-212`), recovery (`request_keyframe` sets `recovering` until the
  next presentation ACK, `D/congestion.rs:234-249, 285`) must all key off
  receipts instead. A backgrounded phone would otherwise be killed after 4 s.
- New consideration: with receipt credit, a backgrounded/hidden viewer keeps
  receiving full-rate video. Add an explicit viewer->host pause/resume (or fps
  cap 0) when the viewer page is not foreground (Dart already knows:
  `setViewerForeground`, `U/state/desktop_store.dart:1024-1042`), or disconnect
  after a grace period.
- Tests to rewrite/delete: `slow_link.rs` slow_consumer assertion
  (`:283-288` "consumer backlog must still apply backpressure") and
  `presentation_feedback_recovers...` no-ACK stop (`:788-807`) now contradict the
  requirement; `session.rs:244-247` premise comment; `media.rs` overflow test if
  the policy changes; `D/congestion.rs` unit tests (host map). New minimal tests:
  receipt is sent without any `presented` call; credit returns while the
  renderer is frozen; input is still refused without a fresh presented frame.

### 7.2 P0-2 even pacing + delay-based adaptation

Mostly host. Viewer/FFI impact:
- The resolution ladder (`D/congestion.rs:359-366`) currently changes encoded
  size mid-session; the viewer accepts any size <= its preferences
  (`D/viewer.rs:244-251`) and Dart resets nothing for size-only changes within a
  generation. After the change resolution should be stable; keep the viewer's
  validation (it is the safety bound).
- Expose the adaptation state for the overlay message ("bitrate lowered to keep
  input responsive"): add e.g. `adaptation: "hold"|"lower_bitrate"|"lower_fps"|...`
  and target fps to `SessionStatus` (additive, safe).
- "Client unresponsive > 2 s -> 2 fps" must be driven by receipts/probe silence,
  not presentation.

### 7.3 P0-3 infinite GOP

- With no periodic IDR, every viewer recovery depends on `Keyframe` being
  honoured. Today the host ignores repeated requests while `recovering`
  (`D/congestion.rs:236-238`) and only presentation clears it; a lost IDR or a
  decoder that never presents can then stall the picture indefinitely (today the
  1 s GOP hides this). Make the host honour keyframe requests idempotently with a
  rate limit, and keep the viewer re-requesting with backoff (Dart already does at
  700 ms / 1 s, `U/state/desktop_store.dart:418-425`).
- Viewer triggers that must all request an IDR: queue overflow/discard
  (`D/media.rs:55-62`), generation change starting with a P-frame (same path),
  decoder error (`needKeyframe`, `U/state/desktop_store.dart:565-574`), new
  session (host starts with `keyframe = true`, `D/worker.rs:383`), Shortcuts menu
  "Refresh picture" (op `keyframe`).

### 7.4 P0-4 targets and quality preference

- Defaults live in two places: Dart `connect` (`U/ffi/desktop_api.dart:428-447`:
  1920x1080, refresh-clamped fps, 12 Mbps) and Rust
  `VideoPreferences::default()` (`D/lib.rs:186-195`, 90 fps, 12 Mbps, also the
  host default for an `Authenticate` without `video`). Put the Quality / Balanced
  / Smooth mapping in ONE place - preferably Rust (`connect` op takes
  `quality: "quality"|"balanced"|"smooth"`, Rust maps to `VideoPreferences`), so
  the SDK, examples and both UIs agree; keep `video` only as an expert override or
  delete it.
- Phone default 1080p / ~4 Mbps / <= 60 fps; `validate()` floors fps at 30
  (`D/lib.rs:201`) - fine for the ceiling; host adaptation below 30 is internal.
- Mid-session change from the Display menu needs a new wire command (e.g.
  `Command::Video{preferences}` or a quality enum) - see 7.0 versioning. The
  viewer must then also update the bound it validates against (`:247-248` uses
  the connect-time `preferences`).
- The measured decode/present fps cap (P1-1) needs a path into Rust: either the
  native decoder reports it directly (if it pulls frames from Rust, 7.5) or a new
  FFI op `renderer{decode_fps, present_fps, backlog}`.

### 7.5 P1-1 phone pipeline (frames straight to the decoder)

Preferred design touches the FFI frame ABI:
- Separate channels in `media::Queue` (`D/media.rs`): ordered video for exactly
  one native consumer; status and cursor as latest-value slots for Dart
  (today `pop` interleaves status ahead of video, `:70-77`, and stamps cursor into
  video, `:80`). Delete the cursor stamping.
- New export for the native decoder thread, in the style of the existing leaf
  calls that return an integer (`pb_mapper_galaxy_frame_v1`): e.g.
  `pb_mapper_desktop_video_next_v1(source, dst *mut u8, cap usize, timeout_ms
  u32, meta *mut VideoMeta) -> i32` copying one access unit into a
  caller-owned (direct ByteBuffer / native) buffer with a fixed binary
  `VideoMeta{generation, frame_id, pts_us, width, height, keyframe, codec}` -
  no JSON, no per-frame allocation, no Dart. Waiting should use a sync condvar /
  `Notify` bridged to a blocking wait, not `block_on` + 16 ms polling.
- Lifetime: a native thread must not borrow `*mut PbMapperHandle` (destroy
  safety, `F/handle.rs:89-91`). Hand out a refcounted source object
  (`pb_mapper_desktop_video_source_v1(handle) -> *mut Source`,
  `..._release`) that survives viewer replacement and returns "ended/new session"
  codes; generation changes are signalled in `VideoMeta`.
- Android: Kotlin needs JNI to call it. Options: add the `jni` crate (Android
  target only) with `Java_com_pb_1mapper_ui_DesktopVideo_*` exports, or decode
  in Rust with NDK `AMediaCodec` on an `ANativeWindow` from the Flutter
  SurfaceTexture (`ndk` crate) - larger change, removes Kotlin from the hot path.
  Windows: `ui/windows/runner/desktop_video.cpp` can call the DLL export directly.
- Dart then receives only status/geometry (and cursor via 7.7); delete the frame
  isolate worker's media role, `export_frame`'s media payload, the per-frame
  JSON metadata, `pb_mapper_desktop_frame` + the `compute` fallback
  (`U/state/desktop_store.dart:342-350`), and the ignored `_after`/`_generation`
  parameters. Keep the 4-byte-length layout only if a JPEG fallback consumer
  remains (non-Android/Windows platforms use JPEG today).
- Fallback design (if JNI is deferred): allow 2-3 credits in the Dart frame
  worker and stop routing input through it (7.7); FFI unchanged except
  removing the 16 ms poll in favour of a longer blocking wait with cancellation.
- Risk: H.264 ordering - exactly one consumer may pop video; any leftover Dart
  `next_frame` caller would corrupt references.

### 7.6 P1-2 delay probe on the network task

- The echo already happens in the viewer's network task, never through FFI/Dart
  (`D/viewer.rs:225-235, 306-311`). Changes: echo unconditionally (no
  `presentation_feedback` gate); host side moves from one fixed nonce 1
  (`D/congestion.rs:121-129`) to a periodic (~1 s) sequence; the viewer echoes
  the latest nonce (watch coalescing is correct).
- Report the host-measured RTT (and baseline) back in `SessionStatus` (new
  additive field, e.g. `network_rtt_ms`) for the overlay's "network RTT" and the
  toolbar chip ("direct 16 ms 60 fps").
- Probes ride the same ordered stream as video, so they measure queueing in the
  sender's socket buffer too - that is the intended "transport delay"; the
  baseline is the minimum.

### 7.7 P1-3 input and cursor independent of video

FFI/viewer side:
- Non-blocking latest-cursor read: keep the newest `(generation, seq,
  CursorPosition)` in the synchronous viewer slot, updated by the receive loop
  for status-only and media messages; export
  `pb_mapper_desktop_cursor_v1(handle, out *mut CursorOut) -> i32` (no JSON, no
  tokio lock) so Dart can poll it per rendered frame on the UI isolate. Remove
  cursor from `media::Queue` (`D/media.rs:17, 41-44, 80, 90`) and from the
  frame stream.
- Input/control/release/presented/keyframe must not route through the isolate
  blocked in `wait_frame` (`U/ffi/pb_mapper_service.dart:288-299`). With the sync
  hot-path slot (7.0) these become microsecond calls that can run directly on
  the main isolate (or a dedicated long-lived input isolate). Do NOT call them
  from the UI isolate while they still `block_on` a `tokio::Mutex`: `disconnect`
  holds that lock for seconds and would freeze the UI.
- Host suppression of cursor echo for the operator who moved within 300 ms is
  host-side; the viewer must draw its local pointer while moving and switch back
  to the remote cursor afterwards (Dart/native).
- Keep: input mpsc bound (32) with revoke-on-overflow, `Controls` ordering, and
  the "fresh presented frame" requirement (Dart passes the presented frame id).

### 7.8 P1-4 zoom/pan local - invariants to keep in this scope

No FFI op exists for zoom, pan, rotation, soft keyboard or window resize, and
none may be added: they must not message the host, change `VideoPreferences`,
reset the decoder or the texture. Only the user's quality choice, a display
switch, or long-term bandwidth may change encode parameters.

### 7.9 P2-1 transport

Nothing to change: the viewer already uses one ordered stream (loopback TCP ->
SDK tunnel -> QUIC direct or relay). Note the extra loopback hop and TLS-in-tunnel
on both ends (`SDK/sdk/client.rs:373-386`, `D/host.rs:92`); measure before
optimising.

### 7.10 Regression guards (how each stays true)

- Bounded queues: after P0-1 the viewer media queue is still bounded (keep a byte
  cap even if the frame cap grows); Dart/native submit credits stay finite;
  watches remain latest-value.
- Reconnect never replays input: new session = new `Viewer` channels; keep Dart
  epochs and `sequence` reset; never persist or re-send queued input in any new
  native path.
- Input requires a fresh presented frame: keep `Command::Presented` and the
  host's 2 s `recent` check; receipts must never call `Engine::presentation`
  or `backend.presented` (receipt != presentation).

### 7.11 Requirement 2 (UI/UX) capabilities that need FFI/viewer work

| Capability | Today | Needed in this scope |
| --- | --- | --- |
| One-button "Enable remote control" + two permission levels | `host_enable` (Windows service) hardcodes `allow_control: true` (`F/desktop.rs:1186`, `F/desktop_trust.rs:75`); `host_start` takes `allow_control`; `host_permission` changes it later | Add `allow_control` to `host_enable` (default view-only per design) so control is never briefly granted on enable; keep `host_permission` for changes. Steps "install helper -> restart -> enabled" map to `{restart:true}` then `host_restore`. |
| Invite copy / QR | invitation returned by `host_start`/`host_enable`, and in every `status` poll via `background.invitation` (250 ms) | QR rendering is Dart. Consider not echoing the invitation in every `status` (fetch on demand) to keep the bearer secret out of routine polls. Optional v2 invitation pinning a SHA-256 fingerprint instead of the full certificate would shrink the QR from ~0.6 KB to ~0.15 KB (changes `Pinned`, `D/security.rs:139-158`; needs 7.0 versioning). |
| Revoke invite | only by discarding the identity (`trust_revoke`, `background_action revoke`); restart with a new identity | Document that revoke = new invitation for everyone; remembered viewers will see the identity-changed state (by design). |
| Host viewer info (watching / controlling / path / since when) | `Host::status()` exposes `state` and `controlling` only; host `SessionStatus.path` is empty | Host must add session start time and path (from `registration.diagnostics()`); FFI `status` passes it through unchanged. |
| Remote computer list (remembered, online, last path, "back to picture") | nothing persisted on the viewer; one viewer slot | New viewer-side store (below). Online = `invitation.service` in the relay's key list: `Client::list_keys()` (`SDK/sdk/client.rs:511-518`) is already fetched and cached 2 s by `fetch_real_status` (`F/state.rs:59-66`), so no new relay call is needed. "Registered" does not mean "accepting": a host with a connected viewer refuses a second one silently. Last path = `status.viewer.path` at disconnect. "Back to picture" = viewer slot occupied. |
| Encrypted invite storage ("Remember this computer", default on) | only the host trust file, Windows DPAPI, single record | New multi-record store keyed by service + certificate (and the relay/namespace it was added under, like `trust::Scope`). Windows can reuse `desktop_trust::protect`; Android needs Keystore (Kotlin/JNI or a maintained plugin such as `flutter_secure_storage`, which uses Keystore-backed encryption on Android). Pick ONE abstraction for both; other platforms: disable "Remember". Deleting a computer must delete its thumbnail too. |
| Add computer: show recognised host before connecting | none; Dart can only call `connect` | Add an `inspect_invitation` op returning `{service, certificate_fingerprint}` using the same `Invitation::decode` (no network), so validation stays in Rust. The invitation has no display name; either derive from `service` or add `name` in an invitation v2. |
| Camera QR scan | none | Dart plugin; result goes to `inspect_invitation` / `connect`. Frames never stored. |
| Last-picture thumbnail | none | Cannot come from Rust (compressed H.264); capture from the native decoder output or Flutter at disconnect; store next to the remembered-computer record in `desktop_config_dir`. |
| Viewer state overlay (6 states) | `connect` is one blocking call; no stages; failures are prose | Report setup stages ("path found (direct/relay)", "identity verified", "waiting for first frame") - e.g. make `connect` return immediately and publish `setup: {stage, path, failure_code}` in `status.viewer`, or emit progress in the pending-connect record. Distinguish `identity_changed`, `busy`/`denied` (host must send a reason before closing - today it drops the socket, `D/host.rs:172`), `version_mismatch` (ALPN), `capture_unavailable` with a structured `secure_desktop` reason, `reconnecting` (Dart), `view_only` (`control_allowed`). |
| Connection quality overlay | have: `path`, `direct_*`, `receive_fps`, `capture_fps`, `bitrate`, `video.{codec,width,height}`, `frame_ack_ms`, `dropped_frames` | Add: network RTT (7.6), relay carrier TCP/QUIC (`connection.diagnostics().relay_transport`, sampled at connect), IP family for "direct · IPv6" (not in SDK diagnostics today), adaptation state (7.2), hardware/software decoder (native), display fps (native/Dart `_presentFps`). Define "picture latency" precisely. |
| Quality preference (Display menu) | connect-time `video` only | 7.4. |
| Shortcut menu / modifier bar | `Key` lacks F1-F12, D, L, most letters, Win combos beyond `meta`; `Input` has no "release all keys" (use op `release`) | Extend `Key` (`D/lib.rs:262-287`) plus the Windows backend mapping; old hosts reject new keys (7.0). "Refresh picture" = op `keyframe`; "Release all keys" = op `release`; Ctrl+Alt+Del stays unsupported. Mobile text stays `Input::Text` (<= 1024 bytes per event). |
| New gestures / virtual mouse | `Button::Right`, `Wheel` exist | No FFI change. Three-finger scroll must chunk wheel deltas to |100| per event (`D/lib.rs:386-391`) or the host rejects them. |

Requirements 3 (icons) and 4 (caption buttons) do not touch this scope.

### 7.12 Code in this scope that the overhaul makes obsolete

- `presentation_feedback` (wire field `D/wire.rs:18-19`, constructor split
  `D/viewer.rs:116-127`, FFI `native_video` branch `F/desktop.rs:748-766`) once
  receipts are universal.
- `pb_mapper_desktop_frame` and the Dart `compute` frame fallback; the ignored
  `_after`/`_generation` ABI parameters; per-frame JSON metadata in
  `export_frame` (if P1-1 lands).
- Cursor stamping and status-before-video interleaving in `media::Queue`.
- `congestion::MAX_FRAMES` tied to `media::MAX_QUEUED_FRAMES`.
- The single fixed probe nonce.
- Tests listed in 7.1 whose premise (presentation drives credit) is removed.
