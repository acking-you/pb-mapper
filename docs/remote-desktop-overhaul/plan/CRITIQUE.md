# CRITIQUE: completeness review of CONTRACT-perf, CONTRACT-ui and WORKPLAN

Reviewed against BRIEF.md, DECISIONS.md, the nine maps and the code at
`a41eaa7` (worktree `pr51-local-work-review-8770c2`). I checked every claim
below in the code. Paths use the contracts' shorthands (`D/`, `F/`, `U/`, `K/`,
`W/`, `T/`). Findings are ordered by severity. Each one names the fix and the
document that has to change.

Verdict: the three documents cover BRIEF requirements 1-4 and D1-D28 almost
completely, and the reconciliation in WORKPLAN §1 is sound. Six problems would
stop a gate or ship a defect, though: H1-H6 below. Fix them before the lead
creates the worktrees. The medium items should be fixed in the contracts too,
because each one becomes a mid-package surprise. The low items are one-line
corrections.

| Id | Severity | One line |
| --- | --- | --- |
| H1 | High | The phone acceptance procedure (Stage 1/3, perf 11.2) cannot run as written |
| H2 | High | D20 backup exclusion misses the Rust config dir on Android (credential, vault, logs stay backed up) |
| H3 | High | While sharing, `DesktopHostView.invitation` has no source; `configured` is wrong for other-relay trust |
| H4 | High | D15 "always-on-top" is not met while the window is hidden or minimised; the deviation was never escalated |
| H5 | High | `p7-d1` cannot be analyze/test green as scoped: old viewer, acceptance tool and one test break; G2 smoke cannot write a thumbnail |
| H6 | High | Missing milestone: `HostStatus`/`Host` API (P2 commit 3) is needed by P3, P5 and P4 commit 3, which depend only on G0 |
| M1 | Medium | The G0 mechanical-edit set is wider than listed and overlaps P3's Wave 1 files |
| M2 | Medium | The non-Windows `scheduler::Wake::notify()` is a no-op. Thread C's lane/cancel wake does not work on Linux CI |
| M3 | Medium | `Key::Unicode` for a-z/0-9 breaks shortcuts and double-types on non-Latin host layouts |
| M4 | Medium | UI bulk input (backspace ×64, latched text) overflows `INPUT_QUEUE = 32`, which revokes control |
| M5 | Medium | R-UI-3 hold: it is unstated whether held input is re-authorised on release; stale presentation does not drop the hold; no security test |
| M6 | Medium | The Windows runner services snapshots only on TIMEOUT/PAUSED, so `disconnect()` thumbnails starve while video flows |
| M7 | Medium | The Android cached-app freezer (test phone is API 36) defeats "resume without reconnect"; no risk or decision |
| M8 | Medium | `DesktopStore.connect` behaviour while an attempt or host action runs is unspecified; today it silently returns |
| M9 | Medium | Vault edge cases: `invitation_for` failures retry forever; `computers_remember` is not bound to the session's identity |
| M10 | Medium | P6 has no video producer on device before G2 |
| M11 | Medium | The ALPN compatibility acceptance item expects text a PR #51 build cannot show |
| M12 | Medium | A paused session ("Back to list") holds the host's only slot indefinitely, and the capsule says "Being viewed" |
| M13 | Medium | Background-host gaps: display switch/rename undefined; double-click metrics read in session 0 |
| M14 | Medium | Breaking `pb_mapper::desktop` SDK API: README/CHANGELOG/semver unowned; the final `rg` check fails on the README |
| M15 | Medium | The shim's coexistence with the new `connect`/`status` ops under one `deny_unknown_fields` enum is unspecified |
| M16 | Medium | `GetDoubleClickTime` needs a `windows` feature that only dev-deps enable, so the release FFI build (G1 shim) breaks |
| L1-L16 | Low | Wording, paths, small test gaps, minor UI rules |

---

## High

### H1. The phone acceptance procedure cannot run as written

Where: WORKPLAN §4.2 "Common phone setup" and Stages 1/3; perf §11.2.

Evidence:

- **Stage 1 uses the old tool, which needs `reportPath`.** The tool throws
  "Set a loopback setupUrl, reportPath and seconds 15..1800" when the define
  is missing (`ui/tool/desktop_viewer_acceptance.dart:19-31`). The WORKPLAN
  command passes no `reportPath`.
- **`run-as` fails on a release APK.**
  `adb exec-out run-as com.pb_mapper.ui cat files/...` works only for
  debuggable packages. The release build type sets only the signing config
  (`ui/android/app/build.gradle.kts:57-61`) and is not debuggable.
- **Both tools write somewhere else.** The old tool writes to the absolute
  `reportPath`, not `files/desktop-acceptance.json`.
- **Release builds need the pinned key.** `flutter build apk --release` aborts
  unless the pinned signing key is present locally
  (`build.gradle.kts:63-84`). The plan never checks that precondition.
- **G2 is blocked.** It "waits for this record" (Stage 1), so the whole G2
  gate is blocked.

Fix:

