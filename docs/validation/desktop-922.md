# Desktop build 0.10.2+922 validation

This is an unpublished local development build. Windows uses the ordinary
`lib/main.dart` entry and includes the matching FFI DLL and helper. Android ARM64
and x64 packages use the same production entry, persistent signing identity and
ABI-specific version codes 2922 and 4922. The separate acceptance entry is never
included as the production application entry.

## Changes retained

- One Enable action owns installation, background configuration, input policy,
  automatic startup and a reusable invitation. A pending installation survives
  the approved restart; transient IPC failure resumes without another installer.
  Cancellation retires that automatic intent.
- Existing service identity remains authoritative within its relay namespace.
  Native revocation serializes with installation handoff and checks the actual
  service scope instead of relying on an old UI snapshot.
- A fresh presented image gates control. Cursor/status events update independently
  without refreshing image age or discarding video with the same frame ID.
- Outstanding compressed video is bounded to eight frames. A replacement IDR
  retains exclusive recovery credit until its actual ACK. ACKs coalesce over
  33 ms; old unsent frames and absent presentation retain their existing bounds.
- Android removes the additional playback delay while retaining one outstanding
  SurfaceTexture output and discarding obsolete decoded outputs safely.
- Normal disconnect exchanges TLS close notifications. Local Stop cancels that
  wait even when the peer stops reading.

## Verification

The final code passed 43 desktop unit tests, 39 FFI tests and two direct/relay
session integration tests. Relevant all-target/all-feature Clippy with warnings
denied and workspace formatting passed. Flutter analysis and focused recovery,
lifecycle, gesture and readiness tests passed; Android's three presentation-queue
tests and release compilation passed.

Final testing exposed a local-stop regression in the graceful TLS close path.
Cancellation now takes priority over that close wait. The original slow-reader
test retained its two-second limit and completed Stop in 2 ms in the focused
Windows debug run; the full desktop unit suite subsequently passed. Normal
direct and relay disconnects left the host waiting with an empty error message.

The explicit approximately 35-second synthetic-source, real-TLS feedback test
retained its original assertions: 750 kbit/s at second 8, 9.06 Mbit/s and 960×540
by second 30, and closure approximately 4.2 seconds after ACKs stopped. This is
bounded-congestion evidence, not native frame presentation or motion-to-photon
latency. It can be reproduced with the ignored
`presentation_feedback_recovers_after_slow_tls_and_renderer` library test.

Package checks cover the fixed Android signing certificate, package identity,
versionCode, single ABI and ZIP/ELF 16 KiB alignment. ARM64 upgrade compatibility
was checked against the existing build 914 package (2914 → 2922). Windows package
DLL/helper hashes match the final release outputs. Local package metadata and
SHA-256 values accompany the artifacts.

## Installed host and input checks

An installed build 922 candidate passed one-action enabling, with the service
Running and Automatic. The UI process was completely terminated and reopened;
the service retained its PID and the invitation fingerprint was unchanged.
Stop removed sharing, and one Enable action resumed it without another UAC
prompt. Clearing the clipboard to a test marker before copying again verified
that the unchanged invitation was freshly obtained.

Android VM tap, hold-drag, two-finger right click and pinch, plus trackpad drag and
right click, reached a benign owned Windows target. Text insertion and scrolling
also reached that target. These checks used actual input events and target
counters, separately from the no-input video measurements.

### Final protected-service update, 2026-11-06

The user subsequently approved installation of the final package built from
`33275f87413040f8e6b1dfc1237b775898a07cea`. The protected helper, FFI DLL, UI
executable and Dart AOT file all match the delivered package by SHA-256. The
upgrade replaced service PID 99536 with 97260, retained Automatic startup and
reopened the unprivileged UI with sharing enabled. A newly copied invitation
matched its fingerprint before the upgrade.

The final normal UI displayed live video on both direct and forced-relay paths,
automatically acquired the existing control grant after a current picture, and
disconnected without a host error. These were functional checks, not new FPS
benchmarks. Full UI process termination left the service running. With no
desktop test UI process present, a separate production-FFI viewer received 680
and 306 distinct H264 frame IDs in eight-second direct and forced-relay runs,
respectively. It used a TCP relay carrier, simulated consumption ACKs, never
requested control, and did not decode or present the pictures. This establishes
background video delivery, not displayed FPS or motion-to-photon latency.

