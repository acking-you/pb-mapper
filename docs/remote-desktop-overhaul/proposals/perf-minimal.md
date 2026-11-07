# Remote desktop performance architecture — least-structural-risk design

Architect angle: reach every requirement (BRIEF P0-1 .. P1-4, regression guards;
DECISIONS D1-D14, D17, D23, D24) while reusing the most pb-mapper code and
keeping the blast radius small. Every "kept / rewritten / deleted" call below is
deliberate and justified; nothing is a local patch over a broken structure.

Repo paths are relative to the worktree
`<repo>` at
`a41eaa7`. `D/` = `crates/pb-mapper-desktop/src/`, `F/` =
`ui/native/pb_mapper_ffi/src/`, `U/` = `ui/lib/src/`. Line numbers were read
from the code at that head.

---

## 0. The shape of the change

### 0.1 One owner per concern (why this is a structure, not a patch)

| Concern | Single owner after the change | Today |
| --- | --- | --- |
| Send credit, probe RTT, fps/bitrate QoS | `congestion::Congestion`, owned by the host **session task** (network facts are observed there) | `congestion::Feedback`, mutated by three parties (capture thread, send loop, receive loop), credit returned on the capture thread |
| Ordered video for a decoder + presentation evidence | `video_source::VideoSource` on the viewer, next to the native consumer | `media::Queue` interleaving status/cursor/video, Dart reconstructs presentation from `_videoStatuses` |
| Input authorisation (sequence, generation, fresh frame, control) | unchanged: `worker::Engine` on the desktop-bound capture/input thread | same |
| Backlog/overflow-to-IDR policy | the viewer `VideoSource` queue only | three places (Rust queue, Kotlin queue>=4, C++ queue>=4) |
| Desktop binding / helper IPC | unchanged: thread C + one pipe | same |
| UI state | `DesktopStore` observes Rust status, owns no media | `DesktopStore` owns the media pipeline and presentation ACKs |

### 0.2 Kept, rewritten, deleted (summary; details per section)

| Kept as is (or with a mechanical signature change) | Rewritten in place (same file, new contents) | Deleted |
| --- | --- | --- |
| `Host` accept loop, TLS acceptor, token check, single-viewer semaphore (`D/host.rs:139-172`); `Policy` epochs; `Engine::{input, presentation, control, release_if_stale, invalidate, geometry}` (`D/worker.rs:392-621`); `scheduler::Waiter`; `Backend` trait (one parameter type change, one new input kind); Windows `Desktop` backend; MF encoder construction, converter, SPS/PPS cache (`D/video/windows.rs`); helper service/worker/pipe architecture; viewer task layout (one task, `receive` + `send` futures, `Controls` mailbox, input mpsc(32)) (`D/viewer.rs:210-341`); FFI `DesktopState` connect/disconnect cancellation logic (`F/desktop.rs:109-166`); Kotlin decode pipeline (`pump`, `configure`, `renderLatest`, `consumeSurface`, `VideoPresentationQueue`); C++ `Decoder` and `AcquireSurface`; Dart host flows, reconnect/backoff, identity-mismatch handling, input ordering chain | `D/congestion.rs` (receipt window + RustDesk-style QoS); host session send/receive loops (`D/host.rs:178-274`); worker command intake and frame tick (`D/worker.rs:138-275`); viewer receive dispatch (`D/viewer.rs:212-292`); `D/wire.rs` message types; FFI frame ABI → video-source ABI; Kotlin/C++ *input side* (pull from Rust instead of MethodChannel `submit`); Dart media plumbing and polling | presentation-driven credit, quality ladder, ACK-stall stop, 256 KiB discard, `MAX_READY_AGE` discard, `presentation_feedback`, `Worker::sent` heuristic, status/cursor slots in `media::Queue`, `pb_mapper_desktop_{frame,wait_frame,free_frame}`, `DesktopBuffer`, Dart frame-isolate routed calls, Dart `_acceptFrame` H.264 path, `_presentVideo`/`_flushFeedback` presentation half, `_videoStatuses`, MethodChannel `submit`/`reset`, `frame_ack_ms` |

### 0.3 Decisions this design takes where DECISIONS leaves the call open

- D6: encoder runs at a **nominal fps = the effective preset fps ceiling**; adaptive fps
  is applied by changing only the encoder's internal pacing interval
  (`Encoder::set_rate`) — no rebuild. A *user* preset switch that changes the
  nominal ceiling rebuilds once (one IDR), which D5/D12 allow (user choice).
- D10: input stays on the desktop-bound thread C with **priority handling**
  (non-blocking intake, drain-all before and after every capture call). Reasons and
  the measurement gate are in 2.8.
- D13: pause is the same command as the viewer fps cap: `fps_cap{fps:0}`.
- D17 `lock`: an `Input::Lock` kind, so it inherits every fail-closed check of
  `Engine::input` and the helper forwarding unchanged (2.9).
- D8: the fps cap reacts to **backlog**, not to raw presented throughput (evidence in 3.5).

---

## 1. Wire protocol v2 (ALPN `pb-mapper/desktop/2`)

### 1.1 Transport and framing (kept)

Unchanged: pb-mapper owned TCP session (direct QUIC or relay) → loopback TCP
(`TCP_NODELAY`) → TLS 1.3 (`desktop.pb-mapper.invalid`, no early data) →
records `u32 BE length | payload`, `MAX_JSON = 8192`, `MAX_FRAME = 4 MiB`,
`IO_TIMEOUT = 3 s` (`D/wire.rs:5-7`).

Changed:
- `D/security.rs:12`: `ALPN = b"pb-mapper/desktop/2"`. A v1 peer fails in the
  handshake with `NoApplicationProtocol`; the viewer maps
  `rustls::Error::AlertReceived(AlertDescription::NoApplicationProtocol)` and
  `PeerIncompatible(..)` to `error("version_mismatch: update pb-mapper on both computers")`,
  and the FFI returns code `PROTOCOL`, which Dart treats as terminal (no
  automatic retry), like `INVALID_ARGUMENT`.
- `write_bytes` no longer flushes per record (`D/wire.rs:65-76`). New
  `write_record` (no flush) + `write_message(io, json, Option<bytes>)` writes the
  header record and the optional payload record and flushes **once**
  (`transport_probe --case tls-split/tls-combined` measures exactly this).
  Commands keep one record + flush.

### 1.2 Viewer → host: `wire::Command` (all `deny_unknown_fields`)

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    /// First record; anything else ends the handshake. All fields required.
    Authenticate {
        token: String,                   // 43-char base64url, compared as today
        codecs: Vec<VideoCodec>,         // 1..=2 entries
        video: VideoPreferences,         // hard ceiling: size, nominal max fps, max bitrate
        quality: QualityPreset,          // initial preset
    },
    Control { enabled: bool },
    Input { event: InputEvent },         // unchanged shape; Input gains Lock + keys (1.4)
    Release,
    Ping,                                // 1 s heartbeat (host read timeout 3 s)
    Probe { nonce: u64 },                // echo of HostMessage::Probe, sent immediately
    /// Cumulative transport receipt: every access unit with frame_id <= this
    /// was fully read by the viewer's network task. Send credit only.
    /// Grants, renews or proves NOTHING about input (see 2.4).
    Received { generation: u64, frame_id: u64 },
    /// Input-authorisation evidence only (2 s freshness, current generation).
    Presented { generation: u64, frame_id: u64 },
    Keyframe,                            // rate-limited both sides
    /// Viewer runtime limit, separate from VideoPreferences. 0 = paused
    /// (hidden viewer); 1..=120 = cap. Resuming from 0 forces one IDR.
    FpsCap { fps: u32 },
    /// Live quality preference (no reconnect).
    Quality { preset: QualityPreset },
}
```

Removed vs v1: `Authenticate.presentation_feedback` (receipts are universal),
`Authenticate.video: Option` (now required). New: `Received`, `FpsCap`,
`Quality`. `Probe` nonces are now a sequence.

### 1.3 Host → viewer: `wire::HostMessage`

Every message is one JSON record; only `Video` is followed by exactly one bytes
record. This removes today's empty bytes record after every status/cursor/probe
and the double serialisation of `SessionStatus` per frame (`D/host.rs:239, 267`).

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HostMessage {
    /// State and 1 s metrics. The ONLY v2.x extension point: additive fields,
    /// no deny_unknown_fields; optional host commands are gated on `capabilities`.
    Status(StatusMessage),
    /// Followed by one bytes record of 1..=MAX_FRAME (Annex B AU or JPEG).
    Video(VideoHeader),
    /// Latest pointer; coalesced, never queued behind admission, suppressed
    /// for 300 ms after the viewer's pointer input (D11).
    Cursor(CursorMessage),
    /// Transport probe; the viewer's network task echoes Command::Probe at once.
    Probe { nonce: u64 },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]   // additive, no deny
pub(crate) struct StatusMessage {
    pub state: String,                 // "connecting"|"streaming"|"capture_unavailable"|"stopped"
    pub message: String,               // includes 5 s notices (lock_failed: ...)
    pub control_allowed: bool,
    pub controlling: bool,
    pub generation: u64,
    pub display: Option<Display>,      // physical px of this generation
    pub desktop: String,
    pub codec: VideoCodec,
    pub capture_fps: f32,              // unique (non-duplicate) produced fps
    pub pace_fps: u32,                 // current QoS target (0 = paused)
    pub bitrate: u32,                  // encoder target, bps
    pub rtt_ms: Option<u32>,           // last probe RTT
    pub base_rtt_ms: Option<u32>,      // min of the last 60 samples
    pub adaptation: String,            // "steady"|"raising"|"lowering"|"unresponsive"|"paused"
    pub dropped_frames: u64,           // host hand-off drops (should stay 0)
    pub quality: QualityPreset,
    pub capabilities: Vec<String>,     // v2.0: "lock" when the backend can lock
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoHeader {
    pub generation: u64,
    pub frame_id: u64,                 // monotonic across generations (worker.rs:550)
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub pts_us: u64,
    #[serde(default)] pub duplicate: bool,
    pub host_ms: u16,                  // encode submit -> socket write start
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CursorMessage { pub generation: u64, pub x: i32, pub y: i32, pub visible: bool }
```