1. Stop depending on `run-as`. The acceptance tool uploads its report to the
   loopback bootstrap that already exists:
   - a `PUT http://127.0.0.1:18747/report` reaches it through `adb reverse`;
   - `scripts/test/desktop-bootstrap.cjs` writes it to
     `target/desktop-acceptance/report-<n>.json`.

   The schema-2 tool (P7 commit 3) does this. For Stage 1, either:
   - backport the upload into the old tool in P5's shim commit (preferred);
     or
   - pass
     `--dart-define=reportPath=/data/user/0/com.pb_mapper.ui/files/desktop-acceptance.json`
     and build the APK with `--debug`, the only variant `run-as` can read,
     using a release-built FFI `.so`.
2. Add a Wave 0 check: the release signing key resolves on this machine
   (`flutter build apk --release` of the base). Otherwise, state that
   acceptance builds use `--debug` with a release-mode FFI.
3. Make perf §11.2 and WORKPLAN §4.2 identical. Also document how to run Case
   B over QUIC: restart `deployed` without `--tcp` and set `relay_only` in
   `controller.json`, since `deployed.rs:77` writes no `relay_only`. Assign
   `scripts/test/desktop-ui-acceptance.md` to P2's docs commit; it is unowned
   today.

### H2. D20 backup exclusion misses the Rust config dir on Android

Where: CONTRACT-ui §5.3, "Android backup exclusions"; WORKPLAN P1 owns the
XML files.

Evidence:

- **The config dir is not under `filesDir`.**
  - On Android the Rust config dir comes from `getApplicationDocumentsDirectory()`
    (`ui/lib/main.dart:93-104`).
  - path_provider_android maps that to `context.getDir("flutter")`
    (`path_provider_android_real.dart:36-42`), which is `app_flutter/`.
  - Rust appends `pb-mapper-ui` (`F/state/configuration.rs:99-107`).

  That directory is in the backup **root** domain, not under `filesDir`.
- **It holds the secrets.** It contains the relay credential
  (`msgHeaderKey`), `desktop-computers.bin`/`.key` (the vault lives in
  `desktop_config_dir()`), `desktop-trust.*` and the persisted logs (6a695de).
- **The contract's rule misses it.** `<exclude domain="file" path="."/>` only
  covers `getFilesDir()` (thumbnails, application support). D20's explicit
  requirement ("credentials are not backed up") is therefore not met.
- **The deferred check is already answered.** The contract's "UI-2b verifies
  on device" step: the code answers it now.

Fix: in both `data_extraction_rules.xml` (under `<cloud-backup>` and
`<device-transfer>`) and `backup_rules.xml`, add
`<exclude domain="root" path="app_flutter/"/>` next to the `file` exclusion.
Alternatively set `android:allowBackup="false"`, which is simpler and safe,
since shared prefs only hold theme and viewer prefs.

Then:

- Delete the on-device "verify" step from UI-2b.
- Add a Stage 3 check:
  `adb shell bmgr backupnow com.pb_mapper.ui` followed by
  `adb shell dumpsys backup`, or a manual restore test.
- Add a CHANGELOG line: tunnels and credentials are no longer restored by
  Android cloud backup.

### H3. While sharing, `DesktopHostView.invitation` has no source

Where: WORKPLAN §1.1 (R-UI-1 `DesktopHostView` comment); perf §5.15, §7.2,
§9.4; CONTRACT-ui §6.2-§6.3.

Evidence:

- **The formula reads a field that does not exist.** WORKPLAN defines
  `invitation = host?.invitation ?? (!sharing ? (trust.saved?.invitation ?? background?.invitation) : null)`.
  `HostStatus` (perf §5.15) has no `invitation` and no `service` field, and
  `host_status` returns only `{host: HostStatus|null, background}` (§7.2).
- **While sharing, the invitation is null.** By that formula, the invitation
  is `null` for local and for background hosts. The sharing card's
  `InviteWell` calls `maskInvitation(v.invitation!)` (UI §6.3), so it would
  throw, or Copy/QR would have nothing to act on.
- **Today the store caches it.** It caches the string returned by
  start/enable/restore/background (`U/state/desktop_store.dart:63, 210-329,
  754, 806`). The new contract drops that without a replacement.
- **`configured` is wrong for other-relay trust.**
  - `configured = trust.saved != null || background.configured`.
  - `trust_status` returns `saved` even when `matches_relay` is false
    (`F/desktop.rs:584-608`), and WORKPLAN makes `saved.invitation` null in
    exactly that case.
  - A trust remembered under another relay or namespace therefore renders the
    "paused" card with no invitation, and a Start button that `HostRestore`
    will refuse.

Fix (amend WORKPLAN §1.1, which is frozen in `p7-d1`):

1. The FFI `host_status` returns `host` as `HostStatus` plus `service` and
   `invitation` (pbd1) siblings, and the background snapshot already carries
   `invitation`. Or define an explicit store cache. Either way, state the
   order: the host's own value, else the result of the last
   enable/start/restore/set-display/rename, else `background.invitation`,
   else `trust.saved.invitation` when `matches_relay`.
2. `configured = (trust.saved != null && trust.matchesRelay) || background?.configured == true`.
3. State the invariant `sharing || configured ⇒ invitation != null`, and
   assert it in `desktop_store_test.dart` with a case each for local sharing,
   background sharing, a restart with remembered trust, and other-relay trust.
