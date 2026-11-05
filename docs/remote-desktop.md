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
2. On Windows, open the monitor button on **Tunnels**, choose a display, and
   use a unique service name such as `desktop:my-computer`. Names start with
   `desktop:` and contain only ASCII letters, digits, `.`, `_`, `:`, or `-`,
   with a maximum of 128 bytes. A slash is invalid for scoped relay services.
3. Sharing defaults to view-only. Enable the separate mouse/keyboard permission
   only when intended. Send the invitation privately: anyone possessing it can
   view this desktop and request control when the host permits it. It contains
   no relay credential.
4. On the controller, open Remote desktop, paste the invitation, and open its
   screen. Request control explicitly; viewing alone never acquires input
   admission. Choose **relay only** before connecting to exercise that path.
5. Use touch/click, drag, scroll, text entry, and the key-combination toolbar.
   Switch to Zoom / pan to navigate the image. Unicode insertion is supported;
   it is not a remote IME session. Secure-attention sequences are unsupported.
6. End control to retain viewing, or disconnect. The host retains a persistent
   **Stop sharing** action across navigation and can revoke input independently.

There is one selected display and one viewer. Stop and restart sharing to select
another display. Input uses the selected display's physical coordinates, DPI,
letterboxing and viewer transform, including negative monitor origins. A stale
screen is visibly blocked. The H.264 capture backend currently rejects rotated
displays instead of presenting incorrect geometry. Cursor metadata shows a
generic position indicator, not the original Windows cursor bitmap or shape.

## Remembered authorization and unattended use

**Remember this invitation and its permissions** is off by default. On Windows,
opting in stores the host TLS identity, 256-bit invitation token, selected display
and permissions using current-user Windows DPAPI. This is one remembered host
grant per application profile, bound to the exact relay address and credential
namespace ID. Remembering a different service replaces the saved grant after
the new host starts successfully. Relay passwords/administrator keys are not
copied into this store. The controller must retain its private invitation;
automatic mobile invitation storage is not implemented.

The invitation is a bearer capability, not a certificate identifying a particular
phone. Changing a relay or namespace does not silently reuse it elsewhere. Input
permission is separately persisted only through an explicit host choice; every
new connection still starts without active control and must request it.

| Action | Result |
| --- | --- |
| Temporary sharing | A fresh identity/invitation; stopping invalidates it |
| Start remembered desktop | Reuses the saved invitation and selected permissions |
| Share automatically when this app opens | Explicit opt-in; restores once after app configuration loads |
| Stop sharing | Closes sessions and disables automatic sharing on the next launch; remembered invitation can still be resumed manually |
| Close the app during temporary/app-owned sharing | Closes owned sessions while preserving the selected startup policy |
| Keep sharing in the Windows background service | Explicitly stores a separate service-owned invitation and permissions; closing the UI does not stop it |
| Revoke invitation and stop sharing | Deletes remembered trust, closes the current host, and requires a new invitation for future sharing |

App-owned automatic sharing requires Windows to be signed in and the UI to run.
Its saved file is `desktop-trust.dpapi` beneath the
existing application configuration directory. SDK embedders own their storage:
`HostIdentity::encode_secret` contains private material and must go in an OS vault
or encrypted storage, never a log, public configuration, or source control.

The separate **Windows background service** option transfers host ownership to
the installed service. It saves the selected relay configuration, invitation
identity, display and permissions using machine DPAPI in a SYSTEM/admin-only
file. The approved installed UI manages it through a separate authenticated,
bounded local pipe; ordinary desktop sessions still use endpoint-pinned TLS and
the existing pb-mapper direct/relay transport. The service retains no Windows
login password. Stopping disables future automatic sharing; revoking destroys
the saved grant. Closing the UI merely stops its viewer and any temporary host.
Enabling background hosting disables the old app-owned automatic-start policy.

The service waits for the installation owner's active console login, including
that same login while locked, and retries failed relay/capture startup with
bounded backoff. Logoff or switching away from that owner closes active capture
and input. A subsequent viewer still needs fresh explicit control admission;
old input is never replayed. A fresh boot with no owner login remains
`waiting_for_owner_session`. Waking from sleep is different from remote input:
S3/S4 cannot serve video while suspended, and no Wake-on-LAN facility or power
policy change is included. Real service-restart, sleep/resume and locked-session
password entry remain acceptance requirements, not consequences of unit tests.

## Windows helper and UAC boundary

