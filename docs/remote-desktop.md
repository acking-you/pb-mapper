# Embedded remote desktop (issue #50)

The optional Rust SDK component and existing Flutter UI share a Windows screen
through an existing pb-mapper deployment. The implementation includes GPU H.264
video, native Windows/Android rendering, an opt-in Windows helper, and explicitly
remembered host authorization. The relay needs no desktop-specific upgrade or
additional rendezvous service.

**Issue #50 is not complete.** The helper has not passed administrator-consent
and standard-user credential-prompt acceptance under the default secure-desktop
policy. The development process is not elevated; it cannot approve its own UAC
prompt. Android validation uses the Windows machine's Android VM, as requested
by the user. This does not establish physical-phone or iOS support, independent
cross-NAT operation, or the same presentation rate on every target device.

The performance milestone is **1080p direct viewing at or near 90 presented
FPS**. Both endpoints prefer pb-mapper's existing direct path, with pre-commit
relay fallback. A viewer asks for at most 1080p at its display's refresh rate
(at most 60 FPS on Android) and a quality preset; the host chooses frame rate
and bitrate within them. Balanced, the default, starts at about 4 Mbit/s and
never exceeds 60 FPS; only Smooth goes higher. A 60 Hz phone or VM cannot
demonstrate 90 displayed FPS. Relay sessions may reduce frame rate and bitrate
to preserve responsiveness; 90 FPS is not a relay acceptance requirement.

**PR #51 remains merge-blocked on phone fluidity.** The receipt-credit pipeline
below replaces the presentation-credit pipeline that stuttered on a physical
phone, but it has not been measured on one. The block stays until the user
accepts real-device fluidity; see
[the block and its evidence](validation/desktop-input-backpressure.md).

## Use the UI

1. Configure both apps for the deployed relay and a namespace they can both
   access. Use a dedicated scoped relay credential for the controller; never
   transfer the relay administrator credential to a phone.
2. On Windows, open the monitor button on **Tunnels** and choose **Enable remote
   control**. This single action prepares the authorized helper, persistent
   service, input permission and automatic restoration. Complete any Windows
   consent yourself; a successful installation restarts the unprivileged UI and
   resumes the saved operation. Advanced settings select the display and a
   unique service name such as `desktop:my-computer`. Names start with
   `desktop:` and contain only ASCII letters, digits, `.`, `_`, `:`, or `-`,
   with a maximum of 128 bytes. A slash is invalid for scoped relay services.
3. Copy the reusable invitation privately: anyone possessing it can view and
   control this desktop while its host permission is enabled. It contains no
   relay credential. Advanced settings can revoke input while retaining viewing.
4. On the controller, open Remote desktop, paste the invitation, and open its
   screen. The UI automatically requests the host's permitted control after a
   fresh frame is actually presented. The SDK's explicit control-grant boundary
   remains unchanged. Choose **relay only** in Advanced before connecting to
   exercise that path.
5. Touch mode uses a single tap for left click and a two-finger tap for right
   click; hold before moving to drag a remote item. Trackpad mode moves the remote
   cursor with one finger and also uses a long press to drag. Moving a two-finger
   gesture zooms/pans in either mode, without switching away from mouse control.
   The toolbar also exposes mouse buttons, scrolling, text and key combinations.
   Unicode insertion is supported;
   it is not a remote IME session. Secure-attention sequences are unsupported.
6. End control to retain viewing, or disconnect. The host retains a persistent
   **Stop sharing** action across navigation and can revoke input independently.

There is one selected display and one viewer. Stop and restart sharing to select
another display. Input uses the selected display's physical coordinates, DPI,
letterboxing and viewer transform, including negative monitor origins. A stale
screen disables input while keeping the texture visible with a status hint.
The H.264 capture backend currently rejects rotated
displays instead of presenting incorrect geometry. Cursor metadata shows a
generic position indicator, not the original Windows cursor bitmap or shape.

## Remembered authorization and unattended use

**Enable remote control** is the explicit host authorization for persistent
viewing and input. The Windows service owns the TLS identity, 256-bit invitation
token, selected display and permissions. The grant is bound to the exact relay
address and credential namespace ID. The controller must retain its private
invitation; automatic mobile invitation storage is not implemented.

The invitation is a bearer capability, not a certificate identifying a particular
phone. Changing a relay or namespace does not silently reuse it elsewhere. Input
permission is separately revocable. Every connection starts without active
control; the UI requests a new grant only after displaying a current frame.
Reconnect never replays queued pointer or keyboard actions. A certificate
mismatch stops automatic retries and never replaces the pinned identity.

| Action | Result |
| --- | --- |
| Enable remote control | Starts the service-owned host with input and automatic restoration enabled; reuses its invitation |
| Stop sharing | Closes sessions and disables automatic sharing; the same invitation resumes on the next explicit Enable |
| Close the app | Stops its viewer; service-owned sharing continues |
| Upgrade legacy app-owned sharing | Transfers the saved identity if no same-scope service identity exists; an existing service identity remains authoritative |
| Revoke invitation and stop sharing | Deletes remembered trust, closes the current host, and requires a new invitation for future sharing |

Legacy app-owned grants and a pending installation handoff use current-user
DPAPI in `desktop-trust.dpapi` beneath the existing configuration directory.
Successful service transfer removes the legacy copy. A pending handoff retries
temporary IPC failures with bounded backoff; it never installs a helper without
the explicit Enable action. Stop or Revoke cancels the pending restoration.
SDK embedders own their storage:
`HostIdentity::encode_secret` contains private material and must go in an OS vault
or encrypted storage, never a log, public configuration, or source control.

The installed **Windows background service** saves the relay configuration, invitation
identity, display and permissions using machine DPAPI in a SYSTEM/admin-only
file. The approved installed UI manages it through a separate authenticated,
bounded local pipe; ordinary desktop sessions still use endpoint-pinned TLS and
the existing pb-mapper direct/relay transport. The service retains no Windows
login password. Stopping disables future automatic sharing; revoking destroys
the saved grant. Closing the UI merely stops its viewer and any temporary host.
The UI stops any local publisher before transferring ownership. Repeating the
same configuration does not restart an active host or rotate its identity.

Process-based proxy and firewall rules must allow `pb-mapper-desktop-helper.exe`
as well as `pb_mapper_ui.exe`: background sharing opens its relay connections in
the helper. A rule for the UI alone does not cover the service. With the QUIC
carrier, the helper needs UDP access to the relay's advertised QUIC port and
TCP access to its configured discovery port. For example, a TUN proxy with UDP
forwarding disabled can leave the UI connected while silently dropping the
helper's QUIC handshake. Add the helper to the same permitted route as the UI,
or explicitly select a working relay carrier before configuring background
sharing. pb-mapper does not change another application's routing rules.