4. `InviteWell` renders nothing (no `!`) when the invitation is null.

### H4. D15 "always-on-top" is not met while the window is hidden or minimised

Where: CONTRACT-ui §0.6, §4.6, §4.7 ("Tray icons unchanged"); WORKPLAN P8;
D15.

Evidence:

- **The surfaces live inside the app window.** The capsule and
  `SharingFallbackLayer` render inside the Flutter window. A sharing host's
  window is normally minimised or hidden to the tray: the close button hides
  it (`window_chrome.dart` close calls `windowManager.close()`).
- **Then only the tray remains, and it does not change.** The tray icon has
  only offline/active/idle states (`U/common/tray/tray_service.dart:307-315`),
  and the contract keeps it unchanged. So a person whose screen is being
  watched or controlled gets **no visible sign** unless they open the tray
  menu.
- **Background hosting has no indicator at all.** The background service
  (SYSTEM, `D/helper/background_service.rs`) can host with the app closed.
- **The deviation was declared but never escalated.** The contract says an
  OS-topmost window "would need a multi-window plugin, which D21 does not
  allow". That reason is incomplete: `window_manager` 0.5.1 already offers
  `setAlwaysOnTop`/`show` on the main window. WORKPLAN never escalates the
  deviation to the owner, which the DECISIONS preamble requires.

Fix: get an owner decision before P8 starts. The default to propose, within
D21:

- (a) While `hostView.viewer != null`, the tray shows a "being viewed" state
  (reuse `tray_active`, or add one asset; this is a state, not an icon
  redesign).
- (b) When a viewer arrives while the window is not visible, show a tray
  balloon or toast "Someone is viewing/controlling this computer", with the
  two actions in the tray menu.
- (c) Optionally, `windowManager.show()` plus `setAlwaysOnTop(true)` in a
  compact capsule-only window mode.

State in BRIEF/DECISIONS that background hosting with the app closed has no
indicator, or require the helper to show a notification. Add the chosen
behaviour to WORKPLAN §4.2 Stage 3 item 4.

### H5. `p7-d1` cannot be analyze/test green as scoped

Where: WORKPLAN P7 ("minimal compile fixes in the old pages"), §3.3 G2
checks, §4.2 Stage 2; perf §12.1 WP-D.

Evidence:

- **The old viewer uses many deleted members.** `U/pages/desktop_viewer.dart`
  uses `store.addListener`, `frame`, `cursorStatus`, `videoPresented`,
  `acknowledgePresented`, `send(rendered, event, mediaEpoch:)` and
  `mediaEpoch` (`:41-205, :443`). perf §9.8 deletes every one of them, so
  this is a port, not a compile fix. The old page must keep working, because
  G2's on-device smoke runs on it (P8/P9 land only at G3).
- **The acceptance tool breaks.** `ui/tool/desktop_viewer_acceptance.dart`
  uses about 15 deleted store members (`:78-153`) and `_AcceptanceApi extends
  DesktopApi` (`:159`). `analysis_options.yaml` excludes nothing, so
  `flutter analyze` is red from commit 1 (`p7-d1`) until commit 3
  (`p7-tool`). P8 and P9 branch from `p7-d1` and must end "analyze clean",
  which they cannot.
- **One old test is unowned.** `T/desktop_viewer_navigation_test.dart`
  extends `FakeDesktopApi` and drives `DesktopStore` plus the old pages
  (`:6-37`). No package owns it before P9 deletes it at G3, so G2's full
  `flutter test` fails.
- **G2 smoke cannot write a thumbnail.** "disconnect (the thumbnail is
  written)" needs a `snapshotPath`, which only P8's `RemoteDesktop`
  supplies.

Fix:

1. Scope `p7-d1` as "whole package green". `flutter analyze` and full
   `flutter test` both pass at the tag. P7 owns:
   - porting the old viewer, page, banner and gesture-surface call sites to
     the slices and `send`/`sendSequence`;
   - the acceptance tool's compile port in commit 1, with the schema-2
     rewrite left in commit 3;
   - deleting `T/desktop_viewer_navigation_test.dart`, since the UI contract
     deletes it anyway.
2. G2 smoke drops the thumbnail item, which moves to G3 Stage 3 item 5.
   Alternatively, the old page passes a temp `snapshotPath` during G2.
3. Add "full `flutter analyze` and `flutter test` green" to P7's acceptance
   for each tagged commit.

### H6. Missing milestone: the `HostStatus` and `Host` API come after G0, but P3, P5 and P4 need them

Where: WORKPLAN P3, P4 and P5 "Depends on"; §0 milestone tags; perf §5.15,
§5.16.5, §12.1.

Evidence:

- **P3 needs the types.** `HostStatus`, `HostViewer`, `HostState` and
  `HostPath` (perf §5.15) live in `D/host.rs`, owned by WP-H, which is P2
  commit 3 ("Report the host's viewer…"). P3's helper work uses them in
  `Snapshot.host: Option<HostStatus>`, plus `EndSession`, which calls
  `Host::end_session` (§5.16.5). P3 depends only on G0, and its "Reads" list
  is the G0 modules.
