# Remote desktop performance architecture: robustness-first design

Author angle: robustness and security first. Every mechanism below starts from a
failure mode, and every state machine has explicit states and transitions so a
test can drive it. Requirements: BRIEF P0-1..P1-4. Decisions applied: D1-D14,
D17, D23, D24. Code citations are to the repo at `a41eaa7` (paths relative to the
worktree root; `D/` = `crates/pb-mapper-desktop/src/`, `F/` =
`ui/native/pb_mapper_ffi/src/`, `U/` = `ui/lib/src/`).

## 0. Summary and where this design refines a decision

The design has five rules.

1. **Three separate acknowledgements:** receipt, presentation and probe.
   - A receipt ack (`Received{seq}`) carries only a transport sequence number.
     It returns send credit and nothing else.
   - A presentation receipt (`Presented{generation, frame_id}`) carries only
     input-authorisation data. It returns no credit.
   - A probe echo (`ProbeEcho{nonce}`) is the only delay signal.
   - Because the three use different types, the authorisation ledger has no
     method that accepts a `seq`. That is a structural guarantee, not a
     convention (D4).
2. **One overflow point per side.** On the host, a 4-unit outgoing queue is
   guarded by a submit gate, so capture skips a tick instead of discarding and
   forcing an IDR. On the viewer, one `VideoSink` drops to the next IDR when
   its head has waited more than 200 ms. The native decoders never overflow:
   they stop pulling.
3. **One desktop-bound thread per process.** Capture and injection share it.
   A strict priority lane drains all queued input before every capture step.
   The single generation authority and the helper request/response discipline
   stay intact (D10, see 2.8).
4. **Only viewer-local presentation and native code see frames.** H.264 units go
   from Rust straight to the Kotlin feeder thread (JNI) or the Windows decoder
   thread (C exports). Dart gets status, geometry, stats and the latest cursor.
   Rust validates presentation for input in one place, `VideoSink::presented`,
   for every platform (D9).
5. **Every recovery path is rate-limited and independent of presentation acks.**
   Keyframe requests use limiters on both sides with backoff. Pause and resume
   are explicit. The viewer reconnects only on transport failure or when no
   video has arrived for 5 s. It never reconnects because a decoder is slow
   (D7, D8, D13).

Refinements (each one says why):

- **D1:** the viewer also sends a capability list in `Authenticate`. With
  `deny_unknown_fields` in both directions, a v2.x host may send a new field or
  message only if the viewer said it understands it. A host-only list protects
  just one direction. Capability values are open strings, so a new value never
  fails parsing.
- **D4:** receipts are keyed by a per-session transport `seq`, not by
  `(generation, frame_id)`. `frame_id` remains the input-authorisation handle
  only.
- **D5:** capture never pauses for credit. But when 3 units are already
  buffered (outgoing plus encoder in flight), it skips that tick. This is
  RustDesk's model (BRIEF: the capture thread "waits only until the connection
  task takes the frame") and the only alternative to discarding plus forcing an
  IDR under congestion. The old pause waited a full phone presentation round
  trip. This skip waits only for transport receipt.
- **D6:** the encoder runs at a fixed nominal rate equal to the session's
  `limits.max_fps`. Pacing skips capture ticks. The controller works in bits
  per frame. A one-way latch switches compensation off if the encoder turns
  out to budget from timestamps (2.3).
- **D10:** a priority lane on the existing desktop-bound thread, not a new
  injection thread. A dedicated thread is the documented escalation if the
  measured `input_wait` budget is missed (risk R1).
- **D13:** explicit `Pause` and `Resume` messages. `Resume` itself makes the
  host emit an IDR, so no separate keyframe request can race it.

---

## 1. Wire protocol v2 (ALPN `pb-mapper/desktop/2`)

### 1.1 Stack and version gate

The stack is unchanged below TLS: a pb-mapper TCP service (`codec: true`) runs
over a relay (TCP or QUIC) or a direct QUIC path, then loopback TCP with
`TCP_NODELAY`, then TLS 1.3 (`D/security.rs`).

- `security::ALPN` changes from `b"pb-mapper/desktop/1"` to
  `b"pb-mapper/desktop/2"`.
- The server and client each offer only v2.
- A v1 peer fails the handshake with `rustls::Error::AlertReceived(NoApplicationProtocol)`
  on the client, or `Error::NoApplicationProtocol` on the server. The viewer
  maps this to `ConnectError::VersionMismatch`. The FFI code is
  `VERSION_MISMATCH`, and the message is "The other computer runs a different
  pb-mapper version. Update pb-mapper on both computers."
- SNI stays `desktop.pb-mapper.invalid`. Early data stays off.

### 1.2 Framing (one flush per message)

- **Viewer to host:** one record `u32 BE len | JSON ViewerMessage`, with
  `len <= MAX_HEADER = 8192`.
- **Host to viewer:** always two records, `u32 BE len | JSON HostMessage`
  (≤ 8192) then `u32 BE len | payload` (≤ `MAX_PAYLOAD = 4 MiB`). The payload
  is non-empty if and only if the message is `Video`; anything else is a
  protocol error.
- `wire::write_message(w, header, payload)` writes both records into one
  `BufWriter` and flushes once. Today `D/wire.rs:65-76` flushes twice per
  message. This removes one TLS record group per frame;
  `transport_probe --case tls-split/tls-combined` already measures the
  difference.
- **Write timeout** = `IO_TIMEOUT (3 s) + bytes * 8 / WRITE_FLOOR_BPS
  (1 Mbit/s)`. A 400 KB IDR on a 1 Mbit/s link must not kill the session.
- **Viewer read timeout:** 4 s for a header (the host sends something at least
  every 1 s), and the same floor formula for the payload.

### 1.3 Invitation `pbd1` (D3)

Text form: `"pbd1." + base64url_no_pad(payload)`.

| Offset | Size | Field | Rule |
| --- | --- | --- | --- |
| 0 | 1 | `version` | `0x02` |
| 1 | 1 | `flags` | `0x00`. Reserved; non-zero → `UnsupportedVersion` |
| 2 | 1 | `n` = service length | 9..=128 |
| 3 | n | `service` | ASCII, `valid_service` (`D/lib.rs:49-56`) |
| 3+n | 32 | `fingerprint` | SHA-256 of the host certificate DER |
| 35+n | 32 | `token` | 32 random bytes (the bearer capability) |
| 67+n | 4 | `check` | `SHA-256(bytes[0..67+n])[0..4]` (typo detection; not a secret) |

The total is `71 + n` bytes. For `desktop:my-computer` that is 90 bytes, which
is 120 base64url characters, so the full text is 125 characters (QR about
v6-M). The current JSON invite is about 650 characters (v20-M).

```rust
// D/invite.rs (new). Replaces Invitation in D/lib.rs:89-123.
#[derive(Clone)]
pub struct Invitation {
    pub service: String,
    pub fingerprint: [u8; 32],
    token: [u8; 32],              // never Debug-printed, never logged
}
impl Invitation {
    pub fn encode(&self) -> String;                         // always pbd1
    pub fn decode(text: &str) -> Result<Self, InvitationError>;
    pub fn short_fingerprint(&self) -> String;              // "AB12-CD34-EF56-7890" (first 8 bytes)
    pub(crate) fn token_text(&self) -> String;              // base64url, 43 chars, only for Authenticate
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationError { Empty, TooLarge, SetupString, Malformed, Checksum, UnsupportedVersion, InvalidService }
```

`decode` works through these steps in order.

1. Trim ASCII whitespace. Empty input → `Empty`. Input over 8192 bytes →
   `TooLarge`.
2. If the text starts with `pbd1.`, decode it as `pbd1`:
   - the text must be ≤ 512 characters;
   - base64url must be strict: no padding, no other alphabet;
   - the length must be exactly `71 + n`;
   - check `version`, then `flags`, then `check`, then `service`.
3. If the text starts with `{`, decode it as **v1 JSON**
   `{version:1, service, certificate, token}` with `deny_unknown_fields`:
   - `fingerprint = SHA-256(b64url_decode(certificate))`;
   - the token must decode to exactly 32 bytes.

   Paste and scan accept v1 JSON. `encode` never emits it.
4. If the text starts with `pbmt1_`, or decodes from standard or url base64 to a
   JSON object with the key `serverAddress` or `msgHeaderKey`, return
   `SetupString`. That is a pb-mapper setup string, not an invite.
5. Anything else → `Malformed`.

**Pinning.** `security::client(fingerprint: &[u8; 32])` replaces
`client(certificate)` (`D/security.rs:109-137`). The new `Pinned::verify_server_cert`:

1. requires `chain.is_empty()`;
2. compares `ring::digest(SHA256, cert)` to `fingerprint` with
   `subtle::ConstantTimeEq`. On mismatch it returns
   `rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(IdentityChanged))))`;
3. builds a `WebPkiServerVerifier` whose only root is the presented
   certificate, and runs normal verification (validity and name).

The viewer finds `IdentityChanged` by **downcast**, never by string match. That
lets `U/state/desktop_store.dart:432` stop matching substrings.