Background startup errors retain the registration carrier, phase and failure
in the UI status and in the bounded protected
`%ProgramFiles%\pb-mapper-desktop\service.log`. `background_start_failed`
records failed attempts; `background_registered` confirms relay readiness.
If a viewer resets while the relay reports that its desktop service is not
registered, resolve that publisher startup failure before diagnosing capture.

The service waits for the installation owner's active console login, including
that same login while locked, and retries failed relay/capture startup with
bounded backoff. Logoff or switching away from that owner closes active capture
and input. A subsequent viewer still needs fresh explicit control admission;
old input is never replayed. A fresh boot with no owner login remains
`waiting_for_owner_session`. A Windows session/privilege query failure is reported
separately with its failing API/error and retried; it must not masquerade as an
owner who has not signed in. Such failures cancel active/pending hosting while
retaining the saved authorization. Waking from sleep is different from remote input:
S3/S4 cannot serve video while suspended, and no Wake-on-LAN facility or power
policy change is included. Real service-restart, sleep/resume and locked-session
password entry remain acceptance requirements, not consequences of unit tests.

## Windows helper and UAC boundary

The ordinary backend stays at the user's integrity level. It checks the active
input desktop and reports an inaccessible secure/locked desktop; it does not
keep a cached Default desktop apparently live. Elevated windows can reject its
input. The separate helper is the implementation path for elevated-window/UAC
access, **pending actual OS acceptance**.

Windows remote-control preparation checks
that the helper is installed, running, that this application is authorized,
and that its live process image matches the protected installation. An identity
query failure requires repairing/updating the helper and is never shown as ready.
Use **Enable remote control** and deliberately complete Windows' local
administrator consent when required. Installation automatically replaces the current
application and returns to the remote desktop page, preserving saved settings
and background sharing authorization. There is no separate "open installed app"
step. An unelevated worker verifies the protected installation and owner, opens
a handle to the old process, and acknowledges readiness before the UI exits.
It launches the replacement only after that process exits, with a bounded wait.
Cancellation or preparation failure leaves the original window open. Sharing
selects the helper automatically when ready; maintenance remains available for
service-owned sharing even when background management loses its connection.
The SDK ordinary-desktop backend remains available to embedders and does not
support UAC or the lock screen. The first
elevation of a portable app may need a person locally. A phone cannot approve a
desktop the unprivileged app cannot access. No UAC policy is changed, no prompt
is auto-consented, and no administrator password is retained.

The Flutter process remains unprivileged. Temporary sharing owns networking in
that process; explicit background hosting owns a narrow desktop endpoint in the
Windows service, independently of Flutter or an agent harness:

- The installer copies the complete UI/helper bundle to a unique protected
  release directory under `%ProgramFiles%\pb-mapper-desktop`. Owner and DACL are
  applied at object creation; unsafe ACLs, parent traversal and reparse points
  are rejected. Copying and cleanup have depth, file-count and size bounds.
- The `PbMapperDesktop` LocalSystem service runs in Session 0 and accepts local
  named-pipe IPC only. Its ACL grants the approved user read/write access without
  permission to create another pipe-server instance. Kernel-reported process
  identity, protected executable path, user SID and active console session are
  checked; the client verifies the installed service as well. Before reporting
  Running, the service merges only `PROCESS_QUERY_LIMITED_INFORMATION` for the
  approved owner into its existing process DACL. This lets the unelevated UI
  query the service executable; it adds no process-control or memory access.
- For an authorized connection, the service launches one restricted SYSTEM
  worker in the active interactive session. Temporary launch privileges are
  restored, a kill-on-close job bounds the child lifetime, and the worker uses a
  separate SYSTEM/admin-only pipe. The capture worker has no remote listener,
  arbitrary command, file, clipboard or credential API. Persistent-host
  management accepts only the protected installed UI and approved owner SID.
- The worker can bind its capture/input thread to Default or the secure desktop
  within the approved console login, including its locked state. The worker
  checks session ID, login time and user identity before context/input; a reused
  session ID or another user's login cannot inherit the old admission.
  Desktop-switch and lock-state changes
  advance a generation independently of resolution. Capture and final input
  injection check that generation; an old click cannot be replayed after a
  detected desktop transition. Failed input releases are retained for retry.
- Only a current rendered frame renews held-input state. Disconnect, permission
  revocation, session change, renderer staleness and local stop release input;
  the service gives the worker a bounded exit interval before its job is closed.

Installation is serialized, replaces configuration atomically, waits for the
service to report Running, and attempts to restore the previous configuration
and startup state if an update fails. A rollback failure is reported and files
are retained for repair. This is not a claim of crash-atomic replacement of the
SCM database and filesystem together.

Abnormal service exits use SCM recovery delays of 5, 15 and then 60 seconds,
reset after 24 hours. Normal Stop exits do not request recovery. Windows may
apply the non-crash failure flag after the next OS restart. Disable and uninstall
disable startup before stopping, so an already queued SCM restart cannot undo
that action. **Enable helper** explicitly restores automatic service startup.
Updating a disabled or stopped installation preserves that state.

Disable stops the service and disables its startup. Uninstall stops/deletes the
service and removes its authorization configuration. Upgrade preserves files
used by an older open UI until the next normal reboot; uninstall deletes what
it can and schedules in-use binaries for that reboot. Unique release paths keep
those delayed deletes separate from a later reinstall. No restart is initiated.
Bounded protected `installation.log`/`service.log` diagnostics may remain.

Required UAC acceptance still includes both administrator consent and a
standard-user credential prompt, with OS version, account type, helper state,
default secure-desktop policy and observed failures recorded. Lock-screen password
entry for the same logged-in owner is implemented but unverified on a Windows VM.
Pre-login boot, secure-attention sequences and control across user switching are
unsupported. In particular, SendInput cannot generate Ctrl+Alt+Del, and no
SoftwareSASGeneration or other security policy is changed.

## SDK ownership and transport

`pb_mapper::desktop` comes from optional `pb-mapper-desktop`, above
`pb-mapper-client` in the crate DAG. Ordinary forwarding/relay builds do not
include desktop capture, codec or input dependencies. Flutter opts in; the SDK
does not require Flutter or a POCOX runtime.

- `Host::start` creates an ephemeral host. `start_with_backend` accepts an
  embedder backend. `start_with_identity` and `start_with_backend_and_identity`
  resume an explicitly authorized `HostIdentity` without changing `HostOptions`
  literals. Discard that identity and stop its host to revoke durable access.
