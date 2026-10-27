# Changelog

All notable changes to this project will be documented in this file.

## [0.9.8] - 2026-10-28
- Publish the SDKs with everything in 0.9.7. Its SDK release stopped at a documentation check before anything was published, so crates.io and npm move from 0.9.6 straight to 0.9.8, and this is also the first release with Android packages since 0.9.6. The app, CLI and wire protocol are otherwise as in 0.9.7.
- Build the documentation on every pull request, as the SDK release does, so a broken doc link fails there rather than on a release tag.

## [0.9.7] - 2026-10-28
- Reach the relay over QUIC as well as TCP. `pb-mapper server --quic-listen 0.0.0.0:7667` offers it (add `--quic-advertise-port` when NAT maps the port), and `register` and `connect` take `--relay-transport quic`. TCP stays the default, each end chooses for itself, and a client that asked for QUIC reports why it cannot have it instead of quietly using TCP. Discovery is one authenticated TCP request that returns the UDP port and the relay's certificate, so the TCP port is still needed; every tunnel of a process then shares one QUIC connection per relay and credential, pinned to that certificate and bound to the session's TLS exporter, with 0-RTT off. The relay bounds handshakes and connections, at most 16 per temporary credential, and says so with a retryable refusal of its own when full. See `docs/relay-quic.zh-CN.md`.
- When a tunnel's stream fails, retire only the QUIC connection it used, and only once that connection has gone silent, rather than every pooled connection to the relay. A temporary credential with many tunnels no longer runs into its 16-connection limit after repeated failures. A failed name lookup leaves the pool alone.
- Choose the relay carrier in the app. Settings offers TCP or QUIC and says whether the relay offers QUIC; on a switch, the app offers to start running tunnels again on the new carrier, since each keeps the one it started with and shows it. Over QUIC the Relay page shows the link's round trip, loss and streams, and the relay this computer runs can offer a QUIC port. The FFI's `pb_mapper_update_config` takes the carrier, `pb_mapper_start_server` takes a QUIC port, and the relay call has a `link` operation.
- The Rust SDK gains `Client::with_relay_transport`, `relay_transport` and `relay_diagnostics`, and its status and administrator calls follow the client's carrier; the Node SDK takes `relayTransport` and reports the same diagnostics.
- Sign the macOS app and disk images with a Developer ID and notarize them, so Gatekeeper opens them without a warning.
- Recover Windows direct handshakes when a wildcard-bound UDP socket rejects a redundant IPv4 source hint with error 10022. Retry only when Windows would select the same source, preserving the socket, port and authenticated direct/fallback behavior.
- Move to Quinn 0.12 and rcgen 0.15, with Quinn's UDP batching chosen per platform at compile time and adapted to the backlog on Apple systems. Bulk transfers ran about 18% faster on macOS loopback; see `deps/README.md` for the measurements and their limits.
- The crates.io SDK now builds on the same Quinn and rcgen source as the binaries. The pinned forks are published as `quinn-plus` (with `quinn-proto-plus` and `quinn-udp-plus`) and `rcgen-plus`, imported under their usual crate names, so the Windows fix above reaches SDK users too.

## [0.9.6] - 2026-10-26
- Download and verify Android updates inside the app, then open the system installer. Continue automatically after install permission is granted, and reuse verified downloads after a denied permission or cancelled installer.
- Build mobile releases locally with a pinned persistent Android certificate, following proxy-everything. Stop publishing CI-signed APKs and unsigned IPAs; verify APK identity, effective versionCode, signatures, and 16 KiB alignment before adding them to the signed release manifest.
- Refuse unsupported desktop architectures, report failures to open downloaded packages, and prevent a cancelled or disposed update from proceeding to installation. Document the one-time migration required from older disposable Android signing keys.
- Android migration: APKs from old CI releases cannot be updated in place because their signing keys were disposable. Export settings before uninstalling the old app, install this release, and import settings. Future releases retain this signing identity and update in place.