The ordinary backend stays at the user's integrity level. It checks the active
input desktop and reports an inaccessible secure/locked desktop; it does not
keep a cached Default desktop apparently live. Elevated windows can reject its
input. The separate helper is the implementation path for elevated-window/UAC
access, **pending actual OS acceptance**.

Preparation in the UI is **Install / update helper**, deliberately complete
Windows' local administrator consent, open the protected installed app, then
enable **Use authorized secure desktop helper** before sharing. The first
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
  checked; the client verifies the installed service as well.
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
  reconnect or replay input automatically.
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
The unacknowledged window starts at 64 KiB, is bounded by 512 KiB / 24 frames,
and allows one bounded independent unit (at most 256 KiB) to initialize the decoder.
Unsent frames older than 250 ms are discarded and request a fresh IDR. True
presentation acknowledgements are coalesced over 100 ms, including a trailing
acknowledgement when there is no next frame. These observations never bypass
input authorization or its two-second freshness limit.

Growing acknowledgement delay lowers bitrate and GPU output size, down to a
256 kbit/s budget and 640×360 / 30 FPS. Progress with healthy latency gradually
restores quality within the viewer's limits. A stable renderer latency baseline
avoids treating Android's fixed presentation delay as congestion. Admission grows
on earned acknowledgements toward the measured round-trip budget, within the
same byte/frame caps. Sustained queueing, rather than one delayed ACK, reduces
quality; application-limited ACK throughput is not treated as link capacity.
Brief credit pauses preserve the predictive GOP. After an expired GPU output,
dependent outputs are discarded until an independent frame arrives. Four seconds
without presentation progress ends the session with an explicit reconnect
message; there is no silent indefinite freeze. This does not establish a fixed
frame-rate guarantee on arbitrary hardware or networks.

Android negotiates no more than its current display refresh rate (up to 90 FPS).
The media isolate waits on native frame arrival for at most 16 ms, and native
submit returns producer credit after consuming encoded input. Android uses
`MediaCodec.Callback` on its codec worker instead of two-ms dequeue polling.
At most four units are decoding or waiting as decoded outputs; it additionally
paces those outputs using capture timestamps, with a 25 ms initial
buffer and an 80 ms scheduling bound. Only one output may await SurfaceTexture
consumption at a time; its exact timestamp returns the next presentation credit.
Full metadata capacity pauses decoder input, while reset, disconnect and a
two-second consumption stall retire pending work. These bounds do not guarantee
a frame rate.

Diagnostics distinguish unique capture FPS, unique receive FPS, native texture
presentation, dropped units and bitrate. `frame_ack_ms` measures host frame-ready
to renderer acknowledgement return, including transport, presentation and ACK
coalescing. It is **not** one-way latency or a camera-measured motion-to-photon
delay. Zero means no current-generation presentation sample is available.

## Measurements and remaining acceptance

Development environment: Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X,
NVIDIA RTX 5070 Ti, 2560×1440 display at 165 Hz; Rust 1.99.1 and Flutter 3.44.9.
The first 0.10.2-based Windows UI run lasted 30.05 seconds with a five-second
warmup and measured **89.58 native texture-consumed FPS at 1920×1080 over the
actual pb-mapper direct path**. Sampled UI presentation averaged 89.54 FPS;
unique capture and receive both averaged 90.00 FPS. Every steady-state sample
was streaming/direct with no viewer error, zero SDK queue drops and zero native
queue drops. This meets the milestone's "at or near 90 FPS" target on this
machine. The endpoints ran on the same Windows host with a moving test window
and self-view; it is not cross-NAT, physical-phone, image-quality or UAC proof.
The raw counters, monotonic timings, environment and native binary hashes are in
[`validation/windows-direct-1080p90.json`](validation/windows-direct-1080p90.json).
The [reproduction procedure](../scripts/test/desktop-ui-acceptance.md) uses the
product viewer and decoder, not a simulated renderer.

After fixing explicit decoder-reset credit recovery, connection cancellation
and mouse hover, an always-visible moving pattern produced **85.19 native
presented FPS**, 89.19 unique capture/receive FPS, and zero SDK/native queue
drops or steady-state errors in another 30-second direct run. Its counters and
binary hashes are in
[`validation/windows-direct-1080p90-reviewed.json`](validation/windows-direct-1080p90-reviewed.json).
The pattern now stays topmost: an intervening run with the pattern occluded by
the viewer measured 78.36 presented / 80.74 captured FPS. These different source
conditions are not a controlled before/after comparison. The observed rates
depend on content and compositor scheduling; 89.58 FPS is not a guaranteed floor.

