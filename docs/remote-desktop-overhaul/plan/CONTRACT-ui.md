# CONTRACT-ui: remote desktop UI, icons and title bar (authoritative)

Status: authoritative implementation contract for BRIEF requirement 2 (remote
desktop UI/UX redesign), requirement 3 (icons) and requirement 4 (title bar),
under DECISIONS D13-D28. It consumes the `DesktopStore` / `DesktopApiClient`
surface of `CONTRACT-perf.md` §9.2-§9.5 **exactly**. Where the UI needs
something that surface lacks, §2.2 asks for it with exact signatures; no perf
member is renamed or re-invented here.

Amended after the adversarial review (2026-11-07). These sections now say
what the merged code does:

- §2.2 (R-UI-1) and §6.2: `DesktopHostView.trustUnreadable`; a trust read
  that fails with `PROTOCOL` (the grant cannot be unlocked, or is invalid)
  counts as loaded, shows its error with Revoke the saved invitation, and is
  never retried on a timer. Any other failed read is retried as before.
  Rename is offered only while sharing.
- §5.2 and §5.3: the vault's `computers_reset` op and `reset()` on the
  vault and the store; `unreadable` (only on `PROTOCOL`) versus `failed`;
  stray last pictures deleted on load; Android resets only on `KEY_GONE`,
  after a new key is wrapped; `computers_unlock` loses `reset`;
  `INVALID_ARGUMENT` from `computers_update` is a refused change; the name
  limit counts runes; `thumbnailImage` replaces `thumbnailVersion`, and a
  replaced record loses its last picture.
- §5.4: `connectNew` gives an unremembered replacement a new id, while a
  later Remember still replaces the confirmed record.
- §6.4 and §6.5: the Clear saved computers note; the featured thumbnail
  uses `thumbnailImage`.
- §6.6: the add dialog only at the rail breakpoint, the sheet below it on
  touch; the scanner reports each distinct code once and keeps running.
- §10.2 and §10.5.2: the delayed drag press reads the cursor before the
  press; one Enter per Return.
- §13.2: the seven strings these amendments add (`remoteVaultUnreadable`, the
  four `remoteVaultReset*`, `hostRenameStartFirst`, `hostRevokeUnreadable`).
- §14.1: T4c added, T5 gains the Return test.
- §18 (K5): the reset runs only on `KEY_GONE`.

Base: `design/ui-interaction.md` (the winner of both judgements). Grafts come
from `design/ui-structure.md`. Every must-fix raised by both judges is
resolved; §1.3 traces each one.

Code citations are at `a41eaa7` and were re-read for this contract. Path
shorthands:

- `U/` = `ui/lib/src/`; `R/` = `ui/lib/src/pages/remote/`;
  `V/` = `ui/lib/src/pages/remote/viewer/`; `T/` = `ui/test/`
- `F/` = `ui/native/pb_mapper_ffi/src/`; `D/` = `crates/pb-mapper-desktop/src/`
- `K/` = `ui/android/app/src/main/kotlin/com/pb_mapper/ui/`
- Artboards are cited as `Main:52`; the design-spec map as `DS §2.6`; the
  RustDesk map as `rd §3.9`; the perf contract as `perf §9.5`.

Normative words: **must** and **never** are contract. Anything left open
belongs to the implementing work package (§17). If a rule is unworkable,
raise it with evidence (DECISIONS preamble); never deviate silently.

---

## 0. The model in one page

1. **Six places.** The rail lists Overview, Tunnels, **Remote desktop**,
   Relay, Logs, Settings. The phone bar lists five; Logs lives in Settings.
2. **The viewer is an app mode, not a route.** `_PbMapperAppState._home`
   returns `RemoteViewerScreen` instead of `AppShell` while
   `ViewerHost.showing`. One viewer exists by construction; the rail is gone
   because the shell is not built; `AppScope` stays reachable.
