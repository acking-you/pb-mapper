# WORKPLAN: remote desktop overhaul (integrated plan)

Status: the integration plan over `CONTRACT-perf.md` (perf) and
`CONTRACT-ui.md` (ui), under `BRIEF.md` and `DECISIONS.md`. It does not
restate the contracts. Each package session reads its own entry here plus the
contract sections the entry names. Where this plan amends a contract, the
amendment is marked **AMEND** and takes precedence for that point only.

Repo: `<repo>`.
Integration branch: `claude/remote-desktop-overhaul`.

Path shorthands follow the contracts: `D/` = `crates/pb-mapper-desktop/src/`,
`F/` = `ui/native/pb_mapper_ffi/src/`, `U/` = `ui/lib/src/`,
`R/` = `U/pages/remote/`, `V/` = `U/pages/remote/viewer/`, `T/` = `ui/test/`,
`K/` = `ui/android/app/src/main/kotlin/com/pb_mapper/ui/`,
`W/` = `ui/windows/runner/`.

Command conventions:

- Rust runs from the repo root.
- Flutter runs in PowerShell from `ui/`, with
  `$flutter = '<flutter 3.44.9>\bin\flutter.bat'`
  and then `& $flutter …`.
- Gradle: `ui/android/gradlew.bat` is git-ignored and is created by the first
  `flutter build apk`. Run unit tests from `ui/android` as
  `.\gradlew.bat :app:testDebugUnitTest`.
- CI (`.github/workflows/syntax-check.yml`) runs on **Linux only**: `cargo fmt
  --check`, `clippy --workspace --all-targets --all-features -D warnings`,
  `cargo test --workspace --all-features`, `cargo doc`, `flutter analyze
  --no-fatal-infos` and `flutter test`. Windows-only Rust (capture, encoder,
  helper, DPAPI) is checked **only locally**, so every Rust package also runs
  its checks on this Windows machine.

---

## 0. The plan in one page

- **9 packages.** P1 UI chrome, strings and finish. P2 protocol foundation and
  host session. P3 capture worker, encoder and helper. P4 viewer core. P5 FFI
  surface and vault backend. P6 native video consumers. P7 Dart pipeline and
  `DesktopStore`. P8 remote desktop state, shell and page. P9 viewer UI.
- **Gates.**
  - G0: P2's first commit (protocol foundation) lands. It also freezes the
    host status types and `Host` signatures (perf §5.15) that P3 and P5 build
    on. It unblocks P3, P4 and P5.
  - G1: the Rust core integrates with P5's shim, and P4 rewrites the
    end-to-end tests on the merged core. The old app runs on it, and the user
    measures case P0.
  - G2: the native pipelines and the new Dart store integrate, and the shim
    is deleted.
  - G3: the UI packages integrate, P1 finishes, and the user runs the final
    on-device acceptance.
- **Milestone tags** let packages build on each other before integration:
  `ovh-g0`, `p4-vapi`, `p5-video`, `p7-upload`, `p7-d1`, `p7-tool`,
  `p8-shell`, `ovh-g1`, `ovh-g2`. A tagged commit is never rewritten.
  Packages consume tags with `git merge`, never with cherry-pick, so
  integration de-duplicates the commits.
- **Owner decisions** that a package waits on are listed in section 1.3,
  each with the default the package builds if no answer has come.