The following earlier development experiments are separate evidence, not
full-product acceptance or substitutes for the native presentation runs:

| Experiment | Recorded result | What it establishes |
| --- | --- | --- |
| Optimized SDK capture/encode/receive, 2560×1440, 10 s per path | 94.79 unique received FPS direct; 104.45 forced relay; zero reported queue drops | The GPU/transport path can exceed 60 FPS in this environment; no native rendering measured |
| Default 1920×1080 / 90 FPS SDK, 10 s per path, after host TCP_NODELAY | 86.48 unique received FPS direct; 85.42 forced relay; zero reported queue drops | Short SDK experiment through the deployed TCP service; not a sustained UI result |
| Native 1920×1080, 90 FPS encoded fixture on Windows | 89.96 texture-presented FPS | Native decode/texture path in isolation; no network/capture latency measured |
| Same fixture on the Android VM with a 60 Hz display | 56.38 texture-presented FPS | Android native path exercised; does not pass the requested 60+ FPS target |
| Full Windows Flutter/FFI/native viewer, 1080p90, direct, 25 s | 88.93 presented FPS over seconds 5–24; 2,194 consumed textures total; zero SDK/native queue drops | Actual integrated viewing, including capture and transport; both endpoints on this Windows host |
| Same Windows UI with final renderer feedback and trailing ACK, 25 s | 85.88 presented FPS over seconds 5–24; 2,095 consumed textures total; zero SDK/native queue drops | Direct self-view on the same host; 1080p / 12 Mbps ceiling, 16.65 ms final ACK sample; no synthetic animation in this run |
| Earlier Android VM viewer, 1080p60, direct, 25 s | 55.24 presented FPS over seconds 5–24; 59.98 received FPS | Before single-output surface credit; not a 60+ FPS pass |
| Android VM with single-output surface credit and feedback, 1080p60, direct, 25 s | 49.62 presented FPS over seconds 5–24; 1,182 of 1,186 decoded outputs consumed; 1 SDK drop / 0 native drops | Android 36 x86_64, `c2.goldfish.h264.decoder`, 60 Hz virtual display; bounded presentation, but below the requested 60+ FPS |
| Earlier Android UI with feedback and single-output credit, deployed TCP relay, 25 s | 22.54 presented FPS over seconds 5–24; 640×360 / 256 kbit/s at the end; 2 SDK drops / 0 native drops | Before the callback decoder and WAN admission fixes; 484 of 485 decoded outputs consumed; no 60 FPS claim |

The SDK run used the deployed relay over TCP with a dedicated expiring scoped
test credential; its log is `target/desktop-scheduler-benchmark.log`. The locally
configured QUIC carrier timed out. The test used TCP without changing the user's
saved configuration, so QUIC connectivity is not verified. A deployed-relay path
and a VM on the same host also do not prove operation across two independent NATs.
Integrated direct-viewer logs are `target/desktop-live-windows-auto.json` and
`target/desktop-live-android-auto.json`. Some earlier full UI forced-relay runs
received only about 100 KiB/s and accumulated stale video. This was application
throughput, **not a measured limit of the public relay**. The Windows feedback
run is `target/desktop-live-windows-feedback-auto.json`. The fixture and SDK
numbers must not substitute for UI results.
The earlier Android relay log is `target/desktop-live-android-package-relay.json`;
all 24 samples after initial connection remained streaming, with no viewer error.
The single-output direct log is `target/desktop-live-android-surface-credit-auto.json`.

The 2026-11-05 transport probe separates carrier capacity from video admission.
Four MiB in each direction, through the deployed TCP relay and the machine's
existing `wintun` route, measured 43.62 / 32.26 Mbit/s with split TLS and a
194 ms median ping round trip. Raw, framed and combined-TLS cases also exceeded
32 Mbit/s in both directions. Upload timing waits for the receiving endpoint's
confirmation. Logs: `target/desktop-transport-deployed.log` and
`target/desktop-transport-environment.log`.

A second consumer route uses an existing SSH connection to a separate cloud
host, forwarding only the local test connection to the same public relay. The
publisher still exits through the local `wintun` route. That controlled topology
measured 42.98 / 38.63 Mbit/s with split TLS and 238.80 ms median round trip;
`target/desktop-transport-ssh-exit.log` records byte verification. It establishes
two different relay entry paths, not independent physical phones or two real
NATs. No remote relay deployment or route settings were changed.