- **P5 needs the methods.** P5 commit 2 implements `host_status`,
  `host_end_session`, `host_set_display` and `host_rename` (WORKPLAN P5
  goal). It needs `Host::status() -> HostStatus` and `Host::end_session()`,
  yet it depends only on G0 and `p4-vapi`.
- **P4 commit 3 needs P2 and P3 code.** It rewrites `D/slow_link.rs` and
  `tests/session.rs` for receipt credit. `slow_link.rs` is
  `#[cfg(test)] mod slow_link;` in the lib (`D/lib.rs:16-17`), so P4's own
  acceptance (`cargo test -p pb-mapper-desktop --lib`, `clippy --all-targets`)
  compiles code that needs P2's session and credit code and P3's worker. None
  of that is on P4's branch.

Fix:

- **Move the shared types and signatures into G0.** Put `HostStatus`,
  `HostViewer`, `HostState` and `HostPath`, plus the `Host` method signatures
  (`status`, `end_session`, `set_control_allowed`, `stop`) as stubs, into G0
  (`D/lib.rs` or `D/protocol.rs`). Alternatively, add a tag `p2-host` after
  P2 commit 3 and make P3 (helper commit) and P5 (commit 2) merge it.
- **Re-sequence P4 commit 3.** Have the lead run it after the G1 merges, or
  make P4 merge `p2-host` plus P3's worker tag.
- **Update the tag list** in WORKPLAN §0 and the P3/P4/P5 "Depends on" lines.

---

## Medium

### M1. The G0 mechanical-edit set is wider than listed and overlaps P3's Wave 1 work

Evidence: perf §5.10.3 changes `Backend::configure_video(limits, nominal)` and
`capture_video(…, rate)`, and adds `drain_video`, `pending_video` and
`double_click_guard`. Every `impl Backend` must change at G0:

- `D/windows.rs:192-260`;
- `D/slow_link.rs:101, 295`;
- `D/tests.rs:44`;
- `D/host.rs:347`;
- `D/worker.rs` (test fakes);
- `tests/session.rs:26, 212`;
- `examples/input_acceptance.rs:206`;
- `F/desktop.rs:1706`.

`VideoPreferences` and `SessionStatus` are also used by:

- `examples/{deployed,video_bench,viewer_probe}.rs`;
- `D/helper/mod.rs:161` and `D/helper/background.rs:46`.

`clippy --all-targets` at G0 compiles all of them. Yet WORKPLAN gives P2 only
"`D/windows.rs` (`key_code` only)", plus the call sites of `worker.rs`,
`helper/{client,worker}.rs`, `viewer.rs` and `F/desktop.rs`. Meanwhile P3's
Wave 1 edits `examples/video_bench.rs` (`next_frame` at `:146`,
`configure_video(VideoPreferences{…})` at `:67`), `D/video/windows.rs` and,
transitively, the `Desktop` backend in `D/windows.rs`.

Fix:

- **Enumerate the files.** List the exact G0 mechanical-edit set in WORKPLAN
  P2 and perf §12.1 WP-0.
- **Restrict P3's Wave 1.** P3's Wave 1 encoder work touches only
  `D/video/windows.rs`, plus a throwaway bench on its scratch branch, until
  `ovh-g0`. It ports `video_bench.rs` and `D/windows.rs` after merging
  `ovh-g0`.
- **Alternative: keep the old types alive.** Keep `VideoPreferences` and
  `SessionStatus` alive until the G1 cleanup, which X4 already does for the
  wire types, and make the Backend change additive (new methods with
  defaults) at G0.

### M2. Thread C's wake does not work on Linux

Evidence:

- **The non-Windows waker does nothing.** On non-Windows, `Wake::notify()` is
  empty, and `Waiter::receive` wakes only on an `mpsc` message or a timeout
  (`D/scheduler.rs:113-145`).
- **The new design depends on it.** perf §5.1/§5.12.2/§5.10.2 replace the
  mpsc command channel with `InputLane` plus `scheduler::Wake`, and rely on
  wakes for lane push, pace change, pause/resume, IDR force and cancel.
- **The effect.** On Linux, where CI runs `cargo test`, C would sleep until
  its next pacer, cursor or context deadline. While paused there is no pacer
  and no cursor, so the next deadline is the 100 ms context check. Lane input
  and cancel are then delayed, and `input_lane_runs_before_capture…`,
  `pacing_is_even…` and `stop` tests become timing-dependent.

Fix: P3, which owns `scheduler.rs`, implements the non-Windows `Wake`/`Waiter`
with a `parking_lot::Condvar` and a pending flag, or a unit `mpsc::Sender<()>`.
Its `receive` must no longer take an mpsc receiver. Add a Linux-run test:
a lane push or cancel wakes C in under 5 ms. Add this to perf §5.10.2 and
WORKPLAN P3.

### M3. `Key::Unicode` for letters and digits breaks shortcuts on non-Latin host layouts

Where: perf §4.5 ("letters and digits to `enigo::Key::Unicode(lowercase char)`").

Evidence:

