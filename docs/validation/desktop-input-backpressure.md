# Installed input and asynchronous video regression

Physical-phone testing of Android ARM64 0.10.2+923 exposed unusable clicks,
jerky pointer movement and an invalid-invitation retry loop. Build 923 used the
same application source as 922. The previous synthetic input and local video
checks did not establish that the installed SYSTEM worker could inject clicks.

## Reproduction on 2026-11-06

Environment: Windows 11, Ryzen 9 9950X, RTX 5070 Ti. A production 922 FFI viewer
connected to the installed background host and targeted a dedicated owned Tk
window. Every input checked foreground ownership and pointer hit-testing. No
input was sent to another application. Video ACKs in this probe simulate
consumption; they do not measure native presentation or phone latency.

The actual Windows pointer reached the intended position in 47–63 ms in two
local runs. Neither button-down nor button-up reached the window. A diagnostic
helper reported `SendInput` failure with Win32 5 (`ERROR_ACCESS_DENIED`), session
1, System integrity (`0x4000`) and `TokenUIAccess=0`. The generic Enigo error's
mention of UIPI alone was not sufficient to diagnose the cause.

The first candidate set `TokenUIAccess=1` on the restricted worker. Installed
acceptance disproved this diagnosis: the worker reported UIAccess=1 but still
returned Win32 5 and delivered neither button event. That token change is removed.

The desktop handle lacked `DESKTOP_JOURNALPLAYBACK`. A separate ordinary-user
process reproduced the boundary on the same owned window: binding a fresh thread
with the previous `0x181` access mask made `SendInput` return zero and Win32 5;
adding only journal-playback access (`0x1a1`) sent both events and the window
observed button-down and button-up. No token changes or desktop ACL changes were
needed. RustDesk's desktop binding requests `GENERIC_WRITE`, which includes this
right; pb-mapper now requests the specific additional right. See
[Microsoft's desktop access mapping](https://learn.microsoft.com/en-us/windows/win32/winstation/desktop-security-and-access-rights)
and [RustDesk's Windows binding](https://github.com/rustdesk/rustdesk/blob/master/src/platform/windows.cc).

The worker retains its restricted SYSTEM token without UIAccess, protected image,
owner/login binding, authenticated IPC, removed privileges and one-process job.
An opt-in Windows regression binds using the production access mask and submits
a zero-distance relative mouse move. It checks actual OS acceptance without
clicking, typing, moving the pointer or switching the visible desktop. The
installed final worker still requires end-to-end event delivery; ordinary-window
success alone does not establish secure-desktop or lock-screen operation.

Input failure now ends the control grant. It remains visible across subsequent
captures, queued button releases and stale presentation ACKs. The UI does not
automatically reacquire control after this failure. A deliberate new control
request can clear it.

## Reconnection

The connection page previously cleared its invitation after connecting, then
allowed an empty invitation to start an endless network-retry loop. It now
keeps the input in page memory, disables empty submission, and treats native
`INVALID_ARGUMENT` as terminal. Native validation precedes configuration waits
and connection ownership. The capability is not saved to a new persistent
store or written into logs.

The widget test exercises connect, navigation, manual disconnect, and reconnect
with the original input. Store tests cover terminal validation, cancellation,
transport retries, input ordering and explicit recovery after native input
failure. These are separate from real installed-service acceptance.

## Asynchronous encoder backpressure

The sender previously stopped calling the encoder when presentation credit was
exhausted. Windows' asynchronous Media Foundation encoder requires output to be
drained even when no new capture is submitted. A controlled worker regression
with a 128 KiB IDR demonstrated an already-completed output remaining unobserved
for over 250 ms. The candidate separates draining from submission; that test
drained in 2.4 ms with zero additional submissions.

An eight-second synthetic asynchronous encoder test used real relay/TLS,
60 fps source cadence and 350 ms delayed feedback on this Windows machine.
Separating draining reduced expired encoder outputs from 4 to 0 and keyframes
from 14 to 9; received frames changed from 154 to 157. The eight-frame credit
limit was unchanged. These measurements demonstrate the codec boundary fix,
not phone FPS or motion-to-photon latency.

Congestion delay also included local waiting for credit, while baseline
calibration started at transmission. Both now start at transmission; the local
250 ms expiry rule remains separate. A single optional startup status probe is
echoed directly by the viewer's network reader/writer, before decoder or display
work. It calibrates propagation delay without acknowledging any video, retiring
presentation debt, refreshing a control lease or enlarging the queue. Old
viewers ignore the optional field and retain frame-based calibration; new
viewers only send the echo after a host advertises it.

A second test distinguishes network propagation from consumer delay. A bounded,
pipelined TCP proxy delays each direction independently and imposes no bandwidth
limit. The host connects directly to the relay; the viewer connects through the
proxy. The synthetic source targets 4 Mbps / 60 fps. Windows debug results:

| Condition | Before network probe | After network probe |
| --- | --- | --- |
| 20 ms network RTT, 8 s | 452 consumed, 4 Mbps, 0 dropped | 451 consumed, 4 Mbps, 0 dropped |
| 350 ms network RTT, 16 s | 294 consumed, 256 kbps at 6 s / 1.3065 Mbps at end, 13 IDRs, 3 dropped | 310 consumed, stable 4 Mbps, 3 IDRs, 1 dropped |
| 20 ms RTT + 67 ms per-frame consumer, 8 s | 104 consumed, 256 kbps, 3 dropped | 104 consumed, 256 kbps, bounded queues |

Both columns include the earlier encoder-drain and send-clock fixes. All three
post-drain conditions had zero expired encoder outputs. The eight-frame and
512 KiB flight limits remain unchanged, so high-RTT throughput is still bounded
by the outstanding-frame limit. These tests establish neither native Android
presentation FPS nor RustDesk-equivalent end-to-end latency.

## Acceptance status

- Rust desktop: 46 library tests and two direct/relay session tests passed;
  workspace formatting and all-target/all-feature Clippy passed with warnings
  denied. Three explicit performance/slow-link tests and the interactive desktop
  permission regression are opt-in.
- Flutter: 22 focused desktop tests and analysis passed.
- FFI: all 39 library tests passed, including malformed-invitation/configuration
  cancellation.
- Encoder drain: asynchronous worker regression and real-TLS comparison passed.
- Legacy feedback peer: the explicit slow-link test ignores the new probe,
  reduces to 256 kbps under pressure, recovers to 9.057 Mbps / 960×540 by second
  30, and stops 4.18 seconds after presentation ACKs cease. Detailed counters,
  source references and commands are in
  [the video evidence](desktop-video-backpressure.json).
- Restricted UIAccess candidate: installed and failed real button delivery;
  UIAccess is removed. Desktop-handle access was reproduced with a controlled
  before/after fixture; final installed-service acceptance is pending.
- Physical phone fluidity, remote secure-desktop input and independent WAN
  performance remain unverified by these local checks.

No invitations, relay credentials or screen contents are included here.