Ordering rule (host send loop): before the first `Video` of a generation the host
writes the `Status` of that generation. The viewer drops any `Cursor`, and drops
but still acknowledges (`Received`) any `Video`, whose generation differs from
the last `Status`.

Startup sequence: `Probe{nonce:1}` (clean baseline: nothing queued yet) →
`Status` → `Video` (IDR) → …

### 1.4 Shared types (`D/lib.rs`)

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset { Quality, #[default] Balanced, Smooth }

/// Per-call encoder rate (replaces the bare `bitrate: u32` of capture_video).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRate { pub bitrate: u32, pub fps: u32 }

pub enum Input {               // tag "kind", deny_unknown_fields (unchanged)
    Move { x: u32, y: u32 },
    Button { button: Button, down: bool },
    Wheel { horizontal: i32, vertical: i32 },
    Key { key: Key, down: bool },
    Text { text: String },
    /// D17: LockWorkStation in the interactive session. Control-gated exactly
    /// like other input (sequence, generation, fresh frame, controlling).
    Lock,
}
```

`Key` (D17): existing `shift control alt meta enter escape tab backspace delete
left right up down home end page_up page_down` plus all letters `a`..`z`, digits
`digit0`..`digit9`, `f1`..`f12`, `insert`, `space`. `meta` is the Win key (no
separate `win` variant). Windows mapping (`D/windows.rs:538-563`): letters/digits
→ `enigo::Key::Unicode(c)` (as A/C/V/X/Z today; enigo resolves the VK so
Ctrl/Win chords work), `F1..F12`, `Insert`, `Space` → the enigo variants.
Ctrl+Alt+Del stays unrepresentable.

`VideoPreferences` keeps its shape and bounds (`max_fps 30..=120`, bitrate
`1M..=40M`): it is the **hard ceiling**. `VideoFormat` gains
`#[serde(default)] encode_us: u32` (needed for `host_ms`).

`SessionStatus` stops being a wire type; it is the local status record returned
by `Host::status()`, `Viewer::status()` and the FFI. Removed: `cursor`,
`frame_ack_ms`, `roundtrip_probe`. Added (all `#[serde(default)]`,
`skip_serializing_if` where optional): `pace_fps`, `rtt_ms`, `base_rtt_ms`,
`adaptation`, `quality`, `capabilities`, `paused`; viewer-only `present_fps`,
`receive_bitrate`, `latency_ms` (+ `host_ms`, `transit_ms`, `viewer_ms`),
`presented_generation`, `presented_frame_id`, `presented_age_ms`,
`present_interval: {p50_ms, p95_ms, p99_ms, max_ms, gaps_100, gaps_250}`,
`recovering` (waiting for an IDR), `keyframes_requested`, `backlog_drops`;
host-only (D24) `session_ms: Option<u64>` (age of the active viewer session) and
`path` filled by the host.

### 1.5 Capability list

`StatusMessage.capabilities` is the only negotiation. v2.0 values: `"lock"`
(backend implements `Input::Lock`). Every v2.0 command is mandatory for v2.0
hosts; a v2.x command is sent only when its capability string is present, so a
v2.0 host never sees an unknown `op`. Host→viewer additions go only into
`StatusMessage` fields (tolerated by older v2 viewers).

### 1.6 Invite `pbd1` (D3)

Text: `"pbd1." + base64url_no_pad(payload)`; whitespace trimmed; total ≤ 8192.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | payload version = `0x02` |
| 1 | 1 | `n` = service suffix length, 1..=120 |
| 2 | n | service suffix (bytes after `desktop:`), `[A-Za-z0-9._:-]` |
| 2+n | 32 | SHA-256 of the host certificate DER |
| 34+n | 32 | bearer token, raw 32 bytes |
| 66+n | 4 | first 4 bytes of SHA-256(bytes 0..66+n) — damage check, compared in constant time |

Typical length: `desktop:my-computer` → 81 bytes → 113 characters.
Decode accepts `pbd1.` or the v1 JSON (fingerprint computed from its DER).
The token is re-encoded as today's 43-char base64url for `Authenticate.token`,
so host authentication is unchanged. The viewer pins by fingerprint:
`security::client(fingerprint: [u8; 32])`; `Pinned::verify_server_cert` requires
an empty chain and `ct_eq(SHA-256(end_entity), fingerprint)` (else the existing
`"desktop identity changed"` text plus a structured code);
`verify_tls13_signature` delegates to
`rustls::crypto::verify_tls13_signature(.., &provider.signature_verification_algorithms)`.
`HostIdentity::invitation()` emits `pbd1`; `desktop-trust.dpapi` stores the
identity, so the new invite is derived on load (no migration).
A pb-mapper setup string or `pbmt1_` credential is rejected with a specific error
by `inspect_invitation` (UI work package).

---

## 2. Host

### 2.1 Tasks, threads, ownership

| # | Runs on | Owns / does | Change |
| --- | --- | --- | --- |
| A | accept task (`D/host.rs:139-295`) | listener, sessions JoinSet, semaphore(1), handshake (3 s) | Authenticate v2 fields; records `session_started: Instant` (D24) |
| B-recv | session task, `receive` future | reads `Command`s; handles `Probe`, `Received`, `FpsCap`, `Quality`, `Keyframe` **inline** on `Congestion`; forwards `Control/Input/Release/Presented` to the worker with `try_send` (never awaits a reply); stamps `last_pointer_input` | rewritten (today awaits `worker.command()` per command, `D/host.rs:187`) |
| B-send | session task, `send` future | writes Status/Cursor/Probe/Video; receipt-window admission; probe schedule; cursor echo suppression | rewritten |
| C | worker thread (`spawn_blocking`) | `Box<dyn Backend>` (Enigo, D3D/MF encoder, desktop binding), `Engine`; reads `Congestion::targets()` each tick; pushes frames into the hand-off; publishes status and cursor | intake/tick rewritten; Engine kept |
| D | pb-mapper registration | tunnel | unchanged |
| E/F | helper runtime inside C; MF internal threads | | unchanged |

Shared state (host, per session):

| Object | Writers | Readers |
| --- | --- | --- |
| `Arc<Congestion>` (`parking_lot::Mutex<State>` + `Notify changed`) | B-recv (receipts, probe replies, cap, quality, keyframe), B-send (sent, probes), C (`captured`) | B-send (admit), C (`targets`) |
| `Arc<media::Queue>` hand-off (8 frames / 8 MiB memory bound) | C push | B-send pop |
| `Arc<Notify> frame_ready` (replaces the `frames` watch) | C | B-send |
| `watch<Option<CursorSample>>` (`{generation, position}`) | C (16 ms sampling) | B-send |
| `status: Arc<Mutex<SessionStatus>>` | C | B-send, `Host::status()` |
| `last_pointer_input: AtomicU64` (ms since session start) | B-recv | B-send |
| worker channel `sync_channel::<Task>(64)` + `Wake` + `revoke_requested: AtomicBool` | B-recv | C |

```rust
pub(crate) enum Task { Control(bool), Input(InputEvent), Release, Presented(u64, u64) }
```

### 2.2 Even pacing without encoder rebuilds (D5, D6)

Choice: **fixed nominal fps, variable pacing interval**.

- The encoder is created with nominal fps `N = effective preset fps ceiling`
  (`min(preset.fps_ceiling, VideoPreferences.max_fps)`, ≥ 30 by `validate`).
  `MF_MT_FRAME_RATE`, converter rate and allocator type keep `N`.
- `Encoder::set_rate(VideoRate { bitrate, fps })` (`D/video/windows.rs`, beside
  `set_bitrate` `:252-264`): bitrate → `CODECAPI_AVEncCommonMeanBitRate` when
  changed (existing); `fps` (clamped `1..=N`) changes only the pacing interval
  used in `next_frame` and the sample duration (`:397-399`). No media-type change,
  no rebuild.
- Pacing step (`:417-419`) becomes
  `next_capture = max(next_capture + interval, last_sent + interval * 3 / 4)`:
  the long-run clock is preserved (the 165 Hz rationale in the comment still
  holds) and a post-stall catch-up can never emit two frames closer than 3/4 of an
  interval.
- `Backend::capture_video(&mut self, expected, codec, keyframe, rate: VideoRate)`
  replaces the `bitrate: u32` parameter (lib.rs `:322-340`, `D/windows.rs:260-309`,
  `D/helper/client.rs:125-142`, helper worker `:348-364`; helper validates
  `fps 1..=120`). `drain_video` is kept (used when the hand-off is full or while
  paused).
- The worker never pauses capture as flow control. Per 2 ms H.264 tick it calls
  `capture_video` when **the hand-off has room** (`outgoing.len() < HANDOFF_SLOTS = 2`),
  else `drain_video`. A full hand-off means the session task has not taken the
  previous frame (the RustDesk "wait until fetched" rule, transport-driven, never
  presentation-driven); the tick is skipped, not caught up. JPEG period:
  `max(125 ms, 1000 / pace_fps)`.
- Preset switch to a different nominal ceiling → worker calls
  `backend.configure_video(prefs with max_fps = N')` (the existing path,
  `D/worker.rs:228-240`, now driven by the preset, not the ladder) → one rebuild +
  IDR.

CBR budget verification (required by D6): `video_bench` gains `--pace-fps P`
(calls `set_rate` with `P` while the encoder is nominal `N`). Run at `N = 60`,
`P ∈ {60, 30, 15}` on each acceptance GPU, report `bitrate_mbps`. Two vendor
models are both safe for this design and get recorded in
`docs/remote-desktop.md`:
- per-frame budget (`actual ≈ target × P / N`): fps reduction also reduces bytes,
  which is the intended congestion response;
- per-second budget (`actual ≈ target`): congestion is shed by the bitrate step
  (2.6) instead.
Neither can exceed the target, so no compensation is applied. If a vendor
exceeds `1.2 × target` at reduced pace, clamp `CODECAPI_AVEncCommonMeanBitRate`
to `target × P / N` for that vendor (only then).

### 2.3 IDR policy and headroom (D7)

- `Codec::activate` (`D/video/windows.rs:607`): set `CODECAPI_AVEncMPVGOPSize` to
  `u32::MAX`; on `E_INVALIDARG` retry `0x7FFF_FFFF`; read back with
  `ICodecAPI::GetValue` and keep the effective value in `Statistics.gop`.
  `video_bench` prints `gop` and asserts `keyframes == 1 + forced` over 30 s of
  `--pattern-only`. A vendor that clamps to a finite GOP ≥ 5 s is accepted and
  documented; < 5 s fails the bench.
- SPS/PPS in every IDR: kept (`annex_b`, `:756-786`).
- IDR triggers after the change (complete list): new encoder (session start,
  context/display change, nominal-fps preset switch, encoder error);
  `Engine::invalidate` (control grant/revoke, desktop change, recovery from
  unavailable — kept, rare); viewer `Keyframe` (host-side rate limit
  `HOST_KEYFRAME_MIN_INTERVAL = 250 ms`, coalesced while one is pending, never
  waits for any ACK); resume from `fps_cap 0`; encoder late output > 250 ms
  (existing self-recovery). Deleted: GOP = fps periodic IDR, admission `Discard`,
  `MAX_INDEPENDENT_FRAME` discard + halve, `MAX_READY_AGE` discard, quality-ladder
  rebuilds.
- A keyframe request clears the hand-off (`outgoing.resynchronize()` without
  setting its own request flag) and sets `engine.keyframe = true`; cleared when an
  IDR is actually output (`D/worker.rs:542-544`, kept).
- Headroom: see the admission rule in 2.4 — an IDR never waits for the whole
  flight to drain.

### 2.4 Receipt-ack credit window (P0-1, D4)

Ledger in `Congestion`: `inflight: VecDeque<Inflight { frame_id, bytes, keyframe, sent_at }>`,
`inflight_bytes`, `idr_inflight: Option<u64>`, `last_receipt: Instant`.

Window (recomputed on every admit):

```
rtt_w        = base_rtt.unwrap_or(INITIAL_RTT = 100 ms) + WINDOW_MARGIN (100 ms)
frame_window = clamp(ceil(fps_target × rtt_w), 3, 64)
byte_window  = clamp(2 × bitrate_target / 8 × rtt_w, 64 KiB, 4 MiB)
```

`base_rtt` (not smoothed RTT) is used so queueing never enlarges the window.

Admission of the popped frame (B-send):

1. `inflight` empty → Send (one unit always fits, including a large IDR).
2. `inflight.len() >= frame_window` → Wait.
3. Frame is an IDR, `idr_inflight` is `None`, and `inflight_bytes <= byte_window`
   → Send and set `idr_inflight` (IDR headroom: it may exceed the byte window by
   its own size; only one such IDR at a time).
4. `inflight_bytes + bytes <= byte_window` → Send; else Wait.

There is no Discard. `received(generation, frame_id)` (B-recv, inline): ignore if
`frame_id` > the highest sent (unearned); pop every entry with id ≤ ack, clear
`idr_inflight` when it is popped, `last_receipt = now`, `changed.notify_one()`.
The viewer acknowledges every fully read unit, including ones its queue later
drops, so receipts never stall on decoder state.

Security boundary (D4), stated in `congestion.rs`, `wire.rs` and
`docs/remote-desktop.md`: `Received` is consumed by the session task only; it
never reaches `Engine`, never sets `Engine.presented`, never calls
`backend.presented` (helper lease), never renews held input. Tested in 7.1.

The 4 s "Renderer stopped acknowledging" stop (`D/host.rs:202-212`) is deleted:
transport death is already detected by the 3 s command read timeout (viewer pings
every 1 s) and the 3 s write timeout; a slow viewer is handled by 2.6.

### 2.5 Delay probe (P1-2)

- B-send writes `Probe{nonce}` when none is outstanding and
  `now - last_probe >= PROBE_INTERVAL (1 s)`; nonces start at 1 and increment;
  the send time is kept per nonce (one outstanding).
- B-recv `Probe{nonce}`: matching nonce → sample `rtt = now - sent`; a sample older
  than `IO_TIMEOUT` is discarded; non-matching nonces are ignored.
- A sample taken while `idr_inflight` was set when the probe was written is
  **tainted**: it updates `rtt_ms` and `base_rtt` (min) but not the delay
  controller, so serialising a rare large IDR never cuts fps.
- `base_rtt = min(last 60 samples)` (RustDesk window); `queue_delay = max(0,
  mean(last 3 untainted samples) - base_rtt)`.

### 2.6 fps / bitrate QoS (P0-2, RustDesk `video_qos.rs`-derived)

Presets (D12; resolution fixed at the session's `VideoPreferences` size, 1080p):

| Preset | fps floor | fps start/normal | fps ceiling | bitrate start | bitrate ceiling | bitrate floor |
| --- | --- | --- | --- | --- | --- | --- |
| `quality` | 8 | 30 | 30 | 6 Mbps | 12 Mbps | 2 Mbps |
| `balanced` (default) | 10 | 45 | 60 | 4 Mbps | 8 Mbps | 1 Mbps |
| `smooth` | 15 | 60 | 120 | 4 Mbps | 8 Mbps | 0.5 Mbps |

All ceilings are additionally clamped by the hard `VideoPreferences`
(`max_fps`, `bitrate`) and the viewer cap. A `Quality` command resets
`fps = normal`, `bitrate = start` (RustDesk resets the ratio to the new target).

fps update on every untainted sample, `d = queue_delay` (ms), `F` = floor,
`Nn` = normal:

| Condition | New fps |
| --- | --- |
| `d < 50` | `quick += 1`; step `+1` if `fps < Nn`; every 3rd consecutive good sample step `+5`; `fps = max(F, fps + step)` |
| `50 ≤ d < 100` | `+1` only if `d < previous d` and `fps < Nn` |
| `100 ≤ d < 150` | hold, `max(F, fps)` |
| `150 ≤ d < 200` | `max(F, ceil(fps × 150 / d))` |
| `200 ≤ d < 300` | `min(F, ceil(fps × 150 / d))` |
| `300 ≤ d < 600` | `150 × F / d` |
| `d ≥ 600` | `min(150 × F / d, ceil(fps × 150 / d))` |

Then: every 3 consecutive samples with `d < 150` add `+1`; `d > 50` resets
`quick`; clamp to `[1, ceiling_eff]`.

Bitrate, every `RATIO_INTERVAL = 3 s` (evaluated on the sample that crosses it),
"changing" = at least `DYNAMIC_FRAMES = 6` non-duplicate frames produced in the
interval (reported by C via `captured(duplicate)`):

| `d` (max of the interval) | Change |
| --- | --- |
| < 50 | ×1.15 if changing |
| < 100 | ×1.10 if changing |
| < 150 | ×1.05 if changing |
| < 200 | ×0.95 |
| < 300 | ×0.90 |
| < 500 | ×0.85 |
| ≥ 500 | ×0.80 |

Increases are capped at `+150 kbps` per step; result clamped to
`[floor, ceiling_eff]`.

Unresponsive (RustDesk `user_delay_response_elapsed`): the probe has been
outstanding > 2 s, or `inflight` is non-empty and `last_receipt` > 2 s ago →
`adaptation = "unresponsive"`, output fps `UNRESPONSIVE_FPS = 2` (state fps
untouched); cleared on the next receipt/probe reply.

`targets() -> Targets { fps, bitrate, nominal_fps, paused, keyframe }` used by C
every tick: `fps = 0` when paused, `2` when unresponsive, else
`min(fps, viewer_cap, preset ceiling, hard max_fps)`.

Trace (kept target and event name so `deployed --feedback-log` keeps working):
`pb_mapper_desktop::feedback`, `desktop_feedback_interval` every 1 s with
`action, fps, pace_fps, viewer_cap, paused, unresponsive, bitrate, delay_ms,
rtt_ms, baseline_ms, tainted, outstanding_frames, outstanding_bytes,
window_frames, window_bytes, idr_inflight, keyframes, input_wait_p50_us,
input_wait_max_us`. Removed fields (`quality` ladder level, `recovering`,
`desired_window_bytes`, `last_ack_*`) are documented as removed.

Resolution changes only on explicit user choice (a new connection with other
`VideoPreferences`) or display switch (context change). The automatic ladder and
the long-term low-bandwidth step are not implemented (D5).

### 2.7 Session send loop (B-send) — exact order per pass

1. Status: if `(state, message, controlling, control_allowed, generation, display,
   desktop, capabilities, quality, paused)` differs from the last sent, or 1 s
   elapsed → write `Status` (also the 1 s liveness).
2. Cursor: if the cursor watch changed, its generation equals the status
   generation, and `now - last_pointer_input >= 300 ms` → write `Cursor`;
   otherwise remember `cursor_pending` and wake at `last_pointer_input + 300 ms`.
   Status/generation changes are never suppressed.
3. Probe (2.5).
4. Video: `pending = pending.or_else(media.pop())`; if its generation differs from
   the last written status generation, write `Status` first; `admit` → Send:
   `write_message(VideoHeader{host_ms}, bytes)` (one flush) then
   `congestion.sent(..)`; Wait: keep `pending`.
5. `select!` on cursor change, `frame_ready`, `congestion.changed` (receipt),
   status-changed notify, and a sleep until the earliest of next probe / liveness /
   cursor-unsuppress (≤ 50 ms).

### 2.8 Input path (D10): priority on the desktop-bound thread

Choice: keep injection on thread C, the thread the helper binds with
`SetThreadDesktop`, and remove every wait that was not inherent:

- B-recv never awaits the worker: `worker.submit(Task)` = `try_send` + `Wake`.
  On a full channel (64): set `revoke_requested` (C performs
  `release_control()` at the top of its next iteration) and drop the task;
  status reports `input congestion; control released`. Fail-closed, same rule as
  the viewer's mpsc overflow (`D/viewer.rs:410-418`).
- C drains **all** queued tasks (FIFO, so control barriers keep their order with
  input) after `waiter.receive` and again immediately after every
  `capture_video`/`drain_video` call. Input therefore waits at most for the one
  backend call already in progress.
- Results are reported through status, as today for most commands
  (`D/host.rs:185-187` comment); the oneshot replies are deleted.
- Unchanged: `Policy` epoch release, `release_if_stale` (2 s), stop check right
  after the wait (no queued input after cancellation, `D/worker.rs:153-157`),
  `Engine::input` order of checks, sticky `input_failed`, helper per-request
  generation/sequence checks and 2 s idle release.
- C records receive→injection time per input (`Task::Input` carries the receive
  `Instant`) into `input_wait` (p50/max per second in the trace).

Why not a dedicated injection thread now: (1) the privileged path binds the
injecting thread to the input desktop; a second thread needs its own
`DesktopBinding` and a cross-thread generation authority — new security-critical
code at the secure-desktop boundary (`D/helper/worker.rs:128-248`); (2) the helper
services one request at a time on one pipe (`:274-427`), so a second host thread
would still serialise there unless the helper also gains a second pipe and
thread; (3) held-key/button release obligations live in the `Desktop` object on
thread C. Gate: `input_wait` p99 ≤ 10 ms (ordinary) / ≤ 20 ms (helper) during the
acceptance run with moving content. If the gate fails, the escalation design is a
dedicated input thread with its own binding and a shared `(EPOCH, desktop name,
lock state)` generation authority, plus a second helper pipe — recorded in
`docs/remote-desktop.md` as the next step, not built speculatively.

### 2.9 Cursor (D11) and lock (D17)

Cursor: C samples every 16 ms (kept, `D/worker.rs:206-220`) and publishes
`CursorSample { generation, position }` only (no `SessionStatus` clone). The
per-frame cursor stamping (`D/host.rs:227-235`) and the per-frame
`backend.cursor` call in `Engine::capture_video` (`D/worker.rs:534`) are deleted.
Echo suppression: B-recv stamps `last_pointer_input` on `Input::{Move, Button,
Wheel}`; B-send suppresses `Cursor` for 300 ms after it (2.7). Cursor
observation never renews input (kept invariant).

Lock: `Input::Lock` passes `Engine::input` unchanged (sequence, controlling,
streaming, generation, fresh frame). `Desktop::input_in_context` calls
`LockWorkStation()` (ordinary host: the user's process; privileged: the helper
worker on `winsta0\default`, forwarded as `Request::Input` unchanged). Special
case in `Engine::input`: a `Lock` failure does **not** `release_control()`
(nothing was held, the call is atomic); it sets a 5 s notice
`lock_failed: <Win32 error>` (`Engine.notice: Option<(String, Instant)>`, shown
in `message` until it expires, so a 4 Hz UI poll sees it). Success switches the
input desktop → existing context change → new generation (no special code).
`capabilities` contains `"lock"` for the Windows backends.

### 2.10 Pause / cap / quality on the host (D8, D12, D13)

- `FpsCap{fps}` (B-recv inline): `0` → `paused = true` (C stops submitting,
  drains and discards encoder output, clears the hand-off, sets
  `engine.keyframe = true`; status liveness, cursor and probes continue).
  `> 0` → `viewer_cap = Some(min(fps, 120))`; if it was paused → `paused = false`
  and force one IDR.
- `Quality{preset}` (inline): `Congestion::set_preset`; C rebuilds only if the
  nominal ceiling changed (2.2).
- `Keyframe` (inline): rate-limited request (2.3).

### 2.11 Status fields (D24)

`Host` keeps `session_started: Arc<Mutex<Option<Instant>>>` (set after a
successful handshake, cleared when the session future ends). `Host::status()`
adds `session_ms` and `path` = `"direct"` if
`registration.diagnostics().direct.active_direct > 0`, else `"relay"` while a
session exists, else empty. `controlling` exists. "Make view-only" =
`set_control_allowed(false)` (existing, live). The background-service host
exposes the same fields through `Snapshot.host`.

### 2.12 Helper upgrade (D2)

Helper IPC changes (`Request::CaptureVideo{rate}`, extended `Key`, `Input::Lock`)
require the installed helper to match. Pipes become
`\\.\pipe\pb-mapper.desktop-helper.v2` (`D/helper/mod.rs:35`) and
`\\.\pipe\pb-mapper.desktop-background.v2` (`D/helper/background.rs:8`), and
`installation.json` gains `"protocol": 2` (schema 2; schema 1 is still parsed).
`helper::Status` gains `update_required` (schema 1 or protocol ≠ 2);
`ensure_ready` fails with "Update the Windows desktop service" and the UI offers
`helper_action install` (existing reinstall that copies the bundle and restarts,
`D/helper/install.rs:488-536`). A v1 helper never receives v2 requests because it
listens on the v1 pipe name.

### 2.13 Host code and tests deleted

- `D/congestion.rs`: `Feedback`, `Admission::Discard`, quality ladder
  (`quality`, `minimum_bitrate`, `RESOLUTION_HOLD`, `upscale_since`),
  `ack_stall`, `desired_window`, `presented_at`, `stalled_at`, `recovering`,
  `MAX_FRAMES = media::MAX_QUEUED_FRAMES`, `MAX_INDEPENDENT_FRAME`,
  `MAX_READY_AGE`, `INITIAL_BASELINE_LIMIT`, baseline-sample logic, constant
  nonce. All ten tests (`:523-857`).
- `D/worker.rs`: `paused = waiting` (`:222-227`), feedback-driven reconfigure,
  `Worker::sent`, `bitrate: Arc<AtomicU32>`, `feedback`/`renderer`,
  `sync_feedback`, oneshot replies, `Engine.acknowledged`, `frame_ack_ms`, per-frame
  `backend.cursor`; tests `presentation_backpressure_drains_encoder_without_new_capture`
  (`:897`), the ack-debt half of
  `unavailable_desktop_retires_old_ack_debt_and_resumes_control` (`:1021`),
  `decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture` (`:1242`, rewritten).
- `D/host.rs`: stall stop, `presentation_feedback` branch, non-feedback select,
  double status serialisation, cursor stamping.
- `D/media.rs`: `push_status`, status slot, cursor fields/stamping; test
  `status_is_coalesced_without_reordering_or_invalidating_video` (`:152`). The
  `MAX_QUEUED_FRAMES` constant becomes a constructor argument
  (`Queue::new(max_frames)`: host hand-off 8, viewer 32; both keep 8 MiB) and the
  queue gains a `parking_lot::Condvar` for `pop_timeout`, `len()` and
  `oldest_age()`.
- `D/slow_link.rs`: the three ignored tests are rewritten (7.1).

---

## 3. Viewer (Rust, `pb-mapper-desktop`)

### 3.1 Public API (SDK)

```rust
pub struct ViewerConfig { pub codecs: Vec<VideoCodec>, pub video: VideoPreferences, pub quality: QualityPreset }

impl Viewer {
    pub async fn connect(client, invitation, options) -> Result<Self>               // JPEG, defaults
    pub async fn connect_with(client, invitation, options, config: ViewerConfig) -> Result<Self>
    pub fn video(&self) -> Arc<VideoSource>
    pub fn status(&self) -> SessionStatus                                            // sync, no runtime
    pub fn cursor(&self) -> Option<CursorReading>                                    // sync
    pub fn input(&self, generation: u64, input: Input) -> Result<()>                 // Rust stamps frame_id + sequence
    pub fn request_control(&self, enabled: bool) -> Result<()>                       // unchanged
    pub fn release_input(&self) -> Result<()>                                        // unchanged
    pub fn set_quality(&self, preset: QualityPreset)
    pub fn set_paused(&self, paused: bool)
    pub async fn stop(&self) -> Result<()>                                           // unchanged
}
```

Deleted: `connect_with_codecs`, `connect_with_video_preferences`,
`connect_with_presentation_feedback`, `presented`, `frames`, `latest_frame`,
`next_frame`, `request_keyframe` (moved to `VideoSource`).

### 3.2 Network task (one task, `receive` + `send` futures — structure kept)

`receive` (read timeout `IO_TIMEOUT + 1 s` per message, kept):

| Message | Action (in this order) |
| --- | --- |
| `Probe{nonce}` | `probe_tx.send_replace(nonce)`; nothing else |
| `Status(s)` | validate display (1..=32768, kept); `stopped` → end with its message; on a new generation `video.new_generation(g)` (clear queue, wait for IDR, mark as already requested: the host's `invalidate` sends one); update state slot (observation time) |
| `Cursor(c)` | if `c.generation == status.generation` → cursor slot `{generation, x, y, visible, seq += 1}` |
| `Video(h)` | read the bytes record; **`received_tx.send_replace((h.generation, h.frame_id))` before anything else**; validate codec and `width/height ≤ VideoPreferences` (kept rule, `D/viewer.rs:244-251`); drop (still acknowledged) if `h.generation != status.generation`; metrics; `video.push(VideoUnit{..}, received_at)` |

`send` (`biased` select, every command one record + flush):
stop → control barrier (drains inputs, kept) → `Received` (watch, coalesced) →
`Probe` echo (watch) → `Keyframe` (from `VideoSource` limiter) → `FpsCap`
(QoS tick result) → `Quality` (watch) → `Presented` (watch, coalesced to
`PRESENTED_INTERVAL = 200 ms`) → `Ping` (1 s) → `Input`.
The probe echo and receipts are produced in the same task that reads the socket;
they never cross FFI, Dart or a decoder (P1-2, D4). Unconditional (no
`presentation_feedback` gate, `D/viewer.rs:206, 231-233`).

`Viewer::status()` re-samples `connection.diagnostics()` at most once per second
so `path` stays live (today sampled once at connect, `:173-188`).

### 3.3 `VideoSource` (new `D/video_source.rs`) — the "video sink" for decoders

```rust
pub struct VideoUnit { pub generation: u64, pub frame_id: u64, pub codec: VideoCodec,
                       pub width: u32, pub height: u32, pub keyframe: bool,
                       pub pts_us: u64, pub bytes: Arc<[u8]> }
pub enum Next { Unit(VideoUnit), Timeout, Closed }

pub struct VideoSource {
    queue: media::Queue,                 // video only, VIEWER_QUEUE = 32 frames / 8 MiB, Condvar
    notify: tokio::sync::Notify,         // async consumers (SDK tests, examples)
    owner: AtomicU64,                    // 0 = unclaimed, else ticket
    closed: AtomicBool,
    paused: AtomicBool,
    ledger: parking_lot::Mutex<Ledger>,  // received ring (256), presented, intervals, latency
    presented_tx: watch::Sender<(u64, u64)>,
    keyframes: parking_lot::Mutex<KeyframeLimiter>, keyframe_tx: watch::Sender<u64>,
}
impl VideoSource {
    pub fn claim(&self) -> Result<u64>                         // ticket; second claim -> error
    pub fn next_blocking(&self, ticket: u64, timeout: Duration) -> Next   // parking_lot Condvar, no tokio
    pub async fn recv(&self, ticket: u64) -> Option<VideoUnit>
    pub fn release(&self, ticket: u64)                         // drop the claim, wake waiters
    pub fn presented(&self, generation: u64, frame_id: u64) -> Result<()>
    pub fn request_keyframe(&self, reason: KeyframeReason)
    pub fn stats(&self) -> VideoStats
}
```

Single-consumer rule: only the ticket holder may pop; the Viewer itself never
pops. On native platforms the consumer is the Kotlin feeder or the Windows
decoder thread; elsewhere the Dart JPEG isolate. The session task closes the
source when it ends (`next` → `Closed`).

### 3.4 Backlog policy (D8) — one place

- Downstream of the network the native side keeps ≤ 2 queued AUs (5.1/5.2) and
  presents newest-decoded-wins (kept `VideoPresentationQueue`); compressed AUs are
  never skipped inside a GOP.
- `media::Queue` (viewer instance) keeps today's overflow semantics (drop all,
  wait for the next IDR, request once) at 32 frames / 8 MiB as a memory bound.
- Backlog drop: on every `QOS_TICK = 250 ms`, if the oldest queued unit is older
  than `BACKLOG_AGE = 400 ms`: drop to the next IDR (`queue.resynchronize()`),
  `request_keyframe(Backlog)`, and run the cap decrease immediately.

### 3.5 fps cap from presented fps (D8, P1-1)

Measured in Rust from `presented()` calls: `present_fps` = presentations in the
last 1 s (ring of timestamps). Per `QOS_TICK` (in the network task):

- backlog = `queue.len()`; `factor` = 0.9 on a direct path, 0.8 on relay.
- Decrease when backlog > `BACKLOG_FRAMES = 2` on two consecutive ticks, or after
  a backlog drop: `cap = max(CAP_MIN = 5, floor(present_fps × factor))`, sent if
  lower than the current cap and ≥ 1 s since the last cap change.
- Increase when backlog ≤ 1 for `CAP_UP_HOLD = 3 s`: `cap = min(ceiling, cap + 5)`;
  remove the cap (send the ceiling) once it reaches the ceiling.

Evidence for "backlog-gated": RustDesk's `limited_fps = decode_fps × 9/10`
(relay 4/5) uses decode **capacity** (`fps_calculate`, timed decode work), and its
`fps_control` only decreases with `queue_len > 1`/`> decode_fps/2` and increases
after 3 s of `queue_len ≤ 1` (map `rustdesk-ux.md` 3.6). We measure presented
**throughput**, which equals the host's rate whenever the phone keeps up; capping
at 90 % of throughput unconditionally would ratchet the host down 10 % per
interval. Under backlog, throughput equals capacity, so the gated rule is the
RustDesk rule. This is an interpretation of D8, not a deviation.

### 3.6 Pause / resume (D13)

`set_paused(true)`: `video.pause()` (clear queue, suppress keyframe requests,
mark the next unit must be an IDR "already requested") → send `FpsCap{0}`.
`set_paused(false)`: clear queue again (units that trickled in), keep the
"already requested" mark → send `FpsCap{current cap or ceiling}`; the host's
resume IDR is the only recovery. No keyframe requests while paused, so a native
stall check firing in the background cannot start a recovery loop.

### 3.7 Keyframe request rate limiting (viewer)

`KeyframeLimiter`: first request immediate; while still waiting for an IDR,
repeats after 500 ms, 1 s, then every 2 s; any received IDR resets it; requests
are dropped while paused. Sources: queue overflow/backlog (Rust), native
decoder failure/awaiting key (`keyframe_v1`), Dart watchdog (700 ms not fresh,
kept), "Refresh picture".

### 3.8 Presentation receipts and input authorisation

`VideoSource::presented(g, id)` is the **single viewer-side enforcement point**
(replaces Dart `_presentVideo`, `U/state/desktop_store.dart:562-605`). Accept only
if: `g` is the current generation; `id` is in the received ring and was
received < `PRESENT_FRESH = 2 s` ago; `id` > the last presented id; the source is
not closed. Then record `presented = (g, id, now)`, the present interval and
`viewer_ms = now - received_at`, and `presented_tx.send_replace((g, id))`.
Rejected receipts are counted, never forwarded.

`Viewer::input(generation, input)`: requires `streaming`, `controlling`,
`generation == presented.generation` (the generation whose geometry Dart used to
map coordinates) and `presented.at` < 2 s; builds
`InputEvent { generation, frame_id: presented.id, sequence: next_sequence(), input }`;
`try_send` into mpsc(32); full → revoke barrier (kept). Dart no longer owns
`frame_id` or `sequence`. The host still enforces its own 2 s production-time
check and strict sequence (`D/worker.rs:586-604`), so the two ends cannot
disagree into granting.

### 3.9 Picture latency (overlay and acceptance metric)

`latency_ms = host_ms + transit_ms + viewer_ms` (medians over the last second):
`host_ms` from `VideoHeader` (encode submit → socket write start), `transit_ms =
rtt_ms / 2` (estimate, labelled as such), `viewer_ms` = receive → compositor
consumption (Rust `Instant`s on both ends; no cross-language clock).

---

## 4. FFI (`pb-mapper-ffi`)

### 4.1 State

`DesktopState.viewer: tokio::Mutex<Option<Viewer>>` (`F/desktop.rs:60`) becomes
`parking_lot::RwLock<Option<Arc<Viewer>>>`. `connect_viewer` keeps its
cancellation/epoch logic (`:109-153`); `disconnect_viewer` takes the `Arc` out
under a short write lock and awaits `stop()` without holding any lock (today the
slot stays locked for the whole stop, `:162-163`). Adds
`video_source: Option<u64>` for unregistering.

### 4.2 JSON ops (`pb_mapper_desktop_call_json`, runtime, called from compute isolates)

| Op | Change | Fields / data |
| --- | --- | --- |
| `connect` | changed | in: `invitation`, `relay_only=false`, `quality="balanced"`, `refresh_hz` (f64). Removed `native_video`, `video`. Rust builds the hard ceiling in one place: `1920×1080`, `max_fps = clamp(round(refresh_hz), 30, 60)` on Android / `clamp(.., 30, 120)` elsewhere, `bitrate 12 Mbps`; codecs `[h264]` on Android/Windows, `[jpeg]` elsewhere. out: `{video_source: u64}` |
| `disconnect` | unchanged | after `stop()`: `close` + unregister the source |
| `status` | changed | `{host, background}` only (viewer moved to the hot path) |
| `inspect_invitation` | new (D3, UI package) | `{service, fingerprint}` |
| `control`, `release`, `keyframe`, `presented`, `input` | moved to the hot export | — |
| others (host/helper/background/trust) | unchanged | — |

### 4.3 New C exports

```c
// Synchronous, never blocks, never touches the tokio runtime or tokio locks.
// Safe on the Dart UI isolate. Rejects any op outside this set (INVALID_ARGUMENT).
char* pb_mapper_desktop_viewer_call_json(PbMapperHandle* handle, const char* request);
//   {"op":"viewer_status"}                         -> SessionStatus (viewer)
//   {"op":"control","enabled":bool} | {"op":"release"} | {"op":"keyframe"}
//   {"op":"input","generation":u64,"input":{...}}  -> Rust stamps frame_id and sequence
//   {"op":"presented","generation":u64,"frame_id":u64}   // JPEG renderer only
//   {"op":"quality","preset":"quality"|"balanced"|"smooth"}
//   {"op":"pause","paused":bool}

typedef struct { uint64_t generation; uint64_t sequence; int32_t x; int32_t y;
                 uint32_t visible; uint32_t reserved; } PbDesktopCursorV1;   // 32 bytes
// 1 = cursor written, 0 = none, -1 = no viewer. Non-blocking, per rendered frame.
int32_t pb_mapper_desktop_cursor_v1(PbMapperHandle* handle, PbDesktopCursorV1* out);

typedef struct { uint64_t generation; uint64_t frame_id; uint64_t pts_us;
                 uint32_t width; uint32_t height; uint32_t len;
                 uint32_t flags; /* bit0 keyframe, bit1 jpeg */ } PbDesktopVideoMetaV1; // 40 bytes

int64_t  pb_mapper_desktop_video_claim_v1(uint64_t source);       // >0 ticket; -1 unknown/closed; -3 already claimed
int32_t  pb_mapper_desktop_video_next_v1(uint64_t source, uint64_t ticket, uint32_t timeout_ms,
                                         uint8_t* dst, size_t capacity, PbDesktopVideoMetaV1* meta);
         // 1 unit (copied into dst, meta filled); 0 timeout; -1 closed/unknown; -2 bad ticket;
         // -3 capacity < len (unit dropped, keyframe requested; never happens with 4 MiB)
int32_t  pb_mapper_desktop_video_presented_v1(uint64_t source, uint64_t generation, uint64_t frame_id); // 1 accepted, 0 rejected, -1 closed
int32_t  pb_mapper_desktop_video_keyframe_v1(uint64_t source, uint32_t reason);   // 1 queued, 0 rate-limited/paused, -1 closed
int32_t  pb_mapper_desktop_video_close_v1(uint64_t source, uint64_t ticket);      // releases the claim, wakes next
uint64_t pb_mapper_desktop_instance_v1(void);                                     // per-load token (address of a static)
```

Removed: `pb_mapper_desktop_frame`, `pb_mapper_desktop_wait_frame`,
`pb_mapper_desktop_free_frame`, `DesktopBuffer`, `export_frame`
(`F/desktop.rs:1211-1292`).

### 4.4 Video-source registry and lifetime

New `F/desktop_video.rs`: `static SOURCES: LazyLock<parking_lot::Mutex<HashMap<u64, Arc<VideoSource>>>>`
and `static NEXT_ID: AtomicU64` (ids never reused). `connect` registers;
`disconnect` (and `DesktopState::drop`) closes and removes. Every export clones
the `Arc` under the registry lock and releases the lock before waiting, so:
- a native thread never holds a Rust pointer between calls (ids, not pointers,
  cross the boundary; a stale id returns `-1`);
- `close` during a blocked `next` wakes it within the Condvar wait and it
  returns `-1`; the `VideoSource` memory lives until the last `Arc` drops;
- the video exports do not take `PbMapperHandle`, so `pb_mapper_destroy` safety
  (`F/handle.rs:89-91`) is unaffected; `cursor_v1` and `viewer_call_json` take the
  handle but are only called synchronously on the Dart UI isolate, which also
  performs `destroy`.

All exports catch panics (`std::panic::catch_unwind`) and return `-1`/null;
`unwrap`/`expect` remain denied.

### 4.5 JNI exports (Android only)

`ui/native/pb_mapper_ffi/Cargo.toml`:
`[target.'cfg(target_os = "android")'.dependencies] jni = { workspace = true }`
with `jni = "0.21.1"` in `[workspace.dependencies]` (already in `Cargo.lock`, so
`--locked` CI keeps working). New `F/desktop_jni.rs` (`#[cfg(target_os = "android")]`),
down-calls only (no `JNI_OnLoad`, no cached `JavaVM`, no
`ndk_context` initialisation):

| Java (`com.pb_mapper.ui.DesktopVideoNative`, `@JvmStatic external`) | Rust symbol | Threading |
| --- | --- | --- |
| `instanceToken(): Long` | `Java_com_pb_1mapper_ui_DesktopVideoNative_instanceToken(env, class) -> jlong` | any |
| `claim(source: Long): Long` | `..._claim(env, class, jlong) -> jlong` | feeder start |
| `next(source: Long, ticket: Long, timeoutMs: Int, meta: LongArray): ByteArray?` | `..._next(env, class, jlong, jlong, jint, JLongArray) -> jbyteArray` | feeder thread only; blocks ≤ timeout inside Rust (Condvar) |
| `presented(source: Long, generation: Long, frameId: Long): Int` | `..._presented(..) -> jint` | codec worker thread (`consumeSurface`) |
| `keyframe(source: Long, reason: Int): Int` | `..._keyframe(..) -> jint` | codec worker / feeder |
| `close(source: Long, ticket: Long): Int` | `..._close(..) -> jint` | main thread (dispose/attach) |

`next` returns one Java `byte[]` made with `env.byte_array_from_slice(&unit.bytes)`
(one copy from the `Arc<[u8]>`) and fills `meta` (`LongArray(8)`):
`[0]` status (1 unit, 0 timeout, -1 closed, -2 bad ticket), `[1]` generation,
`[2]` frameId, `[3]` width, `[4]` height, `[5]` flags, `[6]` ptsUs, `[7]` reserved.

Single instance (D9): Dart and `System.loadLibrary` both load
`libpb_mapper_ffi.so` in the app class-loader namespace (Dart's `dlopen` runs
from `libflutter.so`, itself loaded by `System.loadLibrary`), so they share
statics — the same arrangement RustDesk ships (`Java_ffi_FFI_*` next to Dart FFI in
one `.so`). Verified, not assumed: Dart gets `pb_mapper_desktop_instance_v1()`
and passes it to `create`; Kotlin logs both tokens at debug level and fails
`create` closed on a mismatch ("native video library loaded twice"). Step 1 of
the sequence (8.2) proves this on the phone before anything else is built.

---

## 5. Native consumers

### 5.1 Android `DesktopVideo.kt`

New `DesktopVideoNative.kt` (`internal object`, `init { System.loadLibrary("pb_mapper_ffi") }`).
`MainActivity` passes `applicationContext` to `DesktopVideo` (for the snapshot
directory check only).

MethodChannel `pb_mapper/desktop_video` after the change:

| Method | Args | Behaviour |
| --- | --- | --- |
| `create` | `instance: Long` | token check; creates the `SurfaceTextureEntry`, worker `HandlerThread` (priority `THREAD_PRIORITY_DISPLAY`); returns `textureId` |
| `attach` | `textureId, source` | stops the current feeder (`close` + join ≤ 100 ms), resets `acceptedGeneration=-1, lastAccepted=0, awaitingKey=true, revision++` (the old picture stays on the SurfaceTexture), claims `source`, starts a new feeder |
| `snapshot` | `textureId, path, maxWidth` | D23, below |
| `stats` | `textureId` | kept (decoder name, counters, timings) |
| `dispose` | `textureId` | stop feeder, then today's dispose |
| removed | `submit`, `reset` | — |

Native → Dart `event`: only `{textureId, needKeyframe: true, message}` for
user-visible decoder failures; presented events are deleted (freshness comes from
Rust status).

Feeder (`Thread("pb-desktop-feed")`, one per attached source, priority
`THREAD_PRIORITY_DISPLAY`):

```kotlin
override fun run() {
    val meta = LongArray(8)
    while (!stopped && !closed) {
        if (!slots.tryAcquire(50, TimeUnit.MILLISECONDS)) continue   // FEED_DEPTH = 2
        val bytes = DesktopVideoNative.next(source, ticket, 50, meta)
        when (meta[0]) {
            1L -> accept(bytes!!, meta)        // today's submit() logic minus the reply
            0L -> slots.release()
            else -> { slots.release(); break } // closed or bad ticket
        }
    }
}
```

`slots` (`Semaphore(2)`) is released when `pump` takes an input from `queue` and
for every entry removed by `clearQueue`. `accept` keeps today's generation /
duplicate / awaiting-key rules (`DesktopVideo.kt:217-252`) but without
`MethodChannel.Result` (`Submission.reply`/`complete()` deleted) and without the
`queue.size >= 4` overflow branch (unreachable with depth 2; the Rust queue is the
only overflow point). Awaiting-key drops call `DesktopVideoNative.keyframe(source,
AWAITING_KEY)`.

Presentation receipt: in `consumeSurface` (`:180-215`) replace
`event(packet, true, false)` with
`DesktopVideoNative.presented(source, packet.generation, packet.frameId)` — same
exact-timestamp consumption semantics (Impeller stays disabled, D/P1-4).

Keyframe requests: `fail()` (`:351-362`) calls
`DesktopVideoNative.keyframe(source, reason)` and emits the Dart event with the
message. The 2 s stall checks stay; while the viewer is paused Rust drops the
request (3.6), so backgrounding cannot loop.

Optional decoder hints (measure, keep only if they help): `KEY_PRIORITY = 0`,
`KEY_OPERATING_RATE = 60` in `configure` (`:371-380`).

Snapshot (D23): `PixelCopy.request(surface, bitmap(w, h), listener, worker)` with
`w = min(maxWidth, active.width)` and proportional `h`; the listener writes
`bitmap.compress(JPEG, 75, FileOutputStream(path))` (path inside the app's private
storage, supplied by Dart) and replies `true`; any failure replies `false`. Dart
time-boxes the call to 300 ms and never blocks closing on it.

Background pause: no Kotlin state needed; Dart pauses the Rust session, the
feeder simply times out every 50 ms, and Dart ignores decoder events while not
in the foreground.

### 5.2 Windows `desktop_video.cpp`

At `create` resolve the six `pb_mapper_desktop_video_*_v1` symbols and
`pb_mapper_desktop_instance_v1` with `GetModuleHandleW(L"pb_mapper_ffi.dll")`
(Dart has loaded it), falling back to
`LoadLibraryExW(L"pb_mapper_ffi.dll", nullptr, LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`;
any missing symbol or token mismatch fails `create`. The runner still does not
link the DLL at build time.

`Viewer` changes:
- `Attach(source)`: stop and join the decoder thread (`close_v1` wakes it), reset
  `generation_ = -1, last_ = 0, awaiting_key_ = true` **without** bumping
  `revision_`, so `latest_` (the last picture) stays displayable until the next
  decode; claim and start `Run()`.
- `Run()` (`:455-504`): replaces `wake_.wait` + `queue_` with
  `next_v1(source_, ticket_, 50, buffer_.data(), buffer_.size(), &meta)` into a
  reused 4 MiB `std::vector<uint8_t>`; on `1` applies the generation/awaiting-key
  rules moved from `Submit` (`:144-180`), then `Feed`/`Take` as today; on `0`
  handles revision flush and pending snapshot requests; on `< 0` exits.
- `AcquireSurface` (`:196-222`): on the first acquisition of a (revision, frame)
  call `presented_v1(source_, gen, frame)` (non-blocking, µs) instead of
  `events_->Send(presented)`.
- `Fail` (`:234-246`): `keyframe_v1(source_, reason)` + Dart event with message.
- Deleted: `Submit`, `queue_`, `Submission`, `Events::replies`,
  `Complete`, the `>= 4` overflow, `Reset`.
- Snapshot: request flag + path + reply handled on the decoder thread between
  `next` calls: `CopyResource(latest_ texture → staging)`, `Map`, box-downscale to
  ≤ `maxWidth`, encode JPEG with WIC (`IWICImagingFactory`,
  `GUID_ContainerFormatJpeg`; link `windowscodecs`), write `path`, complete the
  reply through `Events` on the platform thread.
- Stats gain nothing (interval histogram is computed in Rust, 3.9 / 7.3).

### 5.3 Dart JPEG fallback (other platforms, D9)

`_desktopFramesWorker` (`U/ffi/pb_mapper_service.dart:57-114`) becomes
`_desktopJpegWorker((sourceId, SendPort))`: claims the source, loops
`next_v1(.., 50 ms, buffer 4 MiB malloc'd once)`, sends
`{generation, frameId, width, height, bytes}` (one `Uint8List` copy) to the main
isolate with one outstanding credit, and closes the claim on exit. It carries no
routed JSON calls any more. After painting, the store calls the hot op
`presented`.

---

## 6. Dart

### 6.1 `DesktopApiClient` afterwards (`U/ffi/desktop_api.dart`)

```dart
abstract interface class DesktopApiClient {
  // host side — unchanged signatures
  Future<DesktopCapabilities> capabilities(); Future<DesktopHostEnable> enableHost(String service, int displayId);
  Future<DesktopBackground> backgroundAction(String action); Future<DesktopTrust> trustStatus();
  Future<String?> restoreHost(); Future<void> revokeTrust(); Future<bool> helperAction(String action);
  Future<String> startHost({...}); Future<void> stopHost({bool onExit = false}); Future<void> allowControl(bool allow);
  Future<DesktopHostSnapshot> status();                         // {host, background}; compute isolate
  Future<DesktopInvitationInfo> inspectInvitation(String invitation);
  // session lifecycle
  Future<DesktopSession> connect(String invitation, {required bool relayOnly, required DesktopQuality quality}); // videoSource id
  Future<void> disconnect();
  // synchronous hot path (pb_mapper_desktop_viewer_call_json / cursor_v1): never blocks
  DesktopStatus? viewerStatus();
  DesktopCursor? cursor();
  void control(bool enabled);
  void release();
  void keyframe();
  void input(int generation, Map<String, Object?> input);       // throws DesktopCallException when refused
  void presented(int generation, int frameId);                  // JPEG renderer only
  void setQuality(DesktopQuality quality);
  void setPaused(bool paused);
  Stream<DesktopJpegFrame> jpegFrames(int videoSource);         // non-native platforms only
}
```

Deleted: `frames()`, `frame()`, the H.264 `presented`, `DesktopFrame`,
`PbMapperService.desktopFrames/desktopFrame`, `DesktopFrameStream` call routing
and the routed-op set (`pb_mapper_service.dart:288-301`), lookups
`pbMapperDesktopFrame/WaitFrame/FreeFrame` and `DesktopBuffer`
(`U/ffi/pb_mapper_ffi.dart:200-226`). Added lookups:
`pbMapperDesktopViewerCall`, `pbMapperDesktopCursor` (`DesktopCursorV1` struct),
`pbMapperDesktopVideoClaim/Next/Close`, `pbMapperDesktopInstance`.
`DesktopVideoTexture` (`U/ffi/desktop_video.dart`): `create(instance)`,
`attach(source)`, `snapshot(path, maxWidth)`, `stats()`, `dispose()`; events
`needKeyframe` + `message` only.

### 6.2 `DesktopStore` exposes

| Member | Source |
| --- | --- |
| `snapshot` (host, background) | `status()` poll, 1 Hz while `sharing` or the Remote page is visible |
| `viewerStatus: ValueNotifier<DesktopStatus?>` | `viewerStatus()` sync poll, 4 Hz while a target exists; set only when changed |
| `connectionPhase`, `connected`, `fresh`, `controlling` | `fresh = streaming && presentedAgeMs < 2000 && presentedGeneration == generation` (from Rust) |
| `geometry` (`display` w/h, video w/h of the current generation) | `viewerStatus` |
| `stats` → `DesktopQualityStats{path, rttMs, baseRttMs, latencyMs, presentFps, receiveFps, receiveBitrate, targetBitrate, paceFps, width, height, codec, decoder}` | `viewerStatus` (+ `texture.stats().decoder` while the overlay is open) |
| `readCursor()` | `cursor()` sync; called per rendered frame by the viewer (persistent frame callback) plus a 33 ms timer that schedules a frame only when `sequence` changed |
| `quality`, `setQuality(preset)` | live op, no reconnect; `_target` carries the preset for reconnects |
| `setViewerForeground(bool)` | kept; additionally `setPaused(!foreground)`; native events ignored while hidden |
| `requestKeyframe()` (Refresh picture), `lockRemote()` (`input kind lock`) | hot ops |
| `videoTexture` (native) / `jpegImage: ValueNotifier<ui.Image?>` (fallback) | per viewer; the native texture survives reconnects via `attach` |

Kept: host flows, `_scheduleReconnect` (+ terminal `PROTOCOL version_mismatch`),
identity mismatch, `send()` ordering/coalescing/`_inFlight` bound and
`_inputEpoch` hygiene (the FFI call inside becomes synchronous), `release()`,
`control()`.

Deleted: `frame` (H.264), `_frames`, `_consumeFrames`, H.264 half of
`_acceptFrame`, `_presentVideo`, presentation half of `_flushFeedback`,
`_videoStatuses`, `_sequence`, `videoPresentation`/`videoPresented`,
`cursorStatus`, `acknowledgePresented` (replaced by the JPEG `presented`),
`presentFps`/`pictureAge` computations (from status), `frameAckMs`-based
`delayed` (now `adaptation == "unresponsive" || latencyMs > 400`), the `poll()`
pull path, unused `startHost`/`backgroundAction` store wrappers.

### 6.3 Notification policy (D14)

- `AppStore.desktop` no longer adds `_notify` as a listener
  (`U/state/app_store.dart:40-42`); AppStore notifies once when the store is
  created. Widgets that show desktop state (rail dot, D15 bar, tray) listen to
  `DesktopStore` directly.
- `DesktopStore.notifyListeners()` fires only when one of `connectionPhase,
  connected, controlling, sharing, error, busy, restarting, invitation,
  capabilities, trust, quality` changed. Metrics go through `viewerStatus`
  (overlay and toolbar chip rebuild alone); the `Texture` subtree never rebuilds
  for metrics.
- Timers: none while idle. Viewer poll only while `_target != null`; host poll
  only while sharing (needed for the D15 "being viewed" bar) or while the Remote
  page registers interest (`load()` / page dispose).

### 6.4 P1-4 invariant (kept, documented)

No Dart, FFI or wire path exists for zoom, pan, rotation, soft keyboard or window
resize; the texture size follows the encoded size only; `attach` keeps the
texture across reconnects; Impeller stays disabled. The gesture surface keeps
scale and offset across keyboard show/hide (test in 7.2).

---

## 7. Test plan

Minimal and deliberate: each test guards a capability, a failure mode, or a
security/state boundary. No wording tests, no input matrices.

### 7.1 Rust (`cargo test -p pb-mapper-desktop`, `-p pb-mapper-ffi`)

New / rewritten unit tests:

| Test | Guards |
| --- | --- |
| `congestion::receipts_return_credit_and_idr_never_waits_for_the_flight` | window closes at `frame_window`/`byte_window`, a `Received` reopens it, one IDR is admitted over the byte window while P-frames are in flight, a second IDR waits, unearned receipts are ignored |
| `congestion::delay_controller_follows_rustdesk_steps` | fps ramp under `d < 50`, cut at 300 ms, unresponsive → 2 fps and recovery, tainted samples ignored, viewer cap and pause clamp the output |
| `congestion::bitrate_steps_every_three_seconds_within_preset` | +150 kbps cap, only when changing, decrease bands, preset floor/ceiling, preset switch resets |
| `media::overflow_discards_dependent_frames_until_idr` (kept, adapted) | GOP integrity on overflow |
| `video_source::presentation_requires_received_current_fresh_frames` | unknown id, old generation, > 2 s, non-monotonic all rejected; input stamping refused without a fresh presentation; second `claim` rejected; `close` wakes a blocked `next_blocking` |
| `worker::input_is_not_blocked_behind_a_slow_capture` | fake backend whose `capture_video` takes 30 ms; inputs queued during it are injected in order right after it; the session receive path never awaits |
| `worker::lock_is_control_gated_and_failure_keeps_control` | view-only refuses `Lock`; failure leaves `controlling` and shows `lock_failed` for 5 s |
| `worker::pause_submits_nothing_and_resume_starts_with_idr` | no `capture_video` while `fps = 0`; first output after resume is an IDR |
| `worker::fps_changes_never_reconfigure_but_preset_ceiling_does` | `configure_video` call count |
| `worker::keyframe_request_forces_one_idr_without_waiting_for_acks` (rewrites `decoder_reset_...`) | rate limit 250 ms, no ACK dependency |
| kept as is | `input_requires_control_current_geometry_and_unique_sequence`, `native_input_failure_ends_control_and_survives_new_video`, `observation_never_acquires_write_admission_and_revocation_releases_it`, `unavailable_backend_reports_failure_and_stops`, `repeated_releases_are_bounded_and_revocation_precedes_a_new_grant`, `clean_tls_close_is_eof_only_between_messages`, `remembered_invitation_survives_restart_but_not_revocation`, helper binding tests, `windows.rs`/`video/windows.rs` tests |
| `tests.rs`: `authentication_requires_v2_fields_and_bounds` (rewrites `older_authentication_defaults_...`), `v1_peer_fails_at_the_handshake_with_version_mismatch`, `tls_rejects_an_unpaired_host_identity` (kept, fingerprint pin), `blocked_video_does_not_block_local_stop` (kept) | |

Integration (`tests/session.rs`):
- `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown` — kept,
  adapted to `connect_with`/`VideoSource` (bad token sees no frames, input needs
  control and a post-grant presented frame, 1000 releases coalesce, duplicate
  sequence rejected, revoke, direct survives relay stop, backend dropped once).
- `credit_returns_on_receipt_while_the_renderer_is_frozen` (replaces
  `cursor_crosses_a_full_video_window_...`): the test consumer pops but never calls
  `presented`; more than 64 frames keep arriving (credit from receipts), cursor
  messages keep flowing, and **receipts grant no input**: a held button is
  released by the host within 2.5 s and new input is refused locally and remotely.

Ignored acceptance (`D/slow_link.rs`, rewritten around the new model, run with
`-- --ignored --nocapture`): `fixed_wan_keeps_rate_and_queueing_sheds_fps`
(350 ms fixed WAN keeps full rate; a bottlenecked proxy lowers fps then bitrate,
no resolution change, zero expired encoder outputs);
`slow_decoder_caps_fps_without_lowering_bitrate` (consumer decoding at 67 ms/frame
→ viewer cap ≈ 13-15 fps, host bitrate unchanged — replaces the deleted assertion
that a slow consumer must lower bitrate); `sustained_slow_tls_reader` (kept,
adapted).

FFI: `video_source_has_one_consumer_and_closes_on_disconnect` (C ABI: second claim
`-3`; a `next` blocked on another thread returns `-1` within 100 ms of
`disconnect`); `hot_viewer_ops_never_wait_for_a_slow_disconnect` (while `stop()`
is held, `viewer_call_json` and `cursor_v1` return immediately). Deleted:
`status_and_video_with_the_same_id_both_cross_the_binary_boundary`,
`binary_frame_is_owned_until_explicit_release_after_disconnect`. Others kept.

Examples: `viewer_probe` rewritten as a simulated decoder (pops from
`VideoSource`, fixed decode delay, calls `presented`), printing receive and
present interval p50/p95/max and the cap it caused; `deployed` selftest and
`input_acceptance` moved to `connect_with`; `video_bench` gains `--pace-fps` and
prints `gop`.

### 7.2 Dart (`flutter test`, all runnable on the Linux CI runner)

| Test | Guards |
| --- | --- |
| `desktop_store_test: freshness and control come from Rust presentation` (replaces CI-skipped `desktop_lifecycle_test.dart:243-334`) | `fresh`/`controlling` follow `presentedAgeMs`/generation from the fake's scripted `viewerStatus`; input is sent with the presented generation only |
| `desktop_store_test: polling runs only while something needs it` | no timers when idle; viewer poll only with a target; host poll only while sharing or page interest |
| `desktop_store_test: background pauses without recovery churn` | `setViewerForeground(false)` → `setPaused(true)` + `control(false)`, no keyframe/reconnect while hidden; foreground → `setPaused(false)` without reconnect |
| `desktop_store_test: quality change is live` | `setQuality` while connected calls the op, no `disconnect`/`connect` |
| `desktop_store_test: metrics do not notify the app` | `viewerStatus` changes do not notify `DesktopStore` listeners nor `AppStore` |
| `desktop_store_test: late texture is disposed on disconnect while busy` (from `desktop_lifecycle_test.dart:199-241`, injectable texture factory so it runs in CI) | lifecycle |
| `desktop_recovery_test.dart` (kept; fake adapted; `PROTOCOL version_mismatch` added to the terminal-error case) | reconnect/backoff/identity |
| `desktop_test.dart` input mapping (adapted to `input(generation, input)`) | coordinate mapping, view-only |
| gesture: `keyboard show/hide keeps scale and offset` (new, P1-4) | local zoom invariant |

Deleted: `desktop_lifecycle_test.dart:98-143` (ACK cadence) and `:144-197`
(`DesktopFrameStream` call routing). `FakeDesktopApi` implements the new
interface (compile-time completeness).

### 7.3 Kotlin (JVM unit tests)

Extract the feeder's decision logic into `FeedGate` (pure Kotlin: depth
semaphore, generation/duplicate/awaiting-key rules, keyframe request callback
behind a `VideoPull` interface). Tests: `feederNeverHoldsMoreThanTwoAndCloseUnblocks`,
`newGenerationStartsOnlyAtAKeyframeAndRequestsOnce`. `VideoPresentationQueueTest`
kept. CI does not run Gradle unit tests today; add
`./gradlew :app:testDebugUnitTest` to `syntax-check.yml` (owner's call; run
locally at minimum).

### 7.4 On-device measurement (matches `docs/validation/desktop-input-backpressure.md` handoff and `scripts/test/desktop-ui-acceptance.md`)

Setup (Windows host + physical Android phone; then the Windows viewer):
1. Build the FFI/helper, `deployed`, `video_bench`; build the phone acceptance
   APK: `flutter build apk --release --target tool/desktop_viewer_acceptance.dart
   --dart-define=setupUrl=http://127.0.0.1:18747/controller.json
   --dart-define=seconds=120 --dart-define=quality=balanced`; `adb reverse
   tcp:18747 tcp:18747`; report written to app storage and fetched with
   `adb shell run-as com.pb_mapper.ui cat files/acceptance.json`.
2. Host: `deployed.exe <config> target/desktop-acceptance serve 600 --tcp
   --feedback-log --control`; moving content: `video_bench 600 60 --pattern-only`.
3. Cases, 120 s each after a 5 s warm-up: Wi-Fi direct Balanced; relay-only
   Balanced; cellular (direct or relay as found); Smooth and Quality spot checks;
   background 10 s then foreground; a continuous drag for input latency.
4. Report schema 2 (`desktop_viewer_acceptance.dart`): per-second samples of
   `viewer_status` (path, rtt/base RTT, latency and its parts, present/receive
   fps, receive and target bitrate, pace fps, adaptation, cap, backlog drops,
   keyframes requested, `present_interval` p50/p95/p99/max and gaps > 100/250 ms),
   decoder name, Flutter `FrameTiming`; host trace `desktop_feedback_interval`
   (incl. `input_wait`) captured alongside.
5. Proposed thresholds for the owner's acceptance (D28 keeps the merge block until
   the owner accepts): direct Wi-Fi present fps ≥ 0.9 × min(60, host unique
   capture fps); present interval p95 ≤ 25 ms at 60 fps; zero gaps > 250 ms on
   direct (≤ 2 per 120 s on relay); latency p50 ≤ 80 ms on LAN, plus one camera
   check (phone beside the host's "Unique desktop presentation N" counter);
   steady-state keyframes ≤ 1 per minute; `unresponsive` never set on LAN; resume
   from background shows a picture within 1 s with no reconnect; `input_wait` p99
   within the 2.8 gate. Same-condition RustDesk run recorded for comparison.
6. Record build, quality preset, refresh rates, GPU/encoder (`gop`, CBR model
   from `--pace-fps`), phone model and decoder, temperature, duration.

---

## 8. Risks and implementation sequence

### 8.1 Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Two instances of `libpb_mapper_ffi.so` (JNI vs Dart) → registry invisible to Kotlin | Spike first (8.2 step 1); instance token checked at `create`, failing closed with a specific message; RustDesk precedent for one `.so` serving both |
| Vendor MFT clamps or ignores the GOP setting | read-back + `video_bench` keyframe count; finite GOP ≥ 5 s accepted (IDR headroom + receipt credit absorb it) |
| CBR budget at reduced pace differs per vendor | `--pace-fps` classification; both models safe (2.2); clamp only if a vendor overshoots |
| Input still shares thread C with capture | non-blocking intake + drain-all; `input_wait` trace and gate; escalation design recorded (2.8) |
| Probe sampling behind a large IDR cuts fps | tainted samples excluded; IDRs are rare with infinite GOP |
| Throughput-based cap collapse | backlog-gated cap (3.5) |
| Background → native stall → keyframe loop | pause is a host command; Rust drops keyframe requests while paused; Dart ignores native events while hidden; host forces one IDR on resume |
| Buffer bloat in QUIC/relay buffers | window from `base_rtt + 100 ms`, delay controller on probe RTT |
| Hung viewer that pings but never acknowledges | window closes, worker stops submitting (hand-off full), liveness continues; no growth; 3 s timeouts end dead transports |
| Removing the presentation stall stop hides a frozen renderer | input still requires fresh presentation (2 s, both ends); Dart watchdog (700 ms keyframe, 3 s reconnect) kept |
| Sync FFI on the UI isolate accidentally blocks | separate export with a closed op set, no runtime/tokio locks inside; test `hot_viewer_ops_never_wait_for_a_slow_disconnect` |
| Second consumer corrupts H.264 order | claim ticket; Dart never claims on native platforms |
| Helper/service version skew | v2 pipe names, `protocol` in `installation.json`, `update_required` → reinstall prompt |
| PR #51 installs stop interoperating | accepted by D1; explicit `version_mismatch` error, release note |
| Snapshot API unreliable on some phones (`PixelCopy` on a codec Surface) | time-boxed, best effort, placeholder on failure (D23) |
| Losing the temporary shim checkpoint discipline | shim lives in one function and is deleted in step 5; CI must not ship it |

### 8.2 Ordered implementation sequence (every checkpoint builds and passes its narrow tests)

1. **Spikes (no product behaviour change).** a) Android: a debug-only JNI export
   returning `pb_mapper_desktop_instance_v1()`, Kotlin `System.loadLibrary` +
   log, Dart log of the same token; run on the phone. b) `video_bench`: GOP
   read-back and `--pace-fps`. Checkpoint: tokens equal on device; bench results
   recorded. Stop and redesign 4.5 only if tokens differ.
