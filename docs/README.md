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
| [Remote desktop](remote-desktop.md) | the app, gestures and keys, remote resolution, the Windows helper, the SDK, platforms, the video pipeline, measurements |
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
| [Benchmarks](benchmarks/) | the QUIC measurements, the [refactor's performance pass](benchmarks/2026-11-11-refactor/README.md) its [build times](benchmarks/2026-11-11-refactor/compile-time.md) and the open-items passes ([credential store](benchmarks/2026-11-11-refactor/open-items-auth.md), [relay](benchmarks/2026-11-11-refactor/open-items-relay.md), [desktop](benchmarks/2026-11-11-refactor/open-items-desktop.md)), raw and summarised |

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
