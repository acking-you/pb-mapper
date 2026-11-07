# CONTRACT-perf: remote desktop performance architecture (authoritative)

Status: authoritative implementation contract for BRIEF requirement 1 (P0-1 to
P2-1 and the regression guards) and DECISIONS D1-D14, D17, D23 and D24. It also
covers the pbd1 invite and pinning (D3), the helper upgrade (D2) and the
`DesktopStore`/`DesktopApiClient` surface that the UI consumes.

Amended after the adversarial review (2026-11-07). These sections now say
what the merged code does:

- 3 (`IDR_GRACE` row), 6.5 and 6.7: the sink drops nothing for age before
  the consumer's first IDR, and the fps cap judges the consumer only from
  `IDR_GRACE` after that first IDR (`first_idr_pulled_at`).
- 5.5 step 8.3, 5.7 and 13.1: a head ages from the later of `ready_at` and
  the last IDR receipt, and never while an IDR is in flight
  (`CreditLedger::aging_since`).
- 5.8 and 5.9.3: a 3 s window with only tainted samples steps bpf on their
  worst delay with `dynamic = false`, so it can only lower bpf.
- 5.10.2, 5.10.3 and 5.11: `IdrPolicy::on_emitted` became `answered(now)`,
  called when `capture_video` is handed a keyframe and again when one is
  emitted; a capture failure is fresh only when it ends streaming, retries
  run every `UNAVAILABLE_RETRY` and capture right after their context check,
  and a recovery on an unchanged context starts no generation.
- 5.16 item 3: the helper worker's wait for the next request is unbounded;
  the 3 s timeout bounds each answer and write.
- 5.16 item 5: an upgrade's SCM stop ends a background host's session with
  `Stopped{host_stopped}`, which is terminal; its viewer does not reconnect
  by itself.
- 6.10 and 9.6.2: `last_video_age_ms` counts from the later of the last
  video and the last entry into streaming.

Base: `design/perf-minimal.md`, the overall winner. Grafts come from
`design/perf-robust.md` and `design/perf-rustdesk.md`. Every must-fix raised
by the three judges is resolved, and section 1 traces each one.

Code citations refer to the worktree at `a41eaa7` and were re-read for this
contract. Path shorthands:

- `D/` = `crates/pb-mapper-desktop/src/`
- `F/` = `ui/native/pb_mapper_ffi/src/`
- `U/` = `ui/lib/src/`
- `K/` = `ui/android/app/src/main/kotlin/com/pb_mapper/ui/`
- `W/` = `ui/windows/runner/`

Normative words: **must** and **never** are contract. Anything this document
leaves open belongs to the implementing work package (section 12). If a rule
turns out to be unworkable, raise it with evidence, as DECISIONS requires.
Never deviate silently.

Out of scope here, and owned by the UI contract:

- page and widget layout;
- icons and the title bar;
- the QR and camera flows;
- the remembered-computer vault (D20), apart from the integration point in
  9.2.

---

## 0. The model in one page

1. **Send credit comes from transport receipts, never from presentation.**
   The viewer's network read task writes a cumulative `Received{seq}` as soon
   as a video record has been read. That covers frames the sink later drops.
   The host admits a frame against a window of `pace x (base_rtt + 100 ms)`
   frames and a byte budget. That budget has room for one IDR and is clamped by
   the measured delivery rate when receipts show queueing.
2. **Capture runs on an even grid at the QoS pace.** It never pauses or
   resumes as flow control. A tick is consumed whether it fires or is skipped.
   It is skipped only while 2 or more units are already buffered on the host
   (hand-off queue plus encoder in flight).
3. **QoS follows RustDesk `video_qos.rs`, with fixes.**
   - A 1 s in-stream probe is answered by the viewer's network task.
   - The baseline is the minimum of the last 60 RTT samples, and it resets on
     a path change.
   - Probes sent behind an in-flight IDR are tainted: they update the RTT but
     not the fps controller.
   - When the viewer is unresponsive, output drops to 2 fps without poisoning
     the controller's history.
   - The controller works in **bits per frame**. A one-way latch handles
     encoders that budget from timestamps.
   - Resolution never adapts.
4. **Infinite GOP.** IDRs are sent only on demand and are rate-limited on both
   sides. Requests are deferred and coalesced, never dropped.
5. **The viewer owns presentation lag.**
   - One `VideoSink` per session, with exactly one native consumer.
   - When the oldest queued unit is older than 250 ms, the sink drops to the
     next IDR, with a 500 ms grace period after an IDR.
   - An arriving IDR supersedes everything queued.
   - The viewer sends a backlog-gated fps cap (90% direct, 80% relay).
   - Rust owns decoder recovery, through a state machine with a shared limiter.
6. **H.264 never crosses Dart.**
   - Android: a Kotlin feeder thread pulls through JNI into pooled direct
     buffers.
   - Windows: the runner's decoder thread pulls through C exports.
   - Native code reports presentation and keyframe needs straight to Rust.
   - Dart reads status through a sync hot export and a per-vsync 56-byte view
     leaf.
7. **Input is authorised only by presentation.**
   - Only a validated `Presented` (viewer `VideoSink` plus host `AuthLedger`)
     admits input.
   - Receipts, input and cursor traffic never renew or grant input rights.
   - Input is drained with priority on the single desktop-bound thread, and the
     receive task never waits for it.

---

## 1. Resolution log

### 1.1 Disagreements between the designs

| # | Topic | minimal / robust / rustdesk | **Decision** | Why |
| --- | --- | --- | --- | --- |
| R1 | Receipt key | `(gen, frame_id)` / transport `seq` / `frame_id` | **Transport `seq`** (1, 2, 3 … per session, every `Video`) | No authorisation API can accept a seq, which makes D4 structural (judge 2). It also enables the strict `seq == last+1` check and per-frame trace correlation (judge 1 graft). |
| R2 | Framing | 1 JSON record, plus 1 payload record for Video only / always 2 records / typed binary records | **minimal's**: every message is one JSON record; `Video` alone is followed by exactly one payload record; **one flush per message**; payload read zero-copy into `bytes::Bytes` | Least code, and strict serde. It also removes today's empty record per message (`D/wire.rs:65-76`). |
| R3 | Pause | `FpsCap{0}` / explicit `Pause`/`Resume` / explicit | **Explicit `Pause`/`Resume`**. The fps-cap controller is gated on `!paused`. | Must-fix: with the minimal design, the cap controller un-paused a hidden viewer. |
| R4 | Lock (D17) | `Input::Lock` kind / bare `Lock` message / `Lock{gen,frame,seq}` | **`Input::Lock`** inside `InputEvent`. It shares the sequence, generation, presented-frame check and helper forwarding. | Must-fix (judge 2): lock needs the input discipline. As an input kind it gets that by construction. |
| R5 | Handshake reply | none / `Accepted`/`Rejected` / `Hello`/`Stopped` | **`Accepted`/`Rejected`**, plus `Stopped` for ends during a session | Gives structured BUSY/DENIED (judge 3 graft). Busy is revealed only after the token checks (no oracle). |
| R6 | `deny_unknown_fields` | dropped on `StatusMessage` / everywhere / everywhere | **Everywhere.** Both sides send capability lists. | Required by D1 (must-fix on minimal). |
| R7 | Host pacer owner | encoder-internal / worker / backend + host loop | **Host worker (thread C) is the single owner.** The encoder's internal `next_capture` pacing is deleted. The helper never paces. | Must-fix (judge 3): rustdesk left the owner undefined. Two clocks skip early ticks. |
| R8 | Pacer step | `max(next+i, last+0.75i)` / `now+period` after a stall / `max(next+i, now)` | **`next = max(next + period, now + period*3/4)`**, consumed on fire or skip | Keeps the long-run grid, and two fires are never closer than 0.75 periods (judge 1 graft). |
| R9 | Submit gate | `outgoing < 2` / `outgoing + encoder < 3` / `outbound + encoder < 2` | **`outgoing.len() + backend.pending_video() < 2`**. `outgoing` includes the head that B-send holds. | Judge 1 must-fix: at most 2 stale units ever wait behind a closed window. |
| R10 | D6 encoder rate | nominal = preset ceiling, rebuild on preset change / nominal = `limits.max_fps`, bpf plus latch / compensation ×4 | **nominal = `limits.max_fps`. The controller works in bits per frame (bpf). The `Nominal` rate model is the default, and a one-way `Timestamp` latch covers the other encoder type.** Preset changes never rebuild. | Judge 1 graft and must-fix. With either MFT rate model, an fps cut sheds bandwidth. Compensation is rejected. |
| R11 | RTT baseline | min of 60 samples / RttCalculator with all-time min / RttCalculator with MIN_SAMPLES 3 | **min of the last 60 samples, from the first sample. Reset on a viewer `Path` change.** | Must-fix: an all-time min stays biased after a path change, and a raw RTT before 3 samples collapses fps. |
| R12 | Unresponsive | 2 fps output, state untouched / feeds elapsed into the history / same | **2 fps output only. History and fps state are untouched. Clears on the next receipt or probe echo.** | Must-fix: poisoned history takes 15-25 s to recover. |
| R13 | Delay sensing between probes | probe only (all three) | **Probe for fps and bitrate (D5), plus a transport ack-queue clamp on the byte window.** | Judge 1 must-fix for all designs. |
| R14 | Step sizes | unscaled / unscaled / ×`ceiling/30` | **Scaled** (`small = round(scale)`, `quick = round(5·scale)`). Thresholds and `min_fps` stay unscaled. | Reaches 60 fps in about 4-6 s (judged strength). |
| R15 | Presets | see designs | **Table 5.9.1** (Balanced: 4 Mbit/s at 60 fps start, 8 Mbit/s max, so it probes upward) | P0-4 and D12. Must-fix: rustdesk's ceiling equalled its start. |
| R16 | fps cap | backlog-gated presented fps / max queue + halving / capacity meter | **minimal's backlog-gated rule.** A single event lowers the cap by at most 50%. Gated on `!paused`. | Judge 1 must-fix: the rustdesk meter is vsync-bound, and robust over-reacts. |
| R17 | Viewer backlog | 400 ms age on a 250 ms tick / 200 ms wait at push / 250 ms age, ≥3 units, 500 ms IDR grace | **250 ms age, checked at push and pull, ≥ 3 units, 500 ms grace after an IDR is pulled. An arriving IDR supersedes the queue.** | Judge 1 and 3 grafts and must-fixes. |
| R18 | Discontinuity | n/a / rebuild MediaCodec / reset | **Never rebuild for a same-size discontinuity.** Feed the IDR, drop native queued units, and bump the output revision. Rebuild only on a size change, when no codec exists, or after a decoder error. | Judge 1 must-fix: rebuild, IDR and rebuild loops. |
| R19 | Video ABI | copy into caller buffer, drop on too-small / lent `Arc` pointer / copy, keep on too-small | **Copy into a caller buffer. `NEED_CAPACITY` leaves the unit queued. One code space shared by C and JNI.** | Must-fix: a fat pointer in `void*` causes double free and dropped units. |
| R20 | JNI buffers | new `byte[]` per frame / pooled direct / pooled direct | **3 pooled direct `ByteBuffer`s, 1 MiB each, growable to 4 MiB** | Must-fix: GC on the hot path. |
| R21 | Pause to native | none / `setPaused` channel / `PAUSED` return code | **`PAUSED` return code from `next`.** No `setPaused` channel. | One source of truth (judge 2 graft). It replaces judge 3's `setPaused` graft: the same effect without a second channel that can disagree. |
| R22 | Receive clock for native | none / none / `received_ns` | **`age_us` (time since receipt, at pop)** | Gives the same value with no cross-language clock mapping and no new `libc` dependency. |
| R23 | Cursor and status hot path | `cursor_v1` plus 4 Hz JSON / `cursor_v1` / per-vsync `view_v1` leaf | **`pb_mapper_desktop_view_v1` leaf (56 B), plus a sync JSON `viewer_status` at ≤ 2 Hz** | Judge 3 graft. Fixed layout, no JSON per frame. |
| R24 | Hot ops export | own closed-set export / `is_hot()` inside the shared export / whitelist | **Own export `pb_mapper_desktop_viewer_call_json`** with a closed op set and no `block_on` | Judge 2 graft. |
| R25 | Input thread (D10) | priority on C / priority lane on C / dedicated thread | **Priority lane on C.** The dedicated thread is a documented escalation, gated on measured `input_wait`. | Judge 3 must-fix: rustdesk builds it unconditionally at the secure-desktop boundary. |
| R26 | `input_wait` gate | p99 10/20 ms / 4/12 ms / none | **p99 ≤ 5 ms (ordinary), ≤ 15 ms (helper)** | 5 ms is about a third of a 60 fps frame. 15 ms is one frame, which covers one pipe round trip carrying an IDR. |
| R27 | Host IDR interval | 250 / 500 / 300 ms | **300 ms. Deferred, coalesced, never dropped.** | RustDesk-like. Robust semantics. |
| R28 | Write timeout | 3 s fixed / `3 s + bytes·8/1 Mbit/s` / 5 s fixed | **Size-scaled**, on both sides, for writes and payload reads | Must-fix: a large IDR on cellular. |
| R29 | Generation mismatch on the viewer | ack and drop / protocol error / ack and drop older, error on newer | **Ack and drop older. A newer generation that was never announced is a protocol error.** The host also purges stale-generation units before admission. | Must-fix: a control grant with a queued unit killed the session in robust. |
| R30 | `media::Queue` | reuse / delete / delete | **Delete `D/media.rs`.** Two concrete types replace it: host `Outgoing` and viewer `VideoSink`. | Must-fix: reuse was underspecified. |
| R31 | Dart watchdog | keep 700 ms / 3 s / delete / delete | **Delete.** Rust has a 700 ms stall watchdog behind the limiter. Dart reconnects only on transport loss or 5 s without video. | Must-fix. |
| R32 | Helper upgraded mid-session | n/a / waits forever / n/a | **Sticky helper loss ends the session with `Stopped{capture_failed}`, and Dart reconnects.** | Must-fix (judge 2). |
| R33 | Encoder limits on the wire | `VideoPreferences` incl. bitrate / `VideoLimits` / `VideoLimits` | **`VideoLimits{max_width,max_height,max_fps}`.** Bitrate leaves the wire, because presets own it. | — |
| R34 | Who computes limits | Rust from `refresh_hz` / Dart / Dart | **Rust, from `refresh_hz`** (one place, testable) | — |
| R35 | DesktopStore names | two UI designs disagree | **9.5 is authoritative.** Section 9.7 maps both UI designs onto it. | — |

### 1.2 Must-fix traceability (every judge item, and where it is fixed)

| Must-fix (judge) | Fixed in |
| --- | --- |
| rustdesk fps cap measured from vsync-bound presentation (J1) | 6.7 (backlog-gated, never a capacity meter) |
| rustdesk rate compensation keeps bps constant (J1) | 5.9.3 (bpf plus latch, no compensation) |
| robust: discontinuity rebuilds MediaCodec (J1) | 6.5 (flag semantics), 8.1.4 (configure rule), 8.2 |
| robust: cap lowered on max queue for 2 s, halved, 25 s recovery (J1) | 6.7 |
| minimal: CBR clamp cannot fire for timestamp encoders (J1) | 5.9.3 latch |
| minimal: JNI `byte[]` per frame; `-3` drops the unit (J1, J2) | 7.5, 7.6, 8.1.2 |
| minimal: gate ignores pending and encoder; 650 ms stale sink; no IDR grace (J1) | 5.10.2, 6.5 |
| minimal: Dart 3 s not-fresh reconnect (J1, J2) | 9.6.2 |
| rustdesk: raw RTT before 3 samples; Balanced ceiling = start (J1) | 5.8, 5.9.1 |
| rustdesk and robust: unresponsive poisons the history (J1) | 5.9.4 |
| RttCalculator all-time min (J1) | 5.8 (window min, `Path` reset) |
| all: probe-only delay sensing (J1) | 5.7.3 ack-queue clamp |
| all: host input admission without presentation evidence (J2) | 5.12 `AuthLedger.acked`; 6.4 presented-before-input flush |
| robust: `Received.seq > last_sent` race (J2, J3) | 5.7.2 (book at admission, before the write) |
| robust: fat-pointer `_owner` in the C ABI (J2, J3) | 7.5 (copy into the caller buffer) |
| robust: helper reinstall leaves the viewer stuck (J2) | 5.16.4 |
| robust: `Lock` lacks discipline (J2) | 4.3 `Input::Lock` |
| minimal: pause via `FpsCap{0}` (J2) | 4.3 `Pause`/`Resume`; 6.7 gate |
| minimal: Kotlin stall checks while paused (J2) | 8.1.3 (`PAUSED` code) |
| minimal and rustdesk: fixed write timeout; minimal drops `deny_unknown_fields` (J2, J3) | 4.2, 4.3 |
| rustdesk: dedicated input thread; pause does not end control; single-instance check only in debug (J2, J3) | 5.12.5 (escalation only), 5.4 (`Pause` ends control), 7.6/8.1.1 (fail closed) |
| minimal: Kotlin permit leak (J3) | 8.1.2 (condition wait; buffer returned on every path) |
| minimal: missing boundary tests (J3) | 10 |
| minimal: `media::Queue` underspecified (J3) | 5.6, 6.5 |
| minimal and robust: book in-flight before the write (J3) | 5.7.2 |
| robust: old-generation video is a protocol error (J3) | 4.6, 5.5 |
| robust: step-2 big bang; transitional FFI unspecified (J3) | 7.9 shim; 12 gates |
| robust: two return-code spaces (J3) | 7.5 |
| rustdesk: pacer owner (J3) | 5.10 |
| rustdesk: step 2 deletes files the worker is built on (J3) | 12 (deletions happen at gates) |

---

## 2. Ownership, threads and data paths

### 2.1 One owner per concern

| Concern | Single owner |
| --- | --- |
| Send credit, probe, RTT baseline, fps/bpf QoS | Host session network tasks: `credit.rs`, `qos.rs` (a `SessionNet` shared by B-recv and B-send) |
| Capture pacing, encoder, desktop binding, input injection, cursor sampling | Host worker thread C (`worker.rs`, `pacer.rs`) |
| Input authorisation ledger (generation, produced frames, presented evidence) | `AuthLedger`, written by C (production, invalidate) and B-recv (`Presented`). Read by C. |
| IDR decisions on the host | `IdrPolicy` (one per session) |
| Viewer queue, backlog and overflow policy, presentation validation, keyframe limiter, recovery, fps cap | `VideoSink` (viewer) |
| Viewer outbound priorities | `Outbound` (viewer write task) |
| UI state | `DesktopStore`. It observes Rust status and owns no media and no presentation ACKs. |

### 2.2 Host tasks and threads (per session)

| Id | Runs on | Owns | Reads and writes |
| --- | --- | --- | --- |
| A | accept task | listener, `JoinSet ≤ 8`, `Semaphore(1)`, handshake | spawns the session supervisor |
| S | session supervisor (tokio) | `select!{cancel, B-recv, B-send, worker exit}` | writes `Stopped` (best effort, 1 s) |
| B-recv | tokio task, TLS read half | `ViewerMessage` decode | `SessionNet` (credit, qos), `AuthLedger`, `IdrPolicy`, `InputLane.push`, `pointer_input_at_us`, `Targets` |
| B-send | tokio task, TLS write half | `CursorGate`, probe/stats timers, QoS tick (250 ms) | `Outgoing` (peek/pop), `SessionNet`, `state`/`cursor` watches |
| C | `spawn_blocking` std thread | `Box<dyn Backend>` (encoder, Enigo, desktop binding), `Engine`, `Pacer`, `Waiter` | `InputLane.drain`, `Outgoing.push`, `AuthLedger` (production, invalidate), `IdrPolicy`, `Targets` (read), watches (send) |
| E | helper pipe runtime inside C (privileged only) | named pipe | — |

The TLS stream is split with `tokio::io::split`. A blocked write of a large
IDR never delays B-recv.

### 2.3 Viewer tasks

`viewer-read` and `viewer-write` run as two tasks over split halves, under a
supervisor: `select!{cancel, read, write}`, then `sink.close()`, then
`shared.mark_stopped(reason)`. `viewer-write` also runs the 250 ms viewer tick:
fps cap, watchdog, path re-sample and stats.

### 2.4 Data paths

```
video:    C encode -> Outgoing(≤4) -> B-send admit(credit) -> TLS -> viewer-read -> Received{seq} (first)
          -> VideoSink.push -> native consumer next() -> decoder -> compositor
receipt:  viewer-read -> Outbound.receipt -> viewer-write -> B-recv -> CreditLedger.on_receipt     (credit only)
probe:    B-send Probe{n} (in stream order) -> viewer-read -> Outbound.probe -> ProbeEcho{n} -> B-recv -> Qos
present:  compositor -> native presented() -> VideoSink.presented (validate) -> Outbound -> Presented
          -> B-recv AuthLedger.on_presented (validate)                                          (input auth only)
input:    Dart sync op -> Viewer.input (fresh check, stamp frame_id+seq) -> Outbound FIFO
          -> B-recv (policy edge check) -> InputLane -> C Engine::input (AuthLedger.admits) -> backend
cursor:   C sample 16 ms -> watch -> B-send CursorGate (300 ms echo suppression) -> viewer-read -> cursor slot
          -> Dart view_v1 per vsync
```

---

## 3. Constants (the only table; code must use these names)