## [0.9.5] - 2026-10-25
- Install Windows updates from inside the app. None ever had: after the download, the app started a hidden PowerShell to wait for it to quit, run the installer and open it again, but started it in a way that gives it no console, and PowerShell exits at once without running a line. The app then closed, nothing was installed, and the next launch said the update had not gone in. The hand-off is now started from the app's Rust library with a console of its own that has no window, and it outlives the app. Windows still asks for permission when the app is installed for all users.
- Copies of 0.9.4 and earlier carry the old hand-off, so updating them from inside the app still fails: install 0.9.5 by hand once, and updates from it on go in by themselves.
- If the hand-off cannot start, the update now fails in the app, which stays open, instead of closing. Setup writes its log to `setup.log` beside the download, for when an update does not go in. The CLI, both SDKs and the wire protocol are unchanged.

## [0.9.4] - 2026-10-25
- Go direct in the app, as the CLI has since 0.8.0. The app's tunnels ran on the relay-only entry points, so every one went through the relay. They now run on the SDK's `Registration` and `Connection`, the workers the CLI runs: each connection tries a direct path first and falls back to the relay before any traffic is sent, and each share lets connections reach it directly. The app's own relay offers direct over UDP on its TCP port, as `pb-mapper server` does.
- Show which way each tunnel's traffic goes. Each running tunnel on the Tunnels page carries a chip: **Direct**, **Relay**, both with their counts, **Going direct…** while a path is being set up, or **Relay only**. In the galaxy a direct arc is lit end to end and lands past the relay on the planet of the service it reaches, while a relayed one falls into the relay's star; the legend shows both. A tunnel's details panel draws its route and says what it means, and when it is the relay after all, why: the relay offers no direct, the sharing computer declines it, or the two networks could not reach each other.
- Turn direct off for one tunnel under **More options** in the tunnel form, as `--relay-only` does; it is saved with the tunnel. The control CLI's `register` and `connect` take `--relay-only`.
- A tunnel's state now comes from its worker as it happens, rather than from the app asking the relay every few seconds. The FFI's `pb_mapper_register_service` and `pb_mapper_connect_service` take `allow_direct`, and the tunnel lists report each running tunnel's `path`. The galaxy's buffer layout is version 2. The CLI, both SDKs and the wire protocol are unchanged.

## [0.9.3] - 2026-10-25
- With the administrator key, see and connect to every temporary credential's services, not only your own. A relay whose services were all registered by temporary credentials no longer shows none. The galaxy, the Tunnels count, the editor's suggestions and the Relay page list every service the administrator can reach. A credential's service carries its owner's tag and connects in one click into that credential's namespace, and the connection says whose service it reaches. The FFI's `pb_mapper_connect_service` takes the namespace, saved with the connection, and the connection list reports the namespace each running connection was started in, so one that outlives a change of credential is still told apart; the control CLI's `connect` takes `--namespace`.
- Say after an update whether it installed. On Windows the installer reopens the app from the background, where it could come up out of sight, so the update seemed to fail, and a declined permission prompt looked the same. The app now brings itself forward after an update and says "Updated to pb-mapper 0.9.3", or that the update did not install and what to do.
- The CLI, both SDKs and the wire protocol are unchanged.