- `Host::status` returns a `HostStatus`: the state and its reason, whether
  control is allowed, the display and desktop kind, the capabilities, and the
  connected viewer (`HostViewer`: since when, direct or relay, controlling,
  paused, quality preset). `end_session` ends the current viewer's session and
  keeps hosting. `stop` ends hosting, and `stop_with` tells the viewer why
  (`restarting` for a display switch, `revoked` for revoked trust).
  `set_control_allowed` revokes or restores control live.
- A backend is constructed, used and dropped on its dedicated capture/input
  worker. `Backend` deliberately has no `Send` requirement: COM objects and
  desktop bindings remain on their owner thread. The factory itself is
  `Send + Sync`. Do not move a live backend between threads.
  `configure_video(limits, nominal_fps)` runs once per session and again only
  for a new desktop context, a display change or an encoder recovery, never for
  a rate change. `capture_video` drains finished output and submits at most one
  capture at the rate it is given; `drain_video` and `pending_video` let the
  worker collect an asynchronous encoder's output between ticks; `video_gop`
  reports the GOP the encoder read back. A sticky link failure returns
  `NotConnected`, which ends the session instead of retrying.
- `ControlAdmission::acquire` provides an optional embedder-controlled write
  lease. POCOX can pause its Computer Use writer when a human explicitly requests
  control. Lease drop releases admission on end-control, revocation, disconnect
  or local stop. Observation never calls this hook. Integrating that hook into
  POCOX itself remains separate work.
- `Viewer::connect(&client, &invitation, options, ViewerConfig { codecs, limits,
  quality, progress })` uses `Client::connect_tcp` and the existing
  registration, namespace, direct commit and pre-commit relay fallback. It
  returns once the host accepted, or fails with a typed `ConnectError`: identity
  changed, version mismatch, busy, denied, invalid request, cancelled, transport
  or protocol. `VideoLimits::for_viewer(refresh_hz)` derives the limits:
  1920×1080 at the refresh rate, at most 60 FPS on Android and 120 elsewhere.
  These are requested limits, not measured delivery or display guarantees.
  `set_quality` switches the preset live without reconnecting, and `pause` and
  `resume` stop and restart video while the viewer is hidden. The private
  loopback pair retains its bound listener until accepted. Established
  application streams do not reconnect or replay input automatically at the
  SDK transport layer. The UI owns a separate bounded recovery policy described
  below.
- Video has exactly one consumer: `viewer.video_sink().attach()` returns a
  `VideoConsumer`. Its `next(timeout, capacity)` blocks the calling thread,
  never a tokio worker, and returns a unit, `NeedCapacity` (the unit stays
  queued for a larger buffer), `Timeout`, `Paused` or `Closed`. The consumer
  reports each frame it actually showed with `presented(generation, frame_id)`,
  asks for an IDR with `keyframe(reason)`, and names its decoder with `report`.
- A host's `invitation()` encodes as `pbd1.` text (`Invitation::encode`,
  `Invitation::decode`): the service, the SHA-256 fingerprint of the host
  certificate and the bearer token. Decoding also accepts the earlier JSON
  invitation and pins the same certificate.
- TLS 1.3 runs inside both direct and relayed tunnels, with the ALPN
  `pb-mapper/desktop/2`. The invitation pins the exact host certificate; rustls
  also checks its validity, name and signature. The separate random capability
  is sent only after host verification and is compared in constant time. TLS
  early data is disabled. A relay credential alone cannot authorize capture or
  input, and a failed invitation does not start a capture backend. A peer of
  another desktop protocol version fails the handshake with `VersionMismatch`
  ("update pb-mapper on both computers"), never mid-session.
- Input binds to a desktop/display generation and to a presentation the viewer
  validated less than two seconds ago. `Viewer::input(generation, &inputs)`
  sends a batch of up to 16, stamps the presented frame and consecutive
  sequence numbers itself, and fails with a typed `InputError` (`NotFresh`,
  `NotControlling`, `Congestion`, `Closed`, `Invalid`). The host checks the same
  rule against its own ledger and rejects duplicate or reordered sequences.
  Permission revisions, desktop transitions and capture failures invalidate old
  input. Transport receipts, probes, cursor traffic and network heartbeats never
  grant or renew it.
- `Viewer::status` returns a `ViewerStatus` (state and reason, control,
  freshness, video, decoder and the statistics below), and `Viewer::view` an
  allocation-free per-frame snapshot (generation, presented frame, cursor and
  flags) for a renderer.
- Explicit `stop` cancels and joins the endpoint's worker/tasks and stops its
  registration/connection. Drop initiates cancellation. Coordinator loss or
  relay credential revocation does not forcibly close an established direct
  endpoint; local desktop stop does.

## Video pipeline and performance model

The pipeline keeps a slow phone, decoder or display from stalling the host, and
sends no faster than the network delivers. It follows RustDesk's `video_qos.rs`
where that fits. Seven rules carry it:

1. **Send credit comes from transport receipts, never from presentation.** The
   viewer's network read task writes a cumulative receipt as soon as a video
   record has been read, including one its sink later drops. Nothing between
   the network and the screen (FFI, Dart, decoder, compositor) holds credit
   back.
2. **Capture runs on an even grid at the QoS pace.** It never pauses or resumes
   as flow control. A tick is skipped only while two units are already buffered
   on the host (hand-off queue plus encoder in flight).
3. **QoS changes frame rate and bitrate only, from probe delay.** A probe once a
   second is answered by the viewer's network task. Resolution never adapts: it
   changes only with a new session, a display switch or an explicit choice.
4. **Infinite GOP.** IDRs are sent only on demand and are rate-limited on both
   sides. Requests are deferred and coalesced, never dropped.
5. **The viewer owns presentation lag.** Its sink keeps only the newest
   decodable frames, drops to the next IDR when it falls behind, and caps the
   host's frame rate to what it actually presents.
6. **Input is authorised only by presentation.** A validated presentation is
   the only thing that admits input. Input is drained ahead of capture on the
   one desktop-bound thread, and the receive task never waits for it.
7. **The cursor leaves the video path.** It is sampled separately and never
   echoed to the operator who is moving it.

These bounds keep the pipeline responsive; they do not establish a fixed frame
rate on arbitrary hardware or networks.

### Host: capture and encoding

Each session has one desktop-bound thread, which owns the backend, its encoder,
input injection and the desktop binding, and two network tasks: one reads the
viewer's messages and never waits for that thread, and one writes video,
cursor, probes and statistics. A blocked write of a large IDR never delays the
reader.