3. **Management is one page.** The Remote page shows the host card ("this
   computer") and the remembered computers. Each card listens to one
   immutable slice of the store (`hostView`, `session`, `stats`), never the
   whole store, and `AppStore` no longer re-broadcasts desktop changes (D14).
4. **One viewer core, two layouts.** `ViewerStage` (picture, cursor,
   overlays) is shared. Input leaves through one door (`RemoteInput`), touch
   is owned by one deterministic `TouchGestureMachine`, modifiers by one
   `ModifierLatch`, keys by one `KeyRouting`. The desktop layout adds a title
   bar and a floating toolbar; the mobile layout adds gutters, a modifier bar
   and a minimap. The `Texture` element is never re-parented and nothing
   opaque covers it while a frame can still be presented.
5. **Secrets stay put.** The host's own invitation appears only masked on
   the host card (QR on tap, hidden on leave). A viewer invitation crosses
   Dart only when it is pasted or scanned. Remembered computers are stored
   by Rust in an encrypted file and opened **by id**; tokens never return to
   Dart.
6. **D15.** While a remote viewer is connected to this computer, a capsule in
   the title bar offers Disconnect viewer and Stop sharing; a fallback layer
   above the Navigator offers the same wherever no capsule is reachable (no
   title bar, full-screen viewer, setup flow, a dialog's barrier); the tray
   always offers them. Both surfaces live inside the app window, and a
   sharing host's window is usually hidden in the tray, so the tray shows a
   "being viewed" state and the window comes back (without focus) when a
   viewer arrives; this is the default of WORKPLAN owner decision OD-1
   (§4.6).
7. **Chrome.** Lucide weight 400 up to 20 px, 300 from 24 px; sizes 16/18/20/24.
   Caption buttons are 46×32, radius 6, self-drawn 10 px glyphs, 16 px inside
   the sheet edge.

---

## 1. Resolution log

### 1.1 Decisions this contract takes

| # | Decision | Source and evidence |
| --- | --- | --- |
| U1 | Viewer hosted in the shell (`ViewerHost` + `RemoteViewerScreen`), no `MaterialPageRoute`, no `AppBar` | interaction I1, structure S2. Deletes `_viewerOpen`/`_viewerOpenSerial` (`U/pages/desktop_page.dart:289-302`) |
| U2 | On-screen/off-screen and focus are separate signals. On-screen = the perf `viewer` demand (`perf §9.5`, §9.6.4); window blur only releases local input and latches | interaction I2. Today any non-`resumed` state ends control (`U/pages/desktop_viewer.dart:46-54`) |
| U3 | Touch is owned by one **deterministic** `TouchGestureMachine` (explicit timestamps, `nextTimeout`, `timeout(t)`); the surface owns one `Timer`; there is no `ScaleGestureRecognizer` | interaction I3 + structure §2.6.7 (graft). Today's `_scaleUpdate` navigates on `pointerCount` alone (`U/pages/desktop_gesture_surface.dart:302`) |
| U4 | Remote double-click guard: a double-tap-and-hold drag press carries `dragStart`; the viewer defers it to the first click-down + 520 ms (hint) and the **host enforces** it (R-UI-3). Taps stay immediate (D18) | interaction I4 + judge 2 must-fix (one ordered QUIC stream can bunch a retransmitted click with the press) |
| U5 | Desktop viewer has a leading Back (session stays, paused) | interaction I5 / structure deviation 2; Main:118-129 draws "正在查看 · 回到画面" |
| U6 | D15 = width-adaptive title-bar capsule + `SharingFallbackLayer` above the Navigator when no capsule is reachable + tray | interaction I6 + structure §1.5 (graft) + judge 1 must-fix |
| U7 | Collapsed handle expands on click, or hover held 300 ms | interaction I7 |
| U8 | Recognised host shows the short fingerprint | interaction I8, structure deviation 7 (D3 payload has no permission) |
| U9 | Three-finger scroll: one notch per 24 px mean finger travel | interaction I9 |
| U10 | Precision-touchpad two-finger pan scrolls the remote while interactive; pinch zooms locally | interaction I10 (`desktop_gesture_surface.dart:299-320, 358-368`) |
| U11 | Hardware keys: physically held modifiers are real key downs and are never re-wrapped; letters/digits/space with Ctrl/Alt/Win held go as bare keys; latched modifiers wrap; AltGr characters stay text | interaction I11 + structure `route(event, latched)` graft + both judges' must-fixes |
| U12 | The soft keyboard never re-fits the picture | interaction I12, P1-4 |
| U13 | Viewer preferences live in `ViewerPrefs`, not `AppPrefs` | interaction I13 (`U/app.dart:103, 181-187`: every `AppPrefs` change rebuilds `MaterialApp`) |
| U14 | All 93 `desktop*` ARB keys disappear; new keys go in blocks next to the `nav*` and `window*` groups | interaction I14 + structure §6 rule |
| U15 | Remembered computers are stored by Rust (DPAPI on Windows, AES-GCM under an Android-Keystore-wrapped key on Android), listed without secrets, opened by id | structure S6/§3.3 graft + judge 1 must-fix (D20) |
| U16 | Widgets read the perf contract's immutable slices; `hostCardPhase(DesktopHostView)` and `viewerOverlayFor(DesktopSessionView)` are pure | structure §2.1.1/§2.5.5 graft |
| U17 | One `ViewerCard`, one `ViewerPill`, one `ViewerScrim`; one `ViewerMenuEntry` catalogue with a desktop `MenuAnchor` renderer and a touch side-panel renderer | structure S7 graft |
| U18 | Nothing opaque covers the picture while a frame can still be presented, **connecting included**. Opaque grounds only for `identityChanged` and `ended` | judges' must-fix; `desktop_viewer.dart:491-492`; Android credit (`DesktopVideo.kt:166-200`) |
| U19 | No `BackdropFilter` blur under viewer chrome | structure deviation 9 + judge 2 must-fix |
| U20 | Ctrl + wheel stays a **local** zoom, as today | DS §2.12 table "keep"; judge 1 flagged interaction's unlisted change |
| U21 | Phones lock landscape while the viewer is shown (tablets rotate); leaving restores `SystemUiMode.edgeToEdge` and `setPreferredOrientations(const [])` | DS §2.10 proposal (D19); judge 2 graft on `edgeToEdge` |
| U22 | The toolbar is pinned by default | DS §2.6 proposal (D19) |
| U23 | Strings as drawn: desktop pill "未共享", phone row "未在共享", phone remember help separate | DS §2.8, §2.9 (D19) |
| U24 | Mobile modifier bar adds Home, End, PgUp, PgDn, Ins | BRIEF capability table; RustDesk `KeyHelpTools` more row (`remote_page.dart:1049-1075`) |
| U25 | `ComputerPresence.otherRelay`: a computer remembered under another relay address or credential namespace is not shown as "not sharing" | structure §3.3 graft |
| U26 | `RemoteDesktop` holds no `BuildContext`; confirmations live in page helpers (`R/remote_actions.dart`) | judge 2 flaw on interaction §4.4 |
| U27 | The remote cursor is pulled with `readView()` from a post-frame callback chain (rides frames already rendered); no free-running `Ticker` | structure D11 reading (graft) + judge 2 must-fix on structure's Ticker |
| U28 | The toolbar's mode segment track is 36 tall | Viewer:44-53 (track padding 3 around 30-tall segments); judge 1 flaw |

### 1.2 Deviations from the artboards and the design-spec map

| Artboard / DS | This contract | Why |
| --- | --- | --- |
| Toolbar fill `rgba(32,32,35,0.9)` + backdrop blur 20 (Viewer:33, DS §2.6) | same fill, **no blur** (U19) | A `BackdropFilter` over a live `Texture` re-blurs every video frame; the 0.9 fill already hides the picture |
| Viewer has no back control (Viewer:17-27) | leading Back in the title bar and More → "回到电脑列表" (U5) | Back to picture (Main:118-129) presupposes leaving with the session alive; Esc belongs to the remote |
| Recognised host "权限：查看并控制" (AddComputer:46) | short fingerprint (U8) | the invitation carries no permission (D3) |
| Display button glyph (rect with bars) | `AppIcons.remote` (`monitor`) | DS §2.13 deviation |
| Mobile back chevron-left | `AppIcons.back` (arrow-left) | DS §2.13 deviation |
| MobileHome large title (MobileHome:18-23) | shared `PageFrame` bar | D19 |
| Viewer caption buttons 44×36 | 46×32 dark tone, centred in the 36 px bar | D25 |
| Handle expands on hover | click, or hover held 300 ms (U7) | avoids popping the toolbar whenever the pointer sweeps the top edge |
| Camera denied: "打开设置" button (DS §2.9) | no button | no `permission_handler` (D21); paste stays one tap away |
| Modifier bar (MobileKeyboard:30-52) | + Home End PgUp PgDn Ins (U24) | BRIEF capability table |

### 1.3 Must-fix traceability

| Must-fix (judge) | Fixed in |
| --- | --- |
| interaction: connecting overlay opaque (J1, J2) | §7.3 row 4, U18; test T7b |
| interaction: capsule overflows the non-flex title-bar row; absent in full screen, under barriers, on the setup screen (J1) | §4.6 (form by the bar's own `LayoutBuilder` width, `SharingFallbackLayer`); test T3, T1c |
| interaction: Dart vault puts decrypted list and tokens in Dart (J1) | §5.2-§5.3, R-UI-5, R-UI-6 |
| interaction: I4 guard timed on the viewer only (J2) | §10.2, R-UI-3 (host-enforced) |
| interaction: hardware keys ignore latched modifiers (J2) | §10.5.1 rules 2-4; test T4a |
| interaction: blur 20 over the live texture (J2) | §8.3, U19 |
| interaction: no `RemoteTextInput` test (J2) | test T5 |
| structure: `DesktopViewer` reads `AppScope`, breaks `tool/desktop_viewer_acceptance.dart:71-79` (J1, J2) | §8.1 explicit constructor; §17.2 tool edit |
| structure: AltGr becomes Ctrl+Alt chords (J1) | §10.5.1 rule 3 |
| structure: `NavBar` tap dead while Logs shows (J1) | §4.2 |
| structure: double-tap-hold becomes a Windows double-click drag (J1, J2) | §10.2, R-UI-3 |
| structure: direct touch presses before the second pinch finger lands (J1, J2) | §10.2 `kPointerJoinGrace` |
| structure: Disconnect viewer not terminal on the viewer (J1, J2) | perf `DesktopEndReason.disconnectedByHost` is terminal (perf §9.6.2 `host_ended`); §7.3 row 2 |
| structure: unpinned default, no landscape lock, merged strings (J1, D19) | U21, U22, U23 |
| structure: `ScaleGestureRecognizer` as a second owner (J2) | U3, §10.2 |
| structure: chord re-wraps physically held modifiers; host does not count downs (`D/windows.rs:455-475`) (J2) | §10.5.1 rule 3 |
| structure: toolbar hover loop between handle and region (J2) | §10.7 (top margin inside the region + dwell) |
| structure: no focus return after menus and dialogs (J2) | §10.8 |
| structure: soft keyboard cannot reopen; hardware keys edit the sentinel (J2) | §10.5.2 (`TextInputClient`, `show()` = `TextInputConnection.show()`; hardware keys never reach the client) |
| structure: keyboard pan uses the first tiny inset; restore overwrites user navigation (J2) | §10.6 |
| structure: cursor `Ticker` every vsync (J2) | §7.4, U27 |
| structure: accessibility missing (J2) | §10.9 |
| both: no Home/End/PgUp/PgDn on the mobile bar (J1) | §9.4, U24 |

Flaws that were not must-fixes but are fixed anyway: mode segment 36 tall
(U28), Ctrl + wheel stays local (U20), interaction's step 2b (Material icons
replaced in files rewritten later) is dropped (§17), `RemoteDesktop` takes no
`BuildContext` (U26), `ViewerStage` takes listenables (§7.2).

### 1.4 Grafts applied

| Graft | Where |
| --- | --- |
| Immutable slices with `==`; pure `hostCardPhase` and `viewerOverlayFor` | §3, §6.2, §7.3 |
| One `ViewerCard` / `ViewerPill`; `ViewerMenuEntry` lists with two renderers | §7.3, §7.5 |
| Remembered computers opened by id; secrets stay in Rust; Keystore hop inside the FFI vault class | §5.2-§5.4 |
| Builder-layer D15 fallback with `Glass(depth: floating)` styling | §4.6 |
| `ComputerPresence.otherRelay` | §5.2 |
| `DashedBorder` as an `OutlinedBorder`; `InviteScanner.debugSupportedOverride` | §6.4, §6.6 |
| Title-bar geometry test also covers the setup flow; ARB blocks next to `nav*`/`window*` | §14 T6, §13 |
| Deterministic gesture machine | §10.2 |
| Demand handles and the perf poll-rate table | §3 |
| `readView()` pull for the cursor, no `Ticker` | §7.4 |
| No-opaque-cover invariant in every presentable phase; no blur under chrome | §7.3, §8.3 |
| `KeyRouting.route(event, latched)` honouring latches for hardware keys | §10.5.1 |
| Tests: remember only after the first picture; forgetting deletes the thumbnail | §14 T9, T10 |
| `SystemUiMode.edgeToEdge` restore | §9.1 |
| Home/End/PgUp/PgDn/Ins on the modifier bar | §9.4 |

---

## 2. The perf surface this UI consumes, and requests

### 2.1 Consumed (perf §9.4-§9.5; no other store member is used)

| Member (perf name) | Used by |
| --- | --- |
| `hostView: ValueListenable<DesktopHostView>` | `HostCard`, `HostSharingDot`, `SharingIndicator`, `SharingFallbackLayer`, tray |
| `restoreOnStartup()` | `AppStore.start()` (unchanged call site) |
| `enableHost({required int displayId, required bool allowControl, String? service, bool ordinaryOnly = false})` | host setup card |
| `resumeHost()`, `stopHost()`, `allowControl(bool)`, `endViewerSession()`, `setHostDisplay(int)`, `renameHostService(String)`, `revokeTrust()`, `helperAction(String)` (`'install' \| 'disable' \| 'uninstall'`) | host card, capsule, fallback layer, tray |
| `demand(DesktopDemand.page \| .viewer)` → `DesktopDemandHandle.dispose()` | `RemotePage` (page), `ViewerHost` (viewer) |
| `session: ValueListenable<DesktopSessionView>` | overlays, mode segment, `RemoteInput.ready`, list rows, `RemoteDesktop` |
| `geometry: ValueListenable<DesktopGeometry?>` | `RemoteInput` (pixel mapping, `geometryEpoch`), cursor layer, viewport aspect |
| `picture: ValueListenable<DesktopPicture?>` | `ViewerStage` picture (`Texture(textureId)`) |
| `jpegImage: ValueListenable<ui.Image?>` | `JpegPicture` (non-Android/Windows) |
| `stats: ValueListenable<DesktopStats>` (≤ 1 Hz, sampled only while listened to) | toolbar chip, quality panel, gutter quality button, viewing row meta |
| `quality: ValueListenable<DesktopQuality>` | Display menu radios |
| `readView()` | `RemoteCursorLayer` (§7.4) |
| `inspectInvitation(String)` → `DesktopInvitationInfo{service, name, fingerprint, format}` | `AddComputerController` |
| `connect(DesktopTarget)`, `disconnect({String? snapshotPath})`, `capturePicture(String)` | `RemoteDesktop` |
| `retryNow()`, `control(bool)`, `release()`, `requestKeyframe()`, `lockRemote()`, `setQuality(DesktopQuality)` | overlays, toolbar, menus |
| `send(DesktopInput, {required int geometryEpoch})`, `sendSequence(List<DesktopInput>, {required int geometryEpoch})` (≤ 16, atomic, all-or-release) | `RemoteInput` only |
| `DesktopInput` (`move`, `button`, `wheel`, `key`, `text`, `lock`), `RemoteButton`, `RemoteKey` (`U/state/desktop_input.dart`) | `RemoteInput`, `KeyRouting` |
| `DesktopPhase`, `DesktopSetupStep`, `DesktopEndReason`, `DesktopAdaptation`, `DesktopTarget`, `DesktopFailure`, `DesktopQuality` | as named |

The UI never calls `DesktopApiClient` directly, except
`FfiRemoteComputerVault` (§5.3), which uses `PbMapperService.desktopCall`
for the `computers_*` ops (not part of `DesktopApiClient`, perf §9.2).

### 2.2 Requests to the perf contract

Each request is additive, names its owner package, and is needed by a
specific UI behaviour. The UI packages that depend on a request say so (§17).

**R-UI-1 (WP-D, D1 milestone). Host loading and paused state.**

```dart
class DesktopHostView {
  // … perf §9.4 fields unchanged …
  /// False until the first capabilities + host_status round trip completed;
  /// a trust_status read that failed with `PROTOCOL` also counts as loaded.
  final bool loaded;
  /// The remembered grant could not be read: trust_status failed with
  /// `PROTOCOL`, because DPAPI cannot unlock it or it is invalid. `error`
  /// falls back to that read's error. It is re-read on a page visit
  /// (`_loadHost`), never on a timer, and `restoreOnStartup` treats it as
  /// nothing to restore rather than retrying it. Any other failed read
  /// (`IO`) is thrown: the page reads again on its next visit, and the
  /// startup restore retries with backoff.
  final bool trustUnreadable;
  /// A remembered grant **for the current relay and namespace**, or a
  /// configured background host, exists, so sharing can resume without setup:
  /// (trust.saved != null && trust.matchesRelay) || background?.configured == true.
  final bool configured;
  // Invariant: sharing || configured ⇒ invitation != null (perf §9.4 gives the
  // derivation order: local host, background, remembered grant).
}
```

Why: without `loaded` the host card cannot tell loading from unsupported;
without `configured` it cannot tell paused from not enabled (§6.2).

**R-UI-2 (WP-D). Stable viewer start time.**

```dart
class DesktopHostViewer {
  final bool controlling, paused; final String path; final DesktopQuality quality;
  /// Replaces `Duration connectedFor`. Computed once per viewer session from
  /// HostViewer.started_unix_ms (or now - since_ms on first sight); stable.
  final DateTime since;
}
```

Why: `connectedFor` changes on every host poll, so `hostView` would notify
every 1-2 s and every listener (rail dot, tray, capsule) would rebuild. The
UI ticks the displayed duration locally every 30 s.

**R-UI-3 (WP-0 protocol, WP-W worker, WP-HL helper, WP-D input). Host-enforced
double-click guard.**

```rust
// D/lib.rs (perf §4.5), deny_unknown_fields unchanged:
Button { button: Button, down: bool,
         #[serde(default, skip_serializing_if = "std::ops::Not::not")] drag_start: bool },
```

```dart
// U/state/desktop_input.dart:
factory DesktopInput.button(RemoteButton button, bool down, {bool dragStart = false});
// JSON: {"kind":"button","button":"left","down":true,"drag_start":true}; the key is omitted when false.
```

Host rule: **as amended and frozen in WORKPLAN §1.1** ("The C rule"), which
takes precedence. In short:

- Thread C enforces it before the backend call, so the helper needs no rule;
  the position is the last **injected** `Move` in input space.
- An admitted `Button{left, down: true, drag_start: true}` within the
  double-click time and box of the last injected left down moves itself and
  every later lane command, in order, into a C-local hold until
  `t + time + 20 ms`, never longer than `DRAG_GUARD_MAX = 600 ms`. Capture
  continues; `input_wait` (perf §5.12.4) excludes the hold.
- The hold only delays. Held commands carry no authorisation; each runs the
  full `execute` path when released. `Release`, `EndControl`,
  `Control{false}`, a policy or generation change, cancel, a stale
  presentation or a lost freshness drop the whole held queue.
- The double-click time and box come from the `Backend` seam: the ordinary
  host reads them in its own (interactive) session; `RemoteBackend` takes
  them from the helper worker's `Start`/`Attach` response, because the
  background host runs in session 0 (perf §5.16.3). Non-Windows backends
  return none, and the flag is ignored.
- Test (WP-W): `worker::drag_start_press_waits_out_the_double_click_time`
  (timing, order, a down without the flag is never held, and a revoke,
  policy change or stale `Presented` during the hold injects nothing).

Why: Windows turns a second `WM_LBUTTONDOWN` within `GetDoubleClickTime()`
and the `SM_CXDOUBLECLK` box into `WM_LBUTTONDBLCLK`, so Explorer opens the
icon the user meant to drag. Only the host knows when the first click was
injected; a retransmitted first click can arrive bunched with the press.

**R-UI-4 (WP-D). Typed notice.**

```dart
enum DesktopNoticeCode { locked, lockFailed }
class DesktopNotice { final DesktopNoticeCode code; final String message; }   // @immutable, ==
// DesktopSessionView: `final String? notice;` becomes `final DesktopNotice? notice;`
```

Why: the UI localises "remote computer locked" and "couldn't lock: {reason}"
(§7.3 notice pill); keying off English text is forbidden.

**R-UI-5 (WP-F, WP-D). Connect a remembered computer by id (perf I10).**

```dart
class DesktopTarget {
  final String? invitation;   // was String; null when computerId names a remembered computer
  final String name; final String? computerId; final bool relayOnly; final DesktopQuality quality;
  // invariant (assert): invitation != null || computerId != null
}
// DesktopApiClient (perf §9.2):
Future<DesktopSession> connect(String? invitation, {String? computerId, required bool relayOnly,
    required DesktopQuality quality, required double refreshHz});
```

FFI `connect` op (perf §7.2) accepts `computer_id` in place of `invitation`.
Rust resolves it with `crate::desktop_computers::invitation_for(handle, id)
-> Result<Option<desktop::Invitation>, CtlError>`; `None` →
`NOT_FOUND` "remembered computer not found"; a locked or unreadable vault →
`VAULT_UNAVAILABLE`. Both are terminal (perf §9.6.2), never retried. A
reconnect of a target that has `computerId` and no `invitation` resolves
again (the store keeps no token).

**R-UI-6 (WP-F). Vault hooks in WP-F's files.**

- `F/lib.rs`: `mod desktop_computers;` (the file itself belongs to UI-2b).
- `F/desktop.rs`, `pb_mapper_desktop_call_json`: an op name starting with
  `computers_` is routed **before** deserialising into `Request` to
  `desktop_computers::call(handle: &PbMapperHandle, op: &str, request: serde_json::Value) -> Result<Value, CtlError>`
  (an `async fn`, run under the same `block_on`).
- `DesktopState` keeps the decoded invitation of the live viewer session:
  set when `connect_viewer` succeeds, cleared by `disconnect_viewer`:
  `pub(crate) fn current_invitation(&self) -> Option<desktop::Invitation>`.
  `desktop::Invitation` is `Clone` and redacts its token (perf §4.9).

Why: `computers_remember` (§5.3) stores the invitation of the session that
has already passed the TLS pin, so the token never re-enters Dart. Because a
fast switch can deliver a late `live` of the previous computer, the request
also names the expected `service` and short `fingerprint`, and Rust refuses
to store `current_invitation()` under an id when they differ (§5.3).

**R-UI-7 (WP-D). Retry semantics the overlays read.**

- `DesktopSessionView.reconnectAttempt` is > 0 from the first scheduled retry
  until the next `live`, **including** while that retry's connect is in
  flight (phase `connecting`); it resets to 0 on `live` and on a new
  `connect(target)`.
- While a retry is scheduled after `HOST_BUSY`, `failure?.code == 'HOST_BUSY'`.

**R-UI-8 (WP-D). Where a first connect stopped.** When a connect fails
terminally before any picture (`phase == ended`, `hasPicture == false`),
`setup` keeps the step that was current, so the ended card can show it.

**R-UI-9 (WP-D, `T/fake_desktop_api.dart`). Scripting surface UI tests need.**
`FakeDesktopApi` exposes: `final List<(String op, Map<String, Object?> args)> calls`
(every op, including `pause`, `resume`, `input`, `control`, `connect` with
`computerId`, `host_end_session`, `host_stop`); settable `hostSnapshot`
(what `hostStatus()` returns), `viewerStatus` and `view`; `inspectInvitation`
returning info for `pbd1.`-prefixed text (service `desktop:` + the next
segment up to the first non-`[a-z0-9-]`, fingerprint `AB12-CD34-EF56-7890`)
and throwing `DesktopCallException('SETUP_STRING')` for `pbmt1_` text or
base64 JSON setup strings; and `jpegFrames` that can emit scripted encoded
bytes (previews decode PNG through the JPEG path, §15).

**R-UI-10 (WP-D). Local controls never wait behind `busy`.**
`endViewerSession()` and `allowControl(false)` bypass the store's `busy`
gate exactly as `stopHost()` does (perf §9.5 comment), so Disconnect viewer
(D15) and Make view-only (D24) work while an enable, display switch or
helper action is in flight. Test T3 exercises it.

---

## 3. Ownership and notification scoping (D14)

| Concern | Single owner |
| --- | --- |
| Desktop session, host status, media, polling | `DesktopStore` (perf) |
| Remembered computers (records, thumbnails) | `RemoteComputerStore` over `RemoteComputerVault` (§5.2) |
| Which viewer is on screen; open/back/disconnect flows; pending remember | `ViewerHost` + `RemoteDesktop` (§5.4) |
| Viewer UI preferences | `ViewerPrefs` (§5.1) |
| Where D15 controls are reachable | `SharingChrome` (§4.6) |
| Input to the host | `RemoteInput` (§10.4) |
| Touch interpretation | `TouchGestureMachine` (§10.2) |
| Local zoom/pan | `RemoteViewport` (§10.6) |

`AppStore` drops `..addListener(_notify)` on the desktop store
(`U/state/app_store.dart:40-42`; perf §9.6.1 owns that line). Each consumer
listens to the narrowest source:

| Consumer | Listens to | Never listens to | Rate |
| --- | --- | --- | --- |
| Overview, Tunnels, Relay, Logs, Settings, rail foot | `AppStore` (unchanged) | anything desktop | — |
| `HostSharingDot`, `SharingIndicator`, `SharingFallbackLayer`, tray | `desktop.hostView` | session, stats | on change |
| `HostCard` | `desktop.hostView` | session, stats | on change (poll 1 s with the page demand, 2 s while sharing without it) |
| Computer list, featured card | `computers`, `AppStore` (relay state, `desktopServiceNames`), `desktop.session` | hostView | on change; relay ≈ 12 s |
| Viewing row meta, toolbar chip, quality panel, gutter quality | `desktop.stats` | session | ≤ 1 Hz |
| `RemoteViewerScreen`, `DesktopViewer.build`, `TouchViewer.build` | nothing (fullscreen flag, title) | everything | — |
| Picture (`Texture` / `JpegPicture`) | `desktop.picture` / `desktop.jpegImage` | session, stats | per texture / per JPEG frame |
| Overlay layer, mode segment, view-only pill, control hint | `desktop.session` | stats | per transition |
| `RemoteInput.ready` | `desktop.session` (`inputLive`) + `desktop.geometry` | stats | on change |
| Cursor layer | `readView()` pulled post-frame | everything else | per rendered frame |
| Minimap | `viewport.visibleRegion` | store | on zoom/pan |
| Toolbar | `ToolbarController` | store | on expand/collapse |
| Modifier bar, modifier pill | `ModifierLatch` | store | per tap |
| `_PbMapperAppState` | `ViewerHost` (and `AppPrefs` as today) | store | open/close |

Polling follows perf §9.6.1 only: the Remote page holds
`demand(DesktopDemand.page)` while mounted; `ViewerHost` holds
`demand(DesktopDemand.viewer)` while the viewer is on screen; with neither
and no sharing there is no desktop timer.

---

## 4. App shell

### 4.1 `AppPage` (`U/shell/app_scope.dart`)

```dart
enum AppPage {
  overview, tunnels, remote, relay, logs, settings;

  /// The rail, top to bottom (AppRail:23-46).
  static const railEntries = [overview, tunnels, remote, relay, logs, settings];
  /// The bottom bar (MobileTabBar:17-31): Logs lives in Settings there.
  static const barEntries = [overview, tunnels, remote, relay, settings];
  /// The bar entry that stands for this page on a compact window.
  AppPage get barEntry => this == logs ? settings : this;

  IconData get icon;            // remote => AppIcons.remote
  /// [short]: the bar's label ("远程"); the rail and the page title use the long one.
  String label(BuildContext context, {bool short = false});   // remote: short ? navRemote : navRemoteDesktop
  bool get grouped => this == tunnels || this == remote || this == relay || this == settings;
  static AppPage? parse(String? name);   // unchanged; 'remote' is new, old names stay valid
}
```

`AppScope` gains `final RemoteDesktop remote;` and `final SharingChrome
sharing;`, both compared by identity in `updateShouldNotify`. Doc comments
that say "four/five pages" (`U/app.dart:37-38`,
`U/shell/navigation.dart:18,169,223`, `U/shell/app_scope.dart:10`) are
corrected.

### 4.2 Rail and bar (`U/shell/navigation.dart`)

- `NavRail` iterates `AppPage.railEntries`; selection index
  `railEntries.indexOf(page)`. `kRailWidth` 212, rows 38, gap 2 unchanged.
  Rail icons are `AppIconSize.nav` (20) — done in UI-0b.
- `_RailRow` and `_BarItem` gain `Widget? badge`. For `AppPage.remote` the
  badge is `HostSharingDot` (in `navigation.dart`):
  `ValueListenableBuilder<DesktopHostView>(store.desktop.hostView)`;
  `!v.sharing` → nothing; `v.viewer == null` or `v.viewer!.paused` →
  `StatusDot(color: signalColor(scheme), size: 7)`; else
  `PulsingDot(color: signalColor(scheme), size: 7)` (static under
  `Motion.reduced`, which `PulsingDot` already checks). Rail: trailing after
  the label. Bar: `Positioned(top: 3, right: 8)` over the 54×30 pill.
  Semantics value `hostSharing` / `sharingViewerPaused` /
  `sharingBeingViewed` / `sharingBeingControlled`.
- `NavBar` iterates `AppPage.barEntries`:
  `_BarItem(selected: entry == page.barEntry, onTap: entry == page ? null : () => onSelect(entry))`,
  and `_BarItem` uses the `onTap` it is given (today it gates on `selected`,
  `navigation.dart:279`, which would leave Settings dead while Logs shows).
  Top hairline `Border(top: BorderSide(color: scheme.outlineVariant))` (DS §2.2).
- `_RelayFoot`: appends `" · " + l10n.relayLatency(ms)` when online and the
  relay status carries `latencyMs` (`U/ffi/pb_mapper_api.dart:381`). Its doc
  comment is corrected (it opens Relay).

### 4.3 Logs on compact windows

- `PageFrame` gains `VoidCallback? onBack`. When set, the bar starts with
  `IconButton(key: Key('page-back'), icon: Icon(AppIcons.back, size: AppIconSize.button), tooltip: l10n.back)`
  and the title lead drops by 12.
- `LogsPage`: `final compact = !ShellLayout.wideOf(context);` wraps its
  `PageFrame` in `PopScope(canPop: !compact, onPopInvokedWithResult: (didPop, _) { if (!didPop) scope.go(AppPage.settings); })`
  and passes `onBack: compact ? () => scope.go(AppPage.settings) : null`.
- `SettingsPage` (compact only): after the Appearance card, a `GroupCard`
  with one row `Key('settings-logs')`: `AppIcons.logs` 18 · `navLogs` ·
  `settingsLogsHint` · `AppIcons.chevronRight` 16 → `scope.go(AppPage.logs)`.
- A persisted `last_page = 'logs'` on a compact window opens Logs with its
  back button; resizing compact → wide keeps Logs selected and drops the
  back button.

### 4.4 The Remote page in the shell; the `--desktop` hand-off

- `_home`'s switch gains `AppPage.remote => const RemotePage()`.
- `--desktop` (`ui/lib/main.dart:39`, emitted by the helper restart
  hand-off; the file is outside `U/`): `main.dart` passes
  `initialPage: openDesktop ? AppPage.remote : null`.
  `PbMapperApp.initialPage` already overrides `last_page`
  (`U/app.dart:119-121`). **Delete** `openDesktopOnStartup` (`U/app.dart:50,
  74, 124-135`), the import of `pages/desktop_page.dart` (`:6`) and the
  `openDesktopOnStartup` parameter of `pumpApp` (`T/support.dart:34,57`).
- Delete the Tunnels title-bar `open-desktop` button
  (`U/pages/tunnels_page.dart:33-38`); `openDesktop()`
  (`U/pages/desktop_page.dart:9-14`) goes with that file (UI-4).

### 4.5 Hosting the viewer

```dart
// U/app.dart
Widget _home(BuildContext context) {
  …setup / deciding unchanged…
  return AppScope(
    …, remote: _remote, sharing: _sharing,
    child: _viewer.showing
        ? const RemoteViewerScreen()
        : AppShell(page: _page, onSelect: _go, child: switch (_page) { … }),
  );
}
```

- `_PbMapperAppState` owns `ViewerPrefs _viewerPrefs` (loaded in `_boot`
  beside `_prefs.load()`), `ViewerHost _viewer`, `RemoteDesktop _remote`
  (created after `_store`) and `SharingChrome _sharing`;
  `_viewer.addListener(() => setState(() {}))`; all disposed in `dispose`.
  `PbMapperApp` takes `ViewerPrefs? viewerPrefs` for tests.
- No animated switcher between shell and viewer. The viewer fades in with
  `EntranceMotion(offset: 0)`. The Remote page's `ListView` carries
  `PageStorageKey('remote-page')`, so its scroll survives the shell being
  rebuilt.
- `RemoteViewerScreen` (`V/viewer_screen.dart`):
  - reads `AppScope.of(context).remote`;
  - builds `TouchViewer` when `defaultTargetPlatform` is android or iOS,
    else `DesktopViewer` (a Windows touchscreen uses the desktop viewer with
    direct touch, §10.2), both with explicit dependencies (§8.1, §9.1);
  - wraps in `Theme(data: viewerTheme(context))` and
    `MediaQuery.withClampedTextScaling(maxScaleFactor: 1.3)`;
  - `PopScope(canPop: false, onPopInvokedWithResult: (didPop, _) { if (!didPop) remote.back(); })`
    so Android back returns to the list with the session alive (side panels
    and dialogs are routes above it and close first);
  - desktop: `windowManager.setTitle('$title — pb-mapper')` on mount and
    `'pb-mapper'` on dispose (best effort).

### 4.6 D15 sharing controls (`U/shell/sharing_indicator.dart`, new)

Inside the app's one native window, two surfaces that are never both missing
carry the D15 controls, plus the tray outside it. That alone does **not**
meet D15's "always-on-top" while the window is hidden or minimised, which is
the normal state of a sharing host: the close button hides the window to the
tray (`windowManager.close()`), the tray icon has only offline, active and
idle states (`U/common/tray/tray_service.dart:307-315`), and a background
host can share with the app closed. An earlier draft of this section said an
OS-topmost window would need a multi-window plugin; that is incomplete,
because `window_manager` 0.5.1 already offers `show({inactive})` and
`setAlwaysOnTop` on the main window, and `tray_manager` 0.5.1 has no balloon
or notification API.

**Owner decision OD-1 (WORKPLAN §1.3), asked before P8 starts.** P8 builds the
default below unless the owner chooses otherwise; every part stays within
D21:

- (a) **Tray state.** While `hostView.viewer != null` the tray shows the
  existing `tray_active` icon (a state, not an icon redesign) and its tooltip
  leads with `trayHostViewed`, `trayHostControlled` or `trayHostPaused` (§4.7).
- (b) **The window comes back.** When `hostView.viewer` turns non-null while
  the window is hidden or minimised (`windowManager.isVisible()` false or
  `isMinimized()` true), the app calls `windowManager.show(inactive: true)`:
  the window and its title-bar capsule reappear without taking focus from
  the user's work. Once per viewer session; closing the window again is
  respected.
- (c) *Owner option, not built by default:* `setAlwaysOnTop(true)` while a
  viewer is connected, cleared when it leaves.
- **Background hosting with the app closed has no indicator.** The SYSTEM
  service cannot draw in the user's session, and a helper notification is
  out of scope; `docs/remote-desktop.md` and the host card's background row
  say so. When the app runs (tray), (a) and (b) apply to background viewers
  too, because `hostView.viewer` includes `background.host.viewer`.

```dart
/// Where the D15 controls are reachable right now.
final class SharingChrome {
  SharingChrome();
  /// Pass to MaterialApp.navigatorObservers: counts PopupRoutes (dialogs,
  /// bottom sheets, side panels, popup menus) on the root navigator.
  NavigatorObserver get observer;
  /// True when no capsule is reachable: none is mounted and visible, or a
  /// PopupRoute (whose barrier blocks the title bar) is on top.
  ValueListenable<bool> get fallbackNeeded;
  /// A capsule or the host card's viewer panel says whether it shows the
  /// controls. Applied in a post-frame callback (never during build).
  void setReachable(Object owner, bool reachable);
  void dispose();
}

enum SharingIndicatorForm { full, icons, dot }
/// By the bar's own width (LayoutBuilder), never the window's.
SharingIndicatorForm sharingFormFor(double barWidth) =>
    barWidth >= 960 ? SharingIndicatorForm.full
    : barWidth >= 640 ? SharingIndicatorForm.icons
    : SharingIndicatorForm.dot;

class SharingIndicator extends StatefulWidget {
  const SharingIndicator({super.key, required this.form, this.tone = CaptionTone.surface});
  final SharingIndicatorForm form; final CaptionTone tone;
}

class SharingFallbackLayer extends StatefulWidget {   // state: with WindowListener (full screen)
  const SharingFallbackLayer({super.key, required this.desktop, required this.chrome});
  final DesktopStore desktop; final SharingChrome chrome;
}
```

`SharingIndicator` (key `sharing-indicator`):

- Source: `ValueListenableBuilder<DesktopHostView>(scope.store.desktop.hostView)`.
  Visible only while `v.viewer != null` (D15: only while a remote viewer is
  connected); otherwise `SizedBox.shrink()`. It calls
  `chrome.setReachable(this, visible)` on every change and
  `setReachable(this, false)` on dispose.
- `full`: capsule height 30, radius 15, padding `fromSTEB(12, 0, 3, 0)`,
  fill `scheme.tertiaryContainer` (surface tone) / `Color(0x297DB6F2)`
  (viewer tone); `PulsingDot(signalColor, 7)` · 8 · label 12.5/600
  `onTertiaryContainer` (`sharingViewerPaused` while `v.viewer!.paused`,
  with a static dot instead of the pulse, because nothing is captured then;
  else `sharingBeingViewed` / `sharingBeingControlled` by
  `v.viewer!.controlling`) · 8 · two 26-tall text buttons (radius 13,
  12.5/500): `Key('sharing-indicator-disconnect')` `hostDisconnectViewer` →
  `desktop.endViewerSession()`; `Key('sharing-indicator-stop')` `hostStop`
  → `desktop.stopHost()`. About 360 px.
- `icons`: the same capsule without the label; the buttons are 26×26 icon
  buttons (`AppIcons.disconnect`, `AppIcons.screenShareOff`, 16) with the
  same keys and tooltips. About 90 px.
- `dot`: a 30×30 button (`PulsingDot` in a `tertiaryContainer` circle,
  tooltip = the label) opening a `MenuAnchor` with the two actions (same
  keys on the menu items).
- Both actions bypass `busy` (perf `stopHost`; R-UI-10 for `endViewerSession`).
- `Semantics(container: true, liveRegion: true, label: …)`.
- Placement: `_TitleBar` (§12.1) and `ViewerTitleBar` (§8.2) each wrap their
  row in a `LayoutBuilder` and pass `sharingFormFor(constraints.maxWidth)`.
  At the narrowest rail window (720 → sheet 500) the fixed parts are lead 22
  + gap 12 + dot 30 + labelled primary ≈ 140 + 6 + 16 + captions 138 + end 16
  ≈ 380, so the `Expanded` title keeps ≥ 120 px; at 960 (default 1180
  window) the full capsule fits with ≥ 110 px of title. Test T1c guards it.
- `PageFrame` gains `bool sharingIndicator = true`; `RemotePage` passes
  `false` — its host card's `ViewerPanel` carries the same two actions and
  calls `chrome.setReachable(this, true)` while shown (§6.2).

`SharingFallbackLayer` (key `sharing-fallback`):

- `MaterialApp.builder` changes from `Column[banner, Expanded(child)]`
  (`U/app.dart:321-336`) to
  `ToastificationWrapper(child: Stack(children: [Positioned.fill(child: child ?? const SizedBox.shrink()), SharingFallbackLayer(desktop: _store.desktop, chrome: _sharing)]))`,
  and `MaterialApp` gets `navigatorObservers: [_sharing.observer]`. The layer
  is above the Navigator, so it sits above every route, dialog barrier, side
  panel and the viewer.
- Visible iff `hostView.viewer != null && chrome.fallbackNeeded.value`:
  the setup flow and the full-screen viewer (no capsule mounted), and any
  `PopupRoute` on top (a barrier covers the title bar).
- Position: top centre at `top: MediaQuery.paddingOf(context).top + 11`
  (inside the title-bar band); top-left `(12, 12)` while the window is full
  screen (the viewer's toolbar owns the top centre).
- Style: `Glass(depth: GlassDepth.floating)` pill, 36 tall, radius 18,
  padding `fromSTEB(14, 0, 4, 0)`: `PulsingDot` · 8 · label (hidden below
  640 px window width) · 8 ·
  `TextButton(Key('sharing-fallback-disconnect'), hostDisconnectViewer)` ·
  `FilledButton.tonal(Key('sharing-fallback-stop'), hostStop)`. **No
  tooltips and no menus**: the layer is outside the Navigator's `Overlay`.
- `DesktopSharingBanner` and `AppStore.activeDesktop` (its only reader) are
  deleted.
- A host-initiated close is terminal on the viewer side
  (`DesktopEndReason.disconnectedByHost`, perf §9.6.2 `host_ended`), so
  Disconnect viewer is never undone by an automatic reconnect.

### 4.7 Tray (`U/common/tray/tray_service.dart`, `U/app.dart`)

- `TrayStatus` gains `final DesktopHostView? host` (null when not sharing).
- `TrayStrings` gains `hostSharing`, `hostViewed`, `hostControlled`,
  `disconnectViewer`, `stopSharing` (from `trayHostSharing`,
  `trayHostViewed`, `trayHostControlled`, `hostDisconnectViewer`, `hostStop`).
- `initialize(…)` gains `VoidCallback onDisconnectViewer, VoidCallback onStopSharing`.
- Retained `MenuItem`s with stable keys (as `tray_service.dart:86-89`):
  `_hostItem` (`'host'`, disabled status line), `_disconnectViewerItem`
  (`'disconnect-viewer'`), `_stopSharingItem` (`'stop-sharing'`). Menu:
  `[status, if sharing: [hostItem, if viewer: disconnectViewer, stopSharing], separator, open, refresh, separator, quit]`,
  built from the retained instances so Windows' late `WM_COMMAND` still
  resolves (`tray_service.dart:90-91`). `_renderedMenu` gains the host line
  and the two flags.
- Tooltip `'$status · $hostLine'` while sharing; while a viewer is connected
  the host line leads (`'$hostLine · $status'`). The host line is
  `trayHostPaused` while `viewer.paused`, else `trayHostViewed` /
  `trayHostControlled`, else `trayHostSharing`.
- Icon: `_iconFor` returns `tray_active` while `host?.viewer != null` (OD-1
  (a), §4.6); otherwise unchanged. No new icon asset.
- `app.dart`: `_store.desktop.hostView.addListener(_refreshTray)` in addition
  to `_store.addListener(_refreshTray)`; `_trayStatus()` fills `host`; the
  callbacks call `endViewerSession()` / `stopHost()`. The same `hostView`
  listener runs OD-1 (b): on a viewer's arrival while the window is hidden or
  minimised, `windowManager.show(inactive: true)`, once per viewer session.

### 4.8 D26: desktop services leave Tunnels and the galaxy

- `U/state/remote_service.dart`: `bool get isDesktop => name.startsWith('desktop:');`
  on `RemoteService` (mirrors `valid_service`, `D/lib.rs:49-56`).
- `AppStore.remoteServices` (`U/state/app_store.dart:139-148`) skips
  `isDesktop`. That covers the galaxy (`overview_page.dart:42`), the Tunnels
  count/list (`tunnels_page.dart:97,133`) and the editor's suggestions
  (`tunnel_editor.dart:279`). `relayServices` keeps them (the Relay page is
  the relay's own view).
- New `Set<String> get desktopServiceNames` = names of `relayServices` in
  `ownNamespace` with `isDesktop` (D22 online status).
- The Relay page keeps listing `desktop:*` services, but its services tab
  (`U/pages/relay/services_tab.dart:181-189`) offers no tunnel Connect for an
  `isDesktop` row: a TCP tunnel to a desktop service is useless, and D26
  says these services appear in Remote desktop. The row shows no trailing
  action instead.

---

## 5. State

### 5.1 `ViewerPrefs` (`U/state/viewer_prefs.dart`, new, UI-1)

```dart
enum TouchMode { trackpad, direct }
enum ViewerScaleMode { fit, original }

class ViewerPrefs extends ChangeNotifier {
  ViewerPrefs({this.persist = true});
  final bool persist;
  bool get toolbarPinned;           // 'viewer_toolbar_pinned', default true (U22)
  bool get qualityPanel;            // 'viewer_quality_panel', default false
  ViewerScaleMode get scaleMode;    // 'viewer_scale_mode', default fit
  TouchMode get touchMode;          // 'viewer_touch_mode', default trackpad (D18)
  bool get virtualMouse;            // 'viewer_virtual_mouse', default false
  bool get gestureSheetSeen;        // 'viewer_gestures_seen', default false
  Future<void> load();              // SharedPreferences; a failure costs only the choice
  Future<void> setToolbarPinned(bool v); Future<void> setQualityPanel(bool v);
  Future<void> setScaleMode(ViewerScaleMode v); Future<void> setTouchMode(TouchMode v);
  Future<void> setVirtualMouse(bool v); Future<void> setGestureSheetSeen(bool v);
}
```

Same try/catch pattern as `AppPrefs` (`U/shell/app_prefs.dart:44-90`). The
quality preference is per computer (`RemoteComputer.quality`), as RustDesk
keeps it per peer (rd §4.4); an unremembered session's choice is live only.

### 5.2 Remembered computers in Dart (`U/state/remote_computers.dart`, new)

```dart
@immutable
class RemoteComputer {
  final String id;              // 32 lowercase hex chars (128 random bits), generated in Dart
  final String name, service;   // service = "desktop:…"
  final String fingerprint;     // short form "AB12-CD34-EF56-7890" (perf §4.9)
  final String relay;           // AppStore.relayAddress when remembered
  final int namespace;          // AppStore.ownNamespace when remembered
  final bool relayOnly;
  final DesktopQuality quality; // Display-menu choice; default balanced (D12)
  final DateTime? lastConnectedAt; final String? lastPath; final bool? lastControlAllowed;
  // ==/hashCode over all fields
}

sealed class RememberSupport {
  const factory RememberSupport.available() = RememberAvailable;
  const factory RememberSupport.unavailable(String reason) = RememberUnavailable;
}

class RemoteComputerStore extends ChangeNotifier {
  RemoteComputerStore(this._vault, {required Future<Directory> Function() thumbnailDir, math.Random? random});
  RememberSupport get support;                 // unavailable until load() succeeds
  List<RemoteComputer> get computers;          // lastConnectedAt desc (null last), then name
  RemoteComputer? byId(String id);
  RemoteComputer? sameIdentity(DesktopInvitationInfo info);            // service and fingerprint equal
  RemoteComputer? sameServiceOtherIdentity(DesktopInvitationInfo info);
  String newId();                              // Random.secure()
  Future<void> load();                         // vault.open() then vault.list(); resolves the thumbnail dir; idempotent (one in-flight future);
                                               // deletes <id>.thumb files whose id the listed vault no longer holds (D23)
  Future<void> reset();                        // vault.reset(), then load() again: the way out of an unreadable vault
  /// [service] and [fingerprint] name the computer the caller believes is
  /// connected; the vault refuses when the live session is another one (§5.3).
  Future<RemoteComputer?> rememberSession({required String id, required String name,
      required String service, required String fingerprint,
      required String relay, required int namespace, required bool relayOnly,
      required DesktopQuality quality, String? replaceId});   // null on failure; a replaced record with another id
                                                              // loses its last picture (file, cache eviction, version)
  Future<void> rename(String id, String name);
  Future<void> setQuality(String id, DesktopQuality quality);
  Future<void> recordSession(String id, {required DateTime at, String? path, bool? controlAllowed});
  Future<void> forget(String id);              // vault forget, then delete the thumbnail (shares _deleteThumbnail with rememberSession)
  File thumbnailFile(String id);               // <supportDir>/desktop-thumbnails/<id>.thumb; may not exist
  ImageProvider? thumbnailImage(String id, {required int width});
                                               // ResizeImage over a FileImage whose ==/hashCode carry the picture's version,
                                               // so every rewrite is a new cache key at every width; null without a directory
  void thumbnailWritten(String id);            // evicts the provider last handed out for id, bumps the version, notifies
}
```

- At most 50 entries (Rust enforces, §5.3); a full vault makes
  `rememberSession` return null and the caller toasts `remoteRememberFull`.
- `load()` sets the reason `unreadable` only when `list()` fails with
  `PROTOCOL` (the file was read and cannot be decrypted or decoded); any
  other `list()` failure, such as `IO` from a locked file, is `failed`, which
  the next `load()` retries. `unreadable` resolves to
  `remoteVaultUnreadable` with a **Clear saved computers** action
  (`reset()`, behind a destructive confirmation); every other reason,
  `failed` included, still resolves to `addRememberUnavailable`.
- `rememberSession` and `_update` treat `INVALID_ARGUMENT` as a refused
  change: the record stays as it was and the vault stays available. Any
  other vault failure turns `support` into `unavailable(reason)`; nothing is
  ever written in plain text.
- `kComputerNameMax` (128) lives here. The name fields use
  `computerNameLimit` (`R/remote_actions.dart`), which counts runes (Unicode
  scalar values) as Rust's `chars()` does and cuts only between characters.
  It keeps the old value when that value is already at the limit and has a
  collapsed selection, whether or not a keyboard is composing there: that is
  `LengthLimitingTextInputFormatter`'s enforced mode, Flutter's default on
  Android and Windows. When it truncates, it keeps the composing range
  clipped to the kept text, as Flutter's `truncate()` does.
- Owned by `AppStore` as `late final RemoteComputerStore computers`, loaded
  in `start()`. Tests inject `FakeRemoteComputerVault`
  (`T/fake_remote_computer_vault.dart`).

Presence (pure):

```dart
enum ComputerPresence { viewing, online, notSharing, unknown, otherRelay }

ComputerPresence presenceOf(RemoteComputer c, {required DesktopSessionView session,
    required RelayState relay, required String relayAddress, required int ownNamespace,
    required Set<String> desktopServices}) =>
  session.target?.computerId == c.id && session.phase != DesktopPhase.idle
      && session.phase != DesktopPhase.ended                  ? ComputerPresence.viewing
  : c.relay != relayAddress || c.namespace != ownNamespace    ? ComputerPresence.otherRelay
  : relay != RelayState.online                                ? ComputerPresence.unknown
  : desktopServices.contains(c.service)                       ? ComputerPresence.online
  :                                                             ComputerPresence.notSharing;
```

### 5.3 The vault (D20)

**Dart (`U/ffi/remote_computer_vault.dart`, new).** The only Dart code that
talks to the `computers_*` ops and the Keystore channel. No record it returns
holds an invitation or token.

```dart
abstract interface class RemoteComputerVault {
  Future<RememberSupport> open();     // unlocks; the Android Keystore hop happens here
  Future<List<RemoteComputer>> list();
  Future<RemoteComputer> rememberSession({required String id, required String name,
      required String service, required String fingerprint,
      required String relay, required int namespace, required bool relayOnly,
      required DesktopQuality quality, String? replaceId});
  Future<RemoteComputer> update(String id, {String? name, DesktopQuality? quality,
      DateTime? lastConnectedAt, String? lastPath, bool? lastControlAllowed});
  Future<void> forget(String id);
  Future<void> reset();               // computers_reset: deletes the vault file; DPAPI and Keystore alike
}
/// UI-2a ships this returning UnavailableRemoteComputerVault; UI-2b returns FfiRemoteComputerVault.
RemoteComputerVault defaultRemoteComputerVault();
final class UnavailableRemoteComputerVault implements RemoteComputerVault { … }  // open() → unavailable
final class FfiRemoteComputerVault implements RemoteComputerVault {
  FfiRemoteComputerVault(this._service, {MethodChannel keystore = const MethodChannel('pb_mapper/keystore')});
}
```

The `unavailable` reason is resolved by the widgets through
`cannotRememberText`: `remoteVaultUnreadable` for `unreadable` (§5.2),
`addRememberUnavailable` for any other reason. The Rust `reason` text is
only logged.

`FfiRemoteComputerVault.open()`:

1. `computers_support` → `{available, reason?, keystore}`. Not available →
   `unavailable`.
2. If `keystore` (Android): `computers_key` → `{wrapped: b64 | null}`.
   - `null`: `key` = 32 bytes from `Random.secure()`;
     `wrapped = await keystore.invokeMethod('wrap', key)`;
     `computers_unlock{key, wrapped}`.
   - present: `key = await keystore.invokeMethod('unwrap', wrapped)`;
     `computers_unlock{key}`. An unwrap that returns null is an `IO`
     failure, not a reset.
     - A `KEY_GONE` `PlatformException` (the Keystore key is gone or no
       longer opens its wrapped copy, e.g. after a device restore) → generate
       a new key and wrap it, then `computers_reset`, then
       `computers_unlock{key, wrapped}`; the log line
       `Remembered computers reset: <reason>` is written only after the reset
       ran. A wrap that throws or returns null leaves the vault untouched →
       `unavailable(failed)`.
     - Any other `PlatformException` (a busy Keystore included) →
       `unavailable(failed)`, with the vault files untouched; the next load
       tries again.
   - The data key crosses Dart once per process, as base64 inside the JSON
     request, which is an immutable `String`: Dart cannot wipe it, and it
     lives until garbage collection. This is accepted: whoever can read this
     process's memory can read the decrypted vault anyway, because Rust holds
     it unlocked for the process lifetime (below). Tokens never cross Dart.
3. `available()`.

**Rust (`F/desktop_computers.rs`, new; routed per R-UI-6).** Ops over
`pb_mapper_desktop_call_json`:

| Op | Request | `data` | Errors |
| --- | --- | --- | --- |
| `computers_support` | — | `{available: bool, reason?: string, keystore: bool}` (Windows: DPAPI; Android: keystore; else unavailable) | — |
| `computers_key` | — | `{wrapped: b64 \| null}` (Android) | `IO` |
| `computers_unlock` | `key: b64` (32 bytes), `wrapped?: b64` | `{}`; writes `desktop-computers.key` when `wrapped` is given | `INVALID_ARGUMENT` (key length), `IO` |
| `computers_reset` | — | `{}`; drops the cached list and deletes `desktop-computers.dpapi` (Windows) or `desktop-computers.bin` (Android); on Android the unlocked key stays installed and `desktop-computers.key` is kept. Allowed on DPAPI as well as on Keystore | `IO`; `INVALID_ARGUMENT` on unsupported platforms |
| `computers_list` | — | `[record]` | `IO`, `PROTOCOL` (undecryptable) |
| `computers_remember` | `id, name, service, fingerprint, relay, namespace, relay_only, quality, replace_id?` | `record`; stores `DesktopState::current_invitation()` (R-UI-6) encoded `pbd1`, **only if** its `service` and `short_fingerprint()` equal the request's; `replace_id` replaces that entry in place | `INVALID_ARGUMENT` ("no connected session", "the connected session is a different computer", "at most 50 remembered computers"), `IO` |
| `computers_update` | `id, name?, quality?, last_connected_ms?, last_path?, last_control_allowed?` | `record` | `NOT_FOUND`, `INVALID_ARGUMENT` (the record check, e.g. a name of more than 128 Unicode scalar values; the store treats it as a refused change, not a vault failure), `IO` |
| `computers_forget` | `id` | `{}` (idempotent) | `IO` |

- `record` = `{id, name, service, fingerprint, relay, namespace, relay_only,
  quality, last_connected_ms?, last_path?, last_control_allowed?}` — never
  the invitation.
- `pub(crate) async fn invitation_for(handle: &PbMapperHandle, id: &str) -> Result<Option<desktop::Invitation>, CtlError>`
  serves R-UI-5. A locked vault (on Android `computers_unlock` has not run
  yet) or an unreadable file is an error that `connect` maps to the terminal
  `VAULT_UNAVAILABLE` (perf §7.8), never to a retried `IO`.
- File: JSON `{"version":1,"computers":[{…record, "invitation":"pbd1.…"}]}`
  in `desktop_config_dir()` (`F/state/configuration.rs:15`).
  - Windows: DPAPI (`CryptProtectData`, entropy
    `"pb-mapper remembered computers"`, description
    `"pb-mapper remembered computers"`) → `desktop-computers.dpapi`. Reuse
    `F/desktop_trust.rs` `protect`, which today hardcodes the trust entropy
    `b"pb-mapper/desktop-trust/v1"` and description (`:180-214`): it becomes
    `pub(super) fn protect(bytes, encrypt, entropy: &[u8], description: &str)`,
    the trust calls pass their current values (stored trust files still
    decrypt), and `replace` (`:222`) becomes `pub(super)` too.
  - Android: AES-256-GCM (`ring`, already an FFI dependency) with the
    unlocked data key, 12-byte random nonce, AAD `b"pb-mapper/computers/1"`
    → `desktop-computers.bin` = nonce ‖ ciphertext ‖ tag; the wrapped key is
    `desktop-computers.key`.
  - Writes go to a temp file, then
    `pb_mapper_core::durable_file::replace_file` + `sync_parent_directory`.
  - Other platforms: `computers_support` says unavailable; every other op
    fails `INVALID_ARGUMENT` "remembering computers is not supported here".
- The decrypted list lives in a module-level
  `LazyLock<parking_lot::Mutex<Option<Vault>>>` (one process, one vault);
  invitations are `desktop::Invitation` (token zeroed on drop, redacted
  `Debug`). Nothing logs a record's invitation.
- Rust test (UI-2b):
  `desktop_computers::tests::vault_round_trip_hides_secrets_and_rejects_tampering`
  — AES-GCM path with a test key: remember (with a test current invitation)
  → `list` has no `invitation` field → reload decrypts → one flipped byte →
  `PROTOCOL`; `invitation_for` returns the invitation; a remember whose
  `service` or `fingerprint` differs from the current invitation's →
  `INVALID_ARGUMENT` and nothing stored.

**Kotlin (`K/ComputerKeystore.kt`, new).**
`object ComputerKeystore { fun register(messenger: BinaryMessenger) }`
installs MethodChannel `pb_mapper/keystore`:

- `wrap(ByteArray) → ByteArray`, `unwrap(ByteArray) → ByteArray`, both
  `AES/GCM/NoPadding` with the Android Keystore key alias
  `pb_mapper.computers.v1` (`KeyGenParameterSpec`, 256-bit,
  `PURPOSE_ENCRYPT or PURPOSE_DECRYPT`, `setRandomizedEncryptionRequired(true)`,
  no user authentication), created on first `wrap`. Output = IV(12) ‖
  ciphertext ‖ tag.
- Errors → `result.error("KEY_GONE", message, null)` only when no retry could
  ever open the wrapped key: `getKey` returns null,
  `KeyPermanentlyInvalidatedException`, `AEADBadTagException`, or a wrapped
  copy shorter than IV plus tag. Every other failure, a busy Keystore
  included, → `result.error("KEYSTORE", message, null)`.
- `K/MainActivity.kt` `configureFlutterEngine` adds
  `ComputerKeystore.register(flutterEngine.dartExecutor.binaryMessenger)`.

**Android backup exclusions (D20, UI-1).** `<application>` gains
`android:dataExtractionRules="@xml/data_extraction_rules"` and
`android:fullBackupContent="@xml/backup_rules"`. Two directories hold
secrets, in different backup domains:

- **The Rust config dir is not under `getFilesDir()`.** On Android, Dart sends
  `getApplicationDocumentsDirectory()` (`ui/lib/main.dart:93-104`), which
  path_provider maps to `context.getDir("flutter")`, i.e. `app_flutter/`,
  and Rust appends `pb-mapper-ui` (`F/state/configuration.rs:99-107`). That
  directory holds the relay credential (`msgHeaderKey`), the tunnels, the
  vault (`desktop-computers.bin`/`.key`), `desktop-trust.*` and the
  persisted logs. It is in the backup **root** domain.
- `getFilesDir()` holds `getApplicationSupportDirectory()`: the thumbnails.

Both files (new, under `ui/android/app/src/main/res/xml/`) therefore contain

```xml
<exclude domain="root" path="app_flutter/"/>
<exclude domain="file" path="."/>
```

`backup_rules.xml` inside `<full-backup-content>`; `data_extraction_rules.xml`
under both `<cloud-backup>` and `<device-transfer>`. Explicit exclusions are
used rather than `android:allowBackup="false"` alone, because for apps
targeting Android 12+ that attribute does not stop device-to-device transfer.
Shared preferences (theme, viewer prefs) stay backed up. The code above
already answers where the files live, so no on-device "verify the path" step
remains; Stage 3 checks the result (WORKPLAN §4.2 item 5), and the CHANGELOG
says that Android cloud backup no longer restores tunnels or credentials.

### 5.4 `ViewerHost` and `RemoteDesktop` (`U/state/remote_desktop.dart`, new)

```dart
class ViewerHost extends ChangeNotifier {
  ViewerHost(this._desktop);
  bool get showing;
  void open();     // idempotent: holds desktop.demand(DesktopDemand.viewer) (resume, D13); notifies
  void close();    // idempotent: disposes the demand (the store pauses); notifies
  @override void dispose();
}

class RemoteDesktop {
  RemoteDesktop({required this.desktop, required this.computers, required this.viewer,
      required this.prefs, required String Function() relayAddress,
      required int Function() ownNamespace, DateTime Function()? now});
  final DesktopStore desktop; final RemoteComputerStore computers;
  final ViewerHost viewer; final ViewerPrefs prefs;

  /// The title the viewer shows: the remembered name, else the target's name.
  String get title;
  /// Whether opening [c] would end another open session (a switch to confirm).
  bool switchesFrom(RemoteComputer c);
  /// Same computer active → viewer.open(). Otherwise: await computers.load()
  /// (the Android Keystore unlock must precede a connect by id), disconnect the
  /// current session (with its thumbnail), connect DesktopTarget(invitation: null,
  /// computerId: c.id, name: c.name, relayOnly: c.relayOnly, quality: c.quality)
  /// without awaiting (perf §9.6.2: connect supersedes), viewer.open().
  Future<void> open(RemoteComputer c);
  /// result.existing != null → open(existing). Else
  /// id = (result.remember ? result.replaceId : null) ?? computers.newId(), and the
  /// session's quality is looked up from that remember-gated id only, so an
  /// unremembered replacement never writes onto the saved record; connect
  /// DesktopTarget(invitation: result.invitation, computerId: id, …) without
  /// awaiting; viewer.open(); a remember stays pending until the first live picture.
  /// The session target (_Target.replaceId) always carries result.replaceId, so a
  /// later rememberCurrent() on an unremembered replacement calls
  /// rememberSession(id: <new id>, replaceId: <confirmed id>) and replaces that record.
  Future<void> connectNew(AddComputerResult result);
  /// Leave the picture, keep the session: best-effort thumbnail, then viewer.close().
  /// Starts the kPausedSessionTimeout timer (below).
  void back();
  /// End the session: thumbnail, recordSession, desktop.disconnect(snapshotPath:), viewer.close().
  Future<void> disconnect();
  /// From `ended`: desktop.connect(session.target!).
  Future<void> reconnect();
  /// desktop.setQuality(q) (live, D12); if remembered, computers.setQuality(id, q).
  Future<void> setQuality(DesktopQuality q);
  void dispose();
}
```

Rules:

- `RemoteDesktop` keeps the identity of the current target: `service` and
  short `fingerprint` from `AddComputerResult.info` (`connectNew`) or from the
  `RemoteComputer` record (`open`).
- `RemoteDesktop` listens to `desktop.session`.
  - First `phase == live && hasPicture` of a target: if a remember is
    pending for that `computerId` → `computers.rememberSession(id, name,
    service, fingerprint, relay: relayAddress(), namespace: ownNamespace(),
    relayOnly, quality, replaceId)` (D20: after the TLS pin and the first
    picture, never before; the vault refuses if the live session is another
    computer, §5.3); then, for a remembered computer, `recordSession(id, at:
    now, path: session.path, controlAllowed: session.controlAllowed)`.
  - `identityChanged` or `ended` before that drops the pending remember.
  - After `identityChanged`, nothing touches the stored entry or its
    thumbnail; only the explicit "Paste new invitation" path replaces it.
- Thumbnail (D23): for a remembered computer whose session is `fresh` and
  not `identityChanged`: `disconnect()` passes
  `snapshotPath: computers.thumbnailFile(id).path`; `back()` calls
  `desktop.capturePicture(path)` without awaiting, before `viewer.close()`.
  A `true` result → `computers.thumbnailWritten(id)`. Never blocks closing
  (perf bounds both to 300 ms).
- `disconnect()` also calls `recordSession(id, at: now, path: session.path,
  controlAllowed: session.controlAllowed)` for a remembered computer.
- **Paused sessions end.** "Back to list" keeps the session paused, and a
  paused session still holds the host's only slot (perf §5.3), so every other
  viewer would get `HOST_BUSY` until this user returns. `back()` starts a
  `kPausedSessionTimeout` (10 min, §10.1) timer; `viewer.open()`, `open`,
  `connectNew`, `disconnect` and `dispose` cancel it. When it fires,
  `disconnect()` runs; it writes no thumbnail (the session is paused, not
  fresh), because `back()` already wrote one.
- No `BuildContext` here (U26); confirmations live in `R/remote_actions.dart`.

### 5.5 `AppStore` changes (`U/state/app_store.dart`)

- `desktop` without the bridge (WP-D removes the listener line). UI-3
  deletes `activeDesktop` in the same commit that removes the banner, its
  only reader (`U/app.dart:327`); a one-line edit after UI-2a, same track.
- `AppStore({…, RemoteComputerVault? vault})`;
  `late final RemoteComputerStore computers = RemoteComputerStore(vault ?? defaultRemoteComputerVault(), thumbnailDir: getApplicationSupportDirectory)`
  (`path_provider` is already a dependency), loaded in `start()`.
- `remoteServices` filters `isDesktop`; `desktopServiceNames` (§4.8).

---

## 6. The Remote page (`R/`)

### 6.1 `RemotePage` (`R/remote_page.dart`)

```dart
class RemotePage extends StatefulWidget { const RemotePage({super.key}); }
class _RemotePageState extends State<RemotePage> {
  DesktopDemandHandle? _demand;   // store.desktop.demand(DesktopDemand.page) in initState; dispose()
  // initState also: unawaited(store.checkRelay()) — fresh online dots (D22), not forced
}
```

`PageFrame(title: wide ? navRemoteDesktop : navRemote, sharingIndicator: false,
actions: [if (wide && computers.isNotEmpty) IconButton(key: Key('remote-refresh'), icon: Icon(AppIcons.refresh), tooltip: refresh, onPressed: () => store.checkRelay(force: true))],
primary: (computers.isNotEmpty || !wide) ? PrimaryAction(key: Key('remote-add'), icon: AppIcons.add, label: remoteAddComputer, onPressed: () => showAddComputer(context)) : null,
body: …)`. HostSetup hides the header actions while the list is empty
(HostSetup:22-29); compact always shows the add action.

Body (`ListView(key: PageStorageKey('remote-page'))`):

- **Wide** (`ShellLayout.wideOf`): padding `fromLTRB(32, 4, 32, 32)`
  (Main:38). Two columns when the inner width ≥ 1000
  (`Row(crossAxisAlignment: start, [SizedBox(width: 560, child: HostCard()), SizedBox(width: 20), Expanded(child: RemoteComputerList())])`),
  otherwise one centred column, max width 760 (host card above the list).
  The default 1180×780 window gives a 960 sheet, so one column; Main's two
  columns appear from a 1276 px window.
- **Compact**: padding `fromLTRB(16, 4, 16, 24)`: `[HostCard if supported]`,
  `FeaturedComputerCard`, section label `remoteOthers`,
  `CompactComputerList`, `AddComputerButtons`, footer note.
- Host not supported (`v.loaded && !v.supported`): no host card; the list
  goes full width; a note row: `remoteMobileHostNote` (Android/iOS, leading
  `AppIcons.phone` 16) or `hostUnsupported` (macOS/Linux).

The page `build` depends only on layout and whether any computer exists;
each card listens to its own sources (§3).

### 6.2 Host card (`R/host_card.dart`)

```dart
enum HostCardPhase { loading, unsupported, notEnabled, enabling, restarting, sharing, paused }

/// Pure: one host value in, one phase out.
HostCardPhase hostCardPhase(DesktopHostView v) => switch (v) {
  _ when !v.loaded     => HostCardPhase.loading,
  _ when !v.supported  => HostCardPhase.unsupported,
  _ when v.restarting  => HostCardPhase.restarting,
  _ when v.enabling    => HostCardPhase.enabling,
  _ when v.sharing     => HostCardPhase.sharing,
  _ when v.configured  => HostCardPhase.paused,
  _                    => HostCardPhase.notEnabled,
};
```

`HostCard` = `ValueListenableBuilder<DesktopHostView>(desktop.hostView)`.
`v.error` is orthogonal: an inline `scheme.error` line under the header in
every phase, the loading card included. While `v.trustUnreadable`, that error
line (`_HostError`) carries an `scheme.error` outlined
`OutlinedButton.icon(Key('host-error-revoke'), AppIcons.screenShareOff,
hostRevokeUnreadable)` → `confirmDestructive(hostRevokeConfirmTitle,
hostRevokeConfirmBody, hostRevokeAction)` → `revokeTrust()`, and the setup
card's `host-enable` and `host-enable-ordinary` are disabled (enabling reads
the same grant and would fail the same way). Local state of the setup card: `_allowControl = true`
(HostSetup:73), `_displayId = v.displayId ?? v.displays.first.id`.

| Phase | Card | Primary | Transitions |
| --- | --- | --- | --- |
| loading | `GroupCard` header skeleton + 16 px spinner | — | any |
| unsupported | not shown (§6.1) | — | — |
| notEnabled (HostSetup) | grey tile (`AppIcons.remoteLarge` 24 in 44×44 r12 `cardFieldFill`), `hostThisComputer`, `hostOff`, grey pill `remoteNotSharing`; headline `setupHeadline` 22/600; `SetupSteps(active: 1)`; `SwitchListTile(Key('host-allow-control'), hostAllowControl)`; `DisplayPicker`; under the button `TextButton(Key('host-enable-ordinary'), setupOrdinaryOnly)` + note `setupBackgroundNote` | `FilledButton.icon(Key('host-enable'), AppIcons.helper, hostEnable)`, 48 tall, full width, radius 24 | `enableHost(displayId, allowControl, service: v.service ?? defaultServiceName(Platform.localHostname))`; the ordinary link adds `ordinaryOnly: true` |
| enabling | same card; button 18 px spinner + `hostEnabling`, disabled; `SetupSteps(active: 1)` (UAC pending) | — | → restarting / sharing / notEnabled + error |
| restarting | `SetupSteps(active: 2)`; button `hostRestarting` | — | the app quits; the `--desktop` relaunch lands here in `sharing` |
| sharing (Main) | blue tile (`AppIcons.remoteLarge`, `signalColor` on `tertiaryContainer`), `hostThisComputer`, `v.service` mono 12.5 `quietText`, `StatusChip(inkLabel: true, color: successColor, label: hostSharing)`; `ViewerPanel` when `v.viewer != null`; `hostPermissionLabel` + `GlassSegmented<bool>(key: Key('host-permission'), height: 40)` (`hostPermissionView` / `hostPermissionControl`, capsule per D19); `InviteWell`; `InviteQrPanel` when revealed; 48 px rows with hairlines: `hostDisplay` → `DisplayPicker`, `hostBackground` → `hostBackgroundService` / `hostBackgroundApp`, `hostSecure` → helper state; actions row | `OutlinedButton(Key('host-stop'), hostStop)` 40 tall radius 20; `HostMoreMenu`; note `hostStopNote` | Stop → paused; permission → `allowControl(v)` (live, D24); revoke → notEnabled |
| paused | the sharing card without `ViewerPanel`; grey pill `remoteNotSharing`; invitation still shown (R-UI-1) | `FilledButton.icon(Key('host-start'), AppIcons.screenShare, hostStart)` | `resumeHost()` → enabling → sharing |

- `defaultServiceName(String hostname)` = `'desktop:'` + the lowercase
  hostname reduced to `[a-z0-9._-]`, ≤ 120 chars, fallback
  `desktop:computer` (must pass `valid_service`, `D/lib.rs:49-56`).
- `SetupSteps(active)`: three rows (disc 26, title 14.5/600, body 13
  `onSurfaceVariant`); the active disc is ink with a white numeral, the
  others `backdropBase` with an ink numeral; when
  `v.helper == DesktopHelperState.ready`, discs 1-2 show `AppIcons.check` on
  `successColor` (only step 3 remains).
- Helper row values: `ready` → `AppIcons.check` 16 `successColor` +
  `hostHelperReady`; `stopped` → `hostHelperStopped`; `none` →
  `hostHelperNone`; `unverified` → `hostHelperUnverified`; `outdated` →
  `hostHelperUpdate` + inline
  `TextButton(Key('host-helper-update'), hostHelperUpdateAction)` →
  `helperAction('install')` (D2).
- `ViewerPanel` (Main:52-59; `cardFieldFill` r14, padding 14/16):
  `AppIcons.remoteDevices` 20 `signalColor` · title
  `sharingViewerPaused` while `v.viewer!.paused`, else
  `hostViewersControlling(1)` / `hostViewersWatching(1)` 14.5/600 · meta
  `hostViewerMeta(path, duration)` 12.5 (path omitted when unknown) ·
  `OutlinedButton(Key('host-make-view-only'), hostMakeViewOnly)` (only while
  `v.controlAllowed`) → `allowControl(false)` ·
  `TextButton(Key('host-disconnect-viewer'), hostDisconnectViewer)` →
  `endViewerSession()`. A `Timer.periodic(30 s)` refreshes the duration from
  `v.viewer!.since` (`formatSessionDuration`: < 1 min `hostDurationJustNow`,
  < 60 min `hostDurationMinutes`, else `hostDurationHours`). While mounted it
  calls `scope.sharing.setReachable(this, true)` (false on dispose).
- `DisplayPicker` (`Key('host-display')`): a `MenuAnchor` button (not
  `DropdownButtonFormField`) labelled `hostDisplayValue(n, w, h)` (index + 1,
  never the OS name) + `AppIcons.chevronDown` 16. While sharing, choosing
  another display asks `confirmDestructive(context, title: hostDisplaySwitchTitle, body: hostDisplaySwitchBody, action: hostDisplaySwitchAction)`
  then `setHostDisplay(id)` (an allowed resolution change, D5); otherwise the
  choice is local state passed to `enableHost`. While `v.background` the
  picker is disabled with the hint `hostBackgroundStopFirst`: the background
  pipe has no display switch, and the FFI refuses it (perf §5.16.5, §7.2).
- `HostMoreMenu` (`Key('host-more')`, ghost `AppIcons.more` 16 + `more`):
  `host-rename` `hostServiceName` → dialog (suffix field after a fixed
  `desktop:` prefix, `[A-Za-z0-9._:-]{1,120}`; the dialog always warns
  `hostServiceRenameBody`) → `renameHostService('desktop:' + suffix)`;
  enabled only while `v.sharing && !v.background`, since a rename restarts
  the running local host: disabled with `hostRenameStartFirst` as its tooltip
  when not sharing, and with `hostBackgroundStopFirst` (which wins) while
  `v.background` (as the display picker); helper items by `v.helper`: `host-helper-install` (`hostHelperInstall`),
  and when installed `host-helper-disable` (`hostHelperDisable`) and
  `host-helper-uninstall` (`hostHelperUninstall`); divider; `host-revoke`
  (`scheme.error`) → `confirmDestructive(context, title: hostRevokeConfirmTitle, body: hostRevokeConfirmBody, action: hostRevokeAction)`
  (`U/widgets/surfaces.dart:284`) → `revokeTrust()`.

### 6.3 Invite well and QR (`R/host_card.dart`, `R/invite_qr.dart`)

- `InviteWell` (Main:69-83) renders nothing while `v.invitation == null`
  (never a `!`; perf §9.4 makes that unreachable while sharing or
  configured): label `inviteLabel`; 48 tall well
  `cardFieldFill` r14; `maskInvitation(invitation)` mono 13 (`pbd1·` +
  first 4 + 20 × `•` + last 4 of the payload; a JSON v1 invite shows `json·`
  + 24 × `•`); `OutlinedButton.icon(Key('host-copy'), AppIcons.copy 16, copy)`
  → `Clipboard.setData` + toast `hostInviteCopied`;
  `OutlinedButton.icon(Key('host-qr'), AppIcons.qrCode 16, hostQr | hostQrHide)`;
  help `hostInviteHelp` 12.5 `quietText`. The full secret is never rendered
  as text.
- `InviteQrPanel` (`Key('host-qr-panel')`): a white plate in both themes,
  radius 14, padding 16; `QrCode.fromData(data: invitation, errorCorrectLevel: QrErrorCorrectLevel.M)`
  → `QrImage` (package `qr` 3.0.3, D21), drawn by `QrInvitePainter` with a
  4-module quiet zone and modules snapped to whole device pixels
  (`m = ((220 * dpr) / (n + 8)).floorToDouble() / dpr`); caption
  `hostQrCaption`. A ~115-char `pbd1` invite encodes at about version 7-M.
- Visibility is `InviteWell` state `_showQr`, reset in `deactivate()`, by an
  `AppLifecycleListener(onInactive/onHide/onPause → hide)` (never in a
  task-switcher snapshot) and when `invitation` changes. Never logged, never
  persisted.

### 6.4 Remote computer list, wide (`R/computer_list.dart`)

`RemoteComputerList` (Main:112-165): `GroupCard`, header `remoteListTitle`
17/600 + count pill. Listens to
`Listenable.merge([computers, appStore, desktop.session])`. Rows: group 1 =
viewing, online, unknown; a 1 px `outlineVariant` divider inset 12; group 2
= notSharing, otherRelay; inside a group by `lastConnectedAt` desc, then
name. An unremembered or pending session (`session.target != null` and
`computers.byId(target.computerId) == null`) appears first as a transient
viewing row whose menu offers `addRemember` (→ `computers.rememberSession`
for the live session).

`ComputerRow(Key('computer-<id>'))`: padding 12, radius 14, gap 14; tile 40
r11 (`AppIcons.remote` 20; `signalColor` on `tertiaryContainer` when viewing,
`onSurfaceVariant` on `cardFieldFill` online, `onSurfaceMuted` otherwise) ·
name 15/600 + service mono 12 `quietText` · meta 12.5 (fixed 170) · status
pill (fixed 86, `StatusChip(inkLabel: true)`) · action (fixed 96×34, radius
17). Below 600 px row width the meta moves under the service line.

| Presence | Row fill | Meta | Pill | Action |
| --- | --- | --- | --- | --- |
| viewing | `tertiaryContainer @ 0.45` | `remoteViewingMeta(path, rtt, fps)` from `desktop.stats` (nested builder, this row only); `remotePicturePaused(path)` while `session.paused` | `remoteViewing` (`signalColor`) | ink `FilledButton(Key('computer-open-<id>'), remoteBackToPicture)` → `remote.viewer.open()` |
| online | — | `remoteLastPath(when, path)` | `remoteOnline` (success) | ink `remoteOpen` → `openComputer(context, c)` |
| unknown | — | `remoteLast(when)` | none | ink `remoteOpen` (the relay can't say; try) |
| notSharing | — | `remoteLast(when)` | `remoteNotSharing` (grey) | disabled outlined `remoteOpen`, tooltip `remoteOfflineHint` |
| otherRelay | — | `remoteLast(when)` | `remoteOtherRelay` (grey) | disabled outlined `remoteOpen`, tooltip `remoteOtherRelayHint` |

`when` = `formatLastSeen(DateTime, now, locale)` (`remoteToday(HH:mm)`,
`remoteYesterday(HH:mm)`, `DateFormat.MMMd`, `DateFormat.yMd`) in
`R/remote_format.dart`, which also holds `maskInvitation`,
`formatSessionDuration` and `pathLabel` (`'direct'` → `pathDirect`,
`'relay'` → `pathRelay`, else `—`). Row actions (DS §2.3.2 proposal): hover
reveals `IconButton(Key('computer-more-<id>'), AppIcons.more 18)` before the
action button; a secondary click opens the same `MenuAnchor` at the pointer:
`remoteRename` → dialog → `computers.rename`; `remoteRemove` →
`confirmDestructive(context, title: remoteRemoveConfirmTitle(name), body: remoteRemoveConfirmBody, action: remoteRemoveAction)`
→ `computers.forget` (removes the invite and the thumbnail).

`AddComputerTile` (Main:159-163, `Key('remote-add-tile')`): 56 tall,
`Material(shape: DashedBorder(…))` + `InkWell` (1.5 px `outline`, dash 4/3,
r14), `AppIcons.add` 18 + `remoteAddComputer` 14/500 + `remoteAddHint` 13
`quietText` → `showAddComputer`. Footnote `remoteRememberNote`, or
`addRememberUnavailable` when the vault is unavailable, or, when it is
`unreadable` (§5.2), `remoteVaultUnreadable` with a destructive
**Clear saved computers** (`remoteVaultReset`) → `confirmDestructive(
remoteVaultResetTitle, remoteVaultResetBody, remoteVaultResetAction)` →
`computers.reset()`. The same note shows on `RemoteEmptyCard` and on the
compact list (§6.5), and the add form uses the same wording.

`RemoteEmptyCard` (no computers and no session; HostSetup:96-115): min
height 520; `RemoteEmptyIllustration` (`CustomPainter`, 120×84, strokes per
DS §2.4); `remoteEmptyTitle` 16/600; `remoteEmptyBody`; outlined
`Key('remote-add-empty')` `remoteAddComputer`.

### 6.5 Remote tab, compact (`R/computer_list_compact.dart`, MobileHome)

- `FeaturedComputerCard(Key('computer-featured'))`: the viewing computer,
  else the most recently connected. Thumbnail 16:9
  `Image(image: computers.thumbnailImage(id, width: (w * dpr).round()), gaplessPlayback: true, errorBuilder: placeholder, frameBuilder: …)`
  (no `ValueKey` and no `cacheWidth`: the provider's cache key carries the
  picture's version, §5.2);
  placeholder `#1F2328` + `AppIcons.remoteLarge` 24 `onSurfaceMuted`; badge
  `remoteLastPicture(when)`; name 17/600; sub `remotePathPermission(path)` /
  `remotePathViewOnly(path)` (last path and permission); status pill;
  full-width 46-tall ink button `Key('computer-featured-open')` `remoteOpen`
  / `remoteBackToPicture`.
- `CompactComputerList`: a `RaisedSurface` of
  `CompactComputerRow(Key('computer-<id>'))`, dividers inset 68: 64 tall,
  tile 40, name 15/600, sub with a 6 px dot (`remoteOnlineLast(path)` /
  `remoteOfflineLast(when)` / `remoteLast(when)` / `remoteOtherRelay`),
  `AppIcons.chevronRight` 18. Tap → `openComputer` (notSharing → toast
  `remoteOfflineToast(name)`; otherRelay → toast `remoteOtherRelayHint`).
  Long press → bottom sheet (rename / remove).
- `AddComputerButtons`: when `InviteScanner.supported`, two `DashedBorder`
  buttons 52 tall r16: `Key('remote-scan')` `AppIcons.scanQrCode` 18
  `remoteScanAdd` → `showAddComputer(context, mode: AddMode.scan)`;
  `Key('remote-paste')` `AppIcons.paste` 18 `remotePasteInvite` →
  `AddMode.paste` (reads the clipboard at once). Elsewhere one
  `AddComputerTile`.
- Empty (DS proposal): only the buttons and the note.

### 6.6 Add computer (`R/add_computer.dart`, `R/invite_scanner.dart`)

```dart
enum AddMode { auto, scan, paste }
Future<void> showAddComputer(BuildContext context, {AddMode mode = AddMode.auto, RemoteComputer? replacing});

@immutable
class AddComputerResult {
  final String invitation; final DesktopInvitationInfo info; final String name;
  final bool remember, relayOnly; final String? replaceId;
  final RemoteComputer? existing;     // duplicate: Connect opens it instead
}

enum AddState { empty, inspecting, recognised, invalid }
enum InviteRejection { invalid, setupString }
class AddComputerController extends ChangeNotifier {
  AddComputerController({required Future<DesktopInvitationInfo> Function(String) inspect,
      required RemoteComputerStore computers, RemoteComputer? replacing});
  AddState get state; DesktopInvitationInfo? get info; InviteRejection? get rejection;
  RemoteComputer? get duplicate;   // same service + fingerprint
  RemoteComputer? get conflict;    // same service, other fingerprint
  bool replaceConfirmed; String name; bool remember; bool relayOnly;   // remember defaults to support.available
  bool get otherComputer;          // replacing != null && info.service != replacing.service
  void edit(String text);          // debounced 250 ms
  void paste(String text);         // inspects at once
  void clear();
  AddComputerResult? result();     // null unless recognised && (conflict == null || replaceConfirmed),
                                   // whatever Remember is; with Remember off the confirmation takes
                                   // effect through a later Remember of the session (§5.4)
}
```

- `showAddComputer`: non-touch platform or `usesRail(context)` (the shell's
  own breakpoint: touch ≥ 900) →
  `showDialog(barrierColor: Color(0x52141416), builder: (_) => AddComputerDialog(…))`
  (560 wide, r24, padding 24/28/28; DS §2.5). Touch below the rail →
  `showModalBottomSheet(isScrollControlled: true, useSafeArea: true, builder: (_) => AddComputerSheet(…))`,
  which honours `AddMode.scan` / `AddMode.paste`; so a phone in landscape
  (720-899) also gets the sheet, scanner first, from the viewer's Paste new
  invitation.
  Both pop an `AddComputerResult`; the caller runs
  `connectFromAdd(context, result)` (§6.7). The dialog closes at once; the
  viewer opens on the Connecting overlay — no spinner inside the dialog.
- The raw invitation is a private field of the controller; widgets never read
  it. Each inspect increments a serial; stale replies are dropped.
  `SETUP_STRING` → `addSetupString`; any other failure → `addInvalid`. The
  default name is the service without `desktop:`; a duplicate takes the
  saved name. `replacing` (from the identity overlay) prefills the name and
  sets `replaceId = replacing.id` when the service matches; a different
  service shows `addOtherComputer` and is added as new.
- `InviteField`: editing → `TextField(key: Key('add-invite'), minLines: 3, maxLines: 3, style: mono 13, autocorrect: false, enableSuggestions: false, enableIMEPersonalizedLearning: false, smartDashesType: SmartDashesType.disabled, smartQuotesType: SmartQuotesType.disabled)`
  + inline `OutlinedButton.icon(Key('add-paste'), AppIcons.paste 16, paste)`;
  recognised → a read-only well with `maskInvitation(raw)` +
  `IconButton(Key('add-invite-clear'), AppIcons.close)`. Errors under the
  field in `scheme.error`.
- `RecognisedHostCard(Key('add-recognised'))`: `tertiaryContainer @ 0.45`,
  r14: `AppIcons.helper` 20 `signalColor` · service mono 13 ·
  `addRecognized(fingerprint)` 12.5. `duplicate` → `addDuplicate(name)`.
  `conflict` → `addIdentityConflict(name)` +
  `SwitchListTile(Key('add-replace'), addReplaceConfirm)`.
- Name `TextField(Key('add-name'), addName)`;
  `SwitchListTile(Key('add-remember'), addRemember, addRememberHelp | addRememberHelpMobile (Android/iOS))`,
  disabled with `cannotRememberText(support)` as its subtitle when the vault
  is unavailable (D20): `remoteVaultUnreadable` when it is `unreadable`,
  else `addRememberUnavailable`; disclosure `Key('add-more')` `addMoreOptions` →
  `SwitchListTile(Key('add-relay-only'), remoteRelayOnly)`.
- Footer (dialog): `TextButton(cancel)`,
  `FilledButton(Key('add-connect'), connect)` enabled iff `result() != null`.
- `AddComputerSheet` (MobileAdd): grabber, title `addTitle`, close; in scan
  mode (`InviteScanner.supported` and mode ≠ paste) a 330-tall
  `InviteScanner`; a divider with `scanOr`; full-width 48
  `OutlinedButton.icon(Key('add-paste'))`; once recognised the viewfinder is
  replaced by `RecognisedHostCard` + name + full-width `add-connect`; the
  switches stay visible in both phases; footer `cameraNote`.

```dart
/// Live camera QR reading. Android/iOS only (mobile_scanner 7.5.0, D21):
/// QR only, no duplicates, returnImage false — no frame bytes reach Dart.
class InviteScanner extends StatefulWidget {
  const InviteScanner({super.key, required this.onDetect});
  final ValueChanged<String> onDetect;
  static bool get supported => debugSupportedOverride ?? (Platform.isAndroid || Platform.isIOS);
  @visibleForTesting static bool? debugSupportedOverride;
  @visibleForTesting static bool debugFakeCamera = false;   // previews: static viewfinder, no plugin
}
```

`MobileScanner(controller: MobileScannerController(formats: [BarcodeFormat.qrCode], detectionSpeed: DetectionSpeed.noDuplicates, returnImage: false))`;
`onDetect` → each distinct `rawValue` once (a Dart-side last-reported
filter, so a rejected code left in view, also after a camera restart that
resets mobile_scanner's own duplicate memory, is not re-inspected in a loop)
→ `onDetect(value)` → `AddComputerController.paste(value)` (never
auto-connects). The camera keeps running after a read; the sheet unmounts
the scanner once the invitation is recognised, which disposes the
controller. Viewfinder 330
tall r20 `#161618`; reticle painter (220×220, four 28×28 L-corners 3 px white
r10); scan line 2 px `ViewerPalette.signal` animating top↔bottom over 1.6 s,
static under `Motion.reduced`; hint pill `scanHint`. `errorBuilder`:
`MobileScannerErrorCode.permissionDenied` → `AppIcons.scanQrCodeLarge` 24 +
`scanPermissionDenied`; unsupported / no camera → `scanUnavailable`. An
`AppLifecycleListener` stops the controller on any state but `resumed` and
restarts it on `resumed`, unconditionally; it is disposed with the sheet. Tests set
`debugSupportedOverride = false`.

### 6.7 Page actions (`R/remote_actions.dart`)

```dart
/// Opens a remembered computer, confirming a switch away from another open session.
Future<void> openComputer(BuildContext context, RemoteComputer c);
/// Runs an AddComputerResult, confirming a switch first when needed.
Future<void> connectFromAdd(BuildContext context, AddComputerResult r);
```

A switch asks `confirmDestructive(context, title: remoteSwitchTitle(currentName),
body: remoteSwitchBody, action: remoteSwitchAction)`; on confirm →
`remote.open(c)` / `remote.connectNew(r)`. `remote.switchesFrom(c)` decides
whether to ask. These are the only places that combine a `BuildContext` with
`RemoteDesktop`; a failed `rememberSession` (null) toasts
`remoteRememberFull` or the vault's unavailability via a listener on
`computers.support`.

---

## 7. Viewer: shared core (`V/`)

### 7.1 File map

| File | Contents | WP |
| --- | --- | --- |
| `V/viewer_screen.dart` | `RemoteViewerScreen` (§4.5) | UI-3 (transitional), rewritten by UI-6, one line by UI-7 |
| `V/touch_gestures.dart` | `TouchGestureMachine`, `RemotePointerSink`, `HapticKind`, gesture constants | UI-5 |
| `V/remote_viewport.dart` | `RemoteViewport` | UI-5 |
| `V/remote_surface.dart` | `RemoteSurface` (Listener, one `Timer`, `Transform`), `TouchIndicator` | UI-5 |
| `V/remote_input.dart` | `RemoteInput` | UI-5 |
| `V/modifier_latch.dart` | `ModifierLatch`, `LatchState` | UI-5 |
| `V/remote_keys.dart` | `KeyRouting`, `RemoteShortcut` | UI-5 |
| `V/remote_text_input.dart` | `RemoteTextInput` (IME client) | UI-5 |
| `V/viewer_stage.dart` | `ViewerStage` | UI-6 |
| `V/viewer_overlays.dart` | `ViewerOverlayKind`, `ViewerOverlay`, `viewerOverlayFor`, `showsViewOnly`, `ViewerCard`, `ViewerPill`, `ViewerScrim`, `GreyStill`, `ViewerOverlayLayer` | UI-6 |
| `V/remote_cursor_layer.dart` | `RemoteCursorLayer`, `RemoteCursorPainter` (today's `DesktopCursorPainter`, 18×27) | UI-6 |
| `V/jpeg_picture.dart` | `JpegPicture` (paints `desktop.jpegImage`) | UI-6 |
| `V/viewer_menus.dart` | `ViewerMenuEntry` family, `ViewerActions`, `displayMenu`, `shortcutsMenu`, `moreMenu`, `ViewerMenuButton`, `TypeTextDialog` | UI-6 |
| `V/quality_panel.dart` | `QualityPanel` | UI-6 |
| `V/desktop_viewer.dart` | `DesktopViewer` (replaces `U/pages/desktop_viewer.dart`) | UI-6 |
| `V/viewer_title_bar.dart` | `ViewerTitleBar` | UI-6 |
| `V/viewer_toolbar.dart` | `ViewerToolbar`, `ToolbarController`, `ToolbarHandle`, `ToolbarStatusChip` | UI-6 |
| `V/touch_viewer.dart` | `TouchViewer`, `ComposingPreview`, `ModeHint` | UI-7 |
| `V/touch_controls.dart` | `GutterLayout`, `gutterLayoutFor`, `GutterButton`, `QualityGutterButton` | UI-7 |
| `V/side_panel.dart` | `showViewerSidePanel` (touch renderer of `ViewerMenuEntry`) | UI-7 |
| `V/modifier_bar.dart` | `ModifierBar`, `KeyCap` | UI-7 |
| `V/minimap.dart` | `Minimap`, `MinimapPainter` | UI-7 |
| `V/virtual_mouse.dart` | `VirtualMousePad` | UI-7 |
| `V/gesture_sheet.dart` | `GestureSheet`, `GestureGlyph`, `GestureGlyphPainter` | UI-7 |
| `U/theme/viewer_palette.dart` | `ViewerPalette`, `viewerTheme()` | UI-1 |

Deleted (UI-6): `U/pages/desktop_viewer.dart`,
`U/pages/desktop_gesture_surface.dart`,
`U/pages/desktop_connection_banner.dart`.

### 7.2 `ViewerStage` (`V/viewer_stage.dart`)

```dart
class ViewerStage extends StatelessWidget {
  /// Takes listenables, not the store, so a widget test drives phases with ValueNotifiers.
  const ViewerStage({super.key, required this.session, required this.geometry,
      required this.picture, required this.jpegImage, required this.stats,
      required this.readView, required this.viewport, required this.input,
      required this.machine, required this.touchMode, required this.title,
      required this.actions, this.compact = false, this.toolbarInset});
  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopGeometry?> geometry;
  final ValueListenable<DesktopPicture?> picture;
  final ValueListenable<ui.Image?> jpegImage;
  final ValueListenable<DesktopStats> stats;
  final DesktopView? Function() readView;
  final RemoteViewport viewport; final RemoteInput input; final TouchGestureMachine machine;
  final ValueListenable<TouchMode> touchMode; final String title;
  final ViewerActions actions;               // §7.5
  final bool compact;                        // mobile: cards max width = picture width − 32
  final ValueListenable<double>? toolbarInset; // desktop: top offset for top pills (68 expanded / 28 collapsed)
  factory ViewerStage.of(DesktopStore desktop, {…});
}
```

```
Stack(fit: expand)[
  RemoteSurface(viewport, input, machine, touchMode,                    // Listener + Transform(key 'remote-scene-transform')
    child: RepaintBoundary(child: ValueListenableBuilder(picture →
        textureId != null ? Texture(key: ValueKey(textureId), textureId: textureId, filterQuality: FilterQuality.low)
                          : JpegPicture(jpegImage))))
  GreyStill(viewport)               // only for retryWaiting / reconnecting with hasPicture; sibling, never a parent
  RemoteCursorLayer(readView, geometry, viewport, input)
  TouchIndicator(machine)           // trackpad only; 44 circle, fades 150 ms
  ViewerOverlayLayer(session, stats, title, actions, compact, toolbarInset)   // §7.3
]
```

The picture element is the same across zoom, pan, overlays, reconnect
attempts (perf re-binds the texture, never re-creates it) and the soft
keyboard; test T2 asserts it. Nothing that changes at media or state rate
rebuilds above the `Texture`.

### 7.3 Overlays (`V/viewer_overlays.dart`)

```dart
enum ViewerOverlayKind { none, connecting, connectRetry, retryWaiting, reconnecting,
    secureDesktop, waitingDesktop, recovering, identityChanged, ended }

@immutable
class ViewerOverlay {
  final ViewerOverlayKind kind;
  final bool opaque;      // only identityChanged and ended
  final bool greyStill;   // retryWaiting / reconnecting with a picture
}

/// Pure; first match wins.
ViewerOverlay viewerOverlayFor(DesktopSessionView s);
bool showsViewOnly(DesktopSessionView s) => s.target != null && !s.controlAllowed
    && (s.phase == DesktopPhase.live || s.phase == DesktopPhase.recovering
        || s.phase == DesktopPhase.waitingForDesktop);
```

| # | Condition | Kind | Ground and hit testing | Content | Actions |
| --- | --- | --- | --- | --- | --- |
| 1 | `phase == identityChanged` | identityChanged | **opaque** `ViewerScrim.opaque()` (the session is dead); absorbs | `ViewerCard(width: 380, alert: true)`: `AppIcons.warning` `ViewerPalette.danger`, `viewerIdentityTitle`, `viewerIdentityBody` | `viewerDisconnect` → `onDisconnect`; `viewerPasteNewInvite` → `onPasteNewInvite` (hidden when null) |
| 2 | `phase == ended` | ended | **opaque**; absorbs | `ViewerCard(320)`: `viewerEndedTitle` + body by `s.end`: `hostStopped` → `viewerEndedStopped`, `disconnectedByHost` → `viewerEndedByHost`, `revoked` → `viewerEndedRevoked`, `versionMismatch` → `viewerEndedVersion`, `denied` → `viewerDenied`, `invalidInvitation` → `addInvalid`, `decoderFailed` → `viewerEndedDecoder`, `failed` with `s.failure?.code == 'VAULT_UNAVAILABLE'` or `'NOT_FOUND'` → `viewerVaultUnavailable`, other `failed` → `s.failure?.message ?? viewerEndedFailed`. With `!s.hasPicture` the three connect steps show, the one in `s.setup` marked failed (R-UI-8) | `back` → `onDisconnect`; `viewerReconnect` → `onReconnect` (hidden for `versionMismatch`, `denied`, `revoked`, `invalidInvitation`; `decoderFailed` shows `retry` → `desktop.retryNow()`); `viewerPasteNewInvite` for `denied`/`revoked` when `onPasteNewInvite != null` |
| 3 | `phase == reconnecting` (`retryAt != null`) | `hasPicture` ? retryWaiting : connectRetry | retryWaiting: `GreyStill`; the pill absorbs, the rest passes (local zoom/pan stays). connectRetry: `ViewerScrim.translucent()` in `IgnorePointer`; the card absorbs | retryWaiting: `ViewerPill(height: 48)` `viewerRetry(attempt, seconds)` (seconds from `retryAt` by a local 1 s `Timer`) · secondary `viewerInputPaused` (or `viewerBusy` when `failure?.code == 'HOST_BUSY'`, R-UI-7). connectRetry: the connecting card with that line instead of the steps | `viewerRetryNow` → `desktop.retryNow()`; connectRetry also `cancel` → `onDisconnect` |
| 4 | `phase == connecting && reconnectAttempt > 0` | `hasPicture` ? reconnecting : connectRetry | as 3 | reconnecting: `ViewerPill(height: 48)` spinner 16 + `viewerReconnecting(attempt)` · `viewerInputPaused` | connectRetry: `cancel` |
| 5 | `phase == connecting` | connecting | **translucent** `ViewerScrim.translucent()` (alpha 0.72) in `IgnorePointer`; the card absorbs. The first frame is presented in this phase (perf §9.6.2), and Android returns presentation credit only after Flutter consumes the texture (`DesktopVideo.kt:166-200`) | `ViewerCard(320)`: spinner 20, `viewerConnecting(name)`, `ConnectSteps(s.setup, s.path)`: row 1 `viewerStepFind` while `finding`, then `viewerStepFoundDirect` / `viewerStepFoundRelay` by `s.path`; row 2 `viewerStepIdentity` (`verifying` and `authorizing`); row 3 `viewerStepFirstFrame` (`firstPicture`); done ✓ / current dot / pending dot | `cancel` → `onDisconnect` |
| 6 | `phase == waitingForDesktop` | `s.secureDesktop` ? secureDesktop : waitingDesktop | `ViewerScrim.translucent()` in `IgnorePointer`; the card absorbs | `ViewerCard(360)`: `AppIcons.secureDesktop` `ViewerPalette.caution`, `viewerSecureTitle`/`viewerSecureBody` (or `viewerWaitingTitle`/`viewerWaitingBody`), chip `viewerInputPaused` | — |
| 7 | `phase == recovering \|\| phase == paused` | recovering | none; `IgnorePointer` | `ViewerPill(32)` spinner 16 + `viewerRecovering`, top centre at `toolbarInset` (desktop) / 12 (mobile) | — |
| 8 | otherwise (`live`, `idle`) | none | — | — | — |

Independent of the table, at fixed anchors:

- `ViewerPill(AppIcons.viewOnly 16, viewerViewOnlyByHost)` while
  `showsViewOnly(s)` (bottom 34, left 12; Key `viewer-view-only`).
- Degraded pill (`ViewerPill(34)`, dot `ViewerPalette.caution` +
  `viewerSlow(path)` + mono `"{pictureLatencyMs} ms"`) while the toolbar is
  collapsed, `phase == live` and `stats.degraded` (top 28, centred).
- Notice pill while `s.notice != null` (R-UI-4): `locked` →
  `shortcutLocked`; `lockFailed` → `shortcutLockFailed(message)`; bottom
  64, centred; replaces the control hint while shown.
- Control hint (§8.4).

Widgets:

```dart
class ViewerCard extends StatelessWidget {        // r16, ViewerPalette.card, 1 px edge, padding 20/22
  const ViewerCard({super.key, required this.title, this.leading, this.body,
      this.children = const [], this.actions = const [], this.width = 320, this.alert = false});
}
class ViewerPill extends StatelessWidget {        // ViewerPalette.hint fill, 1 px edge, radius height/2
  const ViewerPill({super.key, required this.text, this.leading, this.secondary,
      this.trailing, this.height = 32, this.tint});
}
class ViewerScrim extends StatelessWidget {
  const ViewerScrim.translucent({super.key}) : opaque = false;   // Color(0xB80F0F10), alpha 0.72
  const ViewerScrim.opaque({super.key}) : opaque = true;         // ViewerPalette.ground
  final bool opaque;
}
```

Invariant: in rows 3-7 nothing opaque covers the picture and no overlay is
ever the picture's parent (`desktop_viewer.dart:491-492`). Every card and
pill is `Semantics(liveRegion: true)`. Overlay keys:
`viewer-overlay-<kind>`.

### 7.4 Remote cursor (`V/remote_cursor_layer.dart`, D11)

```dart
class RemoteCursorLayer extends StatefulWidget {
  const RemoteCursorLayer({super.key, required this.readView, required this.geometry,
      required this.viewport, required this.input});
  final DesktopView? Function() readView; final ValueListenable<DesktopGeometry?> geometry;
  final RemoteViewport viewport; final RemoteInput input;
}
```

- The state keeps a post-frame callback chain while mounted:
  `SchedulerBinding.instance.addPostFrameCallback(_onFrame)`, and `_onFrame`
  re-adds itself. A post-frame callback never schedules a frame, so the
  chain rides frames that are rendered anyway; when the cursor moves while
  nothing else renders, the store's 33 ms view timer schedules one (perf
  §9.6.1). No `Ticker`.
- `_onFrame` reads `readView()` (non-blocking leaf). If `cursorSeq`,
  position or `cursorVisible` changed **and** `view.generation ==
  geometry.value?.generation`, it stores the sample and notifies its
  `_repaint` `ChangeNotifier`.
- `CustomPaint(painter: RemoteCursorPainter(repaint: Listenable.merge([_repaint, viewport, input.cursorChanges])))`
  inside `IgnorePointer`; constant size 18×27 whatever the zoom.
- It draws `input.cursor` (the local prediction) while a trackpad stroke runs
  or for `kCursorEchoWindow` (300 ms) after any local move (the host stops
  echoing to the mover for 300 ms, D11); otherwise the remote sample
  (`cursorX/Y ÷ displayWidth/Height`), hidden when `!cursorVisible`.
- The local OS pointer stays visible on desktop.

### 7.5 Menus and shortcuts (`V/viewer_menus.dart`, `V/remote_keys.dart`)

```dart
sealed class ViewerMenuEntry { const ViewerMenuEntry(); }
final class ViewerMenuSection extends ViewerMenuEntry { const ViewerMenuSection(this.label); final String label; }
final class ViewerMenuDivider extends ViewerMenuEntry { const ViewerMenuDivider(); }
final class ViewerMenuItem extends ViewerMenuEntry {
  const ViewerMenuItem({required this.key, required this.label, this.hint, this.icon,
      this.enabled = true, this.checked, this.danger = false, this.disabledHint, required this.onSelected});
  final Key key; final String label;
  final String? hint;          // mono key caps, e.g. "Win + D"
  final IconData? icon; final bool enabled;
  final bool? checked;         // null: not a check/radio item; leading 16 px check slot otherwise
  final bool danger; final String? disabledHint;
  final FutureOr<void> Function() onSelected;
}

/// What the menus act on; built by each viewer from its explicit dependencies.
final class ViewerActions {
  const ViewerActions({required this.desktop, required this.input, required this.prefs,
      required this.viewport, required this.onDisconnect, required this.onQuality,
      required this.onTypeText, this.onBack, this.onGestureSheet, this.onReconnect,
      this.onPasteNewInvite});
}

List<ViewerMenuEntry> displayMenu(AppLocalizations l10n, ViewerActions a);
List<ViewerMenuEntry> shortcutsMenu(AppLocalizations l10n, ViewerActions a);
List<ViewerMenuEntry> moreMenu(AppLocalizations l10n, ViewerActions a, {required bool touch});

/// Desktop renderer: a toolbar button opening a MenuAnchor (ViewerPalette.menuStyle:
/// fill #262628, r14, padding 6, 1 px edge, shadow 0 12 40 rgba(0,0,0,0.5); items 36
/// tall r9, label 13.5 #EDEDEF, hint mono 12 #B4B4B8, hover white@0.08).
/// onOpen → toolbar.hold(); onClose → toolbar.unhold() and picture focus (§10.8).
class ViewerMenuButton extends StatelessWidget {
  const ViewerMenuButton({super.key, required this.buttonKey, required this.icon,
      this.label, required this.entries, required this.toolbar, required this.pictureFocus, this.width = 300});
}
```

The touch renderer is `showViewerSidePanel` (§9.3). Each menu is defined
once.

| Menu | Entries (keys) |
| --- | --- |
| Display (260 wide; DS §2.6.1 proposal) | section `viewerScaleLabel`; `display-fit` `viewerScaleFit` ✓ / `display-original` `viewerScaleOriginal` (local only, P1-4; → `prefs.setScaleMode` + `viewport.setMode`); divider; section `viewerQualityLabel`; `quality-quality` `viewerQualityBest` (hint `viewerQualityBestHint`), `quality-balanced` `viewerQualityBalanced` (`viewerQualityBalancedHint`), `quality-smooth` `viewerQualitySmooth` (`viewerQualitySmoothHint`) — ✓ = `desktop.quality.value`, → `onQuality(q)` (live, D12); divider; check `display-quality-panel` `viewerQuality` ↔ `prefs.qualityPanel` |
| Shortcuts (300 wide, Viewer:78-89) | from `RemoteShortcut`, table below |
| More, desktop | `viewer-type-text` `viewerTypeText` → `TypeTextDialog`; check `viewer-more-quality` `viewerQuality`; `viewer-more-back` `viewerBackToList` → `onBack` (when set) |
| More, touch (DS §2.10 proposal) | `more-gestures` `gestureTitle` → `onGestureSheet`; quality preference (the three radios above); check `viewerQuality`; `more-mode` `viewerModeView`/`viewerModeControl` radios → `desktop.control(bool)`; `more-virtual-mouse` check `virtualMouse` (enabled only in direct touch); `shortcut-refresh`; `shortcut-release`; divider; `more-disconnect` `viewerDisconnect` (`danger`) |

```dart
enum RemoteShortcut {
  showDesktop,   // Win + D         [meta↓ d↓ d↑ meta↑]
  switchWindow,  // Alt + Tab       [alt↓ tab↓ tab↑ alt↑]
  taskManager,   // Ctrl+Shift+Esc  [control↓ shift↓ escape↓ escape↑ shift↑ control↑]
  startMenu,     // Win             [meta↓ meta↑]
  lock,          // Win + L         desktop.lockRemote() (D17 host command)
  ctrlAltDel,    // never enabled, hint shortcutUnsupported (security boundary)
  refresh,       // desktop.requestKeyframe()
  releaseKeys;   // input.releaseAll()
  bool get needsControl => index <= lock.index;
}
```

| Key | Label | Hint | Enabled |
| --- | --- | --- | --- |
| `shortcut-show-desktop` | `shortcutShowDesktop` | `Win + D` | `input.ready` |
| `shortcut-switch-window` | `shortcutSwitchWindow` | `Alt + Tab` | `input.ready` |
| `shortcut-task-manager` | `shortcutTaskManager` | `Ctrl + Shift + Esc` | `input.ready` |
| `shortcut-start` | `shortcutStart` | `Win` | `input.ready` |
| `shortcut-lock` | `shortcutLock` | `Win + L` | `input.ready && session.capabilities.contains('lock')`, else `disabledHint: shortcutUnsupported` |
| divider | | | |
| `shortcut-cad` | `Ctrl + Alt + Del` | `shortcutUnsupported` (12, not mono) | never |
| divider | | | |
| `shortcut-refresh` | `shortcutRefresh` | | always |
| `shortcut-release` | `shortcutReleaseKeys` | | always |

Chords go through `input.shortcut(s)` → one `desktop.sendSequence` (keys
are ordering barriers, never interleaved with moves); they neither consume
nor apply latches. `TypeTextDialog` is a dark `AlertDialog` with
`TextField(autofocus, maxLines: 6)` and `viewerSendText` →
`input.typeText(text)`; it holds the toolbar while open and returns focus to
the picture when closed.

### 7.6 Quality panel (`V/quality_panel.dart`, ViewerStates:44-54)

`QualityPanel(Key('viewer-quality-panel'))`, 236 wide, padding 14/16, r14,
`ViewerPalette.panel`; title `AppIcons.quality` 16 + `viewerQuality`
13/600; rows (label `textSecondary` flex, value right, mono where marked)
from `ValueListenableBuilder(desktop.stats)` (≤ 1 Hz, perf §9.5):

| Row | Value (perf `DesktopStats`) | Mono |
| --- | --- | --- |
| `qualityPath` | `pathLabel(path)` (+ ` · {relayTransport.toUpperCase()}` when relay) | no |
| `qualityRtt` | `{rttMs} ms` or `—` | yes |
| `qualityLatency` | `{pictureLatencyMs} ms` or `—`, `ViewerPalette.caution` when `degraded` | yes |
| `qualityFps` | `{presentedFps.round()}` | yes |
| `qualityBitrate` | `{receivedBitrateBps / 1e6:.1} Mbps` | yes |
| `qualityResolution` | `{width} × {height}` | yes |
| `qualityCodec` | `hardwareDecoder == true` → `qualityHardware(codec)`; `false` → `qualitySoftware(codec)`; null → codec | no |

Footer 12.5 `textSecondary` by `adaptation`: `loweringBitrate` →
`qualityAdaptBitrate`; `loweringFps` or `unresponsive` → `qualityAdaptFps`;
otherwise hidden. While open it holds the toolbar. Desktop anchor: toolbar
bottom + 8, right 14; mobile: the picture's top-left + 12.

### 7.7 Palette (`U/theme/viewer_palette.dart`, UI-1)

`viewerTheme(BuildContext context)` = `buildAppTheme(Brightness.dark)` with
`scaffoldBackgroundColor: ViewerPalette.ground` and menu, tooltip and dialog
themes tuned to the palette. `ViewerPalette` holds the literals of DS §2.0
once: `ground #0F0F10`, `black #000000` (mobile ground), `card #1C1C1E`,
`menu #262628`, `chrome Color(0xE6202023)`, `panel Color(0xF01C1C1E)`,
`hint Color(0xDB18181A)`, `edge white@0.08`, `fill white@0.10`,
`fillStrong white@0.16`, `separator white@0.12`, `keySeparator white@0.14`,
`outline white@0.18`, `text #EDEDEF`, `textSecondary #B4B4B8`,
`textQuiet #8E8E93`, `signal #7DB6F2`, `success #57C27A`, `caution #E0A647`,
`danger #F2B8B5`, plus `static MenuStyle menuStyle`.

---

## 8. Desktop viewer

### 8.1 `DesktopViewer` (`V/desktop_viewer.dart`)

```dart
class DesktopViewer extends StatefulWidget {
  const DesktopViewer({super.key, required this.desktop, required this.prefs, required this.title,
      this.onBack, this.onDisconnect, this.onQuality, this.onReconnect, this.onPasteNewInvite});
  final DesktopStore desktop; final ViewerPrefs prefs; final String title;
  final VoidCallback? onBack;                      // null: no Back control (acceptance tool)
  final Future<void> Function()? onDisconnect;     // default: desktop.disconnect()
  final ValueChanged<DesktopQuality>? onQuality;   // default: desktop.setQuality
  final VoidCallback? onReconnect;                 // default: desktop.connect(desktop.session.value.target!)
  final VoidCallback? onPasteNewInvite;            // null: action hidden
}
```

Explicit dependencies, no `AppScope` reads, so
`tool/desktop_viewer_acceptance.dart` hosts it in a bare `MaterialApp` with
`ViewerPrefs(persist: false)` and holds `desktop.demand(DesktopDemand.viewer)`
itself (§17.2). `RemoteViewerScreen` passes `remote.back`,
`remote.disconnect`, `remote.setQuality`, `remote.reconnect` and, for a
remembered computer, `() => showAddComputer(context, replacing: computer)`.

State: `RemoteViewport _viewport`, `ModifierLatch _latch`, `RemoteInput
_input`, `TouchGestureMachine _machine` (direct mode; Windows
touchscreens), `ToolbarController _toolbar` (pinned from prefs),
`FocusNode _picture`, `bool _fullscreen` (`WindowListener`
`onWindowEnterFullScreen`/`onWindowLeaveFullScreen`), an
`AppLifecycleListener(onInactive: _input.releaseAll, onHide: _input.releaseAll)`
for local input only. `dispose` leaves full screen if set.

```
ColoredBox(ViewerPalette.ground) Column[
  if (!_fullscreen) ViewerTitleBar(title, onBack)                          // 36
  Expanded(Focus(focusNode: _picture, autofocus: true,
                 onKeyEvent: _input.handleHardwareKey,                     // §10.5.1
                 onFocusChange: (f) { if (!f) _input.releaseAll(); },      // §10.8
    child: Stack[
      ViewerStage.of(desktop, …, toolbarInset: _toolbar.inset)             // picture + overlays
      ViewerToolbar(controller: _toolbar, …)                               // top 12 / handle top 0
      if (prefs.qualityPanel) QualityPanel (toolbar bottom + 8, right 14)
      ControlHint (bottom 64, centred)
    ]))
]
```

`DesktopViewer.build` depends only on `_fullscreen` and `title`; everything
that changes at media or state rate is a leaf builder (§3).

### 8.2 `ViewerTitleBar` (`V/viewer_title_bar.dart`, Viewer:17-27)

`SizedBox(height: 36)`;
`Stack[Positioned.fill(WindowDragArea()), Padding(start: trafficLightInset + 8, end: drawsCaptionButtons ? 4 : 12, LayoutBuilder(Row[…]))]`:
`if (onBack != null)` `_TitleBack` (28×28 r6, `AppIcons.back` 16
`textSecondary`, `Key('viewer-back')`, tooltip `viewerBackToList`) · 8 ·
`AppIcons.remote` 16 `ViewerPalette.signal` · 10 ·
`IgnorePointer(Text(title, 13/600 text))` · 10 ·
`IgnorePointer(Text('pb-mapper', 12.5 textQuiet))` · `Spacer` (drag) ·
`SharingIndicator(form: sharingFormFor(width), tone: CaptionTone.viewer)` ·
12 · `if (drawsCaptionButtons) CaptionButtons(tone: CaptionTone.viewer)`
(46×32, 2 px above and below; 4 px end padding — no sheet corner to clear).
Hidden in full screen (the fallback layer covers D15 there, §4.6).

### 8.3 Floating toolbar (`V/viewer_toolbar.dart`, Viewer:33-76)

`ViewerToolbar` = `MouseRegion(onEnter: controller.pointerEnter, onExit: controller.pointerExit)`
around `Padding(top: 12)` + the toolbar — the top margin is part of the
region, so once expanded the handle zone (y 0-20) counts as inside (no
hover loop, §10.7). Expanded: 48 tall, r16, padding 0/6, gap 2, fill
`ViewerPalette.chrome`, **no `BackdropFilter`** (U19), 1 px
`ViewerPalette.edge`, shadow `0 8 30 rgba(0,0,0,0.4)`; separators 1×22
`separator`, margin 0/4; `Semantics(container: true, label: viewerToolbar)`.
Items:

1. `viewer-pin` 36×36: `AppIcons.pin` (pinned, fill `fill`) /
   `AppIcons.unpin`; tooltip `viewerUnpin` / `viewerPin` →
   `controller.setPinned(v)` + `prefs.setToolbarPinned(v)`.
2. sep · `ToolbarStatusChip(Key('viewer-quality-chip'))` from `desktop.stats`:
   dot 8 (`success`; `caution` when `degraded`; `textQuiet` while
   `session.phase != live`) + `pathLabel(path)` 13 + mono 12
   `textSecondary` `"{rttMs} ms · {presentedFps} fps"`; degraded form
   `viewerSlow(path)` + mono caution `"{pictureLatencyMs} ms"`
   (ViewerStates:40-43). Tap toggles `prefs.qualityPanel`.
3. sep · mode `GlassSegmented<bool>(key: Key('viewer-mode'), height: 36, expand: false)`
   (U28): `false` = `AppIcons.viewOnly` + `viewerModeView`, `true` =
   `AppIcons.control` + `viewerModeControl`; selected
   `session.controlWanted`; `onChanged` → `desktop.control(v)`.
   `GlassSegment` gains `bool enabled = true` (UI-1): Control is disabled
   while `!session.controlAllowed` (tooltip `viewerViewOnlyByHost`). While
   control is wanted but not yet `controlling`, the Control icon is a 12 px
   spinner.
4. sep · `ViewerMenuButton(buttonKey: Key('viewer-display'), icon: AppIcons.remote, label: viewerDisplay, entries: displayMenu(…), width: 260)`
   (+ `AppIcons.chevronDown` 16).
5. `ViewerMenuButton(buttonKey: Key('viewer-shortcuts'), icon: AppIcons.shortcuts, label: viewerShortcuts, entries: shortcutsMenu(…))`;
   open state fill `fill`.
6. `ViewerMenuButton(buttonKey: Key('viewer-more'), icon: AppIcons.more, entries: moreMenu(…, touch: false))`, 36×36.
7. sep · `Key('viewer-fullscreen')` 36×36 `AppIcons.fullscreen` /
   `AppIcons.exitFullscreen`, tooltip `viewerFullscreen` /
   `viewerExitFullscreen` → `windowManager.setFullScreen(!_fullscreen)`.
   Esc and F11 go to the remote; only this button toggles.
8. `Key('viewer-disconnect')`: `AppIcons.disconnect` 16 + `viewerDisconnect`
   13, `ViewerPalette.danger` → `onDisconnect`.

Below 760 px of picture width the labels of 3, 4 and 5 are hidden (icons +
tooltips only).

Collapsed: `ToolbarHandle(Key('viewer-toolbar-handle'))` 72×20 at top 0,
centred, radius `vertical(bottom: 10)`, 1 px edge without the top, fill
`chrome`, `AppIcons.chevronDown` 16 (`AppIconSize.inline`; BRIEF req 3 allows only 16/18/20/24); tooltip/semantics `viewerExpand`. Tap,
or hover held `kHandleHoverExpand` (300 ms) → expand (U7). Expand/collapse
animate with `AnimatedSlide` + `AnimatedOpacity` over
`Motion.of(context, Motion.medium)` (instant under reduced motion).
`controller.inset` (68 expanded / 28 collapsed) positions top pills.

### 8.4 Hints

- `ControlHint` (Viewer:91-95; `Key('viewer-control-hint')`): `ViewerPill(height: 40)`
  `AppIcons.keyboard` 16 `signal` + `viewerControlHint(title)` + secondary
  `viewerControlHintSub`; `IgnorePointer`; shown `kHintDuration` (3 s) when
  `session.controlling` turns true and on the first focus of a controlling
  viewer.

---

## 9. Mobile viewer

### 9.1 `TouchViewer` (`V/touch_viewer.dart`)

```dart
class TouchViewer extends StatefulWidget {
  const TouchViewer({super.key, required this.desktop, required this.prefs, required this.title,
      required this.onBack, this.onDisconnect, this.onQuality, this.onReconnect, this.onPasteNewInvite});
}
```

- `initState`: `SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky)`;
  on phones (`MediaQuery.sizeOf(context).shortestSide < 600`, read in
  `didChangeDependencies` once) `SystemChrome.setPreferredOrientations([landscapeLeft, landscapeRight])`
  (U21). `dispose`: `setEnabledSystemUIMode(SystemUiMode.edgeToEdge)` and
  `setPreferredOrientations(const [])`. Tablets rotate freely; layout
  adapts by geometry (§9.2).
- `Scaffold(backgroundColor: ViewerPalette.black, resizeToAvoidBottomInset: false, body: LayoutBuilder(…))`
  — the IME never resizes the stage (U12).
- Layers:

```
Stack[
  ViewerStage.of(desktop, …, compact: true)
  if (!keyboardOpen) ...gutterControls(gutterLayoutFor(size, aspect, padding))   // §9.2
  Minimap            (top 10, right 12 + padding.right; while viewport.visibleRegion != null)
  ModeHint           (top 12, centred over the picture; 3 s on open and on mode change)
  if (prefs.touchMode == TouchMode.direct && prefs.virtualMouse) VirtualMousePad
  ModifierPill       (left 12, bottom: occlusion + 10; while !latch.isEmpty)
  ComposingPreview   (above the bar, while textInput.composing is not empty)
  if (keyboardOpen) Positioned(left: 0, right: 0, bottom: viewInsets.bottom, child: ModifierBar)   // 44
  if (prefs.qualityPanel) QualityPanel (picture top-left + 12)
]
```

- `RemoteTextInput _textInput` is a plain object (§10.5.2), not a widget;
  `keyboardOpen = _textInput.attached.value && MediaQuery.viewInsetsOf(context).bottom > 0`.
- First live session ever (`!prefs.gestureSheetSeen`): after the first
  `live` phase, show `GestureSheet` once.
- `ModeHint` (`Key('touch-mode-hint')`): `ViewerPill(height: 32)`
  `AppIcons.trackpad` / `AppIcons.directTouch` 16 `signal` +
  `touchTrackpadHint` / `touchDirectHint`.
- The hardware keyboard (Bluetooth) goes through the picture's `Focus`
  exactly as on desktop (§10.5.1); the IME client never receives key events.

### 9.2 Gutters (`V/touch_controls.dart`, MobileViewer)

```dart
enum GutterKind { side, bars, overlay }
@immutable class GutterLayout { final GutterKind kind; final Rect lead, trail; }
GutterLayout gutterLayoutFor(Size screen, double aspect, EdgeInsets padding);
//  gx = (W − H·aspect)/2; gy = (H − W/aspect)/2
//  gx ≥ 64 → side gutters inset by padding.left/right (cutouts)
//  gy ≥ 64 → top and bottom bars (tablet portrait, 4:3 remotes)
//  else    → overlaid on the picture's left/right edges on ViewerPalette.hint plates
```

Lead gutter (column, `spaceBetween`, padding 16/0):
`GutterButton(Key('touch-back'), AppIcons.back, a11y touchBack)` →
`onBack`; `QualityGutterButton(Key('touch-quality'))` 56 wide (dot 8,
`pathLabel` 11, fps mono 14/500, "fps" 10.5; a11y `touchQualityA11y(path, fps)`)
→ toggles `prefs.qualityPanel`; `GutterButton(Key('touch-mode-toggle'))`:
trackpad = fill `rgba(125,182,242,0.2)`, `AppIcons.trackpad` 20 `signal`;
direct = `fill`, `AppIcons.directTouch`; label 10.5 `touchModeTrackpad` /
`touchModeDirect`; a11y `touchModeToggleA11y(mode)`; tap →
`prefs.setTouchMode` + `ModeHint`; long press → `GestureSheet`.

Trail gutter (`Semantics(label: touchControls)`, centred column, gap 14):
`touch-keyboard` (`AppIcons.keyboard`, `touchKeyboard`) →
`_textInput.show()`; `touch-shortcuts` (`AppIcons.shortcuts`,
`viewerShortcuts`) → `showViewerSidePanel(title: viewerShortcuts, entries: shortcutsMenu(…))`;
`touch-right-click` (`AppIcons.rightClick`, `touchRightClick`) →
`input.click(RemoteButton.right, at)` where `at` = the cursor (trackpad) or
the last touch point (direct), wrapped by latches; `touch-more`
(`AppIcons.more`, `more`) → `showViewerSidePanel(title: more, entries: moreMenu(…, touch: true))`.
Each = 44 circle `fill` + icon 20 + label 10.5; keyboard, shortcuts and
right click are 40 % opacity and inert while `!input.ready`.

Hit testing: controls are opaque; the empty gutter is not, so a trackpad
stroke can start anywhere (MobileGestures:30, "slide anywhere").

### 9.3 Side panel (`V/side_panel.dart`)

`Future<void> showViewerSidePanel(BuildContext context, {required String title, required List<ViewerMenuEntry> entries})`
= `showGeneralDialog(barrierDismissible: true, barrierColor: Color(0x52000000), transitionDuration: Motion.medium, …)`
sliding a 300-wide full-height `ViewerPalette.panel` sheet in from the
trailing edge; the same entries as the desktop menu, as 48-tall rows (check
and radio slots, danger colour, disabled hint as a subtitle). Keeps immersive
mode; Android back closes it first; `whenComplete` returns picture focus.

### 9.4 Keyboard and modifier bar (MobileKeyboard)

- While the keyboard is open the gutters are hidden (MobileKeyboard:16-28)
  and `viewport.setOcclusion(EdgeInsets.only(bottom: inset + 44))` (§10.6).
- `ModifierBar(Key('modifier-bar'))`, 44 tall, fill `ViewerPalette.card`, top
  1 px edge, mono 13; `Semantics(label: touchModifierBar)`. Layout: the
  modifier group fixed at the leading edge, the middle in a horizontal
  `SingleChildScrollView`, the collapse button fixed at the trailing edge.
  Caps (`KeyCap`, 32 tall, r8, padding 0/10, gap 6, fill `fill`, text
  `text`; separators 1×20 `keySeparator`, margin 0/2):

| Group | Caps (keys) | Behaviour |
| --- | --- | --- |
| modifiers (fixed) | `Ctrl` (min 46) `Alt` `Shift` `Win` (min 42) — `modifier-control/alt/shift/meta` | `latch.tap` (§10.4). Armed: fill `text`, ink `#1D1D1F` 600. Locked: the same + a 2 px `signal` underline bar. `HapticFeedback.selectionClick()` per change |
| keys | `Esc` `Tab` (min 40) `Fn` (min 40, `modifier-fn`) | `input.tapKey(escape \| tab)`; `Fn` toggles the scrolled groups to `F1`…`F12` |
| arrows | `←` `↑` `↓` `→` (36 wide; a11y `keyArrow(dir)`) | tap = `tapKey`; hold = repeat (`pressKey`, 400 ms then every 60 ms, `releaseKey` on up) |
| navigation (U24) | `Home` `End` `PgUp` `PgDn` | `tapKey(home \| end \| pageUp \| pageDown)`, hold = repeat |
| edit | `Ins` `Del` `Ctrl+C` `Ctrl+V` | `tapKey(insert \| delete)`; `Ctrl+C` / `Ctrl+V` = `tapKey(c \| v, extra: {control})` |
| collapse (fixed) | 40×32 `AppIcons.chevronDown` 18, `Key('modifier-collapse')`, a11y `touchHideKeyboard` | `_textInput.hide()` |

  With `Fn` on, the scrolled area shows `Esc Tab Fn | F1 … F12`. Key-cap
  literals are mono text, identical in both languages, not ARB keys.
- `ModifierPill` (MobileKeyboard:24-27, `Key('modifier-pill')`):
  `ViewerPill(height: 30)` dot 7 `signal` + `modifierLocked(keys)` and/or
  `modifierArmed(keys)` joined with " · "; visible whenever a latch is not
  off, keyboard open or not.
- `ComposingPreview`: a `ViewerPill` above the bar showing uncommitted IME
  text (CJK is visible before it is sent).

### 9.5 Minimap (`V/minimap.dart`, MobileKeyboard:21-23)

`Minimap(Key('minimap'))`: 96 × 96/aspect, r6, fill `rgba(20,20,22,0.8)`,
1 px `white@0.25`; `MinimapPainter` draws `viewport.visibleRegion`
(normalised) as a 1.5 px `signal` r2 rectangle clamped to the box. No
picture inside (no second `Texture`). `IgnorePointer`.
`ValueListenableBuilder(viewport.visibleRegion)`; null (hidden) while the
whole picture is visible (± 0.5 %). A11y `minimapA11y(region)` with
`region` from the rect centre on a 3×3 grid.

### 9.6 Virtual mouse (`V/virtual_mouse.dart`, D18 direct touch)

`VirtualMousePad(Key('virtual-mouse'))`, 72×120, `chrome`, r16, 1 px edge,
initial position bottom-right inside the picture above the trail gutter:
top row `L | R` (2 × 36×48) → `input.click(left | right, at: cursor)`;
scroll strip 72×44 (`AppIcons.chevronUp` / `chevronDown` 16): vertical drag
→ wheel, one notch per `kVirtualWheelNotchPx` (16 px), finger down = wheel
up, `wheelStart`/`wheelEnd` around the burst (consumes latches); grip 72×28:
drag moves the pad (clamped to the stage; position kept per session).
Opaque, so a press on it is never also a remote click. Shown only in direct
mode with `prefs.virtualMouse`.

### 9.7 Gesture sheet (`V/gesture_sheet.dart`, MobileGestures)

In the viewer a centred panel, max width 390, scrollable,
`ViewerPalette.card` (DS §2.12 landscape proposal). Title `gestureTitle` +
close; `GlassSegmented<TouchMode>(key: Key('gesture-mode'), height: 44)`
(track 44 → segments 38, MobileGestures:26-29) → `prefs.setTouchMode`;
description `gestureTrackpadDescription` / `gestureDirectDescription`; rows
54 tall, `GestureGlyph` 40×40 tile with a 24 px `GestureGlyphPainter`
(stroke 1.5, paths from DS §2.12):

| Trackpad | Direct touch |
| --- | --- |
| `gestureSlide` → `gestureSlideResult` | `gestureSlide` → `gestureDragResult` |
| `gestureTap` → `gestureTapResult` | `gestureTap` → `gestureTapHereResult` |
| `gestureLongPress` → `gestureLongPressResult` | same |
| `gestureDoubleTapHold` → `gestureDragResult` | (row hidden) |
| `gestureThreeFinger` → `gestureThreeFingerResult` | same |
| `gestureTwoFingerMove` → `gestureTwoFingerMoveResult` | same |
| `gesturePinch` → `gesturePinchResult` | same |

Switch `virtualMouse` / `virtualMouseHelp` (enabled only in direct mode);
`FilledButton(Key('gesture-got-it'), gotIt)` → close +
`prefs.setGestureSheetSeen(true)`.

---

## 10. Interaction specification

### 10.1 Constants (one place each)

| Constant | Value | File | Source |
| --- | --- | --- | --- |
| `kTouchSlop` | 18 px | Flutter | rd §0 |
| `kDoubleTapTimeout` | 300 ms (first up → second down) | Flutter | rd §3.9 |
| `kDoubleTapMinTime` | 40 ms | Flutter | rd §3.9 |
| `kDoubleTapSlop` | 100 px (first down → second down) | Flutter | rd §3.9 |
| `kLongPressTimeout` | 500 ms | Flutter | rd §3.9 |
| `kHoldToDrag` | 300 ms (second touch held) | `touch_gestures.dart` | rd `_startSecondTapDownTimer` |
| `kRemoteDoubleClickGuard` | 520 ms after the first click was sent (viewer hint) | `touch_gestures.dart` | U4; the host enforces `GetDoubleClickTime() + 20 ms` (R-UI-3) |
| `kPointerJoinGrace` | 80 ms | `touch_gestures.dart` | judges' must-fix |
| `kNavigateStart` | 80 ms after the second down, or a finger moved > slop | `touch_gestures.dart` | interaction |
| `kThreeFingerNotchPx` | 24 px mean finger travel per notch | `touch_gestures.dart` | U9 |
| `kVirtualWheelNotchPx` | 16 px | `virtual_mouse.dart` | |
| `kWheelPxPerNotch` | 30 px of `scrollDelta` (fractions accumulated) | `remote_surface.dart` | `desktop_viewer.dart:456-460` |
| `kWheelClamp` | ±100 per event | `remote_input.dart` | perf `DesktopInput.wheel` |
| `kMinScale` / `kMaxScale` | 1 (fit) / 8 | `remote_viewport.dart` | today |
| `kFollowMargin` | 15 % of the visible rect | `remote_viewport.dart` | rd §3.13 |
| `kKeyboardSettle` | 100 ms | `touch_viewer.dart` | rd §3.12 |
| `kCursorEchoWindow` | 300 ms after a local move | `remote_cursor_layer.dart` | D11 |
| `kTouchIndicatorFade` | 150 ms | `remote_surface.dart` | DS §2.10 |
| `kToolbarCollapseDelay` | 2 s | `viewer_toolbar.dart` | BRIEF |
| `kHandleHoverExpand` | 300 ms | `viewer_toolbar.dart` | U7 |
| `kHintDuration` | 3 s | `viewer_overlays.dart`, `touch_viewer.dart` | DS §2.6, §2.10 |
| `kKeyRepeatDelay` / `kKeyRepeatInterval` | 400 ms / 60 ms | `remote_input.dart` | |
| `kTextChunkBytes` | 1024 UTF-8 bytes | `remote_input.dart` | perf `DesktopInput.text` |
| `kSequenceMax` | 16 inputs per `sendSequence` | `remote_input.dart` | perf §7.3 |
| `kLatchedTextKeys` | 16 characters keyed per `typeText` call while a latch is active; the rest goes as `text` | `remote_input.dart` | perf §6.4 sizing (≤ 160 events) |
| `kTextMaxBytes` | 64 KiB of UTF-8 per `typeText` call (≤ 64 `text` events); `TypeTextDialog` enforces it in its field, and an IME commit is cut there at a code-point boundary | `remote_input.dart` | perf §6.4 sizing (`INPUT_QUEUE_BYTES`) |
| `kPausedSessionTimeout` | 10 min after `back()` | `remote_desktop.dart` | perf §9.6.4; the host's single slot |

### 10.2 Touch gesture state machine (`V/touch_gestures.dart`)

```dart
enum HapticKind { press, select }

abstract interface class RemotePointerSink {
  Offset get cursor; set cursor(Offset normalized);   // local prediction (trackpad)
  void hover(Offset n); void move(Offset n);
  void press(RemoteButton b, Offset n, {bool dragStart = false});
  void release(RemoteButton b);
  void click(RemoteButton b, Offset n);
  void wheelStart(); void wheel(int horizontal, int vertical); void wheelEnd();
}

/// Pure and deterministic: every event carries its time; the owner arms one
/// timer for [nextTimeout] and calls [timeout] when it fires.
class TouchGestureMachine {
  TouchGestureMachine({required RemotePointerSink sink, required RemoteViewport viewport,
      required TouchMode Function() mode, required bool Function() interactive,
      required void Function(HapticKind) haptic, required ValueChanged<Offset?> indicator});
  void down(int pointer, Offset local, Duration t);
  void move(int pointer, Offset local, Duration t);
  void up(int pointer, Offset local, Duration t);
  void cancel(Duration t);          // pointer cancel, epoch / mode / readiness change, focus loss
  Duration? get nextTimeout;        // the earliest pending deadline
  void timeout(Duration t);         // fires every deadline ≤ t, in order
  ValueListenable<Offset?> get indicatorPoint;
}
```

`RemoteSurface` feeds touch, stylus and inverted-stylus pointers from one
`Listener(behavior: HitTestBehavior.opaque)`; mouse and touchpad go to
§10.3. Time is the surface's own `Stopwatch` (`_clock.elapsed`), for events
and timeouts alike. After every event and timeout the surface re-arms its
single `Timer` to `nextTimeout − _clock.elapsed` (≥ 0). There is no
`ScaleGestureRecognizer`: the machine is the sole owner of touch, for remote
input and local navigation (U3). Stylus uses direct semantics.

A **sequence** runs from the first finger down to the last finger up. Its
class only escalates — 1 finger → 2 (navigate) → 3+ (scroll) — and never
falls back while any finger is down, so lifting one pinch finger never
becomes a stroke or a drag.

Memory across sequences (trackpad): `lastTap = (upAt, downPos, clickAt, cursorAt)`,
cleared after a second tap, on navigation and on `cancel`.

**Trackpad mode** (default, D18). The click point is always the local
cursor prediction `sink.cursor`, never the finger.

| State | Event | Guard | Actions | Next |
| --- | --- | --- | --- | --- |
| idle | down p @t | `lastTap` set, 40 ms ≤ t − `upAt` ≤ 300 ms, `dist(p, downPos)` ≤ 100 | indicator(p); `holdAt = t + 300 ms` | secondDown |
| idle | down p @t | otherwise | indicator(p); `longPressAt = t + 500 ms` | firstDown |
| firstDown | move | moved > 18 | clear `longPressAt`; `cursor += Δ / (scene · scale)`; `sink.hover(cursor)`; `viewport.ensureVisible(cursor)` | stroke |
| firstDown | timeout `longPressAt` | — | haptic(press); `sink.click(right, cursor)` | rightClicked |
| firstDown | up @t | — | `sink.click(left, cursor)` **now** (D18); `lastTap = (t, p0, t, cursor)` | idle |
| secondDown | move | moved > 18 | clear `holdAt`; `pressDueAt = max(t, lastTap.clickAt + 520 ms)`; the local cursor moves | dragging (press pending) |
| secondDown | timeout `holdAt` | — | haptic(select); `pressDueAt` as above | dragging (press pending) |
| secondDown | up | — | `sink.click(left, cursor)` now (the remote sees a double-click); `lastTap = null` | idle |
| stroke | move | — | relative move + follow | stroke |
| rightClicked | move | moved > 18 | relative move + follow (slide through the context menu) | stroke |
| stroke, rightClicked | up | — | — | idle |
| dragging (press pending) | timeout `pressDueAt` | — | `to = cursor`; `sink.press(left, lastTap.cursorAt, dragStart: true)`; `sink.move(to)` (the cursor is read before the press, which resets the prediction to the press point) | dragging |
| dragging | move | — | `cursor += Δ…`; follow; `sink.move(cursor)` only once the press was sent | dragging |
| dragging | up | press pending | `to = cursor`; `sink.press(left, start, dragStart: true)`, `sink.move(to)`, `sink.release(left)` now (the host holds the press, R-UI-3) | idle |
| dragging | up | press sent | `sink.release(left)` | idle |
| any 1-finger state | 2nd down @t | — | clear deadlines; a sent press → `sink.release(left)`; a pending press is dropped; `lastTap = null`; indicator(null); remember both positions | navigate |
| navigate | move | t ≥ secondDownAt + 80 ms, or a finger moved > 18 | `viewport.pinchPan(startPositions, positions)`: `scale = clamp(s0 · d / d0, 1, 8)`, `offset = mid − anchor · scale` (bounded) | navigate |
| navigate | 3rd down | — | freeze navigation; `sink.wheelStart()` | scroll |
| scroll | move | — | `acc += Σdy / pointers`; `n = trunc(acc / 24)`; if `n ≠ 0`: `sink.wheel(0, −n)` (clamped ±100), `acc −= 24 n` | scroll |
| navigate, scroll | up | fingers remain | — (no de-escalation) | same |
| any | last up | — | `sink.wheelEnd()` if scroll; indicator fades over 150 ms | idle |
| any | cancel | — | deadlines cleared; a sent press → `sink.release(left)`; a pending press is dropped; `wheelEnd()` if scrolling | idle |

Finger down means "pull the content down", i.e. wheel up: a negative
`vertical` (pb-mapper's positive is "scroll down",
`desktop_viewer.dart:362-378`; RustDesk sends finger-down as wheel up, rd
§3.9). The touch indicator is a 44 px circle (`white@0.22`, 1.5 px
`white@0.8`) at the finger in an `IgnorePointer` layer, trackpad mode only.

**Direct-touch mode.** The finger is the pointer; `n = viewport.toNormalized(p, clamp: true)`.

| State | Event | Guard | Actions | Next |
| --- | --- | --- | --- | --- |
| idle | down p | p outside the picture | — | ignored until all fingers are up |
| idle | down p @t | — | `longPressAt = t + 500 ms`; `graceAt = t + 80 ms` | firstDown |
| firstDown | move | moved > 18, t ≥ `graceAt` | clear `longPressAt`; `sink.press(left, n0)`; `sink.move(n)` | dragging |
| firstDown | move | moved > 18, t < `graceAt` | clear `longPressAt`; `dragPending = true` (a second finger may still join) | firstDown |
| firstDown | timeout `graceAt` | `dragPending`, one finger | `sink.press(left, n0)`; `sink.move(last)` | dragging |
| firstDown | timeout `longPressAt` | not moved | haptic(press); `sink.click(right, n0)` | consumed |
| firstDown | up | `dragPending` | `sink.press(left, n0)`, `sink.move(n)`, `sink.release(left)` | idle |
| firstDown | up | otherwise | `sink.click(left, n0)` now | idle |
| dragging | move | — | `sink.move(n)` | dragging |
| dragging | up | — | `sink.release(left)` | idle |
| consumed | move / up | — | — | consumed / idle |
| any 1-finger state | 2nd down | — | as trackpad (a pending drag never presses) | navigate |
| navigate, scroll | | | as trackpad | |

A double tap in direct mode is two immediate clicks (a remote double-click).
A two-finger tap does nothing in either mode (D18; the release note says
so).

**Not interactive** (`!interactive()`: view only, stale picture, control
not granted): one finger pans the picture locally when zoomed
(`viewport.panBy`), two fingers pinch, three do nothing; nothing reaches the
sink (today's rule, `desktop_gesture_surface.dart:302`).

### 10.3 Mouse and touchpad (desktop; also a mouse on Android or an iPad)

| Input | Interactive | Not interactive |
| --- | --- | --- |
| Hover inside the picture | `sink.hover(p)`; original-size mode also `viewport.autoScrollTo(local)` | auto-scroll only |
| Button down inside the picture | request picture focus; `sink.press(left/right/middle, p)` from `event.buttons` | focus only |
| Move with a button held (also outside the picture) | `sink.move(clamp(p))` | — |
| Button up | `sink.release(b)` | — |
| `PointerScrollEvent` with Ctrl physically held | `viewport.zoomAt(p, exp(−dy / 250))` — local, as today (U20) | same |
| `PointerScrollEvent` otherwise | `sink.hover(p)`; `accY += dy / 30`, `accX += dx / 30` → `sink.wheel(nx, ny)` per whole notch | — |
| Touchpad `PointerPanZoomUpdate`, pan | `accY += −panDelta.dy / 30` → wheel (natural scrolling, U10) | `viewport.panBy(panDelta)` |
| Touchpad pan-zoom, scale ≠ 1 | `viewport.zoomAt(focal, scaleDelta)` (local) | same |
| Touch on a Windows touchscreen | `TouchGestureMachine`, direct mode | — |

`ViewerScaleMode.original`: `scale = max(1, videoWidth / (scene.width * dpr))`
(one remote pixel per physical pixel); the view follows the pointer,
`offset = −(pointer / visible) · (scene · scale − visible)` per axis
(RustDesk "scroll auto"). Local only.

### 10.4 Modifier latches (D16) and how input leaves the viewer

```dart
enum LatchState { off, armed, locked }

final class ModifierLatch extends ChangeNotifier {
  static const order = [RemoteKey.control, RemoteKey.alt, RemoteKey.shift, RemoteKey.meta];
  LatchState stateOf(RemoteKey modifier);
  Set<RemoteKey> get active;        // armed ∪ locked, in [order]
  bool get isEmpty;
  void tap(RemoteKey modifier);     // off → armed → locked → off
  Set<RemoteKey> take();            // returns active; armed → off; locked stays
  void clear();                     // everything off
}
```

| From | Event | To |
| --- | --- | --- |
| off | tap | armed (one-shot) |
| armed | tap | locked |
| locked | tap | off |
| armed | an action is sent (`take()`) | off |
| locked | an action is sent | locked |
| any | `clear()` (§10.8 triggers) | off |

"Action" = a key from the modifier bar, the soft keyboard or a hardware key
(§10.5.1), a click (tap gesture, right-click button, virtual mouse), a drag
(at its press), a three-finger scroll (at its start), a virtual-mouse wheel
burst. Pointer moves never consume.

`RemoteInput` (`V/remote_input.dart`) is the only door to the store:

```dart
final class RemoteInput implements RemotePointerSink {
  RemoteInput({required DesktopStore desktop, required ModifierLatch latch,
      required void Function() onLocalCancel, Duration Function()? clock});
  ValueListenable<bool> get ready;     // session.inputLive && geometry != null
  Listenable get cursorChanges;        // fires when the local prediction moves
  Duration? get lastLocalMoveAt; bool get strokeActive;
  KeyEventResult handleHardwareKey(FocusNode node, KeyEvent event);   // §10.5.1
  void tapKey(RemoteKey key, {Set<RemoteKey> extra = const {}});      // latched + extra wrap one tap
  void pressKey(RemoteKey key); void releaseKey();                    // repeat from the modifier bar
  void shortcut(RemoteShortcut s);                                    // menus; latches untouched
  void typeText(String text); void backspace(int count);              // §10.5.2
  void releaseAll();                                                  // §10.8
  void dispose();
}
```

Rules:

- Every method is a no-op unless `ready`, except `releaseAll` and the
  `refresh`/`releaseKeys` shortcuts.
- Normalised points become display pixels with the current
  `geometry.displayWidth/Height`: `x = (n.dx · w).floor().clamp(0, w − 1)`
  (as `desktop_viewer.dart:209-219`). Every call passes
  `geometryEpoch: geometry.epoch`; Rust stamps generation, frame id and
  sequence (perf §9.6.3). A `false` return means dropped; nothing is held.
- `click`, `tapKey`, `typeText` (per keyed character), `shortcut` and
  `backspace` each become **one** `desktop.sendSequence(...)`:
  `[m↓ for m in mods] + action + [m↑ reversed]`, mods = `latch.take() ∪ extra`
  (`click` = `[move(n), mods↓, button↓, button↑, mods↑]`). `sendSequence` is
  atomic and all-or-release (perf), so nothing stays held on the host (D16).
  Sequences longer than `kSequenceMax` are split only between whole taps.
- **Burst bound.** Between two frames no path submits more than 288 events
  or about 100 KB (perf §6.4 sizes the viewer queue on it): `backspace`
  takes ≤ 64 code points per call (128 events in 8 batches); `typeText` keys
  at most `kLatchedTextKeys` (16) characters while a latch is active (≤ 10
  events each: at most 4 modifiers wrapped, Shift included), and the
  remainder of that call goes as `text` events, so a long commit under a
  locked modifier types its characters instead of flooding the host with
  chords; and `typeText` sends at most `kTextMaxBytes` (64 KiB, ≤ 64 `text`
  events) per call. An IME replacement (`backspace` then `typeText`) is the
  largest burst.
- `press` sends `[move(n), m↓…, button↓(dragStart)]` and remembers
  `_heldMods` / `_heldButtons`; `release` sends `[button↑, m↑… reversed]`;
  `wheelStart` sends `[m↓…]` (`latch.take()`), `wheelEnd` `[m↑…]`;
  `pressKey` sends `[m↓…, key↓]`, repeats `key↓` every 60 ms after 400 ms,
  and `releaseKey` sends `[key↑, m↑…]`.
- `hover` and `move` go through `desktop.send(DesktopInput.move(x, y))` (the
  store coalesces moves, perf §9.6.3); a local move stamps `lastLocalMoveAt`.
- `releaseAll`: `onLocalCancel()` (the machine cancels), `latch.clear()`,
  key repeat stops; if anything is held (hardware keys, buttons,
  `_heldMods`) → `desktop.release()`; the held sets clear.
- `ready` true → false, or a `geometry.epoch` change: `onLocalCancel()`,
  `latch.clear()`, held sets clear (the host already released on
  invalidate/pause).

### 10.5 Keyboards

`RemoteKey` comes from perf `U/state/desktop_input.dart` (D17 vocabulary:
`shift control alt meta enter escape tab backspace delete insert space left
right up down home end pageUp pageDown f1…f12 a…z digit0…digit9`). Lock is
`desktop.lockRemote()`, not a key; Ctrl+Alt+Del is unrepresentable.

```dart
abstract final class KeyRouting {   // V/remote_keys.dart, pure
  static RemoteKey? modifier(LogicalKeyboardKey k);   // Shift/Control/Alt/Meta, left or right
  static RemoteKey? special(LogicalKeyboardKey k);    // F1–F12, Enter/NumpadEnter, Escape, Tab, Backspace,
                                                      // Delete, Insert, Home, End, PageUp, PageDown, arrows
  static RemoteKey? base(LogicalKeyboardKey k);       // a–z, digit0–9, space
  static (RemoteKey, bool)? forChar(String c);        // a–z; A–Z → (letter, shift); 0–9; ' '; '\n' → enter
  /// The inputs one hardware event sends, given what is held and latched (rules of §10.5.1).
  static KeyRoute route(KeyEvent e, {required Set<RemoteKey> latched,
      required bool ctrlAltMetaHeld, required bool upIsSwallowed});
}

@immutable
class KeyRoute {
  final bool handled;                 // KeyEventResult.handled, else ignored
  final List<DesktopInput> inputs;    // empty: nothing to send; > 1: one sendSequence
  final bool consumesLatch;           // the caller calls latch.take() (armed → off)
  final bool swallowUp;               // the caller adds the physical key to _swallowUps
  final RemoteKey? holds;             // tracked in _heldKeys (down) / released (up)
}
```

`RemoteInput.handleHardwareKey` only applies a `KeyRoute`; every rule below
lives in `KeyRouting.route`, which is pure and tested directly (T4b).

#### 10.5.1 Hardware keyboard (`RemoteInput.handleHardwareKey`)

Bound to the picture's `Focus.onKeyEvent` in both viewers (a Bluetooth
keyboard on a phone takes the same path). If `!ready` → `ignored`.
Otherwise the first rule that matches:

1. **Modifier key** → `key{m, down: event is! KeyUpEvent}` (repeats are
   ignored), tracked in `_heldKeys` → `handled`. A physically held modifier
   is a real down on the host and is **never** part of a wrap.
2. **Special key.**
   - Down or repeat with latches active → one wrapped tap
     `[latched↓…, key↓, key↑, latched↑…]` (`latch.take()`); its physical key
     joins `_swallowUps` → `handled`.
   - Up whose physical key is in `_swallowUps` → removed; nothing sent →
     `handled`.
   - Otherwise `key{down: event is! KeyUpEvent}` (repeats are downs), tracked
     → `handled`. Tab never moves Flutter focus while the picture is focused.
3. **Letter, digit or space while Ctrl, Alt or Meta is physically down**
   (`HardwareKeyboard.instance.isControlPressed || isAltPressed || isMetaPressed`)
   **and not AltGr** — `event.character` is null or equals the key's base
   character ignoring case (on German, French or Polish layouts AltGr
   arrives as ControlLeft + AltRight and produces a different character):
   - latched modifiers that are not physically held → one wrapped tap with
     only those, on down; the up is swallowed;
   - else a bare `key{down/up}` of the base key, tracked → `handled`. Never a
     chord with the held modifier: the host injects downs and ups without
     counting (`D/windows.rs:455-475`), so a re-wrapped Ctrl-up would release
     the Ctrl the user still holds. (U11: Ctrl+B, Ctrl+S, Alt+F, Win+E work.)
4. **Printable character** on down or repeat (`event.character`, no C0/C1
   control characters; includes AltGr characters):
   - latches active and `forChar(c)` is a key → `tapKey(key, extra: shift ? {shift} : {})`;
   - latches active and no key for `c` → `text(c)` and `latch.take()` (they
     cannot apply and must not linger);
   - otherwise `text(c)`.

   Its physical key joins `_swallowUps` → `handled`. Shift went as a real
   key under rule 1; the character already carries case and layout.
5. Otherwise `ignored`. That includes keys outside the D17 vocabulary:
   Ctrl+punctuation (Ctrl+-, Ctrl+=, Ctrl+[, Ctrl+], Ctrl+/), PrintScreen,
   CapsLock and the Menu key are not sent. `docs/remote-desktop.md` lists
   them as a known limitation (§16).

Known limitation (as today): characters typed this way are injected as
Unicode, so a remote input method does not compose them; CJK text goes
through `viewerTypeText` (desktop) or the soft keyboard (mobile).

#### 10.5.2 Soft keyboard (`V/remote_text_input.dart`, mobile)

```dart
final class RemoteTextInput implements DeltaTextInputClient {
  RemoteTextInput({required RemoteInput input});
  ValueListenable<bool> get attached;      // a TextInputConnection exists and was not closed
  ValueListenable<String> get composing;   // uncommitted IME text for ComposingPreview
  void show();      // attach if needed (TextInput.attach), setEditingState(sentinel), connection.show()
  void hide();      // connection.close()
  void dispose();
}
```

- It is not a widget and never takes Flutter focus: the picture keeps
  primary focus, so hardware keys go through §10.5.1 and never edit the
  sentinel. `show()` always calls `TextInputConnection.show()`, so the
  keyboard reopens after Android Back hid it.
- `TextInputConfiguration(inputType: TextInputType.multiline, inputAction: TextInputAction.newline, enableDeltaModel: true, autocorrect: false, enableSuggestions: false, enableIMEPersonalizedLearning: false, keyboardAppearance: Brightness.dark, smartDashesType: SmartDashesType.disabled, smartQuotesType: SmartQuotesType.disabled)`.
- Baseline: `sentinel = '1' * 64`, caret at the end (RustDesk's pre-filled
  buffer, rd §3.12: Backspace always yields a deletion delta; digits draw no
  autocorrection).
- `updateEditingValueWithDeltas`:
  - insertion with a collapsed `composing` range → `input.typeText(text)`
    (`'\n'` → `tapKey(enter)`);
  - deletion before the caret → `input.backspace(codePoints)` (≤ 64 per call);
  - replacement → `backspace(old)` then `typeText(new)` (committed text only);
  - non-text update → ignored;
  - while `composing` is not empty nothing is sent and `composing` mirrors
    it; on commit the committed segment is sent;
  - when not composing and the buffer drifted (< 32 or > 512 characters) →
    `setEditingState(sentinel)`.
- One Return is one Enter: `performAction(TextInputAction.newline)` and an
  inserted `'\n'` each → `tapKey(enter)`, except that whichever arrives
  second, as the IME's very next event (a delta batch, a value update or an
  action), is the same Return and sends nothing. iOS sends the action and
  then inserts the `'\n'` in the next delta batch; an Android IME may send
  only the action. Two Returns of the same kind in a row are two Enters.
  `connectionClosed()` → `attached = false` (the gutters return).
- `RemoteInput.typeText(s)`: while any latch is active, each of the first
  `kLatchedTextKeys` (16) characters with `forChar(c) = (key, shift)` goes as
  `tapKey(key, extra: shift ? {shift} : {})`; a character without a key, and
  everything after the 16th character, goes as `text` and consumes armed
  latches. With no latch active, text goes in ≤ 1024-byte `text` events.

### 10.6 Viewport and soft keyboard (P1-4)

```dart
final class RemoteViewport extends ChangeNotifier {
  Size get layoutSize;           // from RemoteSurface's LayoutBuilder; ignores the IME
  EdgeInsets get occlusion;      // IME + modifier bar on mobile; zero on desktop
  double get aspectRatio; set aspectRatio(double v);   // from geometry video size
  Size get scene;                // fit(layoutSize, aspectRatio); independent of occlusion
  double get scale; Offset get offset;
  Rect get visibleRect;          // (Offset.zero & layoutSize) minus occlusion
  ValueListenable<Rect?> get visibleRegion;   // normalised; null when everything is visible
  Offset? toNormalized(Offset local, {bool clamp = false});
  Offset toLocal(Offset normalized);
  void layout(Size size);        // rotation/resize: keep scale and the point at the visible centre
  void setOcclusion(EdgeInsets occlusion);    // re-bounds the offset only
  void zoomAt(Offset focal, double factor); void panBy(Offset delta);
  void pinchPan(List<Offset> start, List<Offset> now);
  void ensureVisible(Offset normalized, {double margin = kFollowMargin});   // minimal pan
  void beginKeyboard(); void endKeyboard();
  bool get navigatedSinceKeyboard;
  void setMode(ViewerScaleMode mode, {required Size remotePixels, required double dpr});
  void autoScrollTo(Offset pointer);
}
```

Bounding per axis: with `extent = scene · scale` and the visible span
`[v0, v1]`, the offset centres the picture in the span when
`extent ≤ v1 − v0`, else it is clamped to `[v1 − extent, v0]`. With the
keyboard up on an 844×390 phone the visible height is 390 − IME − 44, so the
fitted picture becomes pannable vertically without any change of scale.

Keyboard sequence (`TouchViewer` watches `MediaQuery.viewInsetsOf(context).bottom`):

1. inset becomes > 0 → `beginKeyboard()` saves `(scale, offset)` and clears
   `navigatedSinceKeyboard`;
2. **every** inset change → `setOcclusion(EdgeInsets.only(bottom: inset + 44))`
   — re-bound only; `scene` and `layoutSize` stay, so nothing relayouts;
3. `kKeyboardSettle` (100 ms) after the **last** change →
   `ensureVisible(input.cursor)` (minimal pan);
4. inset returns to 0 → `endKeyboard()` restores the saved `(scale, offset)`
   exactly, unless the user pinched or panned meanwhile
   (`navigatedSinceKeyboard`), in which case it keeps the current scale and
   re-bounds.

Nothing here calls the store, changes encode parameters or rebuilds the
texture (P1-4). Trackpad strokes call `ensureVisible(cursor)` so the view
follows the cursor when zoomed (rd §3.13).

### 10.7 Toolbar visibility (`ToolbarController`, `V/viewer_toolbar.dart`)

```dart
class ToolbarController extends ChangeNotifier {
  ToolbarController({required bool pinned});
  bool get pinned; bool get expanded;
  ValueListenable<double> get inset;  // 68 expanded, 28 collapsed (top pills)
  void start();                       // viewer shown
  void setPinned(bool value);         // the caller also persists it (ViewerPrefs)
  void pointerEnter(); void pointerExit();   // region = toolbar + its 12 px top margin
  void touchEnded();                  // pointer up on the toolbar from touch or stylus (no hover)
  void handleEnter(); void handleExit(); void handleTap();
  void hold(); void unhold();         // a menu, the quality panel or a dialog is open
  @override void dispose();           // no timer outlives it
}
```

| Event | Effect |
| --- | --- |
| `start` | expanded; unpinned and the pointer outside → schedule collapse (2 s) |
| `pointerEnter` | inside; cancel collapse |
| `pointerExit` | outside; unpinned and no holds → schedule collapse (2 s) |
| `touchEnded` | unpinned and no holds → schedule collapse (2 s) |
| `hold` | holds + 1; cancel collapse; expanded |
| `unhold` | holds − 1; holds = 0, outside, unpinned → schedule collapse (2 s) |
| collapse timer | unpinned, no holds, outside → collapsed |
| `setPinned(true)` | cancel timers; expanded |
| `setPinned(false)` | outside → schedule collapse |
| `handleEnter` | start the 300 ms hover timer → expanded |
| `handleExit` | cancel the hover timer |
| `handleTap` | expanded now |

Pinned defaults to true (U22). Auto-collapse never writes the preference
(RustDesk keeps manual and automatic collapse apart, rd §3.1 step 7).

### 10.8 Focus, release and visibility

| Event | `input.releaseAll()` | Latches | Viewer demand | Control |
| --- | --- | --- | --- | --- |
| Picture loses focus (menu, dialog, side panel) | yes (`release` only if something is held) | cleared | unchanged | unchanged |
| Window blur / `AppLifecycleState.inactive` | yes | cleared | unchanged (U2) | unchanged |
| Minimised, hidden to the tray, `hidden`, `paused` | yes | cleared | still held; the store pauses itself (perf §9.6.4) | ended by the host on `Pause`; re-requested after a fresh frame (perf §9.6.3) |
| Back to the list (`remote.back()`) | yes | cleared | released (`ViewerHost.close` → pause, D13) | as above |
| Back to the picture (`viewer.open()`) | — | — | acquired (resume + IDR) | re-requested once fresh |
| `geometry.epoch` change (reconnect, generation) | machine cancel | cleared | — | — |
| `ready` true → false | machine cancel | cleared | — | — |
| Trackpad ↔ direct switch | machine cancel | kept | — | — |
| Pointer cancel (system edge gesture) | machine cancel | kept | — | — |
| Disconnect or viewer disposed | yes | cleared | released | — |

Focus returns to the picture after every menu (`onClose`), dialog (`then`),
side panel and sheet (`whenComplete`). A pointer down on the picture
requests focus. The viewers' `AppLifecycleListener` handles local input
only; on-screen state belongs to `ViewerHost`; the app-hidden pause belongs
to the store.

### 10.9 Accessibility and motion

- Picture: `Semantics(container: true, label: viewerPictureLabel(title), hint: touchTrackpadHint | touchDirectHint)`.
- Overlay cards and pills: `Semantics(liveRegion: true)`. Toolbar and gutter
  controls carry tooltips/labels; the mode control reports its selection;
  `KeyCap` reports `toggled` with the value `modifierStateArmed` or
  `modifierStateLocked`; the minimap reports `minimapA11y(region)`.
- Reduced motion (`Motion.reduced`): toolbar slide/fade, the viewer's
  entrance fade, the scan line, pulsing dots and the touch-indicator fade are
  instant or static.
- The dark chrome clamps text scaling at 1.3 (§4.5); management pages scale
  normally.
- Haptics: long-press right click `HapticFeedback.mediumImpact`; drag start
  and latch changes `selectionClick`.
- Known limitation: while the picture has focus, Tab belongs to the remote,
  so keyboard users reach the toolbar only after focus leaves the picture
  (RustDesk behaves the same).

---

## 11. Icons (requirement 3)

### 11.1 `AppIconSize` and `AppIcons` (`U/theme/app_icons.dart`)

```dart
/// The four sizes the icon rules allow (Icons:125-127).
abstract final class AppIconSize {
  static const double inline = 16, button = 18, nav = 20, tile = 24;
}

/// Lucide (ISC), the app's one icon family. Weight follows size: every
/// constant is weight 400 for 16-20 px; the `…Large` ones are weight 300 for
/// 24 px and up. Named for what each glyph means here. Separate const
/// constants, never a runtime weight swap: a non-const IconData breaks
/// icon-font tree shaking.
abstract final class AppIcons {
  // Destinations (20).
  static const IconData overview = LucideIcons.orbit400;
  static const IconData tunnels = LucideIcons.route400;           // was cable
  static const IconData remote = LucideIcons.monitor400;          // new rail/bar entry
  static const IconData relay = LucideIcons.radioTower400;
  static const IconData logs = LucideIcons.scrollText400;
  static const IconData settings = LucideIcons.settings400;
  // Tunnels and paths.
  share = arrowUpFromLine400; connect = arrowDownToLine400; direct = zap400;
  mixedPath = split400; carrierTcp = arrowRightLeft400; carrierQuic = layers400;
  computer = laptop400; service = server400; diagnostics = stethoscope400; router = router400;
  // Actions.
  add = plus400; close = x400; copy = copy400; edit = pencil400; delete = trash2400;
  start = play400; stop = square400; refresh = refreshCw400; search = search400;
  more = ellipsis400; export = fileUp400; import = clipboardPaste400;
  paste = clipboardPaste400;  // same glyph, named by meaning
  external = externalLink400; clear = eraser400; follow = arrowDown400;
  // State.
  check = check400; ok = circleCheck400; info = info400; warning = triangleAlert400;
  error = circleAlert400; online = cloud400 /* was wifi */; offline = cloudOff400 /* was wifiOff */;
  lock = lock400; key = keyRound400; link = link400; unlink = unlink400; show = eye400; hide = eyeOff400;
  // Settings.
  themeSystem = sunMoon400; themeLight = sun400; themeDark = moon400; star = star400;
  update = circleFadingArrowUp400; download = download400;
  // Disclosure.
  chevronDown = chevronDown400; chevronUp = chevronUp400;
  chevronRight = chevronRight400Dir; back = arrowLeft400Dir; forward = arrowRight400Dir;
  // Remote desktop: the 19 of the brief, by meaning.
  screenShare = screenShare400; screenShareOff = screenShareOff400;
  remoteDevices = monitorSmartphone400; viewOnly = eye400; control = mousePointer2400;
  keyboard = keyboard400; shortcuts = command400; rightClick = mouse400;
  trackpad = touchpad400; directTouch = pointer400; qrCode = qrCode400;
  scanQrCode = scanQrCode400; fullscreen = maximize400; pin = pin400;
  disconnect = power400; helper = shieldCheck400; secureDesktop = lockKeyhole400;
  quality = gauge400; phone = smartphone400;
  // Remote desktop companions (same family).
  exitFullscreen = minimize400; unpin = pinOff400;
  // Weight 300, 24 px and up.
  relayLarge = radioTower300; remoteLarge = monitor300; scanQrCodeLarge = scanQrCode300;
}
```

(The listing abbreviates `static const IconData x = LucideIcons.y;`.) Every
identifier was checked in `lucide_icons_flutter-3.1.25/lib/lucide_icons.dart`.
Deleted as unused (0 references): `latency`, `list`, `language`.

Where the remote glyphs are used: `remote` (rail/bar, list tiles, Display
button, viewer title), `screenShare` (paused host Start), `screenShareOff`
(capsule Stop in icon form), `remoteDevices` (host viewer panel; the
viewer's device type is unknown, D24), `viewOnly`/`control` (mode segments,
view-only pill), `keyboard` (mobile keyboard, control hint), `shortcuts`
(both viewers), `rightClick`, `trackpad`/`directTouch` (mobile mode),
`qrCode` (host QR), `scanQrCode` (scan to add), `fullscreen`/`exitFullscreen`,
`pin`/`unpin`, `disconnect`, `helper` (Enable, recognised host),
`secureDesktop` (secure-desktop card), `quality` (quality panel title),
`phone` (`remoteMobileHostNote` row).

### 11.2 Sizes and theme in existing code (UI-0b)

| Where | Now | After |
| --- | --- | --- |
| `U/shell/navigation.dart:147` rail | 18 | `AppIconSize.nav` (20) |
| `U/shell/navigation.dart:296` bar | 20 | unchanged |
| `U/galaxy/galaxy_panel.dart:152` relay | 28 at weight 300 | `AppIcons.relayLarge` at 24 |
| `U/theme/app_theme.dart` base `iconButtonTheme` | M3 default 24 on touch | `iconSize: 18` (desktop already 18, `:611-616`) |
| `U/widgets/surfaces.dart:223` `EmptyNote` | 20 | 18 (a one-line note row, not an empty state) |
| 17 px buttons/menu items: `galaxy_panel.dart:66,339,344,400`, `overview_page.dart:306,313`, `relay/credentials_tab.dart:150,202,302`, `relay/services_tab.dart:341,353`, `settings_page.dart:323`, `tunnels_page.dart:381` | 17 | 18 |
| 17 px field prefix `logs_page.dart:154` | 17 | 16 |
| 13-15 px inline/chips: `galaxy_panel.dart:424`, `logs_page.dart:158`, `relay/connections_tab.dart:212`, `relay/services_tab.dart:271`, `relay_page.dart:316,368,418`, `tunnel_diagnostics.dart:336,537,730`, `tunnel_editor.dart:583,599`, `update_dialog.dart:121`, `widgets/carrier.dart:40`, `widgets/connections.dart:76`, `widgets/path.dart:38,111` | 13-15 | 16 |

Literal sizes are replaced by `AppIconSize` constants where touched. Chips
with 12 px text get 16 px glyphs; check them in the previews.

### 11.3 Material `Icons.*` removed

| Today | Replacement | WP |
| --- | --- | --- |
| `U/shell/window_chrome.dart:111,117,117,126` `remove`, `filter_none`, `crop_square`, `close` | self-drawn `CaptionGlyph` (§12.2) | UI-0a |
| `U/pages/tunnels_page.dart:37` `desktop_windows_outlined` | deleted with the `open-desktop` button | UI-3 |
| `U/pages/desktop_page.dart:102,108,136,189,324` | file deleted; the host card uses `copy`, `screenShareOff`, `screenShare`, `remote` | UI-4 |
| `U/pages/desktop_viewer.dart:347,352,357,368,379,407` | file deleted; `keyboard`, `rightClick`, three-finger scroll, Shortcuts → Release all keys | UI-6 |
| `U/pages/desktop_connection_banner.dart:55,61` | file deleted; overlays | UI-6 |
| Implicit: `AppBar` back, `ExpansionTile`, `DropdownButtonFormField` (`desktop_page.dart:57,194,207`, `desktop_viewer.dart:261`) | `MenuAnchor` + `AppIcons.chevronDown`; no `AppBar` | UI-4, UI-6 |

After UI-6, `grep -rnE '\bIcons\.' ui/lib` finds nothing; UI-8 adds that
grep to `.github/workflows/syntax-check.yml`'s Flutter job (on master it
holds after UI-0a). `cupertino_icons` (`ui/pubspec.yaml:43`, unused) is
removed in UI-0b; `uses-material-design` stays (tree-shaken).

### 11.4 Test fonts and posters (UI-0b)

- `T/support.dart` `loadRealFonts`: add
  `family('packages/lucide_icons_flutter/Lucide400', ['packages/lucide_icons_flutter/assets/build_font/LucideVariable-w400.ttf'])`;
  keep `Lucide300` (the `…Large` glyphs); delete the `MaterialIcons` loader
  and its `FLUTTER_ROOT` lookup (`:114-124`) — a stray Material glyph then
  shows as a box in previews.
- `T/gen_poster_test.dart:110-140`: chips use `AppIcons.direct`,
  `AppIcons.relay`, `AppIcons.key`, `LucideIcons.arrowLeftRight400`,
  `LucideIcons.code400` (16 px → weight 400). Regenerate
  `docs/assets/poster.png` and `poster.zh-CN.png`.

---

## 12. Title bar (requirement 4)

### 12.1 `U/shell/page_frame.dart` `_TitleBar`

UI-0a (cherry-pickable) changes only the padding, the gap and the buttons:

```dart
Padding(
  padding: EdgeInsetsDirectional.only(start: lead, end: drawsCaptionButtons ? 16 : 8),   // was end: 8 (:120)
  child: Row(children: [
    Expanded(child: IgnorePointer(child: Text(title, …))),        // unchanged
    ...actions,
    if (primary != null) ...[ const SizedBox(width: 6), /* FilledButton.icon / IconButton.filled, unchanged */ ],
    if (drawsCaptionButtons) ...[
      const SizedBox(width: 16),                                   // was 10 (:167)
      const CaptionButtons(),                                      // was CaptionButtons(height: 36) (:168)
    ],
  ]),
)
```

UI-3 then adds `onBack` (§4.3) and the capsule:

```dart
Padding(…, child: LayoutBuilder(builder: (context, c) => Row(children: [
  if (onBack != null) ...[ IconButton(key: const Key('page-back'), …), const SizedBox(width: 4) ],
  Expanded(…title…),
  if (sharingIndicator) ...[ const SizedBox(width: 12), SharingIndicator(form: sharingFormFor(c.maxWidth)) ],
  ...actions, …primary…, …captions…,
])))
```

The bar stays 58 tall in the sheet (54 compact); the existing
`Stack(alignment: centerStart)` centres the row, so the 32 px buttons sit 13
px from top and bottom. In the sheet the close button's right edge is 16 px
inside the sheet edge (8 px sheet inset + 16 = 24 px from the window edge)
at every scale factor and window state: the acceptance criterion holds by
construction.

### 12.2 `U/shell/window_chrome.dart`

```dart
enum CaptionTone { surface, viewer }
enum CaptionGlyph { minimize, maximize, restore, close }
const Size kCaptionButtonSize = Size(46, 32);
const double kCaptionButtonRadius = 6;
const double kCaptionGlyphExtent = 10;

class CaptionButtons extends StatefulWidget {
  const CaptionButtons({super.key, this.tone = CaptionTone.surface});   // `height` removed
  final CaptionTone tone;
}
class CaptionGlyphIcon extends LeafRenderObjectWidget {                 // RenderCaptionGlyph, 10×10 logical
  const CaptionGlyphIcon({super.key, required this.glyph, required this.color});
}
```

- `_CaptionButtonsState with WindowListener`: as today, plus `onWindowRestore`,
  `onWindowEnterFullScreen` and `onWindowLeaveFullScreen` → `_sync()`. Row of
  three 46×32 buttons, no gaps: keys `caption-minimize`, `caption-maximize`,
  `caption-close`; close still calls `windowManager.close()` (hides to the
  tray).
- `_CaptionButton({required CaptionGlyph glyph, required String tooltip, required VoidCallback onTap, required CaptionTone tone, bool close = false})`
  (`icon`, `iconSize`, `height` removed): `Tooltip` →
  `Semantics(button: true, label: tooltip)` → `MouseRegion` (hover) →
  `GestureDetector(behavior: opaque, onTapDown/Up/Cancel → pressed, onTap)` →
  `AnimatedContainer(duration: Motion.reduced ? 0 : 90 ms, width: 46, height: 32, BoxDecoration(color: bg, borderRadius: 6))`
  → `Center(CaptionGlyphIcon(glyph, color: fg))`.

| Tone | Idle fg | Hover bg / fg | Pressed bg |
| --- | --- | --- | --- |
| surface | `onSurfaceVariant` | `onSurface @ 0.08` / `onSurface` (today's) | `onSurface @ 0.12` |
| viewer | `ViewerPalette.textSecondary` | `white @ 0.09` / `ViewerPalette.text` | `white @ 0.14` |
| close, both | (tone idle) | `#C42B1C` / white (today's `:167`) | `#C42B1C @ 0.9` / `white @ 0.9` |

UI-0a (the cherry-pickable title-bar commit) ships `CaptionTone` with
`surface` only, because `ViewerPalette` does not exist on master or before
UI-1. UI-1 adds `CaptionTone.viewer` in the same commit that creates
`U/theme/viewer_palette.dart` (one owner holds both files in that phase).

`RenderCaptionGlyph` paints in device pixels (DS §2.14):

1. `g = localToGlobal(Offset.zero)`; shift by
   `((g * dpr).round() − g * dpr) / dpr` so the box starts on a device pixel;
   `canvas.scale(1 / dpr)` and draw in device pixels. `dpr` comes from
   `MediaQuery.devicePixelRatioOf(context)`; the render object repaints when
   glyph, colour or dpr change.
2. Stroke `s = max(1, (1 * dpr).round())` (1 px at 100 % and 125 %, 2 px at
   150 % and 200 %); box `B = (10 * dpr).round()` (10, 13, 15, 20); half
   `h = s / 2`; corner `r = max(1, dpr.round())`; restore offset
   `o = (2 * dpr).round()`. `Paint(style: stroke, strokeWidth: s, isAntiAlias: true, strokeCap: StrokeCap.butt)`.
3. Glyphs (the artboard's 10 px grid, Icons:72-89, scaled to `B`):
   - minimize: line `(0, y)–(B, y)`, `y = (B / 2).floor() + (s.isOdd ? 0.5 : 0)`;
   - maximize: `RRect.fromLTRBR(h, h, B − h, B − h, r)`;
   - restore: front `RRect.fromLTRBR(h, o + h, B − o − h, B − h, r)`; back =
     the visible top-right part of a square offset by `o`:
     `M(o+h, o+h) L(o+h, h+r)` arc to `(o+h+r, h)`, `L(B−h−r, h)` arc to
     `(B−h, h+r)`, `L(B−h, B−o−h−r)` arc to `(B−h−r, B−o−h)`, `L(B−o−h, B−o−h)`;
   - close: `(h, h)–(B−h, B−h)` and `(B−h, h)–(h, B−h)` (diagonals stay
     antialiased).

The viewer uses `CaptionButtons(tone: CaptionTone.viewer)` in its 36 px bar
(2 px above and below, 4 px end padding). `U/pages/setup_flow.dart:196-201`
becomes `Positioned(top: 6, right: 16, child: CaptionButtons())` — centred
in its 44 px drag strip, 16 px from the window edge as in the shell. The
`Icons` import leaves `window_chrome.dart`.

---

## 13. Localisation

Rules:

- UI-1 adds **every** key below to `ui/lib/l10n/app_en.arb` and
  `app_zh.arb` (placeholders typed in `@key` metadata; `plural`/`select`
  where shown) and runs `flutter gen-l10n`; no later package edits the ARB
  files except UI-8, which deletes the old keys, and the append-only
  exception of WORKPLAN X10. This keeps the ARB files out of the parallel
  tracks.
- Placement: shell and tray keys go right after the `nav*` group
  (`app_en.arb:65-68` and `navRelay` at `:429`); every remote-desktop block
  goes right after the `window*` group (`:363-366`) in this order: remote,
  add/scan, host/setup, viewer, quality, shortcut, touch/modifier, gesture.
  Same positions in `app_zh.arb`.
- zh uses **共享** (never 分享) for sharing.
- UI-8: every key whose name starts with `desktop` is deleted (93 today;
  `grep -c '"desktop' ui/lib/l10n/app_en.arb` → 0) and
  `l10n_untranslated.json` is empty.
- Key caps (`Ctrl`, `Alt`, `Shift`, `Win`, `Esc`, `Tab`, `Fn`, `F1`…`F12`,
  `Home`, `End`, `PgUp`, `PgDn`, `Ins`, `Del`, `Ctrl+C`, `Ctrl+V`, arrows) and
  shortcut hints (`Win + D` …) are mono literals, not ARB keys.

Placeholder types: `when`, `path`, `name`, `time`, `duration`,
`fingerprint`, `reason`, `keys`, `mode`, `codec`, `w`/`h` as `String` where
pre-formatted; `rtt`, `fps`, `n`, `attempt`, `minutes`, `hours` as `int`;
`count`, `seconds` as `int` (plural); `dir`, `region` as `String` (select).

### 13.1 Old texts carried into new keys

| Old key | New key |
| --- | --- |
| `desktopTitle` | `navRemoteDesktop` |
| `desktopAllowControl` | `hostAllowControl` (new value) |
| `desktopDisplay` | `hostDisplay` (new value) |
| `desktopEnable` | `hostEnable` |
| `desktopHelperInstall` / `Disable` / `Uninstall` | `hostHelperInstall` / `hostHelperDisable` / `hostHelperUninstall` (values kept) |
| `desktopService` | `hostServiceName` |
| `desktopStop` | `hostStop` (zh 停止共享) |
| `desktopStopHelp` | `hostStopNote` (new value) |
| `desktopTrustRevoke` | `hostRevoke` (zh 共享) |
| `desktopHostUnsupported` | `hostUnsupported` |
| `desktopHelperStopped` | `hostHelperStopped` |
| `desktopOrdinaryOnly` | `hostHelperNone` |
| `desktopInvitation` | `inviteLabel` |
| `desktopOpen` | `remoteOpen` |
| `desktopRelayOnly` | `remoteRelayOnly` |
| `desktopDisconnect` | `viewerDisconnect` |
| `desktopIdentityMismatch` / `…Help` | `viewerIdentityTitle` / `viewerIdentityBody` (new value) |
| `desktopKeyboard` | `viewerTypeText` |
| `desktopSendText` | `viewerSendText` |
| `desktopRelease` | `shortcutReleaseKeys` |
| `desktopRightClick` | `touchRightClick` |
| `desktopTouchMode` / `desktopTrackpadMode` | `touchModeDirect` / `touchModeTrackpad` |
| `desktopStateWaitingDesktop` | `viewerWaitingBody` (new value) |

All other `desktop*` keys have no successor (their widgets are gone;
`desktopGestureHelp` described the old gestures). Reused unchanged: `copy`,
`copied`, `cancel`, `close`, `connect`, `more`, `refresh`, `retry`,
`pathDirect`, `pathRelay`, `navLogs`, `relayLatency`, `window*`, `tray*`.

### 13.2 New keys

Shell and tray

| Key | en | zh |
| --- | --- | --- |
| `navRemoteDesktop` | Remote desktop | 远程桌面 |
| `navRemote` | Remote | 远程 |
| `back` | Back | 返回 |
| `settingsLogsHint` | What pb-mapper is doing, for troubleshooting | 运行日志，排查问题时查看 |
| `sharingBeingViewed` | Being viewed | 正在被查看 |
| `sharingBeingControlled` | Being controlled | 正在被控制 |
| `sharingViewerPaused` | Connected · paused | 已连接 · 画面已暂停 |
| `hostDisconnectViewer` | Disconnect viewer | 断开查看者 |
| `trayHostSharing` | Sharing this computer | 正在共享这台电脑 |
| `trayHostViewed` | Someone is viewing this computer | 有人正在查看这台电脑 |
| `trayHostControlled` | Someone is controlling this computer | 有人正在控制这台电脑 |
| `trayHostPaused` | A viewer is connected; the picture is paused | 查看者已连接，画面已暂停 |

Remote page and list

| Key | en | zh |
| --- | --- | --- |
| `remoteAddComputer` | Add computer | 添加电脑 |
| `remoteListTitle` | Remote computers | 远程电脑 |
| `remoteEmptyTitle` | No remote computers yet | 还没有远程电脑 |
| `remoteEmptyBody` | Turn on remote control in pb-mapper on the other computer, copy its private invitation and paste it here. | 在另一台电脑的 pb-mapper 里开启远程控制，复制它的私密邀请，粘贴到这里。 |
| `remoteAddHint` | Paste another computer's private invitation | 粘贴另一台电脑的私密邀请 |
| `remoteRememberNote` | Only invitations you choose to remember are saved on this device, encrypted by the system. | 只有选择“记住此电脑”的邀请会保存在本机，并由系统加密。 |
| `remoteViewing` | Viewing | 正在查看 |
| `remoteOnline` | Online | 在线 |
| `remoteNotSharing` | Not sharing | 未共享 |
| `remoteOfflineHint` | This computer isn't sharing right now | 这台电脑当前未在共享 |
| `remoteOtherRelay` | Other relay | 其他中继 |
| `remoteOtherRelayHint` | Saved under another relay or credential. Switch back in Settings to open it. | 保存于其他中继或凭据下，在“设置”中切换回去后才能打开。 |
| `remoteOpen` | Open | 打开 |
| `remoteBackToPicture` | Back to picture | 回到画面 |
| `remoteLast` | Last {when} | 上次 {when} |
| `remoteLastPath` | Last {when} · {path} | 上次 {when} · {path} |
| `remoteViewingMeta` | {path} · {rtt} ms · {fps} fps | {path} · {rtt} ms · {fps} fps |
| `remotePicturePaused` | {path} · picture paused | {path} · 画面已暂停 |
| `remoteToday` | Today {time} | 今天 {time} |
| `remoteYesterday` | Yesterday {time} | 昨天 {time} |
| `remoteRename` | Rename | 重命名 |
| `remoteRemove` | Remove this computer | 移除此电脑 |
| `remoteRemoveConfirmTitle` | Remove “{name}”? | 移除“{name}”？ |
| `remoteRemoveConfirmBody` | Its saved invitation and last picture are deleted from this device. | 保存在本机的邀请和上次画面会被删除。 |
| `remoteRemoveAction` | Remove | 移除 |
| `remoteRememberFull` | You can remember up to 50 computers. Remove one first. | 最多可以记住 50 台电脑，请先移除一台。 |
| `remoteVaultUnreadable` | The computers saved on this device couldn't be opened. Clear them to remember computers again. | 无法打开本机保存的电脑。清除它们后才能再记住电脑。 |
| `remoteVaultReset` | Clear saved computers | 清除已保存的电脑 |
| `remoteVaultResetTitle` | Clear saved computers? | 清除已保存的电脑？ |
| `remoteVaultResetBody` | Every saved computer is deleted from this device, with its invitation and last picture. To view one again, add it with a new invitation. | 本机保存的每台电脑及其邀请和上次画面都会被删除。要再次查看某台电脑，请用新的邀请重新添加。 |
| `remoteVaultResetAction` | Clear | 清除 |
| `remoteOfflineToast` | “{name}” isn't sharing right now | “{name}”当前未在共享 |
| `remoteSwitchTitle` | Disconnect “{name}”? | 断开“{name}”？ |
| `remoteSwitchBody` | Only one remote computer can be open at a time. | 一次只能打开一台远程电脑。 |
| `remoteSwitchAction` | Disconnect and open | 断开并打开 |
| `remoteLastPicture` | Last picture · {when} | 上次画面 · {when} |
| `remotePathPermission` | {path} · can view and control | {path} · 可以查看和控制 |
| `remotePathViewOnly` | {path} · view only | {path} · 仅查看 |
| `remoteOthers` | Other computers | 其他电脑 |
| `remoteOnlineLast` | Online · last via {path} | 在线 · 上次走{path} |
| `remoteOfflineLast` | Not sharing · last {when} | 未在共享 · 上次 {when} |
| `remoteScanAdd` | Scan to add | 扫码添加 |
| `remotePasteInvite` | Paste invitation | 粘贴邀请 |
| `remoteMobileHostNote` | This phone can view and control Windows computers; it can't be controlled itself. | 手机可以查看并控制 Windows 电脑，暂不能被远程控制。 |
| `remoteRelayOnly` | Relay only | 仅使用中继 |
| `hostUnsupported` | Only Windows can share its desktop for now. This device can view remote desktops. | 目前仅 Windows 可以共享桌面。此设备可以查看远程桌面。 |

Add computer and scan

| Key | en | zh |
| --- | --- | --- |
| `addTitle` | Add a remote computer | 添加远程电脑 |
| `inviteLabel` | Private invitation | 私密邀请 |
| `paste` | Paste | 粘贴 |
| `addRecognized` | Host identity recognised · {fingerprint} · via the current relay | 已识别主机身份 · {fingerprint} · 通过当前中继连接 |
| `addInvalid` | This isn't a valid remote desktop invitation. | 这不是有效的远程桌面邀请。 |
| `addSetupString` | This is a pb-mapper setup string, not a remote desktop invitation. Paste it in Settings. | 这是 pb-mapper 的设置字符串，不是远程桌面邀请。请到“设置”里粘贴。 |
| `addDuplicate` | Already in your list as “{name}”. | 已在列表中：“{name}”。 |
| `addIdentityConflict` | “{name}” was saved with a different identity. | “{name}”保存的身份与这条邀请不同。 |
| `addReplaceConfirm` | Replace the saved invitation | 替换保存的邀请 |
| `addOtherComputer` | This invitation belongs to another computer and will be added as a new one. | 这条邀请属于另一台电脑，会作为新电脑添加。 |
| `addName` | Name | 名称 |
| `addRemember` | Remember this computer | 记住此电脑 |
| `addRememberHelp` | Saved encrypted on this device; remove it anytime | 邀请加密保存在本机，可以随时移除 |
| `addRememberHelpMobile` | Saved encrypted on this phone | 邀请加密保存在这台手机上 |
| `addRememberUnavailable` | This device can't store invitations securely, so it won't remember this computer. | 这台设备无法安全保存邀请，因此不会记住这台电脑。 |
| `addMoreOptions` | More options (relay only) | 更多选项（仅使用中继） |
| `scanHint` | Point at the QR code on the computer's Remote desktop page | 对准电脑“远程桌面”页上的二维码 |
| `scanOr` | or | 或 |
| `cameraNote` | The camera only reads the QR code; nothing is saved or uploaded. | 相机只用来识别二维码，画面不会保存或上传。 |
| `scanPermissionDenied` | Allow camera access in system settings to scan. You can still paste the invitation. | 需要在系统设置里允许使用相机才能扫码。仍然可以粘贴邀请。 |
| `scanUnavailable` | No camera is available on this device. | 这台设备没有可用的相机。 |

Host card and setup

| Key | en | zh |
| --- | --- | --- |
| `hostThisComputer` | This computer | 这台电脑 |
| `hostOff` | Remote control is off | 尚未开启远程控制 |
| `hostSharing` | Sharing | 正在共享 |
| `hostViewersWatching` | `{count, plural, =1{1 viewer is watching} other{{count} viewers are watching}}` | `{count, plural, other{{count} 个查看者正在查看}}` |
| `hostViewersControlling` | `{count, plural, =1{1 viewer is in control} other{{count} viewers are in control}}` | `{count, plural, other{{count} 个查看者正在控制}}` |
| `hostViewerMeta` | {path} · connected {duration} | {path} · 已连接 {duration} |
| `hostDurationJustNow` | just now | 刚刚 |
| `hostDurationMinutes` | {minutes} min | {minutes} 分钟 |
| `hostDurationHours` | {hours} h {minutes} min | {hours} 小时 {minutes} 分钟 |
| `hostMakeViewOnly` | Make view-only | 改为仅查看 |
| `hostPermissionLabel` | People with the invitation can | 持有邀请的人可以 |
| `hostPermissionView` | View only | 仅查看画面 |
| `hostPermissionControl` | View and control | 查看并控制 |
| `hostQr` | QR code | 二维码 |
| `hostQrHide` | Hide QR code | 隐藏二维码 |
| `hostQrCaption` | Scan with a phone to add this computer | 用手机扫描，添加这台电脑 |
| `hostInviteHelp` | Send it only to devices you trust. Anyone holding it can watch, and control when you allow it. | 只发给你信任的设备。持有邀请的人可以查看，允许控制时也可以操作。 |
| `hostInviteCopied` | Invitation copied | 已复制邀请 |
| `hostDisplay` | Shared display | 共享的显示器 |
| `hostDisplayValue` | Display {n} · {w} × {h} | 显示器 {n} · {w} × {h} |
| `hostDisplaySwitchTitle` | Switch the shared display? | 切换共享的显示器？ |
| `hostDisplaySwitchBody` | Switching displays reconnects the current viewer. | 切换显示器会让当前查看者重新连接。 |
| `hostDisplaySwitchAction` | Switch | 切换 |
| `hostBackground` | Runs in background | 后台运行 |
| `hostBackgroundService` | Windows service · keeps sharing after the window closes | Windows 服务 · 关闭窗口后继续共享 |
| `hostBackgroundApp` | With the app · keeps sharing from the tray after the window closes | 随应用运行 · 关闭窗口后在托盘继续共享 |
| `hostBackgroundStopFirst` | Stop background sharing to change this | 请先停止后台共享再修改 |
| `hostSecure` | UAC and lock screen | UAC 与锁屏 |
| `hostHelperReady` | Helper service ready | 辅助服务已就绪 |
| `hostHelperStopped` | Helper service stopped | 辅助服务已停止 |
| `hostHelperNone` | Ordinary desktop only | 仅普通桌面 |
| `hostHelperUnverified` | Helper service not verified | 辅助服务未验证 |
| `hostHelperUpdate` | Helper service needs an update | 需要更新辅助服务 |
| `hostHelperUpdateAction` | Update | 更新 |
| `hostHelperInstall` | Install / update and restart app | 安装／更新并重启应用 |
| `hostHelperDisable` | Disable helper | 停用辅助服务 |
| `hostHelperUninstall` | Uninstall helper | 卸载辅助服务 |
| `hostStop` | Stop sharing | 停止共享 |
| `hostStart` | Start sharing | 开启共享 |
| `hostStopNote` | The invitation is kept and works again when you start sharing | 停止后邀请保留，下次开启继续可用 |
| `hostServiceName` | Desktop service name… | 桌面服务名称… |
| `hostServiceRenameBody` | Computers that saved the old invitation will need a new one. | 已保存旧邀请的电脑需要新的邀请。 |
| `hostRenameStartFirst` | Start sharing to change this | 请先开启共享再修改 |
| `hostRevoke` | Revoke invitation and stop sharing | 撤销邀请并停止共享 |
| `hostRevokeConfirmTitle` | Revoke the invitation? | 撤销邀请？ |
| `hostRevokeConfirmBody` | Everyone holding the current invitation loses access. A new invitation is created the next time you turn sharing on. | 持有当前邀请的人都会失去访问权限。下次开启共享时会生成新的邀请。 |
| `hostRevokeAction` | Revoke and stop sharing | 撤销并停止共享 |
| `hostRevokeUnreadable` | Revoke the saved invitation | 撤销已保存的邀请 |
| `hostAllowControl` | Allow remote mouse and keyboard | 允许远程操作鼠标和键盘 |
| `hostEnable` | Turn on remote control | 开启远程控制 |
| `hostEnabling` | Turning on… | 正在开启… |
| `hostRestarting` | Restarting the app… | 正在重启应用… |
| `setupHeadline` | See and control this computer from your phone or another computer, anytime | 用手机或另一台电脑，\n随时看到并操作这台电脑 |
| `setupStep1Title` | Windows asks for administrator approval once | Windows 请求一次管理员批准 |
| `setupStep1Body` | Installs the protected helper so UAC prompts and the lock screen work remotely. | 安装受保护的辅助服务，远程时也能操作 UAC 提示和锁屏。 |
| `setupStep2Title` | The app restarts and comes back here | 应用自动重启并回到这里 |
| `setupStep2Body` | Your tunnels and settings stay as they are. | 已有的隧道和配置保持不变。 |
| `setupStep3Title` | You get a private invitation | 得到一条私密邀请 |
| `setupStep3Body` | Copy it to another computer or scan it with a phone. It works until you revoke it. | 复制给另一台电脑，或让手机扫码。之后一直有效，直到你撤销。 |
| `setupOrdinaryOnly` | Share the ordinary desktop only (no helper service) | 只共享普通桌面（不安装辅助服务） |
| `setupBackgroundNote` | Keeps sharing in the background after the window closes | 关闭窗口后仍在后台共享 |

Viewer (both)

| Key | en | zh |
| --- | --- | --- |
| `viewerBackToList` | Back to computers | 回到电脑列表 |
| `viewerToolbar` | Remote desktop toolbar | 远程桌面工具栏 |
| `viewerPin` | Pin toolbar | 固定工具栏 |
| `viewerUnpin` | Unpin toolbar | 取消固定工具栏 |
| `viewerExpand` | Show toolbar | 展开工具栏 |
| `viewerModeView` | View | 查看 |
| `viewerModeControl` | Control | 控制 |
| `viewerDisplay` | Display | 显示 |
| `viewerScaleLabel` | Scaling | 缩放方式 |
| `viewerScaleFit` | Fit window | 适应窗口 |
| `viewerScaleOriginal` | Original size | 原始大小 |
| `viewerQualityLabel` | Picture preference | 画质偏好 |
| `viewerQualityBest` | Best quality | 画质优先 |
| `viewerQualityBestHint` | Sharper, lower frame rate | 更清晰，帧率较低 |
| `viewerQualityBalanced` | Balanced | 平衡 |
| `viewerQualityBalancedHint` | Recommended | 推荐 |
| `viewerQualitySmooth` | Smoothest | 流畅优先 |
| `viewerQualitySmoothHint` | Most responsive, softer picture | 最跟手，画面较柔和 |
| `viewerQuality` | Connection quality | 连接质量 |
| `viewerShortcuts` | Shortcuts | 快捷键 |
| `viewerFullscreen` | Full screen | 全屏 |
| `viewerExitFullscreen` | Exit full screen | 退出全屏 |
| `viewerDisconnect` | Disconnect | 断开 |
| `viewerTypeText` | Type text… | 输入文字… |
| `viewerSendText` | Send | 发送 |
| `viewerControlHint` | Keyboard and mouse go to {name} | 键盘和鼠标正发往{name} |
| `viewerControlHintSub` | Send system shortcuts from the toolbar | 系统快捷键请从工具栏发送 |
| `viewerViewOnlyByHost` | View only · control not allowed | 仅查看 · 对方未允许控制 |
| `viewerConnecting` | Connecting to {name} | 正在连接 {name} |
| `viewerStepFind` | Finding the host | 正在寻找主机 |
| `viewerStepFoundDirect` | Host found, going direct | 找到主机，走直连路径 |
| `viewerStepFoundRelay` | Host found, going through the relay | 找到主机，走中继路径 |
| `viewerStepIdentity` | Checking the host's identity | 校验主机身份 |
| `viewerStepFirstFrame` | Waiting for the first picture | 等待第一帧画面 |
| `viewerSlow` | {path} · picture delay | {path} · 画面延迟 |
| `viewerSecureTitle` | The remote computer is on its secure desktop | 远程电脑正在显示安全桌面 |
| `viewerSecureBody` | A UAC prompt or the lock screen is showing. Without the helper service it can't be shown; viewing resumes when the ordinary desktop returns. | 出现了 UAC 提示或锁屏。这台电脑没有启用辅助服务，无法显示这一画面；恢复到普通桌面后自动继续。 |
| `viewerWaitingTitle` | Waiting for the remote desktop | 等待远程桌面 |
| `viewerWaitingBody` | The remote desktop isn't available right now. Viewing resumes when it returns. | 远程桌面暂时不可用，恢复后自动继续。 |
| `viewerInputPaused` | Input paused | 已暂停输入 |
| `viewerRetry` | `Connection lost · retry {attempt} in {seconds, plural, =1{1 second} other{{seconds} seconds}}` | `连接中断，{seconds, plural, other{{seconds} 秒}}后第 {attempt} 次重连` |
| `viewerReconnecting` | Reconnecting, attempt {attempt}… | 正在第 {attempt} 次重连… |
| `viewerRetryNow` | Retry now | 立即重试 |
| `viewerBusy` | Another device is viewing that computer; retrying | 对方正在被另一台设备查看，稍后自动重试 |
| `viewerRecovering` | Recovering the picture | 正在恢复画面 |
| `viewerIdentityTitle` | Host identity changed | 主机身份已变化 |
| `viewerIdentityBody` | This computer no longer matches the invitation, so reconnecting stopped. Ask for a new invitation and stop using the old one. | 这台电脑的身份与邀请里记录的不一致，已停止自动重连。请向对方要一条新的邀请，不要继续使用旧邀请。 |
| `viewerPasteNewInvite` | Paste new invitation | 粘贴新邀请 |
| `viewerEndedTitle` | Connection ended | 连接已结束 |
| `viewerEndedStopped` | The other computer stopped sharing. | 对方已停止共享。 |
| `viewerEndedByHost` | The other computer ended this session. | 对方结束了这次连接。 |
| `viewerEndedRevoked` | The other computer revoked this invitation. Ask for a new one. | 对方已撤销这条邀请，请向对方要一条新的邀请。 |
| `viewerEndedVersion` | The other computer runs a different pb-mapper version. Update both to the latest. | 对方的 pb-mapper 版本不同，请两边都更新到最新版本。 |
| `viewerEndedDecoder` | This device couldn't decode the picture. | 这台设备无法解码画面。 |
| `viewerEndedFailed` | The connection failed. | 连接失败。 |
| `viewerVaultUnavailable` | This computer's saved invitation can't be read on this device. Remove it and add it again. | 无法在本机读取这台电脑保存的邀请，请移除后重新添加。 |
| `viewerDenied` | The invitation is no longer valid. Ask for a new one. | 邀请已失效，请向对方要一条新的邀请。 |
| `viewerReconnect` | Reconnect | 重新连接 |
| `viewerPictureLabel` | Remote screen of {name} | {name} 的远程画面 |
| `qualityPath` | Path | 路径 |
| `qualityRtt` | Network round trip | 网络往返 |
| `qualityLatency` | Picture delay | 画面延迟 |
| `qualityFps` | Display rate | 显示帧率 |
| `qualityBitrate` | Bitrate | 码率 |
| `qualityResolution` | Resolution | 分辨率 |
| `qualityCodec` | Codec | 编码 |
| `qualityHardware` | {codec} hardware | {codec} 硬件 |
| `qualitySoftware` | {codec} software | {codec} 软件 |
| `qualityAdaptBitrate` | Bitrate lowered to keep input responsive | 已自动降低码率，优先保证操作跟手 |
| `qualityAdaptFps` | Frame rate lowered until the network recovers | 已降低帧率，等待网络恢复 |
| `shortcutShowDesktop` | Show desktop | 显示桌面 |
| `shortcutSwitchWindow` | Switch window | 切换窗口 |
| `shortcutTaskManager` | Task Manager | 任务管理器 |
| `shortcutStart` | Start menu | 开始菜单 |
| `shortcutLock` | Lock remote computer | 锁定远程电脑 |
| `shortcutUnsupported` | Not supported | 暂不支持 |
| `shortcutRefresh` | Refresh picture | 刷新画面 |
| `shortcutReleaseKeys` | Release all keys | 释放所有按键 |
| `shortcutLocked` | Remote computer locked | 远程电脑已锁定 |
| `shortcutLockFailed` | Couldn't lock the remote computer: {reason} | 无法锁定远程电脑：{reason} |

Mobile viewer

| Key | en | zh |
| --- | --- | --- |
| `touchBack` | Back to the remote list | 返回远程列表 |
| `touchQualityA11y` | Connection quality: {path}, {fps} frames per second | 连接质量：{path}，{fps} 帧每秒 |
| `touchModeTrackpad` | Trackpad | 触控板 |
| `touchModeDirect` | Direct touch | 直接触摸 |
| `touchModeToggleA11y` | Switch input mode, now {mode} | 切换操作方式，当前为{mode} |
| `touchTrackpadHint` | Trackpad: slide one finger to move the pointer, tap to click | 触控板模式：单指滑动移动光标，轻点单击 |
| `touchDirectHint` | Direct touch: tap where you want to click | 直接触摸模式：点到哪里就点击哪里 |
| `touchKeyboard` | Keyboard | 键盘 |
| `touchRightClick` | Right click | 右键 |
| `touchControls` | Remote controls | 远程操作 |
| `touchModifierBar` | Modifier keys and common keys | 修饰键与常用键 |
| `touchHideKeyboard` | Hide keyboard | 收起键盘 |
| `modifierArmed` | {keys}: sent with your next key or tap | {keys}：下一个键或点击会带上 {keys} |
| `modifierLocked` | {keys} locked · tap again to release | {keys} 已锁定 · 再点一次解除 |
| `modifierStateArmed` | next key | 下一个键 |
| `modifierStateLocked` | locked | 已锁定 |
| `keyArrow` | `{dir, select, left{Left arrow} right{Right arrow} up{Up arrow} other{Down arrow}}` | `{dir, select, left{左方向键} right{右方向键} up{上方向键} other{下方向键}}` |
| `minimapA11y` | `Minimap: showing the {region, select, topLeft{top left} top{top} topRight{top right} left{left} right{right} bottomLeft{bottom left} bottom{bottom} bottomRight{bottom right} other{centre}} of the remote screen` | `小地图：当前显示远程屏幕的{region, select, topLeft{左上} top{上} topRight{右上} left{左} right{右} bottomLeft{左下} bottom{下} bottomRight{右下} other{中间}}部分` |
| `gestureTitle` | Input mode | 操作方式 |
| `gestureTrackpadDescription` | Slide anywhere to move the pointer — best for precise work. | 手指在屏幕任意位置滑动来移动光标，适合精细操作。 |
| `gestureDirectDescription` | Direct touch: tap where you want to click and drag with one finger; the other gestures are the same. Best for buttons and menus. | 直接触摸：点到哪里就在哪里点击，单指拖动即拖拽，其余手势相同。适合点按钮和菜单。 |
| `gestureSlide` | Slide one finger | 单指滑动 |
| `gestureSlideResult` | Move pointer | 移动光标 |
| `gestureTap` | Tap | 单指轻点 |
| `gestureTapResult` | Left click | 左键单击 |
| `gestureTapHereResult` | Click there | 在那里点击 |
| `gestureLongPress` | Long press | 长按 |
| `gestureLongPressResult` | Right click | 右键单击 |
| `gestureDoubleTapHold` | Double-tap, hold and move | 轻点两下并按住移动 |
| `gestureDragResult` | Drag | 拖拽 |
| `gestureThreeFinger` | Three-finger swipe up or down | 三指上下滑动 |
| `gestureThreeFingerResult` | Scroll | 滚轮 |
| `gestureTwoFingerMove` | Two-finger move | 双指移动 |
| `gestureTwoFingerMoveResult` | Pan picture | 移动画面 |
| `gesturePinch` | Pinch | 双指捏合 |
| `gesturePinchResult` | Zoom picture | 缩放画面 |
| `virtualMouse` | Show virtual mouse | 显示虚拟鼠标 |
| `virtualMouseHelp` | Left and right buttons and a wheel in direct touch | 直接触摸时提供左右键和滚轮 |
| `gotIt` | Got it | 知道了 |

---

## 14. Tests

Minimal and deliberate: each test guards a state machine, a security or
performance boundary, or a flow that crosses layers. No wording, colour,
glyph or layout-matrix tests.

### 14.1 New or rewritten

| Id | File (WP) | Test | Guards |
| --- | --- | --- | --- |
| T1c | `T/app_test.dart` (UI-3) | "the title bar never overflows while this computer is viewed" — Windows override, 720×640 and 1180×780, `FakeDesktopApi.hostSnapshot` with a viewer: every rail page pumps with no exception and shows `sharing-indicator` | §4.6 capsule forms |
| T2 | `T/remote_surface_test.dart` (UI-5; replaces `desktop_gesture_test.dart`) | "zoom stays local through pinch, reconnects and the soft keyboard" — pinch: no sink call, same `Texture` element, scale > 1.2, a later direct tap maps through the zoom; a `geometry.epoch` change keeps the transform; `beginKeyboard()` + `setOcclusion(bottom: 200)` keeps `scale`, `scene` and the element; `endKeyboard()` restores the transform exactly unless the user panned meanwhile | P1-4 (the perf contract assigns this test to the UI) |
| T3 | `T/sharing_test.dart` (UI-3) | "while viewed, Disconnect viewer and Stop sharing are reachable in the title bar, over a dialog and without a title bar, and work while busy" — `sharing-indicator-stop` on Tunnels while `busy` calls `host_stop`; with a dialog open `sharing-fallback-disconnect` is hit-testable above the barrier and calls `host_end_session`; on the setup flow the fallback shows | D15, local stop boundary |
| T4a | `T/remote_input_test.dart` (UI-5) | "latched modifiers wrap one action and never stay held" — Ctrl tapped once: `tapKey(c)` sends one sequence `[control↓, c↓, c↑, control↑]` and the latch is off; tapped twice: two keys both wrapped, still locked; a click is wrapped; a hardware `F5` with Ctrl latched is wrapped and its key-up sends nothing; a dropped sequence leaves nothing held; `releaseAll` sends `release` only when something is held | D16 |
| T4b | `T/remote_input_test.dart` (UI-5) | "held Ctrl sends bare letter keys and AltGr characters stay text" — `KeyRouting.route` (pure): Ctrl held + `b` → `[key b↓]` then `[key b↑]` (no Ctrl in it); Ctrl+AltRight + `q` producing `@` → `text('@')`; Shift + `a` producing `A` → `text('A')` | U11, AltGr, `D/windows.rs:455-475` |
| T4c | `T/remote_input_test.dart` (review fix D) | "a trackpad drag presses at the double tap and takes the host to where the finger slid while the press waited" — over `ViewerHarness` and the real `RemoteInput`: the delayed press lands at the double tap, the following move at the slid-to point (both the `pressDueAt` timeout and an `up` while the press is pending), and a quick lift drags rather than clicks | §10.2 delayed drag press, `RemoteInput.press` prediction reset |
| T5 | `T/remote_text_input_test.dart` (UI-5) | "IME deltas: backspace becomes keys, composing waits for its commit, newline is Enter, and drift resets the sentinel" — plus the burst bound the host queue is sized on (perf §6.4): a 64-character IME deletion is 8 sequences of ≤ 16 events; a 40-character commit with Shift locked keys 16 characters and sends the rest as `text` (the Rust side of the bound is perf `viewer::bulk_input_and_moves_never_trip_congestion`). Second test: "a Return is one Enter whether the IME sends the newline action, inserts a newline, or both in either order" (§10.5.2) | IME-to-host boundary (judge 2), input congestion |
| T6 | `T/title_bar_test.dart` (UI-0a) | "caption buttons keep 16 px from the sheet edge and the setup screen's edge" — `debugDefaultTargetPlatformOverride = TargetPlatform.windows`, wide window at dpr 1.25: `caption-close` is 46×32, `closeRect.right ≤ sheetRect.right − 16`, vertically centred in the 58 px bar; on `SetupFlow`, `closeRect.right ≤ windowWidth − 16` and centred in its 44 px strip | requirement 4 acceptance |
| T7a | `T/viewer_chrome_test.dart` (UI-6) | "an unpinned toolbar collapses 2 s after the pointer leaves, never while a menu is open, and focus returns to the picture when the menu closes" — 1.9 s expanded, 2.0 s handle; with Display open, 3 s still expanded; pinned never collapses; after the menu closes a hardware key reaches the fake | §10.7, §10.8 |
| T7b | `T/viewer_chrome_test.dart` (UI-6) | "connection overlays follow the phase, stay translucent while a frame can arrive, and never replace the picture" — `ViewerStage` over `ValueNotifier`s: connecting shows `ViewerScrim` with `opaque == false`; live → reconnecting (retry pill offers Retry now) → waitingForDesktop → live keeps the same `Texture` element; identityChanged is opaque and wins | U18, §7.3, `desktop_viewer.dart:491-492` |
| T7c | `T/viewer_chrome_test.dart` (UI-6; port of `desktop_test.dart:98-224` viewer case) | "the pointer maps to display pixels and nothing is sent unless input is live" | input authorisation |
| T8 | `T/touch_gesture_test.dart` (UI-5; pure, explicit `Duration`s, no `fakeAsync`) | (a) "trackpad taps click at once and a held second tap drags without a remote double-click" — after the first `up()` the sink already has `click(left)`; a second touch within 300 ms held 300 ms produces no press before `clickAt + 520 ms`, then `press(dragStart: true), move…, release` in order; a quick second tap is a second immediate click. (b) "long press right-clicks, a two-finger tap does nothing, three fingers scroll without navigating, and a pinch never presses in direct touch" — 500 ms hold → one `click(right)`; two-finger tap → no sink call; a 72 px downward three-finger swipe → wheel events summing to −3, transform unchanged; direct touch: first finger moves 30 px, second lands at +60 ms → no `press` | D18, U3, U4, join grace |
| T9 | `T/remote_desktop_test.dart` (UI-2a) | "a new computer is remembered only after its first picture; leaving the picture releases the viewer demand and Back to picture takes it again; an identity change never touches the saved computer; a session left paused is ended" — fake session: connecting → identity → no `rememberSession`; connecting → live with picture → one `rememberSession` carrying the target's service and fingerprint; `back()` → `pause` recorded, `viewer.open()` → `resume`; `back()` then `tester.pump(kPausedSessionTimeout)` → `disconnect` recorded | D20 timing, D13, the host's single slot |
| T10 | `T/remote_computers_test.dart` (UI-2a) | "forgetting a computer deletes its thumbnail, and a changed identity is a conflict, not a duplicate" | D23, invariant "replace, never merge" |
| T11a | `T/remote_page_test.dart` (UI-4) | "an invitation is recognised before connecting and never shown in full" — `nav-remote` → `remote-add`; a setup string shows `addSetupString` and Connect stays disabled; a `pbd1.` invite shows service and fingerprint, the token is nowhere on screen, Remember is on; Connect → the fake records `connect` with a `computerId` and the viewer opens on the Connecting overlay | D3 inspect flow, invite secrecy |
| T11b | `T/remote_page_test.dart` (UI-4; from `desktop_test.dart:19-96,280-309`, `desktop_host_readiness_test.dart:70-119`) | "enabling shares view-only or with control, Stop works while busy, and helper maintenance stays reachable" | host permission, local stop |
| T11c | `T/remote_page_test.dart` (UI-4) | "the invitation QR shows only on request and hides when the page is left or the app goes inactive" — `host-qr` shows `host-qr-panel`; navigating to another page and back shows no panel; with the panel shown, `AppLifecycleState.inactive` hides it | invite secrecy (BRIEF capability table, §6.3) |
| R1 | `F/desktop_computers.rs` (UI-2b) | `vault_round_trip_hides_secrets_and_rejects_tampering` (§5.3) | D20 secrecy |

### 14.2 Changed and deleted

- `T/app_test.dart` (UI-3): `:25-37` → "`initialPage: AppPage.remote` (the
  `--desktop` hand-off) lands on Remote desktop"; `:39-52` six rail keys
  incl. `nav-remote`; `:54-63` five bar keys, no `nav-logs`, Logs through
  `settings-logs`, back through `page-back`, Settings tappable while Logs
  shows; `:90-113` the small-phone overflow loop covers all six pages.
- `T/tray_service_test.dart` (UI-3): the new `TrayStrings`/`initialize`
  arguments; one assertion that a sharing status with a viewer renders
  Disconnect viewer and Stop sharing with stable ids, uses the `tray_active`
  icon (OD-1 (a)), and Stop calls `onStopSharing`.
- `T/support.dart`: fonts (UI-0b); `pumpApp(initialPage:, viewerPrefs:, vault:)`
  and no `openDesktopOnStartup` (UI-3).
- `T/desktop_test.dart`: host widget tests move to T11b (UI-4), viewer widget
  tests to T7c (UI-6); the pipeline parts are WP-D's.
- `T/desktop_host_readiness_test.dart`: the widget part moves to T11b (UI-4);
  the store tests stay (WP-D).
- New fake: `T/fake_remote_computer_vault.dart` (UI-2a; in-memory, can be
  set unavailable or full).
- Deleted (UI-6): `T/desktop_gesture_test.dart` (replaced by T2).
- Deleted earlier, by WP-D in its D1 milestone (perf §9.8):
  `T/desktop_viewer_navigation_test.dart` (one viewer is structural now; it
  drives the old pages through the old store surface and would not compile
  against D1).

---

## 15. Previews and posters

`T/preview_test.dart` (opt-in, `PB_MAPPER_UI_PREVIEW_DIR`) gains, in UI-8:
`remote_setup_light`, `remote_sharing_light`, `remote_sharing_dark`,
`remote_add_dialog_light`, `viewer_connecting`, `viewer_shortcuts_dark`
(1440×900, Shortcuts open), `viewer_reconnect`, `viewer_identity`,
`viewer_collapsed_view_only`, `sharing_fallback_dialog`,
`title_bar_windows_125` (Windows override, dpr 1.25, close hovered),
`phone_remote_tab`, `phone_add_paste`, `phone_viewer_trackpad` (844×390),
`phone_viewer_keyboard` (simulated `viewInsets`, Ctrl locked),
`phone_gesture_sheet`.

The picture is `RemoteScreenFixture` (`T/remote_screen_fixture.dart`): a
`PictureRecorder` painting of the RemoteScreen artboard (desktop icons, a
document window, taskbar) encoded to PNG and delivered through the fake JPEG
path (`FakeDesktopApi.jpegFrames`, R-UI-9; `instantiateImageCodec` decodes
PNG), so previews run on Linux without a native texture. The scanner
viewfinder uses `InviteScanner.debugFakeCamera`.

Posters (`T/gen_poster_test.dart`): regenerated in UI-0b (icons thicken)
and again in UI-8 (the rail gains Remote desktop, Tunnels loses
`open-desktop`): `docs/assets/poster.png`, `docs/assets/poster.zh-CN.png`.

---

## 16. Docs (UI-8 unless noted)

- `docs/ui-design.md`: caption geometry (UI-0a), icon weight and size rules
  (UI-0b); then six places, Logs inside Settings on phones, the Remote page,
  the in-shell viewer and its chrome, D15 capsule + fallback + tray, the
  dark viewer palette.
- `docs/remote-desktop.md` (UI sections; WP-M owns the performance model):
  gestures with the change from PR #51, modifier latches, hardware key
  routing and the keys that are not sent (§10.5.1 rule 5), viewer states,
  sharing controls and what a hidden window shows (OD-1; no indicator while
  a background host shares with the app closed), remembered computers and
  the vault, the drag-start guard, the paused-session timeout.
- `CLAUDE.md`: the UI module (Remote desktop page; `pages/remote/` and
  `pages/remote/viewer/`; `state/remote_computers.dart`,
  `state/remote_desktop.dart`, `state/viewer_prefs.dart`; the `computers_*`
  ops and the `pb_mapper/keystore` channel) and "What the UI does".
- `CHANGELOG.md`: right click is a long press; drag is double-tap-and-hold;
  a two-finger tap no longer right-clicks; three-finger scroll; Logs moved
  into Settings on phones; remembered computers; Ctrl+Alt+Del still
  unsupported; Android cloud backup and device transfer no longer restore
  tunnels, the relay credential or remembered computers (§5.3); "SDK:
  breaking" for `pb_mapper::desktop` (text from WP-V); the iOS and macOS
  builds with `mobile_scanner` are unverified (no Mac, no checked-in
  Podfile).
- `ui/README.md`: page list if it has one.

---

## 17. Work packages and sequence

### 17.1 Packages and owned files (one owner per file per phase; separate worktrees)

| WP | Scope | Owns (create / modify / delete) | Depends on | Checks |
| --- | --- | --- | --- | --- |
| **UI-0a Title bar** (cherry-pickable to master) | §12 (`CaptionTone.surface` only; `viewer` arrives in UI-1) | `U/shell/window_chrome.dart`; `U/shell/page_frame.dart` (padding, gap, `CaptionButtons()` only); `U/pages/setup_flow.dart` (caption placement only); `T/title_bar_test.dart` (new); `docs/ui-design.md` (caption paragraph) | nothing | `flutter analyze`; T6; `app_test`; by hand at 100/125/150 %, maximised and restored |
| **UI-0b Icons** (cherry-pickable to master) | §11 | `U/theme/app_icons.dart`; `U/theme/app_theme.dart` (`iconSize: 18`); `U/shell/navigation.dart` (rail icon size only); `U/galaxy/galaxy_panel.dart`; the §11.2 audit files; `U/widgets/surfaces.dart` (`EmptyNote`); `T/support.dart` (fonts); `T/gen_poster_test.dart`; `ui/pubspec.yaml` (− `cupertino_icons`); `docs/assets/poster*.png`; `docs/ui-design.md` (icon rules) | UI-0a (docs order only) | `flutter analyze`; full `flutter test` once (global visual change); posters regenerated |
| **UI-1 Foundation** | strings, shared primitives, deps, platform declarations | `ui/lib/l10n/app_en.arb`, `app_zh.arb` + generated `app_localizations*.dart` (all §13.2 keys); `U/theme/viewer_palette.dart` (new); `U/shell/window_chrome.dart` (`CaptionTone.viewer` only, §12.2); `U/widgets/dashed_border.dart` (new: `DashedBorder extends OutlinedBorder`, usable as `Material(shape:)`); `U/widgets/status.dart` (`StatusChip.inkLabel`); `U/theme/glass.dart` (`GlassSegment.enabled`); `U/theme/app_theme.dart` (`quietText(ColorScheme)`); `U/state/viewer_prefs.dart` (new); `ui/pubspec.yaml` + lock (+ `qr: 3.0.3`, + `mobile_scanner: 7.5.0`); generated plugin registrants; `ui/android/app/src/main/AndroidManifest.xml` (backup attributes) + `res/xml/data_extraction_rules.xml`, `res/xml/backup_rules.xml` (new); `ui/ios/Runner/Info.plist` (`NSCameraUsageDescription`) + `en.lproj`/`zh-Hans.lproj` `InfoPlist.strings` | UI-0b | `flutter analyze`; `flutter gen-l10n` (no new key untranslated); `flutter build apk --debug` (bump the Android Kotlin plugin only if the build requires it, D21) |
| **UI-2a State** (track A) | §5.1-§5.2, §5.4-§5.5, vault interface | `U/state/remote_computers.dart`, `U/state/remote_desktop.dart`, `U/ffi/remote_computer_vault.dart` (interface, `UnavailableRemoteComputerVault`, `defaultRemoteComputerVault`) (new); `U/state/remote_service.dart`; `U/state/app_store.dart` (after WP-D's line); `T/fake_remote_computer_vault.dart`, `T/remote_computers_test.dart`, `T/remote_desktop_test.dart` (new) | UI-1; WP-D D1 with R-UI-1, 2, 4, 5 (Dart), 7, 8, 9 | `flutter analyze`; T9, T10; `store_test` |
| **UI-2b Vault** (parallel) | §5.3 FFI/Kotlin | `F/desktop_computers.rs` (new); `F/desktop_trust.rs` (`protect(bytes, encrypt, entropy, description)` and `replace` → `pub(super)`); `K/ComputerKeystore.kt` (new); `K/MainActivity.kt` (one line); `U/ffi/remote_computer_vault.dart` (`FfiRemoteComputerVault`, default switch) | perf G2 with R-UI-5 (FFI) and R-UI-6 (WP-F hooks); UI-2a | `cargo test -p pb-mapper-ffi` (R1); `cargo clippy --workspace --all-targets -- -D warnings`; `cargo ndk -t arm64-v8a check -p pb-mapper-ffi`. The on-device remember → restart → listed → forget check needs the Dart callers of UI-4, so it runs at G3 (WORKPLAN §4.2 Stage 3 item 5) |
| **UI-3 Shell and D15** (track A) | §4 | `U/shell/app_scope.dart`; `U/shell/navigation.dart`; `U/shell/page_frame.dart` (`onBack`, capsule, `LayoutBuilder`); `U/shell/sharing_indicator.dart` (new); `U/app.dart`; `ui/lib/main.dart`; `U/state/app_store.dart` (delete `activeDesktop` only); `U/common/tray/tray_service.dart`; `U/pages/logs_page.dart`; `U/pages/settings_page.dart`; `U/pages/tunnels_page.dart` (`open-desktop` removed); `U/pages/relay/services_tab.dart` (no Connect for `isDesktop`, §4.8); `R/remote_page.dart` (new, transitional: `PageFrame(title, sharingIndicator: false, body: DesktopPage(store: scope.store.desktop))`, the old page reused unchanged); `V/viewer_screen.dart` (new: the chrome of §4.5 around the old `pages/desktop_viewer.dart` `DesktopViewer(store: remote.desktop)`, transitional); `T/app_test.dart`, `T/tray_service_test.dart`, `T/sharing_test.dart` (new), `T/support.dart` (`pumpApp`) | UI-2a | `flutter analyze`; T1c, T3, `app_test`, `tray_service_test` |
| **UI-4 Remote page** (track A) | §6 | `R/remote_page.dart` (rewrite), `R/host_card.dart`, `R/invite_qr.dart`, `R/computer_list.dart`, `R/computer_list_compact.dart`, `R/add_computer.dart`, `R/invite_scanner.dart`, `R/remote_format.dart`, `R/remote_actions.dart` (new); delete `U/pages/desktop_page.dart`; `T/remote_page_test.dart` (new); `T/desktop_host_readiness_test.dart`, `T/desktop_test.dart` (host widget parts out) | UI-3 | `flutter analyze`; T11a, T11b, T11c, `desktop_host_readiness_test` |
| **UI-5 Viewer input core** (track B) | §10.1-§10.6 | `V/touch_gestures.dart`, `V/remote_viewport.dart`, `V/remote_surface.dart`, `V/remote_input.dart`, `V/modifier_latch.dart`, `V/remote_keys.dart`, `V/remote_text_input.dart` (new); `T/touch_gesture_test.dart`, `T/remote_surface_test.dart`, `T/remote_input_test.dart`, `T/remote_text_input_test.dart` (new) | UI-1; WP-D D1 with R-UI-3 (Dart `dragStart`) | `flutter analyze`; T2, T4, T5, T8 |
| **UI-6 Desktop viewer** (track B) | §7, §8, §10.7-§10.9 | `V/viewer_screen.dart` (rewrite), `V/viewer_stage.dart`, `V/viewer_overlays.dart`, `V/remote_cursor_layer.dart`, `V/jpeg_picture.dart`, `V/viewer_menus.dart`, `V/quality_panel.dart`, `V/desktop_viewer.dart`, `V/viewer_title_bar.dart`, `V/viewer_toolbar.dart` (new); delete `U/pages/desktop_viewer.dart`, `U/pages/desktop_gesture_surface.dart`, `U/pages/desktop_connection_banner.dart`, `T/desktop_gesture_test.dart` (`T/desktop_viewer_navigation_test.dart` is already gone, WP-D D1); `T/viewer_chrome_test.dart` (new); `T/desktop_test.dart` (viewer widget part out); `ui/tool/desktop_viewer_acceptance.dart` (constructor lines, §17.2) | UI-5; UI-3; perf G2 | `flutter analyze`; T7; `flutter build windows`; the acceptance tool builds (`flutter build apk --target tool/desktop_viewer_acceptance.dart`) |
| **UI-7 Mobile viewer** (track B) | §9 | `V/touch_viewer.dart`, `V/touch_controls.dart`, `V/side_panel.dart`, `V/modifier_bar.dart`, `V/minimap.dart`, `V/virtual_mouse.dart`, `V/gesture_sheet.dart` (new); `V/viewer_screen.dart` (the `TouchViewer` line) | UI-6 | `flutter analyze`; existing tests; `flutter build apk --debug`; on-device (§18) |
| **UI-8 Finish** | §13 cleanup, §15, §16 | ARB deletions (all `desktop*`); `T/preview_test.dart`, `T/remote_screen_fixture.dart` (new); posters; `docs/ui-design.md`, `docs/remote-desktop.md` (UI sections), `CLAUDE.md`, `CHANGELOG.md`, `ui/README.md`; `.github/workflows/syntax-check.yml` (`! grep -rnE '\bIcons\.' ui/lib`) | all above; WP-M for shared docs | full `flutter test` once; `flutter gen-l10n` untranslated empty; `grep -c '"desktop' ui/lib/l10n/*.arb` = 0 |

Every package: `flutter analyze --no-fatal-infos` clean, its named tests
green, its own commit(s) with tests (D27). UI-0a and UI-0b touch only files
that exist on master; PR #51 left `shell/`, `theme/`, `setup_flow.dart` and
`galaxy/` untouched (`git diff --stat 0d883d2..HEAD`), and its
`T/support.dart` change (`:34,57`) is a different hunk from the font loader
(`:111-124`), so both cherry-pick cleanly.

### 17.2 Declared cross-owner edits (sequential, never concurrent)

| File | Primary owner | UI edit | When |
| --- | --- | --- | --- |
| `U/state/app_store.dart` | WP-D (D14 line only) | UI-2a: filter, `desktopServiceNames`, `computers`, `vault` parameter; UI-3: delete `activeDesktop` with the banner | after WP-D D1, in package order |
| `ui/tool/desktop_viewer_acceptance.dart` | WP-D (schema 2 rewrite) | UI-6: import `V/desktop_viewer.dart`; `home: DesktopViewer(desktop: store, prefs: ViewerPrefs(persist: false), title: 'acceptance')`; hold `store.demand(DesktopDemand.viewer)` for the run and dispose it at the end | after WP-D's rewrite |
| `T/desktop_test.dart`, `T/desktop_host_readiness_test.dart` | WP-D (pipeline/store parts) | UI-4 removes host widget tests; UI-6 removes viewer widget tests; whoever empties a file deletes it | sequential |
| `U/pages/desktop_page.dart`, `desktop_viewer.dart`, `desktop_connection_banner.dart`, `desktop_gesture_surface.dart` | WP-D (port to the D1 surface, perf §9.8; G2's smoke runs on them) | UI-4 / UI-6 delete them | after G2 |
| `K/MainActivity.kt` | WP-K | UI-2b one registration line | after G2 |
| `F/lib.rs`, `F/desktop.rs` | WP-F | none: WP-F adds the R-UI-5/R-UI-6 hooks | G1/G2 |
| `docs/remote-desktop.md`, `CLAUDE.md` | WP-M (performance sections) | UI-8 (UI sections) | after WP-M |
| `T/support.dart`, `ui/pubspec.yaml`, `U/shell/page_frame.dart`, `U/shell/navigation.dart`, `U/theme/app_theme.dart`, `U/pages/tunnels_page.dart` | — | UI-0a/0b first, then UI-1/UI-3 | in package order |
| `V/viewer_screen.dart` | UI-3 (creates) | UI-6 rewrites, UI-7 one line | in package order |
| ARB files | UI-1 (adds everything) | UI-8 (deletes old keys) | no other package edits them |

### 17.3 Sequence and gates

```
now ─ UI-0a ─ UI-0b ─ UI-1 ─┬──────── (perf WP-D D1 incl. R-UI-1, 2, 3-Dart, 4, 5-Dart, 7, 8, 9, 10) ────────┐
                            │                                                                             │
                            ├─ track A: UI-2a ─ UI-3 ─ UI-4 ──────────────────────────────────────────────┤
                            ├─ track B: UI-5 ──(needs UI-3)── UI-6 ─ UI-7 ───────────────────────────────┤
                            └─ UI-2b (after perf G2 incl. R-UI-5-FFI and R-UI-6) ─────────────────────────┴─ UI-8
```

- UI-0a and UI-0b can start immediately and land on master first (D27).
- UI-1 lands on the branch before either track; it touches no file of the
  perf packages.
- Tracks A and B develop in separate worktrees against WP-D's D1 milestone
  and `FakeDesktopApi`; they merge on top of perf G2 (perf §12.3). Merge
  order: UI-2a, UI-3, UI-5, UI-4, UI-6, UI-2b, UI-7, UI-8.
- R-UI-3 (host guard) is a perf deliverable (WP-0, WP-W, WP-HL); the viewer
  side works without it but the drag guard is only complete with it — the
  G2 checklist includes `worker::drag_start_press_waits_out_the_double_click_time`.
- Every gate leaves the app building and usable: UI-3's transitional Remote
  page and viewer screen reuse the old widgets until UI-4 and UI-6 replace
  them.

---

## 18. Risks and on-device checks (before the merge gate, D28)

| # | Risk or check | Mitigation |
| --- | --- | --- |
| K1 | A `DeltaTextInputClient` with no focused `EditableText` misbehaves on some IMEs (Samsung, Gboard) | on-device check in UI-7; fallback documented in `remote_text_input.dart`: an offstage `EditableText` that never takes primary focus while a hardware key routes through the picture |
| K2 | IME insets in immersive sticky mode on Android | on-device check; the occlusion rule (§10.6) tolerates late or animated insets |
| K3 | `BackdropFilter` grey still over the external texture (Android GL with Impeller off; Windows) | on-device check; if unsupported, the still becomes a translucent 0.5 scrim (no opacity change elsewhere) |
| K4 | `mobile_scanner` 7.5.0 needs a newer Kotlin Gradle plugin | bump only if the build requires it (D21); UI-1 builds the APK |
| K5 | Keystore key lost after a device restore | backup exclusions; the reset path (§5.3) starts an empty vault, only on `KEY_GONE` and only after a new key has been wrapped; any other Keystore failure leaves the vault for the next load |
| K6 | AltGr detection on non-US layouts (German, French, Polish) | T4b covers the rule; on-device check with a German layout on Windows |
| K7 | Caption glyph crispness at 125 % and 150 % | by hand in UI-0a |
| K8 | Feel of the 520 ms drag guard, 24 px scroll notch, 80 ms join grace | tune the constants (§10.1) on device; one place each |
| K9 | A plugin pushing routes on a nested navigator would hide a barrier from `SharingChrome` | none today; the tray still offers Stop |
| K10 | Windows minimise or hide-to-tray not reported as `hidden` (perf R8) | perf fallback; the viewer's local release still runs on `inactive` |
| K11 | D15 while the host window is hidden or minimised: in-window surfaces are invisible (§4.6) | OD-1 default: tray `tray_active` state and tooltip, `show(inactive: true)` on a viewer's arrival; always-on-top is the owner's option; background hosting with the app closed has no indicator (documented). Checked in WORKPLAN §4.2 Stage 3 item 4 |
| K12 | Android freezes the backgrounded app (perf R13), so a resume can need a reconnect | the reconnect overlay keeps the greyed last picture; the first attempt has no backoff (perf §9.6.2) |

---

## Appendix A. Widget keys tests rely on

Old → new: `open-desktop` → deleted (`nav-remote`); `desktop-share` →
`host-enable`; `desktop-stop` → `host-stop`; `desktop-invitation` →
`add-invite`; `desktop-connect` → `add-connect`;
`desktop-helper-maintenance` → `host-more`; `desktop-helper-disable` /
`-uninstall` → `host-helper-disable` / `-uninstall`; `desktop-touch-mode`
→ `touch-mode-toggle`; `desktop-connection-state` →
`viewer-overlay-<kind>`; `desktop-scene-transform` →
`remote-scene-transform`.

New: `nav-remote`, `settings-logs`, `page-back`, `caption-minimize`,
`caption-maximize`, `caption-close`, `sharing-indicator`,
`sharing-indicator-disconnect`, `sharing-indicator-stop`,
`sharing-fallback`, `sharing-fallback-disconnect`, `sharing-fallback-stop`,
`remote-add`, `remote-add-empty`, `remote-add-tile`, `remote-refresh`,
`remote-scan`, `remote-paste`, `computer-<id>`, `computer-open-<id>`,
`computer-more-<id>`, `computer-featured`, `computer-featured-open`,
`host-permission`, `host-copy`, `host-qr`, `host-qr-panel`, `host-display`,
`host-allow-control`, `host-enable`, `host-enable-ordinary`, `host-start`,
`host-stop`, `host-make-view-only`, `host-disconnect-viewer`,
`host-helper-update`, `host-more`, `host-rename`, `host-helper-install`,
`host-helper-disable`, `host-helper-uninstall`, `host-revoke`,
`add-invite`, `add-paste`, `add-invite-clear`, `add-recognised`,
`add-replace`, `add-name`, `add-remember`, `add-more`, `add-relay-only`,
`add-connect`, `viewer-back`, `viewer-pin`, `viewer-quality-chip`,
`viewer-mode`, `viewer-display`, `viewer-shortcuts`, `viewer-more`,
`viewer-fullscreen`, `viewer-disconnect`, `viewer-toolbar-handle`,
`viewer-quality-panel`, `viewer-view-only`, `viewer-control-hint`,
`viewer-overlay-<kind>`, `viewer-type-text`, `viewer-more-quality`,
`viewer-more-back`, `display-fit`, `display-original`,
`display-quality-panel`, `quality-quality`, `quality-balanced`,
`quality-smooth`, `shortcut-show-desktop`, `shortcut-switch-window`,
`shortcut-task-manager`, `shortcut-start`, `shortcut-lock`, `shortcut-cad`,
`shortcut-refresh`, `shortcut-release`, `more-gestures`, `more-mode`,
`more-virtual-mouse`, `more-disconnect`, `touch-back`, `touch-quality`,
`touch-mode-toggle`, `touch-mode-hint`, `touch-keyboard`,
`touch-shortcuts`, `touch-right-click`, `touch-more`, `modifier-bar`,
`modifier-control`, `modifier-alt`, `modifier-shift`, `modifier-meta`,
`modifier-fn`, `modifier-collapse`, `modifier-pill`, `minimap`,
`virtual-mouse`, `gesture-mode`, `gesture-got-it`, `remote-scene-transform`.
