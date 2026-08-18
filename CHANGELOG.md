# Changelog

All notable changes to this project will be documented in this file.

## [0.4.0] - 2026-08-18
- Added a sole administrator credential plus renewable, expiring, and immediately revocable `pbmt1_` temporary credentials with fixed-slot O(1) lookup and isolated per-key service namespaces.
- Added single-flight protocol-v2 authentication with directional AES-256-GCM keys, monotonic frame counters, authenticated routing metadata, replay detection, stable structured errors, and optional legacy framing during migration.
- Added encrypted snapshot/WAL authentication state, lifecycle audit records, hierarchical timing-wheel expiry, hard closure of revoked live connections, safe-mode recovery, root-key rotation, and explicit auth-state reset.
- Extended the unified CLI with temporary-key lifecycle, service/connection inventory, auth status, protocol policy, root rotation, namespace targeting, and human/JSON/NDJSON output.
- Replaced insecure default-key fallback with first-start random administrator-key generation, retained machine-derived keys only for explicit compatibility, and updated Flutter, installers, systemd, Docker, release metadata, and bilingual documentation.
- Fixed remaining review findings: installer migration now honors `MSG_HEADER_KEY` from `/etc/pb-mapper/server.env`, isolated relays validate legacy frames with their own administrator key, first-flight replay retention covers the full clock-skew window, and desktop macOS/Windows servers use a user-writable auth directory.
- Rejected NUL/non-printable rotated administrator keys, cancelled in-flight status reads on credential revocation, refused `--force-init-admin-key` when encrypted auth state already exists, bound isolated-relay legacy continuation checksums to the relay key, and doubled first-flight Bloom retention so a max-future timestamp cannot outlive the filter.
- Centralized env-safe administrator-key checks, isolated-relay legacy codec construction, credential-cancellation races, and auth snapshot/WAL paths so later protocol and lifecycle changes reuse one implementation.
- Refused administrator-key initialization whenever encrypted auth state is present, preserved discarded slot generations across capacity changes, rolled back or fail-closed uncertain WAL appends, authenticated first flights before consuming the replay filter, and reused the registration credential for provider streams.
- Capped legacy first-flight allocations and kept legacy framing denied when authentication state enters safe mode.
- Skipped snapshot compaction while startup is in safe mode, fsynced the auth directory when creating `auth.wal`, cleared retained high-slot entries on reset/rotate, dropped the slot write lock before WAL fail-closed cancellation, fail-closed process checksums after the credential is cleared, and rate-limited first flights per credential before consuming the shared replay filter.
- Persisted first-flight replay admissions across restarts, replaced existing auth files atomically on Windows, rejected explicit out-of-range server auth flags, and exposed the embedded relay's isolated administrator key through FFI/UI.
- Made a second administrator first-flight salt replay surface the dedicated retry-exhausted error instead of leaving that path unreachable.
- Compacted the durable first-flight replay log while the relay is running, rolled back torn replay-log appends, rewrote that log atomically, sized first-flight admission from `PB_MAPPER_NEW_STREAMS_PER_SECOND`, and took an exclusive lock on the authentication state directory.
- Fsynced the replay-log directory on first creation, took the state lock before `--init-admin-key`, aborted accepted connection tasks on relay shutdown, and staged `admin.key.next` so an interrupted root rotation can recover a matching key and snapshot.

## [0.3.0] - 2026-08-18
- Replaced the three role-specific executables with one `pb-mapper` CLI and explicit `server`, `register`, `connect`, and `status` commands.
- Consolidated release archives into one cross-platform binary artifact per target and updated Docker, installers, systemd templates, build scripts, deployment skills, and documentation to use it.
- Fixed cancellation-induced TCP tail loss during half-close handling and added a regression test for delayed final writes.
- Made client relay-health checks tolerant of transient failures with safer intervals, timeouts, and a configurable consecutive-failure threshold.

## [0.2.19] - 2026-08-15
- Fixed a failing connection or registration having no way to be stopped. `failed` does not mean the client gave up — it means the status probe could not reach the server, while the retry loop carries on dialling. The row offered Connect in that state, so a failing tunnel retried indefinitely with nothing in the interface able to stop it.
- Fixed connecting reporting success before it had connected. Accepting the request and coming up are different events, and only the first had a message, so a green "client connection started" appeared while the client was failing to reach the server. The settled result now gets its own message, in red, with the reason beneath it.
- Split Operations into Status, Services and Config. The status page carried both the server's state and the list of registered services side by side, which put the longer of the two — the one you scroll — into half a window.
- Added the connections a service is actually holding, on the Services page. Expanding a key lists each control connection with its id, how long since it was last heard from, its generation, its protocol version, and whether the server considers it healthy. This information was previously shown as a Rust debug dump of the whole connection map.
- Added selection and one-click copying for service keys and connection ids, and made the whole row open rather than just its chevron.
- Replaced the notification bar with stacking toasts. The previous one showed a single message at a time, replaced it mid-read when another arrived, and could not be dismissed. Errors now stay until closed; everything else clears itself.