The thread drains the input lane, makes at most one capture or drain call, and
drains the lane again. Capture is paced on a grid at the QoS pace: a tick is
consumed whether it fires or is skipped, the grid keeps its long-run rate (120
FPS on a 165 Hz display is not rounded down), and after a stall two captures
are never closer than three quarters of a period. A tick is skipped (and
counted in `skipped_ticks`) only while the hand-off queue plus the encoder's
pending units reach two: transport backpressure, never presentation. A static
screen is resubmitted about every 250 ms as a duplicate, which unique-frame
rates exclude. The hand-off queue holds at most four units and 8 MiB; when its
oldest unit is more than a second old, the queue is dropped and replaced by an
IDR. The desktop context is checked at most every 100 ms, after an error, and
inside each capture; the injection boundary re-checks it for every input.

Windows H.264 hosting uses DXGI Desktop Duplication, D3D11 textures and GPU NV12
conversion, followed by a low-latency Media Foundation hardware encoder with no
B-frame reordering. Each duplication frame is released at once, so no update is
missed between ticks. Hosting requires a compatible hardware H.264 MFT on the
capture GPU and reports an error when that path is unavailable. JPEG remains an
SDK compatibility path at up to 8 FPS; there is no promise that every hardware
failure silently falls back to JPEG. The encoder runs at the session's nominal
frame rate (the viewer's `max_fps`) and is never rebuilt for a frame rate,
bitrate or preset change: the bitrate is set live, and sample times follow the
real capture interval. GPU scaling preserves aspect ratio and never upscales;
input and cursor coordinates still use the physical `Display` geometry.

The GOP is infinite. Before its media types are set, the encoder tries GOP
lengths `u32::MAX`, `0x7FFF_FFFF`, 65 535 and 3 600 and keeps the first one it
reads back unchanged, so a vendor that clamps or ignores the setting is
detected; the read-back is reported as `gop`. An IDR comes with a new encoder
(session, display or context change, recovery), a new generation (a control
grant or revoke, an input failure, capture recovered), a resume, a viewer's
request, or a hand-off queue that overflowed or went stale. Requests other
than a resume or a new generation wait until 300 ms after the previous IDR.
Encoder output more than 250 ms late is discarded, and output resumes at the
next IDR. Nothing periodic, and no frame rate, bitrate, preset or congestion
change, produces an IDR.

### Send credit

The host admits a unit against a window computed before each admission:

```text
t      = base RTT (100 ms until the first probe) + 100 ms
frames = clamp(ceil(pace × t), 3, 64)
bytes  = clamp(2 × expected bitrate × t / 8, 64 KiB, 4 MiB)
```

The base RTT is the minimum of recent probes, never a smoothed RTT, so queueing
never enlarges the window. At 60 FPS, a 20 ms base RTT and 4 Mbit/s that is 8
frames and 120 KB. A unit is admitted when nothing is in flight (whatever its
size, so a session always progresses), when it fits both bounds, or when it is
an IDR and no other IDR is in flight: one IDR may go over the byte window and
one frame past the frame window, and never waits for the whole flight to
drain. Its sequence number is booked before the write, so a receipt ahead of
the last booked sequence is a protocol error.

When receipts show a standing queue, the byte window follows the delivery rate.
The ack queue is the median receipt delay above the base RTT over the last
250 ms, or over the newest three receipts when fewer came in that time: a slow
link receipts seldom, and its queue must not vanish between two receipts. It is
zero before three receipts. Above 100 ms, once receipts have been measured for
500 ms, `bytes` becomes 1.25 × the delivery rate of the last 500 ms × `t`, at
least 32 KiB and at most the normal window. An application-limited flow has no
queue and is never clamped. The clamp bounds the queue between probes; frame
rate and bitrate decisions stay with the probe. JPEG sessions use a fixed
window of four pictures and 2 MiB.

Receipts reach only the credit ledger and the QoS controller. They never reach
the input ledger, the backend or the control lease, and no authorisation method
takes a sequence number.

### QoS: frame rate and bits per frame

The host writes a probe once a second, in stream order between video messages,
so it measures downstream queueing; one is outstanding at a time. The base RTT
is the minimum of the last 60 samples from the first sample on, and it resets
when the viewer reports a path change. A probe written behind an IDR in flight
is tainted: it updates the RTT but not the controller. The queue delay is the
mean of the last three untainted RTTs minus the base RTT.

Frame rate follows RustDesk's delay steps, scaled by `max(1, ceiling / 30)`.
Below 50 ms it climbs, with a quick step after three good samples. From 50 to
150 ms it climbs only slowly or holds. From 150 ms it falls to
`fps × 150 / delay`, but not below the preset's minimum; from 200 ms it falls
to the minimum or below. The controller works in bits per frame, so a
frame-rate cut sheds bandwidth at once. Every 3 s it scales bits per frame by
the window's worst delay: ×1.15, ×1.10 or ×1.05 below 50, 100 or 150 ms when
the picture changed (at least six unique frames), and ×0.95, ×0.90, ×0.85 or
×0.80 below 200, 300 or 500 ms or above. An increase adds at most 150 kbit/s.
The result stays between the preset's floor and maximum, scaled by the encoded
size relative to 1080p (0.25 to 4).

| Preset | Frame rate (min / start / ceiling) | Bits per frame at 1080p (start / max / floor) | At 60 FPS (30 for Quality) |
| --- | --- | --- | --- |
| Quality | 8 / 24 / 30 | 200 000 / 400 000 / 50 000 | 6 Mbit/s, up to 12, floor 1.5 |
| Balanced (default) | 10 / 45 / 60 | 66 667 / 133 333 / 16 667 | 4 Mbit/s, up to 8, floor 1 |
| Smooth | 12 / 50 / 120 | 50 000 / 100 000 / 12 500 | 3 Mbit/s, up to 6, floor 0.75 |

The ceiling is also bounded by the viewer's `max_fps`. The pace the host
captures at is the controller's frame rate, bounded by the ceiling and the
viewer's fps cap. It is held to `min(start, 15 × scale)` for 1 s after the
session starts and after a resume, to 2 FPS while the viewer is unresponsive,
and to 8 FPS for JPEG. A viewer is unresponsive while its probe echo, or a
receipt it owes, is more than 2 s late. That cuts only the output pace and
leaves the controller's state untouched; the next receipt or probe echo clears
it, and the session never closes because of it. Changing the preset
mid-session restarts the controller at the preset's start values, without an
IDR, an encoder reconfiguration or a resolution change. Pausing sets the pace
to 0.

The encoder bitrate depends on how the encoder budgets. A per-frame-budget MFT
(the `nominal` rate model, the default) gets `bits per frame × nominal fps` and
spends one frame's budget per frame, so a lower pace sheds bandwidth at once.
An encoder that budgets per second would overspend at a reduced pace. When two
consecutive 3 s windows at no more than 0.75 × the nominal rate produce more
than 1.3 × the expected bits, the session latches to the `timestamp` model
(`bits per frame × pace`) for good and traces `desktop_rate_model_latched`. The
encoder bitrate stays between 256 kbit/s and 40 Mbit/s, and nothing raises it
because the frame rate was cut.

### Encoder rate model per GPU

Measured on the development machine below with its NVIDIA GeForce RTX 5070 Ti
(driver 32.0.16.1088, "NVIDIA H.264 Encoder MFT", 1920×1080, nominal 60 FPS):

- GOP: every candidate sets and reads back unchanged before the media types, so
  sessions run with a GOP of `u32::MAX` (4294967295). After streaming starts,
  the same setting is accepted and silently ignored (the read-back stays at the
  default of 60 frames), which is why it is set first and read back.
- Keyframes: 2 in 60 s of `video_bench 60 60 --animate` (the initial IDR and
  one forced at 1.5 s), 2 across a 60 → 5 → 60 FPS pace sweep, and 1 in 8 s of
  fresh full-frame noise: no scene-cut or periodic IDR.
- Rate model: `nominal`. With synthetic content at 6 Mbit/s, produced bitrate
  at pace 60 / 30 / 15 / 5 was 6.16 / 2.78 / 1.52 / 0.39 Mbit/s against the
  nominal model's 6 / 3 / 1.5 / 0.5. The output was byte-identical under three
  timestamp schemes, so this MFT ignores sample times for rate control. The
  worst overshoot (1.03) is far from the 1.3 latch.
- `video_bench --pace 60,30,15,5` (the moving pattern, nominal 60, 4 Mbit/s):
  produced/expected 0.962 / 0.995 / 1.008 / 1.040, within the ±25% bound. At
  nominal 120 and 8 Mbit/s: 0.834 / 0.954 / 0.993 / 1.096. At nominal 120 and
  only 4 Mbit/s the pattern's own minimum size decides the bitrate (up to 1.79
  at pace 5), which is the content, not a timestamp model. Such a run would
  still pass the latch's test, and a false latch lowers each frame's budget
  while the pace is reduced, so acceptance runs on Smooth watch
  `rate_model`.
