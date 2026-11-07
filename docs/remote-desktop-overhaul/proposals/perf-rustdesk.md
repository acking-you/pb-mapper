# Remote desktop performance architecture — RustDesk-faithful design

Author angle: adopt RustDesk's proven mechanisms (TestDelay probe, `video_qos.rs`
constants, client `fps_control` cap, decode-queue/refresh policy, dedicated input
thread, cursor echo exclusion) as closely as pb-mapper's TLS-over-tunnel transport
allows, and justify every deviation.

Sources read for this design: `BRIEF.md`, `DECISIONS.md`, maps `host-pipeline`,
`viewer-ffi`, `dart-pipeline`, `native-decoders`, `docs-protocol`, `rustdesk-ux`,
`deps-capabilities` (relevant parts); code confirmed in the worktree at `a41eaa7`
(`crates/pb-mapper-desktop/src/{lib,wire,host,worker,viewer,media,security}.rs`,
`video/windows.rs`, `windows.rs`, `helper/{mod,client,worker,service}.rs`,
`ui/native/pb_mapper_ffi/src/{desktop,handle,lib,galaxy}.rs`, `DesktopVideo.kt`,
`desktop_video.cpp`, `desktop_api.dart`) and RustDesk `5f015c9`
(`src/server/video_qos.rs`, `src/server/connection.rs:1158-1240`,
`src/client/io_loop.rs:1170-1281`).

Path shorthands: `D/` = `crates/pb-mapper-desktop/src/`, `F/` =
`ui/native/pb_mapper_ffi/src/`, `U/` = `ui/lib/src/`, `K/` =
`ui/android/app/src/main/kotlin/com/pb_mapper/ui/`, `W/` = `ui/windows/runner/`,
`RD/` = `<rustdesk checkout>\`.

---

## 0. Summary, decision compliance, deviation register

### 0.1 The model in one paragraph

The host captures on an even clock at a delay-adapted target fps (RustDesk
`VideoQoS`, scaled to a 60 fps ceiling), never pausing for presentation. Each
encoded access unit goes into a 4-slot outbound hand-off; a dedicated send task
writes it when a **receipt window** (`fps × (RTT_base + 100 ms)` frames, the
matching byte budget plus one IDR of headroom) allows. The viewer's network
**reader task** sends a cumulative `received{frame_id}` the moment a frame is
read, and answers the host's 1 s `probe` (RustDesk `TestDelay`) from the same
network tasks, so credit and delay never include decode or display time. The
viewer keeps a RustDesk-style bounded decode queue (120 units), drops to the next
IDR when the oldest undecoded unit is older than 250 ms, and sends RustDesk's
`fps_control` cap (90 % direct / 80 % relay of measured **presentation
capacity**). Compressed frames go from Rust straight to the Kotlin feeder thread
(JNI) or the Windows decoder thread (C exports); Dart receives only status,
geometry, stats, and a per-vsync 56-byte "view" (cursor + freshness) through a
non-blocking leaf call. Presentation receipts reach Rust directly from the native
decoders and are used **only** for input authorisation (2 s, generation,
monotonic) and for the viewer-local capacity estimate. Input runs on its own
desktop-bound host thread (RustDesk `handle_input`), the cursor travels as its own
message and is not echoed to the operator within 300 ms of pointer input. GOP is
infinite; IDRs only on demand, rate-limited on both sides.

### 0.2 Decision compliance

| Decision | Where satisfied |
| --- | --- |
| D1 ALPN v2, strict v2 types, capability list | §1.1, §1.3 (`deny_unknown_fields` on every v2 type, `features: Vec<String>` in `Authenticate`/`Hello`) |
| D2 helper upgrade | §2.11 (pipe names `.v2`, `installation.json` schema 2 with `protocol: 2`, `Status.protocol_current`, reinstall prompt) |
| D3 `pbd1.` invite, fingerprint pin, `inspect_invitation`, v1 JSON accepted | §1.7, §4.1 |
| D4 credit on receipt, presentation only for input | §1.3 `received`, §2.4, §3.6; test `receipts_never_authorise_input` (§7.1) |
| D5 even pacing, delay-based fps/bitrate only, no resolution ladder | §2.3, §2.5 |
| D6 fps change without encoder rebuild — picked: fixed nominal fps + tick skipping + frame-budget bitrate compensation with an overshoot guard | §2.3.3, verified by `video_bench --fps-schedule` (§7.4) |
| D7 infinite GOP, SPS/PPS in each IDR, rate-limited keyframes both sides, recovery not dependent on presentation acks | §2.6, §3.5 |
| D8 newest-decodable backlog policy, fps cap 90/80 % separate from preferences | §3.4, §3.7 |
| D9 frames bypass Dart (JNI pull, C-export pull), one consumer, JPEG stays Dart | §4.3–§4.5, §5 |
| D10 input never waits behind capture; desktop binding per thread; fail closed; helper request/response kept per lane; receive task never blocks | §2.7, §2.11 |
| D11 cursor out of the video queue, 300 ms echo exclusion, per-frame non-blocking read | §1.3 `Cursor`, §2.8, §4.2 `pb_mapper_desktop_view_v1` |
| D12 Quality / Balanced / Smooth numbers, live change | §2.5.1, `set_quality` |
| D13 pause/resume on background, resume with IDR, no recovery loops | §3.8, §5.1.6, §6.4 |
| D14 no AppStore fan-out, polls only when needed | §6.5 |
| D17 key vocabulary, control-gated `lock` | §1.4, §2.7.4 |
| D23 last-picture thumbnail from native decoders | §5.1.7, §5.2.6 |
| D24 host status: session start, path, controlling | §2.10 |

No decision is unworkable. Two caveats, stated with evidence:

- **D8 "keep only the newest decodable frame"**: for H.264 the newest *decodable*
  frame needs every P-frame since the last IDR, so the policy is "feed the whole
  chain in order while it is fresh; when the oldest undecoded unit exceeds the
  backlog age, discard the chain and wait for the next IDR". The native side
  already skips superseded *decoded* pictures (`K/VideoPresentationQueue.kt:29-43`).
- **D23 on Android**: `PixelCopy.request(Surface, Bitmap, …)` from the Surface
  that MediaCodec renders into is the only non-invasive readback; it is not
  proven for a SurfaceTexture-backed Surface on every vendor. The design treats
  failure as "show the placeholder" exactly as D23 allows (§5.1.7, risk R12).

### 0.3 Deviation register (RustDesk mechanism → pb-mapper)

| RustDesk mechanism (file) | Adopted as | Deviation and why |
| --- | --- | --- |
| Capture waits ≤3 s until each connection *dequeued* the frame, then sleeps to `spf` (`video_service.rs:875-895`) | Capture clock at target fps; a tick is **skipped** (never deferred) while `outbound + encoder in-flight ≥ 2` | RustDesk's back-pressure comes from a blocking TCP socket; over pb-mapper the host writes into a loopback TCP socket whose bytes are forwarded into a QUIC stream with megabyte windows, so "dequeued" would be reached far too early. Hence the explicit receipt window below. Skipping instead of waiting keeps the clock grid and rules out bursts. |
| No receipt ack (Flutter clients send none; `video_received` legacy only) | `received{frame_id}` cumulative ack from the viewer reader task | Required by D4: QUIC/loopback buffering hides congestion from the writer. |
| `TestDelay` every 1 s, one outstanding, echoed by the client network task, `> 2 s` → 2 fps (`connection.rs:1082-1103`, `video_qos.rs:336-344, 521-525`) | `probe{nonce}` / `probe_reply{nonce}` identically | Echo is a distinct `probe_reply` op (RustDesk reuses `TestDelay` with `from_client`); the host reports `rtt_ms` in `Status` instead of `last_delay` inside the probe. |
| `RttCalculator` 60/10/0.5 | Same constants | **MIN_SAMPLES 10 → 3** for the provisional baseline: with RustDesk's 10, a 200 ms WAN is classified "congested" (`d ≥ 150`) for the first 10 s and fps collapses to `min_fps`. |
| `avg_delay`: `avg − rtt` if `avg > rtt` else **raw `avg`** (`video_qos.rs:67-85`) | `avg.saturating_sub(baseline)` | RustDesk's else-branch reports the full RTT whenever the samples sit at the baseline, so a quiet 60 ms WAN lands in the 50–100 ms band and never takes the fast-increase path. |
| fps steps +1 / +5 per 3 good samples, `normal_fps` 16/20/24, `INIT_FPS` 15 for 1 s, ceiling `FPS` 30 | Same rule, with `normal_fps`, `INIT_FPS` and both step sizes **multiplied by `scale = ceiling/30`** | RustDesk's ladder is tuned for a 30 fps ceiling (≈6 s to ceiling). At the brief's 60 fps ceiling the unscaled ladder needs ≈21 s, which users perceive as a choppy start. `min_fps` (8/10/12) and every delay threshold stay unscaled, so congested behaviour is RustDesk's. |
| Bitrate ratio ×1.15/1.10/1.05 (dynamic only) / ×0.95/0.90/0.85/0.80 every 3 s, +150 kbps cap above `BR_SPEED`, floors per mode | Same multipliers, period, dynamic-screen threshold (≥6 unique encodes per 3 s), +150 kbps cap above 1 Mbps (≈ `BR_SPEED` at 1080p) | Absolute bits/s instead of a ratio of `w·h/1000` kbps, because pb-mapper encodes hardware H.264 Baseline (less efficient than RustDesk's default VP9/H.265) and the brief fixes Balanced at ~4 Mbps. Floors are RustDesk's 1080p values converted (Quality 1 Mbps, Balanced 0.7 Mbps, Smooth 0.3 Mbps). |
| Presets force `custom_fps = 30` | Preset ceilings 30 / 60 / 120 (bounded by renderer limit) | Brief P0-4 asks for ≤60 fps on phones. |
| Client `fps_control` from **decode capacity** (`fps_calculate`), 90 % direct / 80 % relay, decrease/increase/halve rules, sent as `custom_fps` | Same rules, sent as a separate `fps_cap` | Capacity is measured from **presentation intervals while the pipeline was backlogged** (the hardware decoder is asynchronous, so per-frame decode time is meaningless). The cap is a runtime limit separate from the quality preset (D8), not the user fps field RustDesk overloads. |
| Decode queue 120, evict oldest + refresh; backlog refresh `queue > min(decode_fps, 60)`, ≤20 per session, ≥10 s apart, **queue not flushed** | Queue 120 / 16 MiB; backlog = oldest undecoded unit older than 250 ms → **drop to next IDR** + request; first immediate, then ≥2 s with backoff to 10 s; no per-session cap | D8 mandates dropping; with an infinite GOP every recovery needs an IDR, so a per-session cap of 20 could strand the picture. Time-based (250 ms ≈ RustDesk's 150–300 ms "bad" band) instead of a 1 s frame count, because a remote-control picture 1 s behind is unusable. |
| Mobile render slot: newest RGBA wins, decoder never waits for Flutter | Kotlin keeps one picture on the SurfaceTexture and skips superseded decoded pictures (already), feeder stops pulling at 2 queued | Same principle; GPU texture path instead of RGBA copies. |
| No keyframe rate limit for decode errors | Viewer: ≥500 ms with backoff to 5 s; host: ≥300 ms between request-driven IDRs, requests never dropped | Protects against IDR storms from a failing decoder (rustdesk-ux map §7 P0-3). |
| Dedicated input thread per connection, `try_change_desktop()` before each injection (`connection.rs:645, 1158`; `input_service.rs:693…`) | `pb-desktop-input` thread with its own desktop binding; helper worker gets a second (input) thread and lane | Kept pb-mapper's fail-closed authorisation (fresh presented frame, generation, sequence) which RustDesk does not have. |
| Cursor broadcast excludes the connection whose input moved it within 300 ms (`input_service.rs:369-378`) | Identical (single viewer: suppress while that viewer's last pointer input < 300 ms) | none |
| `lock_screen()` → `LockWorkStation` (`platform/windows.rs:1266-1271`) | `lock` command, control-gated, on the input thread | Gated by the same freshness/sequence as input. |
| Idle close 30 s | `READ_IDLE` 10 s | Balances cellular hand-offs against holding a dead single-viewer slot (the host accepts only one viewer). |

---

## 1. Wire protocol v2 (ALPN `pb-mapper/desktop/2`)

### 1.1 Stack and negotiation

Unchanged below TLS: pb-mapper tunnel (TCP service, `codec: true`) → loopback TCP
(`TCP_NODELAY`) → TLS 1.3 (`desktop.pb-mapper.invalid`, no early data).

- `D/security.rs`: `const ALPN` moves to `D/wire.rs` as
  `pub(crate) const ALPN: &[u8] = b"pb-mapper/desktop/2";` used by both
  `HostIdentity::acceptor` and `security::client`.
- Mismatch: rustls on the host fails with `Error::NoApplicationProtocol` and
  sends the `no_application_protocol` alert. The v2 viewer maps an
  `AlertReceived(AlertDescription::NoApplicationProtocol)` (or the host-side
  counterpart in logs) to `ConnectError::VersionMismatch`, surfaced by the FFI as
  code `VERSION_MISMATCH` with "The remote computer runs a different pb-mapper
  version. Update pb-mapper on both computers." A v1 (PR #51) viewer against a v2
  host fails at the handshake with a TLS error, as D1 accepts.
- New `F/error.rs` `ErrorCode` variants (screaming snake case on the wire):
  `VersionMismatch`, `IdentityChanged`, `Busy`, `Denied`, `SetupString`. Dart's
  string matching for `"desktop identity changed"` (`U/state/desktop_store.dart:432`)
  is replaced by the code.

### 1.2 Record framing

Every record is `u32 BE length | u8 type | body` where `length = 1 + body.len()`.

| type | name | direction | body | bound |
| --- | --- | --- | --- | --- |
| `0x01` | `RECORD_JSON` | both | one JSON value (`ViewerMessage` or `HostMessage`) | `MAX_JSON = 8192` |
| `0x02` | `RECORD_VIDEO` | host → viewer only | 36-byte `VideoHeader` ‖ access unit | `36 + MAX_FRAME (4 MiB)` |

Any other type, a video record from the viewer, or a body over its bound ends the
session ("invalid desktop message"). Each message is written with one assembled
header buffer plus the payload (`write_all` ×2 at most) and **one** `flush`
(today every message is two records and two flushes, `D/wire.rs:65-76`). The
reader allocates without zero-fill (`Vec::with_capacity` + `AsyncReadExt::take`
+ `read_to_end`) and converts with `bytes::Bytes::from(vec)` (zero-copy;
`bytes = "1.11"` is already a workspace dependency).

Timeouts (`D/wire.rs`): `HANDSHAKE_TIMEOUT = 3 s` (host reads `Authenticate`),
`HELLO_TIMEOUT = 5 s` (viewer waits for `Hello`), `WRITE_TIMEOUT = 5 s` per
message, `READ_IDLE = 10 s` without any record on either side. The viewer's 1 s
`ping`, the host's 1 s `status` and `probe` keep both directions alive while paused.

### 1.3 Message types (Rust, `D/wire.rs`)

```rust
use serde::{Deserialize, Serialize};