## [0.9.2] - 2026-10-25
- Give the app a **Relay** page, between Tunnels and Logs, for the relay as a whole. Its head says whether the relay answers and how fast, who this computer is to it (administrator, temporary credential #n, or neither) and what it holds. Its sections are the relay's **services**, searchable, with Connect and the connections held for each; its **connections** and direct sessions; and the relay **this computer** can run itself, with its key, moved here from Settings. The rail's relay status and the galaxy's relay star open it.
- With the administrator key, the app does everything `pb-mapper admin` does. **Credentials**: issue one with a name and a lifetime (shown once, with a setup string for the computer it is for), reveal, renew, revoke and clean up. **Security**: credential statistics, allowing older clients, rotating the administrator key, and resetting every credential. Sections follow what the relay says the credential is, and a temporary credential is told what the administrator key would add.
- Tell services apart by namespace as well as name, as the relay routes them: two credentials' services of the same name are two rows, each retired in its own namespace, and Connect is offered only for services this computer can reach.
- Rotate the administrator key safely. The page says before it starts that rotating also erases every temporary credential, as the relay always has. The new key is made before the request is sent; a lost reply is settled by asking the relay which key it takes; and a key this computer could not save (the reply lost, Settings moved to another relay meanwhile, or the save failing) stays on the Relay page until it has been copied, with no further rotation until then. Once the new key is saved, every tunnel this computer runs on that relay with the retired key starts again with it, since a tunnel keeps the credential it started with; one on a temporary credential, which the rotation erased, is named for a new credential rather than moved into the administrator's namespace, and any that would not start is named too.
- Settings' **Test** now says what happened: the relay's latency and service count, or its own reason for not answering. Testing edited fields saves them first.
- The app's FFI library gains one export, `pb_mapper_relay_call_json`, for every relay question and administrator operation. The CLI, both SDKs and the wire protocol are unchanged.

## [0.9.1] - 2026-10-25
- Let the app update itself. Settings › About shows the version and **Check for updates**, and the app looks for a new release about twice a day unless turned off, announcing each once; nothing is downloaded until the user presses Update. Updates come only from a `vX.Y.Z-ui` release whose `release-manifest.json` carries an Ed25519 signature by the release key (checked in Rust with `ring` through a new leaf FFI export, `pb_mapper_ed25519_verify_v1`), and only as a file whose size and SHA-256 that manifest lists. The Windows installer then runs silently once the app has quit and reopens it, a Linux AppImage is swapped in place, a macOS disk image or a portable archive is saved to Downloads and shown, and Android opens the APK in the browser. 0.9.0 has no updater, so this is the first release to update from, not to.
- Name the Windows packages like every other platform's: `pb-mapper-windows-x86_64-setup.exe` and `pb-mapper-windows-x86_64.zip`, instead of `pb_mapper_ui-1.0.0+1-windows-…`. The app's own version was never set and read 1.0.0; it is now the workspace's, and a test and the release workflow keep the two in step.
- The macOS app may now write to the user's Downloads (`com.apple.security.files.downloads.read-write`), where updates are saved. Settings' About card fits a 320 px phone. The CLI, both SDKs and the wire protocol are unchanged.

## [0.9.0] - 2026-10-24
- Redesign the desktop and mobile app from scratch, after POCOX's design language: four pages and a three-step first run in place of five zones, two split workspaces and an ops area. **Overview** draws a galaxy: this computer and the relay are two stars, every tunnel and every service on the relay is a planet in its state's colour, and a running tunnel is an arc of light to the relay; click a body to start, stop, edit or connect it. **Tunnels** lists what this computer shares, what it connects to and the relay's other services, each with a Connect button. Sharing and connecting are one editor with a Share/Connect switch, with encryption and keep-alive folded away. **Settings** carries a setup string to paste on another computer. The window's title bar is the page's, with caption buttons drawn on Windows and Linux.
- Compute the galaxy's per-frame geometry in Rust: `pb-mapper-galaxy`, a dependency-free crate exported by the app's FFI library as the leaf call `pb_mapper_galaxy_frame_v1` (about 11 µs a frame), with a Dart mirror where the library is not loaded. It is additive: the CLI, both SDKs, the wire protocol and every existing FFI export are unchanged.
- Keep every page current from one store, refreshed by the Rust change stream, a read of this computer's tunnels every six seconds (the Rust side only probes a tunnel's health when it is read) and a relay check every twelve; the tray reads the same store instead of polling the FFI on a timer of its own.
- Fix editing a share: a refused edit left the old share stopped, and a new configuration that was not up within ten seconds deleted the share outright. A refused edit now leaves the old tunnel running, or starts it again, and a slow one is followed rather than deleted. Fix a tunnel that came up at once never reporting its result: waiting for it to settle awaited a close that could not complete.
- Render the README posters from the app itself (`ui/test/gen_poster_test.dart`), in English and Chinese.