- The creation bitrate is honoured (4 → 4.05 and 16 → 15.97 Mbit/s), and a live
  bitrate change reads back exactly. The mean-bitrate read-back is stale until
  the first live change and is not evidence of the target.

AMD and Intel encoders have not been measured: this machine's AMD iGPU drives
no display, so its encoder is unreachable from the capture adapter. Run
`video_bench --pace 60,30,15,5` and `video_bench 60 60 --animate` on such a host
before relying on the `nominal` default there; the latch handles a
per-second-budget encoder.

### Viewer: newest decodable frame, fps cap and recovery

The viewer's read task writes the receipt for each video record before it hands
the record to the session's `VideoSink`, so nothing on the screen side delays
it. The sink keeps only what is still worth decoding:

- An arriving IDR supersedes everything queued, marked as a discontinuity.
- When the oldest queued unit is older than 250 ms with at least three queued,
  and no IDR was handed out in the last 500 ms, the sink drops to the next IDR
  and asks for one. Overflow (32 units or 8 MiB) does the same.
- A P-frame that arrives while an IDR is awaited is dropped. A new generation
  starts at an IDR. JPEG keeps only the newest picture.

Backlog also lowers the host's frame rate. Every 250 ms while streaming, the
fps cap compares the queue with what the consumer presented in the last second.
More than two queued on two consecutive ticks, or a backlog drop, sends
`floor(presented × 0.9)` direct or `× 0.8` relayed, at least 5 and at least half
the current cap, at most once a second. After 3 s with at most one queued it
rises by 5, and at the preset's ceiling it is removed. Only backlog lowers it,
so a phone that keeps up, or a static desktop, never ratchets the host down.

One limiter covers every keyframe request: the decoder's, the sink's drops and
the user's. While waiting for an IDR it asks at once (or after a 1 s hold that
follows a new generation, a host recovery or a resume), then after 0.5, 1, 2
and 4 s, and every 4 s after that; a user request skips the backoff but not the
500 ms minimum. Units handed out with nothing presented for 700 ms also ask for
one. Five decoder errors in 30 s with nothing presented end the session with
`decoder_failed`. No request is sent while paused or while the host reports
capture unavailable.

`pause` revokes control, tells the host to stop capturing and clears the sink;
probes are still echoed and pings sent, so the host sees a live, paused viewer.
`resume` restarts at an IDR with the startup pace. A presentation is accepted
only for a unit the sink handed out in the current generation, less than 2 s
after it arrived, newer than the last one, and not while paused. A rejected
presentation is never forwarded.

### Input and cursor

The viewer queues input in order behind a revoke barrier: at most 512 events or
256 KiB, with consecutive pointer moves coalescing into the newest. It sends a
pending presentation ahead of input, and revokes control rather than grow the
queue. The host's reader checks the control policy and pushes input onto a
bounded lane without waiting. The desktop-bound thread drains that lane before
and after its single backend call, so input never waits behind capture or
encoding. It applies each event only after checking its sequence, the display
geometry and generation, control, and a presentation its own ledger recorded
within 2 s. Nothing queued before a stop is executed, and reconnect never
replays queued pointer or keyboard actions. A full lane ends control
(`input_congestion`) rather than inject a burst the user no longer expects.

`input_wait`, from the reader's enqueue to the start of execution, is traced as
p50/p99/max each second. The acceptance gate is p99 ≤ 5 ms on the ordinary
backend and ≤ 15 ms through the helper, with moving content. If the gate
fails, the next step is a dedicated injection thread with its own desktop
binding, a process-wide registry that maps each binding epoch to a generation
(an older epoch fails closed), and a second helper pipe lane. It is not built;
build it only on measured evidence.

A press that starts a drag waits out the OS double-click time (plus 20 ms, at
most 600 ms) after a click on the same spot, so the two never merge into a
double click. Held commands are re-authorised when they are released.

The desktop-bound thread samples the cursor every 16 ms while capturing (never
while paused or unavailable) and publishes it when it changes. The writer sends
it outside the video queue, except that it holds the position back from a
controlling viewer for 300 ms after that viewer's last pointer input and then
sends the final one; a new generation's cursor always passes. Cursor metadata is
a generic position, not the Windows cursor bitmap, and observing it never renews
input authorisation.

### Constants