| Name | Value | Owner |
| --- | --- | --- |
| `ALPN` | `b"pb-mapper/desktop/2"` | security.rs |
| `MAX_HEADER` / `MAX_PAYLOAD` | 8192 B / 4 MiB | wire.rs |
| `IO_TIMEOUT` / `WRITE_FLOOR_BPS` | 3 s / 1_000_000 | wire.rs |
| `HANDSHAKE_TIMEOUT` (host reads `Authenticate`) / `ACCEPT_TIMEOUT` (viewer waits for `Accepted`) | 3 s / 5 s | host.rs, viewer |
| `REJECT_WRITE_TIMEOUT` / `STOP_WRITE_TIMEOUT` | 1 s / 1 s | host.rs, session.rs |
| `HOST_READ_TIMEOUT` / `VIEWER_HEADER_TIMEOUT` | 3 s / 4 s | session.rs, viewer |
| `PING_IDLE` / `STATS_INTERVAL` | 1 s / 1 s | viewer, session.rs |
| `INITIAL_RTT` / `WINDOW_MARGIN` | 100 ms / 100 ms | credit.rs |
| `FRAME_WINDOW_MIN..MAX` | 3..=64 | credit.rs |
| `BYTE_WINDOW_MIN..MAX` / `BYTE_WINDOW_CLAMP_MIN` | 64 KiB..=4 MiB / 32 KiB | credit.rs |
| `ACK_QUEUE_LIMIT` / `ACK_SAMPLE_WINDOW` / `DELIVERY_WINDOW` / `DELIVERY_FACTOR` | 100 ms / 250 ms / 500 ms / 1.25 | credit.rs |
| JPEG window | 4 frames / 2 MiB | credit.rs |
| `SUBMIT_GATE` | 2 (outgoing + encoder pending) | worker.rs |
| `OUTGOING_CAPACITY` / `OUTGOING_BYTES` / `OUTGOING_MAX_AGE` | 4 units / 8 MiB / 1 s | outgoing.rs |
| `PROBE_INTERVAL` / `UNRESPONSIVE_AFTER` / `UNRESPONSIVE_FPS` | 1 s / 2 s / 2 | qos.rs |
| `DELAY_FLOOR` / `DELAY_HISTORY` / `RTT_WINDOW` | 10 ms / 3 / 60 samples | qos.rs |
| delay bands | 50 / 100 / 150 / 200 / 300 / 600 ms | qos.rs |
| `QOS_TICK` / `RATIO_INTERVAL` / `DYNAMIC_UNIQUE_FRAMES` | 250 ms / 3 s / 6 per window | qos.rs |
| bpf factors | ×1.15 / 1.10 / 1.05 (dynamic only) / 0.95 / 0.90 / 0.85 / 0.80; increase ≤ +150 kbit/s | qos.rs |
| `LATCH_OVERSHOOT` / `LATCH_WINDOWS` / `LATCH_PACE_RATIO` | 1.3 / 2 / 0.75 | qos.rs |
| `STARTUP_HOLD` / startup fps | 1 s / `min(normal, round(15·scale))` | qos.rs |
| `JPEG_MAX_FPS` | 8 | qos.rs |
| encoder bitrate clamp / `pixel_scale` clamp | 256_000..=40_000_000 / 0.25..=4.0 | qos.rs |
| `HOST_IDR_MIN_INTERVAL` | 300 ms | idr.rs |
| GOP candidates | `u32::MAX`, `0x7FFF_FFFF`, `65_535`, `3_600` (never 0) | video/windows.rs |
| `LANE_CAPACITY` / `LEASE_RENEW_MIN` | 512 (≥ `INPUT_QUEUE`: a full viewer burst can arrive during one backend call, 6.4) / 250 ms | lane.rs, worker.rs |
| `PRESENTED_FRESH` / `RECENT_FRAMES` | 2 s / 256 | auth.rs, sink |
| `CONTEXT_CHECK_INTERVAL` / `UNAVAILABLE_RETRY` | 100 ms / 200 ms | worker.rs |
| `DRAIN_POLL` (ordinary, while encoder pending) / `HELPER_DRAIN_WAIT_MAX` | 2 ms / 4 ms | worker.rs, helper |
| cursor sampling | 16 ms streaming, 200 ms unavailable, none while paused | worker.rs |
| `CURSOR_ECHO_SUPPRESS` / `NOTICE_TTL` | 300 ms / 5 s | session.rs, worker.rs |
| `input_wait` gate | p99 ≤ 5 ms ordinary, ≤ 15 ms helper | acceptance |
| `SINK_MAX_UNITS` / `SINK_MAX_BYTES` | 32 / 8 MiB | video_sink.rs |
| `BACKLOG_AGE` / `BACKLOG_MIN_UNITS` / `IDR_GRACE` | 250 ms / 3 / 500 ms (`IDR_GRACE` also delays the fps cap's judgment from the first IDR pull, 6.7) | video_sink.rs |
| `PRESENTED_SEND_MIN_INTERVAL` | 100 ms (bypassed for input, see 6.4) | outbound.rs |
| `INPUT_QUEUE` / `INPUT_QUEUE_BYTES` / `INPUT_BATCH_MAX` | 512 events / 256 KiB (encoded `Input` JSON) / 16; a single `Move` replaces a queued tail `Move` (6.4) | outbound.rs |
| `KEYFRAME_MIN_INTERVAL` / backoff | 500 ms / ×2 up to 4 s | recovery.rs |
| `HOLD_AFTER_DISCONTINUITY` / `STALL_WATCHDOG` | 1 s / 700 ms | recovery.rs |
| `DECODER_FAILURE_LIMIT` | 5 in 30 s without a presented frame | recovery.rs |
| fps cap | tick 250 ms; backlog > 2 on 2 ticks; factor 0.9 direct / 0.8 relay; min 5; +5 after 3 s with backlog ≤ 1; ≥ 1 s between changes; ≤ 50% fall per event | fps_cap.rs |
| degraded | (`queue_delay ≥ 150` ∨ `picture_latency ≥ 250` ∨ (`pace ≥ 10` ∧ `presented < 0.5·pace`)) for ≥ 3 s; clears after 3 s | viewer status |
| `FEED_QUEUE_MAX` / feeder `next` timeout / buffer pool | 2 / 50 ms / 3 × 1 MiB, grows to 4 MiB | Kotlin |
| snapshot | ≤ 320 px wide, JPEG q80, ≤ 300 ms, never blocks close | native |
| `NO_VIDEO_RECONNECT` / Dart backoff / `PROTOCOL_RETRY_LIMIT` | 5 s / 1, 2, 4, 8, 15 s / 3 consecutive attempts that end in `protocol` | Dart |
| Dart polls | `viewer_status` 500 ms with a viewer demand, 1 s otherwise with a session; host 1 s with a page demand, 2 s while sharing; none otherwise | Dart |
| Dart move coalescing / cursor timer | ≤ 1 move per 4 ms / 33 ms | Dart |

---

## 4. Wire protocol v2

### 4.1 Stack and version gate

The stack below TLS is unchanged:

- a pb-mapper TCP service (`codec: true`) over a relay or direct QUIC;
- loopback TCP with `TCP_NODELAY`;
- TLS 1.3 with SNI `desktop.pb-mapper.invalid` and no early data.

Version gate:

- `security::ALPN = b"pb-mapper/desktop/2"`. Each side offers only v2.
- On the viewer, `rustls::Error::AlertReceived(AlertDescription::NoApplicationProtocol)`
  or `PeerIncompatible(_)` maps to `ConnectError::VersionMismatch`. The FFI
  code is `VERSION_MISMATCH`, which is terminal. The message is "The other
  computer runs a different pb-mapper version. Update pb-mapper on both
  computers."

### 4.2 Framing and timeouts (`D/wire.rs`)

- A record is `u32 BE len | bytes`.
  - Headers are JSON, ≤ `MAX_HEADER`.
  - Payloads are 1..=`MAX_PAYLOAD`.
- Every message is one JSON record. Only `HostMessage::Video` is followed by
  exactly one payload record.
- A payload after any other message, or a zero-length payload, is a protocol
  error.

```rust
pub(crate) async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, header: &T, payload: Option<&[u8]>) -> Result<()>;
// Serialises the header, writes len+header (+ len+payload) into one BufWriter and flushes ONCE.
// The whole call runs under write_timeout(header_len + payload_len).
pub(crate) fn write_timeout(bytes: usize) -> Duration { IO_TIMEOUT + Duration::from_secs_f64(bytes as f64 * 8.0 / WRITE_FLOOR_BPS as f64) }
pub(crate) async fn read_header<R, T: DeserializeOwned>(r: &mut R, timeout: Duration) -> Result<Option<T>>; // None = clean EOF between messages
pub(crate) async fn read_payload<R>(r: &mut R) -> Result<bytes::Bytes>;   // timeout = write_timeout(len); Vec::with_capacity(len) -> Bytes (no copy)
```

- `D/Cargo.toml` adds `bytes.workspace = true`. Version 1.11 is already in the
  workspace.
- `clean_tls_close_is_eof_only_between_messages` is kept, ported to the v2
  framing.

### 4.3 Viewer → host (`D/protocol.rs`)

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ViewerMessage {
    /// First and only first message. Host answers Accepted or Rejected.
    Authenticate {
        token: String,                  // 43-char base64url of the 32-byte token (constant-time compare, as today)
        codecs: Vec<VideoCodec>,        // 1..=2
        limits: VideoLimits,            // session constant
        quality: QualityPreset,         // initial preset
        capabilities: Vec<String>,      // v2.0 viewer sends []
    },
    /// Cumulative transport receipt: every Video with seq <= this was fully read.
    /// Send credit ONLY. Grants, renews or proves nothing about input.
    Received { seq: u64 },
    ProbeEcho { nonce: u64 },           // echo of HostMessage::Probe, written by the viewer network task
    /// Newest frame actually presented and validated by VideoSink. Input authorisation ONLY.
    Presented { generation: u64, frame_id: u64 },
    Keyframe { reason: KeyframeReason },
    FpsCap { fps: u32 },                // 1..=120, runtime limit separate from limits
    Quality { preset: QualityPreset },  // D12 live, never a reconnect, never a rebuild
    Pause,                              // D13: host ends control, stops capture
    Resume,                             // D13: host forces an IDR and restarts QoS startup
    Path { route: Route },              // sent on change: resets the host RTT baseline
    Control { enabled: bool },          // ordered input lane
    Release,                            // ordered input lane
    Input { event: InputEvent },        // ordered input lane; includes Input::Lock (D17)
    Ping,                               // only after PING_IDLE without any other message
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route { Direct, RelayTcp, RelayQuic }
```

### 4.4 Host → viewer (`D/protocol.rs`)

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HostMessage {
    Accepted { codec: VideoCodec, limits: VideoLimits, nominal_fps: u32, quality: QualityPreset, capabilities: Vec<String> },
    Rejected { reason: RejectReason },                 // written only after the token check, 1 s timeout, then close
    State {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<StateReason>,
        message: String,                               // human text; UI keys off `reason`
        control_allowed: bool,
        controlling: bool,
        generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")] display: Option<Display>,   // physical px, input space
        desktop: DesktopKind,
        #[serde(default, skip_serializing_if = "Option::is_none")] video: Option<VideoSize>,   // encoded size of this generation
    },
    /// Followed by exactly one payload record (Annex B access unit or JPEG).
    Video {
        seq: u64,                // 1, 2, 3 … per session; assigned at admission (5.7.2)
        generation: u64,
        frame_id: u64,           // monotonic for the session, never reused
        keyframe: bool,
        duplicate: bool,         // static-desktop liveness re-encode
        width: u32, height: u32,
        pts_us: u64,
        host_us: u32,            // encoder submit -> socket write start (encode + hand-off + credit wait)
    },
    Cursor { generation: u64, x: i32, y: i32, visible: bool },
    Probe { nonce: u64 },        // 1, 2, 3 …; one outstanding; in stream order
    Stats {                      // every STATS_INTERVAL; also the host->viewer liveness
        capture_fps: f32, pace_fps: u32, target_fps: u32, fps_cap: u32 /*0 = none*/,
        bitrate: u32 /*expected = bpf*pace*/, encoder_bitrate: u32, sent_bitrate: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")] rtt_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")] base_rtt_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")] queue_delay_ms: Option<u32>,
        ack_queue_ms: u32, dropped_frames: u64, skipped_ticks: u64, keyframes: u64,
        frames: u64, bytes: u64, adaptation: Adaptation, rate_model: RateModel, gop: u32,
    },
    Notice { code: NoticeCode, message: String },
    Stopped { reason: StopReason, message: String },   // final, best effort (STOP_WRITE_TIMEOUT)
}

#[derive(..., Serialize, Deserialize)] #[serde(rename_all = "snake_case")] pub enum RejectReason { Denied, Busy, InvalidRequest }
#[serde(rename_all = "snake_case")] pub enum SessionState { Connecting, Streaming, Paused, CaptureUnavailable }
#[serde(rename_all = "snake_case")] pub enum StateReason { SecureDesktop, HelperUnavailable, CaptureFailed, InputFailed, ControlDenied, InputCongestion, StalePresentation, DisplayLost }
#[serde(rename_all = "snake_case")] pub enum DesktopKind { Ordinary, Secure }
#[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)] pub struct VideoSize { pub width: u32, pub height: u32 }
#[serde(rename_all = "snake_case")] pub enum Adaptation { Startup, Steady, Raising, LoweringFps, LoweringBitrate, Unresponsive, Paused }
#[serde(rename_all = "snake_case")] pub enum RateModel { Nominal, Timestamp }
#[serde(rename_all = "snake_case")] pub enum NoticeCode { Locked, LockFailed }
#[serde(rename_all = "snake_case")] pub enum StopReason { HostStopped, HostEnded, Restarting, Revoked, CaptureFailed, ProtocolError }
```

Every enum variant is an inline struct, because internally tagged newtype
variants combined with `deny_unknown_fields` are fragile in serde. Every
struct is `deny_unknown_fields`.

### 4.5 Shared public types (`D/lib.rs`)

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset { Quality, #[default] Balanced, Smooth }

/// Renderer limits, fixed for the session. Replaces VideoPreferences (D/lib.rs:178-208) everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoLimits { pub max_width: u32, pub max_height: u32, pub max_fps: u32 }
impl VideoLimits {
    pub fn validate(&self) -> Result<()>;            // width 2..=3840, height 2..=2160, fps 30..=120
    /// The one place limits are derived (R34). Android: 1920x1080, fps = clamp(round(refresh_hz), 30, 60);
    /// other platforms: 1920x1080, fps = clamp(round(refresh_hz), 30, 120). Non-finite/<=0 refresh -> 60.
    pub fn for_viewer(refresh_hz: f64) -> Self;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason { DecoderError, Gap, Backlog, Stall, User }

/// Per-call encoder rate (replaces the bare `bitrate: u32` of capture_video).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRate { pub bitrate: u32, pub pace_fps: u32 }

pub enum Input {                 // tag "kind", rename_all snake_case, deny_unknown_fields (unchanged)
    Move { x: u32, y: u32 },
    Button { button: Button, down: bool, /* + drag_start (serde default, skipped when false): WORKPLAN §1.1 R-UI-3 */ },
    Wheel { horizontal: i32, vertical: i32 }, Key { key: Key, down: bool }, Text { text: String },
    /// D17: LockWorkStation in the interactive session. Control-gated exactly like other input.
    Lock,
}
pub struct InputEvent { pub generation: u64, pub frame_id: u64, pub sequence: u64, pub input: Input } // + deny_unknown_fields
```

`Key` (D17), with snake_case serde names:

- existing: `shift control alt meta enter escape tab backspace delete left
  right up down home end page_up page_down`;
- `a`..`z`, which replaces the A/C/V/X/Z subset;
- `digit0`..`digit9`;
- `f1`..`f12`;
- `insert`, `space`.

`meta` is the Win key. Windows `key_code` (`D/windows.rs:538-563`) maps:

- letters to the Windows-only virtual-key variants `enigo::Key::A..Z` and
  digits to `enigo::Key::Num0..Num9`, **never** `Key::Unicode`. On a host
  layout that cannot produce the character (Russian, Greek, Arabic …),
  enigo 0.6.1 falls back from `Key::Unicode(c)` to `KEYEVENTF_UNICODE` text
  and emits a down **and** an up for every call
  (`enigo-0.6.1/src/win/win_impl.rs:435-440, 475-500`), so Ctrl+C, Win+E and
  the latched taps would type stray letters instead. Shortcuts use virtual
  keys; text still goes through `Input::Text`.
- `F1..F12`, `Insert` and `Space` to the matching enigo variants.

Ctrl+Alt+Del stays unrepresentable. Keys outside this vocabulary
(Ctrl+punctuation such as Ctrl+- or Ctrl+/, PrintScreen, CapsLock, the Menu
key) are not sent; `docs/remote-desktop.md` lists them as a limitation.

`VideoFormat` gains `#[serde(default)] encode_us: u32`. `SessionStatus`
(`D/lib.rs:125-165`) and `VideoPreferences` are **deleted at the G1 cleanup**
(X4 in WORKPLAN §1.2). Until then they stay beside the new types, because
the v1 pipeline still runs on the integration branch between G0 and G1.
`HostStatus` (5.15) and `ViewerStatus` (6.10) replace `SessionStatus`.

### 4.6 Ordering and validity (strict: any violation ends the session with `ProtocolError`)

**The host guarantees:**

- `Accepted` is the first message, and the first `State` follows before any
  `Video`.
- A `State` of generation G precedes the first `Video` of G.
- `Video.seq` is contiguous.
- Before admission, B-send drops any unit whose generation is lower than the
  last written `State.generation`, without assigning a seq.

**The viewer checks:**

1. `Video.seq == last_seq + 1`.
2. `Video.generation < state.generation` → **ack and drop**.
   `Video.generation > state.generation` → protocol error.
3. `codec == Accepted.codec`, `width ≤ limits.max_width` and
   `height ≤ limits.max_height`.
4. `display` is within 1..=32768 (kept, `D/viewer.rs:239-251`).
5. A payload appears only on `Video`.
6. A `Cursor` of a non-current generation is ignored.

**The host checks:**

1. `Authenticate` comes first and only once.
2. `Received.seq > last_admitted_seq` is a protocol error. This is safe
   because the seq is booked before the write (5.7.2). `seq ≤ acked` is
   ignored as a duplicate.
3. A non-outstanding `ProbeEcho` is ignored.
4. `FpsCap.fps` must be within 1..=120.
5. An `InputEvent` keeps the existing `Input::validate` bounds.

### 4.7 Capabilities

- Values match `[a-z0-9_]{1,32}`, at most 16 entries. Unknown values are
  ignored.
- The v2.0 host advertises `lock` (Windows backends) and `secure_desktop`
  (privileged helper active). The v2.0 viewer advertises none.
- A v2.x peer sends a new message, field or enum value **only** if the other
  side listed the matching capability. Anything else needs a new ALPN.

### 4.8 Session sequence

```
viewer: TLS (ALPN v2) ; Authenticate{...}
host:   Accepted{...} | Rejected{denied|busy|invalid_request}+close   (busy only after a valid token)
host:   Probe{1}  State{connecting,G}  State{streaming,G,display,video}  Video{seq 1, IDR}  Video ...  Cursor ...  Stats (1 s)
viewer: ProbeEcho{1}  Path{route}  Received{seq} (per video, coalesced)  Presented (≤10 Hz, flushed before input)
        Keyframe (limited)  FpsCap (≤1/s)  Pause/Resume  Quality  Control/Release/Input  Ping (idle 1 s)
```

### 4.9 Invitation `pbd1` and pinning (D3) (`D/invite.rs`, `D/security.rs`)

Text form: `"pbd1." + base64url_no_pad(payload)`. Strict base64url: no
padding and no foreign alphabet. The text is at most 512 characters.

| Offset | Size | Field | Rule |
| --- | --- | --- | --- |
| 0 | 1 | version | `0x02` |
| 1 | 1 | flags | `0x00` (non-zero → `UnsupportedVersion`) |
| 2 | 1 | n | suffix length, 1..=120 |
| 3 | n | suffix | bytes after `desktop:`; `[A-Za-z0-9._:-]`; `"desktop:"+suffix` must pass `valid_service` (`D/lib.rs:49-56`) |
| 3+n | 32 | fingerprint | SHA-256 of the host certificate DER |
| 35+n | 32 | token | raw 32 bytes |
| 67+n | 4 | check | `SHA-256(bytes[0..67+n])[0..4]` (typo check, constant-time compare) |

The payload is exactly `71 + n` bytes. For `desktop:my-computer` that is
82 bytes, or 115 characters in total.

```rust
#[derive(Clone)] pub struct Invitation { pub service: String, pub fingerprint: [u8; 32], token: Token }
struct Token([u8; 32]);                       // redacted Debug, zeroed on drop
impl Invitation {
    pub fn encode(&self) -> String;            // always pbd1
    pub fn decode(text: &str) -> Result<Self, InvitationError>;
    pub fn short_fingerprint(&self) -> String; // "AB12-CD34-EF56-7890" (first 8 bytes, upper hex)
    pub fn format(&self) -> InvitationFormat;  // Pbd1 | JsonV1 (from decode)
    pub(crate) fn token_text(&self) -> String; // 43-char base64url for Authenticate
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationError { Empty, TooLarge, SetupString, Malformed, Checksum, UnsupportedVersion, InvalidService }
```

`decode` steps:

1. Trim. Empty input → `Empty`. More than 8192 bytes → `TooLarge`.
2. A `pbd1.` prefix → the binary rules above.
3. A `{` prefix → v1 JSON (`deny_unknown_fields`, `version == 1`, the existing
   checks). The fingerprint is the SHA-256 of the DER bytes **decoded** from
   the v1 `certificate` base64url field (`D/security.rs:79-82`), never of the
   base64 text, so a v1 invite pins the same certificate as its pbd1
   re-encoding. The token must be exactly 32 bytes.
4. A `pbmt1_` prefix, or standard/url base64 that decodes to a JSON object with
   `serverAddress` or `msgHeaderKey` → `SetupString`.
5. Anything else → `Malformed`.

Pinning:

- `security::client(fingerprint: [u8; 32])` replaces `client(certificate)`.
- `Pinned::verify_server_cert` requires `intermediates.is_empty()` and
  `subtle::ConstantTimeEq(SHA-256(end_entity), fingerprint)`. Otherwise it
  returns
  `rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(IdentityChanged))))`.
- `verify_tls13_signature` delegates to `rustls::crypto::verify_tls13_signature`.
  TLS 1.2 is rejected.
- The viewer detects `IdentityChanged` by **downcast**, which gives
  `ConnectError::IdentityChanged`.
- The host side is unchanged: `HostIdentity` stores the certificate, key and
  token. `HostIdentity::invitation(service)` emits `pbd1`, so
  `desktop-trust.dpapi` is re-encoded on load with no migration.

### 4.10 Removed from the wire

- `wire::Command`, with `Authenticate.presentation_feedback` and
  `Authenticate.video`.
- `Command::Probe`, which becomes `ProbeEcho`.
- `SessionStatus` per message, and its `cursor`, `frame_ack_ms` and
  `roundtrip_probe` fields.
- The empty payload record after non-video messages.
- `VideoPreferences.bitrate`.
- The v1 JSON invite as output.
- Cursor stamping into video (`D/host.rs:227-235`, `D/media.rs:41-44`).

---

## 5. Host

### 5.1 Shared session state (`D/shared.rs`, WP-0) and lock discipline

```rust
pub(crate) struct SessionShared {
    pub epoch: Instant,                                   // session start (D24 since_ms)
    pub cancel: CancellationToken,
    pub policy: Arc<Policy>,                              // moved verbatim from D/worker.rs:17-37
    pub auth: parking_lot::Mutex<AuthLedger>,             // 5.12
    pub lane: InputLane,                                  // 5.12
    pub outgoing: Outgoing,                               // 5.6
    pub idr: parking_lot::Mutex<IdrPolicy>,               // 5.11
    pub targets: Targets,                                 // B -> C
    pub state: tokio::sync::watch::Sender<EngineState>,   // C -> B-send, Host::status
    pub cursor: tokio::sync::watch::Sender<Option<CursorUpdate>>, // C -> B-send
    pub pointer_input_at_us: AtomicU64,                   // B-recv (µs since epoch); CursorGate
    pub wake: scheduler::Wake,                            // wakes C's Waiter
}
pub(crate) struct Targets {
    pub pace_fps: AtomicU32, pub encoder_bps: AtomicU32,  // published by B tasks after every Qos change
    pub paused: AtomicBool,
    pub nominal_fps: u32,                                 // = limits.max_fps, session constant
}
#[derive(Clone, PartialEq)]
pub(crate) struct EngineState {
    pub state: SessionState, pub reason: Option<StateReason>, pub message: String,
    pub control_allowed: bool, pub controlling: bool, pub generation: u64,
    pub display: Option<Display>, pub desktop: DesktopKind, pub video: Option<VideoSize>,
    pub capture_fps: f32, pub skipped_ticks: u64, pub dropped_frames: u64, pub gop: u32,
    pub notice: Option<(NoticeCode, String, u64 /*serial*/)>,  // B-send writes each serial once
    pub input_wait: InputWaitStats,                       // p50/p99/max µs over the last second (trace)
}
pub(crate) struct CursorUpdate { pub generation: u64, pub x: i32, pub y: i32, pub visible: bool }
```

`SessionNet` is owned by WP-H and is private to the B tasks. It holds:

- `credit: parking_lot::Mutex<CreditLedger>`;
- `qos: parking_lot::Mutex<Qos>`;
- `credit_changed: tokio::sync::Notify`.

**Lock discipline.**

- Never hold two of `credit`, `qos`, `auth`, `idr` or `lane` at once.
- Never hold any of them across an `.await` or a backend call.
- Each critical section is O(1) or O(≤ 256).
- B-send reads the Qos values it needs into locals, releases `qos`, and only
  then locks `credit`.

### 5.2 Host session state machine

```
Handshake --token ok, slot free--> Starting --first Video written--> Streaming
Handshake --bad token--> [Rejected{denied}] Closed            (constant-time token check kept, D/host.rs:160-162)
Handshake --slot busy (token ok)--> [Rejected{busy}] Closed
Handshake --bad codecs/limits--> [Rejected{invalid_request}] Closed
Streaming <--Pause / Resume--> Paused         (Paused: pace 0, control ended, no cursor; Probe/State/Stats continue)
Streaming <--context error / context ok--> CaptureUnavailable{secure_desktop|display_lost}
any --policy revoke--> same state, controlling=false
any --Host::end_session--> [Stopped{host_ended}] Closed        (D15; hosting continues)
any --host restart for a display change--> [Stopped{restarting}] Closed
any --Host::stop--> [Stopped{host_stopped}];  --trust revoke--> [Stopped{revoked}]
any --worker exits fatally (helper lost, encoder unrecoverable)--> [Stopped{capture_failed}] Closed
any --protocol violation--> [Stopped{protocol_error}] Closed
any --read timeout (HOST_READ_TIMEOUT) / write timeout--> Closed (no message)
```

There is **no** presentation-stall teardown. `D/host.rs:202-212` is deleted.
Unresponsiveness never closes a session.

### 5.3 Handshake (`D/host.rs`, WP-H)

1. The handshake reads `Authenticate` within `HANDSHAKE_TIMEOUT`. Any other
   first message → close.
2. Check the token in constant time. A bad token → `Rejected{denied}` →
   close.
3. Validate `codecs` (1..=2) and `limits.validate()`. A failure →
   `Rejected{invalid_request}`.
4. `Semaphore::try_acquire`. A failure → `Rejected{busy}`.
5. Choose the codec: H.264 if offered and the backend encodes, else JPEG.
6. `nominal_fps = limits.max_fps`.
7. Write `Accepted`. Create `SessionShared`. Record `session_started` (D24).
   Spawn S, B-recv, B-send and the worker:
   `worker::spawn(shared, factory, WorkerConfig{codec, limits, nominal_fps, admission}) -> WorkerHandle`.
8. B-send writes `Probe{1}` before anything is captured, to get a clean
   baseline.

### 5.4 B-recv: never awaits the worker

| Message | Inline handling |
| --- | --- |
| `Received{seq}` | `credit.on_receipt(seq, now)?` (a protocol error ends the session); `qos.on_receipt(now)` (clears unresponsive); `credit_changed.notify_one()` |
| `ProbeEcho{nonce}` | `qos.on_probe_echo(nonce, now)`; publish targets |
| `Presented{g,f}` | `auth.on_presented(g, f, now)`. If it returns false, set `stale_presentation` and wake C (C calls `backend.release()`, fail-closed, as `D/worker.rs:452-457` does today) |
| `Keyframe{reason}` | `idr.request(IdrReason::Viewer(reason))`; wake C |
| `FpsCap{fps}` | validate; `qos.set_viewer_cap(fps)` (cleared when `fps ≥ limits.max_fps`); publish |
| `Quality{preset}` | `qos.set_preset(preset)` (5.9.6); publish |
| `Pause` | `qos.set_paused(true)`; `targets.paused = true`; `lane.push(LaneCommand::EndControl)`; wake C |
| `Resume` | `qos.resume(now)` (paused off; `startup_until = now + 1 s`; unresponsive and delay history cleared); `targets.paused = false`; `idr.force(Resume)`; wake C |
| `Path{route}` | `qos.reset_baseline()` (clears the RTT window and the delay history; keeps fps and bpf) |
| `Control{enabled}` | if `enabled && !policy.allowed()` → drop and set reason `ControlDenied`; else `lane.push(Control)` |
| `Release` | `lane.push(Release)` |
| `Input{event}` | if `!policy.allowed()` → drop. If the kind is `move`, `button` or `wheel`, store `pointer_input_at_us`. Then `lane.push(Input(event))`. On overflow, `push` sets the overflow flag and wakes C. |
| `Ping` | nothing |
| `Authenticate` | protocol error |

### 5.5 B-send loop (exact order per pass)

1. Cancelled → `Stopped{reason}` (`STOP_WRITE_TIMEOUT`) → shutdown.
2. **State.** If the `State`-visible subset of `EngineState` changed → write
   `State`. `last_state_generation` = its generation.
3. **Notice.** For each new notice serial → write `Notice`.
4. **QoS tick.** Every `QOS_TICK`: `qos.tick(now, credit snapshot)`
   (unresponsive, startup expiry, 3 s bpf window, latch); then publish
   targets.
5. **Stats.** Every `STATS_INTERVAL` → write `Stats`. Also emit the 1 Hz
   trace (5.17).
6. **Probe.** If none is outstanding and `now − last_probe ≥ PROBE_INTERVAL`:
   - write `Probe{nonce}`;
   - `qos.probe_sent(nonce, now, tainted = credit.idr_inflight().is_some())`.
7. **Cursor.** Apply `CursorGate` (5.13).
8. **Video** (repeat while admitted):
   1. `head = outgoing.peek_meta()`.
   2. `head.generation < last_state_generation` → `outgoing.pop()`,
      `dropped += 1`, continue. Stale units get **no seq**.
   3. `credit.aging_since(head.ready_at)` is `Some(since)` and
      `now − since > OUTGOING_MAX_AGE` → `outgoing.clear()`,
      `idr.request(HostResync)`, wake C, continue. `aging_since` is
      `max(ready_at, last IDR receipt)`, and `None` while
      `credit.idr_inflight()` is `Some`: a head waiting behind an IDR in
      flight never ages, since that IDR is already the resync.
   4. `head.generation > last_state_generation` → write `State` first.
   5. Compute the window (5.7.1).
   6. `credit.admit(meta, window, now)` → `Send(seq)` or `Wait`.
   7. On `Send(seq)`: `outgoing.pop()`; `write_message(Video{seq, …, host_us}, payload)`.
      `host_us = encode_us + (now − ready_at)`.
   8. On `Wait`: stop the loop.
9. **Wait.** `select!` on:
   - `outgoing.notified()`, `credit_changed`, `state.changed()` and
     `cursor.changed()`;
   - a sleep until the earliest of: next probe, stats, QoS tick, cursor
     un-suppress, and 20 ms while a head waits for credit.

### 5.6 `Outgoing` hand-off (`D/outgoing.rs`, WP-0)

```rust
pub(crate) struct HostUnit { pub generation: u64, pub frame_id: u64, pub codec: VideoCodec, pub width: u32, pub height: u32,
                             pub keyframe: bool, pub duplicate: bool, pub pts_us: u64, pub encode_us: u32,
                             pub ready_at: Instant, pub bytes: bytes::Bytes }
pub(crate) struct Outgoing { q: parking_lot::Mutex<VecDeque<HostUnit>>, notify: tokio::sync::Notify, stats: ProducedCounters }
impl Outgoing {
    pub fn push(&self, u: HostUnit) -> Push;           // Queued | Overflow{dropped}: len == OUTGOING_CAPACITY or bytes > OUTGOING_BYTES
                                                       //  -> clear all, keep `u` only if keyframe; caller requests HostResync
    pub fn peek_meta(&self) -> Option<UnitMeta>;       // {generation, keyframe, len, ready_at}
    pub fn pop(&self) -> Option<HostUnit>;
    pub fn len(&self) -> usize;                        // includes the head B-send is evaluating
    pub fn clear(&self) -> usize;
    pub fn retain_generation(&self, g: u64) -> usize;  // C calls on invalidate
    pub async fn notified(&self);
    pub fn produced(&self) -> ProducedSnapshot;        // cumulative: frames, unique, keyframes, non_idr_frames, non_idr_bytes
}
```

`ProducedCounters` are atomics updated in `push`. Qos diffs them per 3 s window
(5.9.3). Overflow is a bug path, because the submit gate (5.10.2) keeps
`len ≤ 2 + MAX_IN_FLIGHT`. It is counted in `dropped_frames`.

### 5.7 Receipt credit (`D/credit.rs`, WP-H)

```rust
pub(crate) struct CreditLedger {
    last_admitted: u64, acked: u64,
    in_flight: VecDeque<Sent>, in_flight_bytes: usize, idr_inflight: Option<u64 /*seq*/>,
    idr_received: Option<Instant>,                             // the last IDR's receipt; aging_since (5.5 step 8.3)
    ack_samples: VecDeque<(Instant, u32 /*ack delay ms*/)>,   // last ACK_SAMPLE_WINDOW
    delivered: VecDeque<(Instant, u32 /*bytes*/)>,            // last DELIVERY_WINDOW
    last_receipt: Instant,
}
struct Sent { seq: u64, bytes: u32, sent_at: Instant, keyframe: bool }
pub(crate) struct Window { pub frames: usize, pub bytes: usize, pub clamped: bool }
pub(crate) enum Admit { Send(u64 /*seq*/), Wait }
```

#### 5.7.1 Window (computed by B-send before each admission)

```
t       = base_rtt.unwrap_or(INITIAL_RTT) + WINDOW_MARGIN
frames  = clamp(ceil(pace_fps × t), FRAME_WINDOW_MIN, FRAME_WINDOW_MAX)
normal  = clamp(2 × expected_bps × t / 8, BYTE_WINDOW_MIN, BYTE_WINDOW_MAX)
ack_q   = median(ack_samples) − base_rtt         (0 unless base_rtt known and ≥ 3 samples)
if ack_q > ACK_QUEUE_LIMIT && `delivered` spans ≥ DELIVERY_WINDOW:
      bytes = clamp(DELIVERY_FACTOR × delivery_bps × t / 8, BYTE_WINDOW_CLAMP_MIN, normal); clamped = true
else  bytes = normal
JPEG sessions: frames = 4, bytes = 2 MiB, no clamp
```

- `base_rtt` (the window minimum), never the smoothed RTT, so queueing never
  enlarges the window.
- The clamp uses transport receipts only. It is never driven by
  presentation. It bounds the standing queue between probes, while fps and
  bitrate decisions stay on the probe (D5).
- Example: 60 fps, 20 ms base, 4 Mbit/s → 8 frames, 120 KB.

#### 5.7.2 Admission and booking (B-send, under the `credit` lock)

```
1. in_flight empty                                                        -> book, Send   (liveness; any size ≤ MAX_PAYLOAD)
2. keyframe && idr_inflight.is_none()
   && in_flight.len() < frames + 1 && in_flight_bytes ≤ bytes             -> book, Send, idr_inflight = Some(seq)
3. in_flight.len() < frames && in_flight_bytes + size ≤ bytes             -> book, Send
4. otherwise                                                              -> Wait
book: seq = last_admitted + 1; last_admitted = seq; in_flight.push_back(Sent{seq, size, now, keyframe}); in_flight_bytes += size
```

- Booking happens **before** the write. A receipt can therefore never precede
  its booking, which makes the strict `Received.seq > last_admitted → ProtocolError`
  rule safe (must-fix).
- If the write then fails, the session ends.
- There is no Discard, no 256 KiB rule, no ready-age discard, and an IDR never
  waits for the whole flight.

#### 5.7.3 Receipts (B-recv)

`on_receipt(seq, now) -> Result<()>`:

1. `seq > last_admitted` → `Err(ProtocolError)`.
2. `seq ≤ acked` → `Ok` (duplicate).
3. Otherwise pop every entry with `seq ≤` the acked seq:
   - subtract its bytes;
   - push `(now, bytes)` into `delivered`;
   - clear `idr_inflight` and set `idr_received = now` if that entry is
     popped;
   - for the newest popped entry, push `(now, now − sent_at)` into
     `ack_samples`;
   - set `acked = seq` and `last_receipt = now`.

**D4 boundary.** `Received` is consumed only by `CreditLedger` and
`Qos::on_receipt`. It never reaches `AuthLedger`, `Engine`,
`backend.presented` or the input lease. No authorisation method takes a seq,
so the compiler enforces this.

### 5.8 Probe and RTT baseline (`D/qos.rs`)

- Probes are in stream order (between video messages), so they measure
  downstream queueing. Nonces run 1, 2, 3, and one is outstanding at a time.
- `on_probe_echo(nonce, now)`:
  - a non-outstanding nonce is ignored;
  - a sample older than `IO_TIMEOUT` is discarded;
  - otherwise `rtt = max(now − sent, DELAY_FLOOR)`.
- `RttWindow` holds the last `RTT_WINDOW` samples. `base_rtt = min(window)`
  from the **first** sample. `reset_baseline()` (on `Path`) clears the window
  and the delay history.
- A **tainted** sample (the probe was written while `idr_inflight` was set)
  updates `last_rtt` and the window, never the delay history or the fps
  controller. Its delay (`rtt − base_rtt`) is kept apart as the bpf window's
  worst tainted delay (5.9.3).
- `d = max(0, mean(last DELAY_HISTORY untainted samples) − base_rtt)`. With no
  untainted samples, the fps controller holds.

### 5.9 QoS controller (`D/qos.rs`, WP-H)

#### 5.9.1 Presets (D12; resolution fixed)

Bits per frame (bpf) are at 1080p. They are multiplied by
`pixel_scale = clamp(encoded_w × encoded_h / 2_073_600, 0.25, 4.0)`.

| Preset | min_fps | normal_fps | ceiling fps | bpf start | bpf max | bpf floor | i.e. at 60 fps (30 for Quality) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `quality` | 8 | 24 | 30 | 200 000 | 400 000 | 50 000 | 6 → 12 Mbit/s, floor 1.5 |
| **`balanced`** (default) | 10 | 45 | 60 | 66 667 | 133 333 | 16 667 | **4 → 8 Mbit/s**, floor 1 |
| `smooth` | 12 | 50 | 120 | 50 000 | 100 000 | 12 500 | 3 → 6 Mbit/s, floor 0.75 |

- `ceiling_eff = min(preset.ceiling, limits.max_fps)`.
- `normal_eff = min(normal, ceiling_eff)`.
- `scale = max(1.0, ceiling_eff / 30)`.
- The start fps is `normal_eff`.
- On a phone, Balanced starts at 4 Mbit/s (at 60 fps) and 45 fps, probes up to
  60 fps, and can raise bitrate to 8 Mbit/s (P0-4).

#### 5.9.2 fps on every untainted sample (RustDesk `user_network_delay`, scaled steps)

```
small = max(1, round(scale)); quick_step = round(5 × scale)
d < 50:          quick += 1; step = if fps < normal_eff {small} else {0}
                 if quick >= 3 { quick = 0; step = quick_step }; fps = max(min_fps, fps + step)
50 ≤ d < 100:    if last_d.is_some_and(|l| d < l) && fps < normal_eff { fps += small }
100 ≤ d < 150:   fps = max(min_fps, fps)
d ≥ 150:         div = ceil(fps × 150 / d)
                 150..200: fps = max(min_fps, div);  200..300: fps = min(min_fps, div)
                 300..600: fps = 150 × min_fps / d;  ≥ 600:   fps = min(150 × min_fps / d, div)
if d < 150 { increase += 1; if increase >= 3 { increase = 0; fps += small } } else { increase = 0 }
if d > 50 { quick = 0 }
fps = clamp(fps, 1, ceiling_eff); last_d = Some(d); window.max_d = max(window.max_d, d)
the first untainted sample of the session also runs the bpf step immediately (RustDesk)
```

#### 5.9.3 Bits per frame, encoder bitrate and rate-model latch (D6)

The 3 s window is evaluated on the QoS tick that crosses it.

- `dynamic` = `produced.unique` delta ≥ `DYNAMIC_UNIQUE_FRAMES`.
- No sample in the window → hold.
- Only tainted samples in the window → one bpf step on their worst delay
  (`rtt − base_rtt`) with `dynamic = false`, so it can only lower bpf: on a
  link where every IDR takes seconds, smaller IDRs are the way out. Tainted
  samples still never feed the fps controller or the delay history.

| `window.max_d` | bpf factor |
| --- | --- |
| < 50 | ×1.15 if dynamic |
| < 100 | ×1.10 if dynamic |
| < 150 | ×1.05 if dynamic |
| < 200 | ×0.95 |
| < 300 | ×0.90 |
| < 500 | ×0.85 |
| ≥ 500 | ×0.80 |

Increases are capped: `bpf = min(bpf × f, bpf + 150_000 / max(pace, 1))`.
The result is clamped to `[floor × pixel_scale, max × pixel_scale]`.

```
expected_bps  = bpf × pace
encoder_bps   = clamp(match rate_model { Nominal => bpf × nominal_fps, Timestamp => bpf × pace }, 256_000, 40_000_000)
latch (same window, only while rate_model == Nominal && dynamic):
    n = produced.non_idr_frames delta;  bits = produced.non_idr_bytes delta × 8
    if n ≤ LATCH_PACE_RATIO × nominal_fps × 3 && bits > LATCH_OVERSHOOT × bpf × n { overshoot += 1 } else { overshoot = 0 }
    if overshoot ≥ LATCH_WINDOWS { rate_model = Timestamp (one-way for the session); trace desktop_rate_model_latched }
```

How the two encoder rate models behave:

- **Per-frame-budget MFT** (`Nominal`). It spends `encoder_bps / nominal = bpf`
  per frame, so an fps cut sheds bandwidth at once.
- **Per-second-budget MFT.** At reduced pace it spends
  `bpf × nominal / pace ≥ 1.33 × bpf` per frame. The latch detects this within
  two windows, and bandwidth then follows pace.
- Nothing ever raises the encoder bitrate because fps was cut.

`video_bench --pace 60,30,15,5` (10 s per step at nominal 60) prints `pace,
expected_mbps, produced_mbps, model`. Acceptance: produced is within ±25% of
expected under the chosen model, per GPU vendor available. Results are
recorded in `docs/remote-desktop.md`.

#### 5.9.4 Unresponsive (QoS tick)

- **Set** when the probe has been outstanding longer than `UNRESPONSIVE_AFTER`,
  or when `in_flight` is non-empty and `now − last_receipt > UNRESPONSIVE_AFTER`,
  while not paused.
- **Effect:** output pace becomes `min(pace, UNRESPONSIVE_FPS)`. The `fps`
  state, the delay history and bpf are **untouched**.
- **Cleared** on the next receipt or valid probe echo, so recovery is
  immediate.
- The session never closes because of it.

#### 5.9.5 Pace (published to `Targets.pace_fps` after every change)

```
if paused -> 0
p = min(fps, ceiling_eff, viewer_cap.unwrap_or(u32::MAX), limits.max_fps)
if now < startup_until -> p = min(p, min(normal_eff, round(15 × scale)))   // 1 s after Accepted and after Resume
if unresponsive        -> p = min(p, UNRESPONSIVE_FPS)
if codec == Jpeg       -> p = min(p, JPEG_MAX_FPS)
max(p, 1)
```

#### 5.9.6 Live preset, viewer cap, pause

- `set_preset(q)`:
  - `fps = normal_eff(q)`; `bpf = start(q) × pixel_scale`;
  - reset `quick`, `increase` and the window;
  - keep `rate_model`;
  - **no IDR, no `configure_video`, no resolution change.**
- `set_viewer_cap(fps)`: a runtime limit only, never written into `limits`
  (D8).
- `set_paused(true)`: pace 0.
- `resume(now)`: back through the startup rule.

### 5.10 Capture pacing and encoder (thread C, `D/worker.rs`, `D/pacer.rs`, `D/video/windows.rs`, WP-W)

#### 5.10.1 Pacer (single owner: thread C)

```rust
pub(crate) struct Pacer { next: Instant, period: Duration, last: Option<Instant> }
impl Pacer {
    pub fn set_pace(&mut self, fps: u32, now: Instant);  // fps ≥ 1: period = 1 s / fps; next = min(next, last.unwrap_or(now) + period)
    pub fn due(&self, now: Instant) -> bool;              // now >= next
    pub fn consume(&mut self, now: Instant);              // fired OR skipped: next = max(next + period, now + period * 3 / 4); last = Some(now)
    pub fn next(&self) -> Instant;
}
```

This keeps the long-run grid (the 165 Hz rationale at
`video/windows.rs:414-416` still holds), and two fires are never closer than
0.75 periods after a stall.

#### 5.10.2 Worker capture step (per iteration, after the lane drain)

```
if targets.paused:
    if backend.pending_video() > 0 { discard(backend.drain_video(&ctx, ZERO)) }  // never send while paused
    outgoing.clear(); engine.keyframe = true                                     // no cursor sampling either
elif state == CaptureUnavailable && now < context_due: nothing         // context_due: see fail() below
elif retry (CaptureUnavailable, context_due reached) || pacer.due(now):
    // a retry is a context check followed at once by a capture, bypassing the
    // pacer tick, so a failure that persists never publishes Connecting in between
    if outgoing.len() + backend.pending_video() < SUBMIT_GATE:
        rate = VideoRate { bitrate: targets.encoder_bps, pace_fps: targets.pace_fps }
        keyframe = engine.keyframe || idr.take_due(now)
        result = backend.capture_video(&ctx, codec, keyframe, rate)
        if keyframe { engine.keyframe = false; idr.answered(now) }       // handed over: never asked for twice
        produced(result)
    else:
        state.skipped_ticks += 1           // transport backpressure, never presentation
    pacer.consume(now)
elif backend.pending_video() > 0:
    produced(backend.drain_video(&ctx, ZERO /*ordinary*/ | min(HELPER_DRAIN_WAIT_MAX, pacer.next() - now) /*helper*/))
then drain the lane again (5.12.3)
```

`produced(Some(frame))` does the following:

- If `frame.keyframe` → `idr.answered(now)`, which also coalesces requests
  made while that IDR was being encoded. `engine.keyframe` was already
  cleared at the submit that carried it, so a `fail()` re-force between that
  submit and this output survives.
- `frame_id += 1`.
- `auth.on_produced(frame_id, now)`.
- `outgoing.push(HostUnit)`. On `Overflow` → `idr.request(HostResync)`.
- Update the unique capture fps and `state.video`.

`produced(Err(e))`:

- `e.kind() == NotConnected` → fatal. The worker exits with `CaptureFailed`.
- Otherwise → `CaptureUnavailable` with its reason (`fail()`). A failure is
  **fresh** only when it ends streaming (capture was available and a unit
  had been produced in this generation): it may be a desktop switch, so it
  invalidates (a new generation) and is retried at once. Any other failure,
  such as a capture failing again right after a recovery, is retried every
  `UNAVAILABLE_RETRY`, never at the capture pace.
- A recovery on an unchanged context rebuilds the encoder with an IDR but
  starts no generation; only a changed context, or the fresh failure above,
  does.

The context check (`backend.context()`) runs:

- at most every `CONTEXT_CHECK_INTERVAL`;
- right before each retry's capture while unavailable;
- once per submitted capture, inside the backend (the existing
  `capture_consistent` / encoder finish check).

That replaces several `OpenInputDesktop` calls per 2 ms tick. The injection
boundary still re-checks on every input (`input_in_context`).

C waits until the earliest of:

- `pacer.next()`;
- the next cursor sample;
- the next context check;
- `now + DRAIN_POLL` while `pending_video() > 0` (ordinary backend);
- or a `wake` (lane push, pace change, pause or resume, IDR force, cancel).

The pace is re-read from `Targets` each iteration. A change calls
`pacer.set_pace`.

**The wake must work on every platform** (WP-W owns `D/scheduler.rs`).
Today the non-Windows `Wake::notify()` is empty and `Waiter::receive` wakes
only on an `mpsc` message or its timeout (`D/scheduler.rs:113-145`), so on
Linux, where CI runs `cargo test`, C would sleep until the next pacer, cursor
or 100 ms context deadline, and lane input, cancel and the worker tests would
become timing-dependent. WP-W therefore:

- keeps `Wake::{new, notify}` and `Clone` (WP-0's `InputLane` and
  `SessionShared` hold a `Wake`);
- implements the non-Windows `Wake` as an `Arc` of a
  `parking_lot::Mutex<bool /*pending*/>` plus a `Condvar`: `notify` sets
  `pending` and signals, and the wait consumes `pending`;
- replaces `Waiter::receive(&mpsc::Receiver, timeout)` with
  `Waiter::wait(&self, deadline: Instant) -> Result<bool /*woken*/>`, which no
  longer takes an `mpsc` receiver (the command channel is gone, 5.12.2);
- keeps the Windows event-based implementation behind the same API.

Test: `scheduler::wake_ends_a_wait_on_every_platform` (not `cfg(windows)`):
a `notify` from another thread ends a 1 s wait in under 50 ms, and a
`notify` sent before the wait makes it return at once.

#### 5.10.3 `Backend` trait (`D/lib.rs`, WP-0)

```rust
pub trait Backend: 'static {
    fn configure_video(&mut self, limits: VideoLimits, _nominal_fps: u32) -> Result<()> { limits.validate() }
    fn display(&mut self) -> Result<Display>;
    fn context(&mut self) -> Result<DesktopContext> { /* unchanged default */ }
    fn capture(&mut self) -> Result<Vec<u8>>;
    fn cursor(&mut self, _expected: &DesktopContext) -> Result<Option<CursorPosition>> { Ok(None) }
    /// Drain finished output, then (encoder backends) submit at most one capture. JPEG default unchanged.
    /// `keyframe` is asked for once: an encoder backend keeps it until a picture it submits carries it,
    /// and replaces output it discards with an IDR.
    fn capture_video(&mut self, expected: &DesktopContext, codec: VideoCodec, keyframe: bool, rate: VideoRate) -> Result<Option<EncodedFrame>>;
    /// Drain only. `wait` bounds a long-poll (helper); the ordinary backend never blocks.
    fn drain_video(&mut self, _expected: &DesktopContext, _wait: Duration) -> Result<Option<EncodedFrame>> { Ok(None) }
    /// Units submitted to the encoder and not yet output (submit gate).
    fn pending_video(&self) -> usize { 0 }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>>;        // unchanged default
    fn input(&mut self, event: &Input) -> Result<()>;
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()>; // unchanged default
    fn presented(&mut self, _expected: &DesktopContext) -> Result<()> { Ok(()) }        // helper lease renew
    fn release(&mut self);
    /// R-UI-3 (WORKPLAN §1.1): the OS double-click time and box. None = no hold.
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> { None }
}
```

- A sticky, unrecoverable link failure must return
  `io::ErrorKind::NotConnected`. `RemoteBackend` returns it once `failed` is
  set (`D/helper/client.rs:59-61`).
- `Input::Lock` goes through `input` / `input_in_context`. There is no new
  method. The ordinary `Desktop::input` calls `LockWorkStation()`, which needs
  `Win32_System_Shutdown` added to the desktop crate's production `windows`
  features.
- `Desktop::double_click_guard` calls `GetDoubleClickTime`, which needs
  `Win32_UI_Input_KeyboardAndMouse` in the **production** `windows` features
  too. Today only `[target.'cfg(windows)'.dev-dependencies]` enables it
  (`crates/pb-mapper-desktop/Cargo.toml:50`), and enigo's `windows` 0.61 does
  not unify with the workspace's 0.62, so `cargo test` would pass while
  `cargo build -p pb-mapper-ffi --release` fails. `GetSystemMetrics` is
  already covered by the workspace's `Win32_UI_WindowsAndMessaging`.
- The signature change reaches every `impl Backend` at G0. The exact
  mechanical-edit set is listed in 12.1 (WP-0).

#### 5.10.4 Encoder API (`D/video/windows.rs`)

- **Delete** `next_frame` (`:333-426`) and `next_capture`. The worker is the
  only pacer.
- `pub fn submit(&mut self, force_keyframe: bool) -> Result<bool>`. It
  captures and submits at most one sample, and returns `false` without
  submitting in either of these cases:
  - `pending.len() >= MAX_IN_FLIGHT`, `need_input == 0`, or the allocator is
    empty;
  - no new desktop image, no forced keyframe, and less than 250 ms since the
    last submit. This keeps the static-desktop duplicate liveness of about
    4 fps (`:376-384`), which keeps a presented frame fresh for input on a
    static screen.
- Sample time is the real elapsed time. **Sample duration** is the real
  interval since the previous submit, clamped to `[1/nominal, 1 s]` (a VFR
  timestamp model). `MF_MT_FRAME_RATE` stays at nominal.
- `drain()` is unchanged. It keeps the late-output (> 250 ms) self-recovery
  that forces a keyframe (`:294-301`). `Encoded` gains `encode_us`
  (submit → output).
- `pub fn pending_len(&self) -> usize` = `pending.len()`.
- `pub fn set_rate(&mut self, rate: VideoRate) -> Result<()>` sets
  `CODECAPI_AVEncCommonMeanBitRate` only when the bitrate changed (the
  existing `set_bitrate`, `:252-264`). It never changes the media type.
- `MAX_IN_FLIGHT = 3` is kept.

The ordinary `Desktop::capture_video(ctx, codec, key, rate)` runs `set_rate`,
then `drain`, then `submit(key)`, and returns the drained unit.
`pending_video()` returns `encoder.pending_len()`.

**Infinite GOP (D7).** In `Codec::activate` (`:607`), delete
`GOPSize = fps`. `set_infinite_gop()` tries the GOP candidates in order. It
keeps the first one that `SetValue` accepts and that `ICodecAPI::GetValue`
reads back unchanged, stores it in `Statistics.gop` and traces it. It never
uses 0.

`video_bench 60 60 --pattern-only` must report `keyframes == 1 + forced`
(forced at 1.5 s, `examples/video_bench.rs:134-138`). A vendor that clamps to
a finite GOP of 5 s or more is accepted and documented. Less than 5 s fails
the bench. SPS/PPS in every IDR is kept (`annex_b`, `:756-786`).

**Rebuilds.** `configure_video` runs only when:

- the session starts;
- the context or display changes, which gives a new size and an IDR;
- the encoder needs recovery after an error.

QoS fps changes, bpf changes and preset changes **never** call it. The
automatic resolution ladder is deleted (D5).

### 5.11 IDR policy (`D/idr.rs`, WP-0)

```rust
pub(crate) enum IdrReason { Generation, Resume, Viewer(KeyframeReason), HostResync }  // a new encoder and late output need no request
pub(crate) struct IdrPolicy { pending: Option<IdrReason>, forced: bool, last: Option<Instant> }
impl IdrPolicy {
    pub fn force(&mut self, r: IdrReason);              // immediate on the next submit
    pub fn request(&mut self, r: IdrReason);            // deferred until HOST_IDR_MIN_INTERVAL after `last`; coalesced; never dropped
    pub fn take_due(&mut self, now: Instant) -> bool;   // forced || (pending && last.map_or(true, |t| now - t >= HOST_IDR_MIN_INTERVAL))
    pub fn answered(&mut self, now: Instant);           // clears pending and forced; last = now
}
```

`answered(now)` is called when `capture_video` is handed `keyframe = true`
(5.10.2), and again when a keyframe leaves the encoder. `take_due` therefore
stays true only until the IDR is handed over, not until the encoder emits
it: an IDR still being encoded at the next tick is never asked for twice.
Nothing is lost, because the encoder latches a request it has no room for
(`submit` keeps `force_keyframe`), late output re-forces, and a new encoder
starts with an IDR (5.10.3).

| Trigger | Kind |
| --- | --- |
| New encoder (session, context or display change, encoder recovery) | inherent (the encoder starts with an IDR, `video/windows.rs:238`) |
| `Engine::invalidate` (control grant or revoke, input failure, a new desktop, or a capture failure that ended streaming) | `force(Generation)` |
| `Resume` | `force(Resume)` |
| Viewer `Keyframe{reason}` | `request(Viewer(reason))` |
| Outgoing head that waited more than 1 s (5.5 step 8.3), or overflow | `request(HostResync)` |
| Encoder late output | inherent (`:294-301`) |
| **Never** | periodic GOP, fps change, bpf change, preset change, congestion |

Recovery never depends on a presentation ack. The `recovering` single-credit
trap (`D/congestion.rs:234-249`) is deleted.

### 5.12 Input path and the authorisation boundary (D4, D10)

#### 5.12.1 `AuthLedger` (`D/auth.rs`, WP-0): the only authorisation ledger

```rust
pub(crate) struct AuthLedger {
    generation: u64,                                            // mirrors Engine generation (C writes on invalidate)
    recent: VecDeque<(u64 /*frame_id*/, Instant /*produced*/)>, // ≤ RECENT_FRAMES; C writes on every produced frame
    acked: Option<(u64 /*gen*/, u64 /*frame_id*/)>,             // set ONLY by a validated Presented
    stale_presentation: bool,
    lease_due: bool,
}
impl AuthLedger {
    pub fn on_produced(&mut self, frame_id: u64, at: Instant);
    pub fn on_invalidate(&mut self, generation: u64);            // clears recent and acked
    /// B-recv. Accept iff generation current, frame_id in recent produced < PRESENTED_FRESH ago,
    /// and it advances acked (same generation). Sets lease_due. Else sets stale_presentation; returns false.
    pub fn on_presented(&mut self, generation: u64, frame_id: u64, now: Instant) -> bool;
    /// C (Engine::input). Admit iff acked == Some((generation, a)) && frame_id <= a
    /// && frame_id in recent with produced < PRESENTED_FRESH ago. Never mutates.
    pub fn admits(&self, generation: u64, frame_id: u64, now: Instant) -> bool;
    pub fn fresh(&self, now: Instant) -> bool;                   // acked frame of the current gen produced < PRESENTED_FRESH ago
    pub fn take_stale(&mut self) -> bool;
    pub fn take_lease_due(&mut self) -> bool;
}
```

**Security fix (all three designs).**

- Today `Engine::input` (`D/worker.rs:586-609`) admits any `frame_id` produced
  less than 2 s ago and renews `self.presented` itself (`:609`). That was safe
  only while credit came from presentation ACKs.
- Now **only** `on_presented` moves `acked`. Input is admitted only with
  `frame_id ≤ acked.frame_id` of the current generation and a production age
  under 2 s.
- Input never updates `acked`.
- Receipts never reach this type.
- Cursor observation never renews anything.

#### 5.12.2 `InputLane` (`D/lane.rs`, WP-0)

```rust
pub(crate) enum LaneCommand { Control { enabled: bool }, Release, Input(InputEvent), EndControl }
pub(crate) struct InputLane { q: parking_lot::Mutex<VecDeque<(LaneCommand, Instant /*received*/)>>, overflow: AtomicBool, wake: scheduler::Wake }
impl InputLane {
    pub fn push(&self, c: LaneCommand) -> bool;    // false at LANE_CAPACITY: sets overflow, wakes C, drops c
    pub fn drain(&self) -> VecDeque<(LaneCommand, Instant)>;
    pub fn take_overflow(&self) -> bool;
}
```

#### 5.12.3 Thread C iteration (priority lane)

```
1.  if cancel { break }                                         // never execute input queued before stop (D/worker.rs:153-157)
2.  if policy.get() != seen { engine.release_control(); seen = policy.get() }
3.  if lane.take_overflow() { lane.drain(); engine.release_control(); reason = InputCongestion }
4.  if auth.take_stale() { backend.release() }
5.  if controlling && !auth.fresh(now) { backend.release() }     // release_if_stale, on presentation evidence only
6.  for (cmd, at) in lane.drain() { if cancel { break 'outer }; input_wait.record(now - at); execute(cmd) }
7.  if controlling && auth.take_lease_due() && now - last_lease >= LEASE_RENEW_MIN { backend.presented(&ctx) }
8.  capture step (5.10.2): at most ONE backend capture or drain call
9.  repeat step 6 (input queued during the backend call)
10. cursor sample if due (5.13); publish EngineState if changed; wait (5.10.2)
```

`execute`:

- `Control{enabled}` → `Engine::control`. Unchanged semantics:
  - the lease;
  - a repeated request does not reset;
  - a generation bump through `invalidate`, which now does
    `backend.release()`, `generation += 1`, `auth.on_invalidate(gen)`,
    `outgoing.retain_generation(gen)` and `idr.force(Generation)`.
- `Release` → `backend.release()`.
- `EndControl` → `engine.release_control()`.
- `Input(e)` → `Engine::input`. The checks run in this order:
  1. The sequence must strictly increase. It is updated before the other
     checks.
  2. `geometry()`.
  3. `allowed && controlling`.
  4. `state == Streaming`.
  5. `auth.admits(e.generation, e.frame_id, now)`. On failure:
     `backend.release()` and the error "screen changed or is stale; wait for a
     fresh frame".
  6. `backend.input_in_context`.

  On failure of `input_in_context`:
  - `Input::Lock`: control is **not** released, because nothing was held.
    Set notice `LockFailed` with the Win32 text, for `NOTICE_TTL`.
  - Any other input: `release_control()` and a sticky `input_failed: …`
    (kept, `:610-619`).

  On success of `Input::Lock`: notice `Locked`. The desktop switch then
  invalidates the generation through the existing context path.

Results surface through `EngineState`. The oneshot replies are deleted.

The R-UI-3 drag-start hold (WORKPLAN §1.1) only **delays** commands. A held
command carries no authorisation of its own: when the hold ends it runs this
full `execute` path (sequence, `controlling`, `Streaming`, `auth.admits`,
`input_in_context`) at that moment, and steps 2-5 above drop the whole held
queue when they release.

#### 5.12.4 `input_wait` gate

`input_wait` is measured from the B-recv enqueue to the start of execution on
C. The trace reports p50/p99/max per second. The gate is p99 ≤ 5 ms
(ordinary) and ≤ 15 ms (helper), on acceptance runs with moving content.

#### 5.12.5 Escalation (documented, not built)

If the gate fails, the next step is a dedicated injection thread with its own
`DesktopBinding`. It would come with:

- a process-wide `BindingRegistry{(name, EPOCH, console) → generation}` (newer
  EPOCH → generation + 1; older EPOCH → fail closed);
- a second helper pipe lane.

Record this in `docs/remote-desktop.md`. Build it only on measured evidence.

### 5.13 Cursor (D11)

- C samples `backend.cursor(&ctx)` every 16 ms while streaming, every 200 ms
  while unavailable, and never while paused. It publishes
  `CursorUpdate{generation,x,y,visible}` on change.
- The per-frame cursor call in `capture_video` (`D/worker.rs:534`) and the
  stamping in `D/host.rs:227-235` are deleted.

B-send `CursorGate`:

```
on cursor change or wake:
  if cursor.generation != last_sent_cursor_generation               -> send now (never suppressed)
  elif controlling && now - pointer_input_at < CURSOR_ECHO_SUPPRESS -> hold latest; wake at pointer_input_at + 300 ms
  elif cursor != last_sent                                          -> send
on suppression end: send the held value if it differs from last_sent (trailing sync)
```

There is exactly one viewer (`Semaphore(1)`), so "the operator who sent
input" is that viewer while it is controlling.

### 5.14 Lock (D17)

`Input::Lock` → `Engine::input`, with every check in 5.12.3. The ordinary
host calls `LockWorkStation()`. The privileged host sends
`Request::Input{generation, sequence, input: Lock}`, which the helper worker
executes on its `winsta0\default`-bound thread. Both Windows backends put
`"lock"` in `capabilities`.

### 5.15 Host status (D24) and Host API (types in `D/lib.rs`, WP-0; bodies in `D/host.rs`, WP-H)

The types below and the `Host` method signatures are frozen **at G0**,
because WP-HL (`Snapshot.host`, background `EndSession`) and WP-F
(`host_status`, `host_end_session`) build on them before WP-H lands. At G0:

- `HostStatus`, `HostViewer`, `HostState` and `HostPath` are complete in
  `D/lib.rs`, with serde and `deny_unknown_fields`.
- `Host::status() -> HostStatus` replaces `Host::status() -> SessionStatus`
  (`D/host.rs:308`). The G0 body is an adapter over the v1 host's state
  (`viewer` filled from the v1 session where it can be, `path: Unknown`).
- `Host::end_session()` exists. Its G0 body ends the v1 session if the v1
  host can, and otherwise logs at `warn` and does nothing. WP-H's commit 3
  replaces every body, before G1.
- The FFI `status` op (old Dart, `F/desktop.rs:622`) serialises a
  `legacy_host_status(&HostStatus)` mapping from G0 on. The shim (7.9)
  reuses that function.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStatus {
    pub state: HostState,                    // waiting | connecting | streaming | paused | capture_unavailable | stopped
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reason: Option<StateReason>,
    pub message: String,
    pub control_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub viewer: Option<HostViewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub display: Option<Display>,
    pub desktop: DesktopKind,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostViewer {
    pub since_ms: u64,                       // SessionShared.epoch.elapsed()
    pub started_unix_ms: u64,                // display only
    pub path: HostPath,                      // direct | relay | unknown: registration.diagnostics().direct.active_direct > 0, sampled in status()
    pub controlling: bool, pub paused: bool, pub quality: QualityPreset,
}
impl Host {
    pub fn status(&self) -> HostStatus;
    pub fn invitation(&self) -> &Invitation;            // pbd1
    pub fn set_control_allowed(&self, allowed: bool);   // "Make view-only": policy epoch -> C releases within one backend step; B-recv drops at the edge
    pub fn end_session(&self);                          // D15 Disconnect: Stopped{host_ended}; hosting continues
    pub async fn stop(&self) -> Result<()>;             // Stopped{host_stopped}
}
```

### 5.16 Helper IPC v2 and upgrade (D2) (`D/helper/*`, WP-HL)

1. **Version marker.**
   - `HELPER_PROTOCOL: u32 = 2` lives in `helper/mod.rs`.
   - Pipes are renamed to `\\.\pipe\pb-mapper.desktop-helper.v2` (and
     `.v2.worker.<n>`) and `\\.\pipe\pb-mapper.desktop-background.v2`.
   - `installation.json` gets `schema: 2` and `"protocol": 2`. Schema 1 is
     still parsed and means protocol 1.
2. **`helper::Status`** gains `protocol: u32` and `update_required: bool`.
   `update_required` is true when installed and any of these holds:
   - schema 1;
   - `protocol != 2`;
   - SCM reports the service running while the v2 pipe is absent.

   `ensure_ready()` fails first when an update is required, with "Update the
   Windows desktop service to use remote desktop". FFI `capabilities` passes
   both fields through. The UI offers `helper_action install`, which already
   upgrades in place (`helper/install.rs:488-536`). `background-host.dpapi`
   is kept.
3. **IPC v2.** It stays one request and one response, `deny_unknown_fields`
   and sticky failure. The 3 s timeout bounds each answer and each write.
   The helper worker's wait for the next request is unbounded
   (`exchange(pipe, display_id, serve)`), so a viewer paused for longer than
   3 s does not break the pipe. Liveness comes from pipe EOF and the
   service's watch: `service.rs` relays with `copy_bidirectional` and drops
   the worker's pipe when the caller's pipe closes, when the owner process
   exits, on a session change and on service stop; `RemoteBackend::drop`
   sends `Close`; and the worker thread still releases held input after 2 s.

   ```rust
   enum Request {
       Start { display_id: u32 }, Attach { display_id: u32, session: os::SessionIdentity }, Context, Capture,
       ConfigureVideo { limits: VideoLimits, nominal_fps: u32 },
       Video { expected: DesktopContext, codec: VideoCodec, submit: bool, keyframe: bool, rate: VideoRate, wait_ms: u32 /*0..=4*/ },
       Presented { generation: u64 },
       Input { generation: u64, sequence: u64, input: Input },     // includes Input::Lock
       Release, Close,
   }
   struct Response { context: Option<DesktopContext>, error: Option<String>, video: Option<VideoFormat>,
                     cursor: Option<CursorPosition>, pending: u32,
                     #[serde(default, skip_serializing_if = "Option::is_none")] double_click: Option<DoubleClickGuard> }
   ```

   - `Video{submit: false}` replaces `DrainVideo`. The worker waits at most
     `wait_ms` for an encoder output event while `pending > 0`.
   - `RemoteBackend::pending_video()` returns the last response's `pending`.
   - The helper never paces.
   - `Start` and `Attach` responses carry `double_click`, read by the helper
     worker with `GetDoubleClickTime` and `GetSystemMetrics(SM_C?DOUBLECLK)`
     in **its** session, which is the interactive one.
     `RemoteBackend::double_click_guard()` returns that value. It never reads
     the metrics in its own process: the background host runs inside the
     SYSTEM service in session 0 (`D/helper/background_service.rs`), where
     they are defaults, not the user's settings.
4. **Mid-session reinstall or crash.** The pipe hits EOF, `RemoteBackend`
   becomes sticky-failed and returns `NotConnected`, the worker exits with
   `CaptureFailed`, and the host sends `Stopped{capture_failed}`. Dart
   reconnects with backoff (9.6.2) and never waits forever (must-fix).
5. **Background pipe v2.**
   - `Snapshot.host: Option<HostStatus>`.
   - A new `EndSession` request (D15 for background hosts).
   - An upgrade stops the service through SCM, as Disable and uninstall
     do, and that stop ends a background host's session with
     `Stopped{host_stopped}`: `run` in `D/helper/background_service.rs`
     deactivates the host as soon as the stop token is cancelled. Unlike
     the app's host in item 4, its viewer does not reconnect:
     `host_stopped` is terminal (9.6.2), and the ended card says that
     sharing stopped and offers Reconnect. The restarted service resumes
     background hosting with the saved identity, so once it has registered
     again the same invitation works and Reconnect connects.
   - There is no display switch or rename for a background host. The FFI
     `host_set_display` and `host_rename` (7.2) refuse while background
     hosting is configured, and the UI disables both controls there with a
     "stop sharing first" hint (UI §6.2). A user changes them by stopping
     background sharing and enabling again.

### 5.17 Host trace

- Target `pb_mapper_desktop::feedback`, event `desktop_feedback_interval`,
  once per second. The name is kept so `deployed --feedback-log` keeps working.
  - Fields: `action` (Adaptation), `fps`, `pace_fps`, `viewer_cap`, `paused`,
    `unresponsive`, `bpf`, `bitrate`, `encoder_bitrate`, `sent_bitrate`,
    `rate_model`, `rtt_ms`, `baseline_ms`, `delay_ms`, `tainted`,
    `ack_queue_ms`, `window_frames`, `window_bytes`, `window_clamped`,
    `outstanding_frames`, `outstanding_bytes`, `idr_inflight`,
    `skipped_ticks`, `keyframes`, `gop`, `input_wait_p50_us`,
    `input_wait_p99_us`, `input_wait_max_us`.
  - `docs/remote-desktop.md` lists the removed fields: `quality`, `waiting`,
    `recovering`, `desired_window_bytes`, `last_ack_*`.
- Target `pb_mapper_desktop::frames`, event `desktop_frame` (trace level), per
  written unit: `seq, frame_id, keyframe, bytes, ready_us, written_us` (µs
  since the session epoch). The viewer emits the matching per-frame event
  (6.10).

### 5.18 Host deletions

**Code:**

- `D/congestion.rs`, the whole file:
  - `Feedback`, `Admission::Discard`, the quality ladder, `ack_stall`,
    `stalled()`, `request_keyframe`;
  - `MAX_FRAMES`, `MAX_INDEPENDENT_FRAME`, `MAX_READY_AGE`;
  - the single-nonce probe.
- `D/media.rs`, the whole file (R30).
- In `D/worker.rs`:
  - `paused = waiting`;
  - the feedback-driven reconfigure;
  - `Worker::sent` and `bitrate: Arc<AtomicU32>`, `sync_feedback`;
  - the oneshot replies;
  - `Engine.acknowledged`/`frame_ack_ms`;
  - the per-frame `backend.cursor`;
  - the unconditional 2 ms period;
  - handling of `Command::Presented` on C.
- In `D/host.rs`:
  - the stall stop and the `presentation_feedback` branches;
  - the startup probe block;
  - status serialised twice;
  - cursor stamping.
- `VideoPreferences`.
- The `CODECAPI_AVEncMPVGOPSize = fps` setting.
- `Encoder::next_frame`/`next_capture`.

**Tests:**

- All ten `congestion.rs` tests.
- Both `media.rs` tests.
- In `worker.rs`:
  - `presentation_backpressure_drains_encoder_without_new_capture`;
  - `unavailable_desktop_retires_old_ack_debt_and_resumes_control`;
  - `decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture`.
- `tests.rs` `older_authentication_defaults_video_limits_and_rejects_unbounded_preferences`.
- In `slow_link.rs`: the assertions "slow consumer must reduce bitrate",
  `peak <= MAX_QUEUED_FRAMES`, `width >= 960`, `frame_ack_ms < 350` and
  stop-on-no-ACK. The module is rewritten (10.1).

---

## 6. Viewer (Rust, WP-V; `VideoSink` skeleton in WP-0)

### 6.1 Public SDK API (`D/viewer/mod.rs`, which replaces `D/viewer.rs`)

```rust
pub struct ViewerConfig {
    pub codecs: Vec<VideoCodec>,           // FFI: [H264] on Android/Windows, [Jpeg] elsewhere
    pub limits: VideoLimits,               // FFI: VideoLimits::for_viewer(refresh_hz)
    pub quality: QualityPreset,
    pub progress: Option<Arc<dyn Fn(ConnectStage) + Send + Sync>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)] #[serde(rename_all = "snake_case")]
pub enum ConnectStage { Path, Identity, Authorizing }
#[derive(Debug)]
pub enum ConnectError { IdentityChanged, VersionMismatch, Busy, Denied, InvalidRequest, Cancelled, Transport(io::Error), Protocol(String) }
#[derive(Debug)]
pub enum InputError { NotFresh, NotControlling, Congestion, Closed, Invalid(String) }

impl Viewer {
    /// Returns only after Accepted (or a typed Rejected / pin / ALPN error). The four
    /// connect* constructors (D/viewer.rs:81-127) collapse into this one.
    pub async fn connect(client: &Client, invitation: &Invitation, options: ConnectOptions, config: ViewerConfig) -> Result<Viewer, ConnectError>;
    pub fn status(&self) -> ViewerStatus;                    // sync, parking_lot only
    pub fn view(&self) -> DesktopView;                       // sync, no allocation (FFI view_v1)
    pub fn video_sink(&self) -> Arc<VideoSink>;
    pub fn request_control(&self, enabled: bool) -> Result<()>;   // Controls barriers, unchanged
    pub fn release_input(&self) -> Result<()>;
    /// Atomic batch (1..=INPUT_BATCH_MAX). Requires fresh && controlling && generation == current.
    /// Rust stamps frame_id = last presented id and consecutive sequences. Lock is Input::Lock.
    pub fn input(&self, generation: u64, inputs: &[Input]) -> Result<(), InputError>;
    pub fn set_quality(&self, preset: QualityPreset);
    pub fn pause(&self);
    pub fn resume(&self);
    pub fn request_keyframe(&self, reason: KeyframeReason) -> bool;  // through the sink limiter
    pub async fn stop(&self) -> Result<()>;
}
```

Deleted:

- `connect_with_codecs`, `connect_with_video_preferences` and
  `connect_with_presentation_feedback`;
- `presented()` (native receipts go through `VideoConsumer`);
- `frames()`, `latest_frame()` and `next_frame()`.

### 6.2 Shared state

```rust
pub(crate) struct ViewerShared {
    pub state: parking_lot::Mutex<ViewerStateSlot>,   // last State + Accepted + local facts (route, paused, stopped reason, notice)
    pub stats: parking_lot::Mutex<ViewerStatsAcc>,    // last host Stats + local counters/histograms
    pub cursor: parking_lot::Mutex<CursorSlot>,       // {generation, x, y, visible, seq}
    pub outbound: Outbound,                           // 6.4
}
```

`Viewer` holds:

- `Arc<ViewerShared>`;
- `Arc<VideoSink>`;
- `Arc<Connection>`, whose `diagnostics()` is re-sampled at ≤ 1 Hz by the
  write tick;
- the cancellation token;
- the supervisor join handle.

### 6.3 Read task (no FFI, no Dart, no decoder on this path)

| Message | Action, in order |
| --- | --- |
| `Video{…}` + payload | Read the payload (size-scaled timeout). Then **`outbound.receipt(seq)` first**, so credit is transport credit, including for units the sink drops. Validate (4.6). If `generation < current` → drop. Otherwise `sink.push(VideoUnit{…, received_at: now})`. Update received fps and bytes. |
| `Probe{nonce}` | `outbound.probe(nonce)` |
| `State{…}` | Validate the display. Update the state slot. On a new generation → `sink.set_generation(g)`. |
| `Cursor{…}` | If `generation == current` → cursor slot with `seq += 1` |
| `Stats{…}` | Stats slot `host = …` |
| `Notice{…}` | State slot `notice = (code, message, now)` |
| `Stopped{reason, message}` | End the session with that reason |
| `Accepted`, `Rejected` | Protocol error (only valid during connect) |

Header reads use `VIEWER_HEADER_TIMEOUT`. A timeout ends the session with
reason `Timeout`.

### 6.4 Write task and `Outbound` priorities (`D/viewer/outbound.rs`)

```rust
pub(crate) struct Outbound {
    controls: Controls,                                        // kept verbatim (D/viewer.rs:28-79): revoke > release > grant; a barrier drains queued input
    slots: parking_lot::Mutex<Slots>, notify: tokio::sync::Notify,
    inputs: parking_lot::Mutex<VecDeque<InputEvent>>,          // ≤ INPUT_QUEUE
    sequence: AtomicU64,                                       // per session; Rust-stamped (Dart owns none)
}
struct Slots { probe: Option<u64>, receipt: Option<u64>, presented: Option<(u64, u64)>, presented_sent: Option<(u64, u64, Instant)>,
               lifecycle: Option<Lifecycle /*Pause|Resume*/>, keyframe: Option<KeyframeReason>, fps_cap: Option<u32>,
               quality: Option<QualityPreset>, route: Option<Route> }
```

Biased order, one message per pick, each one record plus a flush:

| Prio | Source | Notes |
| --- | --- | --- |
| 1 | cancel | — |
| 2 | control barrier | `Control{false}` / `Release` / `Control{true}` from `Controls` |
| 3 | probe echo | latest nonce |
| 4 | receipt | latest seq (cumulative) |
| 5 | input FIFO | **Before writing an `Input` whose `(generation, frame_id)` is newer than `presented_sent`, write `Presented{slots.presented}` first, regardless of the 100 ms throttle (must-fix).** The stamp came from `slots.presented`, so it is always ≥ the event's frame. |
| 6 | lifecycle | `Pause` / `Resume` |
| 7 | presented | throttled to `PRESENTED_SEND_MIN_INTERVAL`; the trailing value is always flushed |
| 8 | keyframe | latest reason |
| 9 | fps cap | latest |
| 10 | quality | latest |
| 11 | path | latest `Route` |
| 12 | ping | when nothing was written for `PING_IDLE` |

**Viewer tick, every 250 ms (inside the write task's `select!`):**

- `sink.tick(now, direct)` → `Option<cap>` → `slots.fps_cap`;
- the recovery watchdog (6.8);
- path re-sample (≤ 1 Hz) → `slots.route` on change;
- the stats roll-up (1 Hz).

`Viewer::pause()` pushes the revoke barrier, then `lifecycle = Pause`, then
calls `sink.pause()`. `Viewer::resume()` sets `lifecycle = Resume`, then
calls `sink.resume()`.

`Viewer::input(gen, inputs)` runs these steps, all under the `inputs` lock:

1. `let p = sink.presented_for_input()?`. It returns `(gen, frame_id)` only
   if `fresh` (6.6), and `NotFresh` otherwise.
2. Require `controlling`. Otherwise `NotControlling`.
3. `gen == p.0`. Otherwise `NotFresh`.
4. **Move coalescing.** If the call is a single `Move` and the FIFO's tail is
   a `Move` of the same generation, the tail is replaced by the new event
   (with its newer sequence; the host requires only strictly increasing
   sequences, 5.12.3). Return `Ok`. Pointer motion can therefore never fill
   the queue, even while the uplink stalls. A tail `Move` is only ever a
   single-event call: every batch that contains a move ends with a key or
   button event, so replacing it never reorders a move past a press.
5. If `inputs.len() + n > INPUT_QUEUE` or the queued encoded bytes would
   exceed `INPUT_QUEUE_BYTES`:
   - push the revoke barrier;
   - set the reason to `InputCongestion`;
   - return `Err(Congestion)`. This fails closed, as `D/viewer.rs:410-418`
     does today.
6. Push `n` events with consecutive sequences and `frame_id = p.1`.
7. `notify`.

**Sizing.** The write task writes one record and one flush per `Input`, so a
synchronous Dart burst lands before it drains. The largest burst any UI path
submits between two frames is 288 events or about 100 KB (UI §10.4 "Burst
bound"): an IME replacement is `backspace(64)`, which is 128 events in 8
batches (UI §10.5.2), followed by keyed text while a latch is active, capped
at 16 characters of at most 10 events each, or by plain text, capped at
64 KiB per call. `INPUT_QUEUE = 512` and `INPUT_QUEUE_BYTES = 256 KiB` leave
headroom for that, so congestion stays reserved for a genuine flood (a
stalled uplink with input still arriving).

### 6.5 `VideoSink` (`D/video_sink.rs`)

```rust
pub struct VideoUnit {
    pub seq: u64, pub generation: u64, pub frame_id: u64, pub codec: VideoCodec,
    pub width: u32, pub height: u32, pub keyframe: bool, pub duplicate: bool,
    pub discontinuity: bool,     // first unit after any drop/generation change/resume: decoder must restart at THIS IDR (no rebuild unless size changed)
    pub pts_us: u64, pub bytes: bytes::Bytes, pub received_at: Instant,
}
pub enum Pull { Unit(VideoUnit), NeedCapacity(usize), Timeout, Paused, Closed }
pub enum AttachError { Closed, AlreadyAttached }

pub struct VideoSink {
    state: parking_lot::Mutex<SinkState>, ready: parking_lot::Condvar,
    closed: AtomicBool, paused: AtomicBool, attached: AtomicU64 /*0 = none, else consumer token*/,
    shared: Weak<ViewerShared>,  // presented/keyframe forwarding; Weak so a late native call never keeps a session alive
}
impl VideoSink {
    pub fn attach(self: &Arc<Self>) -> Result<VideoConsumer, AttachError>;      // exclusive (D9: exactly one consumer)
    // crate-private, read task / write task:
    pub(crate) fn push(&self, u: VideoUnit);            // never blocks
    pub(crate) fn set_generation(&self, g: u64);
    pub(crate) fn pause(&self); pub(crate) fn resume(&self); pub(crate) fn close(&self);   // close: clear, notify_all
    pub(crate) fn tick(&self, now: Instant, direct: bool) -> Option<u32>;  // fps cap decision (6.7) + watchdog (6.8)
    pub(crate) fn presented_for_input(&self) -> Result<(u64, u64), InputError>;
    pub(crate) fn snapshot(&self) -> SinkStats;          // presented fps, intervals, latency parts, drops, decoder
    #[doc(hidden)] pub fn detached_for_test() -> Arc<Self>;   // FFI/native tests without a session
}
pub struct VideoConsumer { sink: Arc<VideoSink>, token: u64 }
impl VideoConsumer {
    /// Blocks on the Condvar only (never tokio). Paused: waits `timeout`, then returns Paused.
    /// A head larger than `capacity` returns NeedCapacity(len) and STAYS queued.
    pub fn next(&self, timeout: Duration, capacity: usize) -> Pull;
    pub fn presented(&self, generation: u64, frame_id: u64) -> bool;          // 6.6
    pub fn keyframe(&self, reason: KeyframeReason) -> bool;                  // through the limiter (6.8)
    pub fn report(&self, decoder: &str, hardware: bool);                     // ≤ 64 chars; overlay
}
impl Drop for VideoConsumer { /* attached = 0; notify_all */ }
```

`SinkState` holds:

- `queue: VecDeque<VideoUnit>` and `bytes`;
- `generation`, `wait_key`, `next_discontinuity`;
- `idr_pulled_at: Option<Instant>` (the latest IDR the consumer took);
- `first_idr_pulled_at: Option<Instant>`, set once by the first take of an
  IDR or JPEG picture (`next` step 5) and never cleared: the consumer's
  decoder starts there;
- `handed: VecDeque<(gen, frame_id, received_at)>`, up to 256;
- `presented: Option<(gen, frame_id, presented_at, received_at)>`;
- `recovery: Recovery` (6.8) and `fps_cap: FpsCap` (6.7);
- `present: PresentStats`: interval histogram, presented fps ring, viewer
  latency samples;
- counters `superseded, gap_drops, backlog_drops, overflow_drops, stale_receipts`;
- `decoder: DecoderInfo`.

**`push(u)` (the only viewer overflow point):**

1. `u.generation != generation` → clear, `generation = u.generation`,
   `wait_key = true`, `next_discontinuity = true`.
2. JPEG → keep only the newest unit (latest-only slot). Push, notify, return.
3. `wait_key && !u.keyframe` → drop (`gap_drops += 1`),
   `request(KeyframeReason::Gap)` (subject to the hold, 6.8), return.
4. `u.keyframe` → drop every queued unit, because they are superseded by an
   independently decodable picture (`superseded += n`). `wait_key = false`.
   `u.discontinuity = next_discontinuity || n > 0`. `next_discontinuity = false`.
5. **Backlog check** (also run at pull). The condition is:
   - `idr_pulled_at` is `Some` and at least `IDR_GRACE` ago. Before the
     consumer takes its first IDR nothing is dropped for age: what queued
     is the hand-off to a consumer still attaching;
   - `queue.len() + 1 ≥ BACKLOG_MIN_UNITS`;
   - head age > `BACKLOG_AGE`.

   When it holds:
   - clear the queue (`backlog_drops += 1`);
   - `wait_key = true`, `next_discontinuity = true`;
   - `request(Backlog)`;
   - `fps_cap.on_backlog_drop()`, only once the cap judges the consumer
     (`first_idr_pulled_at` at least `IDR_GRACE` ago, 6.7);
   - keep `u` only if it is a keyframe (then it carries `discontinuity`).
6. **Overflow** (`len ≥ SINK_MAX_UNITS` or `bytes + len > SINK_MAX_BYTES`):
   handled the same way as backlog, at any time, counted in
   `overflow_drops`. Within the first `IDR_GRACE` after the first IDR pull
   it still clears the queue and requests an IDR, but is not counted by the
   fps cap.
7. Push, then `ready.notify_one()`.

**`next(timeout, cap)`:**

1. Closed → `Closed`.
2. Paused → wait `timeout`, then return `Paused`.
3. Re-run the backlog check against the head.
4. Head length > `cap` → `NeedCapacity(len)` (no pop).
5. Otherwise pop, record `(gen, frame_id, received_at)` in `handed`, and set
   `idr_pulled_at` if it is a keyframe; the first such take also sets
   `first_idr_pulled_at`.

Discontinuity semantics for consumers (R18):

- Drop any natively queued older units.
- Bump the output revision so stale decoded pictures are never shown.
- Feed this IDR to the **existing** decoder.
- Rebuild only on a size change, when there is no codec, or after a decoder
  error.

### 6.6 Presentation validation and freshness (the single viewer enforcement point)

`VideoConsumer::presented(g, id)` (and the JPEG path, which goes through the
same consumer) is accepted iff:

- the sink is not closed and not paused;
- `g == generation`;
- `(g, id)` is in `handed` and was received less than `PRESENTED_FRESH` ago;
- `presented` is `None`, or of another generation, or `id > presented.frame_id`.

When accepted:

- `presented = (g, id, now, received_at)`;
- `present.record(now, now − received_at)`;
- `recovery.on_presented()`;
- `shared.outbound.presented(g, id)` (via `Weak`);
- trace `desktop_frame_view {seq, received_us, handed_us, presented_us}`.

Otherwise `stale_receipts += 1`, return false. A rejected receipt is never
forwarded.

`fresh` requires all of the following. It is used by `Viewer::input`,
`ViewerStatus.fresh` and `DesktopView.flags`.

- `state == Streaming`;
- `!paused`;
- `presented.gen == state.generation`;
- `presented_at.elapsed() < 2 s`;
- `presented.received_at.elapsed() < 2 s`.

The host re-checks independently with `AuthLedger`, so the two ends lock
independently.

### 6.7 fps cap (`D/viewer/fps_cap.rs`) (D8, P1-1)

It runs on every viewer tick (250 ms), **only while `state == Streaming &&
!paused && first_idr_pulled_at.elapsed() >= IDR_GRACE`**. Otherwise it is
frozen: `high_ticks`, `short_since` and the drop flag are cleared. The
judgment is anchored on the consumer's **first** IDR, not on every IDR: a
later IDR (after a backlog drop or a keyframe request) does not restart this
grace, while the age-drop grace in 6.5 still follows the latest
`idr_pulled_at`.

```
present_fps = accepted presentations in the last 1 s
factor      = if direct {0.9} else {0.8}               // RustDesk io_loop.rs:1205-1212
ceiling     = min(limits.max_fps, preset ceiling)
backlog     = queue.len() sampled this tick
decrease if (backlog > 2 on two consecutive ticks) || on_backlog_drop() since last tick:
     new = max(CAP_MIN=5, floor(present_fps × factor)); new = max(new, cap_or_ceiling / 2)   // ≤ 50% fall per event
     if new < cap_or_ceiling && now - last_change ≥ 1 s { cap = Some(new); send }
increase if backlog ≤ 1 continuously for CAP_UP_HOLD=3 s && cap.is_some() && now - last_change ≥ 1 s:
     cap += 5; if cap ≥ ceiling { cap = None; send ceiling } else { send cap }
```

**Why backlog-gated.** RustDesk's `decode_fps` is decode capacity
(`client.rs:3035-3062`). Presented throughput equals the host rate whenever the
phone keeps up. Capping at 90% of throughput unconditionally would ratchet the
host down by 10% each time. Under backlog, throughput equals capacity, so the
gated rule is the RustDesk rule. A static desktop never lowers the cap.

### 6.8 Recovery state machine and keyframe limiter (`D/viewer/recovery.rs`) (D7)

```
Live --decoder error | backlog/overflow drop | gap | stall watchdog--> AwaitingIdr{attempt}
AwaitingIdr --IDR handed and presented--> Live
AwaitingIdr: request at t0 (unless held), then +0.5 s, +1 s, +2 s, +4 s, +4 s … (KEYFRAME_MIN_INTERVAL, ×2, cap 4 s)
any --pause--> Paused (no requests)        Paused --resume--> AwaitingIdr with HOLD_AFTER_DISCONTINUITY (host sends the IDR on Resume)
any --State != Streaming--> HostUnavailable (no requests)
HostUnavailable --Streaming of a new generation--> AwaitingIdr with HOLD (host invalidate sends the IDR)
new generation while Live --> AwaitingIdr with HOLD
AwaitingIdr --DECODER_FAILURE_LIMIT DecoderError reports within 30 s with no presented frame--> Failed
Failed: no requests; ViewerStatus.reason = decoder_failed; the UI offers Retry (reconnect)
```

- `request(reason)` is the only path to `Outbound.keyframe`. It covers native
  `keyframe()`, sink drops and `Viewer::request_keyframe(User)`.
  - `User` bypasses the backoff but not the 500 ms minimum.
  - Every request is dropped while `Paused`, `HostUnavailable` or `Failed`.
- **Stall watchdog** (from Dart `desktop_store.dart:412-425`, now in Rust):
  units were handed in the last 700 ms but none was presented for 700 ms,
  while Live and not paused → `request(Stall)`.
- Any received IDR resets the backoff.

### 6.9 Pause and resume (D13)

**`pause()`:**

- Outbound: revoke barrier, then `Pause`. The host ends control and stops
  capture.
- Sink:
  - `paused = true`; clear the queue;
  - `wait_key = true`, `next_discontinuity = true`;
  - recovery goes to `Paused`;
  - `next` returns `PAUSED`, so native stall checks suspend;
  - the fps cap and the watchdog are frozen.
- Probes are still echoed and pings sent, so the host sees a live, paused
  viewer.

**`resume()`:**

- `Resume`. The host forces an IDR and applies the startup cap.
- Sink:
  - `paused = false`;
  - recovery goes to `AwaitingIdr` with a 1 s hold.
- The first unit is an IDR with `discontinuity`.

### 6.10 `ViewerStatus`, `DesktopView`, and the metric definitions

```rust
#[derive(Clone, Debug, Serialize)]
pub struct ViewerStatus {
    pub state: ViewerState,                       // connecting | streaming | paused | capture_unavailable | stopped
    #[serde(skip_serializing_if = "Option::is_none")] pub stage: Option<ConnectStage>,
    #[serde(skip_serializing_if = "Option::is_none")] pub reason: Option<ViewerReason>,
    // ViewerReason: secure_desktop | helper_unavailable | display_lost | capture_failed | input_failed | control_denied
    //   | input_congestion | stale_presentation | host_stopped | host_ended | restarting | revoked | transport
    //   | protocol | timeout | decoder_failed
    pub message: String,
    pub control_allowed: bool, pub controlling: bool, pub fresh: bool, pub paused: bool,
    pub generation: u64, pub display: Option<Display>, pub desktop: DesktopKind,
    pub video: Option<VideoInfo>,                 // {codec, width, height}
    pub presented: Option<PresentedInfo>,         // {generation, frame_id, age_ms}
    pub last_video_age_ms: Option<u64>,           // since the later of the last Video record (any generation) and the
                                                  // viewer state's last entry into Streaming (a resume, or the host's State);
                                                  // None before the first Video record; Dart's 5 s rule
    pub quality: QualityPreset, pub capabilities: Vec<String>,
    pub notice: Option<NoticeInfo>,               // {code, message, age_ms}
    pub decoder: DecoderInfo,                     // {name, hardware: Option<bool>, state: ok|recovering|failed}
    pub stats: ViewerStats,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ViewerStats {
    pub path: String /*direct|relay|""*/, pub relay_transport: Option<String> /*tcp|quic*/,
    pub rtt_ms: Option<u32>, pub base_rtt_ms: Option<u32>, pub queue_delay_ms: Option<u32>,
    pub picture_latency_ms: Option<u32>, pub host_ms: Option<u32>, pub viewer_ms: Option<u32>,
    pub presented_fps: f32, pub received_fps: f32, pub capture_fps: f32,
    pub pace_fps: u32, pub target_fps: u32, pub fps_cap: u32 /*0 none*/,
    pub bitrate: u32 /*host expected*/, pub encoder_bitrate: u32, pub received_bitrate: u32,
    pub width: u32, pub height: u32, pub codec: VideoCodec,
    pub adaptation: Adaptation, pub rate_model: RateModel,
    pub keyframes: u64, pub keyframes_requested: u64, pub backlog_drops: u64, pub dropped_frames: u64,
    pub present_interval: IntervalStats,          // {p50_ms, p95_ms, p99_ms, max_ms, gaps_100, gaps_250} over the last 60 s; gaps also session totals
    pub degraded: bool,                           // constants table; computed here, once
}
```

**Metric definitions.** The overlay, the acceptance report and the docs all
use these.

| Name | Definition |
| --- | --- |
| `rtt_ms` | Last probe RTT, transport only (host `Stats`) |
| `queue_delay_ms` | Mean of the last 3 untainted probe RTTs minus `base_rtt` |
| `picture_latency_ms` | Median over 2 s of `host_ms + rtt_ms/2 + viewer_ms`. `host_ms` is encoder submit → socket write start (from `Video.host_us`). `viewer_ms` is network receipt → compositor consumption (Rust `Instant`s). Each side uses its own clock, and the network leg is an estimate. The camera check (11) measures glass-to-glass. |
| `presented_fps` | Accepted presentations per second |
| `received_fps` | Non-duplicate `Video` records per second |
| `present_interval` | Intervals between accepted presentations. Buckets 0-8-12-17-20-25-34-50-67-100-150-250-500-∞. |

`DesktopView` is the Rust mirror of `PbDesktopViewV1` (7.4). It is filled by
`Viewer::view()` from the state, cursor and sink slots with read locks only.

### 6.11 Viewer deletions

- `D/viewer.rs` (it becomes `D/viewer/`).
- `media::Queue` use, the status slot and cursor stamping.
- The `presentation_feedback` gate (`D/viewer.rs:206, 231-233, 306-311`).
- The `frames` watch, `latest_frame` and `next_frame`.
- Path sampling once at connect (`:170-188`).
- `Controls` and the test
  `repeated_releases_are_bounded_and_revocation_precedes_a_new_grant` are
  **kept**.

---

## 7. FFI (`pb-mapper-ffi`, WP-F)

### 7.1 `DesktopState` (`F/desktop.rs`)

```rust
pub(crate) struct DesktopState {
    host: tokio::sync::Mutex<Option<Host>>,                        // unchanged
    viewer: parking_lot::RwLock<Option<Arc<Viewer>>>,              // hot ops read; never held across an await
    connecting: parking_lot::Mutex<Option<PendingConnect>>,        // {epoch, cancel, stage: Arc<AtomicU8>}
    viewer_epoch: AtomicU64,
    host_identity: tokio::sync::Mutex<Option<LocalIdentity>>,      // unchanged
    background: Arc<parking_lot::Mutex<BackgroundCache>>,          // unchanged
}
```

- `connect_viewer` keeps its cancellation and epoch logic (`F/desktop.rs:109-153`).
  The `progress` callback writes the `stage` atomic. On success it registers
  `viewer.video_sink()` in the registry (7.7) and stores `Arc<Viewer>`.
- `disconnect_viewer`:
  1. epoch++ and cancel the pending connect;
  2. `viewer.write().take()`, under a short guard;
  3. `registry::remove_source(id)`, which calls `sink.close()`;
  4. `v.stop().await` **with no lock held**. This fixes the guard that today
     keeps the slot locked during `stop()` (`:162-163`).
- `background_status` spawns with `handle.runtime.spawn` instead of
  `tokio::spawn` (`:93`), so it is safe if it is ever reached off-runtime.

### 7.2 Lifecycle JSON ops (`pb_mapper_desktop_call_json`, `block_on`, compute isolate only)

| Op | Change | Request | `data` |
| --- | --- | --- | --- |
| `capabilities` | changed | — | `helper` gains `protocol`, `update_required` |
| `host_status` | **new**, replaces `status` | — | `{host: {status: HostStatus, service: string, invitation: string}\|null, background: BackgroundSnapshot\|null}`. `invitation` is `Host::invitation().encode()` (pbd1) of the running local host; the background snapshot already carries its own `invitation`. The invitation stays out of `HostStatus` itself, so nothing that serialises a status (traces, the background pipe) carries the secret. |
| `status` | **removed** | | |
| `host_enable` | changed | `service, display_id, allow_control` | as today |
| `host_start`, `host_stop`, `host_permission`, `host_restore`, `trust_status`, `trust_revoke`, `helper_action`, `background_action` | unchanged | | invitations are `pbd1` |
| `host_end_session` | **new** (D15) | — | `{}`. Local `Host::end_session()`, or the background pipe `EndSession` |
| `host_set_display` | **new** | `display_id` | `{invitation}`. Restarts the host with the same identity and service on the new display. The current viewer gets `Stopped{restarting}`, which is reconnectable. While background hosting is configured → `INVALID_ARGUMENT` "stop the background host first" (5.16.5). |
| `host_rename` | **new** | `service` | `{invitation}`. Restarts with a new service. The viewer gets `Stopped{host_stopped}`. While background hosting is configured → `INVALID_ARGUMENT`, as above. |
| `inspect_invitation` | **new** (D3, no network) | `invitation` | `{service, name, fingerprint: "AB12-CD34-EF56-7890", format: "pbd1"\|"json_v1"}`. Errors: `SETUP_STRING`, `INVALID_INVITATION`. |
| `connect` | changed | `invitation` **or** `computer_id` (WORKPLAN §1.1), `relay_only=false, quality="balanced", refresh_hz: f64` | `{video_source: u64, codec: "h264"\|"jpeg"}`. Errors: `IDENTITY_CHANGED`, `VERSION_MISMATCH`, `HOST_BUSY`, `ACCESS_DENIED`, `INVALID_INVITATION`, `NOT_FOUND` (no such remembered computer), `VAULT_UNAVAILABLE` (the vault is locked or unreadable), `IO`. |
| `disconnect` | changed | — | `{}` (7.1 order) |
| `control`, `release`, `keyframe`, `presented`, `input` | **removed here** | | moved to the hot export (7.3) or the video exports (7.5) |

### 7.3 Hot export: `pb_mapper_desktop_viewer_call_json`

```c
// Synchronous. Never calls block_on, never takes a tokio lock, never awaits. Safe on the Dart UI isolate.
// Closed op set; anything else -> INVALID_ARGUMENT. Returns the standard JSON envelope (free with the existing string free).
char* pb_mapper_desktop_viewer_call_json(PbMapperHandle* handle, const char* request);
```

| Op | Request | `data` / errors |
| --- | --- | --- |
| `viewer_status` | — | `ViewerStatus`, or `{state:"connecting", stage}` while connecting, or `null` |
| `control` | `enabled` | `{}` |
| `release` | — | `{}` |
| `input` | `generation: u64, inputs: [Input] (1..=16)` | `{}`. Errors: `NOT_FRESH` (stale, paused or generation mismatch), `NOT_CONTROLLING`, `INPUT_CONGESTION` (control was revoked), `INVALID_ARGUMENT`. `{"kind":"lock"}` is D17. |
| `keyframe` | — | `{requested: bool}` (reason `user`) |
| `quality` | `preset` | `{}` |
| `pause` / `resume` | — | `{}` |

### 7.4 Per-vsync view leaf

```c
typedef struct PbDesktopViewV1 {     /* 56 bytes, native layout, align 8 */
    uint64_t generation;             /*  0 */
    uint64_t presented_frame_id;     /*  8  0 = none */
    uint64_t cursor_seq;             /* 16  increments per new cursor */
    uint32_t presented_age_ms;       /* 24  UINT32_MAX = none */
    uint32_t display_width;          /* 28  physical px of `generation` (input space) */
    uint32_t display_height;         /* 32 */
    uint32_t video_width;            /* 36  encoded size */
    uint32_t video_height;           /* 40 */
    int32_t  cursor_x;               /* 44  physical px relative to the display */
    int32_t  cursor_y;               /* 48 */
    uint8_t  state;                  /* 52  0 none, 1 connecting, 2 streaming, 3 capture_unavailable, 4 paused, 5 stopped */
    uint8_t  flags;                  /* 53  bit0 cursor_visible, bit1 controlling, bit2 control_allowed, bit3 secure_desktop, bit4 fresh, bit5 paused */
    uint8_t  reserved[2];            /* 54  zero */
} PbDesktopViewV1;
// 1 = viewer present, 0 = none (out zeroed), -1 = null pointer/panic. Leaf: no allocation, no tokio,
// parking_lot read locks only, catch_unwind. UI isolate only (the isolate that also calls destroy).
int32_t pb_mapper_desktop_view_v1(PbMapperHandle* handle, PbDesktopViewV1* out);
```

A `const _: () = assert!(size_of::<PbDesktopViewV1>() == 56);` guard and a
Dart `ffi.Struct` mirror are required.

### 7.5 Video C exports (Windows runner, Dart JPEG isolate) and the one code space

```c
typedef struct PbDesktopVideoMetaV1 {   /* 56 bytes */
    uint64_t generation;    /*  0 */
    uint64_t frame_id;      /*  8 */
    uint64_t seq;           /* 16  transport seq (trace correlation) */
    uint64_t pts_us;        /* 24 */
    uint64_t age_us;        /* 32  time since network receipt, measured at pop (no cross-language clock) */
    uint32_t width;         /* 40 */
    uint32_t height;        /* 44 */
    uint32_t len;           /* 48  bytes written; required size on NEED_CAPACITY */
    uint32_t flags;         /* 52  bit0 keyframe, bit1 discontinuity, bit2 jpeg (else h264), bit3 duplicate */
} PbDesktopVideoMetaV1;

/* Return codes shared by every video export AND the JNI functions (R19): */
#define PB_VIDEO_UNIT              1   /* unit copied into dst, meta filled */
#define PB_VIDEO_TIMEOUT           0
#define PB_VIDEO_CLOSED           -1   /* source/consumer closed or unknown: consumer must exit */
#define PB_VIDEO_NEED_CAPACITY    -2   /* meta->len = required; the unit STAYS queued */
#define PB_VIDEO_PAUSED           -3   /* viewer paused: suspend stall checks; call again */
#define PB_VIDEO_NOT_ATTACHED     -4   /* consumer id is not the attached one */
#define PB_VIDEO_INVALID          -5   /* bad argument or caught panic */
#define PB_VIDEO_ALREADY_ATTACHED -6

int64_t  pb_mapper_desktop_video_attach_v1(uint64_t source);       /* > 0 consumer id; CLOSED; ALREADY_ATTACHED */
int32_t  pb_mapper_desktop_video_next_v1(uint64_t consumer, uint32_t timeout_ms /* ≤ 1000 */,
                                         uint8_t* dst, uint32_t capacity, PbDesktopVideoMetaV1* meta);
int32_t  pb_mapper_desktop_video_presented_v1(uint64_t consumer, uint64_t generation, uint64_t frame_id); /* 1 accepted, 0 rejected, CLOSED */
int32_t  pb_mapper_desktop_video_keyframe_v1(uint64_t consumer, uint32_t reason); /* 0 decoder_error, 1 gap, 2 backlog, 3 stall, 4 user; 1 queued, 0 limited/paused, CLOSED */
int32_t  pb_mapper_desktop_video_report_v1(uint64_t consumer, const char* decoder_utf8 /* ≤ 64 bytes */, uint32_t flags /* bit0 hardware */); /* 0 ok, CLOSED */
void     pb_mapper_desktop_video_detach_v1(uint64_t consumer);     /* idempotent; wakes a blocked next */
uint64_t pb_mapper_desktop_instance_token_v1(void);                /* address of a static: D9 single-instance check */
```

Rules:

- Every export is a leaf. Video exports take no handle pointer. Each one is
  wrapped in `catch_unwind` (→ `INVALID`) and never uses `unwrap`.
- `next_v1` is the only blocking call. It waits on the sink `Condvar` and
  never touches the tokio runtime. It must never run on a UI thread or on the
  Dart UI isolate.
- `presented_v1` and `keyframe_v1` are non-blocking: a registry lookup plus a
  mutex. They are safe on the Windows raster thread and the Android codec
  thread.
- No ownership crosses the ABI. Every unit is copied into `dst`. The
  `release_v1` export, with its double-release and use-after-release
  hazards, does not exist.

**Synthetic source for native bring-up (cargo feature `desktop-test-source`,
off by default, never enabled by a release script).** Without it nothing
feeds a sink on a device before G2: the old Dart and the shim cannot drive
channel v2, and `detached_for_test()` is Rust-only.

```c
/* Plays a recorded clip into a new detached VideoSink registered as a source.
   The file holds `u32 BE len | Annex-B access unit` records (`video_bench --dump`).
   Generation 1, frame ids 1, 2, 3 …, keyframe = the unit contains NAL type 5,
   received_at = push time, looped at `fps` (1..=120) until stopped or closed.
   Returns the source id (> 0), or 0 on a bad path, file or fps. */
uint64_t pb_mapper_desktop_video_test_source_v1(const char* path_utf8, uint32_t fps);
void     pb_mapper_desktop_video_test_source_stop_v1(uint64_t source);   /* idempotent; closes the sink */
```

Consumers attach to it with the ordinary exports, so the feeder, decoder,
receipts (validated by the sink, forwarded nowhere), `PAUSED` handling and the
snapshot can all run on the phone in Wave 2.

### 7.6 JNI (Android) (`F/desktop_jni.rs`, `#[cfg(target_os = "android")]`)

Dependencies:

- the root `[workspace.dependencies]` gets `jni = "0.21.1"` (already in
  `Cargo.lock` through netdev, so `--locked` stays offline-safe);
- `F/Cargo.toml` gets `[target.'cfg(target_os = "android")'.dependencies] jni.workspace = true`.

The design is pull-only, so there is no `JNI_OnLoad`, no cached `JavaVM` and
no `ndk_context` initialisation.

| Kotlin (`internal object DesktopVideoNative`, `@JvmStatic external`) | Rust symbol (`extern "system"`, `#[unsafe(no_mangle)]`) |
| --- | --- |
| `instanceToken(): Long` | `Java_com_pb_1mapper_ui_DesktopVideoNative_instanceToken(env, _: JClass) -> jlong` |
| `attach(source: Long): Long` | `…_attach(env, _, source: jlong) -> jlong` (same values as `attach_v1`) |
| `next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int` | `…_next(env, _, consumer: jlong, timeout_ms: jint, buffer: JByteBuffer, meta: JLongArray) -> jint` |
| `presented(consumer: Long, generation: Long, frameId: Long): Int` | `…_presented(…) -> jint` |
| `keyframe(consumer: Long, reason: Int): Int` | `…_keyframe(…) -> jint` |
| `report(consumer: Long, decoder: String, hardware: Boolean): Int` | `…_report(…, decoder: JString, hardware: jboolean) -> jint` |
| `detach(consumer: Long)` | `…_detach(…)` |

`next` works like this:

1. Read the direct buffer's capacity (`GetDirectBufferCapacity`).
2. Call `consumer.next(timeout, capacity)`.
3. On `Unit`: copy into `GetDirectBufferAddress`, and fill
   `meta[0..10] = [generation, frameId, seq, ptsUs, ageUs, width, height, len, flags, 0]`
   with `SetLongArrayRegion`.
4. On `NeedCapacity(n)`: `meta[7] = n` and return `NEED_CAPACITY` **without
   popping**.

A non-direct buffer, or a JNI failure, clears any pending exception and
returns `INVALID`. The registry lock is released before any wait.

Threading:

| Call | Thread |
| --- | --- |
| `next` | the `pb-desktop-feed` thread only |
| `presented` | the codec `HandlerThread` |
| `keyframe`, `report` | the codec thread or the feeder |
| `attach`, `detach` | the feeder at start and exit |
| `instanceToken` | any thread |

**D9 single instance, fail closed in all builds:**

- Dart calls `pb_mapper_desktop_instance_token_v1()` and passes the value to
  `create{instance}`.
- Kotlin compares it with `DesktopVideoNative.instanceToken()`, and the
  Windows runner with the symbol resolved from its own module handle.
- On a mismatch, `create` fails with "native video library loaded twice".
- Both tokens are logged at debug level.

### 7.7 Registry and lifetime (`F/desktop_video.rs`)

```rust
static REGISTRY: LazyLock<parking_lot::Mutex<Registry>> = …;
struct Registry {
    next_id: u64,                                        // ids never reused (sources and consumers share the counter)
    sources: HashMap<u64, Arc<VideoSink>>,               // removed on disconnect / DesktopState::drop
    consumers: HashMap<u64, (u64 /*source*/, Arc<VideoConsumer>)>,  // removed on detach; swept on connect if the source is closed
}
```

- Every call clones the `Arc` under the registry lock and releases the lock
  before it waits.
- Ids, never pointers, cross the boundary. A stale id returns `CLOSED`. A late
  native call can never touch freed memory or a replaced session, because
  `VideoSink.shared` is a `Weak`.
- `pb_mapper_destroy` drops `DesktopState`, which removes and closes the
  sources. Native threads therefore exit even if Dart never disconnects.

### 7.8 Error codes (`F/error.rs`)

`ErrorCode` gains the following variants, all `SCREAMING_SNAKE_CASE` on the
wire:

- `VersionMismatch`, `IdentityChanged`;
- `HostBusy`, `AccessDenied`;
- `SetupString`, `InvalidInvitation`;
- `NotFresh`, `NotControlling`, `InputCongestion`;
- `VaultUnavailable` (`connect{computer_id}` when the remembered-computer
  vault is locked or unreadable; `NOT_FOUND` already exists, `F/error.rs:23-50`).

Each one is mapped from a typed error (`ConnectError`, `InvitationError`,
`InputError`), never from message text. That lets `U/state/desktop_store.dart:432`
stop matching substrings.

### 7.9 Temporary shim (`F/desktop_shim.rs`, G1 only; deleted at G2)

It lets the **unchanged** Dart/Kotlin/C++ app run on the new Rust core, so P0
can be measured on the phone before the phone pipeline changes (judged
strength of minimal). It keeps today's exports and ops:

| Old surface | Shim mapping |
| --- | --- |
| `connect{invitation, relay_only, native_video, video}` | `ViewerConfig{codecs: native_video ? [H264] : [Jpeg], limits: from video{max_width,max_height,max_fps} (bitrate ignored), quality: Balanced}`. The shim attaches an internal consumer. |
| `pb_mapper_desktop_wait_frame` / `pb_mapper_desktop_frame` | internal `consumer.next(16 ms, MAX_PAYLOAD)`. It builds the old `DesktopBuffer`: a legacy `SessionStatus` JSON synthesised from `viewer.status()` and the cursor slot (generation, frame_id, video format, cursor, display, state), plus the bytes. Timeout → null. `free_frame` is unchanged. |
| `presented{generation, frame_id}` | `consumer.presented(g, f)` |
| `input{event{generation, frame_id, sequence, input}}` | `viewer.input(generation, &[input])`. Dart's `frame_id` and `sequence` are ignored, because Rust stamps them. |
| `keyframe` | `viewer.request_keyframe(User)` |
| `control`, `release`, `disconnect` | the new viewer calls |
| `status` | `{host: legacy_host_status(HostStatus) (5.15, from G0), viewer: legacy mapping, background}` |

**Routing.** `pb_mapper_desktop_call_json` deserialises one
`#[serde(deny_unknown_fields)] enum Request` (`F/desktop.rs:193-255`), and
during G1-G2 the old Dart still sends the v1 shapes. So the shim is consulted
**before** `Request` is deserialised, after the `computers_*` prefix route:

1. the op names `status`, `control`, `release`, `keyframe`, `presented` and
   `input` go to the shim;
2. a `connect` without `refresh_hz` (which the old Dart never sends, and the
   new Dart always sends) goes to the shim;
3. everything else is deserialised into the new `Request`.

Test (WP-F): `shim_routes_old_and_new_connect_shapes`: an old-shape
`connect{invitation, relay_only, native_video, video}` reaches the shim, a
new-shape `connect{invitation, relay_only, quality, refresh_hz}` reaches the
new op, and neither is rejected by `deny_unknown_fields`. The test is deleted
with the shim at G2.

The shim lives in one file. It is deleted at G2 together with
`pb_mapper_desktop_{frame,wait_frame,free_frame}`, `DesktopBuffer` and
`export_frame` (`F/desktop.rs:1211-1292`). CI on master must never ship it.

### 7.10 FFI deletions and tests

- **Deleted at G2:** the shim, the old frame exports, `DesktopState::wait_frame`,
  `Request::{Status, Control, Release, Keyframe, Presented, Input}` in the
  lifecycle enum, and `Connect.native_video`/`video`.
- **Deleted tests:**
  - `status_and_video_with_the_same_id_both_cross_the_binary_boundary` (`:1319`);
  - `binary_frame_is_owned_until_explicit_release_after_disconnect` (`:1726`).
- **Kept:** the `disconnect_cancels_*` tests (`:1576-1704`), and the
  background and trust tests.

---

## 8. Native consumers

### 8.1 Android (WP-K)

#### 8.1.1 `K/DesktopVideoNative.kt` (new)

```kotlin
package com.pb_mapper.ui
import java.nio.ByteBuffer

internal interface VideoSourceApi {                    // JVM-testable seam
    fun attach(source: Long): Long
    fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int
    fun presented(consumer: Long, generation: Long, frameId: Long): Int
    fun keyframe(consumer: Long, reason: Int): Int
    fun report(consumer: Long, decoder: String, hardware: Boolean): Int
    fun detach(consumer: Long)
}
internal object DesktopVideoNative : VideoSourceApi {
    init { System.loadLibrary("pb_mapper_ffi") }       // same soname Dart dlopen()ed; verified by instanceToken
    const val UNIT = 1; const val TIMEOUT = 0; const val CLOSED = -1; const val NEED_CAPACITY = -2
    const val PAUSED = -3; const val NOT_ATTACHED = -4; const val INVALID = -5; const val ALREADY_ATTACHED = -6
    const val META_GENERATION = 0; const val META_FRAME_ID = 1; const val META_SEQ = 2; const val META_PTS_US = 3
    const val META_AGE_US = 4; const val META_WIDTH = 5; const val META_HEIGHT = 6; const val META_LEN = 7
    const val META_FLAGS = 8; const val META_SIZE = 10
    const val FLAG_KEYFRAME = 1L; const val FLAG_DISCONTINUITY = 2L; const val FLAG_JPEG = 4L; const val FLAG_DUPLICATE = 8L
    const val REASON_DECODER_ERROR = 0; const val REASON_GAP = 1; const val REASON_STALL = 3
    @JvmStatic external fun instanceToken(): Long
    @JvmStatic external override fun attach(source: Long): Long
    @JvmStatic external override fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int
    @JvmStatic external override fun presented(consumer: Long, generation: Long, frameId: Long): Int
    @JvmStatic external override fun keyframe(consumer: Long, reason: Int): Int
    @JvmStatic external override fun report(consumer: Long, decoder: String, hardware: Boolean): Int
    @JvmStatic external override fun detach(consumer: Long)
}
```

If `@JvmStatic external override` does not compile for an `object`
implementing an interface, use a thin adapter object that delegates. The ABI
above is fixed either way.

#### 8.1.2 `K/VideoFeeder.kt` (new): fixes the permit leak

```kotlin
internal interface FeedTarget {
    fun awaitRoom(timeoutMs: Long): Boolean   // true when queued < FEED_QUEUE_MAX (lock + Condition; signalled on EVERY dequeue/clear/drop)
    fun offer(unit: FeedUnit)                 // the only enqueue point; every non-enqueue path returns unit.buffer to the pool and signals
    fun setPaused(paused: Boolean)
    fun failed(message: String)
}
internal class BufferPool(count: Int = 3, initial: Int = 1 shl 20, private val max: Int = 4 shl 20) {
    fun take(): ByteBuffer?; fun give(b: ByteBuffer); fun grow(b: ByteBuffer, needed: Int): ByteBuffer   // direct buffers; next pow2 ≤ max
}
internal class VideoFeeder(private val api: VideoSourceApi, private val consumer: Long,
                           private val target: FeedTarget, private val pool: BufferPool,
                           // Injected: android.os.Process throws "not mocked" in JVM unit tests; tests pass {}.
                           private val raisePriority: () -> Unit = { Process.setThreadPriority(Process.THREAD_PRIORITY_DISPLAY) }
) : Thread("pb-desktop-feed") {
    @Volatile var stopped = false
    override fun run() {
        raisePriority()
        val meta = LongArray(DesktopVideoNative.META_SIZE)
        try {
            while (!stopped) {
                if (!target.awaitRoom(50)) continue
                var buf = pool.take() ?: continue                // 3 buffers ≥ 2 queued + 1 in hand
                when (val r = api.next(consumer, 50, buf, meta)) {
                    UNIT -> { target.setPaused(false); target.offer(FeedUnit.from(buf, meta)) }
                    TIMEOUT -> pool.give(buf)
                    PAUSED -> { pool.give(buf); target.setPaused(true) }
                    NEED_CAPACITY -> pool.give(pool.grow(buf, meta[META_LEN].toInt()))   // same unit on the next call
                    CLOSED -> { pool.give(buf); break }
                    else -> { pool.give(buf); target.failed("video feed error $r"); break }
                }
            }
        } finally { api.detach(consumer) }
    }
}
```

`stop()` sets `stopped` and joins. `next` returns within 50 ms, and `detach`
from another thread wakes it at once.

#### 8.1.3 `K/DesktopVideo.kt` changes

**MethodChannel `pb_mapper/desktop_video` v2** (the handler stays on the main
thread):

| Method | Args | Behaviour |
| --- | --- | --- |
| `create` | `instance: Long` | Token check (8.1.1), which fails closed. Creates the `SurfaceTextureEntry` and the codec `HandlerThread` `pb-desktop-video` at `THREAD_PRIORITY_DISPLAY`. Returns `textureId`. |
| `attach` | `textureId, source: Long` | Stops and joins the old feeder. `revision++`, `clearQueue()`, `acceptedGeneration = -1`, `lastAccepted = 0`, `awaitingKey = true`. The **texture and the last picture persist** (greyed reconnect overlay). Then `api.attach(source)`: a value > 0 starts a `VideoFeeder`; otherwise reply with an error. |
| `detach` | `textureId` | Stops the feeder (which detaches). Keeps the texture. |
| `snapshot` | `textureId, path: String, maxWidth: Int` | D23, below. Replies `Boolean` within 300 ms. |
| `stats` | `textureId` | Kept: decoder name, counters, timings. |
| `dispose` | `textureId` | Stops the feeder, then today's dispose. |
| **removed** | `submit`, `reset`, native → Dart `event` | Presentation and keyframes go to Rust. Decoder health reaches Dart through `viewer_status.decoder`. |

`viewers` becomes a `ConcurrentHashMap`, because feeders call back.

`FeedTarget.offer(unit)` runs on the feeder thread under `lock`. It replaces
`submit` (`:217-252`) and drops the `MethodChannel.Result`:

```
if closed || unit.generation < acceptedGeneration            -> drop(unit); return
if unit.generation != acceptedGeneration || unit.discontinuity:
       revision++; clearQueue(); acceptedGeneration = unit.generation; lastAccepted = 0; awaitingKey = true
if unit.frameId <= lastAccepted                              -> drop(unit); return
lastAccepted = unit.frameId
if awaitingKey && !unit.keyframe -> drop(unit); api.keyframe(consumer, REASON_GAP); return
awaitingKey = false; queue.addLast(unit.copy(revision = revision.get())); schedulePump(0)
drop(unit) = pool.give(unit.buffer); dropped++; signal room          // every path signals room: no leak possible
```

- The `queue.size >= 4` overflow branch is deleted. It is unreachable with
  depth 2, and the Rust sink is the only overflow point.
- The pump copies from the pooled buffer into the codec input buffer and
  returns the pooled buffer **after** `queueInputBuffer`, then signals room.

**Configure rule (R18).** Replace the condition in `pump()`
(`current.revision != packet.revision || size…`) with:

```kotlin
val needsConfigure = codec == null || active == null || active.width != packet.width || active.height != packet.height
```

- A stale-revision packet is still discarded, as today.
- A same-size discontinuity feeds the IDR to the existing codec. Its
  in-band SPS/PPS resets the references.
- Outputs of older revisions are released without rendering, as today
  (`renderLatest`).
- `fail()` still releases the codec, so the next IDR configures.

**Receipts.** `consumeSurface()` (`:180-215`) calls
`api.presented(consumer, packet.generation, packet.frameId)` where it emits
`event(packet, true, false)` today. The exact-timestamp consumption semantics
are unchanged. Impeller stays disabled (P1-4).

**Recovery.**

- `fail(packet, reason)` does `releaseCodec()`, `revision++`, `clearQueue`,
  `awaitingKey`, and then `api.keyframe(consumer, REASON_DECODER_ERROR)`
  unless paused.
- A successful `configure` calls
  `api.report(consumer, codec.name, !codec.codecInfo.isSoftwareOnly /*API ≥ 29, else name heuristic*/)`.

**Pause.** `setPaused(true)` comes from the `PAUSED` code. While paused:

- `pump()` skips the two 2 s checks (`:524-532`): "texture was not consumed"
  and "decoder stalled";
- `fail()` does not request keyframes.

The next `UNIT` clears paused. The resume IDR carries `discontinuity`.

**Decoder hints** (`const val DECODER_HINTS`, kept only if 11.3 case F shows a
gain): `KEY_PRIORITY = 0` and `KEY_OPERATING_RATE = 60` in `configure`.

**Snapshot (D23).**

1. Post to the codec worker. With nothing presented, or if `PixelCopy` is not
   possible → `false`.
2. `PixelCopy.request(surface, Bitmap(w', h'), listener, workerHandler)`, with
   `w' = min(maxWidth, active.width)` and the aspect preserved (API 24 =
   minSdk).
3. `bitmap.compress(JPEG, 80)` to `path.tmp`, then fsync, then rename to
   `path`. The path is app-private and chosen by Dart.
4. Reply on the main thread. Failure or more than 300 ms → `false`.

It never blocks `dispose`. `PixelCopy` on a SurfaceTexture-backed producer is
risk R6.

`MainActivity` is unchanged. `EnableImpeller=false` stays.

### 8.2 Windows runner (`W/desktop_video.cpp`, `W/desktop_video.h`, WP-WIN)

**`create{instance}`:**

- `GetModuleHandleExW(0, L"pb_mapper_ffi.dll", &module_)`. It finds the copy
  Dart already loaded (Dart passes the instance token, so the DLL is loaded by
  the time `create` runs), takes a reference that pins it, and can never load
  a second copy. `FreeLibrary` runs in `~Impl`. Not loaded → `create` fails
  ("pb_mapper_ffi.dll is not loaded").
- `GetProcAddress` for the seven `pb_mapper_desktop_video_*_v1` symbols and
  `pb_mapper_desktop_instance_token_v1`.
- Any missing symbol → `create` fails ("pb_mapper_ffi.dll does not match this
  app").
- A token mismatch → `create` fails ("native video library loaded twice").
- The runner still does not link the DLL at build time.

**Channel:** `create`, `attach{textureId, source}`, `detach`,
`snapshot{textureId, path, maxWidth}`, `stats`, `dispose`. `submit`, `reset`
and the coalesced `event` are removed. `Events` stays only for the snapshot
replies (`kWake` + `HandleMessage` on the platform thread).

**`Attach(source)`:**

1. Detach and join the old decoder thread (`detach_v1` wakes it).
2. Reset `generation_ = -1, last_ = 0, awaiting_key_ = true`. **Keep
   `latest_`**, so the last picture stays during a reconnect.
3. `consumer_ = attach_v1(source)`.
4. Start `Run()`.

**`Run()`:**

```cpp
std::vector<uint8_t> buf(1 << 20); PbDesktopVideoMetaV1 meta{};
for (;;) {
  int32_t r = next_(consumer_, 50, buf.data(), (uint32_t)buf.size(), &meta);
  if (r == PB_VIDEO_UNIT)          { paused_ = false; Accept(buf.data(), meta); }   // generation/awaiting-key/discontinuity rules from Submit (:144-180)
  else if (r == PB_VIDEO_NEED_CAPACITY) { buf.resize(meta.len); }                   // same unit next call (≤ 4 MiB)
  else if (r == PB_VIDEO_TIMEOUT)  { }
  else if (r == PB_VIDEO_PAUSED)   { paused_ = true; }
  else break;                                                                       // CLOSED / NOT_ATTACHED / INVALID
  if (snapshot_requested_.exchange(false)) ServiceSnapshot();                       // after EVERY return, UNIT included
}
detach_(consumer_);
```

`snapshot_requested_` is a `std::atomic<bool>` set by the `snapshot` channel
call. It is checked after every `next` return, because with steady video
`next` returns `UNIT` every frame and never `TIMEOUT`: a snapshot serviced
only on `TIMEOUT`/`PAUSED` would always miss its 300 ms bound on a normal
`disconnect({snapshotPath})`, which snapshots while streaming. The snapshot
copies `latest_` on this decoder thread, between two `next` calls.

- `Accept`:
  - `discontinuity` → `++revision_` and drop nothing else;
  - the decoder is recreated only on a size change or after `Fail`;
  - `Feed` copies through `MFCreateMemoryBuffer`, then `Take` → `latest_` →
    `MarkTextureFrameAvailable` (unchanged).
- **Receipts.** `AcquireSurface()` (raster thread, `:196-222`), on the first
  acquisition of a new `(revision, frame)`, calls
  `presented_(consumer_, gen, frame)` instead of `events_->Send`.
- **Failures.** `Fail` calls `keyframe_(consumer_, 0)` unless `paused_`.
  Decoder creation calls `report_(consumer_, "Media Foundation DXVA (<adapter>)", 1)`.
- **Snapshot** (on the decoder thread, between `next` calls, ≤ 300 ms):
  1. `VideoProcessorBlt` or `CopyResource` from `latest_` to a staging
     texture, box-downscaled to ≤ `maxWidth`;
  2. `Map`;
  3. WIC `CLSID_WICJpegEncoder` (quality 0.8) to `path.tmp`, then
     `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`;
  4. reply through `Events`.

  `CMakeLists.txt` links `windowscodecs`.
- **Deleted:** `Submit`, `queue_`, `Submission`, `Events::replies` (apart
  from the snapshot replies), `Complete`, the `>= 4` overflow, and `Reset`.
- **Optional, measure first:** a pool of 3 BGRA shared textures, respecting
  `SurfaceLease`, instead of one `CreateTexture2D` per frame (`:433-441`).

---

## 9. Dart (WP-D)

### 9.1 FFI plumbing

**`U/ffi/pb_mapper_ffi.dart`** adds these lookups:

- `pbMapperDesktopViewerCall`;
- `pbMapperDesktopView` (with a `PbDesktopViewV1` `ffi.Struct`);
- `pbMapperDesktopVideoAttach/Next/Presented/Keyframe/Detach`, with
  `PbDesktopVideoMetaV1`;
- `pbMapperDesktopInstanceToken`.

It removes `pbMapperDesktopFrame/WaitFrame/FreeFrame` and `DesktopBuffer`
(`:200-226`).

**`U/ffi/pb_mapper_service.dart`:**

- Gains `desktopViewerCallSync(Map request)`. It calls the hot export on the
  **calling** isolate, the UI isolate, and returns the decoded envelope.
- Gains `desktopView(Pointer<PbDesktopViewV1> scratch)`, which reuses one
  `calloc`'d struct per store.
- Gains `instanceToken()`.
- Deletes `_desktopFramesWorker`, `_desktopFrameIsolate`, `DesktopFrameStream`,
  `desktopFrames`/`desktopFrame`, and the routed-op set
  (`presented|keyframe|input|control|release`, `:288-301`).
- Adds `_desktopJpegWorker((int source, SendPort port))` for JPEG
  platforms only:
  - `attach_v1(source)`;
  - loop `next_v1(consumer, 50 ms)` into a 4 MiB `malloc` buffer, growing on
    `NEED_CAPACITY`;
  - send `TransferableTypedData` plus meta to main, with one outstanding
    credit (main replies after decode);
  - `detach_v1` on `CLOSED` or exit;
  - it holds no handle pin.

### 9.2 `DesktopApiClient` (`U/ffi/desktop_api.dart`)

```dart
enum DesktopQuality { quality, balanced, smooth }          // wire names identical
enum DesktopInvitationFormat { pbd1, jsonV1 }

abstract interface class DesktopApiClient {
  // ---- lifecycle export (async; PbMapperService compute isolate)
  Future<DesktopCapabilities> capabilities();               // helper.protocol, helper.updateRequired
  Future<DesktopHostSnapshot> hostStatus();                 // {host: DesktopHostStatus?, background: DesktopBackground?};
                                                            // DesktopHostStatus carries the FFI siblings service and invitation (7.2)
  Future<DesktopHostEnable> enableHost(String service, int displayId, {required bool allowControl});
  Future<String> startHost({required String service, required int displayId, required bool allowControl,
      bool relayOnly = false, bool privileged = false, bool remember = false, bool autoStart = false, bool background = false});
  Future<void> stopHost({bool onExit = false});
  Future<void> allowControl(bool allow);
  Future<void> endViewerSession();                          // host_end_session (D15)
  Future<String> setHostDisplay(int displayId);             // host_set_display -> new invitation
  Future<String> renameHostService(String service);         // host_rename -> new invitation
  Future<DesktopTrust> trustStatus();
  Future<String?> restoreHost();
  Future<void> revokeTrust();
  Future<bool> helperAction(String action);
  Future<DesktopBackground> backgroundAction(String action);
  Future<DesktopInvitationInfo> inspectInvitation(String text);   // throws DesktopCallException(SETUP_STRING | INVALID_INVITATION)
  Future<DesktopSession> connect(String invitation, {required bool relayOnly, required DesktopQuality quality, required double refreshHz});
  Future<void> disconnect();
  // ---- hot export + leaf (sync; UI isolate only; never block)
  DesktopViewerStatus? viewerStatus();
  DesktopView? view();                                      // pb_mapper_desktop_view_v1
  void control(bool enabled);
  void release();
  void input(int generation, List<Map<String, Object?>> inputs);   // throws DesktopCallException(NOT_FRESH | NOT_CONTROLLING | INPUT_CONGESTION)
  bool requestKeyframe();
  void setQuality(DesktopQuality quality);
  void pause();
  void resume();
  int instanceToken();
  // ---- JPEG consumer (non-Android/Windows only)
  Stream<DesktopJpegFrame> jpegFrames(int videoSource);     // frame carries consumer id
  bool presentedJpeg(int consumer, int generation, int frameId);
}
class DesktopSession { final int videoSource; final String codec; }
class DesktopInvitationInfo { final String service, name, fingerprint; final DesktopInvitationFormat format; }
```

The remembered-computer vault (D20) is **not** part of this interface. The UI
contract owns it in its own file and interface (`RemoteComputerVault`). It
hands `DesktopStore.connect` an invitation string. If the UI contract keeps
secrets in Rust and connects by id, it adds an additive `computer_id` field to
the `connect` op in a separate commit. That is integration point I10.

`DesktopApi` implements every member. `FakeDesktopApi`
(`ui/test/fake_desktop_api.dart`) implements every member with a scriptable
`viewerStatus`, `view` and call log. The interface makes a missing member a
compile error.

### 9.3 `DesktopVideoTexture` (`U/ffi/desktop_video.dart`)

```dart
abstract interface class DesktopVideoTexture {
  int get textureId;
  Future<void> attach(int videoSource);     // reconnect re-binds; the texture and last picture persist
  Future<void> detach();
  Future<bool> snapshot(String path, {int maxWidth = 320});   // D23; caller time-boxes to 300 ms
  Future<Map<String, Object?>> stats();
  Future<void> dispose();
}
typedef DesktopTextureFactory = Future<DesktopVideoTexture> Function({required int instanceToken});
```

The default factory creates the channel-backed texture. Tests inject a fake,
so the old CI-skipped lifecycle tests run on Linux. `submit`, `reset`, the
events stream and `DesktopVideoEvent` are deleted.

### 9.4 Value types (`U/state/desktop_models.dart`, new; all `@immutable` with `==`)

WORKPLAN §1.1 amends these types for the UI requests R-UI-1 to R-UI-10
(`loaded`, `configured`, `DesktopHostViewer.since`, the typed notice, a
nullable `DesktopTarget.invitation` with `computerId`). The amended shapes
are authoritative.

**Host invitation and `configured` (R-UI-1).** The store keeps no invitation
of its own (today it caches the strings returned by start, enable, restore
and background, `U/state/desktop_store.dart:63, 210-329`). It keeps only the
last `hostStatus()` and `trustStatus()` results, and derives
`DesktopHostView.invitation` from them on every `hostView` update, first
match wins:

1. `hostStatus().host.invitation`, while a local host runs;
2. `hostStatus().background.invitation`, while background hosting is
   configured;
3. `trustStatus().saved.invitation`, when `trust.matchesRelay` (the FFI
   returns null otherwise, WORKPLAN §1.1);
4. otherwise null.

`configured = (trust.saved != null && trust.matchesRelay) || background?.configured == true`.
A trust saved under another relay or namespace is therefore "not enabled",
never a paused card whose Start `host_restore` would refuse.

`DesktopHostView.viewer` comes from the local `HostStatus.viewer`, else from
the background snapshot's `host.viewer` (5.16.5), so the D15 surfaces and the
tray see a background host's viewer too.

Invariant: `sharing || configured ⇒ invitation != null`. The store re-reads
`hostStatus()` (and `trustStatus()` where WORKPLAN §1.1 lists it) before it
clears `enabling` or `restarting` after enable, start, restore, set-display,
rename or a background action, so no published `DesktopHostView` breaks it.
Test: "a sharing or configured host always has its invitation" (10.4).

```dart
enum DesktopPhase { idle, connecting, live, recovering, waitingForDesktop, paused, reconnecting, identityChanged, ended }
enum DesktopSetupStep { finding, verifying, authorizing, firstPicture }   // Rust stages path|identity|authorizing, then Dart's first picture
enum DesktopEndReason { hostStopped, disconnectedByHost, revoked, versionMismatch, denied, invalidInvitation, decoderFailed, failed }
enum DesktopAdaptation { startup, steady, raising, loweringFps, loweringBitrate, unresponsive, paused }
enum DesktopDemand { page, viewer }

class DesktopTarget { final String invitation, name; final String? computerId; final bool relayOnly; final DesktopQuality quality; }
class DesktopFailure { final String code; final String message; }      // FFI ErrorCode or ViewerReason
class DesktopSessionView {
  final DesktopTarget? target; final DesktopPhase phase; final DesktopSetupStep? setup;
  final int reconnectAttempt; final DateTime? retryAt; final DesktopEndReason? end; final DesktopFailure? failure;
  final bool controlAllowed, controlWanted, controlling, fresh, secureDesktop, hasPicture, paused;
  bool get inputLive => fresh && controlling;
  final String? path; final String? notice;                              // e.g. lock_failed text for NOTICE_TTL
  final List<String> capabilities;                                        // "lock"
}
class DesktopGeometry { final int epoch, generation, displayWidth, displayHeight, videoWidth, videoHeight; }  // epoch++ on generation/display/reconnect
class DesktopPicture { final int? textureId; final double aspectRatio; final String codec; }
class DesktopView {      // mirror of PbDesktopViewV1
  final int generation, presentedFrameId, presentedAgeMs, cursorSeq, displayWidth, displayHeight, videoWidth, videoHeight, cursorX, cursorY;
  final int state; final bool cursorVisible, controlling, controlAllowed, secureDesktop, fresh, paused;
}
class DesktopStats {     // ≤ 1 Hz, from ViewerStatus.stats + decoder
  final String path; final String? relayTransport;
  final int? rttMs, baseRttMs, queueDelayMs, pictureLatencyMs;
  final double presentedFps, receivedFps; final int paceFps, targetFps, fpsCap;
  final int bitrateBps, receivedBitrateBps; final int width, height; final String codec;
  final String? decoder; final bool? hardwareDecoder;
  final DesktopAdaptation adaptation; final bool degraded, paused;
  final int keyframes, backlogDrops; final DesktopIntervals presentInterval;   // p50/p95/p99/max ms, gaps100, gaps250
}
class DesktopHostView {
  final bool supported, sharing, enabling, restarting, background, controlAllowed;
  final String? service, invitation, error;
  final List<DesktopDisplay> displays; final int? displayId;
  final DesktopHelperState helper;                                       // none | ready | stopped | outdated (update_required) | unverified
  final DesktopHostViewer? viewer;                                       // D24
}
class DesktopHostViewer { final bool controlling, paused; final String path; final Duration connectedFor; final DesktopQuality quality; }
sealed class DesktopInput { Map<String, Object?> toJson(); }             // U/state/desktop_input.dart
//  move(x, y) | button(RemoteButton, down) | wheel(h, v) (|v| ≤ 100) | key(RemoteKey, down) | text(String ≤ 1024 B) | lock()
//  RemoteKey covers the D17 vocabulary with the wire names of 4.5.
```

### 9.5 `DesktopStore` public surface (authoritative; `U/state/desktop_store.dart`)

`DesktopStore` **stops extending `ChangeNotifier`**. Widgets listen to slices.
Each `ValueNotifier` changes only on `!=`.

```dart
class DesktopStore {
  DesktopStore(DesktopApiClient api, {DesktopTextureFactory? textures, DateTime Function()? now,
               void Function()? onRestartReady});

  // ---------- host side
  ValueListenable<DesktopHostView> get hostView;                 // on change only
  Future<void> restoreOnStartup();
  Future<void> enableHost({required int displayId, required bool allowControl, String? service, bool ordinaryOnly = false});
  Future<void> resumeHost();                                     // restoreHost / background start, unified
  Future<void> stopHost();                                       // bypasses busy
  Future<void> allowControl(bool allow);                         // D24 live "Make view-only"
  Future<void> endViewerSession();                               // D15 Disconnect
  Future<void> setHostDisplay(int displayId);
  Future<void> renameHostService(String service);
  Future<void> revokeTrust();
  Future<bool> helperAction(String action);

  // ---------- demand (D14 polling)
  DesktopDemandHandle demand(DesktopDemand d);                   // .dispose(); `viewer` demand = viewer on screen (resume), release = pause

  // ---------- viewer session
  ValueListenable<DesktopSessionView> get session;               // phase/target/control/setup/failure; on change
  ValueListenable<DesktopGeometry?> get geometry;                // generation/display/video size; never per frame
  ValueListenable<DesktopPicture?> get picture;                  // native texture (survives reconnect)
  ValueListenable<ui.Image?> get jpegImage;                      // JPEG platforms only
  ValueListenable<DesktopStats> get stats;                       // ≤ 1 Hz, sampled only while it has listeners
  ValueListenable<DesktopQuality> get quality;
  DesktopView? readView();                                       // per frame; non-blocking leaf (cursor, freshness, flags)
  Future<DesktopInvitationInfo> inspectInvitation(String text);
  Future<void> connect(DesktopTarget target);
  Future<void> disconnect({String? snapshotPath});               // best-effort snapshot first (≤ 300 ms), never blocks
  Future<bool> capturePicture(String path);                      // D23 primitive
  void retryNow();
  void control(bool enabled);
  void release();
  void requestKeyframe();                                        // "Refresh picture"
  void lockRemote();                                             // D17: sends DesktopInput.lock()
  bool send(DesktopInput input, {required int geometryEpoch});   // false = dropped (not live / epoch mismatch)
  bool sendSequence(List<DesktopInput> inputs, {required int geometryEpoch});   // atomic, ordered, all-or-release (≤ 16)
  void setQuality(DesktopQuality q);                             // live op; never reconnects
  void dispose();
}
```

### 9.6 Store policies

#### 9.6.1 Polling and notification (D14)

| Source | When it runs | Rate | Updates |
| --- | --- | --- | --- |
| `api.view()` leaf | `viewer` demand held | every rendered frame (persistent frame callback) plus a 33 ms timer that calls `scheduleFrame()` only if `cursorSeq`, `state`, `generation` or `flags` changed | `readView()` cache; `geometry` and `session` when generation, flags or state change |
| `api.viewerStatus()` (hot, sync) | session exists | 500 ms with a `viewer` demand, else 1 s | `session` (phase, setup, failure, notice), `quality`; `stats` only while `stats` has listeners (≤ 1 Hz) |
| `api.hostStatus()` (lifecycle) | `page` demand: 1 s; sharing without demand: 2 s (the D15 bar must see a viewer arrive) | — | `hostView` |
| anything else | — | **no timer when idle** | — |

- `AppStore.desktop` drops `..addListener(_notify)`
  (`U/state/app_store.dart:40-42`). The rail dot, the D15 bar and the tray
  listen to `desktop.hostView`.
- The `Texture` subtree listens only to `picture` and `geometry`, so it never
  rebuilds at media rate.

#### 9.6.2 Phase derivation and reconnect policy

| Phase | Condition |
| --- | --- |
| `connecting` | Connect in flight, with `setup` from `stage`. Or streaming with no presented frame yet (`setup = firstPicture`). |
| `live` | Streaming, `presented` exists and `decoder.state == ok` |
| `recovering` | Streaming and `decoder.state == recovering`, or not fresh while video flows or the session just re-entered streaming (`last_video_age_ms < 1000`, which also reads true for up to 1 s after any entry into streaming; the decoder is then awaiting an IDR anyway) |
| `waitingForDesktop` | `state == capture_unavailable` (reason `secure_desktop`, `display_lost`, …) |
| `paused` | The `viewer` demand was released, or the app is hidden |
| `reconnecting` | The backoff timer is armed (`retryAt`) |
| `identityChanged` | `IDENTITY_CHANGED`. No retry. The stored invitation is never touched. |
| `ended` | A terminal failure (`end` set). No auto-retry. |

**Reconnect** (backoff 1, 2, 4, 8, 15 s, kept from `desktop_store.dart:452-455`)
happens only when:

- the viewer is `stopped` with reason `transport`, `timeout`,
  `capture_failed` or `restarting`;
- or it is `stopped` with reason `protocol`, at most `PROTOCOL_RETRY_LIMIT`
  (3) times in a row; the next one is terminal (`ended`, failure code
  `PROTOCOL`), so a deterministic parse failure cannot loop. A `live`
  resets the count;
- or connect fails with `IO` or `HOST_BUSY`;
- or `last_video_age_ms ≥ NO_VIDEO_RECONNECT` while the state is `streaming`,
  not paused, and `decoder.state != failed`. Because `last_video_age_ms`
  restarts at every entry into streaming (6.10), this rule never counts time
  spent paused or in `capture_unavailable`.

A resume (9.6.4) that finds the session `stopped` with `transport` or
`timeout` (the process was frozen in the background, risk R13) reconnects at
once, with no backoff wait for that first attempt; the texture and its last
picture stay under the reconnect overlay.

It **never** reconnects:

- because the picture is not fresh while video flows (R31);
- while paused;
- while in `capture_unavailable`.

**Terminal** (→ `ended` or `identityChanged`): `VERSION_MISMATCH`,
`ACCESS_DENIED`, `INVALID_INVITATION`, `SETUP_STRING`, `IDENTITY_CHANGED`,
`NOT_FOUND`, `VAULT_UNAVAILABLE`, `host_stopped`, `host_ended`, `revoked` and
`decoder_failed`, plus any error code not listed above as retryable. The
`decoder_failed` end shows Retry, and `retryNow()` reconnects.

**`connect(target)` while something else runs.** Today `connect` returns
silently when `busy` or an attempt runs (`U/state/desktop_store.dart:874-876`),
and the FFI refuses a second connect ("desktop connection is already pending",
"disconnect the current desktop first", `F/desktop.rs:117-131`). The UI
switches computers by disconnecting and connecting without awaiting
(UI §5.4), so `connect(target)` has supersede semantics:

1. cancel the in-flight attempt and the retry timer, and reset
   `reconnectAttempt`;
2. await the previous `disconnect` (its own or one already running);
3. connect the new target.

It is never gated by the host side's `busy`. Test: "a second connect during
an attempt connects the new target" (10.4).

Each reconnect does the following:

- creates a new `Viewer` in Rust, with the sequence reset in Rust;
- `_inputEpoch++` and `geometry.epoch++`;
- `texture.attach(newSource)` on the same texture.

No input is replayed (regression guard).

The deleted Dart behaviours are the 700 ms keyframe watchdog, the 3 s
not-fresh reconnect and the `'desktop identity changed'` substring match
(`:412-432`).

#### 9.6.3 Input

- `send` and `sendSequence` run these checks first:
  - `session.inputLive`;
  - `geometryEpoch == geometry.epoch`.

  If either fails → return `false`. Dart owns no `frame_id` and no
  `sequence`.
- Then the call is `api.input(geometry.generation, [json…])`, synchronous.
- Pointer moves are coalesced to at most one per 4 ms. A pending move is
  flushed before any non-move event, which preserves order.
- `NOT_FRESH` → return `false` silently.
- `NOT_CONTROLLING` / `INPUT_CONGESTION` / other → `_interruptInput()`, which
  ends control locally as today.
- `_inputEpoch` hygiene is kept. `_inputTail`, `_inFlight` and `_PendingMove`
  are deleted, because calls no longer queue in Dart.
- Control is re-requested only after `session.fresh`, as today
  (`desktop_store.dart:394-410`).

#### 9.6.4 Pause and resume (D13)

The store owns an `AppLifecycleListener`. Effective visibility is:

```
visible = viewer demand held && lifecycle ∈ {resumed, inactive}
```

- **On becoming hidden:**
  1. `api.pause()` (Rust pushes revoke, then `Pause`);
  2. `_inputEpoch++`;
  3. slow the status poll to 1 s and stop the view timer.
- **On becoming visible:** `api.resume()`, and restart the view polling.

No keyframe requests, no reconnects and no Dart watchdogs run while hidden.

A paused session still holds the host's only slot (`Semaphore(1)`, 5.3). The
store has no timer for that; the UI ends a session left paused by "Back to
list" after `kPausedSessionTimeout` (10 min, UI §5.4, §10.1), and the host
side shows "Connected · paused" from `HostViewer.paused` (UI §4.6).

On Android the process can be frozen while in the background (risk R13).
Then the write task stops pinging, the host closes after `HOST_READ_TIMEOUT`,
and the resume finds the session `stopped{transport|timeout}`, which
reconnects at once (9.6.2).

Windows minimise and hide-to-tray (the close button, `windowManager.close()`)
must both report `AppLifecycleState.hidden`. Verify this. The fallback (R8) is
that the runner forwards `WM_SIZE/SIZE_MINIMIZED` and the hide on the
`desktop_video` channel as `visibility`.

#### 9.6.5 Thumbnails, textures, JPEG

- **Native texture.** It is created once on the first connect and kept
  across reconnects (`attach`). It is disposed on `disconnect()` or
  `dispose()`, and a texture that arrives late after a busy disconnect is
  disposed too (kept test).
- **`capturePicture(path)`:**
  - native → `texture.snapshot(path).timeout(300 ms)`, with `false` on
    timeout;
  - JPEG platforms → write the current `ui.Image` as PNG
    (`toByteData(format: ImageByteFormat.png)`). D23 allows JPEG or PNG, and
    Flutter has no JPEG encoder. This is best effort, and the same 300 ms
    bound applies.
- **`disconnect({snapshotPath})`** calls `capturePicture` only when a path is
  given and `session.fresh`. The UI contract decides "remembered and no
  identity mismatch".
- **JPEG.** `jpegFrames` → `instantiateImageCodec` → `jpegImage` → a
  post-frame callback → `presentedJpeg(consumer, g, f)`, then return the
  worker credit.

### 9.7 Mapping to the two UI designs (for the UI contract)

| `ui-structure` §3.1 name | `ui-interaction` §4.2 name | This contract |
| --- | --- | --- |
| `hostView` | `sharing`, `hostChanges`, `hostAction`, `hostError` | `hostView` (all fields inside `DesktopHostView`; `enabling`/`restarting` = host action, `error`) |
| `session` | `session`, `viewerState` | `session` (`DesktopSessionView` carries target, phase, setup, end, failure, control flags) |
| `geometry` | `view`, `picture` | `geometry` + `picture` + `readView()` |
| `stats` | `stats` | `stats` (no `ipFamily`; `hardwareDecoder` provided) |
| `textureId` | `picture.textureId` | `picture.textureId` |
| `jpegFrame` | `jpegImage` | `jpegImage` |
| `latestCursor()` | `view` | `readView()` (cursor fields) |
| `demand(page\|viewer)` | `retain(remotePage)`, `attachViewer/detachViewer` | `demand(DesktopDemand.page\|viewer)` |
| `setViewerForeground` | `attachViewer/detachViewer` | `viewer` demand plus the store's own lifecycle listener |
| `connect(DesktopConnectRequest)` | `connect(DesktopTarget)` | `connect(DesktopTarget)` |
| `disconnect({File? snapshotTo})` | `disconnect()` + `capturePicture(path)` | both: `disconnect({snapshotPath})` and `capturePicture(path)` |
| `lockRemote()` | `lock()` | `lockRemote()` |
| `endViewerSession()` | `disconnectViewer()` | `endViewerSession()` |
| `setDisplay`, `renameService` | `setHostDisplay`, `renameHostService` | `setHostDisplay`, `renameHostService` |
| `resumeSharing()` | `resumeHost()` | `resumeHost()` |
| `send(input, geometryEpoch)`, `sendSequence` | `send(Map)`, `sendSequence(List<Map>)` | typed `DesktopInput` + `geometryEpoch` |
| `degraded` threshold | `stats.degraded` | computed in Rust (constants table) |

### 9.8 Dart deletions

- In `DesktopStore`:
  - `ChangeNotifier` and `addListener`/`notifyListeners`;
  - `frame` (H.264), `_frames`, `_consumeFrames`, the H.264 half of
    `_acceptFrame`, `_presentVideo`, and the presentation half of
    `_flushFeedback`;
  - `_videoStatuses`, `_sequence`, `videoPresentation`/`videoPresented`,
    `cursorStatus`, `acknowledgePresented` (replaced by `presentedJpeg`);
  - `presentFps`/`pictureAge`, and `frameAckMs`/`delayed` (now
    `stats.degraded`);
  - the 700 ms / 3 s watchdog and the identity substring match;
  - the `poll()` pull path (`:342-350`);
  - the dead `startHost`/`backgroundAction` wrappers;
  - the never-cancelled 250 ms timer (`:149, :888`).
- `DesktopApiClient.frames`, `frame`, `status`, the H.264 `presented`, and
  `DesktopFrame`.
- `DesktopVideoTexture.submit`, `reset` and `events`.
- `_AcceptanceApi.presented` counters (they move to Rust stats).

These deletions land in the D1 milestone (`p7-d1`), so every caller is
ported in the same commit and the whole package stays green there
(`flutter analyze` and the full `flutter test`):

- the old `U/pages/desktop_viewer.dart` (it reads `addListener`, `frame`,
  `cursorStatus`, `videoPresented`, `acknowledgePresented`, `mediaEpoch` and
  `send(rendered, event, mediaEpoch:)`, `:41-205, :443`), `desktop_page.dart`,
  `desktop_connection_banner.dart` and `desktop_gesture_surface.dart` move to
  the slices and to `send`/`sendSequence`. G2's on-device smoke runs on these
  pages, because the new ones land only at G3. The old viewer's Disconnect
  passes `snapshotPath: <application support>/desktop-thumbnails/smoke.thumb`,
  so the G2 smoke can check the native snapshot (D23);
- `ui/tool/desktop_viewer_acceptance.dart` (about 15 deleted store members at
  `:78-153`, and `_AcceptanceApi extends DesktopApi` at `:159`) gets a
  compile port; the schema-2 rewrite stays in the third WP-D commit;
- `ui/test/desktop_viewer_navigation_test.dart`, which drives the old pages
  through `FakeDesktopApi` (`:6-37`), is deleted (the UI contract deletes it
  anyway, because one viewer is structural there).

The app is not expected to run against the old FFI between `p7-d1` and G2.

---

## 10. Tests (minimal and deliberate: each guards a capability, failure mode, or boundary)

No wording tests and no input matrices. Each work package owns the tests for
its files (section 12).

### 10.1 Rust, `cargo test -p pb-mapper-desktop`

| Test (module) | Guards | WP |
| --- | --- | --- |
| `protocol::strict_parsing_and_version_gate` | An unknown field or op fails in both directions. Every message round-trips. A v1 ALPN peer gives `VersionMismatch`. | 0 |
| `invite::pbd1_round_trip_rejects_tamper_and_setup_strings` | Encode and decode. A flipped byte → `Checksum`. `pbmt1_` and base64 setup JSON → `SetupString`. v1 JSON is accepted with the same fingerprint. | 0 |
| `security::fingerprint_pin_rejects_another_certificate` (rewrites `tests.rs` `tls_rejects_an_unpaired_host_identity`) | `IdentityChanged` by downcast; a chain is rejected | 0 |
| `auth::receipts_and_input_never_grant_input_rights` | **D4 boundary.** With produced frames and no `Presented`: `admits` is false and `fresh` is false. Input whose `frame_id > acked` → refused. An acked frame produced > 2 s ago → refused. A generation change clears `acked`. Admitting input never moves `acked`. A stale `Presented` sets `stale_presentation`. | 0 |
| `idr::requests_coalesce_defer_and_are_never_dropped` | Ten requests in 100 ms → one IDR now and one after 300 ms. `force` bypasses the interval. Nothing is periodic. | 0 |
| `credit::receipts_return_credit_and_idr_uses_headroom` | The window closes at frames and bytes. A receipt reopens it. One IDR is admitted over the byte window while P-frames are in flight, and a second IDR waits. An empty ledger admits any size. The seq is booked before the write, and `seq > last_admitted` → error. The ack-queue clamp shrinks bytes under synthetic receipt delay and releases afterwards. | H |
| `qos::delay_controller_follows_rustdesk_steps` | Scaled ramp to the ceiling. 200 ms divides fps. 300 ms → `150·min/d`. Unresponsive → 2 fps output with the fps state untouched and immediate recovery. Tainted samples are ignored by the controller. The baseline is min-of-60 and resets on `Path`. The viewer cap and pause clamp the pace. The startup cap lasts 1 s. | H |
| `qos::bits_per_frame_steps_and_one_way_latch` | 3 s steps only when dynamic, the +150 kbit/s cap, the preset floor and max. A preset switch resets fps and bpf with no rebuild. The latch flips after 2 overshoot windows at pace ≤ 0.75 nominal and never flips back. | H |
| `session::cursor_is_not_echoed_within_300ms_but_generation_changes_pass` | `CursorGate`: suppression, trailing sync, generation pass-through | H |
| `session::stale_generation_units_are_dropped_before_admission` | After a control-grant invalidate with units queued, the old-generation units are dropped without a seq, the session continues, and the seq stays contiguous | H |
| `host::handshake_reports_denied_and_busy_before_close` | A bad token → `Rejected{denied}` with no capture started. A second viewer → `Rejected{busy}`. A bad token **while busy** → `Rejected{denied}`, never `busy` (no oracle, R5). | H |
| `windows::key_code_uses_virtual_keys_for_letters_and_digits` (`cfg(windows)`) | `a`..`z` map to `enigo::Key::A..Z` and `digit0`..`digit9` to `Num0..Num9`; no D17 key maps to `Key::Unicode` | 0 |
| `scheduler::wake_ends_a_wait_on_every_platform` | 5.10.2: runs on Linux CI too | W |
| `viewer::bulk_input_and_moves_never_trip_congestion` | 288 events in 24 synchronous batches (8 × 16 backspace events, 16 × 10 latched-key events) are accepted while the writer is blocked; 1000 single moves leave one queued move; a flood past `INPUT_QUEUE` revokes control (6.4) | V |
| `worker::input_lane_runs_before_capture_and_never_after_stop` | A fake backend whose `capture_video` takes 30 ms. Input queued during it runs right after it, in order. Nothing runs after cancel. Overflow ends control. | W |
| `worker::pacing_is_even_and_skips_ticks_without_bursts` | Fake clock and backend. Pace 60 → even submits. A full gate (outgoing + pending ≥ 2) skips ticks. Release gives no catch-up burst (spacing ≥ 0.75 period). Pause → no submits, and the first output after resume is an IDR. | W |
| `worker::fps_and_preset_changes_never_reconfigure_the_encoder` | `configure_video` is called once. A display change rebuilds once, with an IDR. | W |
| `worker::lock_is_control_gated_and_failure_keeps_control` | View-only refuses `Lock`. A lock failure leaves `controlling` set and shows `lock_failed` for 5 s. | W |
| `video_sink::backlog_drops_to_next_idr_with_grace_and_limiter_backoff` | Age rule at push and pull. A burst with fresh `received_at` is not dropped. An arriving IDR supersedes the queue (discontinuity set). No backlog drop within 500 ms of a pulled IDR. Backoff 0.5 → 4 s. | V |
| `video_sink::single_consumer_need_capacity_keeps_unit_and_close_wakes` | A second `attach` → `AlreadyAttached`. `NeedCapacity` leaves the head queued. A blocked `next` returns `Closed` within 100 ms of `close()`. | V |
| `video_sink::presentation_requires_handed_current_fresh_monotonic` | Never handed, old generation, older than 2 s, non-monotonic, and paused → all rejected | V |
| `viewer::fps_cap_lowers_only_under_backlog_and_probes_up` | Throughput-limited presentation keeps the cap. Backlog lowers it to 90% / 80%, at most 50% per event. +5 after 3 s. Removed at the ceiling. Frozen while paused. | V |
| `viewer::recovery_stops_after_five_decoder_failures_and_pause_suppresses_requests` | The D7 state machine | V |
| `viewer::presented_precedes_input_that_references_it` | `Outbound` writes `Presented` before an `Input` with a newer `frame_id`, despite the throttle | V |
| kept: `viewer::repeated_releases_are_bounded_and_revocation_precedes_a_new_grant` | `Controls` | V |
| kept or adapted: `worker::input_requires_control_current_geometry_and_unique_sequence`, `native_input_failure_ends_control_and_survives_new_video`, `observation_never_acquires_write_admission_and_revocation_releases_it`, `unavailable_backend_reports_failure_and_stops`; `wire::clean_tls_close_is_eof_only_between_messages`; `host::remembered_invitation_survives_restart_but_not_revocation` (pbd1); `tests::blocked_video_does_not_block_local_stop`; `windows.rs` held-set and cursor-privacy tests; `video/windows.rs` `gpu_output_dimensions…` and `parameter_sets_survive_partial_updates_and_emulation_prevention`; helper binding tests | — | owners per file |
| `helper::readiness_requires_protocol_2` | Schema 1, protocol ≠ 2, or running without the v2 pipe → `update_required` | HL |

**Integration** (`crates/pb-mapper-desktop/tests/session.rs`, WP-V, runs at G1):

- `encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown`, kept
  and adapted:
  - a bad token gets `Rejected{denied}` and sees no frames;
  - input needs control plus a post-grant frame presented through
    `VideoConsumer`;
  - a duplicate sequence is rejected;
  - 1000 releases coalesce;
  - revoke works;
  - direct survives a relay stop;
  - the backend is dropped once.
- `frozen_renderer_keeps_credit_and_cursor_but_never_input` replaces
  `cursor_crosses_a_full_video_window_without_refreshing_the_picture`
  (`:206`):
  - a consumer pulls but never presents for 5 s;
  - video keeps arriving (≥ 4 s × pace);
  - cursor updates arrive;
  - `Viewer::input` → `NotFresh`;
  - a held button is released by the host within 2.5 s;
  - the host never sends `Stopped`;
  - presenting one frame restores input.
- `pause_stops_video_and_resume_starts_with_an_idr`:
  - no `Video` for 2 s while probes are echoed;
  - the host ends control;
  - after resume, the first unit is `keyframe && discontinuity`.
- `control_grant_with_queued_video_keeps_the_session` (must-fix R29).

**Ignored acceptance** (`D/slow_link.rs`, rewritten, run with
`-- --ignored --nocapture`):

- `fixed_wan_keeps_full_rate_at_fixed_resolution`: a 350 ms fixed WAN reaches
  the ceiling fps and the start bitrate. Resolution never changes.
- `bandwidth_limit_lowers_fps_and_bitrate_then_recovers`: a 1 Mbit/s proxy for
  8 s, then unlimited. The window clamps within 1 s, pace ≤ 15 during the
  limit, and recovery to ≥ 0.8 × start within 30 s, with zero expired encoder
  outputs.
- `slow_presenter_is_capped_by_fps_cap_not_host_backpressure`: a 67 ms per
  frame consumer gives a cap of about 13-15 fps. The host bitrate is
  unchanged.
- `sustained_slow_tls_reader`: kept and adapted.

**Examples:**

- `viewer_probe` is rewritten as a simulated decoder (`VideoConsumer`, fixed
  decode delay, `presented`). It prints receive and present interval
  p50/p95/max and the cap it caused.
- `deployed` (selftest, `--feedback-log`) and `input_acceptance` move to the
  new API.
- `video_bench` gains `--pace` and prints `gop` and `keyframes`.

### 10.2 FFI (`cargo test -p pb-mapper-ffi`, WP-F)

- `hot_ops_never_wait_for_a_slow_disconnect`: while `stop()` is held, the
  `viewer_call_json` ops and `view_v1` each return in < 10 ms, with no
  runtime entered.
- `video_consumer_is_exclusive_and_stale_ids_are_harmless`:
  - `attach` twice → `ALREADY_ATTACHED`;
  - a `next` blocked on another thread returns `CLOSED` within 100 ms of
    `disconnect`;
  - `NEED_CAPACITY` leaves the unit;
  - every call with a stale id returns `CLOSED`;
  - `detach` is idempotent.
- `inspect_invitation_classifies_without_network`: pbd1, v1 JSON, a setup
  string (→ `SETUP_STRING`) and garbage (→ `INVALID_INVITATION`).
- `shim_routes_old_and_new_connect_shapes` (7.9; G1 only, deleted with the
  shim).
- A `size_of` assertion for `PbDesktopViewV1` and `PbDesktopVideoMetaV1`.
- Kept: the `disconnect_cancels_*` tests, and the background and trust tests.
- JNI build check: `cargo ndk -t arm64-v8a check -p pb-mapper-ffi`.

### 10.3 Kotlin (`ui/android/app/src/test/kotlin`, WP-K)

- `VideoFeederTest`, against a fake `VideoSourceApi` and a fake `FeedTarget`:
  1. It never calls `next` while the target holds 2. Drops through every
     non-enqueue path (older generation, duplicate, awaiting key) return
     room. **This is the permit-leak case.**
  2. `NEED_CAPACITY` grows the buffer, and the same unit arrives next.
  3. `PAUSED` sets paused, and the next `UNIT` clears it.
  4. `CLOSED` ends the thread and calls `detach`.
- `DecoderRulesTest` (a pure function extracted from `pump`): a same-size
  discontinuity does not reconfigure. A size change or a missing codec does.
- `VideoPresentationQueueTest` is kept.
- `./gradlew :app:testDebugUnitTest` runs locally. Adding it to
  `syntax-check.yml` is the owner's call.

### 10.4 Dart (`flutter test`, runs on Linux CI, WP-D)

| Test (`test/desktop_store_test.dart` unless noted) | Guards |
| --- | --- |
| "input is sent with the geometry generation only and dropped when not live" | Rust stamps; `send` returns false on `!inputLive` or an epoch mismatch; `NOT_FRESH` is silent |
| "polling runs only on demand" | `testWidgets` with `tester.pump(duration)` (its fake clock; no `fake_async` dependency, D21): no timers when idle; `viewerStatus` only with a session; host poll only with page demand or sharing; view polling only with viewer demand |
| "a sharing or configured host always has its invitation" | 9.4 invariant: local sharing, background sharing, a restart with remembered trust (paused), and trust remembered under another relay (not configured, invitation null) |
| "a second connect during an attempt connects the new target" | 9.6.2 supersede semantics: the first attempt is cancelled, the retry timer cleared, the second target connected |
| "hidden viewer pauses once, never reconnects or requests keyframes; visible resumes" | D13, D14 |
| "quality change is live" | `setQuality` calls the op, with no disconnect or connect |
| "metrics do not notify session or hostView listeners, and AppStore is never notified" | D14 fan-out |
| "reconnect only on transport loss or 5 s without video; identity, version and denied are terminal" | 9.6.2 (rewrite of the `desktop_recovery_test.dart` cases, by error code), including: a fourth consecutive `protocol` stop is terminal, and `NOT_FOUND` / `VAULT_UNAVAILABLE` never retry |
| "texture survives reconnect (attach the new source); snapshot is best effort and time-boxed; a late texture after a busy disconnect is disposed" | injected `DesktopTextureFactory` (replaces the CI-skipped `desktop_lifecycle_test.dart:199-241`) |
| kept or adapted: `desktop_recovery_test.dart` invalid-invitation, retry-same-target, cancelled completion; `desktop_host_readiness_test.dart` (`update_required` → outdated) | — |

**Deleted:**

- `desktop_lifecycle_test.dart:98-143` (ACK cadence) and `:144-197`
  (`DesktopFrameStream`);
- the parts of `desktop_test.dart:98-278` that drive the old frame pipeline.
  They are rewritten against the `viewerStatus`/`view` fakes.

The P1-4 gesture invariant test ("keyboard show and hide keep scale and
offset") belongs to the UI contract.

---

## 11. Measurement and acceptance (D28)

### 11.1 Trace and report

- **Host trace:** `RUST_LOG=pb_mapper_desktop::feedback=debug,pb_mapper_desktop::frames=trace`
  (5.17).
- **Viewer trace:** `desktop_frame_view` (6.6), correlated with the host by
  `seq`. Each side reports durations on its own clock, and the network leg is
  `rtt/2`.
- **Acceptance tool:** `ui/tool/desktop_viewer_acceptance.dart` writes report
  schema 2. It contains:
  - one sample per second of `viewer_status`: `stats`, `decoder`,
    `adaptation`, `fps_cap`, `backlog_drops`, `keyframes_requested`,
    `present_interval` (p50/p95/p99/max) and `gaps_100`/`gaps_250`;
  - Flutter `FrameTiming`;
  - the build, quality preset, phone model and decoder, refresh rate, host GPU,
    `gop`, `rate_model`, battery temperature at start and end, and network
    type.

### 11.2 Procedure

This section and WORKPLAN §4.2 "Common host setup" / "Common phone setup"
are the same procedure; change both together. The P0 run at G1 uses the
schema-1 tool with the report upload (WORKPLAN tag `p7-upload`); every later
run uses the schema-2 tool.

**Host** (Windows, repository root, three terminals; `--control` only against
a benign target you own):

```powershell
cargo build --release -p pb-mapper-desktop --example deployed --example video_bench
target/release/examples/deployed.exe APP_CONFIG_PATH target/desktop-acceptance serve 600 --tcp --control --feedback-log
node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance        # after "host_ready"
target/release/examples/video_bench.exe 600 60 --pattern-only            # moving content
```

- QUIC carrier (case B): restart `deployed` **without** `--tcp`. It then
  uses the carrier of `APP_CONFIG_PATH` (`examples/deployed.rs:37-38`), which
  must be QUIC; check that before the run. `controller.json` hands the
  carrier to the phone.
- `--feedback-log` installs its own filter and deliberately ignores
  `RUST_LOG` (`examples/deployed.rs:23-33`), so setting `RUST_LOG` has no
  effect. WP-V's port of `deployed` widens that filter to
  `off,pb_mapper_desktop::feedback=debug,pb_mapper_desktop::frames=trace`
  (5.17; neither target carries a secret), which gives the 11.1 host trace.

**Phone** (from `ui/`):

```powershell
& $flutter build apk --release --target tool/desktop_viewer_acceptance.dart `
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json --dart-define=seconds=120 `
  --dart-define=quality=balanced
adb install -r build\app\outputs\flutter-apk\app-release.apk
adb reverse tcp:18747 tcp:18747
```

- Relay-only runs (case B) add `--dart-define=relayOnly=true`. `deployed`
  writes no `relay_only` into `controller.json` (`examples/deployed.rs:77`),
  so the define decides.
- `quality` is a schema-2 define; the schema-1 tool ignores it.
- At the end of the run the tool sends its report with
  `PUT http://127.0.0.1:18747/report` through the same `adb reverse`. The
  bootstrap writes it to `target/desktop-acceptance/report-<n>.json` and
  prints `report_saved <path>`. Nothing is read from the app's private
  storage, so a release APK, which is not debuggable and cannot use
  `run-as`, works.
- The release build needs the pinned signing key
  (`ui/android/app/build.gradle.kts:63-84`); WORKPLAN Wave 0 checks that it
  resolves on this machine. If it does not, restore it (the build forbids
  generating a replacement), or build the acceptance APK with `--profile`
  (AOT Dart). If that APK's signature differs from the installed product's,
  `adb install -r` fails: run `adb uninstall com.pb_mapper.ui` first, which
  also deletes the phone's app data.

Never install the tool as the product entry. Reinstall the product APK
afterwards.

### 11.3 Cases

| Case | Conditions |
| --- | --- |
| A | direct Wi-Fi, Balanced, 60 Hz phone |
| B | relay-only (TCP and QUIC carriers), Balanced |
| C | cellular (direct and relay, as found) |
| D | Smooth and Quality spot checks; a Windows viewer at the display refresh |
| E | background 10 s, then resume, × 5; then once each for 60 s and 5 min (records whether the process was frozen, R13) |
| F | decoder hints on vs off (A repeated) |
| G | UAC prompt and lock screen through the helper; Win+L via `lock` |
| H | RustDesk with the same host, phone, network, 1080p content |
| I | `video_bench --pace 60,30,15,5` and `video_bench 60 60` on each host GPU (rate model, GOP) |
| P0 | Cases A and B at **G1** with the shim (old phone pipeline): P0 measured alone |

### 11.4 Proposed pass criteria (the owner decides; the merge-block statements stay until the owner accepts, D28)

**A:**

- presented fps p50 ≥ 55 and ≥ 0.9 × min(60, host unique capture fps);
- present interval p95 ≤ 25 ms and p99 ≤ 50 ms;
- `gaps_250 = 0` and `gaps_100 ≤ 1` per minute after a 5 s warm-up;
- picture latency p50 ≤ 80 ms, plus one camera check of 20 readings against
  the host's "Unique desktop presentation N" counter;
- steady-state keyframes = 1 + requested (≤ 1 per minute);
- `unresponsive` never set;
- `input_wait` p99 within the gate (5.12.4).

**B and C:**

- no burst-and-stall pattern (no `gaps_250` while `rtt_ms` is stable within
  ±20%);
- relay ≥ 30 fps;
- fps may adapt, but resolution never changes.

**E** (default of WORKPLAN owner decision OD-2; R13):

- while the process was not frozen: a picture within 1 s of resume and no
  reconnect;
- after a freeze (the host closed the session): one immediate reconnect, a
  picture within 3 s of resume, the last picture greyed meanwhile;
- either way: no input replay, ≤ 2 IDRs per resume, no keyframe storm.

**G:**

- "waiting for remote desktop" without recovery churn;
- input resumes after a fresh frame of the new generation.

**I:**

- produced bitrate within ±25% of expected under the latched model;
- GOP not clamped below 5 s.

Record build, preset, refresh rates, GPU and encoder (`gop`, rate model),
phone and decoder, temperature and duration.

---

## 12. Work packages and implementation sequence

### 12.1 Packages and file ownership (one owner per file in each phase; work in separate worktrees)

| WP | Scope | Owns (create/modify/delete) |
| --- | --- | --- |
| **WP-0 Foundation** (one engineer, first) | Shared types and the cross-module contracts, implemented in full: protocol types, framing, invite, pinning, ALPN, `Backend` trait, `SessionShared` and its small members, `VideoSink` skeleton, the host status types and `Host` signatures of 5.15 | `D/lib.rs` (types of 4.5, `Backend` of 5.10.3, `HostStatus`/`HostViewer`/`HostState`/`HostPath` of 5.15, module decls incl. `mod pacer;`); `D/protocol.rs` (new); `D/wire.rs` (v2 framing **added** beside the old `Command`, which is deleted at G1); `D/invite.rs` (new); `D/security.rs`; `D/shared.rs`, `D/auth.rs`, `D/lane.rs`, `D/outgoing.rs`, `D/idr.rs` (new, complete, with tests); `D/video_sink.rs` (new: public API, attach, Condvar `next`, `NeedCapacity`, close, plain FIFO); `D/pacer.rs` (empty); `D/windows.rs` **only** `key_code` and the `impl Backend for Desktop` signature adapters; `D/host.rs` (`Host::status() -> HostStatus` adapter and `Host::end_session()`, 5.15); `crates/pb-mapper-desktop/Cargo.toml` (`bytes`); a stub `worker::spawn_session(shared, factory, config) -> WorkerHandle`. **The G0 mechanical-edit set, exactly** (nothing else is touched at G0): every `impl Backend` for the new signature — `D/windows.rs:192`, `D/helper/client.rs:101`, `D/host.rs:347` (test), `D/slow_link.rs:101, 295`, `D/tests.rs:44`, `D/worker.rs:635, 747, 918, 1033, 1245` (test fakes) and the worker's call sites, `D/helper/worker.rs` call sites, `tests/session.rs:26, 212`, `examples/input_acceptance.rs:206`, `examples/video_bench.rs:67, 153` (calls on the backend), `F/desktop.rs:1706`; the `Invitation` change in `D/viewer.rs` and `F/desktop.rs`; the `Host::status()` callers `D/helper/background_service.rs:355, 806`, `examples/deployed.rs:84`, `examples/input_acceptance.rs:531` and the FFI `status` op (`F/desktop.rs:622`, through `legacy_host_status`). `VideoPreferences` and `SessionStatus` stay until the G1 cleanup, so their other users (`examples/{deployed,video_bench,viewer_probe}.rs`, `D/helper/{mod,background}.rs`) need no G0 edit. Every test that runs a v1 viewer against a v1 host (`tests/session.rs`; the cases of `D/tests.rs` that call `Viewer::connect*`) is marked `#[ignore = "v1 pipeline; rewritten for v2 after the G1 merges"]` in the G0 commit, because no package branch has both v2 ends before G1; the G0 check runs them once with `--include-ignored` |
| **WP-H Host session and QoS** | B-recv, B-send, credit, QoS, handshake, Host API bodies, D24 | `D/session.rs` (new), `D/credit.rs` (new), `D/qos.rs` (new), `D/host.rs`; deletes `D/congestion.rs` at G1 |
| **WP-W Worker and encoder** | Thread C: Engine on `AuthLedger`, lane drain, pacer, capture step, cursor, lock, the drag-start hold; encoder submit, VFR, GOP, rate; the cross-platform wake (5.10.2) | `D/worker.rs` (replaces the WP-0 stub; deletes the old `Worker` at G1), `D/pacer.rs` (fills the WP-0 file), `D/scheduler.rs`, `D/video/windows.rs`, `D/windows.rs` (lock, capture, `double_click_guard`; WP-0 already did `key_code` and the signature adapters), `D/platform.rs`, `examples/video_bench.rs` (also `--dump <path>`, which writes the `u32 BE len \| unit` clip that 7.5's test source plays), `examples/input_acceptance.rs`, `examples/capture_probe.rs`; `crates/pb-mapper-desktop/Cargo.toml` production `windows` features `Win32_System_Shutdown` and `Win32_UI_Input_KeyboardAndMouse` (5.10.3). **Before G0** it touches only `D/video/windows.rs` (GOP, `submit`, `set_rate`, `pending_len`), keeping `next_frame` alive, plus a throwaway bench on a scratch branch; it ports `D/windows.rs` and `video_bench.rs` and deletes `next_frame`/`next_capture` only after merging G0 |
| **WP-HL Helper and upgrade** | IPC v2 (with the worker-session double-click metrics, 5.16.3), pipes, install schema, `update_required`, background v2 (on the G0 `HostStatus` and `Host::end_session`) | `D/helper/{mod,client,worker,install,background,background_service,service}.rs` |
| **WP-V Viewer** | Viewer tasks, `Outbound`, the full `VideoSink` policy, fps cap, recovery, status, view; integration tests; the SDK's public description | `D/viewer/` (new dir: `mod.rs`, `outbound.rs`, `fps_cap.rs`, `recovery.rs`, `status.rs`; deletes `D/viewer.rs`), `D/video_sink.rs` (after WP-0), `D/slow_link.rs`, `D/tests.rs`, `tests/session.rs`, `examples/{viewer_probe,deployed,transport_probe}.rs` (`deployed --feedback-log` also enables the `frames` target, 11.2), `crates/pb-mapper/README.md` (`crates/pb-mapper/src/lib.rs:67-70` re-exports this crate and `release-sdk.yml:205` publishes it; the README still documents `Viewer::next_frame`, `presented` and `VideoPreferences`, `:3-21`), plus the "SDK: breaking" CHANGELOG text handed to the UI finish package. Its end-to-end rewrite (`tests/session.rs`, `slow_link.rs`, the host↔viewer cases of `tests.rs`) is authored **after the G1 merges**, because it needs WP-H's session and WP-W's worker (12.3) |
| **WP-F FFI** | `DesktopState`, lifecycle ops, hot export, view leaf, video exports, JNI, registry, error codes, shim, the synthetic test source | `F/desktop.rs`, `F/desktop_video.rs` (new), `F/desktop_jni.rs` (new), `F/desktop_shim.rs` (new, temporary), `F/error.rs`, `F/lib.rs`, `F/Cargo.toml` (feature `desktop-test-source`, 7.5), root `Cargo.toml` (`jni` in workspace deps) |
| **WP-K Android** | JNI object, feeder, buffer pool, `DesktopVideo` changes, snapshot | `K/DesktopVideoNative.kt`, `K/VideoFeeder.kt`, `K/BufferPool.kt` (new), `K/DesktopVideo.kt`, `K/MainActivity.kt` (only if needed), `ui/android/app/src/test/kotlin/com/pb_mapper/ui/{VideoFeederTest,DecoderRulesTest}.kt`, `ui/android/app/build.gradle*` (test deps only) |
| **WP-WIN Windows runner** | Pull path, attach, receipts, snapshot | `W/desktop_video.cpp`, `W/desktop_video.h`, `W/CMakeLists.txt` |
| **WP-D Dart pipeline and store** | FFI lookups, service, `DesktopApi`, texture, models, `DesktopStore`, acceptance tool | `U/ffi/pb_mapper_ffi.dart` (desktop lookups only), `U/ffi/pb_mapper_service.dart`, `U/ffi/desktop_api.dart`, `U/ffi/desktop_video.dart`, `U/state/desktop_store.dart`, `U/state/desktop_models.dart` (new), `U/state/desktop_input.dart` (new), `U/state/app_store.dart` (**only** the D14 listener removal line), `ui/tool/desktop_viewer_acceptance.dart`, `scripts/test/desktop-bootstrap.cjs` (`PUT /report`: loopback only, ≤ 8 MiB, must parse as JSON, written as `report-<n>.json` with a server-chosen name, never a client path), `scripts/test/desktop-ui-acceptance.md` (the procedure follows the tool), `ui/test/fake_desktop_api.dart`, `ui/test/desktop_store_test.dart` (new), `ui/test/desktop_recovery_test.dart`, `ui/test/desktop_lifecycle_test.dart` (delete or rewrite), `ui/test/desktop_test.dart` (pipeline parts), deletes `ui/test/desktop_viewer_navigation_test.dart`; the port of `U/pages/desktop_page.dart`, `U/pages/desktop_viewer.dart`, `U/pages/desktop_connection_banner.dart` and `U/pages/desktop_gesture_surface.dart` to the D1 surface (9.8; the UI WPs replace them at G3). The upload lands first, in the old schema-1 tool, as its own commit that G1 merges (11.2) |
| **WP-M Measurement and docs** | Section 11 runs; docs | `docs/remote-desktop.md` (performance model, constants, CBR model per GPU, input-thread escalation, removed trace fields, the SDK section), `docs/validation/desktop-input-backpressure.md` (new evidence), `CLAUDE.md` (FFI exceptions: view leaf, video exports, JNI; the "three integer calls" sentence), release notes text (ALPN v2, helper update) |

### 12.2 Integration points and stubs

| Id | Interface (section) | Producer → consumers | Stub until it lands |
| --- | --- | --- | --- |
| I1 | Protocol messages (4.3-4.6) | WP-0 → WP-H, WP-V | — (complete in WP-0). WP-H tests drive a scripted viewer, and WP-V tests a scripted host, both over `tokio::io::duplex` using `protocol.rs`. |
| I2 | `SessionShared`, `AuthLedger`, `InputLane`, `Outgoing`, `IdrPolicy`, `Targets`, `EngineState` (5.1, 5.6, 5.11, 5.12) | WP-0 → WP-H, WP-W | — (complete in WP-0). WP-H tests push `HostUnit`s directly. WP-W tests read `Outgoing` and `EngineState` directly. |
| I3 | `Backend` trait (5.10.3) | WP-0 → WP-W, WP-HL, test fakes | — |
| I4 | `worker::spawn_session(shared, factory, WorkerConfig) -> WorkerHandle` | WP-W → WP-H (`host.rs`) | The WP-0 stub thread waits for cancel and publishes `EngineState{connecting}` |
| I5 | `VideoSink` / `VideoConsumer` API (6.5) | WP-0 (skeleton) → WP-V (policy), WP-F | The WP-0 FIFO implementation; `detached_for_test()` |
| I6 | `Viewer` public API (6.1) and `ViewerStatus`/`DesktopView` (6.10) | WP-V → WP-F | **V-API milestone** (WP-V's first commit, within days 1-2): `viewer/mod.rs` with the final public signatures over a minimal implementation. WP-F's registry, JNI and C exports need only I5 and can start at once. |
| I7 | C exports, JNI, code space, structs (7.4-7.6) | WP-F → WP-K, WP-WIN, WP-D | Frozen by this contract. WP-K tests use a fake `VideoSourceApi`. WP-WIN codes against the header. WP-D uses `FakeDesktopApi`. |
| I8 | JSON ops (7.2, 7.3) and `ViewerStatus` JSON | WP-F → WP-D | `FakeDesktopApi` |
| I9 | MethodChannel `pb_mapper/desktop_video` v2 (8.1.3, 8.2) | WP-K, WP-WIN → WP-D (`desktop_video.dart`) | Frozen by this contract; a fake texture factory in tests |
| I10 | `DesktopStore` surface (9.5) and `DesktopApiClient` (9.2) | WP-D → UI contract WPs | **D1 milestone** (WP-D's first commit): models, the store surface and its policies over `FakeDesktopApi`. The real `DesktopApi` follows. UI WPs that rewrite the viewer and Remote pages start from D1. |
| I11 | Shim (7.9) | WP-F → G1 measurement | — |
| I12 | `HostStatus`, `HostViewer`, `HostState`, `HostPath` and the `Host` signatures `status`, `end_session`, `set_control_allowed`, `stop` (5.15) | WP-0 → WP-H (bodies), WP-HL (`Snapshot.host`, `EndSession`), WP-F (`host_status`, `host_end_session`) | — (frozen at G0; the G0 bodies are adapters over the v1 host) |
| I13 | Synthetic video source (7.5, feature `desktop-test-source`) and the `video_bench --dump` clip | WP-F, WP-W → WP-K, WP-WIN on-device bring-up | — |

### 12.3 Sequence and gates (every gate builds, passes its narrow checks, and keeps clippy `-D warnings` green)

**S. Spikes** (run in parallel with WP-0; the results are recorded; nothing
is merged):

- S1 (WP-K engineer): a debug JNI export returning
  `pb_mapper_desktop_instance_token_v1()`, plus Kotlin `System.loadLibrary`
  and a Dart log of the same token, run on the phone. **If the tokens differ,
  stop and redesign 7.6** before G2.
- S2 (WP-W engineer): GOP read-back and `--pace` on the dev GPU, which
  classifies the CBR model.

**G0: WP-0 merged.**

- The new modules carry `#[cfg_attr(not(test), allow(dead_code))]` until G1.
  Removing it is a G1 checklist item.
- Check: `cargo test -p pb-mapper-desktop -- --include-ignored --skip slow_link`
  (the new module tests plus all existing tests, including the v1 end-to-end
  cases that are ignored from here to G1) and
  `cargo clippy --workspace --all-targets -- -D warnings`.
- The old app still runs.

**Parallel phase** (separate worktrees):

- WP-H, WP-W, WP-HL and WP-V on the Rust core;
- WP-F (registry, exports, JNI and error codes first, then the hot export and
  ops after V-API, then the shim);
- WP-K, WP-WIN and WP-D against frozen interfaces;
- the UI contract WPs, after WP-D's D1.

**G1: the Rust core is integrated** (merge order: WP-HL, WP-W, WP-H, WP-V,
WP-F with the shim, then WP-D's report-upload commit):

- `host.rs` wires `session.rs` and `worker::spawn_session`.
- WP-V's end-to-end rewrite (`tests/session.rs`, `slow_link.rs`, the
  host↔viewer cases of `tests.rs`, un-ignored) is authored on top of these
  merges, before the cleanup.
- Delete `D/congestion.rs`, `D/media.rs`, the old `wire::Command` and framing,
  the old `Worker`, `D/viewer.rs`, `VideoPreferences` and `SessionStatus`.
  Remove the dead-code allows.
- Check:
  - `cargo test -p pb-mapper-desktop -p pb-mapper-ffi`;
  - `tests/session.rs`;
  - the ignored `slow_link` runs locally;
  - `cargo build -p pb-mapper-ffi --release` (no test features, 5.10.3);
  - `deployed selftest`;
  - `input_acceptance` on Windows;
  - the installed-helper reinstall path;
  - the old app on Windows and the phone through the shim.
- **Measure case P0** (11.3) and record it before the phone pipeline lands.

**G2: the phone and desktop pipelines are integrated** (merge order: WP-K,
WP-WIN, WP-D, then WP-F's final commit):

- Delete the shim, the old frame exports, the Dart frame isolate and the H.264
  Dart path.
- Check:
  - `flutter analyze` and `flutter test`;
  - `./gradlew :app:testDebugUnitTest`;
  - `cargo ndk -t arm64-v8a build -p pb-mapper-ffi`;
  - `flutter build apk` and `flutter build windows`;
  - an on-device smoke test on both viewers, on the ported old pages
    (connect, control, background and resume, reconnect, and a snapshot
    written by Disconnect while `video_bench` animates, 8.2).

**G3: WP-M.**

- Run section 11 (cases A-I), then update the docs.
- The merge-block statements change only with real-device evidence and the
  owner's acceptance (D28).
- The UI contract WPs integrate on top of G2.

Commits are per work package (D27), with the tests in the same commits.

---

## 13. Risks and failure modes

### 13.1 Failure-mode matrix

| Failure | Behaviour | Owner | Proof |
| --- | --- | --- | --- |
| Loss on the single QUIC stream (head-of-line) | Receipts are delayed, so the window holds. The ack-queue clamp, then the probe, cut fps. Capture skips ticks while the submit gate is full, which discards nothing. Only a unit that waits in `Outgoing` longer than `OUTGOING_MAX_AGE` (1 s), counted from the later of its `ready_at` and the last IDR receipt and never while an IDR is in flight, is cleared, with one deferred IDR request (5.5 step 8.3). The burst drains through the sink by age. | credit, qos, sink | `slow_link`, case C |
| Bandwidth drop | The clamp acts within about 250 ms, the probe cuts fps and bpf within 1-3 s, and resolution is fixed | credit, qos | `bandwidth_limit_…`, case B/C |
| Slow decoder | Sink backlog → fps cap at 90% / 80% of presented. Drop to an IDR only beyond 250 ms of wait. | sink, fps_cap | `slow_presenter_…`, case F |
| Frozen renderer | Credit continues. Input is refused at both ends. Held input is released within 2 s. No teardown. | auth, sink | `frozen_renderer_…` |
| Background or minimise | `Pause`: pace 0, control ended. `PAUSED` suspends native checks. No keyframe requests, no reconnects. `Resume` → IDR. After an Android process freeze (R13): one immediate reconnect on resume. | 6.9, 8.1.3, 9.6.2, 9.6.4 | pause tests, case E |
| Keyframe storm | Host 300 ms, deferred and coalesced. Viewer backoff 0.5 → 4 s. 500 ms IDR grace. Failure limit 5 in 30 s. | idr, recovery | idr and sink tests |
| Control grant with units queued | C purges old-generation units. B-send drops stale units before admission. The viewer acks and drops. | 5.5, 5.12.3 | `control_grant_with_queued_video_…` |
| Secure desktop (helper) | The binding generation bumps, followed by invalidate, a new State and an IDR. Input needs a fresh frame of the new generation. | worker, sink | helper tests, case G |
| Secure desktop (no helper) | `capture_unavailable{secure_desktop}`. No requests, no reconnect. | worker, Dart | Dart test, case G |
| Helper reinstalled mid-session | `NotConnected` → `Stopped{capture_failed}`, and Dart reconnects | 5.16.4 | manual, G1 check |
| Old helper with a new UI | `update_required` → "Update service". v2 pipes never reach v1. | 5.16 | readiness test |
| Viewer acks an unsent seq | `Stopped{protocol_error}` | credit | credit test |
| Input flood | Lane 512, outbound 512 events / 256 KiB, single moves coalesced. Overflow revokes control and never drops a key-up silently. A UI burst (≤ 288 events, ≈ 100 KB) never overflows. | lane, outbound | worker and viewer tests |
| Revoke with input queued | B-recv drops at the edge. C releases on its next iteration. | 5.4, 5.12.3 | worker test |
| Disconnect during a blocked `next` | `close()` wakes it with `CLOSED`. Stale ids → `CLOSED`. | 7.7 | sink and FFI tests |
| Library loaded twice (Android) | `create` fails closed with a specific message | 7.6 | S1 spike |
| Reconnect | A new Viewer with the sequence reset in Rust, `_inputEpoch++`, the same texture re-attached. No input replay. | Dart, viewer | Dart reconnect test |

### 13.2 Risks

| # | Risk | Mitigation |
| --- | --- | --- |
| R1 | The priority lane misses the `input_wait` gate | Measured (trace, cases A and G). The escalation is specified (5.12.5) and built only on evidence. |
| R2 | The MFT rate model at reduced pace | `Nominal` by default, the one-way latch, and per-vendor `--pace` evidence |
| R3 | The vendor clamps the GOP | Read-back. GOP ≥ 5 s accepted (admitted from IDR headroom). Less than 5 s fails the bench and is documented. |
| R4 | Sync FFI on the UI isolate blocks | A separate closed-set export with no tokio, and a test |
| R5 | ALPN v2 strands PR #51 installs | A clean `VERSION_MISMATCH`, `update_required`, and a release note (D1 accepts this) |
| R6 | `PixelCopy` cannot read a SurfaceTexture-backed surface | Time-boxed, with a placeholder (D23). It never blocks close. |
| R7 | Receipt upstream overhead on cellular | About 60 acks/s × ~60 B, coalesced in a slot. Measured in case C. |
| R8 | Windows minimise is not reported as `hidden` | Verify. Fallback: the runner forwards `SIZE_MINIMIZED` on the `desktop_video` channel. |
| R9 | Strict parsing turns host bugs into session drops | Intended (D1). Parser and session tests cover every message. |
| R10 | `jni` API drift | Pinned to `0.21.1`. The seven functions are thin wrappers. |
| R11 | MediaCodec misbehaves on a mid-stream IDR at the same size on some vendor | `fail()` → rebuild on the next IDR (error path), with the decoder failure limit. If the on-device test shows it, a per-device `rebuildOnDiscontinuity` flag is the documented fallback. |
| R12 | The ack-queue clamp throttles a ramp after a static period | The clamp engages only when the ack queueing exceeds 100 ms. App-limited low rates never trigger it. Covered by `credit` and `slow_link` tests. |
| R13 | Android freezes a backgrounded (cached) app process after a short debounce, and OEM managers such as Vivo's (the test phone is a Vivo V2405DA on API 36, `docs/validation/desktop-input-backpressure.md:171`) are more aggressive. A frozen process stops pinging, the host closes after `HOST_READ_TIMEOUT`, and "resume without reconnect" (D13, case E) cannot hold. | Measured at G2 (backgrounds of 10 s, 60 s and 5 min). Default (WORKPLAN OD-2): accept one fast reconnect after a freeze (9.6.2: immediate first attempt, texture and greyed picture kept, no input replay) and the split criterion E (11.4). The alternative, a short-lived foreground service with a notification while a session is paused in the background, needs a foreground-service type and permission and is the owner's call. |