## [0.8.1] - 2026-10-24
- Fix a Windows relay that stopped admitting connections about a minute after starting, and admitted none after a restart: first flights and admin mutations were refused with `connection_replay_store_unavailable`. Compacting or reloading the first-flight replay log replaced the file while still holding it open, which the hand-rolled `MoveFileExW` call refused, and one failed rewrite disabled the log until the process exited. The log is now closed before it is rewritten and replaced through `std::fs::rename`, and a partly written record is rolled back through a handle that can truncate it, which a Windows append handle cannot. Linux behavior and durability are unchanged.

## [0.8.0] - 2026-10-24
- Make direct with relay fallback the default everywhere. `pb-mapper server` serves direct coordination on its TCP port number over UDP (7666/tcp + 7666/udp), `register` allows direct sessions and `connect` uses `--path auto`; the Rust SDK (`p2p` is now a default feature of `pb-mapper`) and the Node SDK follow the same defaults. `--relay-only` on `server`, `register` or `connect` (and `allow_direct: false` / `PathPolicy::RelayOnly` / `path: "relay-only"` in the SDKs) keeps that side on the relay. The 0.7 flags `--enable-direct`, `--allow-direct` and `--path auto` are still accepted.
- Need no public IP to enable direct: without `--direct-advertise` the relay announces only its UDP port and clients pair it with the relay address they dialed, which also works behind cloud 1:1 NAT and port forwarding. A relay started without any direct flag keeps serving relay-only, with a warning, if its UDP port is taken.
- Pause direct on a tunnel after a pre-commit fallback (10 s, doubling to 5 min; cleared by a direct success or a network change), so a network that cannot be punched costs one attempt instead of a full setup budget per connection. Concurrent setups that queue behind a failed capability query go straight to the relay, and a burst of fallbacks counts as one.
- Show recovery as it unfolds in `TunnelDiagnostics`: `direct.attempt` gives the stage of the direct attempt in flight (negotiating, discovering, punching, committing), and `events` keeps the last 16 relay-ready, relay-failed, network-changed, direct-established and direct-fallback events, the fallback naming the stage it left. Consecutive repeats fold into one entry. Both are written only on control-path transitions; the data path and wire protocol are unchanged.
- Node: add `nextStatus(timeoutMs?)` on `Registration` and `Connection`, a per-handle status listener that never misses a change and resolves at once after the tunnel settles; expose `events` and `direct.attempt` in `diagnostics()`.
- Add runnable SDK examples covering every capability: an in-process `quickstart`, TCP and UDP tunnels, path options, event listening, status queries, credential lifecycle, relay inventory and the agent remote-control pattern in Rust, plus register, connect, admin and remote-control scripts for Node.
- Publish UDP 7666 in the Docker image and compose file, with `PB_MAPPER_RELAY_ONLY` and `PB_MAPPER_DIRECT_ADVERTISE` for the entrypoint; document the UDP port in the installers, systemd notes and the deployment Skill. Rewrite both READMEs and redesign the poster, now in English and Chinese.
- 0.7 clients that opted into direct fall back to the relay against a default 0.8 relay, since they do not pair the announced port with the relay address; upgrade them or start the relay with `--direct-advertise`.

## [0.7.0] - 2026-10-23
- Add opt-in direct connections: a relay `--enable-direct` UDP service, publisher `--allow-direct`, and per-connect `--path auto` try UDP hole punching plus mutually authenticated QUIC. Defaults keep the existing relay protocol and control behavior.
- Fall back to the existing relay before direct commit, through a one-use grant pinned to the selected publisher and generation; a committed connection is never replayed after acknowledgement loss. An admitted fallback keeps its namespace slot, a setup that ends before its grant pays for its relay fallback, coordination replies wait for control-queue space, and an older direct-capable registration is no longer hidden by a newer relay-only one.
- Keep established direct sessions owned by the endpoints across coordinator outages and credential revocation; expose namespace-scoped observations and restore them after coordinator restart.
- Receive direct traffic in place inside Quinn's batched, GRO-aware receive path with no extra task, copy or allocation per packet; raise the per-stream QUIC window to 8 MiB for long paths; tolerate Windows ICMP resets. Rare direct messages are boxed so relay mailboxes keep their previous size.
- `set_process_msg_header_key` no longer writes `MSG_HEADER_KEY` into the process environment, which raced other threads' environment reads during root-key rotation and UI configuration changes; the credential lives in process state only.
- Add Rust SDK options (`p2p` feature) and Node `allowDirect` / `path` / `sessionsJson()`; build Quinn, rcgen and stun-proto from maintained fork submodules. Low-level wire enum constructors need the new optional fields. The toolchain and Rust SDK minimum version move to 1.99.1.