- **The fallback types text instead of a key.** In enigo 0.6.1 on Windows,
  `Key::Unicode(c)` falls back to `KEYEVENTF_UNICODE` text when the active
  host layout cannot produce `c` (`enigo-0.6.1/src/win/win_impl.rs:435-440`).
- **It types on both down and up.** `queue_char` emits down **and** up
  regardless of direction (`:475-500`), so a `Key{d, down}` then
  `Key{d, up}` types "d" twice.
- **The effect.** On Russian, Greek, Arabic and similar host layouts, Win+D,
  Ctrl+C/V/S, Alt+F, and the latched taps from the modifier bar do nothing
  except type stray letters.
- **The fix is available.** enigo exposes Windows-only VK variants `Key::A..Z`
  and `Key::Num0..9` (`keycodes.rs:40+`).

Fix:

- In `key_code` (`D/windows.rs:538`), map letters to `enigo::Key::{A..Z}` and
  digits to `Num0..Num9`. Virtual-key codes are what shortcuts use; text
  still goes through `Input::Text`.
- Add a unit test for the mapping.
- Add a WORKPLAN §4.2 Stage 3 item 6 check: with a Russian host layout,
  Ctrl+C and Win+E still work.

### M4. UI bulk input overflows `INPUT_QUEUE = 32`, which revokes control

Evidence:

- **The viewer revokes on overflow.** `Viewer::input` pushes a revoke barrier
  and returns `Congestion` when `inputs.len() + n > INPUT_QUEUE` (perf §6.4
  step 4, `INPUT_QUEUE = 32`).
- **Several UI paths overflow it.**
  - `backspace(≤ 64)` becomes 128 inputs, split into 8 sequences pushed
    synchronously (UI §10.4, §10.5.2).
  - With a latch active, `typeText` sends one sequence per character.
  - Moves are coalesced only to one per 4 ms, so a stalled uplink queues 32
    moves in 128 ms.
- **The write task cannot keep up.** It writes one record and one flush per
  `Input`, so a synchronous Dart loop overflows it before it drains. An IME
  "delete word or selection", or a latched paste, therefore silently ends
  control.

Fix (choose one):

- (a) Coalesce consecutive `Move`s inside `Outbound` (one latest-move slot),
  so pointer motion can never trip congestion.
- (b) `Viewer::input` returns a non-revoking `Busy` when a whole batch does
  not fit. `RemoteInput` keeps a small local FIFO and resubmits on the next
  frame. Ending control stays reserved for genuine overflow.
- (c) Raise `INPUT_QUEUE` to 256, with a byte bound.

Add a T5 case, "a 64-character IME deletion keeps control", and amend perf
§6.4, UI §10.4 and the constants table.

### M5. The R-UI-3 hold does not state whether held input is re-authorised

Where: WORKPLAN §1.1, the "C rule"; CONTRACT-ui R-UI-3.

Evidence: the rule moves an "admitted" `drag_start` press, and every later
lane command, into a C-local `held` queue for up to 600 ms. Three things are
left open:

- It does not say that held commands go through `execute` / `Engine::input`
  (sequence, `controlling`, `Streaming`, `auth.admits`, `input_in_context`)
  **when released**.
- Its drop list (Release, EndControl, Control{false}, policy, generation,
  cancel) omits `auth.take_stale()` and "controlling && !fresh", the two
  releases at perf §5.12.3 steps 4-5.
- The only test, `drag_start_press_waits_out_the_double_click_time`, covers
  timing only.

Fix:

- State that the hold only delays and that `held` entries carry no
  authorisation: each one runs the full `execute` path at release time.
- Add stale presentation and loss of freshness to the drop list.
- Extend the test: a revoke, policy change or stale `Presented` during the
  hold injects nothing, and a down without the flag is never held.

### M6. Windows thumbnails starve while video flows

Where: perf §8.2 `Run()`; §9.6.5.

Evidence:

- **Snapshots are serviced only when idle.** The runner loop calls
  `ServiceSnapshot()` only on `PB_VIDEO_TIMEOUT` and `PB_VIDEO_PAUSED`.
- **That never happens while video flows.** With steady video, `next_v1`
  returns `UNIT` every frame and never times out.
- **The disconnect thumbnail always misses.** `disconnect({snapshotPath})`
  snapshots *before* disconnecting, while streaming, so it always hits the
  300 ms bound. As a result, no thumbnail is written on a normal Windows
  disconnect. `back()` works only because the pause yields `PAUSED`.

Fix: check an atomic `snapshot_requested` flag after every `Accept()` as well,
since the snapshot copies `latest_` on the decoder thread. Add a G2 smoke item:
"disconnect while `video_bench` animates writes a thumbnail".

### M7. The Android cached-app freezer defeats "resume without reconnect"

Evidence:

- **The test phone freezes background apps.** It is a Vivo V2405DA on API 36
  (`docs/validation/desktop-input-backpressure.md:171`). Recent Android
  freezes cached (backgrounded) app processes after a short debounce, and
  OEM managers such as Vivo's are more aggressive.
- **A frozen process stops pinging.** Once frozen, the Rust write task stops
  pinging. The host closes after `HOST_READ_TIMEOUT` (3 s, perf §3), and on
  resume Dart sees `stopped{transport/timeout}` and reconnects.
