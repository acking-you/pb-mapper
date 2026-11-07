# Map: remote desktop HOST pipeline (`crates/pb-mapper-desktop`)

Scope: host side of `crates/pb-mapper-desktop` at `a41eaa7` (worktree
`<repo>`).
All paths below are relative to `crates/pb-mapper-desktop/` unless prefixed.
Viewer (`src/viewer.rs`) and FFI are cited only where they are the other end
of a host contract.

---

## 1. Overview

One `Host` (src/host.rs:20-27) registers a pb-mapper TCP service
(`desktop:<name>`) whose local end is a loopback listener
(`127.0.0.1:0`, host.rs:92-108). Each accepted tunnel connection is a
**TLS 1.3 session** (pinned self-signed cert, ALPN `pb-mapper/desktop/1`,
security.rs:11-12, 68-101) carrying a bearer token check, then a
per-session **Worker** (src/worker.rs) that owns the capture/encode/input
backend on one blocking thread. The session task runs two futures:
`receive` (viewer commands) and `send` (status + video), multiplexed in one
tokio task (host.rs:178-275).

Video is Windows Desktop Duplication -> D3D11 video processor (scale +
BGRA->NV12) -> hardware Media Foundation H.264 MFT (async), baseline profile,
CBR, low-latency, GOP = fps (src/video/windows.rs). A JPEG fallback (xcap +
`image` crate, 8 fps) exists for viewers that do not offer H.264
(src/windows.rs:245-259, worker.rs:264-272).

Flow control is **presentation-ACK driven** when the viewer opts in with
`presentation_feedback: true` (src/congestion.rs `Feedback`): at most
`MAX_FRAMES = 8` frames / a byte window (16-512 KiB) may be un-presented;
credit returns only on `Command::Presented`, which also drives bitrate
(halve on sustained delay, +25% probe) and a resolution/fps **quality ladder**
(640x360@30 ... ceiling). While credit is exhausted the worker stops
submitting captures (`paused = waiting`, worker.rs:225-227, 254) and only
drains the encoder. Viewers without feedback get a socket-write-time bitrate
heuristic instead (worker.rs:294-304).

Input arrives as JSON `Command::Input` and is executed on the **same capture
thread** after authorization against a fresh frame of the current generation
(worker.rs:586-621). Cursor position is sampled every 16 ms on that thread and
sent as status-only messages outside the media queue (worker.rs:206-220,
host.rs:213-226).

An optional installed Windows service (`src/helper/`, feature
`windows-helper`, binary `pb-mapper-desktop-helper`) provides a SYSTEM worker
process bound to the input desktop (UAC / lock screen); the host process then
uses a named-pipe proxy `RemoteBackend` as its `Backend`
(src/helper/client.rs). The service can also own a background `Host` itself
(src/helper/background_service.rs:381-398).

---

## 2. Components and responsibilities

### 2.1 Public API surface (src/lib.rs)

| Item | Lines | Notes |
| --- | --- | --- |
| modules | lib.rs:8-26 | `slow_link` and `tests` are `#[cfg(test)]` (16-19); `helper` and `video::windows` are `cfg(windows)` |
| `ControlAdmission` / `ControlLease` | 36-43 | Embedder hook; `Engine::control` acquires a lease on grant (worker.rs:436-438). Unused by FFI and service (both pass `None`: ui/native/pb_mapper_ffi/src/desktop.rs:1025-1042, background_service.rs:394) |
| `valid_service` | 49-56 | `desktop:` prefix, len 9..=128, `[A-Za-z0-9._:-]` |
| `Display` | 58-67 | physical px, `x/y` may be negative |
| `CursorPosition {x,y,visible}` | 71-76 | hotspot relative to selected display; no cursor image |
| `Capabilities` | 78-84 | from platform.rs:3-15 (`host: cfg!(windows)`, `secure_desktop: false`) |
| `Invitation {version,service,certificate,token}` | 89-123 | `deny_unknown_fields`; decode limits: <=8192 bytes, version 1, token len 43, cert <=4096 |
| `SessionStatus` | 125-165 | THE wire status record (see section 4.2). No `deny_unknown_fields` -> adding fields is viewer-compatible |
| `VideoCodec {Jpeg (default), H264}` | 167-173 | snake_case |
| `VideoPreferences {max_width,max_height,max_fps,bitrate}` | 178-208 | default 1920x1080, 90 fps, 12_000_000 (186-195). `validate`: width 2..=3840, height 2..=2160, fps 30..=120, bitrate 1M..=40M (198-207). `deny_unknown_fields` |
| `VideoFormat {codec,width,height,keyframe,pts_us,duplicate}` | 210-219 | |
| `EncodedFrame`, `Frame {status, bytes: Arc<[u8]>}` | 221-242 | |
| `DesktopContext {display, generation, desktop}` | 229-234 | backend desktop identity; equality is the context check everywhere |
| `Input` (tag `kind`) / `Button` / `Key` / `InputEvent` | 246-295 | `deny_unknown_fields`; Key list is fixed (22 keys, no F-keys) |
| `Backend` trait | 301-375 | `configure_video`, `display`, `context`, `capture`, `cursor`, `capture_video`, `drain_video`, `capture_consistent`, `input`, `input_in_context`, `presented`, `release` |
| `Input::validate` | 377-395 | Move inside display; text <=1024 bytes no NUL; wheel abs <=100 |

### 2.2 `Host` (src/host.rs)

- Constructors (29-304): `start` (native backend), `start_with_backend`,
  `start_with_admission`, `start_with_identity` (remembered identity),
  `start_with_backend_and_identity` (all paths end here).
- Registration: `RegisterRequest { key: service, local_addr: loopback,
  transport: Transport::Tcp, codec: true, force_namespace: false }`,
  `RegisterOptions { allow_direct }` (93-108); waits ready 15 s (109-127).
  `codec: true` means pb-mapper's AES-GCM codec wraps the tunnel *in addition*
  to the desktop TLS.
- Shared state: `allowed: Arc<Policy>` (128), `status: Arc<Mutex<SessionStatus>>`
  (129-133, one per Host, shared by all sessions and the worker),
  `cancel: CancellationToken` (134).
- Accept task (139-295): `Semaphore::new(1)` = **one active viewer** (140,
  165); `JoinSet` with at most 8 concurrent sessions/handshakes (146).
- `status()` (308-315) masks `controlling` when control is disallowed.
- `set_control_allowed` (318-321) -> `Policy::set`.
- `stop()` (322-333): revoke, cancel, join accept task, stop registration,
  state "stopped". `Drop` (335-340) revokes + cancels only.

### 2.3 `Worker` / `Engine` (src/worker.rs)

- `Policy(AtomicU64)` (17-37): bit0 = allowed, upper bits an epoch bumped by
  2 on every `set` (even revoke+grant between iterations invalidates).
- `Worker` (39-49) handles: `requests: mpsc::SyncSender<(Command,
  oneshot)>` (capacity 16, 76), `frames: watch<Option<Frame>>` (wake signal +
  latest frame), `cursor: watch<Option<SessionStatus>>`, `media:
  Arc<media::Queue>`, `feedback: Option<Arc<congestion::Feedback>>` (only if
  `presentation_feedback`, 83-84), `bitrate: Arc<AtomicU32>` (non-feedback
  heuristic), `wake: scheduler::Wake`.