pub(crate) const ALPN: &[u8] = b"pb-mapper/desktop/2";
pub(crate) const MAX_JSON: usize = 8 * 1024;
pub(crate) const MAX_FRAME: usize = 4 * 1024 * 1024;
pub(crate) const VIDEO_HEADER: usize = 36;
pub(crate) const RECORD_JSON: u8 = 0x01;
pub(crate) const RECORD_VIDEO: u8 = 0x02;
pub(crate) const MAX_FEATURES: usize = 16; // each 1..=32 bytes of [a-z0-9_]

/// Viewer -> host. Strict: an unknown op or field ends the session.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ViewerMessage {
    /// First message after TLS. Token is the 43-char base64url of the 32 bytes.
    Authenticate {
        token: String,
        codecs: Vec<crate::VideoCodec>,          // 1..=2 entries
        limits: crate::VideoLimits,              // renderer limits, session constant
        quality: crate::Quality,                 // initial preset
        features: Vec<String>,                   // viewer capabilities; v2.0 sends []
    },
    /// Cumulative transport receipt: every video record with frame_id <= this
    /// was read completely by the viewer's network task. Grants send credit only.
    Received { frame_id: u64 },
    /// Echo of `HostMessage::Probe`, written by the viewer's network tasks.
    ProbeReply { nonce: u64 },
    /// Newest frame actually presented (native receipt validated in Rust).
    /// Input authorisation and held-input renewal only; never send credit.
    Presented { generation: u64, frame_id: u64 },
    /// Request an IDR. The host honours every request, at most one IDR per 300 ms.
    Keyframe { reason: crate::KeyframeReason },
    /// Viewer presentation-capacity cap (RustDesk fps_control). 1..=120.
    FpsCap { fps: u32 },
    /// Viewer hidden (D13): stop capture and video until `Resume`.
    Pause,
    /// Viewer visible again: resume capture, start with an IDR, QoS restarts at INIT.
    Resume,
    /// Live preset change (D12). Never changes resolution.
    SetQuality { quality: crate::Quality },
    Control { enabled: bool },
    Input { event: crate::InputEvent },
    /// LockWorkStation on the host (D17). Gated like input; shares the sequence.
    Lock { generation: u64, frame_id: u64, sequence: u64 },
    Release,
    /// Sent only after 1 s without any other viewer message.
    Ping,
}