- **Single owners** for the hotspots:
  - P1 owns the ARB files, `pubspec`, the plugin registrants, `app_icons.dart`
    and the posters;
  - P2 owns `D/lib.rs`;
  - P5 owns `Cargo.toml`/`Cargo.lock` additions and `MainActivity.kt`;
  - P7 owns `fake_desktop_api.dart`, the acceptance tool and
    `scripts/test/desktop-{bootstrap.cjs,ui-acceptance.md}`;
  - P4 owns `crates/pb-mapper/README.md` (the published SDK's description).
- **The lead resolves every merge** (section 3.4).
- The merge-block statements in docs stay until the user accepts fluidity on
  a real device (D28).

---

## 1. Reconciling the UI contract's requests (ui §2.2) into the perf surface

All ten requests are accepted. Three are amended: R-UI-1 needs FFI additions
that it did not name, R-UI-3's host rule needed a bound, a testable seam and
an explicit re-authorisation rule, and R-UI-9's field names clash with
interface methods. Section 1.2 adds reconciliations that neither contract
made. Section 1.3 lists the questions only the owner can answer.

### 1.1 Decisions and final signatures

| Request | Decision | Owner |
| --- | --- | --- |
| R-UI-1 host `loaded`, `configured` | Accepted. **AMEND:** the invitation needs FFI data that does not exist today. `trust_status` returns `saved` metadata **without** an invitation (`F/desktop_trust.rs:104-109`), and `HostStatus` carries none either, so while sharing the old store's cached string (`U/state/desktop_store.dart:63, 210-329`) would have no replacement. `host_status` therefore returns the running host's `service` and `invitation` beside its `HostStatus` (perf §7.2), and `configured` requires a trust saved for **this** relay and namespace (perf §9.4). | P5 (FFI fields), P7 (Dart) |
| R-UI-2 stable `since` | Accepted, simplified: `HostViewer.started_unix_ms` is non-optional (perf 5.15), so there is no "first sight" fallback | P7 |
| R-UI-3 drag-start double-click guard | Accepted. **AMEND:** the hold is enforced on thread C **before** the backend call, so the helper needs no rule. The OS values sit behind a `Backend` seam; `RemoteBackend` gets them from the helper worker's `Start`/`Attach` response, the one IPC addition (perf §5.16.3). Held moves coalesce, so a 600 ms hold cannot overflow the 512-slot lane. The hold only delays: held commands are re-authorised when released. | P2 (types at G0), P3 (rule, IPC field), P7 (Dart) |
| R-UI-4 typed notice | Accepted | P7 |
| R-UI-5 connect by computer id | Accepted. The invitation wins when both fields are set, because a just-added computer is not yet in the vault. | P5, P7 |
| R-UI-6 vault hooks | Accepted. No stub is needed, because P5 owns both the hooks and `F/desktop_computers.rs`. | P5 |
| R-UI-7 retry semantics | Accepted | P7 |
| R-UI-8 setup step kept on terminal failure | Accepted | P7 |
| R-UI-9 `FakeDesktopApi` scripting | Accepted. **AMEND:** fields named `viewerStatus` and `view` would clash with the interface methods `viewerStatus()` and `view()`. They are renamed. | P7 |
| R-UI-10 local controls bypass `busy` | Accepted, for `endViewerSession()` and `allowControl(false)` only. `allowControl(true)` stays behind `busy`. | P7 |

**Dart (P7, all in the `p7-d1` milestone):**

```dart
// U/state/desktop_models.dart
class DesktopHostView {                       // perf §9.4 fields plus:
  final bool loaded;      // false until the first capabilities() + hostStatus() + trustStatus() round trip
  final bool configured;  // (trust.saved != null && trust.matchesRelay) || background?.configured == true
  // invitation, first match (perf §9.4; no separate invitation cache in the store):
  //   hostStatus().host?.invitation                                   // a local host runs
  //   ?? (background?.configured == true ? background!.invitation : null)
  //   ?? (trust.matchesRelay ? trust.saved?.invitation : null)
  // Invariant: sharing || configured ⇒ invitation != null. hostStatus() (and trustStatus() where
  // listed below) is re-read before enabling/restarting clear, so no published value breaks it.
  // trustStatus() is read on restoreOnStartup and after enable/stop/revoke/rename/setDisplay only, never per poll.
}
// DesktopHostStatus (from hostStatus().host) gains `service` and `invitation` (the FFI siblings, perf §7.2).
class DesktopHostViewer {
  final bool controlling, paused; final String path; final DesktopQuality quality;
  final DateTime since;   // DateTime.fromMillisecondsSinceEpoch(started_unix_ms); replaces connectedFor
}
enum DesktopNoticeCode { locked, lockFailed }
@immutable class DesktopNotice { final DesktopNoticeCode code; final String message; }   // ==, hashCode
// DesktopSessionView.notice: DesktopNotice?  (from ViewerStatus.notice while age_ms < 5000)
class DesktopTarget {
  final String? invitation;      // null when computerId names a remembered computer
  final String name; final String? computerId; final bool relayOnly; final DesktopQuality quality;
  // assert(invitation != null || computerId != null); toString() redacts invitation
}
// DesktopSessionView.reconnectAttempt: > 0 from the first scheduled retry until the next live
//   (including while that retry's connect runs); reset on live and on connect(target).
// DesktopSessionView.failure?.code == 'HOST_BUSY' while a retry after HOST_BUSY is scheduled.
// DesktopSessionView.setup keeps the step that was current when a first connect ends terminally.

// U/ffi/desktop_api.dart
Future<DesktopSession> connect(String? invitation, {String? computerId, required bool relayOnly,
    required DesktopQuality quality, required double refreshHz});
// DesktopApi sends exactly one of 'invitation' | 'computer_id' (the invitation when both are non-null).

// U/state/desktop_input.dart
factory DesktopInput.button(RemoteButton button, bool down, {bool dragStart = false});
// JSON {"kind":"button","button":"left","down":true,"drag_start":true}; the key is omitted when false.

// DesktopStore: endViewerSession() and allowControl(false) never wait for busy (as stopHost()).

// T/fake_desktop_api.dart
class FakeDesktopApi implements DesktopApiClient {
  final List<(String op, Map<String, Object?> args)> calls;   // every lifecycle and hot op, by FFI op name
  DesktopHostSnapshot hostSnapshot;                           // returned by hostStatus()
  DesktopViewerStatus? viewerStatusValue;                     // returned by viewerStatus()
  DesktopView? viewValue;                                     // returned by view()
  void failNext(String op, String code);                      // the next call of op throws DesktopCallException(code)
  void emitJpeg(Uint8List encoded, {required int generation, required int frameId});   // previews: PNG bytes decode too
  // inspectInvitation: 'pbd1.<seg>…' -> service 'desktop:'+<seg up to first non-[a-z0-9-]>,
  //   fingerprint 'AB12-CD34-EF56-7890', format pbd1; 'pbmt1_…' or base64 setup JSON -> throws SETUP_STRING;
  //   anything else -> throws INVALID_INVITATION.
}
```

**Rust (P2 at G0 for the types and trait, P3 for the rule):**

```rust
// D/lib.rs (perf §4.5); Input stays deny_unknown_fields
Button { button: Button, down: bool,
         #[serde(default, skip_serializing_if = "std::ops::Not::not")] drag_start: bool },

// D/lib.rs, Backend trait (perf §5.10.3), additive default:
fn double_click_guard(&mut self) -> Option<DoubleClickGuard> { None }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]                 // it crosses the helper pipe (perf §5.16.3)
pub struct DoubleClickGuard { pub time_ms: u32, pub half_box: (u32, u32) }
// Values: GetDoubleClickTime(), GetSystemMetrics(SM_CXDOUBLECLK)/2, GetSystemMetrics(SM_CYDOUBLECLK)/2,
// always read in the interactive session:
//   Desktop (ordinary host)  -> reads them in its own process, which runs in the user's session;
//   RemoteBackend (helper)   -> returns the value from the helper worker's Start/Attach response.
//     The background host runs inside the SYSTEM service in session 0, where these are defaults.
// GetDoubleClickTime needs the production `windows` feature Win32_UI_Input_KeyboardAndMouse (P3, perf §5.10.3).
```

The C rule (P3, `D/worker.rs`; it amends ui §2.2 R-UI-3):

- C records `last_left_down: Option<(Instant, (u32, u32))>`. The position is
  the last **injected** `Move` in input space, not the OS cursor, so a fake
  backend can test it.
- When an admitted `Button{left, down: true, drag_start: true}` meets
  `now − t < guard.time` within `half_box` of the recorded position, C moves
  that command and every later lane command into a C-local `held` queue until
  `t + guard.time + 20 ms`, capped at `DRAG_GUARD_MAX = 600 ms`.
  - In `held`, consecutive `Move`s coalesce to the newest one.
  - At most `LANE_CAPACITY` non-move commands can be held. An overflow takes
    the existing `InputCongestion` path.
- **The hold only delays; it authorises nothing.** `held` entries carry no
  authorisation of their own. When the hold ends, each one runs the full
  `execute` path at that moment (perf §5.12.3: sequence, `controlling`,
  `Streaming`, `auth.admits`, `input_in_context`), exactly as if it had just
  left the lane.
- These drop the **whole** held queue, because nothing in it was injected
  yet: `Release`, `EndControl`, `Control{false}`, a policy change, a
  generation change, a cancel, `auth.take_stale()` (a stale presentation)
  and `controlling && !auth.fresh(now)` (the two releases of perf §5.12.3
  steps 4-5).
- The hold is excluded from `input_wait`. Capture continues.
- When `double_click_guard()` returns `None` (every non-Windows backend), the
  flag is ignored.
- The helper (`Request::Input`) carries the field through serde and ignores
  it.
- Test: `worker::drag_start_press_waits_out_the_double_click_time`, with a
  fake backend whose guard is 500 ms:
  - a click at t0 and a `drag_start` down at t0 + 200 ms → the down is
    injected at ≥ t0 + 520 ms, and a move queued after it is injected after
    it;
  - a down without the flag is never held;
  - a revoke, a policy change, or a stale `Presented` during the hold →
    nothing from the held queue is injected.

**FFI (P5):**

| Op / item | Final shape |
| --- | --- |
| `connect` | Exactly one of `invitation: string` or `computer_id: string`; anything else → `INVALID_ARGUMENT`. Plus `relay_only`, `quality`, `refresh_hz`. `computer_id` resolves through `desktop_computers::invitation_for(handle, id) -> Result<Option<Invitation>, CtlError>`. `None` → `NOT_FOUND` "remembered computer not found". |
| `trust_status` | `saved` gains `invitation: string \| null`: the pbd1 of the saved identity (`HostIdentity::invitation(service)`), or null when `matches_relay` is false. **New (for R-UI-1).** |
| `host_status` | `host` is `{status: HostStatus, service, invitation}` or null (perf §7.2). **New (for R-UI-1).** `host_set_display` / `host_rename` refuse with `INVALID_ARGUMENT` while background hosting is configured (perf §5.16.5). |
| `connect{computer_id}` failures | A locked or unreadable vault → `VAULT_UNAVAILABLE` (new `ErrorCode`, perf §7.8); unknown id → `NOT_FOUND`. Both terminal (perf §9.6.2). |
| `computers_remember` | Also takes `service` and `fingerprint`; refuses with `INVALID_ARGUMENT` unless they match `current_invitation()` (UI §5.3), so a late `live` during a fast switch cannot store computer B's token under A's id. |
| `pb_mapper_desktop_call_json` | An op name starting with `computers_` is routed **before** `Request` is deserialised, to `desktop_computers::call(handle, op, value).await` under the same `block_on` |
| `DesktopState` | `pub(crate) fn current_invitation(&self) -> Option<Invitation>`, set when `connect_viewer` succeeds (either connect form) and cleared by `disconnect_viewer` |
| `F/lib.rs` | `mod desktop_computers;` |

### 1.2 Reconciliations neither contract made

| # | Issue (evidence) | Resolution |
| --- | --- | --- |
| X1 | Vault ownership. ui §17.1 splits `U/ffi/remote_computer_vault.dart` between UI-2a (interface) and UI-2b (FFI class), which is a cross-owner Dart edit. | **P8 owns the whole Dart vault file**, `FfiRemoteComputerVault` included. That class targets the frozen `computers_*` ops and the `pb_mapper/keystore` channel. When the ops are missing, `open()` returns unavailable. P5 owns the Rust vault and `K/ComputerKeystore.kt`. |
| X2 | `MainActivity.kt`. perf §8.1.3 says it is unchanged, but ui §5.3 adds a keystore registration line. | P5 is the only owner and adds the one line. P6 does not edit it. |
| X3 | `D/viewer.rs` and `D/viewer/` cannot coexist. Once P4's V-API replaces the module, `F/desktop.rs` stops compiling on P4's branch. | Accepted. P4 checks only `-p pb-mapper-desktop`. At G1 the lead merges P4 and P5 in one sitting and runs the gate after the last merge. P4 may edit only the `pub use viewer::…` line(s) of `D/lib.rs`. |
| X4 | perf §12.3 G1 deletes files that span packages: `congestion.rs`, `media.rs`, the old `wire::Command` and framing, the dead-code allows. | P2 authors one cleanup commit on the integration branch right after the G1 merges. Each package deletes only code it alone replaced: the old `Worker` (P3), `D/viewer.rs` (P4), the host stall/probe code (P2). |
| X5 | `D/pacer.rs` would force P3 to edit P2's `D/lib.rs` | G0 pre-declares `mod pacer;` with an empty `D/pacer.rs`. P3 fills it in. |
| X6 | The perf 10.4 Dart tests used `fake_async`, which is only transitive (`ui/pubspec.lock:84`); a direct dev dependency is outside D21's allow-list | Not added. The store tests use `testWidgets` and `tester.pump(duration)`, whose fake clock drives the timers (perf §10.4). P7 therefore no longer depends on P1. |
| X7 | `video_sink::single_consumer_need_capacity_keeps_unit_and_close_wakes` tests exactly the WP-0 skeleton, and P5 starts on that skeleton | The test moves to P2's G0 commit |
| X8 | `origin/master` (`badcad7`, namespaces) is not an ancestor. Both sides edit `crates/pb-mapper-client/src/direct/mod.rs` and `sdk/client.rs`. | Wave 0: the lead merges `origin/master` into the integration branch first (owner approval: it changes PR #51's history shape) |
| X9 | The working tree shows the seven generated plugin registrants as modified, but `git diff --stat` is empty: line endings only | Wave 0: the lead restores them. P1 regenerates them once, with `mobile_scanner`. |
| X10 | A missing ARB string found late by P8/P9 | Such a package may add keys in one commit that touches only `app_en.arb`/`app_zh.arb`, appended at the **end of its own block** (P8: the remote/add/host blocks; P9: the viewer/quality/shortcut/touch/gesture blocks). The generated `app_localizations*.dart` files are never hand-merged (section 3.4). |
| X11 | `DesktopTarget.invitation` is visible on `session.value` | Widgets never read it. `toString()` redacts it, and no log prints it (P7). |
| X12 | Windows minimise, or hide-to-tray (the close button calls `windowManager.close()`), may not report `hidden` (perf R8) | P7 checks both on Windows during G2. If either fails, P6 adds the runner `SIZE_MINIMIZED`/hide → `desktop_video` `visibility` event and P7 consumes it. That is a declared, sequential edit of `W/flutter_window.cpp` and `desktop_video.dart`. |
| X13 | No package branch has both v2 ends before G1: P2's branch runs the v2 host against the v1 viewer, P4's the v2 viewer against the v1 host, and P4's end-to-end rewrite needs P2's session and P3's worker. `D/slow_link.rs` is a `#[cfg(test)]` module of the lib (`D/lib.rs:16-17`), so it must compile everywhere. | G0 marks every v1 viewer↔host test `#[ignore]` (perf §12.1 WP-0) after running it once. P4 keeps those files compiling against its API (still ignored) in commits 1-2. **P4 commit 3 is authored after the G1 merges**, on top of them, and un-ignores the rewritten cases before the G1 gate runs (section 3.1). |
| X14 | perf §9.8 deletes store members that the old pages, the acceptance tool and `T/desktop_viewer_navigation_test.dart` use, and G2's on-device smoke runs on the old pages because the new ones land at G3 | P7's `p7-d1` ports the old pages and the tool's compile, and deletes the navigation test, so `flutter analyze` and the full `flutter test` are green at every P7 tag (perf §9.8) |
| X15 | Release APKs are not debuggable, so `run-as` cannot read the acceptance report, and the old tool insists on a `reportPath` define | The tool uploads the report to the loopback bootstrap (`PUT /report`). P7 lands this first in the old schema-1 tool as `p7-upload`, which G1 merges for Stage 1 (perf §11.2). |

No request was rejected. None of them weakens a security boundary:

- `drag_start` only ever delays injection;
- remembered computers are connected by id, so tokens never return to Dart;
- the invitation that `trust_status` and `host_status` now expose is the same
  secret the sharing host card already holds.

### 1.3 Owner decisions to obtain (the DECISIONS preamble requires escalation)

The lead asks the owner before the named package starts and records the
answer in DECISIONS. If no answer has come, the package builds the default,
which stays within the decisions in force and is easy to change.

| Id | Question | Default | Needed before |
| --- | --- | --- | --- |
| OD-1 | D15 asks for an "always-on-top" bar, but the capsule and fallback layer live inside the app window, which a sharing host normally keeps hidden in the tray; the tray icon has no viewed state, and a background host can share with the app closed (UI §4.6). How should a hidden or minimised host learn it is being watched? | (a) the tray shows `tray_active` and a tooltip that leads with "Someone is viewing/controlling this computer"; (b) on a viewer's arrival while the window is hidden or minimised, `windowManager.show(inactive: true)` brings the window and its capsule back without focus. Not built: (c) always-on-top while viewed. Background hosting with the app closed has no indicator, and the docs say so. `tray_manager` 0.5.1 has no balloon API, so a tray notification would need a package outside D21. | P8 commit 2 |
| OD-2 | Android freezes the backgrounded app (perf R13), so "resume without reconnect" (D13, case E) cannot hold on the test phone after a freeze | Accept one immediate reconnect after a freeze (greyed last picture, no input replay) and the split criterion E (perf §11.4). The alternative is a foreground service with a notification while a session is paused in the background. | The G2 measurement (Stage 2) |
| OD-3 | `pb_mapper::desktop` changes incompatibly (`Viewer`, `VideoConsumer`, no `VideoPreferences`), and `release-sdk.yml:205` publishes `pb-mapper-desktop` | The release notes carry "SDK: breaking", and the workspace version moves to the next minor (pre-1.0 semver) at release | Before any release from this branch |
| OD-4 | Merging `origin/master` (namespaces #52) into the branch changes PR #51's history shape (X8) | Merge it in Wave 0 | Wave 0 |

---

## 2. Work packages

Every package:

- works in its own worktree under
  `<main checkout>\.claude\worktrees\ovh-pN` (branch `ovh/pN-…`, created
  by the lead);
- lands its commits with their tests (D27);
- keeps `unwrap`/`expect` out of production Rust;
- ends with `cargo fmt --all -- --check` and, for Dart, `& $flutter analyze
  --no-fatal-infos` clean.

A file listed under "Owns" is edited only by that package during its phase.
"Reads" lists files it must not modify. A package that finds a contract rule
unworkable stops and reports evidence.

### P1 UI chrome, strings and finish (ui UI-0a, UI-0b, UI-1, UI-8; release notes)

- **Goal.**
  - Early: the title-bar fix and the icon system, as two commits that can be
    cherry-picked to master, then every new string, the shared primitives and
    the new dependencies.
  - Late: delete the PR #51 strings, regenerate the previews and posters, and
    write the UI docs and the release notes.
- **Owns.**
  - Early phase:
    - `U/shell/window_chrome.dart` (commit 1 with `CaptionTone.surface`
      only, so it cherry-picks without `ViewerPalette`; commit 3 adds
      `CaptionTone.viewer` with the palette, ui §12.2);
    - `U/shell/page_frame.dart` (padding, gap, `CaptionButtons()` only);
    - `U/pages/setup_flow.dart` (caption placement only);
    - `T/title_bar_test.dart` (new);
    - `U/theme/app_icons.dart` (the full ui §11.1 list, including every
      remote glyph);
    - `U/theme/app_theme.dart`;
    - `U/shell/navigation.dart` (rail icon size only);
    - `U/galaxy/galaxy_panel.dart` and the ui §11.2 audit files (icon sizes
      only);
    - `U/widgets/surfaces.dart`, `U/widgets/status.dart`, `U/theme/glass.dart`
      (`GlassSegment.enabled`);
    - `U/widgets/dashed_border.dart`, `U/theme/viewer_palette.dart`,
      `U/state/viewer_prefs.dart` (all new);
    - `T/support.dart` (fonts), `T/gen_poster_test.dart`;
    - `ui/pubspec.yaml` and `pubspec.lock` (− `cupertino_icons`;
      + `qr: 3.0.3`, + `mobile_scanner: 7.5.0`; no `fake_async`, X6);
    - the generated plugin registrants (`ui/{windows,linux}/flutter/*`,
      `ui/macos/Flutter/GeneratedPluginRegistrant.swift`);
    - `ui/lib/l10n/app_en.arb`, `app_zh.arb`, the generated
      `app_localizations*.dart` and `l10n_untranslated.json` (every ui §13.2
      key);
    - `ui/android/app/src/main/AndroidManifest.xml` (backup attributes) and
      `res/xml/{data_extraction_rules,backup_rules}.xml` (new), excluding
      **both** `domain="root" path="app_flutter/"` (the Rust config dir:
      credential, tunnels, vault, trust, logs) and `domain="file" path="."`
      (thumbnails), under `<cloud-backup>` and `<device-transfer>` (ui §5.3);
    - `ui/ios/Runner/Info.plist` (`NSCameraUsageDescription`) and the
      `en.lproj`/`zh-Hans.lproj` `InfoPlist.strings`;
    - `docs/assets/poster*.png`, `docs/ui-design.md`.
  - Finish phase:
    - ARB deletions (all `desktop*` keys);
    - `T/preview_test.dart`, `T/remote_screen_fixture.dart` (new);
    - posters;
    - `docs/ui-design.md`;
    - the UI sections of `docs/remote-desktop.md` (ui §16, including the keys
      that are not sent and the hidden-window behaviour of OD-1);
    - the UI module part of `CLAUDE.md`;
    - `CHANGELOG.md` (gestures, Logs location, remembered computers, ALPN v2
      "update pb-mapper on both computers", the helper update, Android backup
      no longer restoring tunnels, the credential or remembered computers,
      "SDK: breaking" for `pb_mapper::desktop`, iOS/macOS builds with
      `mobile_scanner` unverified; P2, P3 and P4 supply their text);
    - `ui/README.md`;
    - `.github/workflows/syntax-check.yml` (`! grep -rnE '\bIcons\.' ui/lib`).
- **Reads.** Everything else. In the finish phase, P8 and P9's widgets for the
  previews.
- **Depends on.**
  - Early: nothing.
  - Finish: P8 and P9 merged (G3); P2's perf docs commit already on the
    branch (`docs/remote-desktop.md` order).
- **Stubs and fakes.** None early. Previews use `FakeDesktopApi.emitJpeg` with
  the `RemoteScreenFixture` PNG and `InviteScanner.debugFakeCamera`.
- **Acceptance (early).**
  - `& $flutter test test/title_bar_test.dart test/app_test.dart`.
  - Full `& $flutter test` once after the icon commit.
  - `$env:PB_MAPPER_REGEN_POSTER='1'; & $flutter test test/gen_poster_test.dart`
    (posters regenerated and reviewed).
  - `& $flutter gen-l10n`, with `lib/l10n/l10n_untranslated.json` empty.
  - `& $flutter build apk --debug`. Bump the Kotlin plugin only if the build
    fails (D21). Check that the merged manifest contains `CAMERA`, from
    mobile_scanner.
  - By hand on Windows at 100/125/150 %, maximised and restored: the hovered
    close button stays ≥ 16 px inside the sheet edge.
  - **Cherry-pick check:**
    1. `git worktree add ..\ovh-master-check origin/master`;
    2. `git -C ..\ovh-master-check cherry-pick <title-bar> <icons>` (if
       `pubspec.lock` conflicts, run `flutter pub get` there);
    3. `flutter analyze` and `flutter test test/title_bar_test.dart` in that
       worktree;
    4. remove the worktree.
- **Acceptance (finish).**
  - Full `& $flutter test` once.
  - `(Select-String lib/l10n/app_en.arb -Pattern '"desktop').Count` is 0.
  - `rg -n "\bIcons\." lib` finds nothing.
  - `$env:PB_MAPPER_UI_PREVIEW_DIR=…; & $flutter test test/preview_test.dart`,
    with the ui §15 list reviewed.
- **Commits.**
  1. "Give caption buttons room and self-drawn glyphs"
  2. "Use weight-400 Lucide icons at four fixed sizes"
  3. "Add remote desktop strings, viewer palette and scanner dependencies"
  4. "Remove PR #51 desktop strings and regenerate previews and posters"
  5. "Document the remote desktop redesign and its gesture changes"

### P2 Protocol v2 foundation and host session (perf WP-0, WP-H; G1 cleanup; perf docs)

- **Goal.**
  - G0: perf §4 (wire v2, ALPN `pb-mapper/desktop/2`, pbd1, pinning) and the
    shared host types of perf §5.1, 5.6, 5.11 and 5.12, complete with tests.
    This includes:
    - the `Backend` trait, with `double_click_guard` and `DoubleClickGuard`;
    - `drag_start` (R-UI-3);
    - `key_code` on virtual keys for letters and digits (perf §4.5), never
      `Key::Unicode`;
    - the `VideoSink` FIFO skeleton;
    - `HostStatus`, `HostViewer`, `HostState`, `HostPath` and the final `Host`
      signatures `status() -> HostStatus` and `end_session()`, with adapter
      bodies over the v1 host, plus `legacy_host_status` for the old FFI
      `status` op (perf §5.15; P3 and P5 build on them before commit 3);
    - the stub `worker::spawn_session`;
    - `mod pacer;`;
    - the exact mechanical-edit set of perf §12.1 WP-0, and nothing else
      outside its own files. `VideoPreferences` and `SessionStatus` stay
      until the cleanup commit;
    - `#[ignore]` on every v1 viewer↔host test (X13).
  - Then the host network side:
    - B-recv and B-send;
    - receipt credit;
    - probe and QoS;
    - the handshake;
    - the Host API (D15, D24).
  - After G1: the cross-package cleanup and the performance docs.
- **Owns.**
  - `D/lib.rs`, `D/protocol.rs`, `D/invite.rs`, `D/shared.rs`, `D/auth.rs`,
    `D/lane.rs`, `D/outgoing.rs`, `D/idr.rs`, `D/video_sink.rs` (skeleton at
    G0), `D/pacer.rs` (empty at G0) (all new except `lib.rs`);
  - `D/wire.rs`, `D/security.rs`;
  - `D/windows.rs` (G0: `key_code` and the `impl Backend for Desktop`
    signature adapters only);
  - `D/host.rs` (G0: the 5.15 signatures and adapters; then the host
    session);
  - `crates/pb-mapper-desktop/Cargo.toml` (`bytes`, G0);
  - G0 mechanical edits only, exactly the perf §12.1 WP-0 list:
    - every `impl Backend`: `D/windows.rs:192`, `D/helper/client.rs:101`,
      `D/host.rs:347`, `D/slow_link.rs:101, 295`, `D/tests.rs:44`,
      `D/worker.rs:635, 747, 918, 1033, 1245` and the worker's call sites,
      `D/helper/worker.rs`, `tests/session.rs:26, 212`,
      `examples/input_acceptance.rs:206`, `examples/video_bench.rs:67, 153`,
      `F/desktop.rs:1706`;
    - the `Invitation` change in `D/viewer.rs` and `F/desktop.rs`;
    - the `Host::status()` callers `D/helper/background_service.rs:355, 806`,
      `examples/deployed.rs:84`, `examples/input_acceptance.rs:531` and the
      FFI `status` op (`F/desktop.rs:622`);
    - `#[ignore]` on the v1 viewer↔host tests in `tests/session.rs` and
      `D/tests.rs` (X13);
  - `D/tests.rs` (G0: the pin test rewrite and the
    `older_authentication…` deletion);
  - then `D/session.rs`, `D/credit.rs`, `D/qos.rs` (new) and `D/host.rs`;
  - after G1: the cleanup commit (`D/congestion.rs`, `D/media.rs`, the old
    `wire::Command` and framing, `VideoPreferences`, `SessionStatus`, the
    dead-code allows) and the performance sections of
    `docs/remote-desktop.md` (perf §12.1 WP-M list, including the SDK
    section; P3's bench numbers).
- **Reads.** `D/worker.rs` and `D/viewer/` after G0. `F/*`.
- **Depends on.**
  - G0: nothing.
  - Host session: G0. It works against the `spawn_session` stub until P3
    lands (I4).
  - Cleanup: the G1 merges.
- **Stubs and fakes.**
  - A scripted viewer over `tokio::io::duplex` using `protocol.rs`.
  - `HostUnit`s pushed into `Outgoing` directly.
  - The G0 worker stub publishes `EngineState{connecting}`.
- **Acceptance.**
  - G0:
    - `cargo test -p pb-mapper-desktop -- --include-ignored --skip slow_link`,
      all green, so the v1 end-to-end cases pass once before they stay
      ignored (X13). It must include:
      - `strict_parsing_and_version_gate` (with a `drag_start` round trip);
      - `pbd1_round_trip_rejects_tamper_and_setup_strings` (a v1 invite pins
        the SHA-256 of its **decoded** certificate DER, perf §4.9);
      - `fingerprint_pin_rejects_another_certificate`;
      - `receipts_and_input_never_grant_input_rights`;
      - `requests_coalesce_defer_and_are_never_dropped`;
      - `single_consumer_need_capacity_keeps_unit_and_close_wakes`;
      - `key_code_uses_virtual_keys_for_letters_and_digits` (Windows).
    - `cargo test -p pb-mapper-ffi` (the old FFI still passes).
    - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
      on Windows.
    - The old app still connects Windows to Windows, and its host card still
      shows the host state (through `legacy_host_status`).
  - Host session: `cargo test -p pb-mapper-desktop` (the v1 end-to-end cases
    stay ignored on this branch), which must include:
    - `receipts_return_credit_and_idr_uses_headroom`;
    - `delay_controller_follows_rustdesk_steps`;
    - `bits_per_frame_steps_and_one_way_latch`;
    - `cursor_is_not_echoed_within_300ms_but_generation_changes_pass`;
    - `stale_generation_units_are_dropped_before_admission`;
    - `handshake_reports_denied_and_busy_before_close` (also: a bad token
      while busy → `Rejected{denied}`, never `busy`);
    - `remembered_invitation_survives_restart_but_not_revocation`.

    Then `cargo clippy -p pb-mapper-desktop --all-targets -- -D warnings`.
- **Commits.**
  1. "Add desktop protocol v2, pbd1 invitations and certificate pinning" (tag
     `ovh-g0`)
  2. "Run host sessions on receipt credit and delay-based QoS"
  3. "Report the host's viewer and let the host end its session"
  4. (after G1) "Delete the presentation-credit desktop pipeline"
  5. "Document the receipt-credit performance model"

### P3 Capture worker, encoder and helper (perf WP-W, WP-HL; R-UI-3 rule; spike S2)

- **Goal.**
  - Thread C: `Engine` on `AuthLedger`, the priority lane drain, the pacer,
    the capture step and the 16 ms cursor sampler.
  - `Input::Lock` and the R-UI-3 hold.
  - The wake on every platform: the non-Windows `Wake`/`Waiter` on a
    `parking_lot` `Condvar` with a pending flag, and `Waiter::wait(deadline)`
    instead of `receive(&mpsc::Receiver, …)` (perf §5.10.2). Today the
    non-Windows `notify()` is empty (`D/scheduler.rs:113-145`), so on Linux
    CI thread C would sleep through lane pushes and cancels.
  - The encoder: `submit`/VFR, `set_rate`, infinite GOP with read-back, no
    internal pacing.
  - Helper IPC v2: pipe names, `installation.json` schema 2,
    `update_required`, background `EndSession`, `Snapshot.host` (on the G0
    `HostStatus`), and `double_click` in the `Start`/`Attach` response
    (perf §5.16.3).
  - `video_bench --dump <path>`: writes the `u32 BE len | unit` clip that
    P5's test source plays for P6 (perf §7.5).
- **Owns.**
  - `D/worker.rs` (replaces the G0 stub; deletes the old `Worker` in its last
    commit);
  - `D/pacer.rs`, `D/scheduler.rs`, `D/video/windows.rs`;
  - `D/windows.rs` (lock, capture and `double_click_guard`, after G0);
  - `D/platform.rs`, `D/helper/*`, `D/bin/helper.rs`;
  - `crates/pb-mapper-desktop/Cargo.toml` (production `windows` features
    `Win32_System_Shutdown` and `Win32_UI_Input_KeyboardAndMouse`, after
    G0; the latter is today a dev-only feature, `Cargo.toml:50`, so without
    it the release FFI build fails while tests pass);
  - `examples/{video_bench,input_acceptance,capture_probe}.rs` (after G0).
- **Reads.** `D/lib.rs`, `D/shared.rs`, `D/auth.rs`, `D/lane.rs`,
  `D/outgoing.rs`, `D/idr.rs` (frozen at G0).
- **Depends on.**
  - S2 and the encoder internals can start in Wave 1 on the base, touching
    **only** `D/video/windows.rs` (GOP, `submit`, `set_rate`, `pending_len`)
    while keeping `next_frame` alive, plus a throwaway bench on a scratch
    branch. G0 edits `D/windows.rs` and `examples/video_bench.rs`, so P3
    ports them, and deletes `next_frame`/`next_capture`, only after merging
    `ovh-g0`.
  - The worker and the helper: G0 (including the 5.15 host types).
  - The `input_acceptance` port: merge `p4-vapi`.
- **Stubs and fakes.**
  - A fake `Backend` with a 30 ms `capture_video` and a scriptable
    `double_click_guard`.
  - A fake clock.
  - It reads `Outgoing`/`EngineState` directly and pushes lane commands
    directly.
- **Acceptance.**
  - `cargo test -p pb-mapper-desktop`, which must include:
    - `input_lane_runs_before_capture_and_never_after_stop`;
    - `pacing_is_even_and_skips_ticks_without_bursts`;
    - `fps_and_preset_changes_never_reconfigure_the_encoder`;
    - `lock_is_control_gated_and_failure_keeps_control`;
    - `drag_start_press_waits_out_the_double_click_time` (with the
      re-authorisation cases of section 1.1);
    - `wake_ends_a_wait_on_every_platform` (also green on Linux);
    - `readiness_requires_protocol_2`;
    - the kept worker and helper tests.
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
    on Windows, and `cargo build -p pb-mapper-ffi --release`.
  - `cargo run --release -p pb-mapper-desktop --example video_bench -- 60 60 --pattern-only`
    reports `keyframes == 1 + forced`.
  - `… --example video_bench -- --pace 60,30,15,5` reports produced within
    ±25 % of expected, and records `gop` and the rate model.
  - S2 results go in the handoff to P2.
- **Commits.**
  1. "Use an infinite GOP and a worker-paced encoder submit"
  2. "Pace desktop capture evenly and drain input ahead of capture"
  3. "Lock the remote workstation and hold drag presses past the double-click time"
  4. "Version the helper IPC and ask to update an outdated helper"

### P4 Viewer core (perf WP-V)

- **Goal.** The `D/viewer/` module of perf §6:
  - the read and write tasks and `Outbound` priorities;
  - the full `VideoSink` policy: backlog, IDR supersede, grace, discontinuity;
  - presentation validation;
  - the fps cap, the recovery state machine, pause and resume;
  - `ViewerStatus` and `DesktopView`;
  - `Outbound` move coalescing and the `INPUT_QUEUE` sizing (perf §6.4);
  - the integration and slow-link tests;
  - the examples (`deployed --feedback-log` also enables the `frames`
    target, perf §11.2);
  - the SDK's description: `crates/pb-mapper/README.md`, and the "SDK:
    breaking" CHANGELOG text for P1 (OD-3). `crates/pb-mapper/src/lib.rs:67-70`
    re-exports this crate, `release-sdk.yml:205` publishes it, and the README
    (`:3-21`) still documents `Viewer::next_frame`, `presented` and
    `VideoPreferences`.
- **Owns.**
  - `D/viewer/{mod,outbound,fps_cap,recovery,status}.rs` (new);
  - deletes `D/viewer.rs`;
  - `D/video_sink.rs` (after G0);
  - `D/slow_link.rs`, `D/tests.rs` (after G0), `tests/session.rs`;
  - `examples/{viewer_probe,deployed,transport_probe}.rs` (after G0);
  - `crates/pb-mapper/README.md`;
  - the `pub use viewer::…` line(s) of `D/lib.rs` only.
- **Reads.** The G0 modules and `D/host.rs`.
- **Depends on.**
  - Commits 1-2: G0.
  - Commit 3: the G1 merges of P3, P2 (commits 1-3), P4 (commits 1-2) and P5
    (X13). P4's worktree moves to the merged integration head at the G1
    sitting and authors commit 3 there, before the gate runs.
- **Stubs and fakes.**
  - A scripted host over `tokio::io::duplex`.
  - `VideoSink::detached_for_test()`.
  - In commits 1-2, the end-to-end files (`tests/session.rs`, `slow_link.rs`,
    the host↔viewer cases of `tests.rs`) only compile against the new API
    and stay `#[ignore]`d (X13); commit 3 rewrites and un-ignores them.
- **Acceptance.**
  - On the branch: `cargo test -p pb-mapper-desktop --lib`, which must
    include:
    - `backlog_drops_to_next_idr_with_grace_and_limiter_backoff`;
    - `presentation_requires_handed_current_fresh_monotonic`;
    - `fps_cap_lowers_only_under_backlog_and_probes_up`;
    - `recovery_stops_after_five_decoder_failures_and_pause_suppresses_requests`;
    - `presented_precedes_input_that_references_it`;
    - `bulk_input_and_moves_never_trip_congestion`;
    - `repeated_releases_are_bounded_and_revocation_precedes_a_new_grant`.

    Then `cargo clippy -p pb-mapper-desktop --all-targets -- -D warnings` and
    `cargo build -p pb-mapper-desktop --examples`.
  - Commit 3, at G1 on the merged core:
    `cargo test -p pb-mapper-desktop --test session` (the four integration
    cases), and
    `cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link`,
    run locally and recorded. `rg -n "next_frame|VideoPreferences" crates/pb-mapper`
    finds nothing.
- **Commits.**
  1. "Add the desktop viewer v2 API" (tag `p4-vapi`, within two days)
  2. "Keep only the newest decodable frame and cap fps under viewer backlog"
     (also the SDK README)
  3. (after the G1 merges) "Rewrite the desktop session and slow-link tests
     for receipt credit"

### P5 FFI surface and remembered-computer vault backend (perf WP-F; ui UI-2b Rust/Kotlin; R-UI-1/5/6 FFI)

- **Goal.**
  - The video registry, the C exports and the JNI functions (one return-code
    space), and the error codes (with `VAULT_UNAVAILABLE`).
  - The synthetic test source behind the `desktop-test-source` feature
    (perf §7.5), so P6 can run its consumers on a device in Wave 2.
  - `DesktopState` with the hot export, the 56-byte view leaf and the
    lifecycle ops, including `inspect_invitation`, `host_status` (with the
    running host's `service` and `invitation`), `host_end_session`,
    `host_set_display` and `host_rename` (both refused while background
    hosting is configured).
  - The section 1.1 FFI items and the G1 shim, routed before `Request` is
    deserialised (perf §7.9).
  - The vault: `computers_*` ops, DPAPI or AES-GCM under an Android Keystore
    key, `ComputerKeystore.kt`, and `computers_remember` bound to the live
    session's identity.
  - At G2: the shim deletion.
- **Owns.**
  - `F/desktop.rs`, `F/desktop_video.rs` (new), `F/desktop_jni.rs` (new),
    `F/desktop_shim.rs` (new, deleted at G2), `F/desktop_computers.rs` (new);
  - `F/desktop_trust.rs` (`protect(bytes, encrypt, entropy, description)`
    and `replace` → `pub(super)`, the trust calls keeping today's entropy and
    description; `saved.invitation`);
  - `F/error.rs`, `F/lib.rs`, `F/Cargo.toml` (feature `desktop-test-source`);
  - the root `Cargo.toml` (`jni = "0.21.1"`, already locked) and
    `Cargo.lock`;
  - `K/ComputerKeystore.kt` (new) and `K/MainActivity.kt` (one line);
  - the FFI part of `CLAUDE.md` (G2: view leaf, video exports, JNI; the
    "three integer calls" sentence).
- **Reads.** `D/*`, `W/*`, `K/DesktopVideo*.kt`, `U/ffi/*`.
- **Depends on.**
  - Commit 1: G0 (`VideoSink` skeleton). Then tag `p5-video` for P6's device
    testing; with P3's `video_bench --dump` clip it feeds a real decoder.
  - Commit 2: `p4-vapi`, and the G0 `HostStatus`/`Host::end_session`.
  - Commit 3: commits 1-2.
  - Commit 4: G0 (`Invitation`).
  - Commit 5: G2 (P6 and P7 merged).
- **Stubs and fakes.**
  - `VideoSink::detached_for_test()` for the registry, exports and JNI.
  - A test key and a test current invitation for the vault.
  - A held `stop()` for the hot-op latency test.
- **Acceptance.**
  - `cargo test -p pb-mapper-ffi`, which must include:
    - `hot_ops_never_wait_for_a_slow_disconnect`;
    - `video_consumer_is_exclusive_and_stale_ids_are_harmless`;
    - `inspect_invitation_classifies_without_network`;
    - `shim_routes_old_and_new_connect_shapes` (G1 only; deleted with the
      shim);
    - `vault_round_trip_hides_secrets_and_rejects_tampering` (including a
      remember whose service or fingerprint differs from the live session's);
    - the kept `disconnect_cancels_*`, background and trust tests (the trust
      tests prove that stored trust still decrypts through the new
      `protect` signature).

    The two `size_of` const asserts compile.
  - `cargo ndk -t arm64-v8a check -p pb-mapper-ffi`, also with
    `--features desktop-test-source`.
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
    after merging `p4-vapi`.
  - Shim (G1): the unchanged app connects Windows to Windows and Windows to
    phone.
  - G2: `rg -n "desktop_shim|wait_frame|DesktopBuffer|free_frame" ui crates`
    finds nothing.
  - The on-device vault check (remember → restart → listed → forget) has no
    Dart caller until P8, so it runs at G3 (section 4.2, Stage 3 item 5).
- **Commits.**
  1. "Hand desktop video to native consumers through C and JNI exports"
  2. "Add the synchronous desktop viewer export and per-frame view leaf"
  3. "Keep the old desktop app running on the new core through a shim"
  4. "Store remembered computers encrypted with DPAPI or an Android Keystore key"
  5. (G2) "Remove the desktop frame shim and the old frame exports"

### P6 Native video consumers (perf WP-K, WP-WIN; spike S1)

- **Goal.**
  - Android: `DesktopVideoNative`, `VideoFeeder` with `BufferPool` (no
    permit leak), channel v2 (`create{instance}`, `attach`, `detach`,
    `snapshot`, `stats`, `dispose`), the same-size discontinuity rule,
    presentation receipts to Rust, `PAUSED` handling and a `PixelCopy`
    snapshot. `VideoFeeder` takes its thread-priority call as a parameter,
    so the JVM tests pass a no-op (`Process.setThreadPriority` throws "not
    mocked" there; perf §8.1.2).
  - Windows runner: `GetModuleHandleExW` on the DLL Dart already loaded (never
    `LoadLibraryExW`, which could load a second copy) and symbol resolution,
    the pull loop, receipts on first acquisition, a WIC snapshot serviced
    after **every** `next` return through an atomic request flag (with steady
    video `next` never times out, perf §8.2), and the token check.
- **Owns.**
  - `K/DesktopVideoNative.kt`, `K/VideoFeeder.kt`, `K/BufferPool.kt` (new),
    `K/DesktopVideo.kt`;
  - `ui/android/app/src/test/kotlin/com/pb_mapper/ui/{VideoFeederTest,DecoderRulesTest}.kt`
    (new);
  - `ui/android/app/build.gradle.kts` (test dependencies only, if needed);
  - `W/desktop_video.cpp`, `W/desktop_video.h`, `W/CMakeLists.txt`
    (`windowscodecs`);
  - `W/flutter_window.cpp` only for X12.
- **Reads.** `F/desktop_video.rs`, `F/desktop_jni.rs`, `K/MainActivity.kt`.
- **Depends on.**
  - Kotlin and C++ code against the frozen ABI (perf 7.4-7.6, 8.1-8.2) start
    at once.
  - On-device runs need `p5-video` built with `--features desktop-test-source`
    and a clip from `video_bench --dump` (P3). Nothing else feeds a sink
    before G2: the old Dart and the shim cannot drive channel v2. The S1
    scratch app (never merged) calls `pb_mapper_desktop_video_test_source_v1`
    and channel v2 `create`/`attach`, so the feeder, decoder, receipts,
    `PAUSED` handling and snapshot run on the phone in Wave 2.
  - S1 runs in Wave 1 on throwaway code in a scratch branch that is never
    merged.
- **Stubs and fakes.**
  - A fake `VideoSourceApi` and a fake `FeedTarget` for the JVM tests.
  - The C ABI header from perf 7.5.
- **Acceptance.**
  - S1: the instance tokens from Dart and Kotlin are equal on the phone
    (recorded). If they differ, **stop**: perf 7.6 needs a redesign before
    G2.
  - `& $flutter build apk --debug`, then from `ui/android`
    `.\gradlew.bat :app:testDebugUnitTest`: `VideoFeederTest` (the
    permit-leak case), `DecoderRulesTest` and `VideoPresentationQueueTest`.
  - `& $flutter build windows --debug`.
  - Wave 2 on the phone, with the test source: present, `PAUSED` and resume,
    a same-size discontinuity without a rebuild, snapshot under 300 ms.
  - At G2 on device: connect, present, background and resume, reconnect
    (the texture persists), snapshot under 300 ms, including a Windows
    disconnect while `video_bench` animates.
- **Commits.**
  1. "Feed Android MediaCodec from Rust on a pooled feeder thread"
  2. "Pull desktop video into the Windows decoder thread"
  3. "Snapshot the last presented picture on Android and Windows"

### P7 Dart pipeline and DesktopStore (perf WP-D; R-UI-1, 2, 3-Dart, 4, 5-Dart, 7, 8, 9, 10)

- **Goal.**
  - Upload (X15, first, on the base): the old schema-1 tool sends its report
    with `PUT /report` to the loopback bootstrap instead of writing
    `reportPath` (the define goes away), and `scripts/test/desktop-bootstrap.cjs`
    accepts it (loopback only, ≤ 8 MiB, must parse as JSON, saved as
    `report-<n>.json` under the dedicated directory with a server-chosen
    name, prints `report_saved <path>`). `scripts/test/desktop-ui-acceptance.md`
    follows (section 4.2). G1 merges this commit for Stage 1; the app itself
    is unchanged.
  - D1: `desktop_models.dart`, `desktop_input.dart` and the perf §9.5
    `DesktopStore` surface with the §9.6 policies (no `ChangeNotifier`,
    demand-driven polling, phases and reconnect, pause and resume, input
    stamping by Rust) over a complete `FakeDesktopApi`, plus the section 1.1
    Dart signatures. Specifically also:
    - the host invitation derivation and `configured` with no invitation
      cache, and the invariant `sharing || configured ⇒ invitation != null`
      (perf §9.4);
    - `connect(target)` supersedes an attempt in flight and is never gated
      by host `busy` (perf §9.6.2);
    - `NOT_FOUND`, `VAULT_UNAVAILABLE` and any unlisted code are terminal; a
      fourth consecutive `protocol` stop is terminal; a resume that finds
      the session stopped by `transport`/`timeout` reconnects at once
      (perf §9.6.2, R13).
  - D1 keeps the **whole package green** (X14, perf §9.8): it ports the old
    viewer, page, banner and gesture surface to the slices and to
    `send`/`sendSequence` (G2's smoke runs on them; the old viewer's
    Disconnect passes a temporary `snapshotPath` so the smoke can check the
    native snapshot), ports the acceptance tool's compile, and deletes
    `T/desktop_viewer_navigation_test.dart`.
  - Then the real `DesktopApi`: the hot export on the UI isolate, the view
    leaf, the JPEG worker, `DesktopVideoTexture` v2 and the FFI lookups.
  - Then the acceptance tool with report schema 2 (still uploading).
- **Owns.**
  - `U/ffi/pb_mapper_ffi.dart` (desktop lookups only),
    `U/ffi/pb_mapper_service.dart`, `U/ffi/desktop_api.dart`,
    `U/ffi/desktop_video.dart`;
  - `U/state/desktop_store.dart`, `U/state/desktop_models.dart` (new),
    `U/state/desktop_input.dart` (new);
  - `U/state/app_store.dart` (the D14 listener line only);
  - the port of `U/pages/desktop_page.dart`, `U/pages/desktop_viewer.dart`,
    `U/pages/desktop_connection_banner.dart`,
    `U/pages/desktop_gesture_surface.dart` and `U/app.dart` (banner lines
    only, if needed);
  - `ui/tool/desktop_viewer_acceptance.dart`,
    `scripts/test/desktop-bootstrap.cjs`, `scripts/test/desktop-ui-acceptance.md`;
  - `T/fake_desktop_api.dart`, `T/desktop_store_test.dart` (new),
    `T/desktop_recovery_test.dart`, `T/desktop_lifecycle_test.dart` (rewrite
    or delete); deletes `T/desktop_viewer_navigation_test.dart`;
  - the pipeline and store parts of `T/desktop_test.dart` and
    `T/desktop_host_readiness_test.dart`.
- **Reads.** `F/*` (the ops), the contracts.
- **Depends on.**
  - Upload and D1: nothing. Start in Wave 1 on `overhaul-base`; no P1
    dependency (X6).
  - The real API: the frozen ops, verified at G2.
- **Stubs and fakes.** `FakeDesktopApi` and a fake `DesktopTextureFactory`.
- **Acceptance.**
  - At **every** tag (`p7-upload`, `p7-d1`, `p7-tool`): `& $flutter analyze
    --no-fatal-infos` and the full `& $flutter test` are green.
  - `p7-upload`: a local run of the tool against `deployed` and the
    bootstrap on Windows writes `report-1.json`.
  - `& $flutter test test/desktop_store_test.dart test/desktop_recovery_test.dart test/desktop_host_readiness_test.dart test/desktop_test.dart test/store_test.dart`
    (plus `desktop_lifecycle_test.dart` if kept). It covers the perf 10.4
    cases, including "a sharing or configured host always has its
    invitation" and "a second connect during an attempt connects the new
    target"; timers run under `testWidgets` and `tester.pump(duration)`.
  - At G2:
    - `& $flutter build apk --debug --target tool/desktop_viewer_acceptance.dart`;
    - `& $flutter build windows --debug`;
    - check on Windows that minimise **and** hide-to-tray report
      `AppLifecycleState.hidden` (X12).
- **Commits.**
  0. "Upload the desktop acceptance report to the loopback bootstrap" (tag
     `p7-upload`)
  1. "Reshape DesktopStore into value slices with demand-driven polling" (tag
     `p7-d1`)
  2. "Bind DesktopApi to the hot viewer export and native video sources"
  3. "Rewrite the desktop acceptance tool for report schema 2" (tag `p7-tool`)

### P8 Remote desktop state, shell and page (ui UI-2a, UI-2b Dart, UI-3, UI-4)

- **Goal.**
  - `ViewerPrefs` wiring, `RemoteComputerStore`, `ViewerHost` and
    `RemoteDesktop`, and the whole Dart vault (X1). `RemoteDesktop.open`
    awaits `computers.load()` first; remembers pass the target's `service`
    and `fingerprint`; a session left paused ends after
    `kPausedSessionTimeout` (ui §5.4).
  - Six rail entries and five bar entries, Logs inside Settings on phones,
    the D15 capsule, `SharingFallbackLayer` and the tray, with the OD-1
    default for a hidden window (tray state, `show(inactive: true)`) and the
    "Connected · paused" state (ui §4.6-§4.7), and D26 filtering, including
    no tunnel Connect for `desktop:*` on the Relay page (ui §4.8).
  - The transitional viewer screen, then the full Remote page: host card
    (display picker and rename disabled while background hosting, an
    `InviteWell` that renders nothing without an invitation), QR, computer
    lists, the add dialog and sheet, and the scanner.
- **Owns.**
  - `U/state/remote_computers.dart`, `U/state/remote_desktop.dart`,
    `U/ffi/remote_computer_vault.dart` (all new);
  - `U/state/remote_service.dart`, `U/state/app_store.dart` (after `p7-d1`);
  - `U/shell/app_scope.dart`, `U/shell/navigation.dart`,
    `U/shell/page_frame.dart` (after P1);
  - `U/shell/sharing_indicator.dart` (new);
  - `U/app.dart`, `ui/lib/main.dart` (the `--desktop` hand-off; it is outside
    `U/`), `U/common/tray/tray_service.dart`;
  - `U/pages/{logs_page,settings_page,tunnels_page}.dart`,
    `U/pages/relay/services_tab.dart` (Connect hidden for `isDesktop`);
  - `R/*` (new): `remote_page`, `host_card`, `invite_qr`, `computer_list`,
    `computer_list_compact`, `add_computer`, `invite_scanner`,
    `remote_format` and `remote_actions`;
  - `V/viewer_screen.dart` (transitional, created in UI-3);
  - deletes `U/pages/desktop_page.dart`;
  - `T/fake_remote_computer_vault.dart`, `T/remote_computers_test.dart`,
    `T/remote_desktop_test.dart`, `T/sharing_test.dart`,
    `T/remote_page_test.dart` (all new);
  - `T/app_test.dart`, `T/tray_service_test.dart`, `T/support.dart`
    (`pumpApp`);
  - the host widget parts of `T/desktop_test.dart` and
    `T/desktop_host_readiness_test.dart`.
- **Reads.** The P1 primitives, the `p7-d1` store, `V/*` after UI-3.
- **Depends on.**
  - P1 commit 3 on the integration branch, plus a merge of `p7-d1`. The lead
    creates the worktree that way.
  - The vault ops arrive with P5 commit 4. Until then, `open()` returns
    unavailable.
  - Commit 2 needs the owner's OD-1 answer, or builds its default.
- **Stubs and fakes.** `FakeDesktopApi`, `FakeRemoteComputerVault`,
  `InviteScanner.debugSupportedOverride = false`.
- **Acceptance.**
  - `& $flutter test test/remote_computers_test.dart test/remote_desktop_test.dart test/app_test.dart test/tray_service_test.dart test/sharing_test.dart test/remote_page_test.dart test/desktop_host_readiness_test.dart test/store_test.dart`.
    It covers T1c, T3, T9 (with the paused timeout), T10, T11a, T11b and
    T11c (the QR hides when the page is left or the app goes inactive).
  - `& $flutter build apk --debug`.
- **Commits.**
  1. "Add remembered remote computers and the viewer host state"
  2. "Add Remote desktop to the rail and show sharing controls while viewed"
     (tag `p8-shell`)
  3. "Rebuild the Remote desktop page with host card, computer list and add flow"

### P9 Viewer UI: input core, desktop viewer, mobile viewer (ui UI-5, UI-6, UI-7)

- **Goal.**
  - The `TouchGestureMachine`, `RemoteViewport`, `RemoteSurface`,
    `RemoteInput` (with the burst bound of ui §10.4: `kLatchedTextKeys`,
    `kTextMaxBytes`), `ModifierLatch`, `KeyRouting` and `RemoteTextInput`.
  - `ViewerStage`, the overlays (no opaque cover while a frame can arrive,
    no blur), the cursor layer, the menus, the quality panel, the desktop
    viewer, its title bar and the toolbar.
  - The landscape touch viewer with gutters, side panel, modifier bar,
    minimap, virtual mouse and gesture sheet.
- **Owns.**
  - `V/*` except the transitional `viewer_screen.dart` before `p8-shell`;
    rewrites `viewer_screen.dart` after it;
  - deletes `U/pages/desktop_viewer.dart`,
    `U/pages/desktop_gesture_surface.dart` and
    `U/pages/desktop_connection_banner.dart`;
  - `T/touch_gesture_test.dart`, `T/remote_surface_test.dart`,
    `T/remote_input_test.dart`, `T/remote_text_input_test.dart`,
    `T/viewer_chrome_test.dart` (all new);
  - deletes `T/desktop_gesture_test.dart` (P7 already deleted
    `T/desktop_viewer_navigation_test.dart`, X14);
  - the viewer widget part of `T/desktop_test.dart`;
  - the constructor lines of `ui/tool/desktop_viewer_acceptance.dart`, after
    `p7-tool`.
- **Reads.** The P1 primitives, the `p7-d1` store, `AppScope`/`ViewerHost`/
  `RemoteDesktop` from P8.
- **Depends on.**
  - UI-5: P1 commit 3 and `p7-d1`.
  - UI-6: also merge `p8-shell`.
  - The tool edit: merge `p7-tool`.
- **Stubs and fakes.** `FakeDesktopApi`, `ValueNotifier`s for `ViewerStage`, a
  recording `RemotePointerSink`, `InviteScanner.debugSupportedOverride`.
- **Acceptance.**
  - `& $flutter test test/touch_gesture_test.dart test/remote_surface_test.dart test/remote_input_test.dart test/remote_text_input_test.dart test/viewer_chrome_test.dart`.
    It covers T2, T4a, T4b, T5 (with the burst bound: a 64-character
    deletion is 8 sequences, a latched commit keys 16 characters), T7a-c and
    T8.
  - `& $flutter build windows --debug`.
  - `& $flutter build apk --debug --target tool/desktop_viewer_acceptance.dart`.
  - After `p8-shell`, `rg -n "\bIcons\." lib` finds nothing.
- **Commits.**
  1. "Add the viewer input core: gestures, latches, key routing and IME"
  2. "Replace the desktop viewer with a full-window viewer and floating toolbar"
  3. "Add the landscape mobile viewer with gutters, modifier bar and minimap"

**Size note.** P9 is the largest package, about 6-7 engineer-days. If its
session compacts repeatedly, hand the work off at a commit boundary with a
self-contained brief (user CLAUDE.md, task orchestration).

---

## 3. Waves, integration order and hotspots

### 3.1 Waves

| Wave | Runs in parallel (separate worktrees) | Ends with |
| --- | --- | --- |
| 0 (lead, about ½ day) | Ask OD-4, then merge `origin/master` (X8), resolving `pb-mapper-client/src/{direct/mod.rs,sdk/client.rs}`; run `cargo test -p pb-mapper-client` and clippy. After that merge, re-check that `ownNamespace = keyId` (`U/state/app_store.dart:115`) and the trust `Scope.namespace = key_id` (`F/desktop.rs:595`) still mean what `RemoteComputer.namespace` and `ComputerPresence.otherRelay` assume (ui §5.2). Restore the line-ending-only registrants (X9). Check that `& $flutter build apk --release` of the base succeeds, i.e. the pinned signing key resolves on this machine (`ui/android/app/build.gradle.kts:63-84`); if not, the owner restores it before Stage 1, or the acceptance APKs use `--profile` (section 4.2). Ask OD-1 and OD-2. Tag `overhaul-base`. Create the worktrees. | `overhaul-base` |
| 1 | P1 (commits 1-3) · P2 commit 1 · P7 commit 0 (`p7-upload`) and D1 · P6 (Kotlin and C++ against the ABI, and spike S1) · P3 (S2 and encoder internals, `D/video/windows.rs` only) | P1 commits 1-3 merged to integration; `ovh-g0` merged; `p7-upload` and `p7-d1` tagged |
| 2 | P2 host session · P3 worker and helper · P4 commits 1-2 · P5 (commits 1, 4, then 2 after `p4-vapi`, then 3) · P6 (device runs after `p5-video` with the test source) · P7 commits 2-3 · P8 · P9 | every Rust package ready for G1 |
| 3 = G1 (lead) | In one sitting: merge P3 → P2 (commits 1-3) → P4 (commits 1-2) → P5 (commits 1-4, with the shim) → `p7-upload`; then P4 authors commit 3 on the merged head (X13) and the lead merges it; then P2's cleanup commit and docs commit; then the gate | `ovh-g1`; **user Stage 1** (case P0) is recorded before G2 merges |
| 4 = G2 (lead) | Merge P6 → P7 → P5 commit 5; P5 updates CLAUDE.md | `ovh-g2`; **user Stage 2** smoke |
| 5 = G3 UI (lead) | Merge P8 → P9 (P9 already contains `p8-shell`) | P1 finish starts |
| 6 | P1 finish (commits 4-5); then the lead's final checklist (section 4.1) | **user Stage 3** acceptance; merge decision (D28) |

P8 and P9 keep developing during Waves 2-4 and land only at G3. Their bases
are `p7-d1`, which already holds the final store surface. P7 must not rename
or remove a `p7-d1` member afterwards. It may only add members.

### 3.2 Merge mechanics

- Merge with `git merge --no-ff ovh/pN-…` into `claude/remote-desktop-overhaul`.
  Package commits stay intact (D27); never squash.
- A package that consumes a milestone runs `git merge <tag>` into its own
  branch. At integration that history is already shared.
- During G1, the workspace may fail to compile between the P4 and P5 merges
  (X3), and the end-to-end tests stay ignored until P4's commit 3. The gate
  checks run after the last commit of the sitting.
- P1 commits 1-2 are also cherry-picked to a master PR by the owner, when the
  owner wants it.

### 3.3 Gate checks (run by the lead after the gate's last merge)

| Gate | Checks |
| --- | --- |
| G0 | P2's G0 acceptance (with `--include-ignored`), plus `cargo test --workspace --all-features` |
| G1 | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` (Windows); `cargo build -p pb-mapper-ffi --release` (the build that `scripts/build/pb-mapper-ffi-windows.ps1` runs; dev-only `windows` features do not reach it); `cargo test -p pb-mapper-desktop -p pb-mapper-ffi` (no v1/v2 end-to-end test left ignored); `cargo test -p pb-mapper-desktop --test session`; the ignored `slow_link` locally; `deployed selftest`; `input_acceptance` on Windows; the helper reinstall path (install the PR #51 helper → `update_required` → Update → works); the old app on Windows and the phone through the shim; `& $flutter test` (the old Dart, unchanged apart from the tool's upload) |
| G2 | All G1 Rust checks; `cargo ndk -t arm64-v8a check -p pb-mapper-ffi`; `& $flutter analyze --no-fatal-infos`; `& $flutter test`; `.\gradlew.bat :app:testDebugUnitTest`; `& $flutter build apk --release`; `& $flutter build windows --release`; the acceptance tool builds for apk and windows |
| G3 | All G2 checks; P8 and P9 acceptance; `rg -n "\bIcons\." ui/lib` empty |

### 3.4 Conflict hotspots: one owner, one rule, the lead resolves

| File(s) | Writers, in order | Rule at merge |
| --- | --- | --- |
| `Cargo.lock` | P2 (G0: a new `bytes` edge), P5 (`jni`) | Never hand-merge. Take the integration side, then `cargo update --workspace --offline` and `cargo check --workspace --locked`. No version may change. |
| Root `Cargo.toml` | P5 | — |
| `crates/pb-mapper-desktop/Cargo.toml` | P2 (G0), then P3 | sequential |
| `D/lib.rs` | P2; P4's `pub use viewer::…` line(s) only | The lead keeps both; P2 reviews |
| The G0 mechanical-edit set (perf §12.1 WP-0): `D/windows.rs`, `D/worker.rs`, `D/helper/{client,worker,background_service}.rs`, `D/host.rs`, `D/slow_link.rs`, `D/tests.rs`, `D/viewer.rs`, `tests/session.rs`, `examples/{input_acceptance,video_bench,deployed}.rs`, `F/desktop.rs` | P2's G0 edits, then P3 (`D/windows.rs`, worker, helper, `input_acceptance`, `video_bench`), P4 (viewer, tests, `deployed`) or P5 (`F/desktop.rs`) | sequential, because the later packages branch from `ovh-g0`; P3 touches none of them before merging `ovh-g0`; a deletion (P4: `viewer.rs`) wins |
| `D/scheduler.rs` | P3 only | `Wake::{new, notify}` and `Clone` stay, because G0's lane and shared state use them |
| `ui/pubspec.yaml`, `pubspec.lock`, plugin registrants | P1 only | Other packages request dependencies from P1. A registrant conflict is regenerated with `& $flutter pub get`. |
| ARB files, generated `app_localizations*.dart`, `l10n_untranslated.json` | P1 (add, then delete); P8 and P9 append only at their block ends (X10) | Keep both sides of the ARB JSON, then `& $flutter gen-l10n`. Never hand-merge the generated Dart. |
| `U/theme/app_icons.dart` | P1 (complete list) | A missing glyph is added by its package at the end of the remote block; keep both |
| `T/support.dart` | P1 (fonts), then P8 (`pumpApp`) | sequential |
| `U/shell/page_frame.dart`, `U/shell/navigation.dart` | P1, then P8 | sequential |
| `U/state/app_store.dart` | P7 (`p7-d1`), then P8 | sequential |
| `T/fake_desktop_api.dart` | P7 only | P8 and P9 extend it in their own test helpers (subclass), never edit it |
| `T/desktop_test.dart`, `T/desktop_host_readiness_test.dart` | P7, P8, P9 | Keep every removal; the merge that empties a file deletes it |
| `U/pages/desktop_page.dart`, `desktop_viewer.dart`, `desktop_gesture_surface.dart`, `desktop_connection_banner.dart` | P7 port to D1 (X14); P8 and P9 delete | Modify-versus-delete: the deletion wins |
| `T/desktop_viewer_navigation_test.dart` | P7 deletes (`p7-d1`) | — |
| `ui/tool/desktop_viewer_acceptance.dart` | P7 (`p7-upload`, `p7-d1`, `p7-tool`), then P9 | sequential |
| `scripts/test/desktop-bootstrap.cjs`, `scripts/test/desktop-ui-acceptance.md` | P7 only | — |
| `crates/pb-mapper/README.md` | P4 only | — |
| `U/pages/relay/services_tab.dart` | P8 only | — |
| `V/viewer_screen.dart` | P8 (creates), then P9 (rewrites) | sequential, through `p8-shell` |
| `K/MainActivity.kt` | P5 only | — |
| `W/flutter_window.cpp`, `desktop_video.dart` (X12 only) | P6, then P7 | sequential, only if needed |
| `docs/remote-desktop.md` | P2 (perf, at G1), P1 (UI, finish), lead (evidence after acceptance) | sequential |
| `CLAUDE.md` | P5 (FFI, G2), P1 (UI, finish) | sequential |
| `CHANGELOG.md` | Wave 0 master merge, then P1 (with text from P2, P3 and P4) | — |
| `docs/ui-design.md`, `docs/assets/poster*.png` | P1 only | — |
| `.github/workflows/syntax-check.yml` | P1 (Icons grep). Gradle tests in CI are the owner's call. | — |

---

## 4. Final integration checklist and on-device acceptance

### 4.1 Lead checklist (after P1 finish, on `claude/remote-desktop-overhaul`)

- [ ] All 9 packages merged in section 3.1 order. The tags `ovh-g0`, `ovh-g1`
      and `ovh-g2` exist. No package commits were squashed.
- [ ] Deleted:
      - `D/congestion.rs`, `D/media.rs`, `D/viewer.rs`, `D/desktop_shim.rs`;
      - `U/pages/desktop_*.dart`;
      - `T/desktop_gesture_test.dart`, `T/desktop_viewer_navigation_test.dart`.

      `rg -n "desktop_shim|wait_frame|DesktopBuffer|MAX_QUEUED_FRAMES|presentation_feedback|VideoPreferences|SessionStatus" crates ui`
      finds nothing (that includes `crates/pb-mapper/README.md`, which P4
      rewrote).
- [ ] `rg -n "\bIcons\." ui/lib` finds nothing. There are no `"desktop` keys
      in the ARB files, and `l10n_untranslated.json` is empty.
- [ ] Rust on Windows:
      - `cargo fmt --all -- --check`;
      - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
      - `cargo test --workspace --all-features`;
      - `cargo doc --workspace --no-deps`;
      - `cargo build -p pb-mapper-ffi --release`;
      - `cargo ndk -t arm64-v8a check -p pb-mapper-ffi`.

      CI (Linux) is green on the pushed branch, including
      `scheduler::wake_ends_a_wait_on_every_platform`.
- [ ] Flutter:
      - `& $flutter analyze --no-fatal-infos`;
      - `& $flutter test` once;
      - `.\gradlew.bat :app:testDebugUnitTest`;
      - `& $flutter build windows --release` and `& $flutter build apk --release`;
      - the acceptance tool builds for both.
- [ ] The security boundaries' tests are present and green:
      - `receipts_and_input_never_grant_input_rights`;
      - `frozen_renderer_keeps_credit_and_cursor_but_never_input`;
      - `drag_start_press_waits_out_the_double_click_time` (held input is
        re-authorised);
      - `vault_round_trip_hides_secrets_and_rejects_tampering` (including the
        identity binding of `computers_remember`);
      - T11a (the invitation is never fully on screen) and T11c (the QR hides);
      - `fingerprint_pin_rejects_another_certificate`.
- [ ] Docs:
      - `docs/remote-desktop.md` (performance model, SDK section and UI);
      - `docs/ui-design.md`;
      - `CLAUDE.md` (pages, FFI exceptions, `computers_*`, `pb_mapper/keystore`);
      - `CHANGELOG.md` (gestures, ALPN v2, helper update, Android backup,
        "SDK: breaking", iOS/macOS unverified);
      - `crates/pb-mapper/README.md`;
      - `ui/README.md`.

      **Merge-block statements still present** (D28). The owner's answers to
      OD-1 to OD-4 are recorded in DECISIONS.
- [ ] P1 commits 1-2 cherry-pick cleanly onto `origin/master` (checked in P1).
      The owner decides whether to open that PR.
- [ ] Builds for Stage 3 are prepared: the release APK (product), the
      acceptance APK, the Windows release with the FFI built by
      `scripts/build/pb-mapper-ffi-windows.ps1`, and the Android jniLibs from
      `cargo ndk -o ui/native/android -t arm64-v8a build -p pb-mapper-ffi --release`.

### 4.2 On-device acceptance procedure (run by the user)

The common setup below and perf §11.2 are the same procedure; change both
together.

**Common host setup** (Windows, repository root, three terminals; `--control`
only against a benign target you own):

```powershell
cargo build --release -p pb-mapper-desktop --example deployed --example video_bench
target/release/examples/deployed.exe APP_CONFIG_PATH target/desktop-acceptance serve 600 --tcp --control --feedback-log
node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance        # after "host_ready"
target/release/examples/video_bench.exe 600 60 --pattern-only            # moving content
```

- QUIC carrier (case B): restart `deployed` **without** `--tcp`. It then
  uses the carrier of `APP_CONFIG_PATH` (`examples/deployed.rs:37-38`), which
  must be QUIC; check that before the run. `controller.json` hands the
  carrier to the phone.
- `--feedback-log` installs its own filter and deliberately ignores
  `RUST_LOG` (`examples/deployed.rs:23-33`), so setting `RUST_LOG` has no
  effect. P4's port of `deployed` widens that filter to
  `off,pb_mapper_desktop::feedback=debug,pb_mapper_desktop::frames=trace`,
  which gives the perf §11.1 host trace.

**Common phone setup** (from `ui/`). Stage 1 uses the schema-1 tool from
`p7-upload`; Stages 2-3 use the schema-2 tool.

```powershell
& $flutter build apk --release --target tool/desktop_viewer_acceptance.dart `
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json --dart-define=seconds=120 `
  --dart-define=quality=balanced
adb install -r build\app\outputs\flutter-apk\app-release.apk
adb reverse tcp:18747 tcp:18747
```

- Relay-only runs (case B) add `--dart-define=relayOnly=true`. `deployed`
  writes no `relay_only` into `controller.json` (`examples/deployed.rs:77`),
  so the define decides.
- `quality` is a schema-2 define; the schema-1 tool ignores it.
- At the end of the run the tool sends its report with
  `PUT http://127.0.0.1:18747/report` through the same `adb reverse`. The
  bootstrap writes it to `target/desktop-acceptance/report-<n>.json` and
  prints `report_saved <path>`. Nothing is read from the app's private
  storage, so a release APK, which is not debuggable and cannot use
  `run-as`, works.
- The release build needs the pinned signing key
  (`ui/android/app/build.gradle.kts:63-84`); Wave 0 checks that it resolves
  on this machine. If it does not, restore it (the build forbids generating
  a replacement), or build the acceptance APK with `--profile` (AOT Dart). If
  that APK's signature differs from the installed product's,
  `adb install -r` fails: run `adb uninstall com.pb_mapper.ui` first, which
  also deletes the phone's app data.

Never install the tool as the product entry. Reinstall the product APK
afterwards.

**Stage 1, at G1 (about 30 min): P0 measured alone.** The phone runs the old
app pipeline through the shim.

- Case A: direct Wi-Fi, 60 Hz phone.
- Case B: relay only (`relayOnly=true`), over TCP (`deployed --tcp`), then
  over QUIC (`deployed` restarted without `--tcp`).

Keep each `report-<n>.json` and the host `feedback` log per case. Look for:
no "burst, stall, burst" (no `gaps_250` while RTT is stable),
`skipped_ticks` without `paused`, and the resolution never changing. G2 waits
for this record. It is evidence only, not a pass or fail.

**Stage 2, at G2 (about 30 min): smoke on both viewers.** It runs on the old
pages that P7 ported (X14). On the phone and on a Windows viewer:

- connect;
- take control and move, click and type;
- background or minimise for 10 s, then resume (a picture within 1 s, no
  reconnect); on Windows also hide to the tray with the close button and
  restore (X12);
- on the phone, background for 60 s and for 5 min and note whether the app
  was frozen (it reconnects once on resume) — the measurement behind OD-2
  (perf R13);
- toggle Wi-Fi (it reconnects, and the last picture stays grey under the
  pill);
- disconnect while `video_bench` animates: the thumbnail file is written on
  both platforms (the Windows runner services the snapshot between frames,
  perf §8.2).

Also confirm that the S1 tokens match in the debug log.

**Stage 3, at G3 (about 2-3 h): full acceptance.** The pass criteria are
perf §11.4. They are proposals, and the owner decides.

1. Performance (perf §11.3):
   - A: direct Wi-Fi, Balanced, 60 Hz. Presented fps p50 ≥ 55, interval
     p95 ≤ 25 ms and p99 ≤ 50 ms, `gaps_250 = 0`, latency p50 ≤ 80 ms, plus
     one camera check (20 readings against the host's "Unique desktop
     presentation N").
   - B: relay over TCP and QUIC (≥ 30 fps, no burst-stall).
   - C: cellular.
   - D: Smooth and Quality, and a Windows viewer at the display refresh.
   - E: background 10 s, then resume, × 5, then once each for 60 s and
     5 min. Without a freeze: a picture within 1 s, no reconnect. After a
     freeze: one immediate reconnect and a picture within 3 s (OD-2
     default). Always ≤ 2 IDRs per resume and no input replay.
   - F: decoder hints on versus off.
   - G: UAC prompt, lock screen and Win+L through the helper.
   - H: RustDesk side by side.
   - I: `video_bench --pace 60,30,15,5` and `video_bench 60 60` on each
     host GPU.

   Record the build, preset, refresh rates, GPU, `gop`, rate model, phone,
   decoder, temperature and network.
2. Windows chrome:
   - At 100/125/150 % scaling, maximised and restored, the hovered close
     button is ≥ 16 px inside the sheet edge, and the glyphs are crisp.
   - The icons look right on every page.
3. Navigation: six rail entries; five bar entries on the phone; Logs is
   reachable from Settings, and its back button returns.
4. Host (Windows):
   - Enable remote control, through UAC and the helper.
   - View only versus view and control.
   - Copy the invite; QR shows and hides, and hides when you leave the page.
   - Make view-only takes effect live.
   - The capsule (Disconnect viewer, Stop sharing) appears while viewed and
     is still reachable over a dialog and in a full-screen viewer; the tray
     offers both.
   - D15 with the window hidden (OD-1, as decided; default): hide the window
     to the tray, connect from the phone → the tray shows the active icon and
     "Someone is viewing this computer", and the window comes back without
     taking focus, showing the capsule.
   - While the phone is on "Back to list", the capsule, tray and host card
     say "Connected · paused"; after 10 min the phone ends the session and a
     second viewer can connect.
   - With the background host configured, the display picker and rename are
     disabled with "Stop background sharing to change this".
   - An outdated helper offers "Update".
5. Add a computer:
   - Phone: scan the QR; deny the camera once (paste still works).
   - Desktop: paste; a setup string is rejected with its message.
   - Remember is on; connect, disconnect (the thumbnail is written), restart
     the app, and the computer is still listed with its online dot and
     thumbnail, on Android and on Windows (the vault check moved here from
     P5).
   - Forget removes it and its thumbnail.
   - Android backup excludes the secrets: with the local transport
     (`adb shell bmgr transport com.android.localtransport/.LocalTransport`),
     `adb shell bmgr backupnow com.pb_mapper.ui`, then uninstall, reinstall
     and restore (`adb shell bmgr restore`): the restored app has no relay
     credential, no tunnels and no remembered computers. A manual
     restore-to-another-device test is equivalent.
6. Desktop viewer:
   - The toolbar pins, and unpinned it collapses 2 s after the pointer
     leaves.
   - Display: fit or original, quality presets live, no reconnect.
   - Shortcuts: Win+D, Alt+Tab, Ctrl+Shift+Esc, Win; Win+L locks;
     Ctrl+Alt+Del is shown as unsupported.
   - Six overlays: Wi-Fi off gives reconnecting; host lock gives the secure
     desktop.
   - A German keyboard layout keeps AltGr characters; Ctrl+S, Alt+F and
     Win+E work.
   - With a **Russian** host layout, Ctrl+C, Ctrl+V and Win+E still work and
     type no stray letters (virtual keys, perf §4.5); the same for a latched
     Ctrl+C from the phone's modifier bar.
7. Mobile viewer:
   - Landscape lock; gutters.
   - Trackpad mode by default: tap clicks at once; long press right-clicks;
     double-tap-and-hold drags (an Explorer icon moves, it does not open);
     three fingers scroll; two fingers pinch and pan locally.
   - Modifier bar: Ctrl one-shot, lock and release; the latches clear when
     the app goes to the background.
   - The soft keyboard keeps zoom and offset.
   - The minimap shows.
   - The virtual mouse works in direct mode.
   - The gesture sheet shows once.
   - The IME works on Samsung and Gboard; deleting a long word or a
     selection, and pasting a paragraph, keep control (perf §6.4 sizing).
8. Compatibility (ALPN v2, D1). A PR #51 build cannot show the new "update
   pb-mapper" text, because that mapping exists only in the new viewer
   (`D/security.rs:12, 98, 134` offers only v1):
   - the new viewer against a PR #51 host ends on `viewerEndedVersion`;
   - a PR #51 viewer against the new host fails at the TLS handshake, with
     whatever message that build shows, and never mid-session.

The lead appends the evidence to `docs/remote-desktop.md` and
`docs/validation/desktop-input-backpressure.md`. The merge-block statements
are lifted only when the user accepts fluidity (D28).

---

## Rejected critique items

`CRITIQUE.md` (H1-H6, M1-M16, L1-L16) was applied in full to this plan and
both contracts; no item was rejected outright. These items were applied
differently from the critic's suggested fix, for the reason given:

| Item | Critic's suggestion | Applied instead | Why |
| --- | --- | --- | --- |
| H1 | Backport the report upload into the old tool in P5's shim commit; P2's docs commit owns `scripts/test/desktop-ui-acceptance.md` | P7 lands the upload as its own first commit, `p7-upload`, which G1 merges; P7 owns the bootstrap script and the acceptance doc | The tool, the bootstrap and their procedure change together, and P7 already owns the tool; a Rust package editing Dart would add a cross-owner edit. The `--debug` + `run-as` route was not taken: debug Dart would distort the P0 measurement. |
| H2 | `domain="root" path="app_flutter/"` exclusions, or `android:allowBackup="false"` | The exclusions (root and file domains, cloud and device transfer) | For apps targeting Android 12+, `allowBackup="false"` does not stop device-to-device transfer, so the explicit rules are needed anyway. |
| H3 | Order: the host's value, else the result of the last enable/start/restore/set-display/rename, else the background's, else the remembered grant's | No cache of action results: `host_status` carries the running host's invitation, and the store re-reads it before it clears `enabling`/`restarting` (perf §9.4) | One source of truth per value; a cached action result could outlive a stop or revoke. The invariant and its four-case test are as the critic asked. |
| H4 | (b) a tray balloon or toast when a viewer arrives | `windowManager.show(inactive: true)`, plus the tray state; always-on-top stays an owner option (OD-1) | `tray_manager` 0.5.1 has no balloon or notification API, an in-app toast is invisible while the window is hidden, and a notification package is outside D21. |
| H6 | Move the host types into G0, **or** a `p2-host` tag | Types and final `Host` signatures in G0, with adapter bodies; P4 commit 3 after the G1 merges; every v1 viewer↔host test ignored from G0 to G1 (X13) | A `p2-host` tag would serialise P3's helper and P5 behind P2's largest commit. The ignore rule also covers P2's own branch, which runs the v2 host against the v1 viewer and which the critique did not mention. |
| M2 | A Linux test that a wake reaches C in under 5 ms | Under 50 ms out of a 1 s wait | A 5 ms bound flakes on loaded CI runners, and 50 ms still cannot pass with a missed wake. |
| M4 | Choose one of (a) move coalescing, (b) a non-revoking `Busy` with a Dart FIFO, (c) a larger queue | (a) and (c) together: tail-move coalescing, `INPUT_QUEUE` 512 / 256 KiB, `LANE_CAPACITY` 512, and a UI burst bound (`kLatchedTextKeys`, `kTextMaxBytes`) | (b) would bring back the Dart input queue that perf §9.6.3 deletes and change the store's `send` result. The host lane had the same overflow (64 slots, drained only between backend calls), so it grows with the viewer queue. |
| M7 | Pick (a) accept a reconnect, or (b) a foreground service | (a) as the default of owner decision OD-2, measured at G2 | (b) needs a foreground-service type and permission on API 34+, which is the owner's call. |
| M9.3 | Accept the unwipeable key, or hand it from Kotlin to Rust through JNI | Accepted and documented (ui §5.3) | The decrypted vault stays unlocked in Rust memory for the process lifetime, so wiping a Dart copy of the key would not change what a memory reader gets. |
| M10 | The synthetic source behind `cfg(debug_assertions)` or a cargo feature | Cargo feature `desktop-test-source`, fed by a `video_bench --dump` clip | A feature is explicit, can run in release mode for snapshot timing, and is never enabled by a release script. |
| M12 | Put the paused-idle timeout in the perf §3 constants | `kPausedSessionTimeout` in ui §10.1, run by `RemoteDesktop`; perf §9.6.4 points to it | Leaving the picture is a UI flow; the store has no notion of "Back to list", and each constant lives in one table. |
| L6 | `unitTests.isReturnDefaultValues = true` | `VideoFeeder` takes its priority call as a parameter | Returning defaults for every unmocked Android call would hide other mistakes in the JVM tests. |
| L10 | `testWidgets` with `tester.pump`, **or** an owner nod for `fake_async` | `testWidgets` with `tester.pump` | It keeps D21's allow-list intact and drops P7's dependency on P1. |