## [0.6.1] - 2026-10-19
- Add per-registration TCP idle deadlines to the Rust SDK (`register_with_idle_timeout`) and Node API (`registerWithIdleTimeout`) so publishers can reclaim abandoned data connections.
- Reset the deadline on traffic in either direction; preserve ordinary registration defaults, control connections, half-close draining, and zero-as-disabled behavior. Reject UDP and unrepresentable timer deadlines before starting a worker.
- Regenerate Node loader version checks and TypeScript declarations alongside the native addon.
- Keep the existing wire protocol and compatibility with deployed relays and subscribers; upgrading the publishing application is sufficient to adopt the new policy.
- Verify encrypted idle reclamation in the Rust SDK and registration lifecycle and argument validation through Node, and isolate stale-control recovery tests from concurrent timeout overrides while waiting for confirmed manager cleanup.

## [0.6.0] - 2026-10-04
- Negotiate authenticated data-v2 independently on each relay leg, with independent per-leg keys and disjoint directional nonce prefixes with checked counters; no data-key derivation, extra round trip or per-frame bytes.
- Preserve rolling wire upgrades with 0.5.2 across TCP/UDP and encrypted/plain traffic. Existing credentials, service keys and the default unencrypted mode are unchanged.
- Make interrupted control-v2 writes terminal so cancellation cannot reuse a nonce or restart a partial frame.
- Keep high-level Rust and Node SDK calls compatible. Low-level Rust request/response enum constructors must initialize the new optional `data_protocol` field, which requires this minor version bump.
- Full data-v2 protection requires an updated relay and both endpoints; old peers continue using the legacy data format on their leg.

## [0.5.2] - 2026-10-03
- Make relay manager admission cancellation-safe, fence commands from closed connection lifetimes, and abort owned relay helpers on parent cancellation.
- Retain relay hostnames, retry initial DNS failures, coalesce OS-resolver refresh and bound outstanding lookups without bypassing VPN/TUN DNS.
- Wake recovery from network-address hints and authenticated relay recovery while preserving healthy controls and forwarded data.
- Share per-relay setup budgets across SDK mappings and distinguish timeout, DNS, service absence and permanent rejection in timing/backoff decisions.
- Expose running SDK versions and bounded recovery diagnostics in Rust, Node and CLI logs; prevent async tracing spans from leaking into unrelated tasks.
- Add cancellation, stale-ID, DNS, prolonged-outage, notification-burst and shared-capacity regressions alongside encrypted fault-injection coverage.

## [0.5.1] - 2026-10-03
- Recover registrations and new subscriptions through bounded, jittered setup attempts, adaptive latency budgets, independent control watchdogs, and cancellation-safe legacy/v2 frame reads.
- Keep connect listeners and healthy data streams alive during transient relay probe/control failures; bound pending setup and control queues so one stalled service cannot block the relay.
- Allow late authenticated activity to clear a suspect control connection before retirement, and cover simultaneous multi-service outages with repeated fault-injection recovery tests.
- Race up to two relay address candidates so an unresponsive first IPv4/IPv6 address cannot hide a working alternative; cancel losing and abandoned attempts.
- Keep SDK workers owned across cancelled/concurrent stop calls, prevent administrator mutation retries after partial writes, and redact raw credentials and SDK configuration in debug/tracing output.
- Reuse an admitted subscription's namespace slot and rate token during control-connection failover, including at the namespace capacity limit.
- Preserve the existing local TCP_NODELAY latency fix and publish the CLI/server crates alongside the Rust and Node SDKs.