Stop remained effective after fully terminating and reopening the UI. A single
Enable then resumed sharing without UAC and retained the same freshly copied
invitation. The final service was left Running/Automatic with sharing enabled.
The private test invitation file was removed. Hashes and bounded observations
are in [`desktop-922-installed.json`](desktop-922-installed.json).

The independent Python/FFI probe failed with Windows socket error 10054 using
the saved QUIC relay carrier; the normal UI direct/relay checks and the same
probe using a temporary TCP profile succeeded. The user's saved profile and
network policies were not changed. No broader QUIC diagnosis follows from this
check. Successful local installer consent is not evidence of remote control on
the Windows secure desktop. Upgrade-driven service replacement was verified;
standalone SCM restart, OS reboot and sleep/resume remain separate checks.

## Measurement environment and rejected experiments

Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X, RTX 5070 Ti (driver 32.0.16.1088),
2560×1440 at 165 Hz, Rust 1.99.1 and Flutter 3.44.9. Android is an Android 36
x86_64 emulator with a 60 Hz virtual display, host OpenGL and
`c2.goldfish.h264.decoder`. No builds ran during the timed measurements.
Both endpoints use the same Windows machine; the relay path uses a deployed
TCP relay. This does not establish independent-NAT or physical-phone performance.
Native presentation uses texture-consumption counters after a five-second
warmup; frame ACK delay includes transport, rendering and feedback coalescing.

The final native Windows build recorded **84.75 presented FPS** at 1920×1080
over the actual direct path in a 60-sample run. Unique capture/receive averaged
89.91 FPS and frame ACK delay averaged 17.35 ms, with no error samples,
reconnections or SDK/native queue drops. The moving pattern remained visible
throughout. A queued Windows firewall prompt for an already-finished test
executable was also visible on the captured desktop; it was not remote UAC.
The corresponding native counters and binary hash are in
[`desktop-922.json`](desktop-922.json). The table below preserves the longer
Android diagnostic runs with the retained congestion/media logic; later fixes
to installation, revocation and TLS close do not turn these into phone evidence.

| Viewer/path | Samples | Native presented FPS | Mean frame ACK |
| --- | --- | --- | --- |
| Final Windows/direct, 1080p | 60 | 84.75 | 17.35 ms |
| Final Android VM/forced-relay smoke | 60 | 14.24 | 427.10 ms |
| Android VM/direct diagnostic baseline | 120 | 17.78 | 251.36 ms |
| Android VM/forced-relay diagnostic baseline | 120 | 15.94 | 384.3 ms |

The final Android smoke had no error samples, reconnections, SDK queue loss or
decoder recoveries. The host stayed enabled after normal disconnect, waiting
with an empty error message. This confirms the final close/ownership behavior;
it does not meet a 60 FPS mobile target.

A two-Surface-slot experiment increased emulator surface delay by about 50 ms
for approximately one extra presented FPS and was reverted. A separate
low-load reference-mean experiment restored higher resolution but reduced
presented FPS from 15.94 to 13.66, raised sampled ACK delay from 384.3 to 439.6 ms,
and increased decoder configurations from 5 to 17 over comparable 120-second
runs. It was also reverted. Neither experiment is in the final package.
Live relay conditions and brief gesture checks mean these runs are not a
strictly controlled before/after benchmark.

The retained controller remains conservative: the emulator can stay at minimum
quality under persistent delay/jitter. The VM does not establish a physical
phone's 60 FPS capability. Surface delivery, GPU work and Flutter scheduling
remain partly combined in its timing measurements.

## Outstanding acceptance

Physical phone (deferred by the user until a device is available); remote UAC
consent and standard-user credential prompts under
default Windows policy; same-owner lock-screen password entry; service/OS
restart outside the verified upgrade and sleep/resume; independent-NAT UI
operation. No Windows password was
stored, no UAC policy was changed, and no secure-desktop success is inferred from
ordinary desktop viewing or a local installation confirmation.