| What | Value |
| --- | --- |
| Records | JSON header ≤ 8 KiB; video payload 1 B to 4 MiB |
| Timeouts | handshake 3 s; host read 3 s; viewer header 4 s; writes 3 s plus the message's time at 1 Mbit/s; `Stopped` best effort within 1 s |
| Viewer ping / host statistics | after 1 s idle / every 1 s |
| Credit window | `t` = base RTT (100 ms until measured) + 100 ms; 3 to 64 frames; 64 KiB to 4 MiB; delivery clamp ≥ 32 KiB once the ack queue exceeds 100 ms |
| Host hand-off | 4 units, 8 MiB, 1 s oldest; capture skipped at 2 buffered |
| Probe / unresponsive | every 1 s / after 2 s, pace 2 FPS |
| QoS | delay history 3; RTT window 60; tick 250 ms; bits-per-frame window 3 s; latch at 1.3 × over 2 windows at ≤ 0.75 × nominal |
| IDR | host minimum interval 300 ms; viewer minimum 500 ms, backoff to 4 s; 1 s hold after a discontinuity |
| Viewer sink | 32 units, 8 MiB; backlog at 250 ms with ≥ 3 queued; IDR grace 500 ms; stall watchdog 700 ms |
| fps cap | tick 250 ms; × 0.9 direct, × 0.8 relay; minimum 5; +5 after 3 s; ≥ 1 s between changes |
| Input | viewer queue 512 events or 256 KiB, batches of 16; host lane 512; presentation fresh for 2 s; presentations sent at most every 100 ms except ahead of input |
| Cursor | sampled every 16 ms; echo held for 300 ms |

### Diagnostics

`ViewerStatus.stats`, fed by the host's once-a-second statistics, defines the
numbers the UI, the acceptance tool and these docs use:

| Name | Definition |
| --- | --- |
| `rtt_ms` | The last probe round trip, transport only |
| `queue_delay_ms` | The mean of the last three untainted probe RTTs minus the base RTT |
| `picture_latency_ms` | Median over 2 s of host time (encoder submit to socket write) + `rtt_ms / 2` + viewer time (network receipt to compositor consumption). Each side uses its own clock and the network leg is an estimate; only a camera measures glass to glass. |
| `presented_fps` | Accepted presentations per second |
| `received_fps` | Non-duplicate video records per second |
| `present_interval` | p50/p95/p99/max of the intervals between accepted presentations over the last 60 s, with the gaps over 100 and 250 ms |
| `degraded` | For 3 s: queue delay ≥ 150 ms, picture latency ≥ 250 ms, or (at a pace of 10 or more) fewer than half the pace presented |

The host traces each second as `desktop_feedback_interval` on
`pb_mapper_desktop::feedback` (debug): `action`, `fps`, `pace_fps`,
`viewer_cap`, `paused`, `unresponsive`, `bpf`, `bitrate`, `encoder_bitrate`,
`sent_bitrate`, `rate_model`, `rtt_ms`, `baseline_ms`, `delay_ms`, `tainted`,
`ack_queue_ms`, `window_frames`, `window_bytes`, `window_clamped`,
`outstanding_frames`, `outstanding_bytes`, `idr_inflight`, `skipped_ticks`,
`keyframes`, `gop`, `input_wait_p50_us`, `input_wait_p99_us` and
`input_wait_max_us`. The presentation-credit fields `quality`, `waiting`,
`recovering`, `desired_window_bytes` and `last_ack_*` are gone. Each written
unit is traced as `desktop_frame` on `pb_mapper_desktop::frames` (trace level),
and the viewer traces each accepted presentation as `desktop_frame_view` with
the same `seq`. `deployed --feedback-log` enables both host targets.

### The app's video path

The viewer's status bar reports connecting, waiting for a local desktop,
recovering video, a reconnect countdown, or live or delayed video. After a
stall the UI asks for a keyframe first. A failed stream or a continued stall
opens a fresh authenticated session with the same in-memory invitation and path
preference, after 1, 2, 4, 8 and 15 seconds. Capture-unavailable states wait for
the local desktop instead of retrying around missing Windows permissions.
Manual disconnect cancels recovery; backgrounding pauses retries and ends
control. A foreground retry may restore previously requested control only after
a fresh picture and host permission. No queued clicks or keys are replayed, and
the invitation is not persisted by this policy.

Windows decodes H.264 with Media Foundation into D3D11 shared textures, and
Android with `MediaCodec` into a `SurfaceTexture` on Flutter's OpenGL renderer.
Capture, encoding, decoding and blocking FFI calls stay off the Dart UI
isolate, and pinch and pan keep the same texture and decoder. On Android a
decoder reset is acknowledged by the codec worker, keeps the Surface, and cannot
close a newer codec; after a stall the renderer drops obsolete decoded pictures
but keeps compressed H.264 references. In this revision the app still reads
video through its earlier frame exports, which return an owned buffer: a
little-endian u32 metadata length, JSON status and the encoded bytes, freed with
`pb_mapper_desktop_free_frame`. Video bytes never become JSON or base64. A
temporary compatibility layer serves them from the session's `VideoSink`, in
order, and passes each presentation the app reports to
`VideoConsumer::presented`. The native consumers replace it, so H.264 never
crosses Dart: a Kotlin feeder thread pulls through JNI on Android, and the
runner's decoder thread pulls through C exports on Windows. Dart then reads
only status and the per-frame view.

## Measurements and remaining acceptance

Development environment: Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X,
NVIDIA RTX 5070 Ti, 2560×1440 display at 165 Hz; Rust 1.99.1 and Flutter 3.44.9.
The [UI acceptance procedure](../scripts/test/desktop-ui-acceptance.md) measures
native texture consumption through the product viewer and decoder.

### This revision: receipt credit

No phone or Windows viewer run of the receipt-credit pipeline has been recorded
yet. Its evidence so far is the encoder bench above and the synthetic cases
below. Through the earlier app's compatibility layer, a session runs on
Balanced, so it never exceeds 60 FPS; the build 922 rates below are not
comparable with such a run.

The ignored `slow_link` cases
(`cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link`) use a
real relay and TLS, a shaped viewer link, a synthetic asynchronous H.264 encoder
(8 ms latency, output lost after 250 ms, a per-frame budget of
`bitrate / nominal`) and a simulated decoder. They observe the pipeline, not a
GPU or real rendering. One debug-build run on this machine, with other builds
running:

| Case | Result |
| --- | --- |
| Fixed 350 ms RTT | Pace 60 (the ceiling); bitrate 4.91 Mbit/s from a 4.0 start; 59 presented and 60 received FPS; one keyframe; one size (1920×1080); the encoder configured once |
| 1 Mbit/s for 8 s after 6 s unlimited | Window clamped at the first trace interval after the limit (990 ms; 1001–1016 ms in three other runs); lowest pace 3; lowest expected bitrate 194 kbit/s; back to at least 3.2 Mbit/s 8.05 s after the limit lifted |
| Decoder taking 67 ms a frame | Host pace held at a mean 12.5 FPS (14.3 in another run) by the viewer's fps cap, while the delay controller stayed at 60 FPS and the encoder bitrate at 8 Mbit/s: presentation lag never reaches congestion control |
| 256 KiB/s link, 64 KiB frames, 30 s | Streaming throughout at both ends; capture held back (498 skipped ticks) instead of queueing; control 61 ms, release 47 ms, revoke 15.7 ms, host stop under 1 ms |