After preserving predictive frames across credit pauses and repairing window
growth, two 30-second 1080p60 GPU streams were received with **simulated** 50 ms
presentation delay and 100 ms ACK coalescing. A moving benchmark window supplied
content. The ordinary public-relay path received 58.95 unique FPS after a five
second warmup (54.96 over the whole run); the SSH consumer path received 57.69
(55.98 overall). Both ended at a 12 Mbit/s codec budget with zero SDK queue drops.
Their receive-gap p95 values were 38.91 and 42.29 ms, so these averages are not
proof of uniform frame pacing. Full saved H.264 streams decoded with no FFmpeg
errors. Logs: `target/desktop-flow-final.log` and
`target/desktop-flow-final-ssh.log`. These are transport/feedback measurements,
**not native Android or Windows UI presentation FPS**.

The callback-based Android revision compiled successfully, but its on-device
installation/run was blocked by automatic approval review. The previous Android
UI measurements above remain the last runtime evidence; the callback change
has no new presented-FPS result. A forced 350 ms GPU-output pause additionally
verified recovery on an independent frame; all 172 exported 1280×720 frames
decoded without error (`target/desktop-pause-recovery.log`).

An explicit 34.22-second synthetic-source / real-TLS feedback acceptance run
throttles reads to 100 KiB/s, then delays presentation by 600 ms and restores the
reader. With the revised controller the budget fell below 1 Mbit/s during the
first eight seconds, bottomed at 256 kbit/s, then recovered to 12 Mbit/s by second
30. Maximum accepted ACK latency was 713.26 ms; intentionally stopping all ACKs
ended the session after 4.18 s. This tests congestion recovery and bounded admission, not image quality
or physical presentation. Reproduce with
`cargo test -p pb-mapper-desktop --all-features --lib presentation_feedback_recovers_after_slow_tls_and_renderer -- --ignored --nocapture`;
log: `target/desktop-feedback-acceptance-optimized.log`.

A separate 30-second acceptance test throttles actual TLS reads through a local
test relay, using synthetic 64 KiB access units and no OS input. At 209.54 KiB/s
measured throughput it captured 2,430 units and discarded 2,304 under the 8-frame /
8 MiB queue limits. Remote control-grant and release commands completed in 2.15 ms
and 0.47 ms; local revocation and stop in 0.15 ms and 0.95 ms. The viewing session
remained active after revocation. This measures the transport/control boundary,
not image quality or WAN latency. A 64 KiB/s attempt disconnected after about
17 seconds and is not a stable-throughput pass. Reproduce the successful explicit
probe with `cargo test -p pb-mapper-desktop --all-features --lib sustained_slow_tls_reader -- --ignored --nocapture`;
results are in `target/desktop-slow-tls-acceptance.log`.

| Capability | Current evidence / limit |
| --- | --- |
| Windows ordinary desktop | Capture and native video paths exercised on the machine above |
| Input / Unicode / touch mapping | Backend and UI paths implemented; focused state/security tests cover mapping and ordering |
| Direct and forced relay | Local integration and deployed TCP SDK experiments exercised; independent cross-network UI acceptance outstanding |
| Android viewer | Native MediaCodec/Flutter texture path exercised in an Android VM; physical phone validation outstanding |
| iOS viewer | Shared Flutter/JPEG path exists; packaged build and device validation outstanding |
| Elevated windows / secure-desktop UAC | Helper implemented; default-policy administrator and standard-user prompt acceptance outstanding |
| Remembered app-owned host policy | Current-user DPAPI; requires signed-in user and running app |
| Windows service-owned background host | Implemented with explicit opt-in, machine DPAPI and protected management IPC; actual service/restart/long-running acceptance pending |
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

`input_acceptance --run-owned-window` additionally exercised real Windows input
through a dedicated local TCP relay and GPU/H.264 backend. Its own EDIT control
read back Chinese, Unicode and emoji exactly; two real mouse-down/up pairs were
observed, with disconnect-triggered release taking 15.245 ms. The program checks
the foreground HWND, DPI-correct geometry and focus before every injection and
never targets other windows. This is ordinary-desktop evidence, not UAC or a
physical touch-device test. Log: `target/desktop-native-input-acceptance.log`.
The probe also accepts `--relay-config PATH --tcp --consumer-server HOST:PORT`
for an existing alternate relay entry, with its own expiring test scope. The
attempt through the SSH entry aborted with `applied=0` when the owned window
lost foreground. This correctly prevented input to another window, but does
not pass cross-network input acceptance; see
`target/desktop-native-input-cross-egress.log`.

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