## [0.5.0] - 2026-08-25
- Added the public `pb-mapper` Rust SDK for register, connect, status, and administrator operations, with handle-based readiness and shutdown.
- Added the `pb-mapper` TypeScript/JavaScript package through Node-API, including generated types and end-to-end tunnel and credential-lifecycle coverage.
- Reclaimed stale registration leases, backed off on relay registration rejections, and made administrator connection retirement cancel wedged sockets unconditionally.
- Switched the client stack from the Git checkout of `uni-stream` 0.3.2 to the crates.io release 0.3.5, including its stable Hickory 0.26 resolver fix.
- Added coordinated crates.io and multi-platform npm publication for the Rust and Node SDKs alongside the existing CLI, UI, and Docker tag releases.

## [0.4.0] - 2026-08-22
This is also the first published release to carry the 0.3.0 changes below: 0.3.0 was never tagged, so its single-CLI consolidation ships here.
- Added a sole administrator credential plus renewable, expiring, and immediately revocable `pbmt1_` temporary credentials with fixed-slot O(1) lookup and isolated per-key service namespaces.
- Added single-flight protocol-v2 authentication with directional AES-256-GCM keys, monotonic frame counters, authenticated routing metadata, durable first-flight replay protection, and optional legacy framing during migration.
- Added encrypted snapshot/WAL authentication state, exclusive `auth.lock`, lifecycle audit records, hierarchical timing-wheel expiry, hard closure of revoked live connections, recoverable root-key rotation, and explicit auth-state reset.
- Extended the unified CLI with temporary-key lifecycle, service/connection inventory, auth status, protocol policy, root rotation, namespace targeting, and human/JSON/NDJSON output.
- Replaced insecure default-key fallback with first-start random administrator-key generation, retained machine-derived keys only for explicit compatibility, and updated Flutter, installers, systemd, Docker, release metadata, and bilingual documentation.
- Recovery keys must decrypt existing snapshot or WAL state before they are persisted. Interrupted rotation and reset recover from staged `admin.key.next` and `server-instance-id.next`.
- First-flight salts are unique for nonce 0: admission is atomic under one lock, torn replay records fail closed, and a nonce-0 error frame is sent only after that salt is reserved.
- Pinned UI and local tunnels to the credential and relay address captured at start, and bound tunneled-frame checksums to each hop's authenticated session key.
- Aborted pooled registration workers and accepted connection tasks on shutdown; the relay reaps connection tasks with a `JoinSet`.
- Split the single crate into six shipped crates under `crates/` plus a test-support one, with the root manifest now virtual: `pb-mapper-core` → `pb-mapper-auth` → `pb-mapper-protocol` → `pb-mapper-server` and `pb-mapper-client` as peers → `pb-mapper-cli`, which holds the `pb-mapper` binary. Each crate owns its own error type and wraps the layer below as a `source` rather than sharing one enum.
- Moved to Rust 1.98.0 and edition 2024, and lifted `clippy::unwrap_used` and `clippy::expect_used` to `deny` for the whole workspace, which every crate and the FFI cdylib now inherit.
- Replaced the frozen `trust-dns-resolver` with `hickory-resolver` 0.26. Custom DNS servers now apply on the async path only: hickory ships no blocking resolver, so `get_socket_addrs_from_host_port` resolves through `std` alone. This matches what the sync path already did in practice, since it fell back to `std` whenever a custom resolver was absent — as it always was inside a Tokio runtime.
- Upgraded snafu to 0.9, hashbrown to 0.17, base64 to 0.23, and dirs to 6.0.
- Removed dead weight rather than carrying it across the split: five error variants left over from externalising `uni-stream`, three unused `#[macro_export]` forwarding macros, `Aes256GcmCodec::try_new_with_default_key`, the unused `once_cell` dependency, and three unreferenced cargo profiles.
- Extracted the end-to-end tunnel harness into `pb-mapper-testkit`, so any test file can stand up a full `server` + `register` + `connect` flow instead of one file owning it, and covered the temporary-credential lifecycle end to end: namespace isolation, renewal, expiry, and revocation.

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