## [0.2.18] - 2026-08-15
- Changed the way out of a workspace. The sidebar's top entry said "Home" and always went there, which meant leaving ops — usually reached from a workspace — dropped you on the landing page to pick a role again. Home is now the app mark in the title bar, the one control in the same place everywhere; the sidebar entry swaps between registering and connecting inside a workspace, and is a real Back elsewhere, returning to the screen you left rather than that zone's default.
- Added the log view to both the register and connect workspaces. Why a registration did not come up is a question you have without leaving the workspace, and the answer used to be one zone away. Removed from Operations rather than duplicated: it is the same view, not a second one.
- Fixed navigation appearing as a rail down the left edge on phones. Between 600 and 1024 pixels the sidebar shrank to a strip of unlabelled icons, and a phone in landscape measures inside that range. Any touch platform now navigates from the bottom whatever its window measures, and so does a desktop window too narrow to label a rail.
- Fixed a list row overflowing by 317 pixels on a phone. The row was laid out against a desktop-width panel — a fixed action button, three icon buttons, and facts sized to their text — leaving under 100 pixels for the content. The facts now wrap, and where the row is narrow the actions drop to their own line.
- Fixed a window narrowed past the breakpoint losing its title bar, and with it the ability to be moved, minimised or closed.
- Replaced the bottom navigation with Material 3's, which has the pill indicator and the label under the icon; the old one drew a bare tinted icon and painted everything unselected a fixed grey that ignored the theme. Both navigations now read one list of destinations, so the bottom bar no longer shows different icons from the sidebar or drops the count off the list entry.
- Added an animated transition between the sidebar and the bottom bar, which previously swapped in a single frame, and slowed the theme change so switching does not repaint the window at once.
- Fixed eight strings still rendering in English under Chinese, in the register and configuration screens. Most already had a translation that was simply not being read.

## [0.2.17] - 2026-08-14
- Fixed the Windows tray icon looking blurry at 125%, 175%, 225% and 250% display scaling. The icons carried 16, 24, 32 and 48 but none of the sizes in between, and `LoadImage` answers a missing size by stretching the nearest entry with GDI, which does no filtering. They now carry an entry for every size the shell asks for.
- Added colour vector sources for the Windows tray icons, which until now existed only as an unreproducible raster, together with a generator that renders every size from them rather than resampling one size from another.
- Fixed the tray icon being sized for the wrong display on mixed-DPI desktops, and never reloading when the display scaling changed, by vendoring `tray_manager` with a DPI-aware Windows plugin.
- Fixed Chinese text rendering at the wrong weight on Windows and Linux. The system fallback there has no 500 or 600 face, so a mixed line rendered its Chinese and its Latin at visibly different weights; a subset of Noto Sans SC is now bundled to supply them. macOS is unaffected, as PingFang SC already has every weight.
- Fixed the log view and server status details falling back to a proportional font on Windows, where the requested `monospace` and `Courier` families match nothing, and columns stopped lining up.

## [0.2.14] - 2026-04-29
- Added V2 control-connection registration metadata, lease responses, and per-service status reporting for exact `conn_id` and `generation` health checks.
- Added server-cli suspect-state probing so missing remote registrations are detected and re-registered without relying on missing heartbeat guesses.
- Added server-side V2 idle lease expiration and client-side healthy-control-connection checks before exposing local listeners.
- Documented the current pb-mapper runtime mechanism with visual topology and request-flow diagrams in both English and Chinese user guides.

## [0.2.13] - 2026-04-29
- Retired stale server control connections instead of keeping them selectable after stream ack or stream-ready timeouts.
- Kept subscribe requests open briefly while replacement control connections register, reducing transient failures during network churn.
- Added continuous client-side key health checks while local listeners are active so missing remote registrations are detected quickly.
- Replaced finite retry loops with capped fast backoff so local server/client processes keep recovering under default settings.
- Added regression coverage for stale control retirement, replacement-control recovery, active client health checks, and non-exhausting retry backoff.

## [0.2.12] - 2026-04-29
- Added parallel server-side control connections for registered services so transient stale control connections can be bypassed quickly.
- Added stream-request acknowledgements and generation checks to reject stale ack/stream responses from previous subscribe attempts.
- Removed missing-pong based local control-connection recycling; real control-plane read/write failures still reconnect immediately.
- Added configurable stream ack, stream ready, and control connection pool settings.
- Added regression coverage for unacked and acked-but-stalled control connection failover.

## [0.2.10] - 2026-04-28
- Fixed server-side connection ID recycling so stale or duplicate deregistration cannot return active IDs to the idle pool.
- Prevented active client/control connection IDs from being reused while still registered in the task manager.
- Added control-plane timeouts for subscribe and stream-establishment handshakes so failed tunnel setup cannot leave downstream requests waiting indefinitely.
- Added regression coverage for duplicate connection deregistration and active-ID reuse prevention.