- **The plan assumes the opposite.** Stage 3 E expects "no reconnect" after a
  10 s background ×5. D13 says "never trip a recovery loop". No risk table
  mentions process freezing or Doze.

Fix:

- Add a risk entry to perf §13.2.
- Measure at G2 (backgrounds of 10 s, 60 s and 5 min on the phone).
- Pick one of these:
  - (a) Accept reconnect after a freeze, and make it fast: texture kept,
    greyed picture, no input replay. Change criterion E to "picture within
    N s, no input replay, ≤ 2 IDRs".
  - (b) Run a short-lived foreground service with a notification while a
    session is paused in the background.

### M8. `DesktopStore.connect` behaviour under an in-flight attempt is unspecified

Evidence:

- **Today the store drops the call.** `connect` returns silently when `busy`
  or `_attemptRunning` (`U/state/desktop_store.dart:874-876`).
- **The FFI refuses a second connect.** It answers "desktop connection is
  already pending" / "disconnect the current desktop first"
  (`F/desktop.rs:117-131`).
- **The UI relies on supersede semantics.** `RemoteDesktop.open(c)` and
  `connectNew` disconnect and then connect "without awaiting" (UI §5.4), so
  switching computers mid-connect, or connecting while the host card is
  enabling, would leave the viewer on "Connecting" forever.
- **perf §9.5/§9.6 do not say.**

Fix: specify in perf §9.6.2 that `connect(target)`:

- cancels any in-flight attempt and retry timer;
- awaits the previous `disconnect`;
- is never gated by host `busy`.

Add a store test: "a second connect during an attempt connects the new
target".

### M9. Vault edge cases

1. **`invitation_for` failures retry forever.** `invitation_for` can fail
   when the vault is still locked, because Android's Keystore hop runs in
   `computers.load()` and a connect can come first, or when the file is
   unreadable. perf §9.6.2 retries `IO` forever. Fix: map vault failures in
   `connect{computer_id}` to a terminal code, `NOT_FOUND` or a new
   `VAULT_UNAVAILABLE`. Also have `RemoteDesktop.open` await
   `computers.load()`.
2. **`computers_remember` can store the wrong session's invitation.** It
   stores `DesktopState::current_invitation()` under the Dart-supplied `id`
   without checking that it belongs to that session. During a fast switch, a
   late `live` event could store computer B's token under A's id. Fix: pass
   the expected `service` and `fingerprint` (from `DesktopInvitationInfo` or
   the target), and reject a mismatch with `INVALID_ARGUMENT`. Add this to
   R1's test.
3. **The data key cannot actually be wiped in Dart.** The raw data key
   crosses Dart as base64 JSON, an immutable `String`, so
   `key.fillRange(0, …)` (UI §5.3) does not wipe it. Either accept this and
   say so, or let Kotlin hand the unwrapped key straight to Rust through a
   JNI function. P5 already adds `jni`.

### M10. P6 has no video producer on device before G2

Evidence:

- **`p5-video` has nothing feeding it.** It exposes the C/JNI consumer
  exports over a `VideoSink`, but nothing feeds a sink until a `Viewer`
  exists. `detached_for_test()` is Rust-only, with no FFI to push units.
- **The old app cannot drive channel v2.** The old Dart and shim cannot drive
  channel v2 (`create{instance}`/`attach`).
- **So nothing on device runs before G2.** WORKPLAN's "on-device runs need
  `p5-video`" therefore enables nothing. The first real Kotlin/Windows
  decoder run is the G2 smoke, which concentrates risk R11 and R6
  (PixelCopy).

Fix: P5 commit 1 adds a debug-only synthetic source:
`pb_mapper_desktop_video_test_source_v1(path) -> source id`, behind
`cfg(debug_assertions)` or a cargo feature. It feeds a canned Annex-B clip
into a detached sink, so P6 can run the feeder, decoder, receipts, pause and
snapshot on the phone in Wave 2. Add one Dart debug button in a scratch
entry, or drive it from the S1 spike app.

### M11. The ALPN compatibility check expects text a PR #51 build cannot show

Where: WORKPLAN §4.2 Stage 3 item 8; D1.

Evidence: a PR #51 build cannot show "update pb-mapper on both computers".
That mapping exists only in the new viewer.

- An old viewer against a new host gets rustls `AlertReceived(NoApplicationProtocol)`,
  and its own generic error, since `D/security.rs:12, 98, 134` offers only v1.
- A new viewer against an old host gets the alert and maps it to
  `VERSION_MISMATCH` (perf §4.1).

Fix: replace item 8 with:

- (a) new viewer → PR #51 host shows `viewerEndedVersion`;
- (b) PR #51 viewer → new host fails at the TLS handshake with any message,
  never mid-session.

### M12. A paused session holds the host's only slot indefinitely

Evidence:

- **Back to list keeps the session alive and paused.** "Back to list" pauses
  but keeps the session (UI §5.4, §10.8), and nothing ends a paused session.
- **The host's only slot stays taken.** The host has `Semaphore(1)` (perf
  §5.3), so every other viewer gets `HOST_BUSY` until the first user returns
  or quits.