Proposed phone criteria, which the owner decides: on a 60 Hz phone over direct
Wi-Fi with Balanced, presented FPS p50 ≥ 55; present interval p95 ≤ 25 ms and
p99 ≤ 50 ms; after a 5 s warm-up no gap over 250 ms and at most one over 100 ms
a minute; picture latency p50 ≤ 80 ms with a camera check; keyframes equal to
one plus those requested; never unresponsive; and `input_wait` within its gate.
Relay and cellular runs must show no burst-and-stall pattern, at least 30 FPS
through the relay, and no resolution change. The merge block above stands until
the owner accepts such a run.

### Earlier builds: presentation credit

The final build 922 native Windows run measured **84.75 presented FPS** at
1920×1080 over direct, with 89.91 unique capture/receive FPS and mean frame ACK
17.35 ms over 60 samples. The final Android VM relay smoke measured 14.24 FPS
and 427.10 ms mean ACK over 60 samples; neither run had errors, reconnects or SDK
queue loss. Android had no decoder recovery, and normal disconnect left the host
waiting without a TLS error. See
[`validation/desktop-922.md`](validation/desktop-922.md) and its counter evidence
for the environment, longer diagnostic baselines and remaining limits.

The earlier 0.10.2-based Windows revision measured **89.58 native presented FPS
at 1920×1080 over the actual direct path**, after a five-second warmup in a
30.05-second run. Unique capture/receive averaged 90.00 FPS; steady-state samples
had no viewer errors or SDK/native queue drops. A later reviewed revision with
an always-visible moving pattern measured **85.19 presented FPS**, 89.19 unique
capture/receive FPS and zero SDK/native drops. Their counters, environment and
binary hashes are preserved in
[`validation/windows-direct-1080p90.json`](validation/windows-direct-1080p90.json)
and
[`validation/windows-direct-1080p90-reviewed.json`](validation/windows-direct-1080p90-reviewed.json).
These are independent evidence for those older revisions on one Windows machine,
not a performance pass for the current package. The installed build 922's recent
69–77 FPS self-view observation was not a controlled 90 FPS benchmark.

The build 922 Android diagnostic baseline uses Android 36 x86_64 emulator
`emulator-5580`, `c2.goldfish.h264.decoder`, a 60 Hz virtual display and host OpenGL
on the RTX 5070 Ti (`-gpu host`). Each path ran for approximately 120 seconds,
without concurrent builds. These runs used the eight-frame admission window and
33 ms ACK coalescing. Their congestion reference was kept in build 922 and the
subsequent reference-mean experiment was reverted; receipt credit replaced all
of it. These are diagnostic baselines, not a physical-phone performance pass.

| Android VM path | Native presented FPS | Submit-to-consume mean | Surface release-to-consume mean | Reconnects / SDK drops / native recoveries |
| --- | --- | --- | --- | --- |
| Direct | 17.78 | 89.00 ms | 48.42 ms | 0 / 0 / 0 |
| Forced relay | 15.94 | 87.20 ms | 50.53 ms | 0 / 0 / 0 |

Both runs adapted mostly to 640×360. The native renderer skipped 1,076 / 489
obsolete decoded pictures respectively; these are distinct from compressed SDK
queue loss. Final native-presented and transmitted-ACK generation/frame IDs
matched, with no ACK errors and one application keyframe request per run.
Local reports are `target/issue50-922/direct-window8-ack33.json` and
`target/issue50-922/relay-window8-ack33.json`; neither contains controller setup.
Flutter build time averaged about 2.7 ms and raster time 15.7–16.8 ms. After
warmup, additional Flutter draws beyond new video presentations were fewer than
one per second. No fixed application 20 FPS limit or dominant repeated-rebuild
cost was established. Surface delivery, GPU work and Flutter scheduling remain
partially combined in the timings; physical-phone 60 FPS is unverified.

A 120-second experiment replaced the lifetime-minimum reference with a low-load
interval mean. It restored substantially higher resolution but reduced the VM's
native presented rate from 15.94 to 13.66 FPS, raised sampled mean ACK delay from
384.3 to 439.6 ms, and increased decoder configurations from 5 to 17. That
experiment was not in build 922. The one-second averaging of congestion
observations, eight-frame bound and exclusive recovery-IDR credit were kept in
build 922; this revision removed them with the presentation credit.

### Lifecycle, input and transport