2. **Protocol v2 + receipt credit + QoS inside `pb-mapper-desktop`.** `wire.rs`,
   `lib.rs` types, ALPN, `congestion.rs`, host send/receive loops, worker intake
   and pacing (`Backend::capture_video(.., VideoRate)`), `Encoder::set_rate`, GOP,
   trimmed `media::Queue`, `VideoSource` (queue + ledger + limiter + cap logic),
   viewer v2. FFI kept compiling with a **temporary shim**: the old JSON ops map
   onto the new viewer (`connect{native_video, video}` → `ViewerConfig`,
   `presented` → `VideoSource::presented`, `input{event}` → `Viewer::input`), and
   `pb_mapper_desktop_wait_frame` claims the source and returns the old buffer
   format with a synthesised status (incl. the latest cursor). Dart unchanged.
   Checkpoint: `cargo test -p pb-mapper-desktop`, `cargo test -p pb-mapper-ffi`,
   app runs end to end. **Measure P0 alone on the phone** (receipt credit, pacing,
   infinite GOP) before touching the phone pipeline.
3. **Host input/cursor/lock/pause/quality and helper v2.** Non-blocking worker
   intake, drain-all, cursor messages + echo suppression, `Input::Lock`, keys,
   `fps_cap`/`quality` handling, D24 status, helper pipes v2 + `protocol`.
   Checkpoint: worker/helper tests, `input_acceptance` on Windows, installed-helper
   reinstall path.