- **The host user is misinformed.** The capsule says "Being viewed" while
  nothing is captured: `SharingIndicator` ignores `HostViewer.paused`.

Fix:

- Add a paused-idle timeout, for example 10 min, after which the viewer
  disconnects and writes its thumbnail. Put it in the perf §3 constants.
- The capsule, tray line and `ViewerPanel` show "Connected · paused" from
  `viewer.paused`; this needs new ARB keys.
- Document it in UI §4.6.

### M13. Background-host gaps

1. **Display switch and rename are undefined for the background host.** perf
   §7.2 defines `host_set_display`/`host_rename` as "restart the host…".
   Background pipe v2 (§5.16.5) adds only `EndSession`, so the UI's display
   picker and rename are undefined for the background host, which is
   reachable via `DesktopHostView.background`. Fix: specify stop background
   → update the saved trust settings → start background (existing actions),
   or disable both controls while `v.background` with a "Stop sharing first"
   hint.
2. **Double-click metrics are read from the wrong session.** WORKPLAN §1.1
   says `GetDoubleClickTime`/`SM_C?DOUBLECLK` are read "in the host process
   (same user)". For the background host, the host runs inside the SYSTEM
   service in session 0 (`D/helper/background_service.rs`, `IsWellKnownSid(…
   WinLocalSystemSid)` at `:655`), so it gets defaults, not the interactive
   user's settings. Fix: the helper worker (user session) returns the metrics
   in its `Start`/`Attach` response (IPC v2 is being redesigned anyway), or
   document the 500 ms default for background hosts.

### M14. Breaking `pb_mapper::desktop` SDK API: README, CHANGELOG and semver are unowned

Evidence:

- **The SDK re-exports the crate.** `crates/pb-mapper/src/lib.rs:67-70`
  re-exports `pb_mapper_desktop`.
- **The README documents removed APIs.** `crates/pb-mapper/README.md:3-21`
  documents `Viewer::next_frame`, `presented` and `VideoPreferences`, all of
  which perf §5.18 and §6.1 delete.
- **The crate is published.** `release-sdk.yml:205` publishes
  `pb-mapper-desktop`.
- **No package owns these.** No package owns the README, an SDK-breaking
  CHANGELOG entry or the version bump.
- **The final check would fail.** WORKPLAN §4.1's
  `rg -n "…|VideoPreferences" crates ui` matches that README.

Fix:

- P4 (Viewer API) owns `crates/pb-mapper/README.md` and an "SDK: breaking"
  CHANGELOG line describing the new `Viewer`/`VideoConsumer` flow.
- Decide the crate version bump in the release notes.
- Keep `docs/remote-desktop.md`'s SDK section in P2's docs commit.

### M15. The shim's coexistence with new ops is unspecified

Evidence: `pb_mapper_desktop_call_json` deserialises one
`#[serde(deny_unknown_fields)] enum Request` (`F/desktop.rs:193-255`).

- During G1-G2 the old Dart sends `connect{native_video, video}`, `status`,
  `control`, `release`, `keyframe`, `presented` and `input`.
- P5 commit 2 changes `connect` to `{invitation|computer_id, relay_only,
  quality, refresh_hz}` and removes the others.
- perf §7.9 says only that the shim "keeps today's ops".

Fix: specify that during G1-G2 `F/desktop_shim.rs` intercepts, before
`Request` is deserialised, both:

- the op names `status|control|release|keyframe|presented|input`;
- `connect` requests that lack `refresh_hz`, which the old Dart never sends.

Add a P5 test that one old-shape and one new-shape `connect` route
correctly.

### M16. `GetDoubleClickTime` breaks the release FFI build

Evidence:

- **The needed feature is only a dev-dependency feature.**
  `GetDoubleClickTime` needs `Win32_UI_Input_KeyboardAndMouse`, which the
  desktop crate enables only for `[target.'cfg(windows)'.dev-dependencies]`
  (`crates/pb-mapper-desktop/Cargo.toml:50`).
- **enigo does not supply it.** enigo uses `windows` 0.61, a different major
  version, so it does not unify.
- **Tests pass but release builds fail.** `cargo test` and
  `clippy --all-targets` pass, because dev features unify. But
  `cargo build -p pb-mapper-ffi --release`, which
  `scripts/build/pb-mapper-ffi-windows.ps1` runs for the G1 shim measurement,
  fails to compile.

Fix: add `Win32_UI_Input_KeyboardAndMouse` to the production `windows`
features in P3's Cargo.toml edit, next to `Win32_System_Shutdown`. Add
`cargo build -p pb-mapper-ffi --release` (non-test) to the G1 gate.

---

## Low

- **L1. v1 fingerprint wording.** perf §4.9 step 3 says
  "`SHA-256(b64url(certificate))`". It must be SHA-256 of the DER bytes
  **decoded** from the v1 `certificate` base64url field
  (`D/security.rs:79-82`). Otherwise v1 invites never pin and
  `pbd1_round_trip…`'s "same fingerprint" assertion fails. Reword.