Installed build 922 passed a persistent-host lifecycle check: one Enable action
left `PbMapperDesktop` Running with Automatic startup. After fully terminating
the UI and reopening the exact protected installed executable, sharing appeared
automatically, the service retained PID 99536, and the copied invitation's SHA-256
matched the value before UI exit. Stop removed the sharing banner and restored
Enable; enabling again resumed sharing within seconds without another UAC prompt
for the already-authorized installation. Replacing the clipboard with a marker
before copying again confirmed that the unchanged invitation was freshly copied.
That first check used a candidate. On 2026-11-06 the final helper, FFI, UI and
Dart AOT files were installed and matched the delivered package hashes. The
upgrade restarted the service as PID 97260 and restored sharing with the same
invitation. Final UI direct and forced-relay viewing passed. With all desktop
test UI processes exited, a separate production-FFI viewer received fresh video
over both paths using a TCP relay carrier. Stop also persisted across a full UI
exit/reopen; Enable restored the same invitation without another UAC prompt.
See [the final installation evidence](validation/desktop-922-installed.json)
and [its limits](validation/desktop-922.md#final-protected-service-update-2026-11-06).
This verifies upgrade-driven service replacement and UI exit/relaunch, not OS
reboot, standalone SCM restart, sleep/resume or remote control of a UAC prompt.

Android VM touch tap, hold-drag, two-finger right click and pinch, plus trackpad
hold-drag/right click, passed against a benign owned Windows target with event
counters. Text insertion and scrolling passed in the preceding input run. These
are actual controller-to-target checks; they do not establish physical touchscreen
or secure-desktop behavior.

Supporting experiments remain separate from UI presentation acceptance. Earlier
split-TLS transport probes measured 43.62 / 32.26 Mbit/s with a 194 ms median ping
round trip through the deployed TCP relay, and 42.98 / 38.63 Mbit/s with a
238.80 ms median using a separate SSH consumer exit. Logs are
`target/desktop-transport-deployed.log` and
`target/desktop-transport-ssh-exit.log`. They demonstrate carrier capacity and two
relay entry paths, not two independent NATs. The earlier SDK/fixture and simulated
presentation-delay results are not current Android UI rates. A forced 350 ms
GPU-output pause recovered on an independent frame; all 172 exported 1280×720
frames decoded without error (`target/desktop-pause-recovery.log`).

Older synthetic-source / real-TLS tests separately exercised slow-reader and
presentation-feedback recovery on the presentation-credit pipeline. Their logs
are `target/desktop-feedback-acceptance-optimized.log` and
`target/desktop-slow-tls-acceptance.log`; the `slow_link` cases above replace
those tests. These do not measure image quality or physical presentation.
Deployed probes use dedicated
expiring scopes and an explicit TCP override without changing saved application
routing; QUIC carrier and independent cross-network UI acceptance remain separate.

| Capability | Current evidence / limit |
| --- | --- |
| Windows ordinary desktop | Capture and native video paths exercised on the machine above |
| Input / Unicode / touch mapping | Android VM gestures and text/scroll reached a benign Windows target; physical-device and secure-desktop acceptance outstanding |
| Direct and forced relay | Android VM runtime baselines above and deployed TCP experiments recorded; independent cross-network UI acceptance outstanding |
| Android viewer | Native MediaCodec/Flutter texture path exercised in an Android VM; physical phone validation outstanding |
| iOS viewer | Shared Flutter/JPEG path exists; packaged build and device validation outstanding |
| Elevated windows / secure-desktop UAC | Helper implemented; default-policy administrator and standard-user prompt acceptance outstanding |
| Remembered app-owned host policy | Current-user DPAPI; requires signed-in user and running app |
| Windows service-owned background host | Final build 922 passed upgrade restart, live direct/relay delivery with its UI exited, and persistent Stop/Enable with the same invitation; OS boot, standalone SCM restart and sleep/resume acceptance pending |
| Same owner's locked login / password entry | Session identity and desktop transition handling implemented; actual password-entry acceptance pending |
| Pre-login boot, SAS, control across user switching | Unsupported; service waits for its approved owner's login |
| Rotated GPU capture, macOS/Linux hosts | Unsupported |
| Audio, clipboard, file transfer, recording, multiple viewers | Not implemented |

Background operation follows each platform's own constraints. Linux/macOS need
their respective host capture/input backends and user-session service/launch
integration; those are not implemented by the Windows service. Android remains
a controller here. Mobile hosting is not implemented: Android screen projection
requires session consent and a media-projection foreground service, and iOS
does not provide unlimited general background execution for a remote-control
host. See [Android media projection](https://developer.android.com/media/grow/media-projection)
and [Apple background execution](https://developer.apple.com/documentation/BackgroundTasks/choosing-background-strategies-for-your-app).
No all-platform background-host acceptance is claimed.

Focused verification includes desktop/FFI tests for identity pinning, durable
identity/revocation, DPAPI corruption, helper ACLs, input generations, receipt
credit, delay-based QoS, the viewer's sink, fps cap and recovery, the rule that
receipts never grant input, ownership and teardown, plus Flutter desktop
interaction tests.
Run the required workspace fmt/Clippy and relevant networking/SDK suites before
delivery. Passing mocks or an isolated codec benchmark cannot complete UAC,
physical-device, independent-network, sustained slow-link or 60+ FPS UI acceptance.

The helper now releases held input and desktop-affine encoder resources before
`SetThreadDesktop`, retries failed binding without committing a new generation,
and handles a desktop-name change even if its event was missed. State-boundary
tests do not establish actual UAC operation. The local host is a physical Windows
machine and Hyper-V inventory returns an authorization failure; no accessible
Windows guest was identified. The running Android VM cannot validate Windows
secure desktop. Use the read-only collector and explicit acceptance matrix in
[`scripts/test/windows-helper-acceptance.md`](../scripts/test/windows-helper-acceptance.md);
host evidence is in `target/issue50-vm-acceptance/`. No UAC policy was changed and
no consent dialog was approved by automation.

The earlier `input_acceptance --run-owned-window` probe independently read back
Chinese, Unicode and emoji and observed mouse input through a dedicated TCP
relay (`target/desktop-native-input-acceptance.log`). Its alternate SSH-entry
attempt aborted with `applied=0` when the owned target lost foreground; that
preserves the input boundary but does not pass cross-network input acceptance
(`target/desktop-native-input-cross-egress.log`).

## Components, builds and licensing

The reference checkout is `../rustdesk` at
`49516c98574f117eea05e10f65c580c37b144e6c`, with its hbb_common submodule. RustDesk's
AGPL code and networking runtime are not copied or linked into this MIT project.
The implementation follows capture/codec/service techniques without adopting a
second rendezvous or networking runtime.

Dependencies include windows-capture 2.0.1 (MIT), windows-service 0.8.1
(MIT OR Apache-2.0), enigo 0.6.1 (MIT), xcap 0.9.8 (Apache-2.0) for monitor/JPEG
compatibility, image (MIT/Apache-2.0), and tokio-rustls/rustls. rustls is accessed
through `quinn::rustls`; versions are pinned in workspace dependencies. Video
uses installed Windows Media Foundation/driver codecs and Android MediaCodec;
no FFmpeg executable or libraries are bundled.

Build Windows with `make build-pb-mapper-ffi-windows` (FFI DLL and optional helper),
then the normal Flutter Windows build. Keep the entire
`ui/build/windows/x64/runner/Release` bundle, including DLLs, helper and data.
The helper binary is built from `pb-mapper-desktop` with `--features windows-helper`;
it is not loaded as a privileged entry point into the Flutter process.

For Android, rebuild the FFI for the intended ABI before Flutter: ARM64 for a
matching phone, or x86_64 for the development VM. For example,
`cargo ndk -o ui/native/android -t x86_64 build -p pb-mapper-ffi --release`, then
`flutter build apk --release --target-platform android-x64` from `ui/`.
Local build artifacts do not constitute a published release. The
`capture_probe`, `video_bench` and `deployed` examples provide bounded development
probes; the deployed probe revokes its dedicated temporary credential on normal
completion and removes its controller setup file. Never publish that file.
`video_bench --pace 60,30,15,5` classifies the encoder's rate model and, like
`video_bench 60 60 --animate`, prints its GOP and keyframe count.

References: [issue #50](https://github.com/acking-you/pb-mapper/issues/50),
[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput),
[Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata),
[RustDesk elevation bootstrap](https://rustdesk.com/docs/en/client/windows/windows-portable-elevation/).