/// Host -> viewer JSON records. Video is RECORD_VIDEO, never JSON.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HostMessage {
    /// First host message after a successful Authenticate.
    Hello {
        features: Vec<String>,                   // v2.0: "lock", "secure_desktop"
        codec: crate::VideoCodec,                // selected codec
        nominal_fps: u32,                        // encoder nominal rate (session constant)
        quality: crate::Quality,
    },
    /// Sent on every change, before the first video of a generation, and every 1 s.
    Status { status: HostStatus },
    /// Latest pointer of `generation`; coalesced, suppressed during operator input.
    Cursor { generation: u64, x: i32, y: i32, visible: bool },
    /// TestDelay. One outstanding; sent every 1 s by the send task, in stream order.
    Probe { nonce: u64 },
    /// Final message before the host closes the stream (best effort).
    Stopped { reason: StopReason, message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostStatus {
    pub state: HostState,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnavailableReason>,
    pub control_allowed: bool,
    pub controlling: bool,
    pub generation: u64,
    pub display: Option<crate::Display>,          // physical px; input coordinate space
    pub desktop: DesktopKind,
    pub codec: crate::VideoCodec,
    pub width: u32,                               // encoded size of this generation
    pub height: u32,
    pub capture_fps: f32,                         // unique (non-duplicate) captures, 1 s
    pub target_fps: u32,                          // QoS target
    pub fps_ceiling: u32,                         // min(limits, preset, viewer cap)
    pub bitrate: u32,                             // QoS target bits/s
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,                      // last probe round trip
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_ms: Option<u32>,                 // RttCalculator
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_delay_ms: Option<u32>,              // avg(last 3) - baseline
    pub send_delay_ms: f32,                       // frame ready -> written, p50 over 1 s
    pub dropped_frames: u64,                      // host resync drops
    pub frames: u64,
    pub bytes: u64,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostState { #[default] Connecting, Streaming, CaptureUnavailable, Paused }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopKind { #[default] Ordinary, Secure }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnavailableReason { SecureDesktop, DisplayLost, EncoderFailed, Other }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopReason { Denied, Busy, InvalidRequest, HostStopped, Revoked, ViewerTimeout, CaptureFailed }
```

Public types added to `D/lib.rs` (also used by FFI/Dart):

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality { Quality, #[default] Balanced, Smooth }

/// Renderer limits, fixed for the session. Replaces VideoPreferences on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoLimits { pub max_width: u32, pub max_height: u32, pub max_fps: u32 }
// validate: max_width 2..=3840, max_height 2..=2160, max_fps 10..=120.
// Default 1920x1080x60.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason { DecoderError, Resync, Backlog, Overflow, Refresh, Resume }
```

`VideoHeader` (binary, big-endian, exactly 36 bytes):

| offset | size | field | rule |
| --- | --- | --- | --- |
| 0 | 8 | `frame_id` u64 | session-monotonic, never reused (Engine `frame_id` semantics today) |
| 8 | 8 | `generation` u64 | must equal the generation of the last `Status` |
| 16 | 8 | `pts_us` u64 | encoder clock, informational |
| 24 | 4 | `width` u32 | must equal `Status.width`, ≤ `limits.max_width` |
| 28 | 4 | `height` u32 | must equal `Status.height`, ≤ `limits.max_height` |
| 32 | 1 | `codec` | `0` H.264 Annex B, `1` JPEG; must equal `Hello.codec` |
| 33 | 1 | `flags` | bit0 keyframe, bit1 duplicate; bits 2–7 must be 0 |
| 34 | 2 | reserved | must be 0 |

Ordering rules the host guarantees (single send task): `Hello` is first; a
`Status` with generation `G` precedes the first video of `G`; video records of a
generation are in `frame_id` order. The viewer acks a video record of an older
generation (credit) and drops it; a video record of an unknown newer generation
ends the session (protocol violation).

### 1.4 Input vocabulary (D17)

`Input` stays `#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]`
with `move/button/wheel/key/text` unchanged. `Key` becomes:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Shift, Control, Alt, Meta,                       // Meta = Win
    Enter, Escape, Tab, Backspace, Space, Insert, Delete,
    Left, Right, Up, Down, Home, End, PageUp, PageDown,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
}
```

Windows mapping (`D/windows.rs::key_code`): letters and digits →
`enigo::Key::Unicode(lowercase char)` (as A/C/V/X/Z today), `F1..F12` →
`enigo::Key::F1..F12`, `Space`, `Insert` → the enigo variants. Win+L is never a
key chord: it is `ViewerMessage::Lock`. Ctrl+Alt+Del stays unrepresentable.

### 1.5 Session sequence

```
viewer: TLS ClientHello (ALPN pb-mapper/desktop/2, SNI desktop.pb-mapper.invalid)
viewer: Authenticate{token, codecs, limits, quality, features}
host:   Hello{...}                         | Stopped{denied|busy|invalid_request} + close
host:   Status{connecting, G}  Probe{1}
viewer: ProbeReply{1}
host:   Status{streaming, G, display, width, height}  Video(IDR, G)  Video ... Cursor ...
viewer: Received{id} per video, Presented (<=10 Hz), FpsCap (<=1 Hz, on change),
        Keyframe (rate-limited), Pause/Resume, SetQuality, Control/Input/Lock/Release, Ping
host:   Probe every 1 s (none outstanding), Status on change + every 1 s
```

`Viewer::connect` returns only after `Hello` (≤5 s) and fails with a structured
`Busy`/`Denied`/`VersionMismatch`/`IdentityChanged` instead of today's silent
close (`D/host.rs:172`). Revealing `busy` only after a valid token leaks nothing
beyond "the token was right".

### 1.6 What v2 removes

- `Command::Authenticate.presentation_feedback`, `Command::Probe` (now
  `ProbeReply`), `Authenticate.video: VideoPreferences` (bitrate leaves the wire;
  fps floor 30 is gone; `VideoLimits` replaces it).
- `SessionStatus.roundtrip_probe`, `frame_ack_ms`, `cursor`, `frame_id`,
  `video.{keyframe, pts_us, duplicate}` on the status channel; the
  "status JSON + bytes pair per message" convention and empty-bytes heartbeats.
- Host stop "Renderer stopped acknowledging presented frames" (`D/host.rs:202-212`).
- The v1 JSON invitation as an *output* (still accepted as input).

### 1.7 Invitation `pbd1.` (D3)

```
invite  = "pbd1." base64url_no_pad(payload)            // total <= 256 chars
payload = version   u8  = 2
        | flags     u8  = 0                            // reserved, must be 0
        | n         u8  in 1..=120
        | suffix    [n] ASCII [A-Za-z0-9._:-]          // service = "desktop:" + suffix
        | fingerprint [32]                             // SHA-256(host certificate DER)
        | token     [32]                               // raw bearer secret
len(payload) == 3 + n + 64, no trailing bytes
```

Example: `desktop:office-pc` → 76-byte payload → 107-character invite (QR
version 6 at level M; the v1 JSON was ≈650 characters).

```rust
pub struct Invitation { pub service: String, pub fingerprint: [u8; 32], token: Token }
struct Token([u8; 32]);                     // redacted Debug; zeroed on drop
impl Invitation {
    pub fn encode(&self) -> String;                          // always "pbd1."
    pub fn decode(text: &str) -> Result<Self, InvitationError>;
    pub fn short_fingerprint(&self) -> String;               // "3F2A-91C0-7B1D-E4A2" (first 8 bytes)
    pub(crate) fn token_text(&self) -> String;               // 43-char base64url for Authenticate
}
pub enum InvitationError { TooLarge, Invalid, SetupString }
```

`decode`: trim; reject > 8192 bytes; `pbd1.` → binary rules above; `{` → v1 JSON
(`version == 1`, existing checks), fingerprint = SHA-256(base64url-decoded
certificate), token decoded from the 43 chars; text starting with `pbmt1_` or
base64 decoding to a JSON object with `serverAddress` or `msgHeaderKey`
(`U/common/setup_string.dart:24-37`) → `SetupString`; anything else → `Invalid`.

TLS pinning by fingerprint (`D/security.rs`):

```rust
struct Pinned { fingerprint: [u8; 32], algorithms: rustls::crypto::WebPkiSupportedAlgorithms }
// verify_server_cert: chain.is_empty() && ct_eq(SHA-256(cert), fingerprint)
//   else Err(rustls::Error::General("desktop identity changed")); no WebPKI path
//   building (the pin is the trust anchor; the rcgen cert is valid 1975-4096).
// verify_tls13_signature: rustls::crypto::verify_tls13_signature(msg, cert, dss, &self.algorithms)
// verify_tls12_signature: Err(PeerIncompatible) (TLS 1.3 only)
// supported_verify_schemes: self.algorithms.supported_schemes()
```

The host side is unchanged (`HostIdentity` keeps certificate + key + token;
`desktop-trust.dpapi` keeps storing it); `HostIdentity::invitation(service)` now
computes the fingerprint and returns a `pbd1.` invite, so persisted host trust
is "re-encoded on load" without user action.

---

## 2. Host

### 2.1 Tasks, threads and ownership (per session)

| Name | Kind | Owns | Reads / writes |
| --- | --- | --- | --- |
| accept | tokio task (`D/host.rs` accept loop) | `TcpListener`, `JoinSet` (≤8), `Semaphore(1)` | spawns a session |
| `desktop-recv` | tokio task | TLS `ReadHalf` | `Flow::on_receipt`, `Qos::on_probe_reply / set_cap / set_quality`, `CaptureControl` (pause, keyframe), `InputQueue::try_send`, `Presented` slot |
| `desktop-send` | tokio task | TLS `WriteHalf`, probe clock | `Outbound::pop`, `Flow::admit`, status/cursor watches |
| `pb-desktop-capture` | OS thread (`std::thread::Builder`, joined via `spawn_blocking`) | `Box<dyn Backend>` (DXGI, MF encoder; helper capture lane), `Waiter` | pushes `Outbound`, updates `Authority` (context, generation, recent), reads `Qos` atomics, `CaptureControl` |
| `pb-desktop-input` | OS thread | `Box<dyn InputBackend>` (Enigo/SendInput + desktop binding; helper input lane), `ControlLease` | `InputQueue` recv, `Authority`, cursor watch |
| `pb-desktop-helper-io` | OS thread + current-thread runtime (privileged only) | named pipe `\\.\pipe\pb-mapper.desktop-helper.v2` | serves the two lanes (§2.11) |

The session task spawns `desktop-recv` and `desktop-send` as two tasks over
`tokio::io::split` halves (today one task joins both futures, `D/host.rs:178-275`),
so a large video write never delays reading a receipt or a probe reply.

Shared session state (`D/session.rs`, new):

```rust
pub(crate) struct SessionCore {
    pub authority: parking_lot::Mutex<Authority>,     // input authorisation ledger
    pub qos: parking_lot::Mutex<Qos>,                 // controller (recv + send + 1 s tick)
    pub target_fps: AtomicU32, pub target_bitrate: AtomicU32,   // capture hot path
    pub flow: parking_lot::Mutex<Flow>, pub flow_changed: tokio::sync::Notify,
    pub outbound: Outbound,                           // capture -> send, 4 slots
    pub capture: CaptureControl,                      // paused, KeyframeLimiter, Wake
    pub status: watch::Sender<u64>,                   // bumped when HostStatus changes
    pub cursor: watch::Sender<Option<CursorUpdate>>,  // latest pointer (input thread)
    pub last_pointer_input_ms: AtomicU64,             // ms since session start, input thread
    pub started: Instant,
    pub cancel: CancellationToken,
}
```

### 2.2 Receive task (never blocks on workers)

| Message | Handling (inline, no await on a thread) |
| --- | --- |
| `received{frame_id}` | `flow.on_receipt(frame_id, now)`; `flow_changed.notify_one()` |
| `probe_reply{nonce}` | `qos.on_probe_reply(nonce, now)` (stale nonce ignored) |
| `fps_cap{fps}` | validate 1..=120; `qos.set_viewer_cap(fps)` |
| `pause` / `resume` | `capture.set_paused(bool)`; resume also `capture.keyframe.force(Resume)` and `qos.restart(now)` |
| `set_quality{q}` | `qos.set_quality(q)` (ceiling + bitrate jump, RustDesk `user_image_quality`) |
| `keyframe{reason}` | `capture.keyframe.request(reason)` (limiter, §2.6) |
| `presented{g,f}` | `presented_slot.send_replace((g, f))` (input thread consumes) |
| `control`, `input`, `lock`, `release` | `input_queue.try_send(cmd)`; on `Full` set `input_overflow` and wake the input thread (fail closed: it ends control with "input congestion; control released") |
| `ping` | nothing |

Any parse error ends the session (strictness, D1). The handshake reads
`Authenticate` within `HANDSHAKE_TIMEOUT`; on failure the host writes
`Stopped{denied|busy|invalid_request}` before closing.

### 2.3 Capture: even pacing without encoder rebuilds (D5, D6)

#### 2.3.1 Backend trait split

```rust
/// Capture side. Lives on pb-desktop-capture only.
pub trait Backend: 'static {
    /// Called once per session. Rebuilds the encoder only when size or codec changes.
    fn configure_video(&mut self, config: VideoConfig) -> Result<()>;
    fn display(&mut self) -> Result<Display>;
    fn context(&mut self) -> Result<DesktopContext>;      // default as today
    fn capture(&mut self) -> Result<Vec<u8>>;             // JPEG still image
    /// One pacing step. Drain finished encoder output; if `plan.submit` and the
    /// backend's Pacer says a tick is due, capture + submit at most one image.
    /// Wait at most `wait` for output (helper: a long-poll). Default impl: JPEG
    /// at <= 8 fps via `capture_consistent`.
    fn next_video(&mut self, expected: &DesktopContext, plan: &CapturePlan, wait: Duration)
        -> Result<Option<EncodedFrame>>;
}
pub struct VideoConfig { pub limits: VideoLimits, pub nominal_fps: u32, pub codec: VideoCodec }
pub struct CapturePlan {
    pub target_fps: u32,      // QoS target (1..=nominal)
    pub bitrate: u32,         // encoder setting after rate-model compensation
    pub keyframe: bool,       // force an IDR on the next submit
    pub submit: bool,         // false while the outbound hand-off is full or paused
}

/// Input side. Lives on pb-desktop-input only.
pub trait InputBackend: 'static {
    fn context(&mut self) -> Result<DesktopContext>;
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()>;
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<CursorPosition>>;
    fn presented(&mut self, _expected: &DesktopContext) -> Result<()> { Ok(()) } // helper lease
    fn lock(&mut self, expected: &DesktopContext) -> Result<()>;
    fn release(&mut self);
}
pub struct Factory {
    pub capture: Arc<dyn Fn() -> Result<Box<dyn Backend>> + Send + Sync>,
    pub input: Arc<dyn Fn() -> Result<Box<dyn InputBackend>> + Send + Sync>,
}
```

Each factory closure runs on its own thread (COM/MF and `SetThreadDesktop` are
thread-affine). `platform::native_factory(display_id)` returns the ordinary
Windows pair (`Desktop` capture with encoder, `InputDesktop` with Enigo);
`helper::factory(display_id)` returns the two helper lanes over one `HelperLink`.
Removed from `Backend`: `input`, `input_in_context`, `presented`, `cursor`,
`release`, `capture_video`, `drain_video` (replaced by `next_video`).

#### 2.3.2 Pacer (shared by the ordinary backend, the helper worker and JPEG)

```rust
pub(crate) struct Pacer { next: Instant, interval: Duration }
impl Pacer {
    fn set_fps(&mut self, fps: u32);              // interval = 1 s / fps; keeps the phase
    fn due(&self, now: Instant) -> bool;          // now >= next
    fn consume(&mut self, now: Instant);          // next = max(next + interval, now) - fired or skipped
    fn until(&self, now: Instant) -> Duration;
}
```

A tick is consumed whether it captured or was skipped, so a stall never produces
a catch-up burst (same intent as `D/video/windows.rs:414-419`, now outside the
encoder). The static-desktop rule stays: no new desktop image, no forced IDR and
< 250 ms since the last submit → no submit (duplicate liveness ≈4 fps).

#### 2.3.3 Encoder at a fixed nominal rate (D6 choice)

- `nominal_fps = limits.max_fps` (session constant: 60 on phones, ≤120 on desktop
  viewers). `Encoder::new_with_size(display, nominal_fps, bitrate, max_w, max_h)`
  is built once per context; QoS fps changes and preset changes **never** call
  `configure_video` (today `D/worker.rs:228-240` rebuilds on every ladder step).
- Encoder API (`D/video/windows.rs`): delete `next_frame` and its internal
  `next_capture` pacing; add

  ```rust
  pub fn submit(&mut self, force_keyframe: bool, interval: Duration) -> Result<bool>;
  pub fn drain(&mut self) -> Result<Option<Encoded>>;          // unchanged
  pub fn pending(&self) -> usize;                              // submitted, not output
  pub fn set_bitrate(&mut self, bps: u32) -> Result<()>;       // CODECAPI_AVEncCommonMeanBitRate
  ```
  `submit` sets sample time = real elapsed time and **sample duration =
  `interval`** (the actual tick interval, i.e. a variable-frame-rate timestamp
  stream), keeps `MF_MT_FRAME_RATE = nominal`.
- **Rate model** (`D/rate.rs`, new): encoders that budget per frame from the
  nominal rate would emit `bitrate × target/nominal` at reduced fps. The capture
  thread therefore sets
  `encoder_bitrate = target_bitrate × clamp(nominal / target_fps, 1.0, RATE_COMP_MAX = 4.0)`,
  clamped to `256_000..=40_000_000`. Overshoot guard: in a 3 s window with ≥6
  unique encodes, if measured output > 1.5 × `target_bitrate` in two consecutive
  windows, the encoder honours timestamps → compensation is switched off for
  that encoder instance (`rate_model = "timestamp"`, logged once in the trace).
  `video_bench --fps-schedule` (§7.4) records per vendor which model applies.

#### 2.3.4 Capture thread loop

```text
loop {
  if cancel: break
  if now >= next_context_check (100 ms) or after an error:
      ctx = backend.context()  -> on change: authority.invalidate(CaptureContext)  (new generation, IDR)
                               -> on error: state capture_unavailable (reason), retry every 200 ms
  if capture.paused: backend.next_video(.., plan{submit:false}, 50 ms) and discard output; continue
  room   = outbound.len() + backend_pending < HANDOFF_LIMIT (2)
  plan   = CapturePlan { target_fps: target_fps.load(), bitrate: rate.encoder_bitrate(),
                         keyframe: capture.keyframe.take_due(now), submit: room }
  match backend.next_video(&ctx, &plan, min(pacer_until, 16 ms)) {
      Some(frame) => stamp frame_id/generation from Authority; authority.recent.push((id, now));
                     outbound.push(frame) (if full: resync - drop all, IDR); rate.observe(frame);
                     stats (unique fps, bytes); notify desktop-send
      None => {}
  }
  if !room && a tick was due: skipped_ticks += 1 (trace)
}
```

Context checks happen at most every 100 ms and once per submitted capture inside
the backend (the ordinary backend's `finish_video` check stays), instead of
several `OpenInputDesktop` calls per 2 ms poll (`host-pipeline` map §7.0 item 6).
The 2 ms encoder event poll remains inside `next_video` only while
`pending() > 0`.

Outbound constants: `OUTBOUND_CAP = 4` (encoder `MAX_IN_FLIGHT` 3 + 1, so an
encoded unit is never dropped by the hand-off itself), `HANDOFF_LIMIT = 2`,
`OUTBOUND_STALE = 1 s` (head older than this → drop all, force IDR; only reached
when the window stays closed while QoS is already at 2 fps).

### 2.4 Receipt-ack credit window (D4)

`D/flow.rs` (new; replaces `D/congestion.rs`):

```rust
pub(crate) struct Flow {
    sent: VecDeque<Sent>,            // (frame_id, bytes, sent_at, keyframe), oldest first
    in_flight_bytes: usize,
    last_idr_bytes: usize,           // EWMA (alpha 0.5) of written IDR sizes
}
pub(crate) enum Admit { Send, Wait }
```

With `fps` = current QoS target, `rtt` = `Qos::baseline_ms()` (provisional
before 3 samples; `INITIAL_RTT_MS = 100` before the first probe reply),
`bps` = current target bitrate:

```
W_frames   = clamp(ceil(fps * (rtt + RECEIPT_MARGIN_MS) / 1000) + 2, 4, 64)
W_bytes    = clamp(bps / 8 * (rtt + RECEIPT_MARGIN_MS) / 1000 * 2, 64 KiB, 8 MiB)
IDR_ROOM   = clamp(max(last_idr_bytes, bps / 8 / 4) * 5 / 4, 64 KiB, 4 MiB)
RECEIPT_MARGIN_MS = 100        // JPEG sessions: W_frames = 4, W_bytes = 2 MiB, IDR_ROOM = 0
```

Admission of the outbound head:

- empty ledger → `Send` (any size ≤ `MAX_FRAME`);
- P-frame / JPEG → `Send` iff `sent.len() < W_frames && in_flight_bytes + size ≤ W_bytes`;
- IDR → `Send` iff `sent.len() < W_frames + 1 && in_flight_bytes + size ≤ W_bytes + IDR_ROOM`.

An IDR therefore never waits for every in-flight frame (today it does,
`D/congestion.rs:196-205`), and nothing over 256 KiB is discarded or halves the
bitrate (`congestion.rs:191-195` is deleted). `on_receipt(id)` pops every entry
with `frame_id ≤ id` and records `ack_ms` for the trace only (QoS uses the probe,
as RustDesk). Receipts never touch `Authority` (D4; tested).

Example: 60 fps, 40 ms RTT, 4 Mbps → `W_frames = 11`, `W_bytes ≈ 140 KiB`,
`IDR_ROOM ≥ 156 KiB`.

### 2.5 QoS controller (D5, D12) — `D/qos.rs` replaces the ladder

#### 2.5.1 Preset table (one place: `D/quality.rs`)

| Preset (RustDesk analogue) | max fps | bitrate ceiling @1080p | floor | `min_fps` | `normal_fps` @30 |
| --- | --- | --- | --- | --- | --- |
| `quality` (Best) | 30 | 8 Mbps | 1.0 Mbps | 8 | 16 |
| `balanced` (Balanced) — default | 60 | 4 Mbps | 0.7 Mbps | 10 | 20 |
| `smooth` (Low, "optimize reaction time") | 120 | 3 Mbps | 0.3 Mbps | 12 | 24 |

- `fps_ceiling = min(limits.max_fps, preset.max_fps, viewer_cap.unwrap_or(120))`
  (RustDesk `highest_fps`). Phone Balanced → 1080p, 4 Mbps, ≤60 fps (P0-4).
- Bitrate ceiling scales with encoded pixels:
  `clamp(ceiling_1080p × w×h / 2_073_600, max(floor × 2, 1_000_000), 40_000_000)`.
- Resolution never changes from QoS or presets (D5). Encoded size =
  `scaled_size(display, limits)`; it changes only on a display switch (new
  context → new encoder → IDR). No `set_limits` op exists in v2.0.

#### 2.5.2 Constants (`D/qos.rs`)

| Name | Value | RustDesk source |
| --- | --- | --- |
| `PROBE_INTERVAL` | 1 s | `TEST_DELAY_TIMEOUT` |
| `UNRESPONSIVE_AFTER` | 2000 ms | `user_delay_response_elapsed` |
| `MIN_FPS` / `MAX_FPS` | 1 / 120 | same |
| `UNRESPONSIVE_FPS` | `MIN_FPS + 1` = 2 | `adjust_fps` |
| `INIT_FPS` | `15 × scale` (≤ ceiling); applies for 1 s after connect and after `resume` | `INIT_FPS` 15, `new_user_instant` |
| `HISTORY_DELAY_LEN` | 2 (window of 3 samples) | same |
| `DELAY_FLOOR_MS` | 10 | `delay.max(10)` |
| `DELAY_GOOD / OK / THRESHOLD` | 50 / 100 / 150 ms | same |
| `RTT_WINDOW_SAMPLES` / `RTT_MIN_SAMPLES` / `RTT_ALPHA` | 60 / **3** / 0.5 | 60 / 10 / 0.5 (deviation 0.3) |
| `ADJUST_RATIO_INTERVAL` | 3 s | same |
| `DYNAMIC_SCREEN_THRESHOLD` | 2 unique encodes/s (≥6 per 3 s) | same |
| `SPEED_BITRATE` | 1_000_000 (cap increases at +150 kbps above it) | `BR_SPEED` ≈ 1.04 Mbps at 1080p |
| `MAX_INCREASE_BPS` | 150_000 | `ratio_add_150kbps` |
| `scale` | `max(1, fps_ceiling / 30)` (f32) | deviation 0.3 |

#### 2.5.3 Algorithm (per probe reply; RustDesk `user_network_delay`)

```text
delay  = max(rtt_ms, 10); push to history (keep 3); rtt_calc.update(delay)
avg    = mean(history);  base = rtt_calc.get_rtt()   // None before 3 samples
d      = max(base.map_or(avg, |b| avg.saturating_sub(b)), 10)
small  = max(1, round(scale)); quick = round(5 * scale)
normal = min(round(preset.normal_fps * scale), ceiling); min_fps = preset.min_fps
if d < 50:   quick_count += 1; step = (fps < normal ? small : 0);
             if quick_count >= 3 { quick_count = 0; step = quick }; fps = max(min_fps, fps + step)
elif d < 100: if d < old_d && fps < normal { fps += small }; fps = max(min_fps, fps)
elif d < 150: fps = max(min_fps, fps)
else:        div = ceil(fps / (d / 150))
             d < 200 -> max(min_fps, div); d < 300 -> min(min_fps, div)
             d < 600 -> 150 * min_fps / d;  else -> min(150 * min_fps / d, div)
if d < 150 { increase_count += 1 } else { increase_count = 0 }
if increase_count >= 3 { increase_count = 0; fps += small }
if d > 50 { quick_count = 0 }
fps = clamp(fps, 1, ceiling); first sample -> adjust bitrate immediately
```

Every 1 s tick (send task): if a probe has been outstanding > 2 s → `unresponsive
= true`, add the elapsed time as a delay sample (RustDesk) and force
`fps = UNRESPONSIVE_FPS`; within 1 s of connect/resume `fps = min(fps, INIT_FPS)`.
`target_fps` atomic and `Pacer` follow immediately.

Bitrate (RustDesk `adjust_ratio`, every 3 s, using `d` of the latest sample and
`dynamic = unique_encodes_3s ≥ 6`):

| `d` | multiplier |
| --- | --- |
| < 50 | ×1.15 if dynamic |
| < 100 | ×1.10 if dynamic |
| < 150 | ×1.05 if dynamic |
| < 200 | ×0.95 |
| < 300 | ×0.90 |
| < 500 | ×0.85 |
| ≥ 500 | ×0.80 |

Increases above `SPEED_BITRATE` are limited to +150 kbps per step; result clamped
to `[floor, ceiling]`. Start value = ceiling (RustDesk starts at the target ratio).
`set_quality` sets bitrate to the new ceiling and recomputes `fps_ceiling`
immediately (RustDesk `user_image_quality` "update ratio directly").

#### 2.5.4 Trace (instrumentation the brief relies on)

Target stays `pb_mapper_desktop::feedback`, event `desktop_feedback_interval`,
emitted once per probe sample and per 3 s bitrate decision, fields: `action`
(`fps_up|fps_hold|fps_down|unresponsive|bitrate_up|bitrate_down|bitrate_hold`),
`fps`, `fps_ceiling`, `fps_cap`, `bitrate`, `encoder_bitrate`, `rate_model`,
`rtt_ms`, `baseline_ms`, `delay_ms`, `samples`, `window_frames`, `window_bytes`,
`idr_room`, `outstanding_frames`, `outstanding_bytes`, `ack_ms`, `skipped_ticks`,
`idr_count`, `keyframe_requests`, `unique_fps`, `paused`. Removed fields:
`quality`, `waiting`, `recovering`, `congested_after_ms`, `desired_window_bytes`,
`last_ack_*`, `unacknowledged_age_ms`, `healthy_ms`.

### 2.6 IDR policy and headroom (D7)

- GOP: `Codec::activate` sets `CODECAPI_AVEncMPVGOPSize` to `0x7FFF_FFFF`,
  falling back to `65535` if rejected; reads it back with `ICodecAPI::GetValue`
  into `Statistics.gop_size` and logs it once. Merge gate: `video_bench` over 60 s
  reports exactly the initial IDR plus the forced one (§7.4).
- SPS/PPS on every IDR: unchanged (`Codec::annex_b` prepends the cached sets,
  `D/video/windows.rs:756-786`).
- IDR triggers (complete): new encoder (session start, display/context switch,
  encoder error); `Authority::invalidate` (control grant/revoke/input failure —
  kept because input must reference a post-grant picture, a security property);
  viewer `keyframe` request; `resume`; host resync (outbound full/stale); encoder
  late output > 250 ms (existing internal rule). No periodic IDR.
- `KeyframeLimiter` (`D/session.rs`): `request(reason)` marks pending; `take_due`
  returns true when pending and ≥ `MIN_IDR_INTERVAL = 300 ms` since the last IDR
  *output*; `force(reason)` (invalidate, resync, resume, new encoder) bypasses the
  interval. Requests are never ignored (today `request_keyframe` drops them while
  `recovering`, `D/congestion.rs:234-249`), so recovery never depends on a
  presentation ack.
- Headroom: `IDR_ROOM` (§2.4).

### 2.7 Input path (D10)

#### 2.7.1 Thread

`pb-desktop-input` loop:

```text
loop {
  cmd = input_rx.recv_timeout(until next cursor sample, <= 16 ms)
  if cancel: break                                   // never execute input queued before stop
  if input_overflow.swap(false): release_control("input congestion; control released")
  if policy epoch changed: release_control(None)     // Policy(AtomicU64), as today
  if capture invalidated generation: backend.release()   (release_requested flag)
  if presented_slot changed: renew((g, f))           // validate + backend.presented()
  release_if_stale()                                 // presented frame not in recent < 2 s
  match cmd { Control | Input | Lock | Release }
  if cursor due: sample (§2.8)
}
```

`input_rx` is a `std::sync::mpsc::sync_channel(64)`; the receive task only
`try_send`s (D10: "the receive task must not block on worker replies").

#### 2.7.2 Authority (shared with capture)

```rust
pub(crate) struct Authority {
    generation: u64,                       // starts at 1
    state: HostState,
    context: Option<DesktopContext>,       // capture thread's current context
    recent: VecDeque<(u64, Instant)>,      // produced frames of `generation`, <= 256
    controlling: bool,
    presented: Option<(u64, u64)>,         // last presented/input evidence
    sequence: u64,                         // last accepted input/lock sequence
    input_failure: Option<String>,         // sticky until a new explicit grant
}
impl Authority {
    fn invalidate(&mut self, why: Invalidation, keyframes: &KeyframeLimiter); // gen += 1, clear recent/presented, force IDR
    fn fresh(&self, generation: u64, frame_id: u64) -> bool;                  // streaming, gen match, frame in recent < 2 s
}
```

Rules kept verbatim from `Engine::input` (`D/worker.rs:586-621`): sequence
strictly increasing (updated before other checks); allowed + controlling;
`fresh(event.generation, event.frame_id)`; `presented = (g, f)`; then
`input.input_in_context(&authority.context, &event)`; failure → `release_control`
+ sticky `input_failed: …`. `ControlAdmission` lease lives on the input thread
(`ControlLease` is not `Send`). Policy revoke, desktop change, stop, Engine drop
and helper idle all still release held input.

#### 2.7.3 Desktop binding per injecting thread

- Ordinary: `InputDesktop::context()` checks `available()` (input desktop named
  `Default`, `D/windows.rs:44-66`) and `current_display`; `input_in_context`
  requires equality with `Authority.context` (capture's view) and never rebinds
  after checking.
- Privileged (helper): the worker process runs **two** threads, each with its own
  `DesktopBinding`. Their generations are made consistent by a process-wide
  `BindingRegistry` (`D/helper/worker.rs`):

  ```rust
  static BINDINGS: parking_lot::Mutex<(BindingKey, u64)>;   // (name, EPOCH, console) -> generation
  fn generation_for(key: &BindingKey) -> Result<u64>;      // same key -> same generation;
                                                           // newer EPOCH -> generation + 1;
                                                           // older EPOCH -> Err (stale view, fail closed)
  ```
  `BindingState::enter` takes its generation from the registry instead of its own
  counter, so both threads report identical `DesktopContext.generation` for the
  same desktop and input fails closed on any mismatch. Each binding keeps its own
  `Events` hook thread (EPOCH is already a process-wide static).

#### 2.7.4 Lock (D17)

`ViewerMessage::Lock{generation, frame_id, sequence}` → input queue → same
checks as input (sequence space shared) → `InputBackend::lock`:
ordinary `LockWorkStation()`; helper input lane `Request::Lock{expected}` →
`LockWorkStation()` on the worker input thread (bound to the input desktop).
Failure → `Status.message = "Could not lock the remote computer: …"`; it does not
end control. Success switches desktops → EPOCH → new generation → control needs
a fresh picture again.

### 2.8 Cursor (D11)

- Sampled on the input thread every `CURSOR_INTERVAL = 16 ms` while streaming and
  not paused (moved off the capture thread, `D/worker.rs:206-220`), via
  `InputBackend::cursor(&authority.context)` (privacy rules of
  `relative_cursor` unchanged). A change publishes
  `CursorUpdate{generation, x, y, visible}` into `SessionCore.cursor` (watch).
- The send task writes `HostMessage::Cursor` unless
  `now − last_pointer_input < ECHO_SUPPRESS = 300 ms`; while suppressed it arms a
  timer at the window's end and then sends the latest value, so the final
  position always syncs. `last_pointer_input` is set by the input thread after a
  successful `move`/`button`/`wheel`. `Status` is never suppressed.
- Cursor never enters the outbound queue, never needs credit, never renews input.

### 2.9 Send task

```text
loop select! (biased) {
  cancel                                    -> Stopped{host_stopped}; shutdown
  status changed or 1 s liveness            -> Status
  probe timer (1 s, none outstanding)       -> Probe{nonce}; qos.probe_sent(nonce, now)
  cursor changed && !suppressed             -> Cursor
  outbound head && flow.admit(head) == Send -> (Status first if generation changed) Video; flow.sent(...)
  flow_changed / outbound notify / suppression timer
}
```

`send_delay_ms` = p50 over 1 s of (frame ready → write completed).

### 2.10 Host status (D24)

`Host::status() -> SessionStatus` (public, §3.9) adds:
`session_ms: Option<u64>` (age of the current viewer session, set when the
handshake succeeds, cleared when it ends), `path: String` from
`registration.diagnostics().direct` (`active_direct > 0` → `"direct"`, an active
relay forward → `"relay"`, else `""`), `controlling` (masked by policy as today),
`state` (`waiting` without a viewer). The background service exposes the same
through `Snapshot.host`. "Make view-only" = `set_control_allowed(false)`
(policy epoch → input thread releases and invalidates within ≤16 ms).

### 2.11 Helper IPC (D10) and upgrade (D2)

- One pipe, two **lanes**, each strictly request/response with at most one
  outstanding request:

  ```rust
  #[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  struct Envelope { lane: Lane, request: Request }
  #[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  struct Reply { lane: Lane, response: Response }        // + one bytes record as today
  #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
  #[serde(rename_all = "snake_case")]
  enum Lane { Capture, Input }
  ```
  `Request` changes: `CaptureVideo`/`DrainVideo` → `NextVideo{expected, plan:
  CapturePlan, wait_ms: u32 (≤50)}` (long-poll: the worker's own Pacer runs the
  clock, so capture costs ≈ fps round trips instead of 500/s);
  `Input{expected: DesktopContext, sequence, input}`; `Presented{expected}`;
  new `Cursor{expected}`, `Lock{expected}`; `ConfigureVideo{config: VideoConfig}`.
- Worker process (`helper/worker.rs::run`): the pipe reader routes by lane into
  `capture_tx`/`input_tx` (`sync_channel(1)`; a second request on a busy lane is
  a protocol error → close); a capture thread (`create_privileged` capture half)
  and an input thread (input half, own `DesktopBinding`, the 2 s idle release
  moves here); responses from both threads go through one writer.
- Host process: `HelperLink` thread `pb-desktop-helper-io` owns the pipe and a
  current-thread runtime; `RemoteCapture` and `RemoteInput` send
  `(Envelope, oneshot)` over per-lane channels and block ≤3 s (today's
  `TIMEOUT`); any link failure is sticky for both lanes.
- The service proxy (`helper/service.rs:190-206`, `copy_bidirectional`) is
  unchanged; it does not parse frames.
- Upgrade (D2): `HELPER_PROTOCOL: u32 = 2`; pipes renamed
  `\\.\pipe\pb-mapper.desktop-helper.v2` and `…desktop-background.v2`;
  `installation.json` schema 2 adds `protocol: 2`. `helper::status()` gains
  `protocol_current: bool` (false for schema 1, protocol ≠ 2, or the v2 pipe
  absent while SCM reports running); `ensure_ready` then fails with "Update the
  Windows desktop service to use remote desktop" and the UI offers the existing
  `helper_action install` (whole-bundle copy + restart, `helper/install.rs:488-536`).
  Old service + new UI never exchange a message, so no mid-session parse errors.

### 2.12 Host code and tests to delete

Code: `D/congestion.rs` (whole); `D/media.rs` (host side becomes `Outbound`,
viewer side becomes `VideoQueue`, §3.4); `Worker::sent` write-time heuristic and
`bitrate: Arc<AtomicU32>`; `presentation_feedback` plumbing (`D/host.rs:157-200`,
`D/worker.rs:83-84, 222-243`); the 4 s renderer stall stop; the one-shot startup
probe; cursor stamping into frames (`D/host.rs:227-235`); status serialised twice
per frame (`D/host.rs:239`); `Engine.acknowledged`/`frame_ack_ms`;
`VideoPreferences` (replaced by `VideoLimits` + `Quality`);
`CODECAPI_AVEncMPVGOPSize = fps`; `Encoder::next_frame` + `next_capture`; the
2 ms worker period for H.264 (`D/worker.rs:262-273`); `Backend::{capture_video,
drain_video, input, input_in_context, presented, cursor, release}`.

Tests deleted (premise removed): all 10 in `D/congestion.rs`;
`D/worker.rs` `presentation_backpressure_drains_encoder_without_new_capture`,
`unavailable_desktop_retires_old_ack_debt_and_resumes_control`,
`decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture`;
`D/media.rs` both tests; `D/tests.rs`
`older_authentication_defaults_video_limits_and_rejects_unbounded_preferences`;
`D/slow_link.rs` `presentation_feedback_recovers_after_slow_tls_and_renderer`
and the assertions `slow_consumer must reduce bitrate`, `peak <= MAX_QUEUED_FRAMES`,
`width >= 960`, `frame_ack_ms < 350`, stop-on-no-ACK. Rewritten tests in §7.

---

## 3. Viewer (Rust, `D/viewer.rs`, `D/video_source.rs`)

### 3.1 Structure

```rust
pub struct Viewer {
    shared: Arc<ViewerShared>,
    cancel: CancellationToken,
    connection: Arc<Connection>,
    join: AsyncMutex<Option<JoinSet<()>>>,      // reader, writer, tick
}
/// Everything the FFI hot path touches; no tokio lock, no await.
pub struct ViewerShared {
    controls: Controls,                          // revoke > release > grant (unchanged)
    input: tokio::sync::mpsc::Sender<ViewerMessage>, // cap 32: Input, Lock
    outbox: Outbox,                              // latest-value slots + Notify (below)
    status: parking_lot::Mutex<ViewerState>,     // host status + local metrics
    cursor: parking_lot::Mutex<CursorSnapshot>,  // {generation, x, y, visible, seq}
    video: Arc<VideoShared>,
}
struct OutboxState {
    probe: Option<u64>, received: Option<u64>, keyframe: Option<KeyframeReason>,
    lifecycle: Option<Lifecycle /* Pause | Resume */>, quality: Option<Quality>,
    fps_cap: Option<u32>, presented: Option<(u64, u64)>, presented_sent_at: Option<Instant>,
}
```

Public SDK API: `connect(client, invitation, options, ViewerConfig{codecs, limits,
quality})`, `status()`, `view() -> DesktopView`, `video_source() -> VideoSource`,
`request_control`, `release_input`, `input(InputEvent)`, `lock(g, f, seq)`,
`set_quality`, `pause`, `resume`, `request_keyframe()` (reason `Refresh`),
`shared() -> Arc<ViewerShared>`, `stop()`. Deleted: `connect_with_codecs`,
`connect_with_video_preferences`, `connect_with_presentation_feedback`,
`presented()` (native receipts go through `VideoConsumer`), `frames()`,
`latest_frame()`, `next_frame()`.

### 3.2 Network read task (receipts and probe replies immediately)

```text
loop (READ_IDLE per record) {
  match read_record() {
    Json(Status{s})   -> validate display (1..=32768) and width/height <= limits; set ViewerState.host;
                         video.begin_generation(s.generation, s.display, s.width, s.height) if new
    Json(Cursor{..})  -> if generation == current: cursor.seq += 1, store
    Json(Probe{n})    -> outbox.probe(n)                      // writer sends ProbeReply first
    Json(Stopped{r,m})-> end session with structured error
    Json(Hello{..})   -> protocol error
    Video(h, bytes)   -> validate (§1.3); outbox.received(h.frame_id)   // BEFORE queueing
                         stats.receive(bytes.len());
                         if h.generation < current: drop (already acked)
                         else video.push(unit{.., received: Instant::now(), received_ns: monotonic_ns()})
  }
}
```

The reader never waits for the consumer (push is non-blocking) and acks frames
the queue later drops: credit is transport credit, not decodability.
`monotonic_ns()` = `clock_gettime(CLOCK_MONOTONIC)` on Android/Linux (same clock
as `System.nanoTime()`), `QueryPerformanceCounter` scaled to ns on Windows (same
clock as MSVC `steady_clock`).

### 3.3 Write task

`select! (biased)`: cancel → `shutdown()` → `controls.next()` (drain queued
input as today, `D/viewer.rs:304`) → `outbox` in order **probe_reply, received,
keyframe, pause/resume, set_quality, fps_cap, presented** (presented at most
every `PRESENTED_INTERVAL = 100 ms`, trailing value always flushed) → `input` →
`ping` after 1 s idle. Each write uses `WRITE_TIMEOUT`.

### 3.4 Bounded queue and backlog policy (`VideoQueue` inside `VideoShared`)

| Constant | Value | Origin |
| --- | --- | --- |
| `MAX_UNITS` | 120 | RustDesk `VIDEO_QUEUE_SIZE` |
| `MAX_QUEUED_BYTES` | 16 MiB | memory guard (≥4 max-size IDRs) |
| `BACKLOG_AGE` | 250 ms | RustDesk 150–300 ms "bad" band |
| `BACKLOG_MIN_UNITS` | 3 | ignore one late IDR plus a follower |
| `IDR_GRACE` | 500 ms after an IDR is pulled | Android `configure` cost |
| `DELIVERED_LEDGER` | 256 | host `recent` size |

`push(unit)`:

1. New generation → clear, `wait_key = true`, `discontinuity = true`.
2. H.264 while `wait_key` and not a keyframe → drop (count) and
   `request_keyframe(Resync)`.
3. Keyframe → `wait_key = false`.
4. Overflow (`len ≥ MAX_UNITS` or bytes) → clear, `wait_key`, `discontinuity`,
   `request_keyframe(Overflow)`; keep `unit` only if it is a keyframe.
5. Backlog: not in grace, `len ≥ BACKLOG_MIN_UNITS`, oldest unpulled unit older
   than `BACKLOG_AGE` → clear, `wait_key`, `discontinuity`,
   `request_keyframe(Backlog)`, `backlog_drops += 1`; keep `unit` only if key.
6. Push, `ready.notify_one()`.

The first unit after any clear carries `discontinuity = true`; native decoders
reset on it (replaces the Dart `texture.reset()` barrier). JPEG units are
independent: the queue keeps only the newest JPEG (latest-only slot).

### 3.5 Keyframe request rate limiting (viewer)

`KeyframeRequests` (inside `VideoShared`):

| Class | Reasons | Rule |
| --- | --- | --- |
| decode | `DecoderError`, `Resync` | ≥500 ms apart; interval doubles to 5 s while no presentation succeeds; reset on a presentation |
| backlog | `Backlog`, `Overflow` | first immediate, then ≥2 s, doubling to 10 s; reset after 30 s without backlog |
| user | `Refresh` | ≥1 s |
| lifecycle | `Resume` | once per resume |

All suppressed while paused. Watchdog (moved from Dart
`U/state/desktop_store.dart:418-425`): units delivered but no presentation for
700 ms (not paused) → `Resync` through the limiter. Three `DecoderError`
requests within 10 s without a presentation set
`ViewerState.reason = DecoderFailed` (UI error), replacing today's raw native
error strings.

### 3.6 Video sink consumed by native decoders, and presentation receipts

```rust
pub struct VideoSource(Arc<VideoShared>);
impl VideoSource { pub fn attach(&self) -> Result<VideoConsumer>; }   // exclusive

pub struct VideoConsumer { shared: Arc<VideoShared>, token: u64 }
pub enum Pull { Unit(VideoUnit), Timeout, Paused, Closed }
impl VideoConsumer {
    pub fn next(&self, timeout: Duration) -> Pull;     // parking_lot::Condvar; no runtime
    pub fn presented(&self, generation: u64, frame_id: u64) -> bool;
    pub fn keyframe(&self, reason: KeyframeReason);
    pub fn report_decoder(&self, name: &str);          // <= 64 chars, shown in overlay
}
impl Drop for VideoConsumer { /* detach + notify_all */ }

pub struct VideoUnit {
    pub generation: u64, pub frame_id: u64, pub pts_us: u64,
    pub width: u32, pub height: u32, pub codec: VideoCodec,
    pub keyframe: bool, pub discontinuity: bool,
    pub bytes: bytes::Bytes, pub received: Instant, pub received_ns: u64,
}
```

- `attach` fails if a consumer is attached or the source is closed — **exactly
  one consumer pops video** (D9). `next` records `(generation, frame_id,
  received)` into the delivered ledger and `last_pulled_frame_id`.
- **Input-authorisation receipts are validated here** (single source of truth for
  Dart, the viewer pre-check and the wire): `presented(g, f)` is accepted iff not
  paused, `g` is the current generation, `(g, f)` is in the delivered ledger,
  `f > last_presented.frame_id` (same generation), and
  `received.elapsed() < FRESH_WINDOW = 2 s`. Accepted → `last_presented =
  (g, f, received, now)`; capacity sample (§3.7); receive-to-present sample;
  `outbox.presented(g, f)`. Rejected → `false` (counted). `Viewer::input` and
  `lock` require `video.fresh(event.generation, event.frame_id)` (frame presented,
  `≤ last_presented.frame_id`, same generation, received < 2 s) before queueing
  — fail closed with "a current picture is required". The host re-checks
  independently (`Authority::fresh`).
- Lifetime: `VideoShared` is `Arc`-shared by the `Viewer`, the FFI registry and
  any live `VideoConsumer`. `Viewer::stop`/`Drop` and `pause` call `close()` /
  set paused and `notify_all`, so a blocked `next` returns `Closed`/`Paused`
  within microseconds; memory stays valid while any `Arc` exists.

### 3.7 fps cap from presentation capacity (RustDesk `fps_control`, D8)

Capacity (`CapacityMeter`, RustDesk `fps_calculate` analogue): on an accepted
receipt for frame `N`, if `last_pulled_frame_id > N` (the consumer already held
newer work, i.e. the pipeline was backlogged) and a previous receipt exists in
the same generation, record `sample = now − previous_receipt`. Skip 3 samples
after a discontinuity; every 10 samples set `capacity_fps = count × 1000 / Σms`;
reset at 30. Samples older than 3 s expire → `capacity = None`.

Tick task, every 1 s (skipped while paused or with no units for 15 s):

```text
ceiling = min(limits.max_fps, preset.max_fps)                // never the viewer cap itself
limited = capacity.map_or(ceiling, |c| c * (direct ? 9/10 : 4/5)).min(ceiling)
len     = queued units (VideoQueue)        decode = capacity.unwrap_or(ceiling)
dec     = (len > 1 && last_cap > limited) || len > max(1, decode / 2)
inc     = !dec && idle_ticks(len <= 1) > 3 && last_cap + 3 <= limited
if last_cap.is_none() || dec || inc {
    cap = limited; if dec && limited < max_len_this_tick { cap = limited / 2 }; cap = max(cap, 1)
    if Some(cap) != last_cap { outbox.fps_cap(cap); last_cap = Some(cap) }
}
```

Because the capacity sample only counts intervals in which work was waiting,
throughput limited by the network or the host does not spiral the cap down
(the failure mode of measuring plain presented fps). `direct` is re-sampled from
`connection.diagnostics()` each tick, which also keeps `ViewerState.path` live
(today sampled once, `D/viewer.rs:170-188`).

### 3.8 Pause / resume (D13)

`pause()` → `VideoShared.paused = true`, clear queue, `wait_key`,
`discontinuity`; `next` returns `Paused` (after its timeout, so consumers idle);
keyframe requests and the watchdog are suppressed; `outbox.lifecycle(Pause)`.
`resume()` → `paused = false`, `outbox.lifecycle(Resume)` (host forces an IDR and
restarts QoS at `INIT_FPS`), one `Resume` keyframe allowance. The network tasks
keep answering probes and pinging while paused, so the host sees a live but
paused viewer, never an unresponsive one; no recovery loop can start.
`ViewerState.state = "paused"` while paused (not `stale`).

### 3.9 Public `SessionStatus` (FFI/Dart JSON) v2

```rust
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub state: String,         // waiting|connecting|streaming|capture_unavailable|paused|stale|stopped
    pub message: String,
    pub reason: Option<String>,// secure_desktop|display_lost|encoder_failed|identity_changed|busy|denied|
                               // version_mismatch|viewer_timeout|host_stopped|decoder_failed|input_failed
    pub control_allowed: bool, pub controlling: bool, pub generation: u64,
    pub display: Option<Display>, pub desktop: String,
    pub video: VideoFormat,    // {codec, width, height} only
    pub capture_fps: f32, pub target_fps: u32, pub fps_ceiling: u32, pub bitrate: u32,
    pub rtt_ms: Option<u32>, pub queue_delay_ms: Option<u32>, pub send_delay_ms: f32,
    pub frames: u64, pub bytes: u64, pub dropped_frames: u64,
    pub session_ms: Option<u64>,                 // host status
    pub path: String,                            // host: registration; viewer: live connection
    pub direct_stage: Option<String>, pub direct_failure: Option<String>,
    pub relay_transport: Option<String>,
    pub receive_fps: f32, pub receive_kbps: u32,
    pub presented_fps: f32, pub capacity_fps: Option<f32>, pub fps_cap: Option<u32>,
    pub receive_to_present_ms: Option<f32>,      // p50 over 1 s, viewer clock
    pub picture_latency_ms: Option<f32>,         // send_delay + rtt/2 + receive_to_present
    pub backlog: u32, pub backlog_drops: u64, pub keyframe_requests: u64,
    pub decoder: Option<String>, pub paused: bool, pub quality: Quality,
}
```

**Picture latency** is defined once (overlay, acceptance report, docs):
`picture_latency_ms = send_delay_ms (host p50) + rtt_ms / 2 + receive_to_present_ms
(viewer p50)`. It excludes capture-to-ready (encoder) time and display scan-out;
the camera procedure (§7.5) measures true glass-to-glass.

### 3.10 Viewer code and tests to delete

`media::Queue` use, status coalescing slot and cursor stamping; the
`presentation_feedback` probe gate (`D/viewer.rs:206, 231-233, 306-311`); the
`frames` watch; `latest_frame`/`next_frame`; once-only path sampling. The test
`repeated_releases_are_bounded_and_revocation_precedes_a_new_grant` stays
(`Controls` is unchanged).

---

## 4. FFI (`F/desktop.rs`, new `F/desktop_video.rs`)

### 4.1 JSON ops of `pb_mapper_desktop_call_json`

Hot ops are answered **synchronously without `block_on` and without any tokio
lock** (they read `DesktopState.live: parking_lot::RwLock<Option<Arc<ViewerShared>>>`,
the host `tokio::Mutex::try_lock`, and the parking_lot background cache; the
background refresh spawns with `handle.runtime.spawn`). Dart may call them on the
main isolate. Lifecycle ops keep `block_on` and stay on a worker isolate.

| op | class | request fields | `data` | change |
| --- | --- | --- | --- | --- |
| `inspect_invitation` | hot | `invitation` | `{service, fingerprint: "3F2A-…", format: "pbd1"\|"json_v1"}`; errors `INVALID_ARGUMENT` / `SETUP_STRING` | new (D3) |
| `connect` | lifecycle | `invitation`, `relay_only=false`, `quality="balanced"`, `limits:{max_width,max_height,max_fps}`, `sink:"native"\|"dart"` | `{video_source: u64}` | changed: `native_video` and `video` removed; returns after `Hello`; errors `VERSION_MISMATCH`, `IDENTITY_CHANGED`, `BUSY`, `DENIED` |
| `disconnect` | lifecycle | — | `{}` | changed: takes `live` and the viewer under short guards, closes the video source, then `stop()` outside every lock |
| `status` | hot | — | `{host, viewer, background}` (`SessionStatus` v2) | changed: no viewer lock |
| `control` | hot | `enabled` | `{}` | sync |
| `release` | hot | — | `{}` | sync |
| `input` | hot | `event:{generation, frame_id, sequence, input}` | `{}` | sync; viewer freshness pre-check |
| `lock` | hot | `generation`, `frame_id`, `sequence` | `{}` | new (D17) |
| `keyframe` | hot | — | `{}` | reason `refresh` ("Refresh picture") |
| `set_quality` | hot | `quality` | `{}` | new (D12) |
| `pause` / `resume` | hot | — | `{}` | new (D13) |
| `presented` | — | — | — | **removed** (receipts use `pb_mapper_desktop_video_presented_v1`) |
| `host_*`, `helper_action`, `background_action`, `trust_*`, `capabilities` | lifecycle | unchanged | `capabilities.helper` gains `protocol_current` | — |

Removed exports: `pb_mapper_desktop_frame`, `pb_mapper_desktop_wait_frame`,
`pb_mapper_desktop_free_frame`, `DesktopBuffer`, `export_frame`,
`DesktopState::wait_frame`, `viewer_epoch` (the registry ids replace it).

### 4.2 Per-vsync view (Dart, D11)

```rust
#[repr(C)]
pub struct DesktopViewV1 {          // 56 bytes, little-endian native layout
    pub generation: u64,            //  0
    pub presented_frame_id: u64,    //  8  latest accepted receipt in `generation`, 0 none
    pub cursor_seq: u64,            // 16  increments per new cursor
    pub presented_age_ms: u32,      // 24  now - receive time of that frame; u32::MAX none
    pub display_width: u32,         // 28  physical px of `generation`
    pub display_height: u32,        // 32
    pub video_width: u32,           // 36
    pub video_height: u32,          // 40
    pub cursor_x: i32,              // 44
    pub cursor_y: i32,              // 48
    pub state: u8,                  // 52  0 none,1 connecting,2 streaming,3 capture_unavailable,4 paused,5 stopped
    pub flags: u8,                  // 53  bit0 cursor_visible, bit1 controlling, bit2 control_allowed, bit3 secure_desktop
    pub reserved: [u8; 2],          // 54
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_view_v1(handle: *mut PbMapperHandle, out: *mut DesktopViewV1) -> i32;
// 1 = viewer present, 0 = none (out zeroed), -1 = null pointer. Leaf: no allocation,
// no tokio, parking_lot read locks only, panics caught (-1). UI-isolate safe.
```

### 4.3 Video C exports (Windows runner and the Dart JPEG isolate)

```c
typedef struct PbDesktopVideoUnitV1 {    /* 48 bytes */
    uint64_t generation, frame_id, pts_us, received_ns;
    uint32_t width, height, len;          /* len = bytes written, or needed when -2 */
    uint8_t  codec;                       /* 0 H.264 Annex B, 1 JPEG */
    uint8_t  flags;                       /* bit0 keyframe, bit1 discontinuity */
    uint16_t reserved;
} PbDesktopVideoUnitV1;

uint64_t pb_mapper_desktop_video_attach_v1(uint64_t source);
    /* consumer id; 0 = unknown/closed source or already attached */
int32_t  pb_mapper_desktop_video_next_v1(uint64_t consumer, uint8_t *buf, uint32_t cap,
                                         uint32_t timeout_ms /* <= 1000 */,
                                         PbDesktopVideoUnitV1 *out);
    /* 1 unit copied; 0 timeout; -1 closed/unknown; -2 buffer too small (out->len = needed,
       unit stays queued); -3 paused; -4 invalid argument/panic */
int32_t  pb_mapper_desktop_video_presented_v1(uint64_t consumer, uint64_t generation, uint64_t frame_id);
    /* 1 accepted; 0 rejected (stale, older, never delivered, paused); -1 closed/unknown */
int32_t  pb_mapper_desktop_video_keyframe_v1(uint64_t consumer, uint32_t reason);
    /* reason: 0 decoder_error, 1 resync, 2 backlog, 3 overflow; 1 queued, 0 rate-limited, -1 closed */
int32_t  pb_mapper_desktop_video_report_v1(uint64_t consumer, const char *decoder_utf8 /* <= 64 */);
void     pb_mapper_desktop_video_detach_v1(uint64_t consumer);   /* wakes a blocked next */
uint64_t pb_mapper_desktop_instance_v1(void);                    /* address of a static; D9 check */
```

Rules: every export is a leaf (no handle pointer, no callbacks), wraps its body in
`std::panic::catch_unwind`, never `unwrap`s. Waiting in `next` uses the
`VideoShared` `parking_lot::Condvar`, no tokio runtime, so it cannot block the
FFI runtime and survives handle destruction safely (it returns `-1`).

### 4.4 Android JNI (Kotlin feeder)

`jni = "=0.21.1"` (already in `Cargo.lock`, the version RustDesk uses) added to
root `[workspace.dependencies]` and to `F/../Cargo.toml` under
`[target.'cfg(target_os = "android")'.dependencies] jni.workspace = true`.
`F/desktop_video.rs` `#[cfg(target_os = "android")] mod jni_exports`:

| Kotlin (`object DesktopVideoNative`, package `com.pb_mapper.ui`) | Rust export | Notes |
| --- | --- | --- |
| `@JvmStatic external fun attach(source: Long): Long` | `Java_com_pb_1mapper_ui_DesktopVideoNative_attach(env, class, source: jlong) -> jlong` | 0 = refused |
| `@JvmStatic external fun next(consumer: Long, buffer: ByteBuffer, meta: LongArray, timeoutMs: Int): Int` | `…_next(env, class, consumer: jlong, buffer: JByteBuffer, meta: JLongArray, timeout_ms: jint) -> jint` | `buffer` must be direct; `meta` (len 8) = `[generation, frameId, ptsUs, receivedNs, width, height, len, flags or (codec shl 8)]`; same codes as the C export |
| `@JvmStatic external fun presented(consumer: Long, generation: Long, frameId: Long): Int` | `…_presented` | called from the codec HandlerThread |
| `@JvmStatic external fun keyframe(consumer: Long, reason: Int): Int` | `…_keyframe` | |
| `@JvmStatic external fun report(consumer: Long, decoder: String): Int` | `…_report(…, decoder: JString)` | |
| `@JvmStatic external fun detach(consumer: Long)` | `…_detach` | |
| `@JvmStatic external fun instance(): Long` | `…_instance` | same static as `pb_mapper_desktop_instance_v1` |

`DesktopVideoNative` has `init { System.loadLibrary("pb_mapper_ffi") }`. Pull
design ⇒ no `JNI_OnLoad`, no cached `JavaVM`, no Rust-attached threads, no
`ndk_context` initialisation (`netdev` would change behaviour). Threading: every
call comes from an already attached Java thread (feeder thread, codec
HandlerThread). The single-instance check (D9): at first `create`, Kotlin logs
`instance()` and Dart logs `pb_mapper_desktop_instance_v1()` at debug level;
equal values prove one loaded image (both resolve the same soname in the app
linker namespace, as RustDesk's `Java_ffi_FFI_*` + Dart FFI do).

### 4.5 Registry, single consumer, lifetime

`F/desktop_video.rs`:

```rust
static REGISTRY: LazyLock<parking_lot::Mutex<Registry>>;
struct Registry {
    next_id: u64,                                   // never reused
    sources: HashMap<u64, Arc<VideoShared>>,        // source id -> shared (removed on disconnect)
    consumers: HashMap<u64, VideoConsumer>,         // consumer id -> exclusive consumer
}
```

- `connect` registers the new viewer's `VideoSource` and returns its id;
  `sink:"dart"` (JPEG) works the same way (the Dart isolate attaches).
- `attach(source)` → `VideoSource::attach()` (exclusive) → new consumer id.
  Calls look up the consumer, clone its `Arc` under the registry lock, release
  the lock, then wait. A stale or detached id returns `-1`; ids are plain integers,
  never pointers, so a late native call can never touch freed memory or a
  replaced session.
- `disconnect`: clear `live`, close the source (`next` → `-1` at once), remove the
  source id, stop the viewer outside the locks. Consumers stay registered until
  `detach` (they report `-1`); `connect` sweeps consumers of closed sources.
- `pb_mapper_destroy` drops `DesktopState` → `Viewer::drop` closes the source, so
  native threads exit even if Dart never called `disconnect`.

---

## 5. Native decoders

### 5.1 Android (`K/DesktopVideo.kt`, new `K/DesktopVideoNative.kt`, `K/VideoFeeder.kt`)

1. **Channel** `pb_mapper/desktop_video` methods after the change:
   `create {source: Long} -> textureId`, `rebind {textureId, source}` (reconnect
   keeps the texture and its last picture), `snapshot {textureId, path, maxWidth}
   -> Boolean`, `stats {textureId}`, `dispose {textureId}`. Removed: `submit`,
   `reset`, native→Dart `event` (presented and needKeyframe now go to Rust).
2. **Feeder thread** (`VideoFeeder`, one per `Viewer`, `Thread("pb-desktop-feed")`,
   `Process.setThreadPriority(THREAD_PRIORITY_DISPLAY)`):

   ```kotlin
   interface VideoPull { fun next(buffer: ByteBuffer, meta: LongArray, timeoutMs: Int): Int }
   class VideoFeeder(private val pull: VideoPull, private val sink: (Packet) -> Unit,
                     private val pool: BufferPool, private val maxQueued: Int = 2) {
       // loop: wait (lock/condition) while queued >= maxQueued; buf = pool.take();
       //  code = pull.next(buf, meta, 100)
       //  1 -> sink(Packet(buf, meta...)) ; -2 -> pool.grow(meta[6]) and retry ; 0 -> continue
       //  -3 -> paused = true (Viewer suppresses stall checks) ; -1 -> exit, detach
   }
   ```
   `BufferPool`: 3 direct `ByteBuffer`s, initial 1 MiB, grown on `-2` up to
   `MAX_BYTES` (4 MiB). `Packet` carries the pooled buffer + `len`; the buffer
   returns to the pool after `queueInputBuffer` (copies per frame: Rust → direct
   buffer → codec input, down from ≈6).
3. **Submit logic** moves from `submit(Packet, MethodChannel.Result)`
   (`K/DesktopVideo.kt:217-252`) into `accept(packet)` with no reply object:
   `discontinuity` or a new generation → `revision++`, `clearQueue()`,
   `awaitingKey = true`; `queue.size >= 4` stays as a defensive bound (never
   reached with `maxQueued = 2`); non-key while awaiting → drop +
   `DesktopVideoNative.keyframe(consumer, 1 /* resync */)`. `pump()`, `configure()`,
   `renderLatest()` and the single-picture surface credit are unchanged.
4. **Presentation receipt path**: `consumeSurface()` (`:180-215`) calls
   `DesktopVideoNative.presented(consumer, packet.generation, packet.frameId)`
   in place of `event(packet, true, false)`; local `presented` counter and the
   new interval histogram update there.
5. **Keyframe requests**: `fail(packet, reason)` → `DesktopVideoNative.keyframe
   (consumer, 0 /* decoder_error */)`; `configure()` success →
   `DesktopVideoNative.report(consumer, codec.name)`. Rate limiting is Rust's.
6. **Background pause**: when the feeder sees `-3` it sets `paused`; `pump()`
   skips the "texture was not consumed" and "decoder stalled" 2 s checks
   (`:524-532`) while paused, and does not call `fail`. The next unit after resume
   carries `discontinuity` → `revision++` → codec rebuilt on the resume IDR.
7. **Snapshot (D23)**: `snapshot(path, maxWidth)` posts to the codec worker:
   if a picture was presented, `PixelCopy.request(surface, bitmap(≤maxWidth wide,
   aspect of the last packet), listener, worker)`; on success
   `bitmap.compress(JPEG, 80)` to `path` (app-private
   `filesDir/desktop-thumbnails/<computer>.jpg`, path chosen by Dart); reply via
   `main.post`. Any failure or > 300 ms → reply `false` (placeholder). Never
   blocks dispose.
8. **Stats**: keep `presented`, `decoded`, `dropped`, `decoder`, timings; add
   `present_interval_ms` buckets `[≤17, ≤20, ≤25, ≤34, ≤50, ≤100, ≤250, >250]`,
   `max_interval_ms`, `gaps_over_100`, `gaps_over_250`.
9. **Decoder hints** (measured, behind one constant each): `KEY_PRIORITY = 0`
   (realtime) and `KEY_OPERATING_RATE = 60` in `configure`; HandlerThread
   `pb-desktop-video` at `THREAD_PRIORITY_DISPLAY`. These target the DVFS
   "burst, stall" hypothesis; keep only if the on-device interval histogram
   improves.
10. `MainActivity` is unchanged (it still constructs `DesktopVideo` and disposes
    it in `cleanUpFlutterEngine`); `EnableImpeller=false` stays (P1-4).

### 5.2 Windows runner (`W/desktop_video.cpp`)

1. `struct VideoApi { attach, next, presented, keyframe, report, detach }`
   resolved once in `Impl` via `GetModuleHandleW(L"pb_mapper_ffi.dll")` +
   `GetProcAddress` (Dart has loaded the DLL before `connect`). Missing symbol →
   `create` fails ("pb_mapper_ffi.dll is out of date"). The runner still does not
   link the DLL.
2. Channel: `create {source}`, `rebind {textureId, source}`, `snapshot {textureId,
   path, maxWidth}`, `stats`, `dispose`; `submit`/`reset` and event delivery
   removed. `Events` shrinks to a reply queue for `snapshot` (replies must run
   on the platform thread; `kWake` + `HandleMessage` stay for that).
3. Decoder thread `Run()`: while `consumer_ == 0` wait on `wake_` (rebind/close);
   else `api.next(consumer_, buf_.data(), buf_.size(), 50, &unit)`:
   `-2` → grow `buf_` to `unit.len` (≤4 MiB) and retry; `-3` → paused, continue;
   `-1` → detach, `consumer_ = 0`; `1` → the generation/revision/awaiting-key
   logic formerly in `Submit` (`:144-180`), `discontinuity` → `++revision_`;
   `Feed` → `Take` → `latest_` → `MarkTextureFrameAvailable` (unchanged).
4. Receipts: `AcquireSurface()` (raster thread, first acquisition of
   `(revision, frame)`) calls `api.presented(consumer_, gen, frame)` instead of
   `events_->Send`. Non-blocking (parking_lot lock, watch send).
5. Keyframes: `Fail()` → `api.keyframe(consumer_, 0)`; a P-frame at a new size →
   `api.keyframe(consumer_, 1)`. Report decoder: `"Media Foundation DXVA (<adapter
   description>)"`.
6. Last picture across reconnect/recovery: on a revision bump keep `latest_` for
   display but acknowledge only pictures of the current revision (fixes the blank
   picture after `Reset`/`Fail`, native map "sharp edges"). Snapshot (D23): on the
   decoder thread, `CopyResource(latest_ → staging)`, `Map`, box-downscale to
   `maxWidth`, encode JPEG via WIC (`windowscodecs.lib`), write `path`, post the
   reply; 300 ms bound.
7. Stats add the same interval histogram as Android. Pause: Dart lifecycle
   (§6.4); the runner needs no window hook.
8. Optional, measure first: a 3-texture BGRA pool respecting `SurfaceLease`
   instead of a new shared texture per decoded frame (`:433-441`).

---

## 6. Dart

### 6.1 `DesktopApiClient` after the change (`U/ffi/desktop_api.dart`)

```dart
enum DesktopQuality { quality, balanced, smooth }
enum DesktopVideoSink { native, dart }
class DesktopVideoLimits { final int maxWidth, maxHeight, maxFps; }
class DesktopInvitationInfo { final String service, fingerprint, format; }
class DesktopView {            // mirror of DesktopViewV1 (ffi Struct in pb_mapper_ffi.dart)
  final int generation, presentedFrameId, presentedAgeMs, cursorSeq;
  final int displayWidth, displayHeight, videoWidth, videoHeight, cursorX, cursorY;
  final DesktopViewState state; final bool cursorVisible, controlling, controlAllowed, secureDesktop;
  bool get fresh => state == DesktopViewState.streaming && presentedFrameId != 0 && presentedAgeMs < 2000;
}

abstract interface class DesktopApiClient {
  // host side: unchanged signatures (enableHost, backgroundAction, trustStatus, restoreHost,
  // revokeTrust, helperAction, capabilities, startHost, stopHost, allowControl)
  Future<DesktopInvitationInfo> inspectInvitation(String invitation);
  Future<int> connect(String invitation, {required bool relayOnly, required DesktopQuality quality,
      required DesktopVideoLimits limits, required DesktopVideoSink sink});   // video source id
  Future<void> disconnect();
  DesktopSnapshot status();                 // sync hot op
  DesktopView? view();                      // sync leaf, per vsync
  void control(bool enabled); void release(); void keyframe();
  void input({required int generation, required int frameId, required int sequence,
      required Map<String, Object?> input});
  void lock({required int generation, required int frameId, required int sequence});
  void setQuality(DesktopQuality quality); void pause(); void resume();
  Stream<DesktopJpegFrame> jpegFrames(int source);           // sink == dart only
  void presentedJpeg(int consumer, int generation, int frameId);
}
```

Removed members: `frames()`, `frame()`, `presented()`, the `connect` named
`video` map. `PbMapperService` gains `desktopCallSync` (main isolate, whitelisted
hot ops) and `desktopView`; `_desktopFramesWorker`, `DesktopFrameStream`,
`_desktopFrameIsolate`, `desktopFrames`, `desktopFrame`, the `presented/keyframe/
input/control/release` routing into the frame isolate, and the `DesktopBuffer`
lookups are deleted. JPEG uses `_desktopJpegWorker`: attaches, loops
`pb_mapper_desktop_video_next_v1(…, 100 ms)` into a `calloc` 4 MiB buffer, sends
`TransferableTypedData` + meta to main, exits on `-1`; it holds no handle pin.

`connect` limits: Android `max_fps = min(refreshRate.round(), 60)`, Windows/desktop
`min(refreshRate.round(), 120)`, both `1920×1080`; quality from the per-computer
preference (default Balanced).

### 6.2 `DesktopVideoTexture` (`U/ffi/desktop_video.dart`)

`create(int source)`, `rebind(int source)`, `snapshot(String path, {int maxWidth = 320})
-> Future<bool>`, `stats()`, `dispose()`. `DesktopVideoEvent`, `submit`, `reset`
and the static event handler are deleted. The store creates the texture once per
viewer and `rebind`s it on reconnect, so "Reconnecting" can show the greyed last
picture.

### 6.3 `DesktopStore` surface (`U/state/desktop_store.dart`)

Exposes: `snapshot` (host/viewer/background, replaced only when something
relevant changed), `connectionPhase`, `error`/`reason`, `quality`
(`DesktopQuality`, persisted per remembered computer; `setQuality` is live),
`paused`, `videoTexture`, `view` (`ValueListenable<DesktopView?>`, updated by a
frame callback only while a viewer widget is attached), `fresh` and `controlling`
(`ValueListenable<bool>`s derived from `view`), `stats`
(`ValueListenable<DesktopStats>` for the overlay: path, relayTransport, rttMs,
pictureLatencyMs (+ components), presentedFps, receiveKbps, bitrate, targetFps,
fpsCap, width×height, codec, decoder, queueDelayMs), `sharing`
(`ValueListenable<DesktopSharing>`: hosting, viewer connected, viewer controlling,
path, sessionMs). Methods: `connect(invite, …)`, `disconnect()` (best-effort
thumbnail first), `attachViewer()/detachViewer()`, `control`, `release`, `send`,
`lock`, `requestKeyframe`, `setQuality`, host methods unchanged.

Input: `send` reads `view.value` (`generation`, `presentedFrameId`), refuses
unless `fresh && controlling`, coalesces pointer moves to one per frame callback,
and calls `api.input` synchronously; a `DesktopCallException` →
`_interruptInput()` (ends control, as today). `_inputEpoch`/`_sequence` rules are
unchanged (reset per session).

Deleted: `_consumeFrames`, `_acceptFrame` (H.264 half and heartbeat branch),
`_presentVideo`, presentation half of `_flushFeedback`, `_videoStatuses`,
`acknowledgePresented` (JPEG → `presentedJpeg` after paint), `videoPresentation`,
`cursorStatus` (cursor now comes from `view`), `presentFps` (Rust
`presented_fps`), the `poll()` pull path, the 700 ms keyframe watchdog (Rust
§3.5), `frameAckMs`/`delayed` (now `queue_delay_ms > 150` or
`picture_latency_ms > 250`), dead `startHost`/`backgroundAction`.

### 6.4 Pause/resume (D13)

An `AppLifecycleListener` in the store: `onHide`/`onPause` → `api.pause()`;
`onShow`/`onResume` → `api.resume()` (Flutter Windows reports `hidden` when
minimised). `detachViewer()` (viewer route not visible while the session lives,
"Back to picture") also pauses. While paused: no status polling, no reconnect
timer, no keyframe requests (Rust suppresses them too), control released as
today (`setViewerForeground(false)`).

### 6.5 Notification and polling policy (D14)

- `AppStore.desktop` drops `..addListener(_notify)` (`U/state/app_store.dart:40-42`);
  AppStore notifies once when the store is created. The rail dot, tray and the
  D15 "being viewed" bar listen to `desktop.sharing` only.
- `DesktopStore` calls `notifyListeners` only when phase, error, host state,
  quality, paused or texture identity change; media-rate data flows through
  `view`/`stats` notifiers to the widgets that paint them.
- Polls (`status` is a sync call now, no isolate per poll): viewer 250 ms only
  while a session exists, a viewer is attached and not paused; overlay stats
  piggyback at 1 Hz; host 1 s only while hosting (local or background enabled);
  otherwise no timer. Today's 250 ms timer is never cancelled
  (`desktop_store.dart:149, 888`).

### 6.6 Thumbnail (D23)

`disconnect()`: if the computer is remembered, the picture is `fresh`, and no
identity mismatch → `await videoTexture.snapshot(path).timeout(400 ms)`
(best-effort), then `api.disconnect()`. JPEG path encodes its own `ui.Image`.
Deleting a remembered computer deletes its thumbnail.

---

## 7. Test plan and on-device measurement

### 7.1 Rust unit tests (`cargo test -p pb-mapper-desktop`)

| Test | File | Covers |
| --- | --- | --- |
| `fps_follows_queue_delay_and_unresponsive_peer` | `D/qos.rs` | <50 ms climbs to ceiling with scaled steps; 200 ms divides; probe outstanding >2 s → 2 fps; recovery; INIT cap for 1 s |
| `bitrate_steps_every_three_seconds_only_on_dynamic_content` | `D/qos.rs` | multipliers, +150 kbps cap above 1 Mbps, floor/ceiling per preset, `set_quality` jump, no resolution field |
| `receipts_return_credit_and_idr_uses_headroom` | `D/flow.rs` | window formula, cumulative receipts, IDR admitted with frames in flight, empty ledger admits any size |
| `capture_paces_evenly_and_skips_ticks_while_handoff_is_full` | `D/session.rs` (fake async backend) | no burst after the window reopens (≤1 submit per interval), skipped ticks counted |
| `fps_and_quality_changes_never_rebuild_the_encoder` | `D/session.rs` | `configure_video` called once; display switch rebuilds once with an IDR |
| `keyframe_requests_are_never_dropped_and_are_rate_limited` | `D/session.rs` | 10 requests in 100 ms → 1 IDR now, 1 after 300 ms; forced bypass |
| `input_runs_while_capture_is_blocked` | `D/session.rs` | capture `next_video` blocks 500 ms; input injected < 50 ms |
| `receipts_never_authorise_input` | `D/session.rs` | receipts only, no `presented` → input refused; held input released after 2 s |
| `input_requires_control_current_geometry_and_unique_sequence` | rewritten from `D/worker.rs:677` | same rules across two threads; capture context change invalidates |
| `native_input_failure_ends_control_and_survives_new_video` | kept/adapted | sticky failure |
| `observation_never_acquires_write_admission_and_revocation_releases_it` | kept/adapted | lease on input thread |
| `unavailable_backend_reports_failure_and_stops` | kept | |
| `cursor_is_not_echoed_within_300ms_of_pointer_input` | `D/session.rs` | suppression and trailing sync |
| `lock_requires_control_and_a_fresh_frame` | `D/session.rs` | D17 gating, failure keeps control |
| `backlog_drops_to_next_idr_and_rate_limits_requests` | `D/video_source.rs` | age rule, grace after IDR, overflow, limiter |
| `presentation_receipts_require_a_delivered_current_recent_frame` | `D/video_source.rs` | stale, other generation, never delivered, duplicate/older, paused → rejected |
| `single_consumer_and_close_wakes_a_blocked_next` | `D/video_source.rs` | exclusive attach; `next` returns Closed < 10 ms after close |
| `fps_cap_follows_capacity_not_throughput` | `D/video_source.rs` | input-limited throughput keeps the cap; backlogged pipeline lowers it 90 %/80 %; halving rule |
| `pause_suppresses_recovery_and_resume_requests_one_idr` | `D/video_source.rs` | D13 |
| `v2_records_round_trip_and_reject_unknown_fields` | `D/wire.rs` | JSON + video header strictness; `clean_tls_close_is_eof_only_between_messages` kept |
| `alpn_mismatch_reports_version_mismatch` | `D/tests.rs` | v1 ALPN client vs v2 server |
| `pbd1_round_trip_pins_fingerprint_and_rejects_setup_strings` | `D/lib.rs` | encode/decode, v1 JSON accepted, setup string error, pin mismatch → "desktop identity changed"; `tls_rejects_an_unpaired_host_identity` kept |
| `binding_generation_is_shared_between_threads` | `D/helper/worker.rs` (pure registry logic, all platforms) | same key → same generation; older EPOCH fails |
| `lanes_answer_input_while_capture_waits` | `D/helper/client.rs` (Windows, duplex pipe fake) | per-lane request/response independence |

Kept unchanged: `remembered_invitation_survives_restart_but_not_revocation`,
`blocked_video_does_not_block_local_stop` (adapted to v2), scheduler, windows
held-set, cursor privacy, `scaled_size`, parameter-set tests, helper binding tests.

### 7.2 Integration (`crates/pb-mapper-desktop/tests/session.rs`)

- `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown` — kept,
  adapted: a consumer presents via `VideoConsumer::presented`; input needs
  control and a presented post-grant frame; receipts alone never enable input;
  1000 releases coalesce; revoke; direct survives relay stop; teardown drops both
  backends once.
- `frozen_renderer_keeps_receiving_but_cannot_control` — replaces
  `cursor_crosses_a_full_video_window_without_refreshing_the_picture`: a consumer
  that pulls but never presents still receives at the host's fps (credit on
  receipt), cursor messages flow, input is refused and held input is released
  after 2 s.
- `paused_viewer_receives_no_video_and_resumes_with_an_idr`.
- `busy_and_denied_are_reported_before_close`.

Ignored acceptance (`D/slow_link.rs`, rewritten): `fixed_wan_keeps_full_rate`
(350 ms RTT: after baseline, fps = ceiling, bitrate = ceiling, resolution
constant); `bandwidth_limit_lowers_fps_and_bitrate_not_resolution` (proxy at
1 Mbps); `slow_presenter_is_capped_by_fps_cap_not_host_backpressure`;
`unresponsive_viewer_drops_to_two_fps_then_recovers`;
`sustained_slow_tls_reader` kept (host stays streaming, stop < 1 s).

### 7.3 FFI, Dart, Kotlin

- FFI (`cargo test -p pb-mapper-ffi`): `hot_ops_do_not_wait_for_disconnect`
  (disconnect stalled in `stop()`; `status`/`input`/`view` return < 10 ms);
  `video_consumer_ids_are_single_use_and_stale_ids_are_harmless`. Deleted:
  `status_and_video_with_the_same_id_both_cross_the_binary_boundary`,
  `binary_frame_is_owned_until_explicit_release_after_disconnect`. Kept: the
  connect/disconnect cancellation tests and trust tests. Build check for JNI:
  `cargo ndk -t arm64-v8a check -p pb-mapper-ffi` locally (CI already builds
  the Android library in `release-ui.yml`).
- Dart (`flutter test`, runs in CI because nothing depends on a real texture;
  the store takes a `DesktopVideoTextureFactory` so the old CI-skipped lifecycle
  tests become platform-independent): `fresh_and_controlling_come_only_from_the_view`;
  `input_uses_the_presented_frame_and_stops_when_the_view_goes_stale`;
  `background_pauses_without_keyframes_or_reconnects_and_foreground_resumes`;
  `quality_change_is_live_without_reconnect`;
  `polling_stops_when_nothing_needs_it_and_app_store_is_not_notified` (D14);
  `reconnect_rebinds_the_same_texture`. `desktop_recovery_test.dart` and the host
  readiness tests are adapted to the new `FakeDesktopApi` (implements every new
  member; `view()` scriptable). Deleted: `desktop_lifecycle_test.dart:98-143`
  (ACK cadence), `:144-197` (`DesktopFrameStream`); `:199-241`, `:243-334`
  rewritten as above. `desktop_gesture_test.dart` pinch/transform invariant kept
  (P1-4).
- Kotlin (`./gradlew :app:testDebugUnitTest`): `VideoFeederTest` with a fake
  `VideoPull` — stops pulling at 2 queued and resumes on drain; `-2` grows the
  buffer and retries; `-3` sets paused and the stall check is suppressed; `-1`
  exits and detaches. `VideoPresentationQueueTest` kept. Add a
  `gradle unit test` step to `syntax-check.yml` (JDK 17, no emulator) so these
  run in CI.

### 7.4 Host bench gates (Windows GPU)

`video_bench` additions: `--fps-schedule 60,30,15,5` (10 s each, nominal fixed)
reports per phase `unique_encoded_fps`, `bitrate_mbps`, `rate_model`; pass when
achieved bitrate is within ±25 % of target at every phase with compensation on
the vendor's model. `--seconds 60` prints `gop_size` (read back) and
`keyframes`; pass when `keyframes == 2` (initial + the forced one at 1.5 s).
Record NVIDIA/Intel/AMD results in `docs/validation/`.

### 7.5 On-device measurement (matches the perf doc acceptance; D28)

1. Host: `deployed.exe APP_CONFIG target/desktop-acceptance serve 600 --tcp
   --control --feedback-log`; moving content `video_bench.exe 600 60 --pattern-only`.
2. Phone: `flutter build apk --release --target tool/desktop_viewer_acceptance.dart
   --dart-define=setupUrl=http://127.0.0.1:18747/controller.json
   --dart-define=reportPath=<app files dir>/acceptance.json --dart-define=seconds=300`,
   `adb reverse tcp:18747 tcp:18747`, `node scripts/test/desktop-bootstrap.cjs
   target/desktop-acceptance`, run; `adb shell run-as com.pb_mapper.ui cat
   files/acceptance.json > report.json`. Never install it as the product entry.
3. Report schema 2 (`desktop_viewer_acceptance.dart`): per second
   `presented_fps`, `capacity_fps`, `fps_cap`, `target_fps`, `receive_fps`,
   `receive_kbps`, `bitrate`, `rtt_ms`, `queue_delay_ms`, `send_delay_ms`,
   `receive_to_present_ms`, `picture_latency_ms`, `backlog`, `backlog_drops`,
   `keyframe_requests`, `dropped_frames`, `path`, `relay_transport`, `quality`,
   `decoder`, `paused`; native interval histogram, `gaps_over_100/250`,
   `max_interval_ms`; Flutter build/raster timings; plus device model, battery
   temperature at start/end, Wi-Fi/cellular, host GPU.
4. Matrix: direct Wi-Fi Balanced; relay (TCP and QUIC carriers) Balanced;
   cellular Balanced; direct Smooth and Quality; background 30 s then resume.
   Same content with RustDesk (same host, phone, network, 1080p) for comparison.
5. Proposed thresholds (the user decides; D28 keeps the merge block until then):
   direct Wi-Fi Balanced on a 60 Hz phone — presented fps p50 ≥ 55 and ≥ 90 % of
   host unique capture fps, interval p95 ≤ 25 ms, zero gaps > 250 ms in 5 min,
   ≤ 1 gap > 100 ms per minute, picture latency p50 ≤ 80 ms; relay — ≥ 30 fps
   with no gaps > 250 ms outside adaptation; background/resume — first picture
   ≤ 1 s after resume, zero session stops.
6. Glass-to-glass: film host and phone side by side showing the
   `Unique desktop presentation N` counter; report median of 20 readings.

---

## 8. Risks and implementation sequence

### 8.1 Risks and mitigations

| # | Risk | Mitigation |
| --- | --- | --- |
| R1 | `System.loadLibrary` and Dart `dlopen` produce two library instances (two registries) | `instance()` vs `pb_mapper_desktop_instance_v1()` logged and asserted in a debug build; on mismatch fall back to the documented MethodChannel path (`makeBackgroundTaskQueue`, 3 submits in flight) before release |
| R2 | Vendor MFT clamps or ignores `AVEncMPVGOPSize` | read-back + `video_bench` gate (§7.4); an unexpected periodic IDR costs bandwidth only, because IDR headroom admits it |
| R3 | CBR budget at reduced fps (D6) | frame-budget compensation + overshoot guard; per-vendor bench evidence |
| R4 | Loopback + QUIC buffering hides congestion | receipt window bounds written bytes; in-order probe measures what is buffered |
| R5 | Slow fps ramp at session start | scaled steps (≈6 s to 60 fps); constants in one module; on-device trace |
| R6 | IDR loop from backlog right after a resync | 500 ms grace after an IDR is pulled, limiter backoff, host 300 ms minimum |
| R7 | Capacity estimate spirals the cap down | only backlogged intervals count; increase rule after 3 idle ticks; unit test |
| R8 | Two desktop-bound threads disagree on generation (helper) | process-wide `BindingRegistry`; any mismatch fails closed; unit test + UAC acceptance rows |
| R9 | Old installed helper | v2 pipes + schema 2 + `protocol_current` → reinstall prompt |
| R10 | Dart freshness and host lease disagree | one validation point (Rust `VideoShared`) feeding Dart, viewer pre-check and the wire; host re-checks independently |
| R11 | Android stall checks fire while backgrounded | feeder `-3` → paused suppresses checks; Rust suppresses keyframes; Dart stops polling |
| R12 | PixelCopy cannot read a SurfaceTexture-backed Surface on some devices | 300 ms bound, placeholder, never blocks close (D23) |
| R13 | Sync JSON hot ops on the UI isolate become slow | whitelist, no tokio locks (test), moves coalesced per frame; fall back to a long-lived control isolate if a profile shows > 0.5 ms |
| R14 | Phone network suspended in background (Doze) | host `READ_IDLE` 10 s ends the session; Dart reconnects on foreground with the same target |
| R15 | Upstream receipt traffic on cellular | ≈60 × 60 B/s, coalesced by the outbox; negligible against video |
| R16 | Removing per-frame status loses metadata Dart used | `Status` per generation + `view` leaf carry geometry; video header carries per-frame fields to native |

### 8.2 Ordered implementation sequence (each step builds and tests green)

1. **Invite and ALPN** (`D/lib.rs`, `D/security.rs`, FFI `inspect_invitation`,
   error codes). Gate: `cargo test -p pb-mapper-desktop`, `cargo clippy
   --workspace --all-targets -- -D warnings`.
2. **Wire v2 + flow + QoS + probe** (`wire.rs`, `flow.rs`, `qos.rs`,
   `quality.rs`, `session.rs` send/recv tasks; delete `congestion.rs`, host
   `media.rs`). Viewer keeps a temporary adapter that still feeds the old FFI
   frame export so the Windows app runs (presentation used only for input). Gate:
   unit tests, `tests/session.rs`, ignored `slow_link` run locally, Windows app
   smoke.
3. **Encoder and capture** (Pacer, `next_video`, `submit`/`drain`, rate model,
   infinite GOP, keyframe limiter; examples `video_bench`, `deployed`,
   `viewer_probe` updated). Gate: bench §7.4 on the host GPU.
4. **Input thread, cursor, lock, keys, helper lanes and v2 helper protocol**.
   Gate: unit tests, `input_acceptance` example, helper tests on Windows, manual
   UAC rows of `scripts/test/windows-helper-acceptance.md`.
5. **VideoSource, FFI registry, C exports, JNI, view leaf, sync hot ops** (old
   frame exports still present). Gate: FFI tests, `cargo ndk -t arm64-v8a build
   -p pb-mapper-ffi`.
6. **Native consumers and Dart pipeline** (Kotlin feeder + JNI natives, Windows
   pull path, `DesktopApi`/`DesktopStore` rewrite, JPEG isolate; then delete the
   old frame exports, the Dart frame isolate and the temporary adapter). Gate:
   `flutter analyze`, `flutter test`, Gradle unit tests, Windows release build,
   APK build, on-device smoke.
7. **Lifecycle, quality, overlay stats, thumbnails, D14 polling**. Gate: Dart
   tests; manual background/resume on the phone.
8. **Docs and acceptance**: `docs/remote-desktop.md` performance model (receipt
   credit, QoS table, GOP, pause, metrics definitions), acceptance procedure
   (Android steps, schema 2), `CLAUDE.md` FFI exceptions list (view leaf, video
   exports, JNI), validation records; run §7.5. Merge-block statements change
   only with real-device evidence and the user's acceptance (D28).