- `Worker::command` (305-315): `try_send` (full -> error "desktop input worker
  unavailable"), `wake.notify()`, await reply with **2 s timeout**.
- `Worker::sent` (294-304): non-feedback only; if a frame write took >60 ms,
  `bitrate = clamp(0.8 * bytes*8/elapsed, 1 Mbps, current)` (never raises).
- `Engine` (331-347) fields: `backend`, `state: SessionStatus`, `sequence`
  (last input seq), `recent: VecDeque<(frame_id, Instant)>` (<=256 entries,
  553-556; frame creation times), `presented`, `acknowledged`, `context`,
  `codec`, `keyframe` (pending IDR request), `bitrate`, fps sampling,
  `admission`, `lease`, `input_failure` (sticky message).

### 2.4 Congestion/feedback (src/congestion.rs)

`Feedback { state: Mutex<State>, changed: Notify }` (65-68). Ledgers:
`ready` (frames produced, <=256, with production time) and `sent` (admitted,
with send time, bytes, keyframe, baseline_sample). See section 3.5 for the
algorithm and all constants.

### 2.5 Media queue (src/media.rs)

`Queue` (23-109) is shared by host (worker -> sender) and viewer
(network -> decoder). Bounds `MAX_QUEUED_FRAMES = 8`, `MAX_QUEUED_BYTES =
8 MiB` (10-11). Generation change or empty-bytes frame clears it (33-40);
overflow clears all queued frames, counts them dropped, and for H.264 waits
for the next keyframe, raising a one-shot keyframe request (47-62);
`push_status` holds one coalesced status slot that `pop` returns first
(70-93); `resynchronize` clears + requests keyframe (100-109).

### 2.6 Scheduler (src/scheduler.rs)

Windows `Waiter` (39-111): an auto-reset event + a per-worker
`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` timer; `receive` does `try_recv`,
then `WaitForMultipleObjects(event, timer)` (79-96), then `try_recv`
again. `Wake::notify` = `SetEvent` (29-36). Purpose: 2 ms codec polls
without the 15.6 ms default timer granularity, without changing process
timer resolution (header 1-4). Non-Windows: `recv_timeout` (114-145).

### 2.7 Windows ordinary backend (src/windows.rs)

`Desktop` (137-146): `id` (HMONITOR as u32), `monitor_handle`, `Enigo`
input, held `buttons`/`keys` sets, `secure: Option<DesktopBinding>` (only
privileged), `encoder: Option<(DesktopContext, video::windows::Encoder)>`,
`video_preferences`.

- `available()` (44-66): `OpenInputDesktop(DESKTOP_READOBJECTS)` and the name
  must be `Default`, else `secure_desktop_requires_helper` error (67-71).
- `prepare()` (165-181): ordinary -> `available()`, on failure release input
  and drop encoder; privileged -> `DesktopBinding::enter(before_switch)` where
  `before_switch` releases input and drops the encoder.
- `display()` (201-205) = DPI scope + `prepare` + `GetMonitorInfoW`
  (`current_display` 90-114). `context()` (206-216) = `display()` +
  binding generation/name (ordinary: generation 0, desktop "ordinary").
- `configure_video` (193-200): changed prefs -> `encoder = None` (rebuild).
- `capture_video` (260-309): JPEG -> `capture_consistent`; H.264 -> context
  check, (re)create `Encoder::new_with_size(display, prefs.max_fps,
  min(bitrate, prefs.bitrate), prefs.max_width, prefs.max_height)` when
  context differs (284-300), `set_bitrate` every call (306), `next_frame
  (keyframe)` (307), `finish_video` re-checks context (148-164).
- `drain_video` (310-323): context check, `encoder.drain()`.
- `cursor` (217-244): context check, `GetCursorInfo`, context check again
  + `secure.changed()`, `relative_cursor` (488-504: hidden/off-display ->
  `CursorPosition::default()` so position is not disclosed).
- `input_in_context` (328-378): context + `changed()` check (release on
  mismatch), `validate`, then: Move -> `SetCursorPos(display.x + x,
  display.y + y)` (343); Button/Key -> Enigo with held-set bookkeeping
  (`apply_held` 455-475 keeps release obligation on partial failure);
  Wheel -> Enigo scroll; Text -> Enigo text.
- `native_input_error` (384-444): captures Win32 error + token
  session/integrity/UIAccess, records a service event (helper feature), returns
  a sanitized message.
- `create_privileged` (506-523): helper worker only; `DesktopBinding::new` +
  initial `enter`.
- `key_code` (538-563): A/C/V/X/Z are `enigo::Key::Unicode(..)`.

### 2.8 Video encoder (src/video/windows.rs)

See section 3.4. Constants: `MAX_IN_FLIGHT = 3` submitted-not-output
(34), `MAX_ACCESS_UNIT = 4 MiB` (35). Must be created/used/dropped on one
thread (`Runtime` COM/MF init per thread, `check_thread`, 76-108, 266-271).

### 2.9 Wire (src/wire.rs) and security (src/security.rs)

`MAX_JSON = 8192` (5), `MAX_FRAME = 4 MiB` (6), `IO_TIMEOUT = 3 s` (7).
Framing: `u32` big-endian length + payload; every `write_bytes` flushes
(65-76). `read_json_or_eof` distinguishes clean EOF before a header (52-64).
`HostIdentity {certificate, private_key, token}` (security.rs:24-30),
32-byte random token base64url (14-19), TLS13-only acceptor, ALPN
`pb-mapper/desktop/1`, no 0-RTT (68-101); client pins the exact DER and
requires an empty chain (139-158).

### 2.10 Helper (Windows service) - brief

`src/helper/` is the opt-in installed Windows service `PbMapperDesktop`
(helper/mod.rs:34-37, binary `src/bin/helper.rs`, feature
`windows-helper`). Roles:

- **Service process** (helper/service.rs): SYSTEM, session 0. Listens on
  `\\.\pipe\pb-mapper.desktop-helper.v1` (owner-SID ACL, ipc.rs:12-28).
  For each authorized caller (`verify_client`, must be the installed UI run by
  the owner SID in the active console login) it spawns a **worker process**
  (`spawn_worker`, service.rs:222-330: duplicated SYSTEM token moved to the
  user session, `CreateRestrictedToken(DISABLE_MAX_PRIVILEGE)`, job object
  kill-on-close, 1 process, desktop `winsta0\default`) and then just
  `copy_bidirectional`s the caller pipe to a private worker pipe
  (service.rs:190-206) until stop, login change, owner exit, or session event
  (logoff/disconnect revoke; lock/unlock does not, 59-76).
- **Worker process** (helper/worker.rs:251-458): validates launch, reads
  `Request::Attach{display_id, session}`, runs the privileged `Desktop`
  (`create_privileged`) on a dedicated `std::thread` (278-427) servicing one
  request at a time (`recv_timeout(15 ms)`, 304), releases held input if no
  `Input`/`Presented` for 2 s (300-302). `DesktopBinding` (128-248) binds the
  thread to the input desktop with `INPUT_DESKTOP_ACCESS = READOBJECTS |
  WRITEOBJECTS | SWITCHDESKTOP | JOURNALPLAYBACK` (12-17), re-binding on
  name/epoch/console-lock change; a WinEvent hook thread (`Events`, 31-89)
  bumps static `EPOCH` on `EVENT_SYSTEM_DESKTOPSWITCH`.
- **Host-process proxy** `RemoteBackend` (helper/client.rs): a `Backend` whose
  every call is one blocking request/response over the pipe (`call`, 60-99,
  own current-thread runtime, 3 s timeout, any failure is sticky `failed`).
  `helper::backend(display_id)` (16-20) is what the FFI passes as the host
  `Factory` when `settings.privileged` (ui/native/pb_mapper_ffi/src/desktop.rs:1021-1034).
  `cursor()` returns the cursor cached from the last response (119-124).
- **Background host** (helper/background.rs + background_service.rs): the
  service itself runs `Host::start_with_backend_and_identity` with
  `client::service_backend` (background_service.rs:381-398). So the whole
  host pipeline in this map (Host/Worker/congestion/encoder) also runs **inside
  the installed service binary**, not only in the UI process. The UI is a
  management client (`Command::{Configure,Status,Start,Stop,Revoke,
  RevokeScope,Permission}`, background.rs:55-74).

Helper IPC `Request` (helper/mod.rs:148-182, tag `op`, `deny_unknown_fields`):
`start{display_id}`, `attach{display_id, session}`, `context`, `capture`,
`configure_video{preferences}`, `capture_video{expected, codec, keyframe,
bitrate}`, `drain_video{expected}`, `presented{generation}`,
`input{generation, sequence, input}`, `release`, `close`. `Response {context,
error, video?, cursor?}` + a bytes record (183-192). Worker enforces
`expected == context`, bitrate 256k..=40M (354-356), input generation and
strictly increasing sequence (377-381).

### 2.11 Examples (what each measures)

| Example | Measures | Notes |
| --- | --- | --- |
| `examples/capture_probe.rs` | capabilities, displays, 10 JPEG captures per display (elapsed, bytes) | xcap/JPEG path only; no H.264, no network |
| `examples/video_bench.rs` | capture+encode only: `unique_capture_fps`, `encoded_fps`, `unique_encoded_fps`, `bitrate_mbps`, `mean_encode_ms`, `max_in_flight`; `--backend` mode: full `native_backend` per-call `mean_context_ms`/`mean_capture_video_ms` polled every `--poll-us` (default 8333) | args: seconds (10), fps (120), `--max-width/--max-height`, `--bitrate` (24M), `--animate`/`--pattern-only` (DwmFlush-animated window), `--output` raw Annex B, `--pause-ms` (expire GPU output; next output must be IDR). Forces one IDR at 1.5 s (prints `forced_keyframe_ms`), drops bitrate to <=12M at 2 s. Prints `keyframes` -> use it to verify GOP change |
| `examples/transport_probe.rs` | renderer-free transport: 12x 64 B ping RTT, bulk throughput both directions (16 KiB blocks, `--mib`), setup time, direct diagnostics | cases `raw`, `codec`, `tls-split` (separate length write + payload write, like wire.rs), `tls-combined` (one write); `--nagle`, `--auto`, `--local`, `--consumer-server`, `--host` (registration time only) |
| `examples/deployed.rs` | deployed-relay acceptance; issues an ephemeral key; `serve` prints host status/s and writes `controller.json`; `selftest` runs a local H.264 Viewer **without** feedback for Auto and RelayOnly: `unique_receive_fps`, `receive_mbps`, `dropped` | `--feedback-log` enables only `pb_mapper_desktop::feedback=debug`; `--max-width/--max-height/--fps/--bitrate/--control/--tcp` |
| `examples/viewer_probe.rs` | Viewer **with** presentation feedback against `controller.json`, simulated renderer: fixed `delay` ms then ACK newest every `ack` ms; prints unique fps, keyframes, Mbps, host `capture_fps`, `bitrate`, `dropped`, `frame_ack_ms`, steady-state receive gap p50/p95/max after 5 s | not a UI presentation measurement |
| `examples/input_acceptance.rs` | Windows ordinary-desktop native input into an owned window via the full SDK path (H.264 1280x720@30, 2 Mbps): Unicode text, move, button down/up, held button released on disconnect (`disconnect_release_ms`) | `Guarded` backend aborts on foreground/geometry change |

---

## 3. Data/control flow

### 3.1 Threads/tasks and what each owns (host process)

| # | Where | Runs on | Owns |
| --- | --- | --- | --- |
| A | Accept task, host.rs:139-295 | caller's tokio runtime (`tokio::spawn`) | `TcpListener`, `JoinSet` of sessions, `Semaphore(1)`, clones of tls/token/factory/policy/status/admission |
| B | Session task, host.rs:150-290 | same runtime (`sessions.spawn`) | TLS stream (split into `read`/`write`), `Worker` handle, `pending` frame slot; runs futures `receive` (178-189) and `send` (190-274) concurrently via `select!` (275) -- **one task, two futures**, so a slow `write` and a blocked `worker.command().await` interleave but each future is strictly sequential internally |
| C | Capture/input/encode thread, worker.rs:90-281 | `tokio::task::spawn_blocking` | `Box<dyn Backend>` (Enigo, D3D/MF encoder, desktop binding), `Engine`, `Waiter`, command receiver; writes `status`, `frames`/`cursor` watches, pushes `media` queue, mutates `Feedback` |
| D | pb-mapper-client registration tasks | caller's runtime | relay/direct tunnel, forwards each viewer connection to the loopback listener |
| E | (privileged) `RemoteBackend` runtime | current-thread runtime `block_on` **inside thread C** (client.rs:38-40, 64) | named pipe to the service |
| F | MF async MFT internal threads | driver | encoder work; host polls events with `MF_EVENT_FLAG_NO_WAIT` (video/windows.rs:644-664) |

Helper side (separate processes): service (SCM thread + current-thread
runtime + per-session pipe proxy + optional background supervisor actor
running its own A-D), worker process (pipe exchange runtime + one backend
`std::thread` + `Events` hook thread).

Shared-state ownership: `status: Arc<Mutex<SessionStatus>>` written by C
every iteration (`*status.lock() = engine.state.clone()`, worker.rs:275, also
211, 106, 124, 201, 278-280), read by B (generation/cursor, host.rs:219,
229-234, 194, 203, 251) and by `Host::status()`. `Feedback` mutated by C
(`captured`, `budget`, `presented`, `request_keyframe`, `context_changed`),
B-send (`admit`, `stalled`, `start_probe`, `changed`) and B-receive
(`probe_received`). `media::Queue`: C pushes, B pops/resynchronizes.

### 3.2 Session setup

1. Viewer's tunnel connection lands on the loopback listener; accept only if
   `sessions.len() < 8` (host.rs:146).
2. `set_nodelay(true)` on the accepted socket (155), TLS accept (156).
3. First message must be `Command::Authenticate` (157); `codecs.len() <= 2`
   (158); codec = H264 if offered else JPEG (159); token compared as
   `SHA256(presented) ct_eq SHA256(token)` (160-162); `video.unwrap_or_default()`
   validated (163-164); `active.try_acquire_owned()` else "a viewer is already
   connected" (165). Whole handshake bounded by `IO_TIMEOUT` 3 s and local
   stop (168-172). Any failure just drops the connection (172).
4. `Worker::start_video(factory, allowed, status, codec, preferences,
   presentation_feedback, admission)` (174).
5. Worker thread init (worker.rs:91-127): `Waiter::new`, validate prefs,
   `factory()`, `backend.configure_video(prefs)`. On failure: status
   `capture_unavailable` (generation 1) pushed every 125 ms and every command
   answered with the error until stop (99-126).
6. Send future: if feedback, sends the **startup probe** first: a clone of
   `status` with `roundtrip_probe = Some(feedback.start_probe())` (always 1)
   + empty bytes (193-200).

### 3.3 Worker loop (worker.rs:138-276), one iteration

1. Policy epoch changed -> `engine.release_control()` (139-143) (invalidates
   generation, ends control, drops lease). `state.control_allowed` refreshed
   (144). `release_if_stale()` (145): if controlling and the last
   presented/input frame is not in `recent` within 2 s (or wrong generation)
   -> `backend.release()` (402-414).
2. Wait for a command up to `min(next_frame, next_cursor) - now`, capped at
   **10 ms** (146-152). Stop check right after the wait so a queued input is
   never executed after cancellation (153-157).
3. Dispatch one command (158-204) -- see 3.6.
4. Cursor tick (206-220): every **16 ms** (200 ms while
   `capture_unavailable`): `sample_cursor()` (502-520: `geometry()` +
   `backend.cursor(context)`); if `state == "streaming"` and
   `(generation, cursor)` changed -> write `status` and
   `cursor_tx.send_replace(Some(state))`.
5. Frame tick (221-274) when `now >= next_frame`:
   - with feedback: `sync_feedback` (context change -> `feedback.
     context_changed(unavailable)`, 359-368); `(prefs, bitrate, waiting) =
     feedback.budget()`; `engine.bitrate = bitrate`; **`paused = waiting`**;
     if `prefs != configured` -> `backend.configure_video(prefs)`, then
     `outgoing.resynchronize()` + `engine.keyframe = true` (228-240; error ->
     break loop);
   - without feedback: `engine.bitrate = rate.load()` (242);
   - `engine.keyframe |= outgoing.take_keyframe_request()` (251),
     `dropped_frames = outgoing.dropped()` (252);
   - `engine.capture_video(!paused)` (254): submit=true ->
     `backend.capture_video(ctx, codec, keyframe, bitrate)`; submit=false ->
     `backend.drain_video(ctx)` (only completes already-submitted encoder
     work). Then `backend.cursor(ctx)` for the frame (534).
   - On a frame: `feedback.captured(&frame)` (records `ready` time, 255-258),
     `outgoing.push(frame)`, `frames_tx.send_replace` (259-260).
   - `next_frame = max(started + period, now)` with period **200 ms**
     (unavailable) / **2 ms** (H.264 poll; real pacing is inside the encoder)
     / **125 ms** (JPEG = 8 fps) (262-273).
6. `*status.lock() = engine.state.clone()` (275).
7. On exit: `backend.release()`, status `waiting`, `controlling=false`
   (277-280).

`Engine::capture_video` (521-585): `geometry()` first (474-496: context
changed or recovering from unavailable -> `invalidate()`, set
`display`/`desktop`, state `connecting`; error -> invalidate once, state
`capture_unavailable`). A non-empty frame <= `MAX_FRAME` clears
`keyframe` if it is an IDR (542-544), sets `video`, `cursor`, `state =
streaming`, `message = input_failure or ""`, `frame_id += 1`, `frames += 1`,
`bytes += len`, pushes `(frame_id, now)` into `recent` (cap 256),
`capture_fps` over >=1 s windows excluding `duplicate` (557-565). Errors or
oversized frames produce an **empty-bytes frame** with state
`capture_unavailable` and the message (569-579). `Ok(None)` (encoder not
ready) produces nothing.

### 3.4 Capture, pacing and encoder (src/video/windows.rs)

Creation `Encoder::new_with_size(display, fps, bitrate, max_w, max_h)`
(142-243):
- bounds fps 1..=120, bitrate 256k..=80M, size 2..7680 x 2..4320 (149-154);
- `DxgiDuplicationApi::new_options(monitor, [Bgra8])` (161-162); rotated
  displays rejected (164-168);
- output size `scaled_size` = fit inside max, preserve aspect, never upscale,
  even dims (174, 429-448);
- `ID3D11Multithread::SetMultithreadProtected(true)` (176-179); DXGI device
  manager (180-188);
- `Codec::new` (189, 462-544): `MFTEnum2(VIDEO_ENCODER, HARDWARE |
  SORTANDFILTER, NV12 -> H264)` restricted to the capture adapter LUID; tries
  each candidate.
- `Codec::activate` (546-636) configuration:
  - attributes `MF_TRANSFORM_ASYNC_UNLOCK = 1`, `MF_LOW_LATENCY = 1`; requires
    `MF_SA_D3D11_AWARE`; `MFT_MESSAGE_SET_D3D_MANAGER` (561-572)
  - `CODECAPI_AVLowLatencyMode = true` (**required**, 598)
  - `CODECAPI_AVEncMPVDefaultBPictureCount = 0` (optional, ignored on error, 601)
  - `CODECAPI_AVEncCommonRateControlMode = CBR` (optional, 602-605)
  - `CODECAPI_AVEncCommonQualityVsSpeed = 0` (optional; 0 = fastest/lowest quality, 606)
  - `CODECAPI_AVEncMPVGOPSize = fps` (optional; **IDR every second**, 607)
  - output type: H264, `MF_MT_AVG_BITRATE = bitrate`, `MF_MT_MPEG2_PROFILE =
    Baseline`, frame size, `MF_MT_FRAME_RATE = fps/1`, PAR 1:1, progressive,
    BT.709 primaries/matrix, nominal range 16-235 (608-624, 983-1016); input
    NV12 same size; BEGIN_STREAMING + START_OF_STREAM; `update_header` caches
    SPS/PPS (634, 734-755).
- `Converter` (799-974): D3D11 video processor BGRA(full RGB 709) -> NV12
  (studio YCbCr 709), `D3D11_VIDEO_USAGE_OPTIMAL_SPEED`, scaling source rect
  -> dest rect.
- Sample allocator: 4 initial / 6 max NV12 render-target samples (205-219).
- `force_keyframe = true`, `wait_independent_output = true` initially (238-239).

`next_frame(force_keyframe)` (333-426) -- called every ~2 ms by the worker:
1. `force_keyframe |= arg`; `drain()` first (335).
2. Skip submission if `pending.len() >= MAX_IN_FLIGHT (3)` or
   `need_input == 0` or `now < next_capture` (336-341).
3. `acquire_next_frame(0)` (non-blocking); if `LastPresentTime` changed, copy
   into the persistent `latest` texture, `unique_captures += 1` (342-375).
4. Static desktop: if nothing new, no forced IDR and last submit < **250 ms**
   -> skip; otherwise re-encode `latest` as a **duplicate liveness frame**
   (~4 fps when static) (376-384).
5. `AllocateSample` (allocator empty -> skip), convert, `pts_us =
   created.elapsed()`, sample time `pts*10`, duration `1e7/fps` (388-400).
6. Forced IDR: `CODECAPI_AVEncVideoForceKeyFrame = 1` (401-404);
   `ProcessInput`; `need_input -= 1`.
7. Pacing: `next_capture = max(next_capture + 1/fps, last_sent)` -- keeps the
   target clock across refreshes, never bursts to catch up (414-419).
8. `pending.push_back((pts_us, at, duplicate))`.

`drain()` (275-331): poll MF events (NeedInput/HaveOutput counters,
644-664); on output: `GetSampleTime` must equal the oldest pending pts (no
reordering, 286-290); Annex B check + SPS/PPS prepended to IDRs lacking them
(`annex_b`, 756-786); output older than **250 ms** is discarded and forces an
IDR (`wait_independent_output`), a P-frame while waiting for an independent
output is also discarded (294-301); errors: pending head > **2 s** or no
submission within **3 s** of creation -> "hardware video encoder stalled"
(322-329). `set_bitrate` -> `CODECAPI_AVEncCommonMeanBitRate` only when
changed (252-264).

Note: encoder `fps` = `VideoPreferences.max_fps` at creation; any fps or size
change goes through `configure_video` -> encoder dropped -> rebuilt with a new
IDR (windows.rs:193-200, 284-300).

### 3.5 Flow control and congestion (src/congestion.rs, host.rs send loop)

Constants (congestion.rs:11-26):

| Name | Value | Use |
| --- | --- | --- |
| `INITIAL_WINDOW` | 64 KiB | starting byte window |
| `MIN_WINDOW` | 16 KiB | floor; also baseline-sample size limit |
| `MAX_WINDOW` | 512 KiB | ceiling |
| `MAX_FRAMES` | `media::MAX_QUEUED_FRAMES` = 8 | max un-presented frames |
| `MAX_INDEPENDENT_FRAME` | 256 KiB | any single admitted message larger -> **discard + halve bitrate** |
| `MIN_BITRATE` | 256_000 | floor |
| `MAX_READY_AGE` | 250 ms | produced-to-admitted age limit -> discard |
| `CONGESTED` | 250 ms | minimum congestion threshold |
| `INITIAL_BASELINE_LIMIT` | 250 ms | cap on uncalibrated baseline |
| `OBSERVATION_INTERVAL` | 1 s | delay averaging / hold |
| `RECOVERY_HOLD` | 4 s | healthy time before +25% |
| `RESOLUTION_HOLD` | 5 s | before quality upshift |
| stall-stop | 4 s (stalled_at, 375-380) | host sends `stopped` and ends session |

Send loop (host.rs:201-272), each pass:
1. `feedback.stalled()` (no presentation ACK progress for 4 s while frames
   outstanding) -> send status `state:"stopped"`, message "Renderer stopped
   acknowledging presented frames; reconnect the viewer", end session
   (202-212).
2. Cursor watch changed -> write cursor status (if its generation equals
   current) **before any video**, bypassing admission (213-226).
3. `pending = media.pop()` if empty; overwrite its `status.cursor` with the
   current cursor when generations match (227-235).
4. Admission (feedback only): size = `serde_json::to_vec(status).len() +
   bytes.len() + 8` (status serialized twice per frame) -> `Send` / `Wait`
   (`waiting = true`) / `Discard` (`pending = None;
   media.resynchronize()` -> IDR) (236-245).
5. Nothing to send or waiting: if feedback and >= 1 s since last write, send
   a liveness status (empty bytes; does not renew input freshness, 246-257);
   then `select!` on cursor change / frames change / `feedback.changed` /
   **20 ms sleep** (258); non-feedback waits on cursor/frames only (260).
6. Else write status JSON + bytes with `IO_TIMEOUT` (3 s; a slow reader that
   cannot take one message in 3 s ends the session), then `worker.sent(...)`
   (264-271).

`admit_at` (163-230), in order:
- empty bytes -> `Send` (status-only carries no credit);
- frame not in `ready` -> `Discard`; `now - ready > 250 ms` -> `Discard`;
- `recovering && sent.empty && H264 && !keyframe` -> `Discard` (stale P-frame
  after IDR request);
- `bytes > 256 KiB` -> `reduce(bitrate/2)`, `Discard`;
- if `sent` non-empty and (`recovering` or `sent.len() >= 8` or
  `bytes_in_flight + bytes > window`) -> `Wait`. **Only an empty ledger
  admits one over-window unit**, so a large IDR waits for every in-flight
  frame to be presented;
- else record `Sent` (baseline_sample = non-key, in-flight <= 16 KiB, no IDR
  in flight), set `unacknowledged_since` if unset, and set `waiting =
  recovering || bytes >= window || sent.len() >= 8` (227-228) -- i.e. capture
  is paused as soon as the window fills.

`presented_at` (257-319): cumulative -- finds `(generation, id)` in `sent`
and pops everything up to it; latency = now - send time (excludes credit
wait); `unacknowledged_since = now` if frames remain else None; clears
`recovering` and `waiting`; baseline: first baseline_sample when uncalibrated,
else `min(old, sample or latency)` (uncalibrated capped at 250 ms); window
doubles toward `desired_window` when first ACK or latency <
`congested_after`; `observe_delay` except on first ACK.

`observe_delay` (383-422): average latency over >= 1 s; if avg >=
`congested_after()` = `max(250 ms, baseline + 100 ms)` (465-467) and that has
persisted >= 1 s -> `reduce(bitrate/2)` ("decrease"); else after 4 s healthy and
>= 1 s since last adjust -> `bitrate = min(ceiling, bitrate*1.25 + 64 kbps)`
("probe"). Emits `desktop_feedback_interval` debug trace (423-458; fields
`action, samples, bitrate, quality, baseline_ms, baseline_calibrated,
delay_ms, congested_after_ms, window_bytes, desired_window_bytes,
outstanding_frames, outstanding_bytes, oldest/newest_frame_id, recovering,
waiting, last_ack_generation/frame_id, last_ack_age_ms,
unacknowledged_age_ms, healthy_ms`).

`desired_window` (459-464) = `bitrate * max(baseline + 50 ms, 125 ms) / 8`,
clamped [16 KiB, 512 KiB]. `reduce` (468-474): clamp bitrate [256k,
ceiling], `window = max(16 KiB, min(window/2, desired))`, reset
`healthy_since`/`upscale_since`.

`budget_at` (323-371), called every frame tick:
- **ack_stall**: if unacknowledged for >= `max(1 s, congested_after + 250
  ms)` and >= 1 s since last adjust -> `bitrate = bitrate*2/3` (325-335);
- **quality ladder**: `quality(bitrate)` = 0 (<500k), 1 (<1M), 2 (<2M), 3
  (<4M), 4 (>=4M) (477-485); downshift immediately when bitrate < 0.8 x
  `minimum_bitrate(current)`; upshift when bitrate >= 1.25 x
  `minimum_bitrate(q+1)`, healthy, no congestion, held 5 s (336-358);
  mapping 0 -> 640x360@30, 1 -> 640x360@60, 2 -> 960x540@60, 3 ->
  1280x720@60, 4 -> ceiling, each min'ed with the ceiling (359-369);
- returns `(prefs, bitrate, waiting)`.

`request_keyframe` (234-249): if already `recovering` -> false (repeated
requests neither add credit nor extend deadlines); else `recovering = true`,
clears `ready`/`sent`/bytes/waiting (keeps `unacknowledged_since`).
`context_changed(unavailable)` (147-159): clears ledgers/waiting/recovering;
if unavailable, clears `unacknowledged_since` (no stall while the desktop is
unavailable). `start_probe`/`probe_received_at` (121-144): **one-shot,
nonce always 1**, RTT <= 3 s sets `baseline` and `baseline_calibrated`; never
retires frames or grants credit.

Non-feedback sessions: no admission, no pause; only `Worker::sent` lowers
`bitrate` from write time (worker.rs:294-304); host `media::Queue` overflow
(8 frames / 8 MiB) drops + IDR.

### 3.6 Commands from the viewer (host.rs:178-189 -> worker.rs:158-204)

`receive` reads a JSON command with a 3 s timeout (viewer heartbeats `ping`
every 1 s, viewer.rs:294); clean EOF ends normally; any parse error (unknown
`op`, unknown field, unknown `Key`) **ends the session** (180, `?`).
`Probe` is handled inline (`feedback.probe_received`, 181-184). Every other
command goes through `worker.command(command).await` -- the receive future
**waits for the capture thread to process it** (<= 2 s) before reading the
next command; its error result is discarded (187) and surfaces in the next
status `message`.

On the capture thread:
- `Control{enabled}` -> `Engine::control(enabled, allowed)` (420-442):
  repeated enable while controlling is a no-op; disable while not controlling
  only releases; otherwise `release_control()` (new generation, IDR, lease
  dropped), clear `input_failure`, and on enable require `allowed` (else error)
  and acquire the `ControlAdmission` lease, `controlling = true`.
- `Input{event}` -> `Engine::input(event, allowed && policy unchanged)`
  (586-621), see 5.2.
- `Release` -> `backend.release()`.
- `Ping` -> nothing.
- `Keyframe` -> if no feedback or `feedback.request_keyframe()` returned true:
  `outgoing.resynchronize()`, `engine.keyframe = true` (172-181).
- `Presented{generation, frame_id}` -> `feedback.presented(...)` (**credit
  returns here, on the capture thread**) then `Engine::presentation` (443-473):
  frame must be the current generation and in `recent` < 2 s old, else
  `release()` + error "stale presentation"; records `frame_ack_ms` once per
  frame (ready -> ACK time); sets `presented`; if it advances and controlling
  -> `backend.presented(context)` (helper: renews its 2 s input lease).
- `Authenticate`/`Probe` -> error (191-193).
- Any error -> `report_command_failure` (message = sticky input failure if
  any, 349-357).

### 3.7 Input injection path

Viewer `Command::Input` -> TLS -> session task `receive` -> `SyncSender`
(cap 16) -> `Wake` event -> capture thread (C) picks it up at the next loop
wait (after any in-progress `capture_video`/`drain_video`/cursor sampling, and
for the helper after its pipe round trips) -> `Engine::input` ->
`backend.input_in_context(context, input)`:
- ordinary: `Desktop::input_in_context` on thread C (process's own desktop;
  no `SetThreadDesktop`), `SetCursorPos` / Enigo;
- privileged: `RemoteBackend::input_in_context` (client.rs:160-169) ->
  `Request::Input{generation, sequence, input}` -> service proxy -> worker
  process backend thread bound to the input desktop -> privileged
  `Desktop::input_in_context`.
Then the reply returns to `receive`, which only then reads the next command.

### 3.8 Cursor path

Sampled on thread C: per frame (worker.rs:534) and every 16 ms
(206-220). Published as a whole `SessionStatus` clone through `cursor`
watch (latest value only). The send loop writes it as a status-only message
(empty bytes) ahead of video and also stamps the newest cursor into the
pending frame's status (213-235). Not admitted, not counted as credit, not
input freshness. No echo suppression for the controlling operator. Helper
path: cursor is whatever the last `Context`/`Capture`/`CaptureVideo`/
`DrainVideo` response carried (helper/worker.rs:320-326, 391-395).

### 3.9 Generations, epochs, sequences

| Counter | Where | Advances when |
| --- | --- | --- |
| `SessionStatus.generation` | Engine, starts 1 (worker.rs:374) | `invalidate()` (392-401): context change, entering `capture_unavailable`, `release_control()` (grant, revoke, policy epoch change, native input failure) |
| `DesktopContext.generation` | ordinary: always 0; privileged: `BindingState.generation` (helper/worker.rs:99-123) | desktop name / `EPOCH` / console lock-state change on `enter` |
| `EPOCH` (static) | helper/worker.rs:8, 19-29 | WinEvent `EVENT_SYSTEM_DESKTOPSWITCH` |
| `Policy` epoch | worker.rs:17-37 | every `set_control_allowed` |
| `frame_id` | Engine (550) | every produced non-empty frame; never resets |
| input `sequence` | Engine.sequence (587-590); helper keeps its own (helper/worker.rs:297, client.rs:162) | must strictly increase per session |
| `pts_us` | Encoder (394) | microseconds since encoder creation |
| probe nonce | congestion.rs:121-124 | constant 1 |

`invalidate()` also: `backend.release()`, clear `recent`, `presented`,
`acknowledged`, `cursor`, `frame_ack_ms`, sets `keyframe = true`.

### 3.10 Keyframe (IDR) triggers today (all of them)

1. New encoder: first frame (`force_keyframe: true`, video/windows.rs:238) --
   on session start, context change (windows.rs:284-300), `configure_video`
   change (windows.rs:193-200), encoder error (`finish_video`,
   windows.rs:148-164), ordinary desktop unavailable or privileged desktop
   switch (`prepare` drops the encoder, windows.rs:165-181).
2. `Engine.keyframe = true` initially (worker.rs:383) and on every
   `invalidate()` (400) -- control grant/revoke etc.
3. Viewer `Command::Keyframe` (gated by `request_keyframe`) (172-181).
4. Host `media::Queue` overflow or `Discard` -> `resynchronize` ->
   `take_keyframe_request` (media.rs:47-62, 100-109; worker.rs:251;
   host.rs:243).
5. Quality-ladder/preference change -> resynchronize + keyframe (228-234).
6. Encoder late output (>250 ms) / P-frame after expired output
   (video/windows.rs:294-301).
7. **Periodic: `CODECAPI_AVEncMPVGOPSize = fps`** (607) -> one IDR per
   second regardless of anything else.
`Engine.keyframe` is cleared only when an IDR is actually output
(worker.rs:542-544), so a periodic IDR also satisfies a pending request.

---

## 4. Contracts and wire/channel formats

### 4.1 Transport stack

pb-mapper tunnel (TCP service, `codec: true`, relay TCP or QUIC carrier, or
direct) -> loopback TCP (`TCP_NODELAY`) -> TLS 1.3 (`desktop.pb-mapper.invalid`,
ALPN `pb-mapper/desktop/1`, pinned cert) -> length-prefixed records (u32 BE
length). Viewer -> host: one JSON record per `Command` (<= 8192 bytes).
Host -> viewer: **every message is two records**: `SessionStatus` JSON
(<= 8192) then a bytes record (<= 4 MiB, may be empty). Each record write
flushes (wire.rs:65-76), i.e. 2 flushes / TLS record groups per message.

### 4.2 Host -> viewer `SessionStatus` (lib.rs:125-165)

```
state: String            // "connecting" | "streaming" | "capture_unavailable" | "waiting" | "stopped"
message: String
control_allowed: bool
controlling: bool
generation: u64
frame_id: u64
display: Option<Display {id,name,x,y,width,height}>
path: String             // host leaves "", viewer fills "direct"/"relay"
direct_stage?: String    // viewer-local, skip if None
direct_failure?: String  // viewer-local
frames: u64              // cumulative produced frames
bytes: u64               // cumulative produced bytes
desktop: String          // "ordinary" | "secure" (serde default)
video: VideoFormat {codec:"jpeg"|"h264", width, height, keyframe, pts_us, duplicate}
capture_fps: f32         // host unique (non-duplicate) fps
receive_fps: f32         // viewer-local
dropped_frames: u64      // host media-queue drops (+ viewer adds its own)
bitrate: u32             // current encoder target
cursor?: CursorPosition {x,y,visible}
frame_ack_ms: f32        // frame-ready -> first presentation ACK (host-measured)
roundtrip_probe?: u64    // only on the startup probe message
```

Message kinds, distinguished by content:

| Kind | state | bytes | Sent from | Credit |
| --- | --- | --- | --- | --- |
| startup probe | current status (any) + `roundtrip_probe: 1` | empty | host.rs:193-200, feedback only, once | no |
| video frame | `streaming` | access unit (H.264 Annex B or JPEG) | host.rs:264-271 | admitted (feedback) |
| cursor/status update | `streaming` | empty | host.rs:216-226 | no |
| liveness | current | empty | host.rs:248-257 (feedback, waiting/idle >=1 s) | no |
| capture unavailable | `capture_unavailable` | empty | worker via media queue | always `Send` |
| stopped | `stopped` + reason | empty | host.rs:202-212 (4 s ACK stall) | n/a |

Viewer contract (other end): a `roundtrip_probe` with empty bytes is echoed
as `Command::Probe` and not shown (viewer.rs:225-235); empty-bytes
`streaming` messages update metadata only (`push_status`, viewer.rs:269-280);
viewer rejects H.264 frames larger than its own `max_width/max_height`
(viewer.rs:244-251) and display > 32768 (239-243).

### 4.3 Viewer -> host `Command` (wire.rs:9-37; tag `op`, snake_case, `deny_unknown_fields`)

```
{"op":"authenticate","token":"<43 chars>","codecs":["h264","jpeg"],
 "video":{"max_width":..,"max_height":..,"max_fps":..,"bitrate":..},   // optional
 "presentation_feedback":true}                                        // omitted when false
{"op":"control","enabled":true}
{"op":"input","event":{"generation":G,"frame_id":F,"sequence":S,"input":INPUT}}
{"op":"release"}
{"op":"ping"}
{"op":"probe","nonce":1}
{"op":"keyframe"}
{"op":"presented","generation":G,"frame_id":F}
```

`INPUT` (lib.rs:246-287; tag `kind`, `deny_unknown_fields`):
`{"kind":"move","x":u32,"y":u32}`, `{"kind":"button","button":"left|right|middle","down":bool}`,
`{"kind":"wheel","horizontal":i32,"vertical":i32}`,
`{"kind":"key","key":K,"down":bool}`, `{"kind":"text","text":"..."}` with
`K` in `shift control alt meta enter escape tab backspace delete left right
up down home end page_up page_down a c v x z`.

Compatibility facts: adding a field to `Command`/`Input`/`VideoPreferences`
or a new `op`/`kind`/`Key` value makes **an older host fail to parse and end
the session**; adding fields to `SessionStatus` is safe for older viewers
(no `deny_unknown_fields`, defaults on most fields). The only existing
capability negotiation is `presentation_feedback` (viewer -> host) and the
`roundtrip_probe` field (host -> viewer).

### 4.4 Internal channels

| Channel | Type | Producer -> consumer |
| --- | --- | --- |
| `requests` | `std::sync::mpsc::sync_channel::<(Command, oneshot::Sender<Result<()>>)>(16)` | session receive -> capture thread |
| `frames` | `watch::Sender<Option<Frame>>` | capture thread -> send loop wake, tests |
| `cursor` | `watch::Sender<Option<SessionStatus>>` | capture thread -> send loop |
| `media` | `Arc<media::Queue>` | capture thread push -> send loop pop |
| `feedback.changed` | `tokio::sync::Notify` (`notify_one`) | ACK/keyframe/context -> send loop |
| `status` | `Arc<parking_lot::Mutex<SessionStatus>>` | capture thread -> send loop, `Host::status()` |
| `bitrate` | `Arc<AtomicU32>` | send loop (`sent`) -> capture thread (non-feedback) |
| helper pipe | JSON `Request` -> JSON `Response` + bytes record, 8 KiB pipe buffers (ipc.rs:25-26) | RemoteBackend <-> service proxy <-> worker process |

---

## 5. Invariants and security boundaries

### 5.1 Access

- Invitation token is a 256-bit bearer capability; compared in constant time
  over SHA-256 digests (host.rs:160-162). Relay credentials grant nothing
  (tests/session.rs:99-119). TLS pins the invitation's certificate, TLS 1.3
  only, no early data (security.rs:68-158).
- One active viewer per Host (`Semaphore(1)`, host.rs:140, 165); <= 8
  concurrent sessions/handshakes (146); 3 s handshake bound (170).
- Viewing never acquires `ControlAdmission` (worker.rs:826-869 test).
- Revocation (`set_control_allowed(false)`, `stop`, `Drop`) bumps the
  Policy epoch; the worker releases held input and invalidates the generation
  at its next loop top (<= ~10 ms plus any blocking backend call)
  (worker.rs:139-143). Input is additionally gated by
  `allowed.allowed() && allowed.get() == policy` (165).

### 5.2 Input authorization (fail closed) -- `Engine::input` (worker.rs:586-621)

1. `sequence` must strictly increase (updated before later checks) -- no
   duplicate/reordered input, and queued input never replays across
   sessions (each session has a fresh Worker and channel; stop check at
   153-157).
2. `geometry()` refreshed (context change -> new generation).
3. Must be allowed and `controlling`.
4. `state == "streaming"`, `event.generation == state.generation`, and
   `event.frame_id` in `recent` with age < **2 s** (frame production time) --
   else `release()` + "screen changed or is stale".
5. Records `presented = (generation, frame_id)`.
6. `backend.input_in_context(context, input)` re-checks the backend context
   at the injection boundary and never rebinds after checking
   (lib.rs:362-369, windows.rs:328-336); failure -> `release_control()` +
   sticky `input_failed: ...` until a new explicit control request
   (worker.rs:610-619).
- Held input is released when: the last presented/input frame is not fresh
  (`release_if_stale`, 2 s), policy changes, desktop changes, stop, Engine
  drop, Desktop drop, helper worker idle > 2 s, helper pipe EOF.
- `Presented` is input freshness evidence only if the frame is current and
  < 2 s old (443-457); presentation ACKs also renew the helper's input lease
  (`backend.presented`, 466-471). **Credit (congestion) and input freshness
  both ride `Presented` today.**
- Input validation: pointer inside display, wheel |v| <= 100, text <= 1024
  bytes without NUL (lib.rs:377-395); helper revalidates (client.rs:161,
  helper/worker.rs:377-382).

### 5.3 Desktop binding

- Ordinary host never injects into Winlogon/UAC: `available()` requires the
  input desktop to be `Default` (windows.rs:44-66) on every `context()`.
- Privileged worker: SYSTEM restricted token in the owner's attested login
  session (`SessionIdentity {session, logon_time, user, domain}` checked on
  every `enter`/`changed`, helper/os.rs:352-357, 904-929); binds with
  `SetThreadDesktop` only after releasing input and dropping desktop-affine
  DXGI/MF/COM objects (helper/worker.rs:175-237); any name/epoch/lock change
  advances `DesktopContext.generation`, which changes the Engine generation
  and invalidates all outstanding frames and input.
- Cursor positions are never disclosed for another monitor or a hidden
  cursor (windows.rs:488-504); cursor sampling fails on desktop change rather
  than reporting another desktop's pointer (lib.rs:316-321).
- Ctrl+Alt+Del / SAS: not representable in `Key` and not implemented
  (Enigo cannot generate SAS).

### 5.4 Resource bounds

`MAX_JSON` 8 KiB, `MAX_FRAME` 4 MiB, `MAX_ACCESS_UNIT` 4 MiB, media queue 8
frames / 8 MiB, `recent` 256, `ready` 256, `sent` <= 8, encoder in-flight 3,
command channel 16, sessions 8. Every network write/read has a 3 s timeout.
Local stop never waits for a slow peer (host.rs:275, 284-288;
tests.rs:76-173).

### 5.5 Encoder/threading invariants

Encoder and COM/MF runtime are created, used and dropped on one thread
(video/windows.rs:76-108, 266-271). Draining never captures or submits
(273-275). Baseline profile => output order equals input order (checked,
286-290). A late (>250 ms) output or a P-frame after an expired output is
never exported (294-301).

---

## 6. Tests covering it

Unit tests (default `cargo test -p pb-mapper-desktop`):

| Test | File:line | Covers |
| --- | --- | --- |
| `admission_bounds_startup_and_rejects_unearned_acknowledgments` | congestion.rs:523 | over-window IDR admitted only with empty ledger; foreign ACKs ignored; 250 ms age discard; empty bytes always Send |
| `an_unpresented_flight_cannot_overflow_the_receiver_queue` | congestion.rs:547 | **MAX_FRAMES == viewer queue bound** invariant |
| `keyframe_recovery_retires_old_frames_without_resetting_unacknowledged_limit` | congestion.rs:576 | `request_keyframe` single credit, stale P discard, stall deadline not extended |
| `replacement_idr_exclusively_holds_credit_until_its_actual_ack` | congestion.rs:626 | recovering IDR blocks dependents until its ACK |
| `unavailable_capture_retires_debt_but_new_video_still_requires_acks` | congestion.rs:655 | `context_changed` semantics |
| `renderer_baseline_and_congestion_recovery_use_real_ack_progress` | congestion.rs:683 | one spike != congestion; persistent delay halves; recovery > 4 Mbps |
| `startup_probe_measures_path_without_retiring_presentation_debt` | congestion.rs:729 | probe sets baseline only |
| `slow_first_idr_does_not_normalize_persistent_video_delay` | congestion.rs:756 | 250 ms uncalibrated cap |
| `cumulative_small_frame_acks_calibrate_wan_delay_and_recover_through_jitter` | congestion.rs:776 | cumulative ACK baseline on WAN |
| `resolution_recovers_with_headroom_without_chattering_at_rate_boundaries` | congestion.rs:815 | **quality ladder** hysteresis |
| `input_requires_control_current_geometry_and_unique_sequence` | worker.rs:677 | input auth rules, frame_ack_ms only from Presented, stale release, desktop change invalidation |
| `native_input_failure_ends_control_and_survives_new_video` | worker.rs:741 | sticky input failure |
| `observation_never_acquires_write_admission_and_revocation_releases_it` | worker.rs:825 | ControlAdmission lease lifecycle, idempotent control |
| `unavailable_backend_reports_failure_and_stops` | worker.rs:871 | factory failure reporting |
| `presentation_backpressure_drains_encoder_without_new_capture` | worker.rs:897 | **pause = drain only** while waiting |
| `unavailable_desktop_retires_old_ack_debt_and_resumes_control` | worker.rs:1021 | cursor independent of credit, unavailable -> no stall, old ACK cannot release new IDR credit |
| `decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture` | worker.rs:1242 | `Keyframe` command recovery |
| `overflow_discards_dependent_frames_until_idr` | media.rs:132 | queue overflow + generation switch |
| `status_is_coalesced_without_reordering_or_invalidating_video` | media.rs:152 | status slot semantics |
| `clean_tls_close_is_eof_only_between_messages` | wire.rs:116 | EOF framing |
| `remembered_invitation_survives_restart_but_not_revocation` | host.rs:374 | identity persistence |
| `wake_preserves_queued_input_and_interrupts_idle_stop` | scheduler.rs:151 (Windows) | waiter |
| `failed_native_input_retains_the_release_obligation` | windows.rs:477 | held-set |
| `pointer_coordinates_are_physical_and_confined_to_the_selected_display` | windows.rs:569 | cursor privacy |
| `gpu_output_dimensions_fit_renderer_without_upscaling` | video/windows.rs:1112 | `scaled_size` |
| `parameter_sets_survive_partial_updates_and_emulation_prevention` | video/windows.rs:1119 | SPS/PPS cache |
| `tls_rejects_an_unpaired_host_identity` | tests.rs:15 | pinning |
| `blocked_video_does_not_block_local_stop` | tests.rs:76 | slow reader, 3 MiB JPEG frames, stop < 2 s, prefs reach backend |
| `older_authentication_defaults_video_limits_and_rejects_unbounded_preferences` | tests.rs:175 | Authenticate default/validation |
| helper: `capture_requires_installed_running_service_and_approved_caller`, `desktop_transition_releases_before_binding_and_retries_without_committing_failure`, `desktop_hook_exits_when_startup_is_abandoned_or_owner_drops`, `lock_keeps_approved_login_but_logoff_and_switch_revoke_it`; ignored `bound_input_desktop_allows_send_input` | helper/mod.rs:219, helper/worker.rs:466-599, helper/service.rs:338 | binding/session rules |

Integration (`tests/session.rs`):
- `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown`
  (73): bad token sees no frames and no capture; input needs control and a
  post-grant frame; 1000 releases coalesce; duplicate sequence rejected;
  revoke; direct survives relay stop; teardown drops backend once.
- `cursor_crosses_a_full_video_window_without_refreshing_the_picture` (206):
  with a never-ACKed 128 KiB IDR owning all credit, cursor updates still flow,
  viewer goes `stale` after 2 s but keeps receiving cursor.

Ignored acceptance (`src/slow_link.rs`, test-only module; run with
`-- --ignored --nocapture`):
- `delayed_feedback_does_not_starve_async_encoder` (158): real relay/TLS,
  bounded delaying TCP proxy (`delayed_copy` 16-47, 32 x 16 KiB),
  `AsyncVideo` (20 ms encode latency, 250 ms expiry, <=2 pending) over
  `SyntheticVideo` sized `bitrate/fps/8` (x4 for IDR); cases low_delay 20 ms,
  fixed_wan 350 ms, slow_consumer 67 ms/frame; asserts fixed WAN keeps the
  full bitrate, **slow consumer must reduce bitrate**, peak deferred ACKs <=
  8, zero expired encoder outputs.
- `sustained_slow_tls_reader` (382): 30 s, 256 KiB/s raw TLS reader, no
  feedback; host stays streaming, drops > 0, control/release/revoke latency,
  capture continues after revoke, stop < 1 s.
- `presentation_feedback_recovers_after_slow_tls_and_renderer` (627): 100
  KiB/s reader for 8 s, ACK delay 100 -> 600 -> 20 ms; asserts < 1 Mbps by
  8 s, recovers >= 4 Mbps and width >= 960, `frame_ack_ms` < 350; when ACKs
  stop, host sends `stopped` within 6 s.

Validation evidence and handoff: `docs/validation/desktop-input-backpressure.md`,
`docs/validation/desktop-video-backpressure.json`.

---

## 7. Impact on the planned overhaul

### 7.0 Code-level mechanisms that can produce "burst, stall, burst" (hypotheses, not measured)

1. Credit only on presentation ACK, routed through the capture thread
   (worker.rs:182-190), while capture is paused whenever the 8-frame/byte
   window is full (congestion.rs:227-228, worker.rs:225-227, 254). Resume
   produces up to 8 frames at fps, then pauses again for a full phone
   decode/present round trip.
2. GOP = fps (video/windows.rs:607) produces an IDR every second; an IDR that
   does not fit the remaining window must wait until **every** in-flight frame
   is presented (congestion.rs:198-205), and often fills the window itself,
   pausing capture until its own presentation ACK.
3. Any message > 256 KiB (IDR at 1080p/12 Mbps can be) is discarded, the
   bitrate halved and another IDR requested (congestion.rs:191-195,
   host.rs:243) -- a possible discard/IDR loop.
4. Presentation latency (includes phone decode and Dart hand-off) feeds
   `observe_delay` and `ack_stall`, halving bitrate (congestion.rs:400-405,
   325-335) and dropping the ladder to 640x360 (336-369), which rebuilds the
   encoder with a new IDR (worker.rs:228-234, windows.rs:193-200).
5. `receive` awaits each command's completion on the capture thread
   (host.rs:187, worker.rs:305-315): `Presented` ACKs queue behind input
   commands and behind capture/encode/helper-pipe work.
6. Each 2 ms tick does several `context()` calls (each
   `OpenInputDesktop` + name query + `GetMonitorInfoW`; windows.rs:153, 206,
   219, 229, 281; worker.rs:475, 503); privileged path adds 2 named-pipe round
   trips per tick (client.rs:111, 132) and ships every access unit through 8
   KiB pipe buffers. Use `video_bench --backend` (`mean_context_ms`,
   `mean_capture_video_ms`) to quantify.
7. Every control grant/revoke bumps the generation and forces an IDR
   (worker.rs:415-419, 392-401).

### 7.1 P0-1 Decouple send credit from presentation

Must change:
- **wire.rs `Command`**: add a cumulative receive acknowledgment, e.g.
  `{"op":"received","generation":G,"frame_id":F}` (or carry a receive
  timestamp). Handle it **inline in the host `receive` future** like `Probe`
  (host.rs:181-184), never via `worker.command`.
- **congestion.rs**: retire the `sent` ledger on receipt, not presentation;
  window = `fps x RTT + margin` in both frames and bytes (replace `MAX_FRAMES
  = media::MAX_QUEUED_FRAMES`, the fixed 64/16/512 KiB window and
  `desired_window`); keep a reserved headroom for one IDR (P0-3). Remove the
  `waiting` -> pause coupling. Re-base `stalled()` on transport receipt/probe
  silence (not presentation); prefer RustDesk's "unresponsive > 2 s -> 2 fps"
  before ending the session.
- **worker.rs**: delete `paused = waiting` (225-227) and the
  `capture_video(!paused)` split (254); capture paces at the target fps; if
  the sender has not taken the previous frame, skip that tick (no catch-up
  burst; encoder already avoids catch-up at video/windows.rs:417-419). Keep
  `drain_video` for encoder completion. `Command::Presented` keeps calling
  `Engine::presentation` (input freshness, `frame_ack_ms`) but must stop
  calling `feedback.presented` (186-188).
- **host.rs send loop**: admission becomes receipt-window based; the 20 ms
  sleep/1 s liveness while waiting (246-258) can stay; host `media::Queue`
  should become a 1-2 slot hand-off (H.264 P-frames still cannot be dropped
  individually: drop => resync IDR).
- Delete/replace obsolete tests: `an_unpresented_flight_cannot_overflow_the_receiver_queue`,
  `presentation_backpressure_drains_encoder_without_new_capture` (keep a
  drain-without-submit test only if submission can still be skipped),
  presentation-driven parts of `renderer_baseline_...`, `keyframe_recovery_...`,
  `replacement_idr_...`, `unavailable_desktop_retires_old_ack_debt_...`
  (rewrite around receive ACKs), `cursor_crosses_a_full_video_window...`
  (premise "IDR owns all credit until presented" disappears; keep the
  cursor-independence assertion), slow_link `slow_consumer` assertion
  ("actual consumer backlog must still apply backpressure") which contradicts
  the new model.

Coupling/risks:
- Viewer subsystem must send `received` from its network read task
  (viewer.rs:216-291) and handle backlog locally (newest decodable frame,
  request IDR / lower fps). The viewer's `media::Queue` (same type) will now
  overflow instead of being protected by host credit -- by design; its
  overflow already requests an IDR (viewer.rs:286-289).
- Security boundary unchanged: input still requires a fresh **presented**
  frame (`Engine::input`/`presentation`, `release_if_stale`); do not let a
  receive ACK set `Engine.presented` or call `backend.presented` (helper
  lease).
- Compatibility: an older host ends the session on an unknown `op`
  (host.rs:180). PR #51 is unreleased, so either gate on a host capability
  advertised in `SessionStatus` (safe to add) or bump ALPN
  (`pb-mapper/desktop/2`, security.rs:12) so mismatches fail at TLS with a
  clear error. The installed service host (background_service.rs:381-398) only
  picks this up after the protected install is updated.

### 7.2 P0-2 Even pacing with delay-based adaptation

Must change:
- Replace the controller in congestion.rs (`observe_delay`, `budget_at`,
  `reduce`, `quality`, `minimum_bitrate`, `RESOLUTION_HOLD`,
  `upscale_since`, `INITIAL_BASELINE_LIMIT`, baseline_sample logic) with a
  RustDesk `video_qos`-style controller: queue delay = probe RTT - min RTT
  baseline; <50 ms: +fps step after 3 good samples; 50-150 ms: slow increase
  only; >=150 ms: `fps / (delay/150)`; unresponsive > 2 s: 2 fps; bitrate
  ratio every 3 s (+5-15% up to +150 kbps when low delay and the picture is
  changing -- `duplicate`/`unique_captures` tell "changing"; -5-20% when
  high). Keep a debug trace equivalent to `desktop_feedback_interval`
  (fields listed in 3.5; the brief's instrumentation relies on it).
- **fps must change without rebuilding the encoder**: today fps is in
  `VideoPreferences` -> `configure_video` -> `encoder = None`
  (windows.rs:193-200) and is baked into `MF_MT_FRAME_RATE`, sample duration
  and the converter rate (video/windows.rs:216, 397-399, 608-609, 819-828).
  Add `Encoder::set_fps` that changes only the pacing interval in
  `next_frame` (417-419) and sample duration; keep the media type nominal
  rate; adjust bitrate via `set_bitrate` (CODECAPI_AVEncCommonMeanBitRate,
  252-264). Risk: CBR per-frame budget assumes nominal fps -- verify actual
  bitrate with `video_bench` at reduced fps.
- `Engine`/worker: the adaptive fps target lives with the controller, not in
  `VideoPreferences`; `VideoPreferences.max_fps` stays the negotiated
  ceiling. `validate` requires `max_fps >= 30` (lib.rs:201) -- fine for the
  ceiling, but the internal target must go to 2 fps.
- Resolution: remove automatic ladder (worker.rs:228-240 only for explicit
  preference changes). Resolution changes only on user choice (needs a new
  mid-session command or reconnect, see 7.10), display switch (already via
  context change), or long-term low bandwidth (define explicitly, with a
  long hold, once per session -- must not flap).
- Remove `MAX_INDEPENDENT_FRAME` discard+halve (congestion.rs:191-195) or
  raise it to the wire limit; replace `ack_stall` (325-335) with the
  unresponsive rule.
- Delete `resolution_recovers_with_headroom_without_chattering_at_rate_boundaries`
  and ladder assertions in slow_link (`recovered.video.width >= 960`).

Coupling: P0-4 targets and the UI Quality/Balanced/Smooth mapping define the
ceilings; P1-1 supplies the viewer decode-fps cap.

### 7.3 P0-3 Infinite GOP

Must change:
- video/windows.rs:607: set `CODECAPI_AVEncMPVGOPSize` to the maximum
  (note 0 typically means "encoder default", not infinite). The property is
  set with `let _ =`; read it back (`ICodecAPI::GetValue`) or verify with
  `video_bench` (`keyframes` count over a long run; forced IDR still works:
  `forced_keyframe_ms`). Vendor MFTs may clamp.
- Keep IDR only for: new encoder (session/subscriber, context/resolution
  change), explicit viewer `Keyframe`, resync after host-side drop, encoder
  late output. Decide on trigger 2 in 3.10: control grant/revoke bump the
  generation and force an IDR; the viewer queue also requires a keyframe on a
  generation change (media.rs:33-40). Keeping it is acceptable (rare), or
  split "input epoch" from "video generation" -- that touches the input
  security boundary and the viewer queue, so do it only deliberately.
- congestion admit (198-205): an IDR must not wait for all in-flight frames;
  reserve IDR headroom in the receipt window.
- Bigger, rarer IDRs: bounded by `MAX_ACCESS_UNIT`/`MAX_FRAME` (4 MiB); remove
  the 256 KiB discard (7.2). `request_keyframe` single-credit rule
  (234-249) should become "at most one IDR outstanding" without blocking
  dependents for a presentation RTT.

### 7.4 P0-4 Targets

Host side: `VideoPreferences::default()` (lib.rs:186-195: 1920x1080, 90
fps, 12 Mbps) applies when `Authenticate.video` is absent (host.rs:163) and
in `Engine::new` (bitrate 16 Mbps placeholder, worker.rs:384, overwritten at
130). Phone defaults (1080p, ~4 Mbps, <=60 fps) are chosen by the viewer
(UI `desktop_api.dart`), but host defaults/bounds should be consistent.
`validate` bounds (fps 30..=120, 1-40 Mbps) constrain the three presets.
Adaptation probes upward from the starting bitrate toward the ceiling.

### 7.5 P1-1 Phone pipeline (host-facing parts)

- A viewer -> host message carrying measured decode/present fps (new `op`)
  so the host caps target fps at 90% of it; handle inline on the network task
  and feed the controller (not the worker).
- Backlog -> viewer drops + `Command::Keyframe` (exists; gated by
  `request_keyframe`, worker.rs:172-181). Make sure a keyframe request never
  stalls the host (today recovery blocks dependents until the IDR's
  presentation ACK; with receipt credit it must not).

### 7.6 P1-2 Delay feedback off the display path

- `start_probe` returns constant nonce 1 and is one-shot
  (congestion.rs:121-144). Make it periodic (~1 s) with a nonce sequence and
  send-time map; send from the send loop between messages (it then measures
  queuing behind in-flight video -- the intended signal); keep handling
  `Probe` inline in `receive` (host.rs:181-184).
- Viewer already echoes from its send task, independent of decoding
  (viewer.rs:225-235, 306-311) but only when it opted into feedback, and the
  watch coalesces to the newest nonce (fine for periodic probes).
- Delete `startup_probe_measures_path_without_retiring_presentation_debt`
  or rewrite for periodic probes.

### 7.7 P1-3 Input and cursor independent of video

Host input thread -- what moving input off thread C entails:
- The backend is one `Box<dyn Backend>` on thread C (worker.rs:90-128) holding
  both Enigo and the encoder. Split input into its own backend/thread
  (Enigo is per-thread; `SetCursorPos`, `release`, held sets).
- Authorization state shared between the capture side (produces `recent`,
  `generation`, `context`) and the input side (`sequence`, `controlling`,
  `presented`, `lease`, `input_failure`): needs a shared, lock-protected
  ledger. Preserve every rule in 5.2, `release_if_stale` (2 s), policy-epoch
  release, and "stop never executes queued input" (worker.rs:153-157).
- Desktop binding is per thread: the privileged input thread needs its own
  `DesktopBinding` (helper/worker.rs:128-248) with `INPUT_DESKTOP_ACCESS`;
  the two threads' `BindingState.generation` numbers diverge, so the
  `DesktopContext` equality check must use a shared authority (e.g. the shared
  `EPOCH` + name + console state) or input must validate against the capture
  thread's published context and fail closed on any mismatch. Each binding
  currently spawns its own `Events` hook thread (`EPOCH` is already global).
- Helper IPC is strictly request/response on one pipe, serviced by one thread
  in the worker process (helper/worker.rs:274-427): input is serialized behind
  `CaptureVideo` round trips. Needs a second pipe/channel and a second
  worker-process thread, plus `Request` changes (`deny_unknown_fields`) -> the
  installed helper must be upgraded with the UI (install copies the whole
  bundle, helper/install.rs:488-536; `authorized_application` requires the
  installed UI executable, helper/mod.rs:124-126).
- host.rs `receive`: stop awaiting each command's completion (187); route
  `Input/Control/Release` to the input thread's bounded queue with the same
  fail-closed overflow semantics (viewer already releases control on its own
  overflow, viewer.rs:410-418), `Presented` to the freshness ledger,
  `Keyframe`/new ops to the controller.

Cursor:
- Today the cursor is a full `SessionStatus` JSON every 16 ms ahead of video
  (host.rs:216-226) and sampled on thread C at a cost of ~3 `context()` calls
  per sample (worker.rs:502-520). Viewer currently routes status-only
  messages through its media queue status slot (viewer.rs:269-280; media.rs
  85-93) -- the "latest cursor" FFI read is a viewer/FFI change; the host
  should keep cursor out of admission (already) and consider a compact cursor
  message (new kind -> compat gate).
- Echo suppression: track last successful input time on the host and skip
  sending cursor updates to the (single) controlling viewer within 300 ms of
  its input (RustDesk). Never suppress the generation/desktop-change status.
- Cursor freshness on the helper path equals the last pipe response
  (client.rs:119-124); an independent cursor sampler there needs its own
  request type or worker thread.

### 7.8 P1-4 Zoom/pan local

Host invariant: no `Command` changes encode parameters except an explicit
user quality/resolution choice (7.10). Nothing in the host reacts to viewer
viewport/zoom today; keep it that way (no "viewport" or "visible rect" op).

### 7.9 P2-1 Transport

Single TLS stream over the pb-mapper tunnel stays. Cheap wins inside scope:
write status + payload as one buffered write with one flush (wire.rs:65-76
flushes twice per message; `transport_probe --case tls-split/tls-combined`
measures exactly this); avoid serializing each frame's status twice
(host.rs:239 + 267). Note `codec: true` double-encrypts under TLS
(host.rs:99) -- changing it changes the registration contract; out of scope
unless measured.

### 7.10 Requirement 2 host capabilities

- **Host viewer info** (watching/controlling, direct/relay, connected for how
  long): `Host::status()` (host.rs:308-315) has `state`/`controlling` but
  `path` is empty on the host and there is no connect time. Add a host-side
  status (do not put host-only fields on the wire `SessionStatus`): session
  start `Instant` set at host.rs:172, viewer present flag, and path from
  `registration.diagnostics().direct` (`active_direct`/`active_relay`,
  crates/pb-mapper-client/src/diagnostics.rs:291-301). Background-service
  hosts expose this through `Snapshot.host` (background.rs:42-53).
- **Two permission levels**: already `HostOptions.allow_control` +
  `set_control_allowed` (host.rs:318-321); revoke releases input.
- **Connection quality overlay**: host-provided fields exist in
  `SessionStatus` (`bitrate`, `capture_fps`, `video.codec/width/height`,
  `dropped_frames`, `frame_ack_ms`); network RTT/queue delay should come from
  the periodic probe (new optional status field, e.g. `rtt_ms`, safe for old
  viewers).
- **Quality preference** (Quality/Balanced/Smooth): today preferences are
  fixed at `Authenticate` (host.rs:157-164). Either reconnect on change or add
  a `set_video` op (compat gate) that updates the ceiling; resolution change
  then rebuilds the encoder (one IDR).
- **Shortcut menu / modifier bar**: extend `Key` (lib.rs:262-287) with F1-F12,
  Insert, Space, digits/letters as needed, and `key_code`
  (windows.rs:538-563); Win combos use `Meta`. Unknown keys end the session on
  an older host (host.rs:180) -> capability gate or ALPN bump. Ctrl+Alt+Del
  stays unsupported (5.3).
- **Gestures** map onto existing `Button`/`Wheel` (three-finger scroll ->
  `Wheel`, |v| <= 100 per event, lib.rs:386-391).

### 7.11 Regression guards (host)

- Bounded queues: keep `MAX_FRAME`, `MAX_JSON`, command channel, a bounded
  host hand-off, 3 s write timeout, stop independent of a slow peer
  (tests.rs:76).
- Reconnect never replays input: fresh Worker/channel per session, stop
  check after wait, `sequence` monotonic.
- Input authorization requires a fresh presented frame of the current
  generation: keep `Engine::input`/`presentation`/`release_if_stale` semantics
  and their tests (worker.rs:677, 741, 825; tests/session.rs:73).
- Keep `native_input_failure_ends_control_and_survives_new_video`,
  `observation_never_acquires_write_admission...`, helper binding tests.

### 7.12 Docs

`docs/remote-desktop.md` performance model (credit, ladder, GOP) and
`docs/validation/desktop-input-backpressure.md` handoff must be updated to
the receipt-credit / delay-based model; record the gesture change elsewhere.