- **L2. DPAPI reuse versus a separate entropy.** UI §5.3 asks for entropy
  `"pb-mapper remembered computers"` while "reusing `protect`". But
  `F/desktop_trust.rs:180-214` hardcodes the entropy
  `b"pb-mapper/desktop-trust/v1"` and the trust description. Either give
  `protect` an entropy and description parameter (and make `replace`
  `pub(super)` too), or accept the shared entropy and say so.
- **L3. The cherry-pickable title-bar commit cannot use `ViewerPalette`.**
  UI §12.2's viewer tone uses `ViewerPalette.*`, created in P1 commit 3.
  Commit 1 must use literals, or add the `viewer` tone in commit 3 or P9.
- **L4. Icon size rule.** The toolbar handle (§8.3) and the virtual-mouse
  scroll strip (§9.6) use 14 px chevrons. BRIEF req 3 allows 16/18/20/24 only.
  Use 16, or record the deviation in UI §1.2.
- **L5. Wrong path.** The `U/main.dart` path is wrong in UI §4.4 and
  WORKPLAN P8. The file is `ui/lib/main.dart` (`--desktop` at `:39`).
- **L6. The JVM unit test will throw.** `VideoFeeder.run()` calls
  `Process.setThreadPriority`, which throws "not mocked" in JVM unit tests.
  Set `testOptions.unitTests.isReturnDefaultValues = true` in
  `build.gradle.kts` (P6 owns its test section), or inject the priority
  call.
- **L7. Busy oracle not tested.** `handshake_reports_denied_and_busy_before_close`
  should also assert that a bad token while busy gives `Rejected{denied}`,
  not `busy`, as perf R5 claims.
- **L8. QR secrecy has no test.** Add a widget test: `host-qr` shows the
  panel, and leaving the page or `AppLifecycleState.inactive` hides it
  (BRIEF capability table, UI §6.3). Today only Stage 3 checks it by hand.
- **L9. D26 on the Relay page.** It still offers a generic tunnel Connect for
  `desktop:*` services (`U/pages/relay/services_tab.dart:181-189`). D26 says
  these services "appear in Remote desktop". Hide Connect for `isDesktop`, or
  route it to Remote.
- **L10. `fake_async` dependency.** Adding it as a direct dev dependency is
  outside D21's allow-list. Use `testWidgets` with `tester.pump(duration)`
  instead, or get an owner nod.
- **L11. P5's vault check runs too early.** P5's on-device acceptance
  ("remember → restart → listed → forget") has no Dart caller until P8 at G3.
  Move it to G3 or Stage 3 item 5.
- **L12. perf contradicts itself on the outgoing head.** §13.1 row 1 says
  "capture skips ticks (no discard, no IDR)", but §5.5 step 3 clears and
  requests an IDR when the head is older than 1 s. Reconcile the text.
- **L13. Protocol errors retry forever.** perf §9.6.2 reconnects on
  `protocol` indefinitely, every 15 s. Cap it at 3 attempts, then `ended`,
  so a deterministic parse failure does not loop.
- **L14. Prefer `GetModuleHandleExW`.** D9 says the Windows runner looks
  symbols up "from the already-loaded DLL". Use `GetModuleHandleExW` with the
  module name, which pins and never loads a second copy, instead of
  `LoadLibraryExW(…, LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`. The token check
  stays.
- **L15. Unsupported keys.** Ctrl+punctuation (Ctrl+-/=/[/], Ctrl+/),
  PrintScreen, CapsLock and the Menu key fall outside D17's vocabulary and
  are silently dropped by `KeyRouting` rule 5. Document this limitation in
  `docs/remote-desktop.md`, or add OEM VK keys.
- **L16. Unverified checks.**
  - iOS/macOS builds with `mobile_scanner` are never built in the plan: no
    Mac, and no Podfile is checked in (deps map §2.3). Mark them unverified
    in the release notes.
  - X12 verifies minimise only. Add "hide to tray" (close → hide) to the
    `hidden` check.
  - After the Wave 0 merge of namespaces #52, re-check that
    `ownNamespace = keyId` (`U/state/app_store.dart:115`) and the trust
    `Scope.namespace = key_id` (`F/desktop.rs:595`) still mean what
    `RemoteComputer.namespace` and `ComputerPresence.otherRelay` assume.

---

## Checked and consistent (no action)

These cross-document points were reconciled correctly:

- All BRIEF requirement rows and D1-D28 map to a contract section.
- WORKPLAN §1 amends R-UI-1/3/9 coherently.
- Every Lucide identifier in UI §11.1 that I checked (all the ones
  referenced) exists in `lucide_icons_flutter` 3.1.25, including the 300
  "Large" variants and the `…400Dir` ones. So do the w400 and w300 TTF
  assets.
- The undrawn screens in the design-spec map's "open decisions" list each
  have a proposal in UI §6-§9.
- `valid_service` allows 128 bytes, matching the pbd1 suffix limit of 120.
- `jni` 0.21.1 and `bytes` 1.11 are already locked.
- The handle lifetime makes UI-isolate sync calls safe: `destroy` runs on the
  same isolate (`pb_mapper_service.dart:454-461`).
- `Registration::diagnostics().direct.active_direct` exists.
- `NOT_FOUND` and `PROTOCOL` error codes exist (`F/error.rs:23-50`).
- CI is Linux-only, as WORKPLAN states.
