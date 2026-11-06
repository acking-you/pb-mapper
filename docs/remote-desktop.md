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
relay fallback. The native UI requests 1080p / 90 FPS / 12 Mbit/s; Android caps
that request at its current display refresh rate. A 60 Hz phone or VM cannot
demonstrate 90 displayed FPS. Relay sessions may reduce frame rate and resolution
to preserve responsiveness; 90 FPS is not a relay acceptance requirement.

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
- A backend is constructed, used and dropped on its dedicated capture/input
  worker. `Backend` deliberately has no `Send` requirement: COM objects and
  desktop bindings remain on their owner thread. The factory itself is
  `Send + Sync`. Do not move a live backend between threads.
- `ControlAdmission::acquire` provides an optional embedder-controlled write
  lease. POCOX can pause its Computer Use writer when a human explicitly requests
  control. Lease drop releases admission on end-control, revocation, disconnect
  or local stop. Observation never calls this hook. Integrating that hook into
  POCOX itself remains separate work.
- `Viewer` uses `Client::connect_tcp` and the existing registration, namespace,
  direct commit and pre-commit relay fallback. The private loopback pair retains
  its bound listener until accepted. Established application streams do not
  reconnect or replay input automatically at the SDK transport layer. The UI
  owns a separate bounded recovery policy described below.
- TLS 1.3 runs inside both direct and relayed tunnels. The invitation pins the
  exact host certificate; rustls also checks its validity, name and signature.
  The separate random capability is sent only after host verification and is
  compared in constant time. TLS early data is disabled. A relay credential alone
  cannot authorize capture or input, and a failed invitation does not start a
  capture backend.
- Input binds to a desktop/display generation and a recently rendered frame.
  Sequence numbers reject duplicate/reordered actions. Permission revisions,
  desktop transitions and capture failures invalidate old input. Network
  heartbeats alone cannot keep keys held when the renderer freezes.
- Explicit `stop` cancels and joins the endpoint's worker/tasks and stops its
  registration/connection. Drop initiates cancellation. Coordinator loss or
  relay credential revocation does not forcibly close an established direct
  endpoint; local desktop stop does.

## Media and rendering

The viewer's status bar continuously reports connecting, waiting for a local
desktop, recovering video, reconnect countdown or live/delayed video. Frame age
is time since receipt of the displayed picture; frame confirmation time includes
encoding/transport/render/return feedback and is not a one-way latency estimate.
After a stall the UI requests a keyframe first. A failed stream or continued
stall opens a fresh authenticated session with the same in-memory invitation and
path preference, using 1/2/4/8/15-second retry intervals. Capture-unavailable
heartbeats wait for the local desktop instead of retrying around missing Windows
permissions. Manual disconnect cancels recovery; backgrounding pauses retries
and ends control. A foreground retry may restore previously requested control
only after a fresh picture and host permission. No queued clicks/keys are
replayed, and the invitation is not persisted by this policy.

Input release/revoke messages coalesce in a bounded mailbox. Consecutive unsent
pointer moves coalesce only between click/key barriers; input congestion releases
held input without deliberately tearing down video. Desktop transitions retire
old presentation debt without allowing viewer keyframe requests to extend the
ACK timeout. On Android, decoder reset is acknowledged by the codec worker,
preserves the texture/Surface, and cannot asynchronously close a newer codec.
The renderer drops obsolete decoded pictures after a stall while preserving
compressed H.264 references. Pinch/pan transforms keep the same mounted texture;
an opaque stale-picture overlay must not block its consumption callbacks.

Windows H.264 hosting uses DXGI Desktop Duplication, D3D11 textures and GPU NV12
conversion, followed by a low-latency Media Foundation hardware encoder with no
B-frame reordering. The tested NVIDIA path uses the installed driver encoder.
It requires a compatible hardware H.264 MFT on the capture GPU and reports an
error when that path is unavailable. JPEG remains an SDK compatibility path;
`Viewer::connect` requests it, while native renderers use codec negotiation.
There is no promise that every hardware failure silently falls back to JPEG.

`VideoPreferences` defaults to a 1920×1080 maximum encoded size, 90 FPS and a
12 Mbit/s bitrate ceiling. `Viewer::connect_with_video_preferences` accepts
explicit renderer limits. GPU scaling preserves aspect ratio and never upscales;
input and cursor coordinates still use the physical `Display` geometry. These
are requested limits, not measured delivery or display guarantees.

H.264 units must be consumed **in order** through `Viewer::next_frame`. The
`frames` watch receiver is a wake/status observation; `latest_frame` can skip
predictive frames and is suitable for independently decodable JPEG, not H.264
playback. The Flutter binary FFI follows the ordered queue and sends H.264 to
native decoder workers: Media Foundation/D3D11 shared textures on Windows and
MediaCodec/SurfaceTexture on Android. Capture, encoding, decoding and blocking
FFI calls stay off the Dart UI isolate.

`pb_mapper_desktop_frame`/the ordered media bridge return owned buffers containing
a little-endian u32 metadata length, JSON status and encoded bytes. Free each
non-null native buffer with `pb_mapper_desktop_free_frame`. Video bytes never
become JSON/base64. Native texture consumption, rather than merely submitting
encoded input to a decoder, produces presentation acknowledgements.

