# pb-mapper docs

Each page owns one part of pb-mapper: what it does, its limits and what a
change must keep. Start with the [user guide](user-guide.md); contributor rules
are in [AGENTS.md](../AGENTS.md). Pages marked 中文 are written in Chinese.

## Use

| Page | Covers |
| --- | --- |
| [User guide](user-guide.md) · [中文](user-guide.zh-CN.md) | install, every command and flag, direct connections, credentials, the app, environment variables |
| [Docker](../DOCKER_README.md) | the relay as a container: environment, the auth volume, tags |
| [systemd units](../services/readme.md) | the relay, `register` and `connect` as services |
| [Changelog](../CHANGELOG.md) | what each release changed, and what an upgrade needs |

## SDKs

| Page | Covers |
| --- | --- |
| [Rust SDK](../crates/pb-mapper/README.md) | tunnels, administration, diagnostics, and the remote desktop behind the `desktop` feature |
| [Node SDK](../js/README.md) | the same client for Node.js and Bun |
| [Relay library](../crates/pb-mapper-server/README.md) | the relay embedded in your own process |

## Remote desktop

| Page | Covers |
| --- | --- |
| [Remote desktop](remote-desktop.md) | the app, gestures, keys and input methods, sharing a Mac, remote resolution, the Windows helper, the SDK, platforms, the video pipeline and each viewer's decoder, measurements |
| [Validation records](validation/) | device and acceptance runs, by build |
| [Overhaul notes](remote-desktop-overhaul/README.md) | 中文; the redesign's working notes: designs, performance priorities, work plan |

## Paths, relay and recovery

| Page | Covers |
| --- | --- |
| [Direct connections](issue-13-direct-connect-design.zh-CN.md) | 中文; direct first with relay fallback: design and implementation |
| [Direct validation](issue-13-validation.zh-CN.md) | 中文; the acceptance record of direct connections |
| [NAT traversal validation](nat-traversal-validation.zh-CN.md) | 中文; multi-layer NAT, IPv6 and port mapping |
| [QUIC relay](relay-quic.zh-CN.md) | 中文; the optional QUIC carrier to the relay |
| [Weak-network recovery](weak-network-recovery.md) | how register, connect and the relay recover when a route returns |
| [UDP datagram forwarding](udp-datagram-forwarding.md) | why UDP is framed, and how to validate it |
| [Benchmarks](benchmarks/) | the QUIC measurements, the [refactor's performance pass](benchmarks/2026-11-11-refactor/README.md) its [build times](benchmarks/2026-11-11-refactor/compile-time.md) and the open-items passes ([credential store](benchmarks/2026-11-11-refactor/open-items-auth.md), [relay](benchmarks/2026-11-11-refactor/open-items-relay.md), [desktop](benchmarks/2026-11-11-refactor/open-items-desktop.md)), the [remote desktop's data paths before and after every platform](benchmarks/2026-11-12-remote-desktop-platforms/README.md) and the [M3 Pro hardware run](benchmarks/2026-11-17-macos-desktop/README.md) |
| [Multi-device control benchmark](benchmarks/2026-11-18-multi-device-control/README.md) | `host_send_bench` before and after multi-viewer desktop rooms and coloured peer cursors |
| [Desktop pipeline measurements](benchmarks/2026-11-24-desktop-pipeline/README.md) | `host_send_bench` and the slow-link suite before and after armed capture ticks and the receipt-rate estimator, and the A/B runs behind the withdrawn hand-off and thread-priority changes |
| [Capture pacing measurements](benchmarks/2026-11-24-capture-pacing/README.md) | `host_send_bench` before and after present-anchored capture pacing, and the Windows and Mac device runs its effect still needs |
| [Windows host kept awake](benchmarks/2026-11-24-win-keep-awake/README.md) | what a capture call's keep-awake touch costs, and the device checks still outstanding |
| [Rate-model latch measurements](benchmarks/2026-11-24-rate-model-latch/README.md) | the slow-link suite, with an encoder that budgets per second, before and after the rate-model latch read the last 2 s of QoS ticks instead of two 3 s windows |
| [Viewer fps cap measurements](benchmarks/2026-11-24-viewer-fps-cap/README.md) | the slow-link suite, three runs before and after the viewer's fps cap measured its decoder, raised in steps scaled to the ceiling and drained a backlog instead of freezing until the IDR |
| [Shared direct cooldown measurements](benchmarks/2026-11-24-direct-cooldown/README.md) | `connect_tcp`'s time to the first byte inside a direct pause, before and after one client's calls share it |
| [Mac viewer decoder stats](benchmarks/2026-11-24-viewer-decoder-stats/README.md) | the macOS viewer's new decode time and decoder-thread CPU, read by the acceptance tool at 1080p and 4K, against a paced VideoToolbox harness on the same clips |
| [Native pointer shape measurements](benchmarks/2026-11-24-native-cursor/README.md) | `ui/tool/pointer_latency.dart` on the macOS runner: a pointer shape change through a rebuild and the post-frame pass, against the direct system cursor and runner bitmap calls the cursor session makes, and how long a painted pointer took to draw after a mouse move |
| [Viewer pixel snap measurements](benchmarks/2026-11-24-viewer-pixel-snap/README.md) | the contrast a 1-pixel stroke keeps through the viewer's picture transform, before and after keeping a 1:1 picture on device pixels and showing a near-1 fit at exactly 1:1 |
| [Mac desktop check reuse](benchmarks/2026-11-24-mac-session-check/README.md) | what the session and geometry check around each Mac capture and pointer read costs thread C and WindowServer, back to back and paced at 60 FPS, before and after reusing a passing one for 16 ms |
| [Mac VideoToolbox submit thread](benchmarks/2026-11-24-macos-vt-submit/README.md) | what a VideoToolbox submit costs a Mac host's thread C and an input queued meanwhile, and VideoToolbox's dwell with four and two units in flight, before and after the encoder's own submit thread |
| [Mac VideoToolbox rate control](benchmarks/2026-11-24-macos-rate-model/README.md) | what low-latency VideoToolbox drops with and without a QP bound of 51, how soon a dropped last picture after an IDR arrives with and without sending it again, its rate model under a pace that follows QoS, low latency against standard rate control at 120 FPS, and a bitrate cut |
| [Mac capture cadence](benchmarks/2026-11-24-mac-capture-cadence/README.md) | what finding a display costs ScreenCaptureKit with every window and with the on-screen windows only, and reading a display's refresh, on a Mac whose screen was locked, and the device runs the capture cadence, a held-back change, the pause, the start, the kept capture and the filled frame still need |
| [Windows encoder settings](benchmarks/2026-11-24-windows-encoder-settings/README.md) | the VideoToolbox and hand-built SPS samples behind the H.264 SPS rewrite, Core Media building a decoder's format from each rewrite, and the device runs the High profile, quality against speed, the display-remoting scenario and the GPU priority still need |