**Host side.** `HostIdentity::invitation(service)` computes the fingerprint from
its DER. The persisted `desktop-trust.dpapi` stores `HostIdentity`
(certificate, key and token), not the invite, so no stored data changes. The
invite is simply emitted as `pbd1` from the next load on (D3, "re-encoded on
load").

### 1.4 Viewer → host messages

```rust
// D/protocol.rs (new; replaces wire::Command, D/wire.rs:9-37)
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ViewerMessage {
    /// First and only first message. Host answers Accepted or Rejected.
    Authenticate {
        token: String,                 // 43-char base64url; compared as SHA-256 digests in constant time
        codecs: Vec<VideoCodec>,       // 1..=2 entries
        limits: VideoLimits,
        quality: QualityPreset,
        capabilities: Vec<String>,     // v2.0 viewer sends []
    },
    Control { enabled: bool },         // ordered input lane
    Release,                           // ordered input lane
    Input { event: InputEvent },       // ordered input lane
    Lock,                              // ordered input lane; control-gated (D17)
    Received { seq: u64 },             // cumulative receipt ack (credit only)
    ProbeEcho { nonce: u64 },          // echo of HostMessage::Probe
    Presented { generation: u64, frame_id: u64 }, // input authorisation only
    Keyframe { reason: KeyframeReason },
    FpsCap { fps: u32 },               // 1..=120; runtime limit separate from limits
    Quality { quality: QualityPreset },// live preset change (D12)
    Pause,                             // D13
    Resume,                            // D13; host emits an IDR first
    Ping,                              // only when nothing else was sent for 1 s
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoLimits { pub max_width: u32, pub max_height: u32, pub max_fps: u32 }
// validate(): width 2..=3840, height 2..=2160, fps 30..=120. Default 1920x1080@60.
// Replaces VideoPreferences (D/lib.rs:178-208); bitrate leaves the wire (presets own it).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset { Quality, #[default] Balanced, Smooth }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason { DecoderError, Gap, Backlog, Stall, User }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvent { pub generation: u64, pub frame_id: u64, pub sequence: u64, pub input: Input }
// frame_id is stamped by the Rust viewer from its latest validated presented
// frame (3.5); Dart no longer supplies it.
```

`Input` keeps its shape (`D/lib.rs:246-254`, `tag = "kind"`). `Key` is
extended for D17 (snake_case serde names):

| Group | Variants (serde name) |
| --- | --- |
| existing | `shift control alt meta enter escape tab backspace delete left right up down home end page_up page_down` |
| letters | `a` … `z` (replaces the A/C/V/X/Z subset) |
| digits | `digit0` … `digit9` |
| function | `f1` … `f12` |
| other | `insert`, `space` |

The Windows `key_code` (`D/windows.rs:538-563`) maps letters and digits to
`enigo::Key::Unicode(c)`, `F1..F12` to `enigo::Key::F1..F12`, `Insert` to
`enigo::Key::Insert` and `Space` to `enigo::Key::Space`. Win is `meta`. SAS
(Ctrl+Alt+Del) is still not representable.

### 1.5 Host → viewer messages

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HostMessage {
    Accepted { codec: VideoCodec, limits: VideoLimits, quality: QualityPreset, capabilities: Vec<String> },
    Rejected { reason: RejectReason },
    State {
        state: SessionStateKind,
        #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<StateReason>,
        message: String,                  // human text; UI keys off `reason`
        control_allowed: bool,
        controlling: bool,
        generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")] display: Option<Display>,
        desktop: DesktopKind,
        #[serde(default, skip_serializing_if = "Option::is_none")] video: Option<VideoSize>,
    },
    Video {                               // followed by a non-empty payload
        seq: u64,                         // 1, 2, 3 … per session, every Video message
        generation: u64,
        frame_id: u64,
        keyframe: bool,
        duplicate: bool,                  // static-desktop liveness re-encode
        width: u32, height: u32,
        pts_us: u64,
        host_us: u32,                     // capture-ready → write start (host pipeline + credit wait)
    },
    Cursor { generation: u64, x: i32, y: i32, visible: bool },
    Probe { nonce: u64 },
    Stats {
        capture_fps: f32, pace_fps: u32, target_fps: u32, fps_cap: u32,
        bitrate: u32,          // expected bandwidth = bits_per_frame * pace_fps
        encoder_bitrate: u32,  // value given to the encoder
        sent_bitrate: u32,     // measured over the last second
        rtt_ms: u32, baseline_rtt_ms: u32, queue_delay_ms: u32,
        frame_ack_ms: u32,     // host-measured ready → Presented (diagnostic only)
        dropped_frames: u64, skipped_ticks: u64, keyframes: u64,
        frames: u64, bytes: u64,
        adaptation: Adaptation,
    },
    Notice { code: NoticeCode, message: String },
    Stopped { reason: StopReason, message: String },
}

#[serde(rename_all = "snake_case")] pub enum RejectReason { Denied, Busy, InvalidRequest }
#[serde(rename_all = "snake_case")] pub enum SessionStateKind { Connecting, Streaming, Paused, CaptureUnavailable }
#[serde(rename_all = "snake_case")] pub enum StateReason { SecureDesktop, HelperUnavailable, CaptureFailed, InputFailed, ControlDenied, InputCongestion, StalePresentation }
#[serde(rename_all = "snake_case")] pub enum DesktopKind { Ordinary, Secure }
#[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)] pub struct VideoSize { pub width: u32, pub height: u32 }
#[serde(rename_all = "snake_case")] pub enum Adaptation { Startup, Hold, Increase, Decrease, Unresponsive, Paused }
#[serde(rename_all = "snake_case")] pub enum NoticeCode { Locked, LockFailed }
#[serde(rename_all = "snake_case")] pub enum StopReason { HostStopped, Revoked, CaptureFailed, ProtocolError, Timeout }
```

All enum variants are inline structs because internally tagged newtype
variants combined with `deny_unknown_fields` inner structs are fragile in serde.

### 1.6 Ordering and validity rules (strict; any violation ends the session)

**Viewer side, checking the host:**

1. The first message is `Accepted` or `Rejected`. The first `State` follows
   `Accepted` before any `Video`.
2. `Video.seq == last_seq + 1`.
3. `Video.generation` equals the latest `State.generation`.
4. `width ≤ limits.max_width` and `height ≤ limits.max_height`.
5. `display` is 1..=32768 in each dimension (kept from `D/viewer.rs:239-251`).
6. A payload appears only on `Video`.
7. A `Cursor` whose generation is not current is ignored, not treated as an
   error, because a cursor may legitimately race a `State`.

**Host side, checking the viewer:**

1. `Authenticate` is only the first message.
2. `Received.seq ≤ last_sent_seq`. An ack for an unsent frame is a protocol
   error. An ack with `seq ≤ acked` is a duplicate and is ignored.
3. `ProbeEcho.nonce` other than the outstanding one is ignored.
4. `FpsCap.fps` is in 1..=120.
5. `Input`, `Lock`, `Control{true}`, `Pause` and `Resume` all follow
   `Accepted`.

### 1.7 Capabilities and versioning

- Capability values are `[a-z0-9_]{1,32}`, with at most 16 entries. Unknown
  values are ignored.
- The v2.0 host advertises `lock` (Windows hosts) and `secure_desktop` (when the
  privileged helper backend is active). The v2.0 viewer advertises none.
- **Rule:** a v2.x peer sends a new message, field or enum value only if the
  other side listed the matching capability. A future display switch would be
  `displays`; a future explicit resolution choice would be `set_limits`.
- Anything outside these rules is a v3, with a new ALPN.

### 1.8 Removed from the wire

- `wire::Command` and its `Authenticate.presentation_feedback`.
- `Command::Probe` (renamed `ProbeEcho`) and `SessionStatus.roundtrip_probe`.
- `SessionStatus` as the per-message record (`D/lib.rs:125-165`). The
  viewer-local fields `path`, `direct_stage`, `direct_failure` and
  `receive_fps` move to the FFI-only `ViewerStatus` (4.2).
- `frame_ack_ms`, which moves to `Stats`.
- `VideoPreferences.bitrate`.
- The JSON invite on emit.
- Cursor stamping into video statuses (`D/host.rs:227-235`, `D/media.rs:41-44,80`).

---

## 2. Host

### 2.1 Tasks, threads and ownership

| # | Name | Runs on | Owns | Shares (lock or atomic) |
| --- | --- | --- | --- | --- |
| A | accept | tokio (`D/host.rs:139`) | listener, `JoinSet` ≤ 8, `Semaphore(1)` | — |
| B1 | `session-recv` | tokio task | TLS read half, `ViewerMessage` decode | `credit`, `qos`, `auth`, `idr`, `lane` (push), `pointer_input_at` |
| B2 | `session-send` | tokio task | TLS write half, `CursorGate`, probe/stats/tick timers | `outgoing` (pop), `credit`, `qos`, `state` watch, `cursor` watch |
| C | `desktop-worker` | `spawn_blocking` std thread | `Box<dyn Backend>` (encoder, Enigo, `DesktopBinding`), `Engine`, `Waiter`, pacer | `lane` (pop), `outgoing` (push), `auth` (write), `pace`/`encoder_bps` atomics (read), `state`/`cursor` watches (send) |
| D | SDK registration | tokio | tunnel | — |
| E | helper pipe | current-thread runtime inside C (`D/helper/client.rs:38`) | named pipe | — |

The receive and send halves become **two tasks** under a session supervisor:
`select!{ cancel, recv, send }`, then `cancel`, then `worker.stop()`. A 3 s
blocked write then never delays processing of `Received` or `Presented`.

```rust
// D/session.rs (new) — one per accepted viewer
pub(crate) struct SessionShared {
    epoch: Instant,                        // session start (D24 since_ms)
    cancel: CancellationToken,
    policy: Arc<Policy>,                   // D/worker.rs:17-37, unchanged
    credit: parking_lot::Mutex<CreditLedger>, credit_changed: tokio::sync::Notify,
    qos: parking_lot::Mutex<QosController>,
    pace_fps: AtomicU32, encoder_bps: AtomicU32,   // published by B2 every tick, read by C
    idr: parking_lot::Mutex<IdrPolicy>,
    auth: parking_lot::Mutex<AuthLedger>,
    lane: InputLane,
    outgoing: Outgoing,
    pointer_input_at_us: AtomicU64,        // since epoch; for CursorGate
    paused: AtomicBool,
}
```

**Lock discipline (to avoid deadlock):** never hold two of
`credit`/`qos`/`idr`/`auth` at once. Never hold any of them across an
`.await` or a backend call. Each critical section is O(1) or O(ledger length
≤ 256).

### 2.2 Host session state machine

```
Handshake --token ok, slot free--> Starting --first video written--> Streaming
Handshake --bad token--> [Rejected{denied}] Closed
Handshake --slot busy--> [Rejected{busy}] Closed          (only after the token verified: no oracle)
Handshake --bad/limits/codecs--> [Rejected{invalid_request}] Closed
Streaming <--Pause/Resume--> Paused                       (Paused: pace 0, no cursor, probes and State continue)
Streaming <--context error / desktop ok--> CaptureUnavailable
any --policy revoke--> same phase, controlling=false
any --host stop / trust revoke--> [Stopped{host_stopped|revoked}] Closed
any --protocol violation--> [Stopped{protocol_error}] Closed
any --read timeout 3 s / write timeout--> Closed (no message; transport is gone)
```

The 3 s handshake bound (`D/host.rs:170`) and the constant-time token check
(`:160-162`) stay. `Rejected` is written with a 1 s timeout before closing.

There is **no presentation stall teardown.** `feedback.stalled()` and the
"Renderer stopped acknowledging presented frames" stop (`D/host.rs:202-212`)
are deleted. A frozen renderer is a viewer-local fact.

### 2.3 Capture pacing without encoder rebuilds (D6)

**Pick.** The encoder is created once per desktop context at nominal
`fps = limits.max_fps` (`MF_MT_FRAME_RATE`, `D/video/windows.rs:608-609`). The
pacer lives in the worker (C), not the encoder:

```
period = 1 s / pace_fps                       // pace_fps from SessionShared.pace_fps
on each tick deadline `next_tick`:
    if outgoing.len() + backend.pending_video() < SUBMIT_GATE (3):
        backend.capture_video(ctx, codec, keyframe = engine.keyframe || idr.take_due(now), bitrate = encoder_bps)
    else:
        skipped_ticks += 1                    // transport backpressure, never presentation
    next_tick += period
    if next_tick < now - period { next_tick = now + period }   // no catch-up burst, no drift
on pace change: next_tick = last_tick + 1 s / new_pace
between ticks: if backend.pending_video() > 0 -> drain_video() every 2 ms (D/worker.rs:269)
pace_fps == 0 (Paused): no capture, no drain once pending == 0
```

**Encoder changes (`D/video/windows.rs`):**

- Rename `next_frame` (`:333-426`) to `submit(force_keyframe)`. Remove its
  internal `next_capture` pacing (`:339-341, 414-419`), so the worker is the
  only pacer and a 0.5 ms early tick can never be skipped by a second clock.
- Keep MAX_IN_FLIGHT (3), allocator-empty skip, and the 250 ms static-desktop
  duplicate (`:376-384`). The 4 fps duplicates keep a presented frame fresh for
  input on a static screen.
- Sample time stays the real capture time. Sample duration becomes the real
  interval since the previous submission, clamped to
  `[1/nominal, 1 s]` (a VFR timestamp).
- Add `pub fn pending_len(&self) -> usize`.
- `Backend::pending_video(&self) -> usize` (default 0). The ordinary backend
  returns `encoder.pending_len()`. `RemoteBackend` returns the `pending` field
  of the last helper response (2.11).

**Bitrate model (verifies CBR budgets).** Hardware MF encoders budget per frame
from the nominal `MF_MT_FRAME_RATE`. A tick-skipping pace below nominal then
produces `encoder_bitrate × pace / nominal`. The controller therefore works in
**bits per frame** (`bpf`):

```
RateModel::Nominal   (default): encoder_bps = clamp(bpf * nominal_fps, 256_000, 40_000_000)
RateModel::Timestamp (latched): encoder_bps = clamp(bpf * pace_fps,   256_000, 40_000_000)
expected_bps = bpf * pace_fps
```

- Under congestion an fps cut lowers bandwidth at once, as in RustDesk.
- A lower preset fps keeps per-frame quality.
- **Latch:** every `RATIO_INTERVAL` (3 s) window that is dynamic and has
  `pace ≤ 0.75 × nominal` compares produced non-IDR bits to
  `1.3 × expected_bps × 3 s`. If production exceeds that in 2 consecutive
  windows, the encoder budgets from timestamps. Set
  `rate_model = Timestamp` for the session and trace
  `desktop_rate_model_latched`. This is one-way, so it cannot oscillate.
- **Verification:** `video_bench` gains `--pace 60,30,15,5` (it switches pace
  every 10 s at nominal 60 and prints `pace, target_mbps, produced_mbps` per
  step, with and without compensation).
- **Acceptance on each GPU vendor available:** produced bitrate is within ±25%
  of `expected_bps` at every step under the chosen model. Record the result in
  `docs/remote-desktop.md`.

**Infinite GOP (D7).** `Codec::activate` (`:607`) stops setting
`GOPSize = fps`. `set_infinite_gop()` tries `CODECAPI_AVEncMPVGOPSize`
`u32::MAX`, then `0x7FFF_FFFF`, then `65_535`, then `3_600`, and keeps the first
that `SetValue` accepts and `GetValue` returns unchanged. It stores the result
in `Statistics.gop` and traces it. `0` is never used, because it means "encoder
default".

`video_bench 120 60` must report `keyframes == 1 + forced` (forced at 1.5 s,
`examples/video_bench.rs:134-138`). If the vendor clamps, the clamp is recorded
and the risk R3 mitigation applies.

**SPS/PPS** stay in every IDR through `annex_b` (`:756-786`), under the existing
test `parameter_sets_survive_partial_updates_and_emulation_prevention`.

### 2.4 Receipt-ack credit window (P0-1)

```rust
// D/credit.rs (new; replaces congestion.rs admission)
pub(crate) struct CreditLedger {
    last_sent: u64, acked: u64,
    in_flight: VecDeque<Sent>,            // seq order
    in_flight_bytes: usize,
    idr_bytes_ewma: f64,                  // α = 0.5 over emitted IDRs
    sent_bps: Ewma,                       // 1 s window
}
struct Sent { seq: u64, bytes: u32, sent_at: Instant, keyframe: bool }

pub(crate) struct Window { bytes: usize, reserve: usize, frames: usize }

fn window(rate_bps: u32 /* max(expected_bps, sent_bps) * 1.25 */, baseline_rtt: Duration, pace: u32, bpf: f64) -> Window {
    let t = (baseline_rtt + WINDOW_MARGIN).clamp(WINDOW_TIME_MIN, WINDOW_TIME_MAX);
    let reserve = clamp(1.25 * idr_bytes_ewma_or(16.0 * bpf / 8.0), IDR_RESERVE_MIN, IDR_RESERVE_MAX);
    Window {
        bytes: max(WINDOW_BYTES_MIN, rate_bps as f64 * t.as_secs_f64() / 8.0) as usize + reserve,
        reserve,
        frames: clamp(ceil(pace as f64 * t.as_secs_f64()) as usize + 2, WINDOW_FRAMES_MIN, WINDOW_FRAMES_MAX),
    }
}
```

**Admission** (B2, for the head of `outgoing`):

```
if in_flight.is_empty()                                     -> admit (any size ≤ MAX_PAYLOAD; liveness guarantee)
else if keyframe && in_flight_bytes + size ≤ w.bytes
                 && in_flight.len() < w.frames + 1          -> admit (an IDR may use the reserve)
else if !keyframe && in_flight_bytes + size ≤ w.bytes - w.reserve
                  && in_flight.len() < w.frames             -> admit
else                                                        -> wait for credit_changed / 20 ms tick
```

An over-window IDR waits only until receipts free `size` bytes, which is about
one RTT. It never waits for all in-flight frames to be presented. This replaces
`D/congestion.rs:196-205`.

**Receipt** (B1): `on_receipt(seq)`.

- `seq > last_sent` → `ProtocolError`.
- `seq ≤ acked` → ignore.
- Otherwise pop entries up to `seq`, subtract their bytes, update the IDR EWMA
  input, and `credit_changed.notify_one()`.

Receipts **never** touch `AuthLedger`, `Engine`, or `backend.presented`.

| Constant | Value | Why |
| --- | --- | --- |
| `WINDOW_MARGIN` | 100 ms | Absorbs ack-path scheduling jitter (Android network task); bounds the standing queue to about 100 ms |
| `WINDOW_TIME_MIN` / `MAX` | 150 ms / 1000 ms | Floor for LAN; cap for satellite-class RTT |
| `WINDOW_BYTES_MIN` | 64 KiB | One 1080p P-frame burst |
| `WINDOW_FRAMES_MIN` / `MAX` | 4 / 120 | 120 = RustDesk's client queue bound |
| `IDR_RESERVE_MIN` / `MAX` | 64 KiB / 1 MiB | Headroom for one IDR (P0-3) |
| `SUBMIT_GATE` | 3 | Outgoing plus encoder in flight; above it, capture skips the tick |
| `OUTGOING_CAPACITY` | 4 units / 8 MiB | ≥ gate plus encoder MAX_IN_FLIGHT; overflow is a bug path → resync |
| `OUTGOING_MAX_AGE` | 1 s | The head waited 1 s for credit: discard all queued, `idr.request(HostResync)` |

There is no 256 KiB discard-and-halve rule (`D/congestion.rs:191-195` is
deleted), and no 250 ms ready-age discard.

### 2.5 Delay probe and RTT baseline (P1-2)

Host-initiated, as in RustDesk `connection.rs:1082-1103`:

- B2 writes `Probe{nonce}` every `PROBE_INTERVAL = 1 s` when no probe is
  outstanding, plus once immediately after `Accepted` (a clean baseline sample
  before the first IDR). The nonce is `1, 2, 3 …`.
- The probe goes in stream order between video messages, so it measures
  downstream queueing.
- B1 handles `ProbeEcho{nonce}`. A sample is taken only if
  `nonce == outstanding`: `rtt = max(now - sent_at, DELAY_FLOOR = 10 ms)` →
  `qos.on_rtt(rtt)`.

`RttCalculator` follows RustDesk `video_qos.rs:540-595`:

- `min_hist` (historical minimum);
- a window of the last `RTT_WINDOW = 60` samples;
- `baseline = 0.5*min_hist + 0.5*window_min` once there are 60 samples;
  `min_hist` once there are ≥ `RTT_MIN_SAMPLES = 10`;
- **deviation:** before 10 samples, `baseline = min(samples)`, so a cellular
  link's first 10 s are not read as 80 ms of queueing.

`queue_delay = avg(last DELAY_HISTORY = 3 samples) - baseline` (saturating).
With no samples it is 150 ms, as in RustDesk.

### 2.6 QoS controller: fps and bits per frame (P0-2, P0-4, D12)

```rust
// D/qos.rs (new; replaces congestion.rs observe_delay/budget/quality ladder)
pub(crate) struct QosController {
    preset: QualityPreset, limits: VideoLimits, codec: VideoCodec, pixel_scale: f64,
    fps: u32, bpf: f64,
    quick: u8, good_streak: u8, last_delay: Option<u32>,
    rtt: RttCalculator, history: VecDeque<u32>,
    probe: ProbeState,                     // next_nonce, outstanding: Option<(u64, Instant)>, last_rtt
    viewer_cap: Option<u32>, paused: bool, startup_until: Instant, unresponsive: bool,
    window: RatioWindow,                   // started, max_delay, unique_frames, produced_bits_non_idr
    rate_model: RateModel, overshoot_windows: u8,
}
```

**Presets.** Bandwidth numbers are at 1080p and at the preset ceiling.
`pixel_scale = clamp(encoded_w*encoded_h / 2_073_600, 0.25, 4.0)` scales every
bitrate. `bpf = bandwidth / ceiling`.

| Preset | ceiling fps | normal_fps | min_fps | start bandwidth | max bandwidth | floor bandwidth |
| --- | --- | --- | --- | --- | --- | --- |
| Quality | 30 | 24 | 8 | 6.0 Mbit/s | 10.0 Mbit/s | 1.5 Mbit/s |
| **Balanced (default)** | 60 | 40 | 10 | 4.0 Mbit/s | 8.0 Mbit/s | 1.0 Mbit/s |
| Smooth | `min(limits.max_fps, 90)` | 50 | 12 | 3.0 Mbit/s @60 | 6.0 Mbit/s @60 | 0.75 Mbit/s @60 |

- Every ceiling is further capped by `limits.max_fps`.
- `min_fps` and `normal_fps` mirror RustDesk's per-mode table
  (Best 8/16, Balanced 10/20, Low 12/24), raised for the 60 fps ceilings the
  BRIEF wants.
- The start fps is `normal_fps`.
- Resolution never changes from QoS (D5); the ladder is deleted.

**fps on every probe sample** (exactly RustDesk `user_network_delay`,
`video_qos.rs:246-334`). `d` is the queue delay in ms.

```
d < 50:        quick += 1; step = if fps < normal {1} else {0}; if quick >= 3 { quick = 0; step = 5 }; fps = max(min, fps + step)
50 ≤ d < 100:  if last_delay.is_some_and(|l| d < l) && fps < normal { fps += 1 }
100 ≤ d < 150: fps = max(min, fps)
d ≥ 150:       div = ceil(fps / (d/150))
               150..200: fps = max(min, div); 200..300: fps = min(min, div)
               300..600: fps = 150*min/d;     ≥600: fps = min(150*min/d, div)
every 3 consecutive samples with d < 150: fps += 1
d > 50: quick = 0
fps = clamp(fps, 1, min(preset ceiling, viewer_cap, limits.max_fps))
```

**pace** (published to `SessionShared.pace_fps` every 250 ms tick and on every
change):

```
if paused                    -> 0
p = min(fps, preset ceiling, viewer_cap.unwrap_or(∞), limits.max_fps)
if now < startup_until       -> p = min(p, STARTUP_FPS_CAP = 15)   // first 1 s after start/Resume (RustDesk INIT_FPS)
if unresponsive              -> p = min(p, UNRESPONSIVE_FPS = 2)
if codec == Jpeg             -> p = min(p, JPEG_MAX_FPS = 8)
max(p, 1)
```

**Unresponsive** (B2, 250 ms tick):

- set when the probe has been outstanding for more than 2 s, **or** the oldest
  unacked receipt is more than 2 s old while not paused;
- while it is set, feed `elapsed` as a delay sample once per second (RustDesk
  `:336-344`);
- clear it on a valid `ProbeEcho` when `oldest_unacked_age < 1 s`.

The session does **not** close on unresponsiveness. The 3 s read timeout on
viewer `Ping`, and the write timeout, detect a dead transport.

**bits per frame every `RATIO_INTERVAL = 3 s`** (RustDesk `adjust_ratio`,
`:416-508`). `max_delay` is the worst `d` seen in the window. `dynamic` means
`unique_frames ≥ 6` (`DYNAMIC_UNIQUE_PER_SEC = 2` × 3 s).

| max_delay | factor |
| --- | --- |
| < 50 | ×1.15 if dynamic |
| < 100 | ×1.10 if dynamic |
| < 150 | ×1.05 if dynamic |
| < 200 | ×0.95 |
| < 300 | ×0.90 |
| < 500 | ×0.85 |
| ≥ 500 | ×0.80 |

Increases are capped at `+150 kbit/s` of bandwidth:
`bpf_new = min(bpf*f, bpf + 150_000/pace)`. Then clamp to the preset floor and
max. The first sample adjusts immediately, as in RustDesk.

**Live preset change** (`ViewerMessage::Quality`, D12):

- `preset = q`; `bpf = start(q)`; `fps = min(fps, ceiling(q))`, raised to
  `normal(q)` if lower; reset `quick` and `good_streak`.
- No IDR, no encoder rebuild, no resolution change.
- The host replies `State` (no change) and the next `Stats` shows the new
  targets.

**Viewer cap** (`FpsCap{fps}`): `viewer_cap = Some(fps)`, cleared at
`fps ≥ limits.max_fps`. It is a separate runtime limit, never written into
`limits` (D8).

**Debug trace** replaces `desktop_feedback_interval` (`D/congestion.rs:423-458`;
the BRIEF instrumentation names fields `action, bitrate, delay_ms, baseline_ms,
waiting, outstanding_frames`). Target `pb_mapper_desktop::qos`, event
`desktop_qos_interval`, once per second. Fields: `action` (Adaptation),
`pace_fps`, `target_fps`, `fps_cap`, `bitrate`, `encoder_bitrate`,
`sent_bitrate`, `rtt_ms`, `baseline_ms`, `delay_ms`, `outstanding_frames`,
`outstanding_bytes`, `window_bytes`, `reserve_bytes`, `skipped_ticks`,
`waiting` (= the head waits for credit), `unresponsive`, `paused`,
`rate_model`, `input_wait_p99_us`.

### 2.7 IDR policy and headroom (P0-3, D7)

```rust
// D/idr.rs (new)
pub(crate) struct IdrPolicy { pending: Option<(IdrReason, bool /*immediate*/)>, last_emitted: Option<Instant>, requested: u64, emitted: u64 }
pub(crate) enum IdrReason { NewEncoder, Generation, Resume, Viewer(KeyframeReason), HostResync, EncoderLate }
impl IdrPolicy {
    fn force(&mut self, r: IdrReason);                  // immediate
    fn request(&mut self, r: IdrReason);                // deferred to cooldown, never dropped
    fn take_due(&mut self, now: Instant) -> bool;       // immediate || now - last_emitted ≥ HOST_IDR_MIN_INTERVAL
    fn on_emitted(&mut self, now: Instant);             // when the encoder output is a keyframe
}
```

| Trigger | Kind | Source |
| --- | --- | --- |
| New encoder (session, context/display change, helper desktop switch, encoder error) | inherent (encoder starts with IDR, `D/video/windows.rs:238`) | C |
| `Engine::invalidate` (control grant/revoke, capture recovered) | `force(Generation)` | C |
| `Resume` | `force(Resume)` | B1 |
| `Keyframe{reason}` | `request(Viewer(reason))` | B1 |
| Outgoing age > 1 s / overflow | `request(HostResync)` | B2 |
| Encoder late output > 250 ms / P after expired | inherent (encoder sets `force_keyframe`, `:294-301`) | C |
| **Never** | periodic GOP, fps change, bitrate change, preset change, congestion | — |

- `HOST_IDR_MIN_INTERVAL = 500 ms` applies to `request` only.
- Requests during the cooldown coalesce into one pending IDR emitted when the
  cooldown ends. Recovery therefore never depends on the viewer asking again,
  or on any presentation ack. That removes the `recovering` single-credit trap
  (`D/congestion.rs:234-249`).
- Headroom is the window reserve (2.4).
- `Stats.keyframes` counts emitted IDRs.

### 2.8 Input path (D10): priority lane on the desktop-bound thread

**Why not a new thread.** D10 requires a desktop binding on every injecting
thread, generation checks, and the helper's request/response discipline. A
second injecting thread needs:

- a second `DesktopBinding` per process, whose `BindingState.generation`
  diverges (`D/helper/worker.rs:91-123`), so a new shared generation authority
  is needed;
- a second pipe or pipe multiplexing through the service proxy
  (`service.rs:190-206`);
- a second `Events` hook.

Each of these is a new place where UAC transitions can race.

The real latency sources today were different:

- `receive` awaits each command's reply (`D/host.rs:187`, up to 2 s);
- `Presented` ACKs queue on the capture thread (`D/worker.rs:182-190`);
- Dart's frame isolate (`U/ffi/pb_mapper_service.dart:288-301`).

All three go away without a second thread. The remaining wait is bounded by one
**non-blocking** backend step. `acquire_next_frame(0)`, the video-processor
blit, async-MFT `ProcessInput` and `drain` never block on the encoder.

**Budget, measured by `input_wait`** (B1 enqueue → C execution start):

- p99 ≤ 4 ms ordinary, ≤ 12 ms helper (one pipe round trip, the IDR transfer
  included);
- the trace reports it, and the acceptance in 7.7 checks it;
- a miss triggers risk R1's escalation.

```rust
// D/lane.rs (new)
pub(crate) enum LaneCommand { Control { enabled: bool }, Release, Input(InputEvent), Lock, EndControl /* pause, overflow */ }
pub(crate) struct InputLane { queue: parking_lot::Mutex<VecDeque<(LaneCommand, Instant)>>, overflow: AtomicBool, wake: scheduler::Wake }
impl InputLane {
    fn push(&self, c: LaneCommand) -> bool;   // false when len == LANE_CAPACITY (64): sets overflow, wakes C
    fn drain(&self) -> VecDeque<(LaneCommand, Instant)>;
}
```

**B1 (receive) for `Control`, `Release`, `Input` and `Lock`:**

1. If `!policy.allowed()` and the command is `Input`, `Lock` or
   `Control{true}`, drop it and set `state.reason = ControlDenied`. Revocation
   is immediate at the edge.
2. For `Input` whose kind is `move`, `button` or `wheel`, store
   `pointer_input_at_us` (used by the cursor gate).
3. `lane.push(cmd)`. **B1 never awaits a reply.** Results surface in the next
   `State` (reason and message), which is the existing sticky-failure model
   (`D/worker.rs:349-357, 610-619`).

**C (worker) iteration:**

```
1. if cancel { break }                                         // never execute queued input after stop (D/worker.rs:153-157)
2. if policy.get() != seen_policy { engine.release_control(); seen_policy = policy.get() }
3. if lane.overflow.swap(false) { lane.drain(); engine.release_control(); reason = InputCongestion }
4. auth.take_stale_presentation() || !auth.fresh_for_controlling(now) -> backend.release()   // release_if_stale (D/worker.rs:402-414)
5. for (cmd, at) in lane.drain():                             // PRIORITY LANE: all queued input first
       if cancel { break 'outer }
       input_wait.record(now - at)
       execute(cmd)                                           // see below
6. lease: if controlling && auth.presented advanced && now - last_lease ≥ LEASE_RENEW_MIN (250 ms) { backend.presented(ctx) }
7. capture lane: at most ONE capture or drain step (2.3)
8. cursor: every 16 ms streaming / 200 ms unavailable / never while paused (2.9)
9. publish state and cursor watches on change; wait until min(next_tick, next_cursor, drain_poll) or lane.wake
```

**Execution** reuses `Engine` with `presented`/`recent` moved to `AuthLedger`:

- `Control{enabled}` → `Engine::control` (unchanged: lease, generation bump,
  `idr.force(Generation)`).
- `Release` → `backend.release()`.
- `EndControl` → `Engine::release_control()`.
- `Input(e)` → `Engine::input`, with every check unchanged
  (`D/worker.rs:586-621`):
  - strictly increasing `sequence`;
  - `geometry()`;
  - `allowed && controlling`;
  - `streaming`;
  - `e.generation == auth.generation`;
  - `auth.admits_input(e.frame_id, now)` (produced < 2 s ago);
  - then `auth.presented = (gen, frame_id, now)` and
    `backend.input_in_context`;
  - on failure, `release_control()` and a sticky `input_failed`.
- `Lock` → requires `allowed && controlling && auth.fresh(now)`. Then
  `backend.release()`, then `backend.lock(&ctx)`. The result goes to
  `Notice{locked | lock_failed, message}` through the state watch, sent by B2.

```rust
// D/auth.rs (new) — the only input-authorisation ledger; no method accepts a transport seq
pub(crate) struct AuthLedger {
    generation: u64,
    recent: VecDeque<(u64 /*frame_id*/, Instant /*ready*/)>,   // ≤ 256, written by C on every produced frame
    presented: Option<(u64, u64, Instant)>,
    stale_presentation: bool,
    frame_ack_ms: u32,
}
impl AuthLedger {
    fn on_produced(&mut self, frame_id: u64, at: Instant);
    fn on_invalidate(&mut self, generation: u64);            // clears recent, presented
    fn on_presented(&mut self, generation: u64, frame_id: u64, now: Instant) -> bool; // B1: gen current && frame in recent < 2 s && advances
    fn admits_input(&self, generation: u64, frame_id: u64, now: Instant) -> bool;
    fn fresh(&self, now: Instant) -> bool;                    // presented frame of current gen, produced < 2 s ago
    fn take_stale_presentation(&mut self) -> bool;
}
```

If `Presented` fails validation, B1 sets `stale_presentation`, and C releases
held input at the top of its next iteration. That is fail-closed, the same as
`D/worker.rs:452-457` today.

**`Backend` trait changes** (`D/lib.rs:301-375`):

- add `fn pending_video(&self) -> usize { 0 }`;
- add `fn lock(&mut self, expected: &DesktopContext) -> Result<()> { Err(error("lock not supported")) }`;
- `configure_video(limits: VideoLimits)`.

The ordinary `Desktop` implements `lock`: `available()` check, then
`LockWorkStation()`. `Win32_System_Shutdown` is added to the `windows` features.
The privileged worker implements `Request::Lock{generation}` the same way, on
its bound thread.

### 2.9 Cursor (D11)

- C samples the cursor every 16 ms while streaming and controlling or viewing,
  every 200 ms while `capture_unavailable`, and never while paused. It
  publishes `watch<Option<CursorUpdate{generation,x,y,visible}>>`.
- The cursor never enters `outgoing` or credit.

**B2 `CursorGate`:**

```
on change or 16 ms tick:
  if cursor.generation != last_sent_generation  -> send now (generation change is never suppressed)
  else if now - pointer_input_at < CURSOR_ECHO_SUPPRESS (300 ms) && controlling -> hold latest (pending)
  else if changed since last sent -> send
when the suppression window ends: send pending if it differs from last sent
```

There is exactly one viewer (`Semaphore(1)`), so "the operator who sent input"
is that viewer while it is controlling. The helper path's cursor is the last
pipe response (`D/helper/client.rs:119-124`), unchanged.

### 2.10 Host status (D24)

`Host::status()` (`D/host.rs:308-315`) returns:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStatus {
    pub state: HostState,                 // waiting | connecting | streaming | paused | capture_unavailable | stopped
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reason: Option<StateReason>,
    pub message: String,
    pub control_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub viewer: Option<HostViewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub display: Option<Display>,
    pub desktop: DesktopKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostViewer {
    pub since_ms: u64,                    // monotonic age of the session (SessionShared.epoch.elapsed())
    pub started_unix_ms: u64,             // wall clock, display only
    pub path: HostPath,                   // direct | relay | unknown
    pub controlling: bool,
    pub paused: bool,
    pub quality: QualityPreset,
}
```

- `path` comes from `registration.diagnostics().direct.active_direct > 0`. It
  is sampled when `status()` is called, never per frame.
- "Make view-only" goes through `set_control_allowed(false)`:
  - the Policy epoch bump makes C call `release_control()` at its next
    iteration (≤ one backend step);
  - B1 starts dropping `Input`, `Lock` and `Control{true}` at once (2.8 step 1);
  - B2 sends `State{control_allowed:false, controlling:false}`.
- The background service's `Snapshot.host` (`D/helper/background.rs:42-53`)
  carries `HostStatus` (background pipe v2, 2.11).

### 2.11 Helper upgrade (D2)

- `HELPER_PROTOCOL: u32 = 2` lives in `D/helper/mod.rs`.
- Pipes are renamed `\\.\pipe\pb-mapper.desktop-helper.v2` (and
  `.v2.worker.<n>`) and `\\.\pipe\pb-mapper.desktop-background.v2`. A v1
  service and a v2 UI therefore cannot half-talk.
- `installation.json` gets `schema: 2` and adds `"protocol": 2`
  (`install.rs:544-549`). `configuration()` accepts schema 1 (meaning protocol
  1) and schema 2.
- `Status` adds `protocol: u32` and `update_required: bool`
  (`installed && protocol != HELPER_PROTOCOL`). `ensure_ready()` fails first on
  `update_required`: "The Windows desktop service is from an older pb-mapper;
  update it".
- FFI `capabilities` passes these fields through. The UI offers "Update
  service" → `helper_action install`. `install_bundle` already upgrades in
  place (`install.rs:514-559`: previous config, new release directory, service
  reconfigure).
- The machine-DPAPI background grant `background-host.dpapi` is kept, so
  remembered viewers keep their pinned identity. The service emits a `pbd1`
  invite after the upgrade.
- **Helper IPC v2** (`D/helper/mod.rs:148-192`):
  - `ConfigureVideo { limits: VideoLimits }`;
  - `Lock { generation: u64 }` (new);
  - `Response` gains `pending: u32` (encoder in flight, for the submit gate).
  - Everything else is unchanged: one request, one response, 3 s timeout,
    `deny_unknown_fields` and sticky failure.
- **During a session:** reinstalling restarts the service, the pipe hits EOF,
  and `RemoteBackend` fails stickily (`client.rs:86-97`). The host reports
  `capture_unavailable{reason: helper_unavailable}`. Viewers wait. Background
  hosts drop their sessions, and viewers reconnect with backoff to the v2
  service.

### 2.12 Host code and tests deleted

**Code:**

- `D/congestion.rs` entirely, which includes the quality ladder,
  `MAX_FRAMES = media::MAX_QUEUED_FRAMES`, `MAX_INDEPENDENT_FRAME`,
  `ack_stall`, `stalled()`, the single-nonce probe and `request_keyframe`
  recovery.
- `D/media.rs`. Its role is split into `D/outgoing.rs` (host) and
  `D/viewer/sink.rs`.
- `Worker::sent` and the non-feedback bitrate heuristic (`D/worker.rs:294-304`).
  Every session now uses receipts.
- `paused = waiting` and `capture_video(!paused)` (`D/worker.rs:222-254`).
- `Command::Presented → feedback.presented` (`:186-188`).
- Worker-thread handling of `Presented`. `Presented` moves to B1 and `AuthLedger`.
- The startup probe block (`D/host.rs:193-200`).
- Cursor stamping (`:227-235`).
- `presentation_feedback` everywhere.
- `Encoder` internal pacing.
- `VideoPreferences` (replaced by `VideoLimits` plus `QualityPreset`).

**Tests:**

- all ten `congestion.rs` tests (`:523-857`);
- `media.rs` `overflow_discards_dependent_frames_until_idr` and
  `status_is_coalesced_without_reordering_or_invalidating_video`;
- `worker.rs` `presentation_backpressure_drains_encoder_without_new_capture`
  (`:897`), `unavailable_desktop_retires_old_ack_debt_and_resumes_control`
  (`:1021`) and `decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture`
  (`:1242`);
- `slow_link.rs` assertions `:283-289` and `:780-807`. That module is rewritten
  (7.2).

---

## 3. Viewer (Rust)

### 3.1 Tasks and shared state

```rust
// D/viewer/mod.rs
pub struct Viewer {
    shared: Arc<ViewerShared>,
    sink: Arc<VideoSink>,
    cancel: CancellationToken,
    connection: Arc<Connection>,
    join: AsyncMutex<Option<JoinHandle<()>>>,   // supervisor
}
pub struct ViewerOptions { pub path: ConnectOptions, pub codecs: Vec<VideoCodec>, pub limits: VideoLimits, pub quality: QualityPreset }
impl Viewer {
    pub async fn connect(client: &Client, invitation: &Invitation, options: ViewerOptions) -> Result<Self, ConnectError>;
    pub fn status(&self) -> ViewerStatus;           // sync, no tokio
    pub fn cursor(&self) -> Option<CursorSample>;   // sync
    pub fn video_sink(&self) -> Arc<VideoSink>;
    pub fn request_control(&self, enabled: bool) -> Result<()>;
    pub fn release_input(&self) -> Result<()>;
    pub fn input(&self, generation: u64, sequence: u64, input: Input) -> Result<()>;
    pub fn lock(&self) -> Result<()>;
    pub fn set_quality(&self, q: QualityPreset);
    pub fn pause(&self); pub fn resume(&self);
    pub fn request_keyframe(&self, reason: KeyframeReason) -> bool;  // via sink limiter
    pub async fn stop(&self) -> Result<()>;
}
pub enum ConnectError { IdentityChanged, VersionMismatch, Busy, Denied, InvalidRequest, Transport(String), Protocol(String) }
```

- `connect` returns only after `Accepted` (or a `Rejected` reason), so busy and
  denied become structured errors. Today `D/host.rs:172` drops the socket
  silently.
- The four constructors `connect*` (`D/viewer.rs:81-127`) collapse into one.

**Tasks:** `viewer-read` and `viewer-write` (split TLS halves, two tasks) under
a supervisor:

```
select! { cancel, read, write }; then sink.close(); shared.mark_stopped(reason)
```

The two halves never block each other, so receipts and probe echoes are
written as soon as the write task is scheduled. On the multi-thread FFI runtime
that is parallel to reads.

```rust
pub(crate) struct ViewerShared {
    state: parking_lot::Mutex<ViewerState>,       // last State + local facts (path, paused, phase, reason)
    stats: parking_lot::Mutex<ViewerStatsAcc>,    // last host Stats + local counters/histograms
    cursor: parking_lot::Mutex<Option<CursorSample>>,
    outbound: Outbound,                           // 3.3
}
```

### 3.2 Read task

For each `HostMessage`, in arrival order:

| Message | Action (no FFI, no Dart, no decoder) |
| --- | --- |
| `Video{seq,…}` + payload | validate (1.6) → `received_at = Instant::now()` → **`outbound.ack(seq)` first** (credit is transport credit, also for units the sink then drops) → `sink.push(VideoUnit{…})` → stats (`received_fps`, bytes) |
| `Probe{nonce}` | `outbound.echo(nonce)` immediately |
| `State{…}` | `shared.state` update; if the generation changed → `sink.set_generation(g)` |
| `Cursor{…}` | if `generation == current` → `shared.cursor` (seq++) |
| `Stats{…}` | `shared.stats.host = …` |
| `Notice{…}` | `shared.state.notice = Some((code, message, at))` |
| `Stopped{reason,…}` | end the session with that reason |

The live `path` is re-sampled from `connection.diagnostics()` at most once per
second inside `status()`, not per frame (`D/viewer.rs:170-188` sampled once).

### 3.3 Write task and outbound priorities

`Outbound` is a set of coalesced slots plus one bounded FIFO and one `Notify`.
The write task picks in this **biased** order:

| Prio | Slot | Kind | Bound |
| --- | --- | --- | --- |
| 1 | stop | cancel | — |
| 2 | control barriers | `Controls` (`D/viewer.rs:28-79`: revoke > release > grant; any barrier drains queued input) | 3 bits |
| 3 | probe echo | latest nonce | 1 |
| 4 | receipt | latest `seq` (cumulative) | 1 |
| 5 | input FIFO | `Input`, `Lock` in order | `INPUT_QUEUE = 32`; full → revoke barrier + `InputCongestion` (kept from `:410-418`) |
| 6 | pause/resume | latest boolean | 1 |
| 7 | presented | latest `(gen, frame_id)`, sent ≥ `PRESENTED_SEND_MIN_INTERVAL = 100 ms` apart (host needs < 2 s) | 1 |
| 8 | keyframe | latest reason (limiter in the sink) | 1 |
| 9 | fps cap | latest | 1 |
| 10 | quality | latest | 1 |
| 11 | ping | if nothing was written for 1 s | 1 |

`pause()` pushes the revoke barrier first (`Control{false}`, which drains
queued input), then sets `pause = true`.

### 3.4 `VideoSink`: the single-consumer video source (D8, D9)

```rust
// D/viewer/sink.rs
pub struct VideoUnit {
    pub seq: u64, pub generation: u64, pub frame_id: u64,
    pub codec: VideoCodec, pub keyframe: bool, pub duplicate: bool,
    pub discontinuity: bool,              // first unit after drop/generation change/resume: decoder must restart from it
    pub width: u32, pub height: u32, pub pts_us: u64,
    pub bytes: Arc<[u8]>,
    received_at: Instant,
}
pub enum Next { Unit(VideoUnit), Timeout, Closed, NotClaimed }
pub enum Receipt { Accepted, Stale, Closed }

pub struct VideoSink {
    state: parking_lot::Mutex<SinkState>,
    ready: parking_lot::Condvar,
    claimed: AtomicBool,
    closed: AtomicBool,
    outbound: Weak<ViewerShared>,         // presented / keyframe forwarding; Weak so a stale sink never extends a session
}
struct SinkState {
    queue: VecDeque<VideoUnit>, bytes: usize,
    generation: u64, wait_keyframe: bool, next_discontinuity: bool,
    handed: VecDeque<(u64 /*gen*/, u64 /*frame_id*/, Instant /*received*/)>, // ≤ 256, popped units only
    presented: Option<(u64, u64, Instant /*presented*/, Instant /*received*/)>,
    recovery: Recovery, keyframes: KeyframeLimiter,
    dropped: u64, backlog_drops: u64, stale_receipts: u64,
    present: PresentStats,                // intervals histogram, presented fps, viewer pipeline latency
    decoder: DecoderReport,
}
impl VideoSink {
    pub fn claim(&self) -> bool;                                  // CAS false→true; second claimant gets false
    pub fn unclaim(&self);
    pub fn next(&self, timeout: Duration) -> Next;                // blocks on Condvar only; no tokio
    pub fn presented(&self, generation: u64, frame_id: u64) -> Receipt;
    pub fn need_keyframe(&self, reason: KeyframeReason) -> bool;  // via limiter; false = coalesced/limited
    pub fn report(&self, report: DecoderReport);
    pub(crate) fn push(&self, unit: VideoUnit);                   // read task; never blocks
    pub(crate) fn set_generation(&self, g: u64);
    pub(crate) fn close(&self);                                   // closed=true, clear queue, notify_all
}
```

**`push` policy** (the only viewer overflow point):

1. If `unit.generation != state.generation`, drop the old queue.
   `wait_keyframe = true`, `next_discontinuity = true`.
2. If waiting for a keyframe and the unit is not a keyframe: drop it, `+1`, and
   `need_keyframe(Gap)`. The limiter holds requests for 1 s after a
   discontinuity, because the host sends that IDR on its own.
3. If the unit is a keyframe, drop every queued unit. They are superseded by an
   independently decodable picture. This is "keep the newest decodable frame",
   counted as `superseded`. Set `discontinuity = true` on this unit if anything
   was dropped or `next_discontinuity` was set.
4. **Backlog:** if the head waited more than `SINK_MAX_WAIT = 200 ms`, or the
   queue exceeds `SINK_MAX_UNITS = 32` or `SINK_MAX_BYTES = 8 MiB`:
   - drop the queue and this unit if it is a P-frame;
   - `wait_keyframe = true`; `backlog_drops += 1`;
   - `need_keyframe(Backlog)`;
   - signal the fps-cap controller to halve (3.6).
5. Push, then `ready.notify_one()`.

Backlog is judged by **wait age**, not count. A head-of-line burst after QUIC
loss recovery (all units with fresh `received_at`) drains through a hardware
decoder that runs faster than real time, without an IDR.

**`next`** returns NotClaimed if `!claimed`, and Closed if `closed`. Otherwise
it pops the head (waiting up to `timeout`), records `(gen, frame_id,
received_at)` in `handed` (≤ 256), and sets `unit.discontinuity`.

### 3.5 Presentation receipts and input authorisation

There is one enforcement point for Android, Windows and the Dart JPEG
consumer: `VideoSink::presented(gen, id)`.

```
Accepted only if: !closed
               && gen == state.generation
               && (gen, id) ∈ handed with received_at.elapsed() < PRESENTED_FRESH (2 s)
               && (presented is None || presented.gen != gen || id > presented.frame_id)   // monotonic
then: presented = (gen, id, now, received_at); present.record(now, now - received_at);
      recovery.on_presented(); outbound.presented(gen, id) (coalesced 100 ms)
else: stale_receipts += 1; Receipt::Stale
```

**Freshness** (`ViewerStatus.fresh`) requires all of: `state == Streaming`,
`!paused`, `presented.gen == state.generation`,
`presented.presented_at.elapsed() < 2 s`, and
`presented.received_at.elapsed() < 2 s`.

**`Viewer::input(generation, sequence, input)`:**

1. Require `fresh`, `controlling`, and `generation == state.generation`.
2. Stamp `frame_id = presented.frame_id`.
3. Push `ViewerMessage::Input{event}` to the outbound FIFO.

The host still checks `frame_id` in its own `recent` (< 2 s since produced),
current generation, sequence, policy and context (2.8). That is two
independent locks, as today. `Lock` has the same precondition.

Receipt acks (`seq`) appear nowhere in this path. Test
`receipt_acks_grant_no_input_rights` (7.1) proves it.

### 3.6 fps-cap controller (D8)

It runs every `FPS_CAP_INTERVAL = 1 s` in the read task, and only while
`state == Streaming && !paused`.

```
presented = valid receipts in the last second
backlog   = max sink.queue.len() observed during the second
factor    = if path == direct {0.9} else {0.8}           // RustDesk io_loop.rs:1205-1212
ceiling   = limits.max_fps
if backlog > max(1, presented/2) || (backlog > 1 for 2 consecutive seconds):
      new = max(FPS_CAP_MIN = 5, floor(presented * factor)); if backlog > presented { new = max(5, new/2) }
      cap = min(cap.unwrap_or(ceiling), new); clear = 0
elif backlog ≤ 1: clear += 1
      if clear > 3 && cap.is_some(): cap += FPS_CAP_STEP_UP (5); clear = 0; if cap ≥ ceiling { cap = None }
on sink backlog drop: cap = max(5, floor(presented * factor / 2)) immediately
send FpsCap{cap or ceiling} only on change, ≥ 1 s apart
```

A static desktop never lowers the cap, because a decrease needs backlog.
Display refresh bounds presented fps naturally: Android consumes one
SurfaceTexture image per Flutter frame. That satisfies "not above measured
Flutter fps".

### 3.7 Video recovery state machine and keyframe limiter (D7)

```
            push(IDR handed)                 presented ok
Live <------------------------- AwaitingIdr ------------> Live
 |  decoder error / backlog drop / gap / stall(no present 2 s while units flow)
 +--------------------------------> AwaitingIdr{attempt}
AwaitingIdr: request at t0, then t0+0.5, +1, +2, +4, +4 … (KEYFRAME_MIN_INTERVAL 500 ms, backoff ×2, cap 4 s)
any --Pause--> Paused (no requests)          Paused --Resume--> AwaitingIdr (hold 1 s: host sends IDR on Resume)
any --State != streaming--> HostUnavailable (no requests)   HostUnavailable --streaming, new gen--> AwaitingIdr (hold 1 s)
AwaitingIdr --DECODER_FAILURE_LIMIT (5) decoder errors in 30 s with no presented frame--> Failed (no requests; UI error + Retry)
```

`KeyframeLimiter` lives in `SinkState`, so native calls, sink drops and Dart's
"Refresh picture" share one limiter. `need_keyframe(User)` bypasses backoff but
not the 500 ms minimum.

### 3.8 Pause and resume (D13)

- **`Viewer::pause()`:** revoke barrier → `Pause` → `shared.paused = true` →
  sink recovery goes to `Paused`. The sink keeps whatever is queued: the
  feeder stops pulling only because nothing arrives.
- **Host on `Pause`:**
  - `qos.paused = true` → pace 0;
  - lane `EndControl`;
  - cursor sampling off;
  - probes and `State` continue (liveness), with `State.state = paused`.
- **`Viewer::resume()`:** `Resume` → `paused = false`. The host then:
  - forces an IDR;
  - sets `startup_until = now + 1 s`;
  - clears `unresponsive` and the delay history (a stale queue reading after
    a long pause must not divide fps).

The viewer never trips a recovery loop while paused:

- the sink makes no keyframe requests;
- native stall checks are suspended (5.1);
- Dart does not reconnect (6.4).

### 3.9 Metric definitions (overlay, acceptance report and docs use the same names)

| Name | Definition | Source |
| --- | --- | --- |
| `path` | `direct` / `relay` from `connection.diagnostics()`, re-sampled ≤ 1 Hz; `relay_transport` tcp/quic | viewer |
| `rtt_ms` | latest probe RTT (host → viewer → host), transport only | host `Stats` |
| `queue_delay_ms` | avg of the last 3 RTT minus baseline | host `Stats` |
| `picture_latency_ms` | `host_us/1000 + rtt_ms/2 + (presented_at - received_at)`, median over the last 2 s. No cross-machine clock | viewer |
| `presented_fps` | accepted `presented` receipts per second | viewer sink |
| `received_fps` | non-duplicate Video units per second | viewer |
| `present_interval_ms` | histogram (buckets 0-8-12-17-20-25-34-50-67-100-150-250-500-∞) with p50/p95/p99/max per session plus `gaps_100ms`, `gaps_250ms` counts | viewer sink |
| `pace_fps` / `target_fps` / `fps_cap` | host pace, controller fps, viewer cap | host `Stats` |
| `bitrate` / `received_bitrate` | host expected bps / viewer-measured bytes·8 per second | host / viewer |
| `width`, `height`, `codec`, `decoder` | encoded size; h264/jpeg; native decoder name from `report` | viewer |
| `dropped_frames` | host discards + sink drops (superseded, gap, backlog) | both |
| `keyframes` | IDRs received | viewer |

---

## 4. FFI (`pb-mapper-ffi`)

### 4.1 Split `DesktopState`: lifecycle vs hot path

```rust
// F/desktop.rs
pub(crate) struct DesktopState {
    host: tokio::sync::Mutex<Option<Host>>,                        // unchanged
    viewer_lifecycle: tokio::sync::Mutex<Option<Viewer>>,          // connect/disconnect only
    viewer_hot: parking_lot::RwLock<Option<Arc<ViewerHot>>>,       // every hot op; never awaited
    connecting: parking_lot::Mutex<Option<PendingConnect>>,        // (epoch, cancel, stage: ConnectStage)
    viewer_epoch: AtomicU64,
    host_identity: …, background: …,                               // unchanged
}
struct ViewerHot { viewer: Arc<ViewerHandle /* clones of Viewer's sync parts */>, sink_id: u64 }
pub enum ConnectStage { Path, Identity, Authorizing }              // reported by viewer_status while connecting
```

- `disconnect_viewer` order: `viewer_hot.write().take()`, then
  `video_registry::remove(sink_id)`, then `let v = lifecycle.lock().await.take();`.
  The guard drops before `v.stop().await`. This fixes the edition-2024 guard
  that today keeps the slot locked during `stop()` (`F/desktop.rs:154-166`).
- `pb_mapper_desktop_call_json` parses `Request`. If `request.is_hot()`, it
  calls `call_hot(&handle.desktop, request)` **synchronously, without
  `runtime.block_on`**. Otherwise it runs `block_on(call(..))` as today.
- Hot ops touch only `viewer_hot.read()` plus sync `Viewer` methods
  (parking_lot locks, watch sends, mpsc `try_send`). They are safe on the Dart
  UI isolate.

### 4.2 JSON ops

| op | Change | Request fields | `data` |
| --- | --- | --- | --- |
| `capabilities` | changed | — | helper gains `protocol`, `update_required` |
| `host_status` | **new** (replaces the host+background part of `status`) | — | `{host, background}`, each nullable (`HostStatus`, `BackgroundSnapshot`) |
| `status` | **removed** | — | — |
| `host_enable` | changed | `service, display_id, allow_control` | as today |
| `trust_status`, `trust_revoke`, `host_restore`, `helper_action`, `background_action`, `host_start`, `host_stop`, `host_permission` | unchanged | | invitations are `pbd1` text |
| `inspect_invitation` | **new** (no network) | `invitation: string` | `{service, name, fingerprint: "AB12-CD34-EF56-7890", format}`, where `format` is `pbd1` or `json_v1`; errors `NOT_DESKTOP_INVITATION` (setup string), `INVALID_ARGUMENT` (malformed/checksum) |
| `connect` | changed | `invitation, relay_only=false, quality="balanced", limits:{max_width,max_height,max_fps}, consumer` (`native` or `dart`) | `{video_sink: u64}`; errors `IDENTITY_CHANGED`, `VERSION_MISMATCH`, `HOST_BUSY`, `ACCESS_DENIED`, `INVALID_ARGUMENT`, `IO` |
| `disconnect` | unchanged | — | `{}` |
| `viewer_status` | **new, hot** | — | `ViewerStatus` (below) or `{state:"connecting", stage}` / `null` |
| `control` | hot | `enabled` | `{}` |
| `release` | hot | — | `{}` |
| `input` | changed, hot | `event:{generation, sequence, input}` (no `frame_id`) | `{}`; error `NOT_FRESH` / `NOT_CONTROLLING` |
| `lock` | **new, hot** | — | `{}` (result arrives as `ViewerStatus.notice`) |
| `keyframe` | changed, hot | `reason` (`user`, …) | `{requested: bool}` |
| `presented` | changed, hot (Dart JPEG consumer only) | `generation, frame_id` | `{accepted: bool}` |
| `quality` | **new, hot** | `quality` | `{}` |
| `pause` / `resume` | **new, hot** | — | `{}` |
| `native_video`, `video` fields | **removed** | | |

`consumer: "native"` offers `[H264]` (Android and Windows).
`consumer: "dart"` offers `[Jpeg]`.

```jsonc
// ViewerStatus (FFI-local, never on the wire)
{
  "state": "connecting|streaming|paused|capture_unavailable|stopped",
  "reason": null | "secure_desktop|helper_unavailable|input_failed|control_denied|input_congestion|host_stopped|revoked|transport|protocol|timeout|decoder_failed",
  "message": "…",
  "control_allowed": true, "controlling": false, "fresh": true, "paused": false,
  "generation": 7, "display": {"id":…,"name":…,"x":…,"y":…,"width":…,"height":…}, "desktop": "ordinary|secure",
  "video": {"codec":"h264","width":1920,"height":1080},
  "presented": {"generation":7,"frame_id":1234,"age_ms":12} | null,
  "quality": "balanced", "capabilities": ["lock"],
  "notice": {"code":"lock_failed","message":"…","age_ms":30} | null,
  "decoder": {"name":"c2.qti.avc.decoder","state":"ok|recovering|failed","message":""},
  "stats": { "path":"direct","relay_transport":null,"rtt_ms":14,"queue_delay_ms":3,"picture_latency_ms":41,
             "presented_fps":59.8,"received_fps":60.0,"pace_fps":60,"target_fps":60,"fps_cap":0,
             "bitrate":4000000,"received_bitrate":3810000,"dropped_frames":0,"keyframes":1,
             "present_interval_ms":{"p50":16.7,"p95":18.1,"p99":24.9,"max":33.0},"gaps_100ms":0,"gaps_250ms":0,
             "adaptation":"hold" },
  "video_sink": 3
}
```

### 4.3 New C exports (Windows runner pull path and the Dart JPEG consumer)

```c
// All in pb_mapper_ffi.{dll,so}; integer returns; no JSON; sink ids are opaque, never pointers.
typedef struct PbDesktopVideoUnit {
    const uint8_t* data; size_t len;     // valid until pb_mapper_desktop_video_release_v1
    uint64_t seq, generation, frame_id, pts_us;
    uint32_t width, height;
    uint32_t codec;                      // 1 = h264, 2 = jpeg
    uint32_t flags;                      // bit0 keyframe, bit1 duplicate, bit2 discontinuity
    const void* _owner;                  // Arc<[u8]> raw; private; zeroed on release
} PbDesktopVideoUnit;

typedef struct PbDesktopDecoderReport {
    uint32_t state;                      // 0 ok, 1 recovering, 2 failed
    uint32_t queued;                     // native queue length
    uint64_t decoded, dropped, configurations;
    char decoder[64];                    // NUL-terminated
    char message[192];                   // NUL-terminated
} PbDesktopDecoderReport;

int32_t pb_mapper_desktop_video_claim_v1(uint64_t sink);                 //  1 ok, 0 already claimed, -1 unknown/closed
void    pb_mapper_desktop_video_unclaim_v1(uint64_t sink);
int32_t pb_mapper_desktop_video_next_v1(uint64_t sink, uint32_t timeout_ms, PbDesktopVideoUnit* out);
                                                                         //  1 unit, 0 timeout, -1 closed/unknown, -2 not claimed, -3 invalid arg
void    pb_mapper_desktop_video_release_v1(PbDesktopVideoUnit* unit);    // idempotent: null _owner is a no-op
int32_t pb_mapper_desktop_video_presented_v1(uint64_t sink, uint64_t generation, uint64_t frame_id); // 1 accepted, 0 stale, -1 closed
int32_t pb_mapper_desktop_video_need_keyframe_v1(uint64_t sink, uint32_t reason); // 1 sent, 0 limited/coalesced, -1 closed
                                                                         // reason: 1 decoder_error, 2 gap, 4 stall, 5 user
int32_t pb_mapper_desktop_video_report_v1(uint64_t sink, const PbDesktopDecoderReport* report); // 0 ok, -1 closed
int32_t pb_mapper_desktop_cursor_v1(PbMapperHandle* handle, PbDesktopCursor* out); // 1 sample, 0 none; sync, no tokio
typedef struct PbDesktopCursor { uint64_t generation, seq; int32_t x, y; uint8_t visible; uint8_t _pad[7]; } PbDesktopCursor;
uint64_t pb_mapper_desktop_instance_token_v1(void);                      // address of a static; D9 single-instance check
```

**Removed:** `pb_mapper_desktop_frame`, `pb_mapper_desktop_wait_frame`,
`pb_mapper_desktop_free_frame` and `DesktopBuffer` (`F/desktop.rs:1211-1292`).

- Every export wraps its body in `catch_unwind` (returning -3 or 0) and uses
  no `unwrap`/`expect`.
- `next_v1` is the only blocking call. It blocks on the sink's
  `parking_lot::Condvar`, never on tokio. It must never run on a UI thread or
  the Dart UI isolate.

### 4.4 JNI (Android feeder)

Rust: `F/desktop_jni.rs`, `#[cfg(target_os = "android")]`. Add
`[target.'cfg(target_os = "android")'.dependencies] jni.workspace = true`,
pinning `jni = "0.21.1"` in the root `[workspace.dependencies]`. That version is
already in `Cargo.lock` (via `netdev`), so `--locked` CI stays offline-safe, and
RustDesk sets the precedent.

The design needs no `JNI_OnLoad`, no `JavaVM` and no `ndk_context` (pull
design: every call is a down-call on an attached Java thread).

```kotlin
// ui/android/app/src/main/kotlin/com/pb_mapper/ui/DesktopVideoNative.kt (new)
package com.pb_mapper.ui
import java.nio.ByteBuffer
internal object DesktopVideoNative : VideoSourceApi {
    init { System.loadLibrary("pb_mapper_ffi") }        // same soname Dart dlopen()ed: one instance (verified by instanceToken)
    const val TIMEOUT = -1; const val CLOSED = -2; const val NEED_CAPACITY = -3; const val NOT_CLAIMED = -4; const val FAILED = -5
    const val META_SEQ = 0; const val META_GENERATION = 1; const val META_FRAME_ID = 2; const val META_WIDTH = 3
    const val META_HEIGHT = 4; const val META_FLAGS = 5; const val META_PTS_US = 6; const val META_LENGTH = 7; const val META_SIZE = 8
    @JvmStatic external fun claim(sink: Long): Int
    @JvmStatic external fun unclaim(sink: Long)
    @JvmStatic external fun next(sink: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int
    @JvmStatic external fun presented(sink: Long, generation: Long, frameId: Long): Int
    @JvmStatic external fun needKeyframe(sink: Long, reason: Int): Int
    @JvmStatic external fun report(sink: Long, state: Int, queued: Int, decoded: Long, dropped: Long, configurations: Long, decoder: String, message: String)
    @JvmStatic external fun instanceToken(): Long
}
internal interface VideoSourceApi { /* the same methods, so VideoFeeder is JVM-testable with a fake */ }
```

Rust symbols use `extern "system"` and `#[unsafe(no_mangle)]`; package `_`
mangles to `_1`:

```rust
Java_com_pb_1mapper_ui_DesktopVideoNative_claim(env: JNIEnv, _: JClass, sink: jlong) -> jint
Java_com_pb_1mapper_ui_DesktopVideoNative_unclaim(env: JNIEnv, _: JClass, sink: jlong)
Java_com_pb_1mapper_ui_DesktopVideoNative_next(env: JNIEnv, _: JClass, sink: jlong, timeout_ms: jint, buffer: JByteBuffer, meta: JLongArray) -> jint
Java_com_pb_1mapper_ui_DesktopVideoNative_presented(env: JNIEnv, _: JClass, sink: jlong, generation: jlong, frame_id: jlong) -> jint
Java_com_pb_1mapper_ui_DesktopVideoNative_needKeyframe(env: JNIEnv, _: JClass, sink: jlong, reason: jint) -> jint
Java_com_pb_1mapper_ui_DesktopVideoNative_report(env: JNIEnv, _: JClass, sink: jlong, state: jint, queued: jint, decoded: jlong, dropped: jlong, configurations: jlong, decoder: JString, message: JString)
Java_com_pb_1mapper_ui_DesktopVideoNative_instanceToken(env: JNIEnv, _: JClass) -> jlong
```

**`next`:**

1. Look up the sink and peek the head. If `len > GetDirectBufferCapacity(buffer)`,
   write `meta[META_LENGTH] = len` and return `NEED_CAPACITY` **without popping**.
2. Otherwise pop, copy into `GetDirectBufferAddress`, write meta with
   `SetLongArrayRegion`, and return `len`.

**Threading:**

| Call | Thread |
| --- | --- |
| `next` | only the `pb-desktop-feed` thread (blocks ≤ `timeoutMs` = 50) |
| `presented` | the `pb-desktop-video` HandlerThread (`consumeSurface`) |
| `needKeyframe`, `report` | codec thread |
| `claim`, `unclaim` | the feeder thread at start and exit |
| `instanceToken` | any thread |

Every function wraps its body in `catch_unwind`. A JNI exception after a
failed `get_direct_buffer_address` is cleared and the function returns
`FAILED`.

### 4.5 Single consumer and lifetime

```rust
// F/desktop_video.rs
static SINKS: parking_lot::Mutex<HashMap<u64, Weak<VideoSink>>>;  static NEXT_SINK: AtomicU64 = AtomicU64::new(1); // ids never reused
```

- `connect` registers `Weak(viewer.video_sink())` and returns the id. Every
  export upgrades the `Weak` per call and holds the `Arc` only for that call.
  A blocked `next` therefore keeps the sink alive just until `close()` wakes
  it with `Closed`.
- `disconnect` removes the id, the session supervisor calls `sink.close()`
  (`closed = true`, queue cleared, `notify_all`), and the `Viewer` drops its
  `Arc`. A late native call with a stale id gets -1, never UB.
- A unit handed out through `PbDesktopVideoUnit` keeps its own
  `Arc<[u8]>` clone in `_owner` until `release_v1`. That is valid after
  disconnect, the property that
  `binary_frame_is_owned_until_explicit_release_after_disconnect`
  (`F/desktop.rs:1725-1812`) protects today.
- **Single consumer:** `claim` is a CAS. `next` without a claim returns
  `NOT_CLAIMED`. Exactly one of {Kotlin feeder, Windows decoder thread, Dart
  JPEG isolate} claims per session. A second claimant fails loudly.
- **Double-load detection:** if Android ever loaded two copies of the
  library, Kotlin's `claim(id)` would hit an empty `SINKS` and return -1.
  The feeder reports "native library instance mismatch" (via Dart's texture
  error) instead of streaming nothing silently. `instanceToken()` (Kotlin) and
  `pb_mapper_desktop_instance_token_v1()` (Dart) are logged at debug level at
  first use, and must be equal.

### 4.6 Error codes

`F/error.rs` adds `IDENTITY_CHANGED`, `VERSION_MISMATCH`, `HOST_BUSY`,
`ACCESS_DENIED`, `NOT_DESKTOP_INVITATION`, `NOT_FRESH` and `NOT_CONTROLLING`.
Each maps from typed errors (`ConnectError`, `InvitationError`, input
precondition errors). No error is produced from message text.

---

## 5. Native decoders

### 5.1 Android `DesktopVideo.kt`

**MethodChannel `pb_mapper/desktop_video` v2:**

- `create` → `textureId`
- `attach {textureId, sink}`
- `detach {textureId}`
- `setPaused {textureId, paused}`
- `snapshot {textureId, path, maxWidth}` → `bool`
- `stats {textureId}`
- `dispose {textureId}`

**Removed:** `submit`, `reset`, and the native→Dart `event`. Presentation and
keyframes go to Rust; Dart reads decoder health from `viewer_status`.

**Feeder (new class `VideoFeeder`, JVM-testable through `VideoSourceApi`):**

```kotlin
internal class VideoFeeder(private val api: VideoSourceApi, private val sink: Long, private val target: FeedTarget) : Thread("pb-desktop-feed") {
    // FEED_QUEUE_MAX = 2 queued submissions + 1 in hand = pool of 3 direct ByteBuffers (grow to next pow2, ≤ 4 MiB)
    override fun run() {
        if (api.claim(sink) != 1) { target.failed("video sink unavailable"); return }
        try {
            while (!closed) {
                target.awaitRoom()                                  // waits while queue.size >= FEED_QUEUE_MAX (lock + condition)
                val buf = pool.take()
                when (val r = api.next(sink, 50, buf, meta)) {
                    TIMEOUT -> pool.give(buf)
                    NEED_CAPACITY -> pool.grow(buf, meta[META_LENGTH].toInt())      // same unit stays at the head
                    CLOSED -> { pool.give(buf); break }
                    else -> if (r >= 0) target.offer(Unit(meta, buf, r)) else { pool.give(buf); target.failed("feed error $r"); break }
                }
            }
        } finally { api.unclaim(sink) }
    }
}
```

`Viewer.attach(sink)` (main thread):

1. Stop and join the previous feeder (≤ 60 ms: `next` times out at 50 ms).
2. `revision++`, `clearQueue()`, `awaitingKey = true`.
3. Start a new `VideoFeeder`.

The texture and surface persist across reconnects, so the last picture stays
for the greyed "Reconnecting" overlay.

`target.offer(unit)` runs on the feeder thread. It is today's `submit` logic
(`DesktopVideo.kt:217-252`) without the `MethodChannel.Result`:

- `discontinuity` → `revision++` and a codec rebuild on this IDR;
- a generation fence;
- `awaitingKey`;
- enqueue the unit;
- `schedulePump(0)`, posting to the `pb-desktop-video` worker.

The codec pump, `VideoPresentationQueue`, one picture on the surface, and
revision fencing stay unchanged. The pump copies from the pooled direct buffer
into the codec input buffer and returns the buffer to the pool after
`queueInputBuffer`.

**Receipts and recovery:**

- `consumeSurface` (`:180-215`) calls
  `DesktopVideoNative.presented(sink, packet.generation, packet.frameId)` where
  it emits `event(... presented)` today.
- `fail(...)` (`:351-362`) calls `needKeyframe(sink, 1)` and
  `report(sink, RECOVERING, …)`.
- Awaiting-key drops call `needKeyframe(sink, 2)`.
- A successful present after a failure sends `report(OK)`.

**Pause** (`setPaused`):

- sets `@Volatile paused`;
- disables the 2 s consume/decode stall checks (`:524-539`);
- makes `fail` skip `needKeyframe`;
- on unpause, clears the `surfacePtsUs` wait. The next unit is the host's
  resume IDR, flagged `discontinuity`, which rebuilds the codec.

This removes the backgrounded "texture was not consumed" recovery loop that
the native-decoders map documented.

**Decoder hints** are applied only when measurement (7.7 case F) shows a
gain:

- `KEY_PRIORITY = 0`;
- `KEY_OPERATING_RATE = Short.MAX_VALUE` guarded per API level;
- `HandlerThread` at `Process.THREAD_PRIORITY_DISPLAY`.

They are behind a `const val DECODER_HINTS = true`, set after the experiment.

**Snapshot (D23):**

1. Post to the worker:
   `PixelCopy.request(surface, Bitmap(w', h'), listener, workerHandler)`, with
   `w' = min(maxWidth, width)` and the aspect preserved. API 24 matches
   minSdk 24.
2. Compress to JPEG at quality 80 into `path.tmp`.
3. `fsync`, then rename to `path`.
4. Reply `true` or `false` within 300 ms; on timeout reply `false`.

Dart shows a placeholder on `false`. Whether `PixelCopy` reads a
`SurfaceTexture`-backed producer surface must be verified on the device
(risk R6). It never blocks closing.

**MainActivity** is unchanged apart from passing nothing new. The channel
handler stays on the main thread, because the remaining calls are rare.
`viewers` becomes a `ConcurrentHashMap`, since feeders call back.

### 5.2 Windows `ui/windows/runner/desktop_video.cpp`

**Symbol resolution at `create`:**

- `module_ = LoadLibraryExW(L"pb_mapper_ffi.dll", nullptr, LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`.
  This adds a reference to the DLL Dart already loaded. `FreeLibrary` runs in
  `~Impl`.
- `GetProcAddress` for the eight `pb_mapper_desktop_video_*_v1` symbols. If any
  is missing, `create` fails ("pb_mapper_ffi.dll does not match this app"),
  because the DLL and EXE ship together.

**`Viewer::Run()`** (`:455-504`) replaces `wake_.wait` and `queue_`:

```cpp
if (claim_(sink) != 1) { Fail("video sink unavailable"); return; }
while (!stopped_) {
  PbDesktopVideoUnit unit{};
  int32_t r = next_(sink, 50, &unit);
  if (r == 0) continue; if (r < 0) break;
  // generation / revision / awaiting-key / discontinuity logic moved here from Submit (:144-180)
  decoder->Feed(unit);  release_(&unit);              // MFCreateMemoryBuffer copies, so release immediately
  for (auto& s : decoder->Take()) { latest_ = s; registrar_->MarkTextureFrameAvailable(id_); }
}
unclaim_(sink);
```

- **Receipts:** `AcquireSurface` (raster thread, `:196-222`) calls
  `presented_(sink, gen, frame)` on the first acquisition of a new
  `(revision, frame)`. That is a non-blocking registry lookup plus a mutex.
- **Failures:** `Fail` calls `need_keyframe_(sink, 1)` and `report_`.
- **Attach:** stop and join the worker, `++revision_`, **keep `latest_`** so
  the last picture stays during reconnect (today it blanks after reset), then
  start a new `Run`.
- **Pause:** only stops keyframe requests. Windows has no consume-stall check.
- **Perf fix:** a pool of 3 BGRA `D3D11_RESOURCE_MISC_SHARED` textures,
  respecting outstanding `SurfaceLease`s, replaces one `CreateTexture2D` per
  frame (`:433-441`).
- **Snapshot:** a request posted to the decoder thread:
  1. `VideoProcessorBlt` from `latest_` to a ≤ 320-wide BGRA texture;
  2. `CopyResource` to a staging texture, then `Map`;
  3. WIC JPEG encoder (`CLSID_WICJpegEncoder`, quality 0.8) to `path.tmp`,
     then `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)`.

  The reply goes through `Events` within 300 ms.
- **Removed:** `Submit`, `queue_`, `Events::replies`, the `submit`/`reset`
  handlers, and the coalesced `event` to Dart.

### 5.3 Unchanged invariants

- "Presented" still means consumed by the compositor (Android
  `OnFrameConsumedListener` timestamp; Windows first `AcquireSurface`).
- Impeller stays off (`AndroidManifest.xml:14-21`).
- The texture size comes from the encoded size only. Zoom, pan, rotation and
  keyboard never reach native code (P1-4).

---

## 6. Dart

### 6.1 `DesktopApiClient` after the change (`U/ffi/desktop_api.dart`)

```dart
abstract interface class DesktopApiClient {
  // host (unchanged except hostStatus / enableHost allowControl)
  Future<DesktopCapabilities> capabilities();
  Future<DesktopHostSnapshot> hostStatus();                       // {host: HostStatus?, background}
  Future<DesktopHostEnable> enableHost(String service, int displayId, {required bool allowControl});
  Future<DesktopTrust> trustStatus(); Future<String?> restoreHost(); Future<void> revokeTrust();
  Future<bool> helperAction(String action); Future<DesktopBackground> backgroundAction(String action);
  Future<void> stopHost({bool onExit = false}); Future<void> allowControl(bool allow);
  // viewer
  Future<DesktopInvitationInfo> inspectInvitation(String text);   // throws DesktopCallException(code: NOT_DESKTOP_INVITATION|INVALID_ARGUMENT)
  Future<DesktopSession> connect(String invitation, {required bool relayOnly, required DesktopQuality quality,
                                 required DesktopLimits limits, required DesktopConsumer consumer}); // -> videoSink id
  Future<void> disconnect();
  ViewerStatus? viewerStatus();                                   // sync (hot op on the UI isolate)
  DesktopCursorSample? cursor();                                  // sync C export, per rendered frame
  void control(bool enabled); void release(); void lock();       // sync hot ops; throw DesktopCallException
  void input({required int generation, required int sequence, required Map<String, Object?> input});
  void setQuality(DesktopQuality quality); void pause(); void resume();
  bool requestKeyframe();                                         // reason "user"
  // Dart JPEG consumer only (non-Android/Windows)
  Stream<DesktopJpegFrame> jpegFrames(int videoSink);             // isolate pulling video_next_v1, 1 credit
  bool presented(int generation, int frameId);
}
```

- `DesktopLimits` for phones: `1920×1080`, `maxFps = min(60, display refresh)`.
  Desktop: `min(90, refresh)`. Both are clamped to 30..=120.
- `DesktopConsumer.native` on Android and Windows; `dart` elsewhere.
- `PbMapperService` gains `desktopCallSync(Map)`, which calls
  `pb_mapper_desktop_call_json` directly on the calling isolate and is used
  only for hot ops. It also gains the C-export lookups (4.3).
- `DesktopVideoTexture` becomes `create()`, `attach(int sink)`, `detach()`,
  `setPaused(bool)`, `snapshot(String path, {int maxWidth = 320})`, `stats()`
  and `dispose()`. It takes an injectable `DesktopTextureFactory` so tests run
  on Linux CI.

### 6.2 `DesktopStore` public surface

```dart
class DesktopStore extends ChangeNotifier {
  // host side
  DesktopCapabilities? get capabilities; DesktopTrust? get trust; DesktopHostSnapshot? get host; String? get invitation;
  // viewer side — notify-worthy snapshot (== compared)
  DesktopViewState get view;            // phase, viewOnly, controlling, fresh, generation, displayWidth/Height,
                                        // videoWidth/Height, codec, paused, errorCode, retryIn, textureId, target
  ValueListenable<ViewerStats?> get stats;     // overlay: path, relayTransport, rttMs, queueDelayMs, pictureLatencyMs,
                                               // presentedFps, bitrate, receivedBitrate, width, height, codec, decoder, adaptation
  DesktopQuality get quality;
  DesktopCursorSample? readCursor();    // sync; called by the viewer's Ticker per rendered frame
  // commands
  Future<void> connect(DesktopTarget target, {DesktopQuality? quality});
  Future<void> disconnect({String? thumbnailPath});    // snapshot is best-effort, ≤ 300 ms, only when remembered && fresh
  void retryNow(); void control(bool enabled); void release(); void lock(); void requestKeyframe();
  void send(DesktopInput input);        // sync: drops when !view.controlling || !view.fresh; sequence per session
  void setQuality(DesktopQuality q);    // live op; never reconnects
  void setViewerVisible(bool visible);  // route mount/unmount + AppLifecycle (hidden/paused → false; resumed/inactive → true)
  void setRemotePageVisible(bool visible);
}
enum DesktopViewerPhase { idle, connecting, live, slow, waitingForDesktop, paused, reconnecting, identityChanged, failed }
```

Mapping to the six `ViewerStates` overlays:

| Overlay | Phase and condition |
| --- | --- |
| connecting (steps) | `connecting` with `stage` |
| slow | `slow`: `queueDelayMs ≥ 150` for ≥ 3 s, or `presentedFps < 0.5 × paceFps` for ≥ 3 s with `paceFps ≥ 10` |
| waiting for remote desktop | `waitingForDesktop` (`reason: secure_desktop`) |
| reconnecting | `reconnecting` (Dart backoff 1, 2, 4, 8, 15 s, kept from `desktop_store.dart:452-455`) |
| identity changed | `identityChanged` (`IDENTITY_CHANGED` code; no retry) |
| view-only | `view.viewOnly` |

**Reconnect policy** (the Dart state machine):

- Reconnect on a viewer `stopped` state caused by transport, timeout or
  protocol.
- Reconnect when no video has arrived for `NO_VIDEO_RECONNECT = 5 s` while the
  host is streaming, the session is not paused, and the decoder state is not
  `failed`.
- **Never** reconnect because the picture is not fresh while video flows. The
  Rust sink owns decoder recovery.
- Never reconnect while paused or `capture_unavailable`.
- `VERSION_MISMATCH`, `ACCESS_DENIED` and `INVALID_ARGUMENT` are terminal.
  `HOST_BUSY` retries at the backoff rate.

**Input:** `send` stays synchronous and ordered.

- Delete `_inputTail`, `_inFlight` and `_PendingMove`. Calls no longer queue
  in Dart.
- Keep `_inputEpoch` and `_sequence = 0` per session.
- Coalesce moves to at most one per 4 ms.
- Rust's 32-slot queue does the overflow: it revokes and returns
  `input_congestion`.

### 6.3 Notification and polling policy (D14)

| Source | When it runs | Rate | Notifies |
| --- | --- | --- | --- |
| viewer poll (`viewerStatus`, sync) | viewer visible and session exists | 250 ms | `notifyListeners` only if `view` changed (`==`); `stats` ValueNotifier at ≤ 1 Hz |
| remote-list poll | Remote page visible, session exists, viewer not visible | 2 s | as above |
| host poll (`hostStatus`) | sharing is active (needed for the D15 "being viewed" bar) or Remote page visible | 1 s | only on `host` change |
| cursor (`readCursor`) | viewer `Ticker`, only while visible | per frame | none; the cursor layer repaints through its own `ValueNotifier` only when `seq` changes |
| everything else | — | stopped | — |

- `AppStore.desktop` drops `..addListener(_notify)` (`U/state/app_store.dart:41-42`).
  The rail dot, tray and D15 bar listen to `DesktopStore` directly.
- `DesktopViewer` rebuilds only overlays on `view`. The `Texture` subtree is a
  `const`-stable child and never rebuilds at media rate.

### 6.4 Pause and resume in Dart

- `setViewerVisible(false)` with a live session:
  1. `texture.setPaused(true)`;
  2. `api.pause()` (Rust pushes revoke, then `Pause`);
  3. `_inputEpoch++`; stop the 250 ms poll and the cursor ticker.
- `setViewerVisible(true)`:
  1. `texture.setPaused(false)`;
  2. `api.resume()`;
  3. restart the poll.
- Control is re-requested only after `view.fresh`, as today
  (`desktop_store.dart:394-410`).

### 6.5 Dart deletions

- `_desktopFramesWorker`, `_desktopFrameIsolate` (H.264), `DesktopFrameStream`,
  `PbMapperService.desktopFrames` and `desktopFrame`, and the routing of
  `presented|keyframe|input|control|release` (`U/ffi/pb_mapper_service.dart:27-316`).
- The `pbMapperDesktopFrame`, `WaitFrame` and `FreeFrame` lookups and
  `DesktopBuffer` (`U/ffi/pb_mapper_ffi.dart:200-226`).
- `DesktopApiClient.frames`, `frame` and `status`.
- In `DesktopStore`: `_frames`, `_consumeFrames`, the H.264 half of
  `_acceptFrame`, `_presentVideo`'s validation (moved to Rust), `_flushFeedback`'s
  presented half, `_videoStatuses`, `videoPresentation`, per-frame
  `cursorStatus`, the 700 ms keyframe watchdog (moved to Rust 3.7), the
  `frameAckMs > 500` `delayed` phase, the `identity changed` substring match,
  the test-only pull path in `poll()` (`:342-350`), and `startHost`/`backgroundAction`
  (dead).
- `DesktopVideoTexture.submit`, `reset` and `events`.
- `_AcceptanceApi.presented` counters, which move to Rust stats.

---

## 7. Test plan (minimal, deliberate)

### 7.1 Rust unit (`cargo test -p pb-mapper-desktop`)

| Test (module) | Boundary it guards |
| --- | --- |
| `invite::pbd1_round_trip_rejects_tamper_and_setup_strings` | encode/decode; a flipped byte gives `Checksum`; a `pbmt1_` or base64 setup JSON gives `SetupString`; v1 JSON is accepted and gives the same fingerprint |
| `security::fingerprint_pin_rejects_another_certificate` (rewrite of `tests.rs:15`) | identity downcast, chain rejected |
| `protocol::strict_parsing_and_version_gate` | unknown field or op fails in both directions; a v1 ALPN peer produces `VersionMismatch` |
| `credit::receipt_window_admits_idr_from_reserve_without_presentation` | P-frames fill to `bytes - reserve`; an IDR is admitted from the reserve; a receipt frees credit; an ack for an unsent seq is a protocol error; an empty ledger admits anything |
| `qos::delay_scenario_follows_rustdesk_table` | one scenario walk: 3 good samples give +5; 200 ms divides fps; unresponsive gives 2 fps; a preset change resets bpf without changing resolution; the latch flips once |
| `idr::requests_coalesce_and_defer_but_are_never_dropped` | cooldown deferral; force bypass; nothing periodic |
| `auth::receipt_acks_grant_no_input_rights` | **D4 security boundary.** Frames received but never presented: input is refused and `release_if_stale` releases. A presented fresh frame admits input. Older than 2 s, or a generation change, is refused. Stale `Presented` sets `stale_presentation` |
| `worker::input_lane_runs_before_capture_and_never_after_stop` | lane priority with a capture due; a queued input is not executed after cancel; overflow ends control |
| `worker::pacing_skips_ticks_without_bursts` | fake backend and clock: pace 60 gives evenly spaced submits; a full submit gate skips ticks; releasing the gate causes no catch-up burst; pace 0 means no submits |
| `host::cursor_not_echoed_within_300ms_but_generation_changes_pass` | `CursorGate` |
| `viewer::sink::backlog_drops_to_next_idr_and_requests_once` | wait-age rule; a burst with fresh `received_at` is not dropped; an IDR supersedes the queue; the limiter backs off |
| `viewer::sink::single_consumer_and_close_wakes_waiter` | claim CAS; `next` blocked in another thread returns `Closed` on `close()` |
| `viewer::sink::presentation_requires_current_handed_fresh_monotonic` | Stale cases |
| `viewer::fps_cap_lowers_only_under_backlog_and_probes_up` | the 3.6 rules |
| `viewer::repeated_releases_are_bounded_and_revocation_precedes_a_new_grant` | kept (`D/viewer.rs:442`), on `Outbound` |
| `worker::input_requires_control_current_geometry_and_unique_sequence`, `native_input_failure_ends_control_and_survives_new_video`, `observation_never_acquires_write_admission_and_revocation_releases_it`, `unavailable_backend_reports_failure_and_stops` | kept, adapted to `AuthLedger` |
| `wire::clean_tls_close_is_eof_only_between_messages` | kept, v2 framing |
| `host::remembered_invitation_survives_restart_but_not_revocation` | kept, pbd1 |
| `tests::blocked_video_does_not_block_local_stop` | kept |
| `video/windows.rs` `gpu_output_dimensions…`, `parameter_sets…`; `windows.rs` held-set and cursor-privacy tests; helper binding tests | kept |

### 7.2 Integration (`crates/pb-mapper-desktop/tests/session.rs`) and acceptance

- **Keep and adapt** `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown`
  (`:73`):
  - a bad token gets `Denied`, sees no frames and starts no capture;
  - a second viewer gets `HOST_BUSY`;
  - input needs control plus a presented frame obtained through
    `sink.claim/next/presented`;
  - a duplicate sequence is rejected; revoke works; direct survives a relay
    stop.
- **Replace** `cursor_crosses_a_full_video_window_without_refreshing_the_picture`
  (`:206`) with `frozen_renderer_keeps_credit_and_cursor_but_never_input`:
  - a claimed consumer pulls but never presents for 5 s;
  - video keeps arriving (receipt credit, `received ≥ 4 s × pace`);
  - cursor updates arrive;
  - `Viewer::input` returns `NOT_FRESH`;
  - the host never sends `Stopped`;
  - presenting one frame restores input.
- **New** `pause_stops_video_and_resume_starts_with_an_idr`:
  - after `pause`, no `Video` for 2 s, while probes are still echoed;
  - after `resume`, the first unit is `keyframe && discontinuity`.
- `slow_link.rs` (ignored acceptance) is rewritten as two tests:
  - `receipt_credit_tracks_the_link_not_the_renderer`. A fixed WAN of 350 ms
    keeps the target bitrate. A slow **consumer** of 67 ms per frame does
    **not** lower host bitrate and does lower the fps cap. Encoder outputs never
    expire.
  - `slow_tls_reader_lowers_fps_and_bitrate_then_recovers_at_fixed_resolution`.
    A 100 KiB/s reader for 8 s, then unlimited. Asserts pace ≤ 15 during the
    restriction, recovery to ≥ 0.8 × start bitrate within 30 s, and
    `width/height` unchanged throughout.

### 7.3 FFI (`cargo test -p pb-mapper-ffi`)

- `hot_ops_never_wait_for_disconnect`: hold `viewer_lifecycle` in a task, then
  call the `viewer_status`, `input`, `control` and `pause` JSON ops. Each
  completes in < 10 ms with no runtime entered.
- `video_unit_outlives_disconnect_and_stale_ids_are_harmless`: rewrite of
  `:1725-1812`. Claim, next, disconnect: the unit is still readable until
  release; `next` and `presented` on the old id return -1; double release is a
  no-op.
- `inspect_invitation_classifies_without_network`: pbd1, v1 JSON and a setup
  string give `NOT_DESKTOP_INVITATION`.
- **Keep:** `disconnect_cancels_*` (`:1575-1704`), background and trust tests.
- **Delete:** `status_and_video_with_the_same_id_both_cross_the_binary_boundary`
  (`:1318`).

### 7.4 Dart (`flutter test`, runs on Linux CI)

| Test | Guards |
| --- | --- |
| `desktop_store_test` "control is requested only after the Rust view is fresh, and input is dropped otherwise" | input gating from `ViewerStatus.fresh` (replaces `desktop_lifecycle_test.dart:243-334`, whose validation moved to Rust) |
| "hidden viewer pauses once, stops polling, never reconnects; visible resumes" | D13 and D14 (`fake_async`: count `viewerStatus` calls, which is 0 while hidden; `pause`/`resume` once) |
| "stats-only changes do not notify the store and AppStore is never notified" | D14 fan-out |
| "quality change is a live op, not a reconnect" | D12 |
| "reconnect only on transport loss or 5 s without video; identity/version errors are terminal" | rewrite of the `desktop_recovery_test.dart` cases, by error code |
| "texture survives reconnect (attach new sink) and thumbnail capture is best-effort before dispose" | injected `DesktopTextureFactory` (replaces the CI-skipped `desktop_lifecycle_test.dart:199-241`) |
| keep: `desktop_recovery_test.dart` invalid-invitation, retry-same-target, cancelled-completion, motion-coalescing (adapted to sync input) | |

**Delete:** `desktop_lifecycle_test.dart:98-143` (ACK cadence) and `:144-197`
(DesktopFrameStream). Rewrite `desktop_test.dart:98-278` against a
`viewerStatus`-driven fake. `FakeDesktopApi` implements the new interface,
including sync members.

### 7.5 Kotlin (`ui/android/app/src/test/kotlin`)

- Keep `VideoPresentationQueueTest`.
- **New** `VideoFeederTest` against a fake `VideoSourceApi`:
  1. it does not call `next` while the target holds 2;
  2. `NEED_CAPACITY` grows the buffer and gets the same unit next;
  3. `CLOSED` ends the thread and calls `unclaim`;
  4. a failed `claim` reports and exits.
- Add `./gradlew :app:testDebugUnitTest` to the `flutter-syntax-check` CI job.
  No Gradle unit test runs in CI today.

### 7.6 On-device measurement procedure (matches `docs/validation/desktop-input-backpressure.md:190-226`)

**Setup:**

1. Host (Windows): run `deployed serve 600 --tcp --control --feedback-log`, so
   the trace includes `pb_mapper_desktop::qos=debug` and
   `pb_mapper_desktop::frames=trace` (per-frame `seq, frame_id, ready_us,
   written_us, bytes, keyframe`).
2. Run `video_bench 600 60 --pattern-only` for moving content.
3. Run `node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance`.
4. Phone:
   - `flutter build apk --release --target tool/desktop_viewer_acceptance.dart
     --dart-define=setupUrl=http://127.0.0.1:18747/controller.json
     --dart-define=seconds=120 --dart-define=quality=balanced
     --dart-define=reportPath=files/desktop-acceptance.json`;
   - `adb install -r`, then `adb reverse tcp:18747 tcp:18747`;
   - after the run,
     `adb exec-out run-as com.pb_mapper.ui cat files/desktop-acceptance.json > report.json`.

The report schema is 2. One sample per second comes from `viewer_status.stats`,
plus the host `Stats` and Flutter `FrameTiming`. A per-frame viewer trace
(`seq, received_us, handed_us, presented_us`) is correlated with the host
trace by `seq`. Each side reports durations on its own clock, and the network
leg is `rtt/2`.

**Cases:**

| Case | Conditions |
| --- | --- |
| A | direct Wi-Fi, Balanced |
| B | relay-only, Balanced |
| C | cellular (direct and relay) |
| D | Smooth on a Windows viewer at 90 Hz |
| E | background for 10 s, then resume, × 5 |
| F | decoder hints on vs off (A repeated) |
| G | UAC prompt and lock screen through the helper |
| H | RustDesk, same host, phone, network and content |
| I | `video_bench --pace 60,30,15,5` and `video_bench 120 60` on the host GPU (rate model and GOP) |

**Proposed pass criteria** for the user's decision. The merge block stays until
the user accepts (D28).

- **A:**
  - median `presented_fps` ≥ 54 on a display of 60 Hz or more;
  - `present_interval` p95 ≤ 25 ms and p99 ≤ 50 ms;
  - `gaps_250ms = 0` and `gaps_100ms ≤ 1` per minute after a 5 s warm-up;
  - `picture_latency_ms` p95 ≤ 80;
  - `keyframes = 1 +` the requested count;
  - host `input_wait_p99_us` ≤ 4000 (ordinary).
- **B and C:** no burst-and-stall pattern (no `gaps_250ms` while `rtt_ms` is
  stable within ±20%). fps may adapt, but resolution never changes.
- **E:** the picture is back within 1 s of resume. No reconnect, no keyframe
  storm (≤ 2 IDRs per resume).
- **G:** `waiting for remote desktop` without recovery churn. Input resumes
  after a fresh frame of the new generation.

---

## 8. Risks and implementation sequence

### 8.1 Failure-mode matrix (what handles it, and which test or measurement proves it)

| Failure | Behaviour | Owner | Proof |
| --- | --- | --- | --- |
| Packet loss on the single QUIC stream (HOL) | receipts and probes are delayed, so the window holds; the delay controller cuts fps; capture skips ticks (no discard, no IDR); the burst drains in the sink by wait age | credit, qos, sink | `slow_link` tests, case C |
| Reconnect | new `Viewer` channels, `sequence = 0`, Dart `_inputEpoch++`; the texture persists, a new sink is attached; no input replay | viewer, Dart | session test 1, Dart reconnect test |
| Backgrounding / minimise | `Pause`: host pace 0, control ended; native stall checks off; no keyframes, no reconnect; `Resume` emits an IDR | 3.8, 5.1, 6.4 | pause session test, Dart pause test, case E |
| Keyframe storm | host limiter (500 ms, deferral); viewer backoff (0.5 → 4 s); halved fps cap on backlog; decoder failure cap (5 in 30 s) | idr, sink | idr and sink tests |
| Slow decoder | sink backlog → fps cap at 90% / 80% of presented; drop to IDR only beyond 200 ms of wait | 3.4, 3.6 | `slow_link` slow consumer, case F |
| Frozen renderer | credit continues on receipt; input refused; no host teardown | auth | frozen-renderer session test |
| Secure desktop / UAC (helper) | binding generation bump → `invalidate` → `State(new gen)` + IDR; the sink drops the old generation; input needs a fresh frame of the new generation | worker, sink | helper tests, case G |
| Secure desktop (no helper) | `capture_unavailable{secure_desktop}`; no requests, no reconnect | worker, Dart | Dart test, case G |
| Helper upgraded mid-session | pipe EOF → sticky `helper_unavailable`; background hosts restart and viewers reconnect | 2.11 | manual |
| Old helper, new UI | `update_required` → "Update service"; v2 pipes never reach v1 | 2.11 | readiness unit test |
| Buggy/malicious viewer acks unsent frames | protocol error → `Stopped{protocol_error}` | credit | credit test |
| Input flood | lane 64 / outbound 32 → overflow revokes control, never drops a key-up silently | lane, outbound | worker and viewer tests |
| Revoke with input queued | B1 drops at the edge; C releases at the next iteration | 2.8 | worker test |
| Disconnect during a blocked `next` | `close()` wakes the waiter with `Closed`; stale ids return -1 | 4.5 | sink and FFI tests |
| Library loaded twice (Android) | claim fails loudly; instance tokens differ in the log | 4.5 | manual check (D9) |
| Encoder stall | existing 2 s / 3 s stall error → `capture_unavailable` → new encoder IDR | encoder | existing |

### 8.2 Risks and mitigations

| # | Risk | Mitigation |
| --- | --- | --- |
| R1 | The priority lane misses the `input_wait` budget (helper path: an IDR pipe transfer on the bound thread) | Measured in the trace and case A/G. **Escalation:** a dedicated injection thread per process with its own `DesktopBinding`, plus a process-global `BindingAuthority { name, EPOCH, console } → generation`, so both bindings report the same generation for the same triple. Helper IPC becomes two pipes (`.v2.worker.<n>.input`) paired by the service at spawn. Specified here but not built unless measurement requires it |
| R2 | The encoder rate model is not nominal | One-way latch (2.3) plus `video_bench --pace` per vendor |
| R3 | A vendor clamps the GOP | Fallback values plus readback; `keyframes` checked in case I. If clamped to N, accept one IDR per N frames (rare, and admitted from the reserve) and document it |
| R4 | Sync FFI on the UI isolate blocks | Only hot ops are sync; FFI test `hot_ops_never_wait_for_disconnect` |
| R5 | The ALPN bump strands PR #51 installs (helper v1, older phones) | Clean `VERSION_MISMATCH` message; `update_required`; release note |
| R6 | `PixelCopy` on a `SurfaceTexture` producer fails | D23 placeholder, verified on the device. Never blocks close |
| R7 | Receipt upstream overhead on cellular (~60 acks/s × ~70 B ≈ 4 KB/s) | Natural coalescing (latest `seq` in the slot). Measured in case C |
| R8 | Windows does not report minimise as `AppLifecycleState.hidden` | Verify. Fallback: the runner forwards `WM_SIZE/SIZE_MINIMIZED` on the existing `desktop_video` channel as a `visibility` event |
| R9 | Strict v2 parsing turns a host bug into a session drop | Strictness is intended (D1). The parser tests plus the session tests exercise every message |
| R10 | `jni` crate version or API drift | Pin `0.21.1` (in the lock, RustDesk precedent). The six JNI functions are thin wrappers over `VideoSink` |

### 8.3 Implementation sequence (every checkpoint builds and passes its narrow tests)

1. **Invite and pinning.** Add `invite.rs` and fingerprint `security::client`,
   keeping ALPN v1 for now. Add the FFI `inspect_invitation` op. Make
   `Host::invitation` emit pbd1, and make `connect` accept both formats.
   - *Check:* invite and security unit tests; `cargo test -p pb-mapper-ffi`;
     the app pastes either format.
2. **Protocol v2 core, Rust only.** `protocol.rs`, the v2 framing, ALPN v2,
   `credit.rs`, `qos.rs`, `idr.rs`, `outgoing.rs`, `auth.rs`, `lane.rs` and
   the session supervisors. Rewrite `host.rs`, `worker.rs` and `viewer/`
   (read/write tasks, `Outbound`, `VideoSink`, fps cap, recovery). Delete
   `congestion.rs` and `media.rs`.
   - **Transitional FFI:** keep `pb_mapper_desktop_wait_frame`, implemented as
     a claimed sink consumer for the Dart path, so the app still runs until
     step 4.
   - *Check:* all 7.1/7.2 Rust tests; `deployed selftest`; `viewer_probe`
     rewritten on the sink.
3. **Encoder and helper.** Worker pacer, `submit`/`pending_len`, infinite GOP,
   rate model, `video_bench --pace`. Helper IPC v2 (`Lock`, `pending`,
   `limits`), pipe renames, schema-2 install, `update_required`. Key
   vocabulary (D17) and `lock`. `HostStatus` (D24).
   - *Check:* case I on the dev GPU; helper unit tests; `input_acceptance`
     example.
4. **FFI hot path and exports.** `DesktopState` split, hot dispatcher, the 4.2
   ops, the 4.3 C exports, `desktop_jni.rs` (Android), the registry and
   error codes. Remove `wait_frame`, `frame` and `free_frame`.
   - *Check:* 7.3; `cargo ndk … build -p pb-mapper-ffi` for arm64.
5. **Native and Dart pipeline.** Kotlin `DesktopVideoNative`, `VideoFeeder`,
   and the `DesktopVideo` changes. Windows runner pull path. Dart
   `DesktopApi`/`DesktopStore` rewrite, the JPEG consumer over `next_v1`,
   pause/resume, and the polling and notification policy. Delete the frame
   isolate.
   - *Check:* `flutter test`; 7.5 Kotlin tests;
     `flutter build apk` and `flutter build windows`; a manual Windows run;
     Android smoke test.
6. **Snapshot and overlay plumbing.** Native `snapshot` (D23) and the
   `ViewerStats` overlay data. The acceptance tool moves to schema 2.
   - *Check:* the acceptance tool runs on Windows and on the phone.
7. **On-device measurement (7.6) and docs.** Update `docs/remote-desktop.md`
   (performance model, gestures handed off by the UI team),
   `desktop-ui-acceptance.md` (Android procedure) and `CLAUDE.md` (exports, JNI,
   desktop crate). The merge-block statements stay until the user accepts
   (D28).

### 8.4 Constants (single table)

| Constant | Value | Where |
| --- | --- | --- |
| `MAX_HEADER` / `MAX_PAYLOAD` | 8192 / 4 MiB | protocol |
| `IO_TIMEOUT` / `WRITE_FLOOR_BPS` | 3 s / 1 Mbit/s | wire |
| `PROBE_INTERVAL` / unresponsive after | 1 s / 2 s | qos |
| `UNRESPONSIVE_FPS` / `STARTUP_FPS_CAP` / `STARTUP_HOLD` | 2 / 15 / 1 s | qos |
| `DELAY_HISTORY` / `DELAY_FLOOR` | 3 / 10 ms | qos |
| `RTT_WINDOW` / `RTT_MIN_SAMPLES` / `RTT_ALPHA` | 60 / 10 / 0.5 | qos |
| delay bands | 50 / 100 / 150 / 200 / 300 / 600 ms | qos |
| `RATIO_INTERVAL` / dynamic | 3 s / ≥ 2 unique fps | qos |
| bitrate factors | 1.15 / 1.10 / 1.05 / 0.95 / 0.90 / 0.85 / 0.80; step ≤ +150 kbit/s | qos |
| latch | > 1.3 × expected for 2 windows when pace ≤ 0.75 × nominal | qos |
| `JPEG_MAX_FPS` | 8 | qos |
| `WINDOW_MARGIN` / `TIME_MIN` / `TIME_MAX` | 100 / 150 / 1000 ms | credit |
| `WINDOW_BYTES_MIN` / `FRAMES_MIN` / `FRAMES_MAX` | 64 KiB / 4 / 120 | credit |
| `IDR_RESERVE_MIN` / `MAX` | 64 KiB / 1 MiB | credit |
| `SUBMIT_GATE` / `OUTGOING_CAPACITY` / `OUTGOING_MAX_AGE` | 3 / 4 units, 8 MiB / 1 s | worker, outgoing |
| `HOST_IDR_MIN_INTERVAL` | 500 ms | idr |
| `LANE_CAPACITY` / `LEASE_RENEW_MIN` | 64 / 250 ms | lane, worker |
| cursor sample / `CURSOR_ECHO_SUPPRESS` | 16 ms (200 ms unavailable, none when paused) / 300 ms | worker, send |
| `input_wait` p99 budget | 4 ms ordinary, 12 ms helper | worker |
| `SINK_MAX_WAIT` / `UNITS` / `BYTES` | 200 ms / 32 / 8 MiB | sink |
| `PRESENTED_FRESH` / `PRESENTED_SEND_MIN_INTERVAL` | 2 s / 100 ms | sink, outbound |
| `KEYFRAME_MIN_INTERVAL` / backoff cap / hold after discontinuity | 500 ms / 4 s / 1 s | sink |
| `DECODER_FAILURE_LIMIT` | 5 in 30 s | sink |
| fps cap factors / min / step / up after | 0.9 direct, 0.8 relay / 5 / +5 / 3 s | viewer |
| `INPUT_QUEUE` (viewer) | 32 | outbound |
| `FEED_QUEUE_MAX` / feeder `next` timeout | 2 / 50 ms | Kotlin |
| `NO_VIDEO_RECONNECT` / Dart backoff | 5 s / 1, 2, 4, 8, 15 s | Dart |
| polls | viewer 250 ms, remote list 2 s, host 1 s (only when needed) | Dart |
| snapshot | ≤ 320 px wide JPEG q80, ≤ 300 ms | native |