## [0.2.9] - 2026-02-19
- Removed redundant server-status caching layer from FFI state, eliminating stale-cache UI inconsistencies.
- Removed blocking "server unavailable" banners from service registration, client connection, and status monitoring views so pages remain fully operable regardless of server reachability.
- Simplified client connection view to allow manual service key input alongside dropdown selection.
- Cleaned up automatic server-status retry loops that caused unnecessary background network traffic on mobile.

## [0.2.8] - 2026-02-19
- Fixed mobile UI status check instability where server showed as unreachable intermittently despite correct configuration.
- Added synchronous `forceRefreshServerStatus` FFI endpoint that waits for actual network result instead of returning stale cache.
- Parallelized dual TCP status queries (Keys + RemoteId) with `tokio::join!` to halve round-trip time.
- Increased background status refresh timeout from 800ms to 3000ms to accommodate mobile network latency.
- Fixed Flutter parallel loading race in client connection and service registration views by sequencing config load before status check.
- Upgraded `MSG_HEADER_KEY` atomic hash operations from `Ordering::Relaxed` to `Release`/`Acquire` for cross-thread visibility.

## [0.2.7] - 2026-02-15
- Added one-click configuration export in UI Config page with JSON payload encoded as Base64.
- Added one-click configuration import in UI Config page from Base64-encoded JSON, including validation and immediate apply/save flow.
- Included clipboard-friendly export dialog and import dialog to simplify cross-device/shareable configuration transfer.

## [0.2.6] - 2026-02-15
- Fixed Android UI release verification on `armeabi-v7a` by preferring NDK LLVM ELF tools instead of host `objcopy/readelf` that could not parse the artifact format.
- Added an exported-symbol hash fallback check in the Android UI release workflow to preserve FFI provenance validation when raw and stripped hashes differ.
- Rerolled `0.2.5` as a patch release to unblock end-to-end UI artifact publishing.

## [0.2.5] - 2026-02-15
- Fixed Android UI release workflow false failures when APK-packaged `libpb_mapper_ffi.so` differs at raw byte level from staged FFI output.
- Kept strict FFI provenance checks by adding ELF Build ID and debug-stripped hash fallback verification before upload.
- Rerolled `0.2.4` runtime `MSG_HEADER_KEY` synchronization fixes as a new patch release after CI pipeline stabilization.

## [0.2.4] - 2026-02-15
- Fixed runtime `MSG_HEADER_KEY` propagation so UI-configured key changes take effect immediately without process restart.
- Replaced one-time static header-key snapshot with mutable runtime key state used by checksum and default codec creation.
- Wired UI FFI config application to the shared key setter to keep Linux/Windows behavior consistent.
- Resolved cross-platform mismatch where Windows UI failed against keyed server while Linux appeared to ignore UI key changes.

## [0.2.3] - 2026-02-15
- Fixed `clippy::needless_as_bytes` in `ui/native/pb_mapper_ffi/src/state.rs` to restore strict CI lint pass (`-D warnings`).
- No runtime behavior change; release is a CI/lint hotfix on top of `0.2.2`.

## [0.2.2] - 2026-02-15
- Added full Flutter UI + FFI support for `MSG_HEADER_KEY` configuration, including config persistence, validation, and runtime propagation.
- Made `MSG_HEADER_KEY` optional in UI config: empty value now falls back to the default header key behavior.
- Updated service registration/client connection setup guidance in UI to explicitly include `MSG_HEADER_KEY` consistency checks.
- Hardened UI release build flow to enforce FFI-first build order per platform (Windows/Linux/macOS/Android/iOS).
- Added workflow-level FFI integrity checks (hash verification between staged FFI artifacts and packaged UI outputs).

## [0.2.1] - 2026-02-09
- Added `pb-mapper-server --use-machine-msg-header-key` to derive `MSG_HEADER_KEY` from machine hostname + MAC addresses.
- Persisted the derived key to `/var/lib/pb-mapper-server/msg_header_key` for operator reuse in `pb-mapper-server-cli` and `pb-mapper-client-cli`.
- Added fallback MAC collection paths (`/sys/class/net`, `ip link`, `ifconfig`) to improve portability.
- Updated user guides (English and Chinese) with setup and usage instructions for machine-derived key mode.
- Added/expanded code documentation for public key-related APIs and derivation rationale.

## [0.1.1] - 2026-01-17
- Extracted stream/UDP logic into `deps/uni-stream` and switched core networking to use it.
- Fixed UDP forwarding by preserving datagram boundaries and adding explicit datagram APIs.
- Added `into_split()` owned halves for spawn-friendly IO split.
- Updated UI Rust bridge to pass correct UDP datagram mode to server/client.
- Added deep-dive docs on async Send/Sync/Pin and UDP datagram forwarding.