## Security

| Page | Covers |
| --- | --- |
| [Authentication and protocol v2](authentication-v2.md) · [中文](authentication-v2.zh-CN.md) | the administrator key, temporary credentials, namespaces, persistence, the first flight |
| [Data encryption](data-encryption-compatibility.md) | `--codec`, per-leg negotiation and rolling upgrades |

## App

| Page | Covers |
| --- | --- |
| [UI design](ui-design.md) | the pages, the galaxy, the design system, how the app reaches the relay, tests and previews |
| [Updates and mobile releases](ui-updates.md) | the signed self-update per platform |
| [Building the app](../ui/README.md) | the FFI boundary, the app's architecture, building and running |
| [macOS signing](macos-signing.zh-CN.md) | 中文; signing a macOS release |

## Project

| Page | Covers |
| --- | --- |
| [Architecture](architecture.md) | the code map: every crate and module, the layering, the app's pages and FFI, building and testing |
| [Rust engineering guide](rust-engineering-guide.md) | how Rust is written here: crates, types, errors, async, processes, wire formats, tests and lints |
| [Refactor open items](refactor-2026-11-open-items.md) | what the 2026-11 refactor and its fixes left on purpose: compatibility breaks, large changes, upstream dependencies, trade-offs kept |
| [Bugfix notes](bugfixes.md) · [中文](bugfixes.zh-CN.md) | how recurring problems were found and fixed |
| [Claude code review](claude-code-review.md) | the pull-request review workflow |
| [Introduction](pb-mapper-intro.zh-CN.md) | 中文; why the project exists and how it grew |
| [Shared mutability and locks](rust-shared-mutability-and-locks.zh-CN.md) | 中文; when a data structure needs a lock, from the timing wheel's rewrite |
| [Video script](vibe-coding-everywhere-video-script.zh-CN.md) | 中文; coding from anywhere with HAPI and pb-mapper |

## Historical

Kept for design intent; each describes an implementation that has since changed.

| Page | Covers |
| --- | --- |
| [UI as a CLI](ui-cli-mode-spec.md) | a 2025 design, from before the unified binary |
| [Send, Sync, Pin and async](rust-async-send-sync-pin-deep-dive.md) | 中文; why forwarding futures must be `Send + 'static` |
| [VibeVoice integration](vibevoice-integration-spec.md) | 中文; an integration on the removed Rinf architecture |