Each SDK media queue holds at most eight access units and 8 MiB; an individual
unit is capped at 4 MiB and wire JSON at 8 KiB. Native decoder input queues hold
four units, with at most 16 pending frame metadata entries. Overflow or generation
change discards dependent H.264 frames and requests a new IDR. Input/release
traffic is separate from the video writer and bounded. A blocked write times out
after three seconds; there is no automatic replay on a new connection.

The capture schedule is independent of encoding duration. Static-screen refreshes
at about 250 ms are marked duplicates and excluded from unique-frame FPS.
`Viewer::connect_with_presentation_feedback` opts into renderer-driven admission;
the UI uses it and the existing receive-only SDK APIs retain their behavior.
The unacknowledged window starts at 64 KiB, is bounded by 512 KiB / eight frames,
and allows one bounded independent unit (at most 256 KiB) to initialize the decoder.
The frame limit matches the receiver's queue. A recovery IDR holds the window
until its presentation is acknowledged, so another burst cannot evict that
replacement GOP while the receiver is still waiting for an independent image.
Unsent frames older than 250 ms are discarded and request a fresh IDR. True
presentation acknowledgements are coalesced over 33 ms, including a trailing
acknowledgement when there is no next frame. These observations never bypass
input authorization or its two-second freshness limit.

Growing acknowledgement delay lowers bitrate and GPU output size, down to a
256 kbit/s budget and 640×360 / 30 FPS. Progress with healthy latency gradually
restores quality within the viewer's limits. A renderer latency baseline is
calibrated from small predictive flights. Admission grows
on earned acknowledgements toward the measured round-trip budget, within the
same byte/frame caps. Sustained queueing, rather than one delayed ACK, reduces
quality; application-limited ACK throughput is not treated as link capacity.
Brief credit pauses preserve the predictive GOP. After an expired GPU output,
dependent outputs are discarded until an independent frame arrives. Four seconds
without presentation progress ends the session with an explicit reconnect
message; there is no silent indefinite freeze. This does not establish a fixed
frame-rate guarantee on arbitrary hardware or networks. Persistent delay and
jitter in the Android VM can still leave quality at its minimum; recovery on
that real renderer remains a limitation despite the synthetic slow-link test.

Android negotiates no more than its current display refresh rate (up to 90 FPS).
The media isolate waits on native frame arrival for at most 16 ms, and native
submit returns producer credit after consuming encoded input. Android uses
`MediaCodec.Callback` on its codec worker instead of two-ms dequeue polling.
At most four units are decoding or waiting as decoded outputs. The newest
decoded picture is immediately eligible, without an additional playback pacer.
Only one output may await SurfaceTexture consumption at a time; its exact
timestamp returns the next presentation credit.
Full metadata capacity pauses decoder input, while reset, disconnect and a
two-second consumption stall retire pending work. These bounds do not guarantee
a frame rate. Android uses Flutter's supported OpenGL renderer for this
SurfaceTexture path; zoom/pan retains the same texture and decoder.

Cursor position changes use a separate coalesced status channel, including when
capture is idle or the video window is full. Same-generation streaming events
with empty bytes update status/cursor only: retain the picture and its age, do
not reset the decoder or acknowledge a new frame. Cursor observation never
renews held-input authorization.

Diagnostics distinguish unique capture FPS, unique receive FPS, native texture
presentation, dropped units and bitrate. `frame_ack_ms` measures host frame-ready
to renderer acknowledgement return, including transport, presentation and ACK
coalescing. It is **not** one-way latency or a camera-measured motion-to-photon
delay. Zero means no current-generation presentation sample is available.

## Measurements and remaining acceptance

Development environment: Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X,
NVIDIA RTX 5070 Ti, 2560×1440 display at 165 Hz; Rust 1.99.1 and Flutter 3.44.9.
The [UI acceptance procedure](../scripts/test/desktop-ui-acceptance.md) measures
native texture consumption through the product viewer and decoder.

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

The current Android diagnostic baseline uses Android 36 x86_64 emulator
`emulator-5580`, `c2.goldfish.h264.decoder`, a 60 Hz virtual display and host OpenGL
on the RTX 5070 Ti (`-gpu host`). Each path ran for approximately 120 seconds,
without concurrent builds. These runs used the eight-frame admission window and
33 ms ACK coalescing. Their congestion reference is retained in the final code;
the subsequent reference-mean experiment was reverted. These are diagnostic
baselines, not a physical-phone performance pass.

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
experiment is not in the final package. The one-second averaging of congestion
observations, eight-frame bound and exclusive recovery-IDR credit are retained.

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
presentation-feedback recovery. Their logs are
`target/desktop-feedback-acceptance-optimized.log` and
`target/desktop-slow-tls-acceptance.log`; rerun the ignored
`presentation_feedback_recovers_after_slow_tls_and_renderer` and
`sustained_slow_tls_reader` tests for the current congestion revision. These do
not measure image quality or physical presentation. Deployed probes use dedicated
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
identity/revocation, DPAPI corruption, helper ACLs, input generations, bounded
media recovery, ownership and teardown, plus Flutter desktop interaction tests.
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

References: [issue #50](https://github.com/acking-you/pb-mapper/issues/50),
[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput),
[Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata),
[RustDesk elevation bootstrap](https://rustdesk.com/docs/en/client/windows/windows-portable-elevation/).