4. **FFI surfaces.** `RwLock<Option<Arc<Viewer>>>` slot, `viewer_call_json`,
   `cursor_v1`, video-source registry and C exports, JNI module, `connect` v2 op
   (shim ops still present). Checkpoint: FFI tests; `cargo ndk` build for the
   three ABIs; Windows DLL build.
5. **Native consumers and Dart media switch.** Kotlin feeder/attach/snapshot,
   Windows pull/attach/snapshot, Dart `DesktopVideoTexture`, JPEG worker on
   `next_v1`, store uses `videoSource`; delete the shim, the old frame exports and
   the Dart H.264 path. Checkpoint: `flutter analyze`, `flutter test`, Android
   APK and Windows builds, on-device smoke on both viewers.
6. **Dart store finish.** Hot-path calls, polling gates, D14 notifications,
   quality, pause, cursor reading per frame, overlay stats, acceptance tool schema
   2. Checkpoint: Dart tests in 7.2, Kotlin unit tests.
7. **Docs and measurement.** `docs/remote-desktop.md` performance model (receipt
   credit, QoS, GOP, pause, cap, metrics, input threading, CBR model per GPU),
   `docs/validation/desktop-input-backpressure.md` (new evidence only), CLAUDE.md
   FFI export list (the "three integer calls" sentence is already false),
   release notes (ALPN v2, helper update). Run 7.4; the merge-block statements stay
   until the owner accepts fluidity (D28).
