# Remote desktop UI — interaction-first architecture (requirements 2–4)

Scope: BRIEF requirements 2 (redesign), 3 (icons), 4 (title bar) under
DECISIONS D14–D26. Angle: get the viewers' interaction exactly right —
gesture state machine, modifier latches, keyboards, toolbar behaviour, focus,
timings — and build the pages and shell around that.

Paths are relative to `ui/` unless they start with `crates/` or `docs/`.
Line numbers are at `a41eaa7`. Artboards are cited as `Main:52`; the
design-spec map is cited as `spec §2.3`; the RustDesk map as `rd §3.9`.

---

## 0. Decisions this design takes, and where it pushes back

| # | Decision | Why / evidence |
| --- | --- | --- |
| I1 | **The viewer is hosted in the shell, not on a route.** `_PbMapperAppState._home` returns `RemoteViewerScreen` instead of `AppShell` while `ViewerHost.showing` is true. No `MaterialPageRoute`, no `AppBar`. | One viewer by construction (today: `_viewerOpen` + `_viewerOpenSerial` + awaiting `route.completed`, `desktop_page.dart:289-302`). `AppScope` stays reachable (routes cannot see it, `app_scope.dart` §2.4 of the ui-pages map). The rail disappears because `AppShell` is not built. |
| I2 | **Visibility and focus are separate signals.** On-screen/off-screen drives `attachViewer`/`detachViewer` (D13 pause). Window focus loss only releases local input and latches; it neither pauses nor ends control. | Today any non-`resumed` state ends control (`desktop_viewer.dart:46-54` → `setViewerForeground(false)`), so alt-tab on desktop costs a control round trip (≥ 1 s re-request, `desktop_store.dart:394-410`). Keys cannot leak: everything held is released on blur (§3.9). |
| I3 | **Touch gestures are a raw-pointer state machine** (`TouchGestureMachine`), not arena recognisers; the `ScaleGestureRecognizer` in the surface is removed. | D18's "send the tap immediately" cannot be done with RustDesk's recognisers: their tap fires only after the double-tap/hold recognisers give up, ~300 ms after release (rd §3.9 table, `onTap`). Class escalation 1→2→3 fingers and "never de-escalate within a sequence" are one rule in a state machine. |
| I4 | **Pushback inside D18: remote double-click guard.** When a double-tap-and-hold becomes a drag, its left-button *down* is sent no earlier than 520 ms after the first tap's click-down; moves meanwhile are buffered locally. Taps themselves stay immediate. | In trackpad mode both presses land on the same cursor pixel. Windows converts a second `WM_LBUTTONDOWN` within `GetDoubleClickTime()` (500 ms default) and `SM_CXDOUBLECLK`×`SM_CYDOUBLECLK` (4×4 px) into `WM_LBUTTONDBLCLK`, so "click, then press-and-drag" is a *double-click-drag*: Explorer opens the icon the user meant to drag, editors switch to word-wise selection. The guard costs at most ~260 ms of drag-start latency, only when the drag starts fast. D18's literal wording (convert the already-sent click into a drag) is preserved: no tap is delayed. |
| I5 | **Deviation from Viewer artboard: a way back without disconnecting.** Desktop viewer title bar gets a leading back button (and More → "回到电脑列表"). | The artboard's toolbar offers only Disconnect (`Viewer:72-75`) while Main draws a "正在查看 · 回到画面" row (`Main:118-129`), which presupposes leaving the viewer with the session alive. Esc cannot be used (it belongs to the remote). |
| I6 | **D15 "always-on-top" = every title bar of this app + the tray.** A `SharingIndicator` capsule lives inside `PageFrame`'s title bar and the viewer's title bar; tray tooltip/menu carry the same state and actions. It never shifts content and never steals focus. | An OS-topmost window needs a second native window (e.g. `desktop_multi_window`); D21 allows only `qr` and `mobile_scanner`. Every screen of the app has exactly one of these two title bars, so the capsule is on top of everything the app shows. Hidden on the Remote page, whose host card carries the same controls. |
| I7 | Collapsed toolbar handle expands on **click, or hover held 300 ms**. | spec §2.6 says "hover or click"; a dwell avoids popping the toolbar over remote content whenever the pointer sweeps along the top edge (RustDesk has no hover expand at all, rd §3.1 step 6). |
| I8 | AddComputer's recognised-host line shows the **short fingerprint**, not "权限：查看并控制". | The invite carries no permission (spec §2.5 GAP; D3 payload = service + certificate fingerprint + token). |
| I9 | Three-finger scroll: **one wheel notch per 24 px** of mean finger travel (RustDesk: 4 px, rd §3.9). | A notch scrolls ~3 lines ≈ 60 remote px; a 1080p picture fitted on a phone is drawn at ≈ 0.36–0.4 scale, so 60 remote px ≈ 22–24 phone px: content tracks the fingers. 4 px/notch scrolls ~15× faster than the finger. One constant, tune on device. |
| I10 | **Precision-touchpad two-finger pan over the picture scrolls the remote** (pinch still zooms locally). | Today trackpad `PointerPanZoom` goes into `_scaleUpdate` navigation (`desktop_gesture_surface.dart:299-320, 358-368`); at fit scale the offset is clamped, so a laptop user's two-finger scroll does nothing. |
| I11 | **Hardware letters/digits with Ctrl, Alt or Win held go as `key`**, using the D17 vocabulary; printable text without those modifiers stays `text`; AltGr characters stay `text`. | Today such combos are dropped (`desktop_viewer.dart:244-252`), so Ctrl+B, Ctrl+S, Alt+F do nothing. |
| I12 | **The soft keyboard never re-fits the picture.** Layout size ignores the IME (`resizeToAvoidBottomInset: false`); a separate visible rect bounds panning; the view pans minimally to keep the cursor visible and is restored exactly on hide. | P1-4 "keep scale and offset across soft-keyboard show/hide"; today `_layout` re-fits on any viewport change (`desktop_gesture_surface.dart:275-293`). |
| I13 | Viewer preferences live in a separate `ViewerPrefs` notifier, not `AppPrefs`. | Every `AppPrefs` notification rebuilds the whole `MaterialApp` (`app.dart:103,181-187`). Pinning the toolbar must not. |
| I14 | All 93 `desktop*` ARB keys disappear: the 42 dead ones are deleted, the used ones are renamed into `host*`/`remote*`/`viewer*`/`touch*`/`shortcut*` or deleted. | Every call site is rewritten anyway; the old keys sit outside the file's grouping (`app_en.arb:2-60, 640-678`). |

Everything else follows DECISIONS as written. Undrawn screens follow the
design-spec proposals (D19).

---

## 1. App shell

### 1.1 `AppPage` (`lib/src/shell/app_scope.dart`)

```dart
enum AppPage {
  overview, tunnels, remote, relay, logs, settings;

  /// The rail, top to bottom (AppRail:23-46).
  static const railEntries = [overview, tunnels, remote, relay, logs, settings];
  /// The bottom bar (MobileTabBar:17-31): Logs lives in Settings there.
  static const barEntries = [overview, tunnels, remote, relay, settings];
  /// The bar entry that stands for this page on a compact window.
  AppPage get barEntry => this == logs ? settings : this;

  IconData get icon => switch (this) { …, remote => AppIcons.remote, … };
  /// [short]: the bar's label ("远程"); the rail and page title use the long one.
  String label(BuildContext context, {bool short = false}) =>
      switch (this) { …, remote => short ? l10n.navRemote : l10n.navRemoteDesktop, … };
  bool get grouped => this == tunnels || this == remote || this == relay || this == settings;
  static AppPage? parse(String? name);   // unchanged; 'remote' is new, old names stay valid
}
```

`AppScope` gains one field, `final RemoteDesktop remote;` (§4.4), included in
`updateShouldNotify` by identity. Doc comments that say "four/five pages"
(`app.dart:37-38`, `navigation.dart:18,169,223`, `app_scope.dart:10`) are
corrected.

### 1.2 Rail and bar (`lib/src/shell/navigation.dart`)

- `NavRail`: iterate `AppPage.railEntries`; selection index
  `AppPage.railEntries.indexOf(page)`. `kRailWidth` stays 212, rows 38, gap 2
  (spec §2.1 keeps shell metrics). `_RailRow` icon size 18 → **20**
  (`AppIconSize.nav`, requirement 3).
- `_RailRow` gains `Widget? trailing`. For `AppPage.remote` the trailing is
  `HostSharingDot`: `ValueListenableBuilder<DesktopSharing>(desktop.sharing)`;
  nothing when not hosting; `StatusDot(color: signalColor(scheme), size: 7)`
  while hosting with no viewer; `PulsingDot(color: signalColor(scheme), size: 7)`
  while a viewer is connected (static under reduced motion — `PulsingDot`
  already checks `Motion.reduced`). Semantics value: `hostSharing` /
  `sharingBeingViewed` / `sharingBeingControlled`.
- `_RelayFoot`: append `" · {n} ms"` when online and
  `AppStore.relay?.latencyMs` is known (spec §2.1). Fix its doc comment (it
  opens Relay, not Settings).
- `NavBar`: iterate `AppPage.barEntries`; `selected: entry == page.barEntry`;
  `onTap: entry == page ? null : () => onSelect(entry)` (so Settings is
  tappable while Logs is showing). Top hairline
  `Border(top: BorderSide(color: scheme.outlineVariant))` (spec §2.2). The
  Remote item gets the same `HostSharingDot`, `Positioned(top: 3, right: 8)`
  over the 54×30 pill (only a narrow Windows window can host; phones never show
  it).

### 1.3 Logs on compact windows

- `PageFrame` gains `VoidCallback? onBack`. When set, the bar starts with
  `IconButton(key: Key('page-back'), icon: Icon(AppIcons.back), tooltip: l10n.back)`
  and the title lead shrinks from 16 to 4.
- `LogsPage.build`: `final compact = !ShellLayout.wideOf(context);` wraps its
  `PageFrame` in
  `PopScope(canPop: !compact, onPopInvokedWithResult: (didPop, _) { if (!didPop) scope.go(AppPage.settings); })`
  and passes `onBack: compact ? () => scope.go(AppPage.settings) : null`.
- `SettingsPage`: on compact only, a `GroupCard` with one navigation row
  `Key('settings-logs')`: `AppIcons.logs` 18 · `navLogs` · `settingsLogsHint` ·
  `AppIcons.chevronRight` 16 → `scope.go(AppPage.logs)`. Placed after the
  Appearance card.
- A persisted `last_page = 'logs'` on a compact window simply opens Logs with
  its back button; nothing is remapped. Resizing compact → wide keeps Logs
  selected in the rail and the back button disappears.

### 1.4 The Remote page in the shell

`app.dart:_home` switch gains `AppPage.remote => const RemotePage()`. The page
is an ordinary `PageFrame` page on the grouped sheet (§2.1). The `--desktop`
launch flag (`main.dart:39`, emitted by the Rust helper restart hand-off) keeps
its name; `PbMapperApp.openDesktopOnStartup` is renamed `openRemoteOnStartup`
and `_boot` sets `_page = AppPage.remote` instead of pushing a route
(`app.dart:124-135` deleted). The Tunnels title-bar `open-desktop` button
(`tunnels_page.dart:33-38`) and `openDesktop()` (`desktop_page.dart:9-14`) are
deleted.

### 1.5 Hosting the full-window viewer

```dart
// app.dart
Widget _home(BuildContext context) {
  …setup checks unchanged…
  return AppScope(
    …, remote: _remote,
    child: _remote.viewer.showing
        ? const RemoteViewerScreen()
        : AppShell(page: _page, onSelect: _go, child: switch (_page) { … }),
  );
}
```

- `_PbMapperAppState` owns `ViewerPrefs _viewerPrefs`, `ViewerHost _viewer`
  and `RemoteDesktop _remote` (§4.4), created after `_store`;
  `_viewer.addListener(() => setState(() {}))`; disposed in `dispose`.
- No animated switcher between shell and viewer (two widgets with one key would
  collide during a fast in-out). The viewer fades in with
  `EntranceMotion(offset: 0)`; the shell's page already animates on mount
  (`app_shell.dart:45-49`). The Remote page's `ListView` carries
  `PageStorageKey('remote-page')`, so its scroll position survives the shell
  being rebuilt (the bucket belongs to the home route, which persists).
- `RemoteViewerScreen` (`pages/remote/viewer/viewer_screen.dart`):
  - reads `AppScope.of(context).remote`;
  - picks `TouchViewer` when `defaultTargetPlatform` is android or iOS,
    `DesktopViewer` otherwise (a Windows touchscreen uses the desktop viewer
    with direct touch, §3.2);
  - wraps in `Theme(data: viewerTheme(context))` (dark, §2.3.8) and
    `MediaQuery.withClampedTextScaling(maxScaleFactor: 1.3)` so fixed-height
    dark chrome cannot overflow;
  - `PopScope(canPop: false, onPopInvokedWithResult: (didPop, _) { if (!didPop) remote.back(); })`
    so Android back returns to the list with the session alive.
- Window chrome while viewing (desktop): `ViewerTitleBar` (§2.3.2) provides the
  drag area, double-click maximise and `CaptionButtons(tone: CaptionTone.viewer)`.
  `windowManager.setTitle('$name — pb-mapper')` on show, `'pb-mapper'` on hide
  (best effort, desktop only); leaving the viewer exits full screen.
- `ViewerHost` (§4.4) is the only object that tells the store whether a viewer
  is on screen (`attachViewer`/`detachViewer`). Widget init/dispose order no
  longer matters.

### 1.6 D15 sharing indicator (`lib/src/shell/sharing_indicator.dart`, new)

```dart
class SharingIndicator extends StatelessWidget {
  const SharingIndicator({super.key, this.tone = CaptionTone.surface});
  final CaptionTone tone;
}
```

- Source: `ValueListenableBuilder<DesktopSharing>(AppScope.of(context).store.desktop.sharing)`.
  Visible only while `sharing.viewer != HostViewer.none` (D15: only while a
  remote viewer is connected). Otherwise `SizedBox.shrink()`.
- Capsule: height 30, radius 15, padding `EdgeInsetsDirectional.fromSTEB(12, 0, 3, 0)`;
  fill `scheme.tertiaryContainer` (surface tone) or `Color(0x297DB6F2)`
  (viewer tone); `PulsingDot(color: signalColor(scheme), size: 7)` · 8 ·
  label 12.5/600 `onTertiaryContainer` (`sharingBeingViewed` /
  `sharingBeingControlled`) · 8 · two 26-tall text buttons (radius 13,
  12.5/500): `Key('sharing-indicator-disconnect')` → `desktop.disconnectViewer()`
  (`hostDisconnectViewer`), `Key('sharing-indicator-stop')` →
  `desktop.stopHost()` (`hostStop`). Both bypass `busy` (store invariant 10).
- Narrow (window width < 640): label hidden; buttons become 26×26 icon buttons
  (`AppIcons.disconnect`, `AppIcons.screenShareOff`, 16) with the same tooltips.
- Placement: in `_TitleBar`'s row right after the expanded title
  (`const SizedBox(width: 12), const SharingIndicator()`), and in
  `ViewerTitleBar` before the caption buttons. `PageFrame` gains
  `bool sharingIndicator = true`; `RemotePage` passes `false`.
- `Semantics(container: true, liveRegion: true, label: …)`. Key
  `sharing-indicator`.
- Disconnecting a viewer must not be undone by its automatic reconnect: the
  viewer side treats a host-initiated close (`reason: disconnected_by_host`) as
  terminal (Ended overlay, no retry) — store contract in §4.2.

### 1.7 Tray (`lib/src/common/tray/tray_service.dart`, `app.dart`)

- `TrayStatus` gains `final DesktopSharing host;` (default not hosting).
- `TrayStrings` gains `hostSharing`, `hostViewed`, `hostControlled`,
  `disconnectViewer`, `stopSharing`.
- `initialize(…)` gains `VoidCallback onDisconnectViewer, VoidCallback onStopSharing`.
- Retained `MenuItem`s: `_hostItem` (disabled status line), `_disconnectViewerItem`,
  `_stopSharingItem`. Menu: `[status, if hosting: [hostItem, if viewer: disconnectViewer, stopSharing], separator, open, refresh, separator, quit]`,
  built from the retained instances so Windows' late `WM_COMMAND` still
  resolves (comment at `tray_service.dart:90-91`). `_renderedMenu` key gains the
  host line and the two flags.
- Tooltip: `'$status · $hostLine'` while hosting. Tray icons unchanged (out of
  scope).
- `app.dart`: `_store.desktop.sharing.addListener(_refreshTray)` in addition to
  `_store.addListener(_refreshTray)`; `_trayStatus()` fills `host`.

### 1.8 What leaves `MaterialApp.builder`

The `DesktopSharingBanner` column (`app.dart:321-336`) is deleted; `builder`
returns `ToastificationWrapper(child: child)`. Local stop stays reachable from
every screen while viewed (indicator), from the Remote page (host card) and
from the tray at all times while hosting.

### 1.9 D26: desktop services leave Tunnels and the galaxy

- `RemoteService.isDesktop => name.startsWith('desktop:')`
  (`lib/src/state/remote_service.dart`).
- `AppStore.remoteServices` skips `isDesktop` (feeds Tunnels' count, the
  galaxy's planets, the tunnel editor's suggestions).
- New `Set<String> AppStore.desktopServiceNames` = names of `relayServices` in
  `ownNamespace` with `isDesktop` (online status, D22). The admin Relay page
  keeps listing everything.

---

## 2. Files and widgets

### 2.1 File map

| File | Status | Contents |
| --- | --- | --- |
| `app.dart` | changed | §1.4, §1.5, §1.7, §1.8 |
| `shell/app_scope.dart` | changed | §1.1 |
| `shell/navigation.dart` | changed | §1.2 |
| `shell/page_frame.dart` | changed | §6 title bar, `onBack`, `sharingIndicator` |
| `shell/window_chrome.dart` | changed | §6 `CaptionButtons(tone)`, `CaptionGlyph` |
| `shell/sharing_indicator.dart` | new | §1.6 |
| `state/app_store.dart` | changed | §4.6 |
| `state/remote_service.dart` | changed | `isDesktop` |
| `state/desktop_store.dart` | changed by the pipeline package | UI contract §4.2 |
| `state/remote_computers.dart` | new | `RemoteComputerStore`, `RemoteComputer`, `RemoteComputerVault`, `RememberSupport` |
| `state/remote_desktop.dart` | new | `RemoteDesktop`, `ViewerHost` |
| `state/viewer_prefs.dart` | new | `ViewerPrefs` |
| `pages/remote/remote_page.dart` | new | `RemotePage` (wide + compact layouts) |
| `pages/remote/host_card.dart` | new | `HostCard`, `HostCardPhase`, `hostCardPhase()`, `SetupSteps`, `ViewerPanel`, `InviteWell`, `DisplayPicker`, `HostMoreMenu` |
| `pages/remote/invite_qr.dart` | new | `InviteQrPanel`, `QrInvitePainter` |
| `pages/remote/computer_list.dart` | new | `RemoteComputerList`, `ComputerRow`, `ComputerStatus`, `computerStatusOf()`, `AddComputerTile`, `RemoteEmptyCard`, `RemoteEmptyIllustration` |
| `pages/remote/computer_list_compact.dart` | new | `FeaturedComputerCard`, `CompactComputerRow`, `AddComputerButtons` |
| `pages/remote/add_computer.dart` | new | `AddComputerController`, `AddComputerResult`, `showAddComputer()`, `AddComputerDialog`, `AddComputerSheet`, `InviteField`, `RecognisedHostCard` |
| `pages/remote/qr_scan_view.dart` | new | `QrScanView` (mobile_scanner) |
| `pages/remote/remote_format.dart` | new | `maskInvitation`, `formatLastSeen`, `formatSessionDuration`, `pathLabel` |
| `pages/remote/viewer/viewer_screen.dart` | new | `RemoteViewerScreen` |
| `pages/remote/viewer/viewer_palette.dart` | new | `ViewerPalette`, `viewerTheme()` |
| `pages/remote/viewer/desktop_viewer.dart` | new (replaces `pages/desktop_viewer.dart`) | `DesktopViewer` |
| `pages/remote/viewer/viewer_title_bar.dart` | new | `ViewerTitleBar` |
| `pages/remote/viewer/viewer_toolbar.dart` | new | `ViewerToolbar`, `ToolbarController`, `ToolbarHandle`, `ToolbarStatusChip` |
| `pages/remote/viewer/viewer_menus.dart` | new | `DisplayMenu`, `ShortcutsMenu`, `ViewerMoreMenu`, `RemoteShortcut`, `kRemoteShortcuts`, `TypeTextDialog` |
| `pages/remote/viewer/quality_panel.dart` | new | `QualityPanel`, `qualityRows()` |
| `pages/remote/viewer/viewer_overlays.dart` | new | `ViewerOverlayKind`, `overlayFor()`, `ConnectingCard`, `SecureDesktopCard`, `ReconnectPill`, `ReconnectStill`, `IdentityCard`, `EndedCard`, `RecoveringPill`, `DegradedPill`, `ViewOnlyPill`, `ControlHint` |
| `pages/remote/viewer/viewer_stage.dart` | new | `ViewerStage` (picture, surface, cursor, overlays; shared by both viewers) |
| `pages/remote/viewer/jpeg_picture.dart` | new | `JpegPicture` (the Dart JPEG path, moved from `desktop_viewer.dart:111-165`) |
| `pages/remote/viewer/remote_surface.dart` | new (replaces `pages/desktop_gesture_surface.dart`) | `RemoteSurface`, `RemoteCursorLayer`, `TouchIndicator`, `RemoteCursorPainter` |
| `pages/remote/viewer/touch_gestures.dart` | new | `TouchGestureMachine`, `TouchMode`, gesture constants |
| `pages/remote/viewer/remote_viewport.dart` | new | `RemoteViewport`, `ViewerScaleMode` |
| `pages/remote/viewer/remote_input.dart` | new | `RemoteInput`, `RemotePointerSink`, `RemoteButton` |
| `pages/remote/viewer/modifier_latch.dart` | new | `ModifierLatch`, `LatchState` |
| `pages/remote/viewer/host_keys.dart` | new | `HostKey` (mirror of Rust `Key`), `hostKeyForHardware()`, `hostKeyForChar()` |
| `pages/remote/viewer/touch_viewer.dart` | new | `TouchViewer` |
| `pages/remote/viewer/touch_controls.dart` | new | `GutterLayout`, `gutterLayoutFor()`, `GutterButton`, `QualityGutterButton`, `ModeHint` |
| `pages/remote/viewer/remote_text_input.dart` | new | `RemoteTextInput`, `ComposingPreview` |
| `pages/remote/viewer/modifier_bar.dart` | new | `ModifierBar`, `KeyCap`, `ModifierStatusPill` |
| `pages/remote/viewer/minimap.dart` | new | `Minimap`, `MinimapPainter` |
| `pages/remote/viewer/virtual_mouse.dart` | new | `VirtualMousePad` |
| `pages/remote/viewer/gesture_sheet.dart` | new | `GestureSheet`, `GestureGlyph`, `GestureGlyphPainter` |
| `pages/remote/viewer/side_panel.dart` | new | `showViewerSidePanel()`, `ViewerShortcutsPanel`, `ViewerMorePanel` |
| `widgets/dashed_outline.dart` | new | `DashedOutline` (add tiles, mobile dashed buttons) |
| `widgets/status.dart` | changed | `StatusChip(inkLabel: bool)` (spec §2.3.1 pill text in status colour) |
| `widgets/surfaces.dart` | changed | `EmptyNote` icon 18 (§5.2) |
| `theme/glass.dart` | changed | `GlassSegment.enabled` (a disabled segment: the viewer's Control while the host disallows it) |
| `theme/app_icons.dart` | changed | §5 |
| `theme/app_theme.dart` | changed | `quietText(scheme)` token (spec §2.0); base `IconButtonThemeData` `iconSize: 18` |
| `pages/settings_page.dart`, `pages/logs_page.dart` | changed | §1.3 |
| `pages/tunnels_page.dart` | changed | `open-desktop` button deleted |
| `pages/setup_flow.dart` | changed | caption buttons per §6 |
| `common/tray/tray_service.dart` | changed | §1.7 |
| `tool/desktop_viewer_acceptance.dart` | changed | new `DesktopViewer` constructor (§2.3.1) |
| `pages/desktop_page.dart`, `pages/desktop_viewer.dart`, `pages/desktop_connection_banner.dart`, `pages/desktop_gesture_surface.dart` | **deleted** | |
| `pubspec.yaml` | changed | `+ qr: 3.0.3`, `+ mobile_scanner: 7.5.0`; `- cupertino_icons` (unused) |
| `android/app/src/main/AndroidManifest.xml`, `ios/Runner/Info.plist` | changed | camera (mobile_scanner merges `CAMERA`, `required=false`); `NSCameraUsageDescription` (en + zh `InfoPlist.strings`). The `allowBackup` exclusions of D20 belong to the vault package, not this one |

### 2.2 Remote page (`pages/remote/`)

#### 2.2.1 `RemotePage`

```dart
class RemotePage extends StatefulWidget { const RemotePage({super.key}); }
class _RemotePageState extends State<RemotePage> {
  late final DesktopInterestLease _lease;      // §4.2 retain(DesktopInterest.remotePage)
  @override void initState() { … _lease = remote.desktop.retain(DesktopInterest.remotePage);
                               unawaited(store.checkRelay()); }   // fresh online dots (D22), not forced
  @override void dispose() { _lease.release(); … }
}
```

`PageFrame(title: wide ? navRemoteDesktop : navRemote, sharingIndicator: false,
actions: [if (wide && computers.isNotEmpty) IconButton(key: 'remote-refresh', AppIcons.refresh, tooltip: refresh)],
primary: (computers.isNotEmpty || !wide) ? PrimaryAction(key: Key('remote-add'), icon: AppIcons.add, label: remoteAddComputer, onPressed: () => showAddComputer(context)) : null,
body: …)`. Refresh = `store.checkRelay(force: true)` + `desktop.refreshHost()`.
HostSetup hides the header actions while the list is empty
(`HostSetup:22-29`); compact (MobileHome, D19: shared `PageFrame`, no large
title) always shows the add action.

Body (`ListView(key: PageStorageKey('remote-page'))`):

- **Wide** (`ShellLayout.wideOf`): padding `fromLTRB(32, 4, 32, 32)`
  (`Main:38`). Two columns when the inner width ≥ 1000
  (`Row(crossAxisAlignment: start, [SizedBox(width: 560, HostCard), SizedBox(width: 20), Expanded(RemoteComputerList)])`),
  otherwise one centred column, max width 760 (host card above the list). At the
  default 1180×780 window the sheet is 960 wide, so one column; the Main
  artboard's two columns appear from a 1276-px window.
- **Compact**: padding `fromLTRB(16, 4, 16, 24)`: `[HostCard if host-capable]`,
  `FeaturedComputerCard`, section label `remoteOthers`, other rows,
  `AddComputerButtons`, footer note (`remoteMobileHostNote` when the device
  cannot host).
- Not host-capable on desktop (macOS/Linux): no host card; the list goes full
  width; one note line `hostUnsupported` under it.

Each card listens to its own sources (§4.1); the page `build` itself depends
only on layout and on the computers list length.

#### 2.2.2 Host card state machine (`host_card.dart`)

```dart
enum HostCardPhase { loading, unsupported, notEnabled, enabling, restarting, sharing, paused }
enum HostViewer { none, watching, controlling }        // from DesktopSharing

HostCardPhase hostCardPhase(DesktopStore d) => switch (()) {
  _ when d.capabilities == null                         => HostCardPhase.loading,
  _ when !d.capabilities!.host                          => HostCardPhase.unsupported,
  _ when d.restarting                                   => HostCardPhase.restarting,
  _ when d.hostAction == HostAction.enabling            => HostCardPhase.enabling,
  _ when d.sharing.value.hosting                        => HostCardPhase.sharing,
  _ when d.trust.saved != null || d.snapshot.background?.configured == true
                                                        => HostCardPhase.paused,
  _                                                     => HostCardPhase.notEnabled,
};
```

`hostError` is orthogonal: an inline `error`-coloured line under the header in
every phase. `HostCard` is `ListenableBuilder(listenable: desktop.hostChanges)`.

| Phase | Card | Primary | Transitions |
| --- | --- | --- | --- |
| loading | `GroupCard` with header skeleton + 16 px spinner | — | → any (first `capabilities`) |
| unsupported | not shown (§2.2.1) | — | — |
| notEnabled (HostSetup) | grey tile (`AppIcons.remoteLarge` 24/300 in 44×44 r12 `cardFieldFill`), "这台电脑", `hostOff`, grey pill `hostNotSharing`; headline 22/600 (`setupHeadline`); `SetupSteps(active: 1)`; `SwitchListTile` `Key('host-allow-control')` `hostAllowControl` (default **on**, `HostSetup:73`); `DisplayPicker`; under the button: `TextButton(Key('host-enable-ordinary'), setupOrdinaryOnly)` + note `setupBackgroundNote` | `FilledButton.icon(Key('host-enable'), AppIcons.helper, hostEnable)`, 48 tall, full width, radius 24 | Enable → `desktop.enableHost(service: defaultService, displayId: d, allowControl: v, helper: true)` → enabling. Ordinary link → same with `helper: false` |
| enabling | same card; button shows 18 px spinner + `hostEnabling`, disabled; `SetupSteps(active: 1)` (UAC prompt pending) | — | → restarting (`restart: true`) / sharing / notEnabled + error |
| restarting | `SetupSteps(active: 2)`; button `hostRestarting` | — | app quits; relaunch with `--desktop` lands here in `sharing` |
| sharing (Main) | blue tile (`AppIcons.remoteLarge` 24/300, `signalColor`, `tertiaryContainer`), "这台电脑", service mono 12.5 `quietText`, `StatusChip(inkLabel: true, color: successColor, label: hostSharing)`; `ViewerPanel` when `sharing.viewer != none`; permission `GlassSegmented<bool>(key: Key('host-permission'))` (`hostPermissionView` / `hostPermissionControl`, capsule corners per D19); `InviteWell`; `InviteQrPanel` when revealed; rows: `DisplayPicker`, background (`hostBackgroundService` or `hostBackgroundApp`), helper (`hostHelperReady` ✓ success / `hostHelperStopped` / `hostHelperNone` / `hostHelperUpdate` with inline `TextButton(Key('host-helper-update'))`); actions row | `OutlinedButton(Key('host-stop'), hostStop)` 40 tall radius 20 1 px ink; `HostMoreMenu`; note `hostStopNote` | Stop → paused. Permission → `allowControl(v)` (live, D24). Revoke → notEnabled |
| paused | sharing card without `ViewerPanel`; grey pill `hostNotSharing`; invite still shown (the remembered grant stays valid) | `FilledButton.icon(Key('host-start'), AppIcons.screenShare, hostStart)` | Start → `desktop.resumeHost()` → enabling → sharing |

`SetupSteps(active, helperReady)`: three rows (disc 26, title 14.5/600, body
13 `onSurfaceVariant`); the active disc is ink with a white numeral, others
`backdropBase` with ink numeral; when `capabilities.helper` is installed and
verified, discs 1–2 show `AppIcons.check` on `successColor` (only step 3
remains; `HostSetup` proposal). `defaultService` = `'desktop:'` + lowercase
`Platform.localHostname` reduced to `[a-z0-9._-]`, ≤ 120 chars (must satisfy
`valid_service`, `crates/pb-mapper-desktop/src/lib.rs:49-56`).

`ViewerPanel` (`Main:52-59`): `cardFieldFill` r14, padding 14/16:
`AppIcons.remoteDevices` 20 `signalColor` (device type is unknown, D24) ·
title `hostViewersControlling(1)` / `hostViewersWatching(1)` 14.5/600 · meta
`hostViewerMeta(path, duration)` 12.5 (path omitted when unknown — an older
helper sends none) · buttons `OutlinedButton(Key('host-make-view-only'), hostMakeViewOnly)`
(only while control is allowed) and `TextButton(Key('host-disconnect-viewer'), hostDisconnectViewer)`.
A `Timer.periodic(30 s)` refreshes the duration while mounted
(`formatSessionDuration`: < 1 min "刚刚", < 60 "{n} 分钟", else "{h} 小时 {m} 分钟").

`InviteWell` (`Main:69-83`): 48-tall well `cardFieldFill` r14; masked invite
mono 13 (`maskInvitation`: `pbd1·` + first 4 + 20 × `•` + last 4 of the payload;
a legacy JSON invite shows `json·` + 24 × `•`); `OutlinedButton.icon(Key('host-copy'), AppIcons.copy 16, copy)`
→ `Clipboard.setData` + toast `hostInviteCopied`;
`OutlinedButton.icon(Key('host-qr'), AppIcons.qrCode 16, hostQr | hostQrHide)`;
help `hostInviteHelp` 12.5 `quietText`. The full secret is never rendered.

`InviteQrPanel` (`invite_qr.dart`, proposal spec §2.3.1): white plate even in
dark mode (scanners need dark-on-light), radius 14, padding 16; QR from
`QrCode.fromData(data: invitation, errorCorrectLevel: QrErrorCorrectLevel.M)`
→ `QrImage`, drawn by `QrInvitePainter` with a 4-module quiet zone and the
module size snapped to whole device pixels
(`m = ((220 * dpr) / (n + 8)).floorToDouble() / dpr`); caption `hostQrCaption`.
Visibility is `InviteWell` state `_showQr`, reset in `deactivate()`, on any
`AppLifecycleState` other than `resumed` (so it never lands in task-switcher
snapshots), and when `invitation` changes. Never logged, never persisted.

`DisplayPicker`: a `MenuAnchor` button (not `DropdownButtonFormField`, which
draws Material's `arrow_drop_down`): label `hostDisplayValue(n, w, h)` (index,
never the OS device name) + `AppIcons.chevronDown` 16. While sharing, choosing
another display asks (`hostDisplaySwitchBody`) then
`desktop.setHostDisplay(id)` (a display switch is an allowed resolution
change, D5).

`HostMoreMenu` (`Key('host-more')`, ghost button `AppIcons.more` 16 + `more`):
`host-rename` (`hostServiceName`… → dialog; while sharing it warns
`hostServiceRenameBody`) → `desktop.renameHostService(name)`;
helper items from `capabilities.helper`: `host-helper-install`
(`hostHelperInstall`), `host-helper-disable`, `host-helper-uninstall`; divider;
`host-revoke` in `scheme.error` → `confirmDestructive(hostRevoke, hostRevokeConfirmBody)`
→ `desktop.revokeTrust()`.

#### 2.2.3 Remote computer list, wide (`computer_list.dart`)

`RemoteComputerList` (`Main:112-165`): `GroupCard`, header `remoteListTitle`
17/600 + count pill. Rows ordered: active session, then `online` by
`lastConnectedAt` desc, then `unknown`, a 1 px `outlineVariant` divider inset
12, then `offline`.

```dart
enum ComputerStatus { viewing, online, offline, unknown }
ComputerStatus computerStatusOf(RemoteComputer c, {required DesktopSession? session,
    required RelayState relay, required Set<String> desktopServices}) =>
  session?.computerId == c.id ? ComputerStatus.viewing
  : relay != RelayState.online ? ComputerStatus.unknown
  : desktopServices.contains(c.service) ? ComputerStatus.online
  : ComputerStatus.offline;
```

`ComputerRow` (`Key('computer-<id>')`), padding 12, radius 14, gap 14:
tile 40 r11 (`AppIcons.remote` 20; `signalColor` on `tertiaryContainer` when
viewing, `onSurfaceVariant` on `cardFieldFill` online, `onSurfaceMuted`
offline) · name 15/600 + service mono 12 `quietText` · meta 12.5 (fixed 170) ·
status pill (fixed 86, `StatusChip(inkLabel: true)`) · action (fixed 96×34,
radius 17). Below 600 px row width the meta moves under the service line.

| Status | Row fill | Meta | Pill | Action |
| --- | --- | --- | --- | --- |
| viewing | `tertiaryContainer @ 0.45` | `remoteViewingMeta(path, rtt, fps)` from `desktop.stats` (1 Hz, its own builder); `remotePicturePaused(path)` while detached (D13) | `remoteViewing` (`signalColor`) | ink `FilledButton(Key('computer-open-<id>'), remoteBackToPicture)` → `remote.viewer.open()` |
| online | — | `remoteLastPath(when, path)` | `remoteOnline` (success) | ink `remoteOpen` → `remote.open(context, c)` |
| unknown | — | `remoteLast(when)` | none | ink `remoteOpen` (relay can't say; try) |
| offline | — | `remoteLast(when)` | `remoteNotSharing` (grey) | disabled outlined `remoteOpen`, tooltip `remoteOfflineHint` |

Row actions (spec §2.3.2 proposal): hover reveals `IconButton(Key('computer-more-<id>'), AppIcons.more 18)`
before the action button; secondary click opens the same `MenuAnchor` at the
pointer: `remoteRename` → dialog → `computers.rename`; `remoteRemove` →
`confirmDestructive(remoteRemoveConfirmTitle(name), remoteRemoveConfirmBody)`
→ `computers.remove` (deletes invite and thumbnail). An unremembered active
session appears as a transient viewing row whose menu offers `addRemember`.

`AddComputerTile` (`Main:159-163`): 56 tall, `DashedOutline` (1.5 px
`outline`, dash 4/3, r14), `AppIcons.add` 18 + `remoteAddComputer` 14/500 +
`remoteAddHint` 13 `quietText` → `showAddComputer`. Footnote
`remoteRememberNote`, or `addRememberUnavailable` when the vault is
unsupported (D20).

`RemoteEmptyCard` (no computers and no session, `HostSetup:96-115`): min
height 520; `RemoteEmptyIllustration` (`CustomPainter`, 120×84, strokes per
spec §2.4); `remoteEmptyTitle` 16/600; `remoteEmptyBody`; outlined
`Key('remote-add-empty')` `remoteAddComputer`.

#### 2.2.4 Remote tab, compact (`computer_list_compact.dart`, MobileHome)

- `FeaturedComputerCard(Key('computer-featured'))`: the active session's
  computer, else the most recently connected. Thumbnail 16:9
  `Image.file(computers.thumbnailOf(id), cacheWidth: (w * dpr).round(), gaplessPlayback: true)`
  or placeholder (`#1F2328`, `AppIcons.remoteLarge` 24/300 `onSurfaceMuted`);
  badge `remoteLastPicture(when)`; name 17/600; sub `remotePathPermission(path)`
  / `remotePathViewOnly(path)` (last session's path and permission); status
  pill; full-width 46-tall ink button `remoteOpen` / `remoteBackToPicture`.
- `CompactComputerRow(Key('computer-<id>'))`: 64 tall, tile 40, name 15/600,
  sub with a 6 px dot (`remoteOnlineLast(path)` / `remoteOfflineLast(when)` /
  `remoteLast(when)`), `AppIcons.chevronRight` 18. Tap → `remote.open(context, c)`;
  offline → toast `remoteOfflineToast(name)` instead. Long press → bottom sheet
  (rename / remove).
- `AddComputerButtons`: on Android/iOS two `DashedOutline` buttons, 52 tall,
  r16: `Key('remote-scan')` `AppIcons.scanQrCode` 18 `remoteScanAdd` →
  `showAddComputer(mode: AddMode.scan)`; `Key('remote-paste')`
  `AppIcons.paste` 18 `remotePasteInvite` → `AddMode.paste` (reads the
  clipboard at once). Elsewhere one `AddComputerTile`.
- Empty (spec proposal): only the two buttons and the note.

#### 2.2.5 Add computer (`add_computer.dart`, `qr_scan_view.dart`)

```dart
enum AddMode { auto, scan, paste }
Future<void> showAddComputer(BuildContext context,
    {AddMode mode = AddMode.auto, RemoteComputer? replacing});
```

- Non-touch platform or width ≥ 720 → `showDialog(barrierColor: Color(0x52141416), builder: (_) => AddComputerDialog(...))`
  (560 wide, r24, padding 24/28/28; spec §2.5).
- Touch and compact → `showModalBottomSheet(isScrollControlled: true, useSafeArea: true, builder: (_) => AddComputerSheet(...))`.
- Both pop an `AddComputerResult` and the caller runs
  `remote.connectNew(result)` (§4.4). The dialog closes immediately; the
  viewer opens on the Connecting overlay (§3.8) — there is no spinner wait
  inside the dialog.

`AddComputerController extends ChangeNotifier` (pure; injected
`inspect: Future<DesktopInvitationInfo> Function(String)`,
`computers: RemoteComputerStore`):

```dart
enum AddState { empty, inspecting, recognised, invalid }
enum InviteRejection { invalid, setupString }
class AddComputerController extends ChangeNotifier {
  AddState state; DesktopInvitationInfo? info; InviteRejection? rejection;
  RemoteComputer? duplicate;     // same service + fingerprint: Connect opens it
  RemoteComputer? conflict;      // same service, other fingerprint: replace only if confirmed
  bool replaceConfirmed = false;
  String name; bool remember;    // remember defaults to computers.support.available
  bool relayOnly = false;
  void edit(String text);        // debounced 250 ms
  void paste(String text);       // inspects at once
  void clear();
  AddComputerResult? result();   // null unless recognised && (conflict == null || replaceConfirmed)
}
```

The raw invitation is a private field; widgets never read it. Each inspect
increments a serial; stale replies are dropped. `SETUP_STRING` →
`InviteRejection.setupString` (`addSetupString`), any other failure →
`addInvalid`. Default name = service without `desktop:`; a duplicate takes the
saved name. `replacing` (from the identity-changed overlay) prefills the name
and expects the same service; a different service shows `addOtherComputer`
and is added as a new computer.

Widgets:

- `InviteField`: editing → `TextField(key: Key('add-invite'), minLines: 3, maxLines: 3, style: mono(13), autocorrect: false, enableSuggestions: false, enableIMEPersonalizedLearning: false, smartDashesType: disabled, smartQuotesType: disabled)`
  + inline `OutlinedButton.icon(Key('add-paste'), AppIcons.paste 16, paste)`;
  recognised → a read-only well with `maskInvitation(raw)` (first 8, last 4,
  `AddComputer:35-44`) + `IconButton(Key('add-invite-clear'), AppIcons.close)`.
  Errors under the field in `scheme.error`.
- `RecognisedHostCard(Key('add-recognised'))`: `tertiaryContainer @ 0.45`, r14:
  `AppIcons.helper` 20 `signalColor` · service mono 13 ·
  `addRecognized(fingerprint)` 12.5. `duplicate` → `addDuplicate(name)`.
  `conflict` → `addIdentityConflict(name)` + `SwitchListTile(Key('add-replace'), addReplaceConfirm)`.
- Name `TextField(Key('add-name'))`; `SwitchListTile(Key('add-remember'), addRemember, addRememberHelp | addRememberHelpMobile)`,
  disabled with `addRememberUnavailable` when the vault is unsupported (D20);
  disclosure `Key('add-more')` `addMoreOptions` → `SwitchListTile(Key('add-relay-only'), remoteRelayOnly)`.
- Footer (dialog): `TextButton(cancel)`, `FilledButton(Key('add-connect'), connect)`
  enabled iff `result() != null`.
- `AddComputerSheet` (MobileAdd): grabber, title `addTitle`, close; in scan
  mode a 330-tall `QrScanView`; divider with `or`; `OutlinedButton.icon(Key('add-paste'))`
  full width 48; once recognised the viewfinder is replaced by
  `RecognisedHostCard` + name + full-width `add-connect`; switches; footer
  `cameraNote`.

`QrScanView` (Android/iOS only, `kQrScanSupported`): `MobileScanner(controller:
MobileScannerController(formats: [BarcodeFormat.qrCode], detectionSpeed:
DetectionSpeed.noDuplicates, returnImage: false))` — no frame bytes reach Dart
(D21). `onDetect` → first `rawValue` → `controller.stop()` →
`AddComputerController.paste(value)`. Reticle painter (220×220, four 28×28
L-corners 3 px white r10); scan line 2 px `#7DB6F2` animating top↔bottom over
1.6 s, static under reduced motion. `errorBuilder`: permission denied →
`AppIcons.scanQrCodeLarge` 24/300 + `scanPermissionDenied` (no settings button:
no `permission_handler`, D21; paste stays one tap away); no camera →
`scanUnavailable`. A `WidgetsBindingObserver` stops the controller on any
state other than `resumed` and restarts it on `resumed`; disposed with the
sheet. Scanned text never auto-connects: it goes through the same
recognised-host step.

### 2.3 Desktop viewer (`pages/remote/viewer/`)

#### 2.3.1 `DesktopViewer`

```dart
class DesktopViewer extends StatefulWidget {
  const DesktopViewer({super.key, required this.desktop, required this.prefs,
      required this.title, this.onBack, this.onDisconnect, this.onPasteNewInvite});
  final DesktopStore desktop; final ViewerPrefs prefs; final String title;
  final VoidCallback? onBack, onDisconnect, onPasteNewInvite;
}
```

Explicit dependencies (no `AppScope` reads) so the acceptance harness
(`tool/desktop_viewer_acceptance.dart:71-79`) can host it with
`ViewerPrefs(persist: false)`.

State: `RemoteViewport _viewport`, `ModifierLatch _latch`, `RemoteInput _input`,
`ToolbarController _toolbar`, `FocusNode _picture`, `bool _fullscreen`
(`WindowListener.onWindowEnterFullScreen/LeaveFullScreen`).

Layer tree (ground `#0F0F10`):

```
Column[
  if (!_fullscreen) ViewerTitleBar(title, onBack)                      // 36
  Expanded(Focus(focusNode: _picture, autofocus: true,
                 onKeyEvent: _input.handleHardwareKey,                 // §3.5
                 onFocusChange: (f) { if (!f) _input.releaseAll(); },  // §3.9
    child: Stack[
      ViewerStage(desktop, viewport, input, touchMode: TouchMode.direct,
                  onPasteNewInvite, onBack)                            // picture + overlays
      ViewerToolbar(controller: _toolbar, …)                           // top 12, centred
      QualityPanel                     (prefs.qualityPanel; toolbar bottom + 8, right 14)
      ViewOnlyPill                     (bottom 34, left 12; controlAllowed == false)
      ControlHint                      (bottom 64, centred; 3 s on control gain)
    ]))
]
```

`DesktopViewer.build` depends only on `_fullscreen` and `title`. Everything
that changes at media or state rate is a leaf `ValueListenableBuilder`
(§4.1), so the `Texture` subtree is built once per texture id.

#### 2.3.2 `ViewerTitleBar` (`Viewer:17-27`)

`SizedBox(height: 36)`; `Stack[Positioned.fill(WindowDragArea()), Padding(start: trafficLightInset + 8, end: drawsCaptionButtons ? 4 : 12, Row[…])]`:
`_TitleBack` (28×28 r6, `AppIcons.back` 16 `#B4B4B8`,
`Key('viewer-back')`, tooltip `viewerBackToList`, I5) · 8 ·
`AppIcons.remote` 16 `#7DB6F2` · 10 · `IgnorePointer(Text(title, 13/600 #EDEDEF))` ·
10 · `IgnorePointer(Text('pb-mapper', 12.5 #8E8E93))` · `Spacer` (drag) ·
`SharingIndicator(tone: CaptionTone.viewer)` · 12 ·
`if (drawsCaptionButtons) CaptionButtons(tone: CaptionTone.viewer)` (46×32,
centred: 2 px above/below; 4 px end padding — no sheet corner to clear).
Hidden in full screen.

#### 2.3.3 Floating toolbar (`viewer_toolbar.dart`, `Viewer:33-76`)

`ViewerToolbar` = `MouseRegion(onEnter/onExit → controller)` around a
`Padding(top: 12)` + the toolbar (the top padding is part of the region, so
the handle zone at y 0–20 counts as "inside" once expanded). Expanded:
48 tall, r16, padding 0/6, gap 2, fill `ViewerPalette.chrome`
(`rgba(32,32,35,0.9)`) + `BackdropFilter(blur 20)`, 1 px `ViewerPalette.edge`,
shadow `0 8 30 rgba(0,0,0,0.4)`; `Semantics(container: true, label: viewerToolbar)`.
Items (keys):

1. `viewer-pin` 36×36: `AppIcons.pin` (pinned, fill `white@0.1`) /
   `AppIcons.unpin`; tooltip `viewerUnpin` / `viewerPin` →
   `controller.setPinned` + `prefs.setToolbarPinned`.
2. sep · `ToolbarStatusChip(Key('viewer-quality-chip'))` from `desktop.stats`
   (1 Hz): dot 8 (`success` normal / `caution` when `stats.degraded` /
   `onSurfaceMuted` while connecting) + `pathLabel(path)` 13 + mono 12
   `#B4B4B8` `"{rtt} ms · {fps} fps"`; degraded form `viewerSlow(path)` +
   mono caution `"{latency} ms"` (`ViewerStates:40-43`). Tap toggles
   `prefs.qualityPanel`.
3. sep · mode `GlassSegmented<bool>(key: Key('viewer-mode'), height: 30)`:
   `false` = `AppIcons.viewOnly` + `viewerModeView`, `true` =
   `AppIcons.control` + `viewerModeControl`; value `viewerState.controlWanted`;
   `onChanged` → `desktop.control(v)`. `GlassSegment` gains `bool enabled = true`
   (Control disabled while `controlAllowed == false`, tooltip
   `viewerViewOnlyByHost`). While control is wanted but not yet granted the
   Control segment's icon is a 12 px spinner.
4. sep · `DisplayMenu` button `Key('viewer-display')`: `AppIcons.remote` 16 +
   `viewerDisplay` 13 + `AppIcons.chevronDown` 16 (spec deviation: the drawn
   rect-with-bars glyph → `monitor`).
5. `ShortcutsMenu` button `Key('viewer-shortcuts')`: `AppIcons.shortcuts` 16 +
   `viewerShortcuts` + chevron; open state fill `white@0.1`.
6. `ViewerMoreMenu` `Key('viewer-more')` 36×36 `AppIcons.more` 18.
7. sep · `Key('viewer-fullscreen')` 36×36 `AppIcons.fullscreen` /
   `AppIcons.exitFullscreen`, tooltip `viewerFullscreen` /
   `viewerExitFullscreen` → `windowManager.setFullScreen(!_fullscreen)`. Esc
   and F11 go to the remote; only this button toggles.
8. `Key('viewer-disconnect')`: `AppIcons.disconnect` 16 + `viewerDisconnect` 13,
   `#F2B8B5` → `onDisconnect` (`remote.disconnect()`).

Below 760 px of picture width the labels of 4, 5 and the mode segments are
hidden (icons + tooltips only).

Collapsed: `ToolbarHandle(Key('viewer-toolbar-handle'))` 72×20 at top 0,
centred, radius `vertical(bottom: 10)`, 1 px edge without top, fill
`ViewerPalette.chrome`, `AppIcons.chevronDown` 14; tooltip/semantics
`viewerExpand`. Hover 300 ms or tap → expand (I7). When collapsed and
`stats.degraded`, `DegradedPill` sits at top 28 so degradation is never
hidden (spec §2.7.2). Expand/collapse animate with
`AnimatedSlide`+`AnimatedOpacity` over `Motion.of(context, Motion.medium)`
(instant under reduced motion). The state machine is §3.7.

#### 2.3.4 Menus (`viewer_menus.dart`)

All three are `MenuAnchor`s with `ViewerPalette.menuStyle` (fill `#262628`, r14,
padding 6, 1 px edge, shadow `0 12 40 rgba(0,0,0,0.5)`) and items built by one
helper (`MenuItemButton`, 36 tall, r9, label 13.5 `#EDEDEF`, trailing hint mono
12 `#B4B4B8`, hover `white@0.08`, leading 16-px `AppIcons.check` slot for
radio/check items). `onOpen` → `toolbar.hold()`; `onClose` →
`toolbar.unhold()` and `_picture.requestFocus()` (§3.9).

- `DisplayMenu` (260 wide, not drawn; spec §2.6.1): section `viewerScaleLabel`;
  `display-fit` `viewerScaleFit`, `display-original` `viewerScaleOriginal`
  (local only, P1-4; §3.6); divider; section `viewerQualityLabel`;
  `quality-quality`, `quality-balanced`, `quality-smooth` with titles
  `viewerQualityBest/Balanced/Smooth` and hints `…Hint` → `remote.setQuality(q)`
  (live, D12; remembered per computer); divider; check `display-quality-panel`
  `viewerQuality` ↔ `prefs.qualityPanel`.
- `ShortcutsMenu` (300 wide, `Viewer:78-89`) from `kRemoteShortcuts`:

  | Key | Label | Hint | Sends | Enabled |
  | --- | --- | --- | --- | --- |
  | `shortcut-show-desktop` | `shortcutShowDesktop` | Win + D | `meta↓ d↓ d↑ meta↑` | `input.ready` |
  | `shortcut-switch-window` | `shortcutSwitchWindow` | Alt + Tab | `alt↓ tab↓ tab↑ alt↑` | ready |
  | `shortcut-task-manager` | `shortcutTaskManager` | Ctrl + Shift + Esc | `control↓ shift↓ escape↓ escape↑ shift↑ control↑` | ready |
  | `shortcut-start` | `shortcutStart` | Win | `meta↓ meta↑` | ready |
  | `shortcut-lock` | `shortcutLock` | Win + L | `desktop.lock()` (D17 host command; failure → toast `shortcutLockFailed(reason)`) | ready |
  | — | divider | | | |
  | `shortcut-cad` | Ctrl + Alt + Del | `shortcutUnsupported` (12, not mono) | nothing | never (security boundary) |
  | — | divider | | | |
  | `shortcut-refresh` | `shortcutRefresh` | | `desktop.requestKeyframe()` | always |
  | `shortcut-release` | `shortcutReleaseKeys` | | `input.releaseAll()` (§3.9) | always |

  Combos go through `RemoteInput.combo()` → one `desktop.sendSequence` (§3.4);
  they neither consume nor apply the latches.
- `ViewerMoreMenu`: `viewer-type-text` `viewerTypeText` → `TypeTextDialog`
  (dark `AlertDialog`, `TextField(autofocus, maxLines: 6)`, `viewerSendText` →
  `input.typeText(text)` chunked to ≤ 1024 UTF-8 bytes per `text` event);
  `viewer-more-quality` check `viewerQuality`; `viewer-more-back`
  `viewerBackToList` → `onBack`.

#### 2.3.5 Quality panel (`quality_panel.dart`, `ViewerStates:44-54`)

`QualityPanel(Key('viewer-quality-panel'))`, 236 wide, padding 14/16, r14,
`ViewerPalette.panel`, title `viewerQuality` 13/600, rows (label `#B4B4B8`
flex, value right, mono where marked), rebuilt by
`ValueListenableBuilder(desktop.stats)` at ≤ 1 Hz:

| Row | Value | Mono |
| --- | --- | --- |
| `qualityPath` | `pathLabel(path)` (+ ` · {relayTransport}` for relay) | no |
| `qualityRtt` | `{rttMs} ms` or `—` | yes |
| `qualityLatency` | `{pictureLatencyMs} ms`, caution when `degraded` | yes |
| `qualityFps` | `{presentedFps}` | yes |
| `qualityBitrate` | `{receiveKbps/1000:.1} Mbps` | yes |
| `qualityResolution` | `{w} × {h}` | yes |
| `qualityCodec` | `qualityHardware(codec)` / `qualitySoftware(codec)` / codec | no |

Footer 12.5 `#B4B4B8` by `stats.adaptation`: `lowerBitrate` →
`qualityAdaptBitrate`, `lowerFps` → `qualityAdaptFps`, otherwise hidden.
While open it holds the toolbar (`toolbar.hold`).

#### 2.3.6 Overlays and hints (`viewer_overlays.dart`)

Specified with their state mapping in §3.8. Visual specs from spec §2.7:
`ConnectingCard` 320 wide; `SecureDesktopCard` 360 over a 0.72 scrim;
`ReconnectPill` 48 tall + `ReconnectStill`; `IdentityCard` 380; `EndedCard`
(proposal) same chrome as Connecting; `RecoveringPill` (proposal) top 12, 32
tall, 16 px spinner; `DegradedPill` 34 tall; `ViewOnlyPill` 30 tall
(`AppIcons.viewOnly` 16 + `viewerViewOnlyByHost`); `ControlHint` 40 tall,
`AppIcons.keyboard` 16 `#7DB6F2` + `viewerControlHint(name)` +
`viewerControlHintSub` `#B4B4B8`, `IgnorePointer`, shown 3 s when
`viewerState.controlling` turns true and on the first focus of a controlling
viewer.

#### 2.3.7 `ViewerStage` (`viewer_stage.dart`, shared by both viewers)

```dart
class ViewerStage extends StatelessWidget {
  // Takes listenables, not the store, so a widget test can drive phases with
  // ValueNotifiers (§8). Viewers build it with ViewerStage.of(desktop, …).
  const ViewerStage({super.key, required this.state, required this.picture,
      required this.view, required this.stats, required this.viewport,
      required this.input, required this.touchMode, required this.title,
      this.jpeg, this.onPasteNewInvite, this.onBack, this.onDisconnect,
      this.onRetryNow, this.onReconnect, this.compact = false});
  final ValueListenable<DesktopViewerState> state;
  final ValueListenable<DesktopPicture?> picture;
  final ValueListenable<DesktopView?> view;
  final ValueListenable<DesktopStats> stats;
  factory ViewerStage.of(DesktopStore desktop, {…});
}
```

```
Stack(fit: expand)[
  RemoteSurface(viewport, input, touchMode,                     // §3.2/3.3; Listener only
    child: ValueListenableBuilder(picture → Texture(key: ValueKey(id), textureId: id,
           filterQuality: FilterQuality.low) | JpegPicture(jpeg)))
  RemoteCursorLayer(view, viewport)                             // painted remote cursor
  TouchIndicator (trackpad only)                                // 44 circle, fades 150 ms
  ValueListenableBuilder(state → overlayFor(...))               // §3.8 layer
]
```

The picture widget is the same element across zoom, pan, overlays, reconnect
attempts (the native texture is re-bound, not re-created) and keyboard changes
(asserted by the gesture test, §8).

#### 2.3.8 Dark palette (`viewer_palette.dart`)

`viewerTheme(context)` = `buildAppTheme(Brightness.dark)` with
`scaffoldBackgroundColor: Color(0xFF0F0F10)`. `ViewerPalette` holds the
translucent literals of spec §2.0 in one place: `ground #0F0F10`, `black`
(mobile ground), `card #1C1C1E`, `menu #262628`, `chrome rgba(32,32,35,0.9)`,
`panel rgba(28,28,30,0.94)`, `hint rgba(24,24,26,0.86)`, `edge white@0.08`,
`fill white@0.10`, `fillStrong white@0.16`, `separator white@0.12`,
`keySeparator white@0.14`, `outline white@0.18`, `text #EDEDEF`,
`textSecondary #B4B4B8`, `textQuiet #8E8E93`, `signal #7DB6F2`,
`success #57C27A`, `caution #E0A647`, `danger #F2B8B5`, plus `menuStyle`.

### 2.4 Mobile viewer (`touch_viewer.dart` and friends)

#### 2.4.1 `TouchViewer`

```dart
class TouchViewer extends StatefulWidget {
  const TouchViewer({super.key, required this.desktop, required this.prefs,
      required this.title, required this.onBack, required this.onDisconnect,
      this.onPasteNewInvite});
}
```

- `initState`: `SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky)`;
  on phones (`shortestSide < 600`)
  `SystemChrome.setPreferredOrientations([landscapeLeft, landscapeRight])`.
  `dispose`: `setEnabledSystemUIMode(SystemUiMode.manual, overlays: SystemUiOverlay.values)`,
  `setPreferredOrientations(const [])`. Tablets rotate freely; the layout
  adapts by geometry, not orientation (§2.4.2).
- `Scaffold(backgroundColor: ViewerPalette.black, resizeToAvoidBottomInset: false, body: LayoutBuilder(...))`
  — the IME never resizes the stage (I12).
- Layers:

```
Stack[
  ViewerStage(desktop, viewport, input, touchMode: prefs.touchMode, compact: true)
  if (!keyboardOpen) ...gutterControls(gutterLayoutFor(size, aspect, padding))   // §2.4.2
  Minimap            (top 10, right 12 + padding.right; when viewport.visibleRegion != null)
  ModeHint           (top 12, centred over the picture; 3 s on open and on mode change)
  if (prefs.touchMode == TouchMode.direct && prefs.virtualMouse) VirtualMousePad
  ModifierStatusPill (left 12, bottom: occlusion + 10; when latch.active.isNotEmpty)
  if (keyboardOpen) Positioned(left 0, right 0, bottom: viewInsets.bottom, ModifierBar)  // 44
  RemoteTextInput    (no visible size; owns the TextInputConnection)
  if (prefs.qualityPanel) QualityPanel (picture top-left + 12)
]
```

- `keyboardOpen` = `_textInput.attached && MediaQuery.viewInsetsOf(context).bottom > 0`.
- First live session ever (`!prefs.gestureSheetSeen`): after the first
  `live` phase, show `GestureSheet` once.
- `ModeHint`: `AppIcons.trackpad`/`AppIcons.directTouch` 16 `#7DB6F2` +
  `touchTrackpadHint` / `touchDirectHint`, `ViewerPalette.hint`, r16, 32 tall.

#### 2.4.2 Gutters (`touch_controls.dart`, MobileViewer)

```dart
enum GutterKind { side, bars, overlay }
class GutterLayout { final GutterKind kind; final Rect lead, trail; }
GutterLayout gutterLayoutFor(Size screen, double aspect, EdgeInsets padding) {
  final gx = (screen.width - screen.height * aspect) / 2;   // side letterbox
  final gy = (screen.height - screen.width / aspect) / 2;   // top/bottom letterbox
  if (gx >= 64) return side gutters, inset by padding.left/right (cutouts);
  if (gy >= 64) return top and bottom bars (portrait, 4:3 remotes);
  return overlay on the picture's left/right edges with ViewerPalette.hint backgrounds;
}
```

Lead gutter (column, `spaceBetween`, padding 16/0):
`GutterButton(Key('touch-back'), AppIcons.back, a11y touchBack)` →
`onBack`; `QualityGutterButton(Key('touch-quality'))` 56 wide (dot 8,
`pathLabel` 11, fps mono 14/500, "fps" 10.5; a11y `touchQualityA11y(path, fps)`)
→ toggles `prefs.qualityPanel`; `GutterButton(Key('touch-mode-toggle'))`:
trackpad = fill `rgba(125,182,242,0.2)`, `AppIcons.trackpad` 20 `#7DB6F2`;
direct = `white@0.1`, `AppIcons.directTouch`; label 10.5
`touchModeTrackpad`/`touchModeDirect`; a11y `touchModeToggleA11y(mode)`; tap →
`prefs.setTouchMode(...)` + `ModeHint`; long press → `GestureSheet`.

Trail gutter (`Semantics(label: touchControls)`, centred column, gap 14):
`touch-keyboard` (`AppIcons.keyboard`, `touchKeyboard`) → `_textInput.show()`;
`touch-shortcuts` (`AppIcons.shortcuts`, `viewerShortcuts`) →
`showViewerSidePanel(ViewerShortcutsPanel)`; `touch-right-click`
(`AppIcons.rightClick`, `touchRightClick`) → `input.click(RemoteButton.right, at: cursor)`
(wrapped by latches, §3.4); `touch-more` (`AppIcons.more`, `more`) →
`showViewerSidePanel(ViewerMorePanel)`. Each = 44 circle `white@0.1` + label
10.5; keyboard, shortcuts and right-click at 40 % opacity and inert while
`!input.ready`.

Hit testing: controls are opaque; the empty gutter area is not, so a
trackpad stroke can start anywhere (RustDesk: "slide anywhere", MobileGestures:30).

#### 2.4.3 Side panels (`side_panel.dart`)

`showViewerSidePanel(context, child)` = `showGeneralDialog(barrierDismissible: true, barrierColor: Color(0x52000000), transitionDuration: Motion.medium, …)`
sliding a 300-wide full-height `ViewerPalette.panel` sheet in from the
trailing edge (keeps immersive mode; Android back closes it first).

- `ViewerShortcutsPanel`: the `kRemoteShortcuts` list (§2.3.4).
- `ViewerMorePanel` (spec §2.10 proposal): `gestureTitle` → `GestureSheet`;
  quality preference (three radios, §2.3.4); `viewerQuality` switch; mode
  `GlassSegmented` view/control (as the toolbar); `virtualMouse` switch (only in
  direct mode); `shortcutRefresh`; `shortcutReleaseKeys`; `viewerDisconnect` in
  `ViewerPalette.danger`.

#### 2.4.4 Keyboard and modifier bar (MobileKeyboard)

- `RemoteTextInput` (§3.5.2) attaches a `TextInputConnection` with
  `keyboardAppearance: Brightness.dark`; `show()`/`hide()`; reports composing
  text to `ComposingPreview` (a `ViewerPalette.hint` pill above the bar
  showing the uncommitted IME text, so CJK input is visible before it is sent).
- While the keyboard is open the gutters are hidden (`MobileKeyboard:16-28`)
  and `viewport.setOcclusion(EdgeInsets.only(bottom: inset + 44))` (§3.6).
- `ModifierBar(Key('modifier-bar'))`, 44 tall, padding 0/8, gap 6, fill
  `ViewerPalette.card`, top 1 px edge, mono 13, horizontally scrollable when
  it overflows; `Semantics(label: touchModifierBar)`. Caps (`KeyCap`, 32
  tall, r8, padding 0/10, fill `white@0.1`, text `#EDEDEF`):

  | Group | Caps (keys) | Behaviour |
  | --- | --- | --- |
  | modifiers | `Ctrl` (min 46) `Alt` `Shift` `Win` (min 42) — `modifier-control/alt/shift/meta` | latch (§3.4). Armed: fill `#EDEDEF`, text `#1D1D1F` 600. Locked: same + 2 px `#7DB6F2` underline bar |
  | sep | | |
  | keys | `Esc` `Tab` (min 40) | `input.tapKey(escape|tab)` |
  | | `Fn` (min 40, `modifier-fn`) | toggle: replaces this group and the next two with `F1`…`F12` |
  | sep | | |
  | arrows | `←` `↑` `↓` `→` (36 wide; a11y `keyArrow(dir)`) | tap = `tapKey`; hold = repeat (§3.4) |
  | sep | | |
  | edit | `Del` `Ctrl+C` `Ctrl+V` | `tapKey(delete)`; combos `control+c` / `control+v` (latches untouched) |
  | spacer | | |
  | collapse | 40×32 `AppIcons.chevronDown` 18, `Key('modifier-collapse')`, a11y `touchHideKeyboard` | `_textInput.hide()` |

- `ModifierStatusPill` (`MobileKeyboard:24-27`): 30 tall r15
  `ViewerPalette.hint`, dot 7 `#7DB6F2`, text `modifierLocked(locked)` and/or
  `modifierArmed(armed)` joined with " · ". Visible whenever any latch is not
  off, keyboard open or not.

#### 2.4.5 Minimap (`minimap.dart`, `MobileKeyboard:21-23`)

`Minimap(Key('minimap'))`: 96 × 96/aspect, r6, fill `rgba(20,20,22,0.8)`,
1 px `white@0.25`; `MinimapPainter` draws `viewport.visibleRegion` (normalised)
as a 1.5 px `#7DB6F2` r2 rectangle clamped to the box. No picture inside (no
second `Texture`). `IgnorePointer`. `ValueListenableBuilder(viewport.visibleRegion)`;
null when the whole picture is visible. A11y `minimapA11y(region)` with
`region` from the rect centre on a 3×3 grid (`topLeft` … `bottomRight`,
`center`).

#### 2.4.6 Virtual mouse (`virtual_mouse.dart`, D18 direct touch)

`VirtualMousePad(Key('virtual-mouse'))`, 72×120, `ViewerPalette.chrome`, r16,
1 px edge, initial position bottom-right inside the picture, above the trail
gutter: top row `L | R` (2 × 36×48) → `input.click(left|right, at: cursor)`;
scroll strip 72×44 (`AppIcons.chevronUp`/`chevronDown` 14 hints): vertical
drag → wheel, 1 notch per 16 px, finger down = wheel up; grip 72×28 drag →
moves the pad (clamped to the stage; position kept per session). Opaque, so a
press on it is never also a remote click. Enabled by `prefs.virtualMouse`,
shown only in direct mode.

#### 2.4.7 Gesture sheet (`gesture_sheet.dart`, MobileGestures)

In the viewer: centred panel, max width 390, scrollable, `ViewerPalette.card`
(spec §2.12 landscape proposal). Title `gestureTitle` + close;
`GlassSegmented<TouchMode>(key: Key('gesture-mode'), height: 38)` →
`prefs.setTouchMode`; description `gestureTrackpadDescription` /
`gestureDirectDescription`; rows (54 tall, `GestureGlyph` 40×40 tile with a
24-px `GestureGlyphPainter`, stroke 1.5, geometry from spec §2.12):

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

## 3. Interaction specifications

### 3.1 Constants (one place each)

| Constant | Value | File | Source |
| --- | --- | --- | --- |
| `kTouchSlop` | 18 px | Flutter | rd §0 |
| `kDoubleTapTimeout` | 300 ms (first up → second down) | Flutter | rd §3.9 |
| `kDoubleTapMinTime` | 40 ms | Flutter | rd §3.9 |
| `kDoubleTapSlop` | 100 px (first down → second down) | Flutter | rd §3.9 |
| `kLongPressTimeout` | 500 ms | Flutter | rd §3.9 |
| `kHoldToDrag` | 300 ms (second touch held) | `touch_gestures.dart` | rd `_startSecondTapDownTimer` |
| `kRemoteDoubleClickGuard` | 520 ms after the first click's down | `touch_gestures.dart` | I4 (Windows default 500 ms + 20) |
| `kPointerJoinGrace` | 80 ms | `touch_gestures.dart` | ours; RustDesk debounces class changes by 200 ms (rd §3.9) |
| `kThreeFingerNotchPx` | 24 px mean finger travel per notch | `touch_gestures.dart` | I9 |
| `kVirtualWheelNotchPx` | 16 px | `virtual_mouse.dart` | |
| `kWheelPxPerNotch` | 30 px of `scrollDelta` (fractions accumulated) | `remote_surface.dart` | today `desktop_viewer.dart:456-460` |
| `kWheelClamp` | ±100 per event | `remote_input.dart` | Rust `lib.rs:386-391` |
| `kMinScale` / `kMaxScale` | 1 (fit) / 8 | `remote_viewport.dart` | today; min stays fit (rd §7 P1-4) |
| `kFollowMargin` | 15 % of the visible rect | `remote_viewport.dart` | rd §3.13 camera follow |
| `kKeyboardSettle` | 100 ms | `touch_viewer.dart` | rd §3.12 |
| `kCursorEchoWindow` | 300 ms after a local move | `remote_surface.dart` | D11 (host stops echoing to the mover for 300 ms) |
| `kTouchIndicatorFade` | 150 ms | `remote_surface.dart` | spec §2.10 |
| `kToolbarCollapseDelay` | 2 s | `viewer_toolbar.dart` | BRIEF |
| `kHandleHoverExpand` | 300 ms | `viewer_toolbar.dart` | I7 |
| `kHintDuration` | 3 s | `viewer_overlays.dart`, `touch_controls.dart` | spec §2.6, §2.10 |
| `kKeyRepeatDelay` / `kKeyRepeatInterval` | 400 ms / 60 ms | `modifier_bar.dart` | |
| `kTextChunkBytes` | 1024 UTF-8 bytes | `remote_input.dart` | Rust `Input::Text` bound |

### 3.2 Touch gesture state machine (`TouchGestureMachine`)

```dart
enum TouchMode { trackpad, direct }
class TouchGestureMachine {
  TouchGestureMachine({required RemoteViewport viewport, required RemotePointerSink sink,
      required TouchMode Function() mode, required bool Function() interactive,
      required ValueChanged<Offset?> indicator});
  void down(PointerDownEvent e); void move(PointerMoveEvent e); void up(PointerUpEvent e);
  void cancel();   // pointer cancel, input epoch / mode / readiness change, focus loss
  void dispose();
}
```

`RemoteSurface` feeds it touch, stylus and inverted-stylus pointers from one
`Listener(behavior: HitTestBehavior.opaque)`; mouse and touchpad go to §3.3.
Stylus always uses direct semantics. A **sequence** runs from the first finger
down to the last finger up. Its class only escalates — 1 finger → 2 (navigate)
→ 3+ (scroll) — and never falls back while any finger is still down, so lifting
one pinch finger never becomes a cursor stroke or a drag (today's
`_maxTouches` idea, `desktop_gesture_surface.dart:179, 221`, made explicit).

Memory across sequences (trackpad only): `lastTap = (upTime, downPos, clickDownSentAt)`,
cleared after a second tap, on navigation and on `cancel()`.

**Trackpad mode** (default, D18). The click point is always the local cursor
prediction `input.cursor` (normalised), never the finger.

| State | Event | Guard | Actions | Next |
| --- | --- | --- | --- | --- |
| idle | down p | `lastTap` set, 40 ≤ `now − upTime` ≤ 300, `dist(p, lastTap.downPos)` ≤ 100 | indicator(p); timer `kHoldToDrag` | secondDown |
| idle | down p | otherwise | indicator(p); timer `kLongPressTimeout` | firstDown |
| firstDown | move | moved > 18 | cancel timer; `cursor += delta / (scene * scale)`; `sink.hover(cursor)`; `viewport.ensureVisible(cursor)` | stroke |
| firstDown | timer | — | haptic medium; `sink.click(right, cursor)` | rightClicked |
| firstDown | up | — | `sink.click(left, cursor)` **now** (D18); `lastTap = (now, p0, clickDownAt)` | idle |
| secondDown | move | moved > 18 | cancel timer; `sink.press(left, cursor)` **deferred** to `clickDownAt + 520 ms` (I4); deltas move the local cursor | dragging |
| secondDown | timer | — | haptic selection; `sink.press(left, cursor)` deferred as above | dragging |
| secondDown | up | — | `sink.click(left, cursor)` now (the remote sees a double-click); `lastTap = null` | idle |
| stroke | move | — | relative move + follow | stroke |
| rightClicked | move | moved > 18 | relative move + follow (hold, then slide through the context menu) | stroke |
| stroke, rightClicked | up | — | — | idle |
| dragging | move | — | relative move + follow; while the press is deferred only the local cursor moves | dragging |
| dragging | up | — | flush a pending press (`press`, `move`), then `sink.release(left)` | idle |
| any 1-finger state | 2nd down | — | cancel timers; if dragging `sink.release(left)`; `lastTap = null`; indicator(null) | navigate |
| navigate | move | 80 ms since the 2nd down, or a finger moved > 18 | pinch/pan from the positions at the 2nd down: `scale = clamp(s0 * d / d0, 1, 8)`, `offset = mid − anchor * scale` (bounded) | navigate |
| navigate | 3rd down | — | freeze navigation; `sink.wheelStart()` | scroll |
| scroll | move | — | `acc += dy / pointers`; `n = trunc(acc / 24)`; if `n != 0`: `sink.wheel(0, −n)` (clamped ±100), `acc −= 24 n` | scroll |
| navigate, scroll | up | fingers remain | — (no de-escalation) | same |
| any | last up | — | `sink.wheelEnd()` if scroll; indicator fades over 150 ms | idle |
| any | cancel | — | timers off; release a held or pending drag; `wheelEnd()`; clear | idle |

Notes:

- The I4 guard only delays a *press*. If the finger lifts before the guard
  expires, the drag is flushed at the guard time as one ordered sequence
  (`move(start)`, `left down`, `move(end)`, `left up`).
- Finger down means "pull the content down", i.e. wheel up: negative
  `vertical` (pb-mapper's positive is "scroll down",
  `desktop_viewer.dart:362-378`; RustDesk sends finger-down as wheel up, rd §3.9).
- The touch indicator is a 44-px circle (`white@0.22`, 1.5 px `white@0.8`) at
  the finger in an `IgnorePointer` layer, trackpad mode only.

**Direct-touch mode.** The finger is the pointer.

| State | Event | Guard | Actions | Next |
| --- | --- | --- | --- | --- |
| idle | down p | p outside the picture | — | ignored (until all fingers are up) |
| idle | down p | — | timer `kLongPressTimeout` | firstDown |
| firstDown | move | moved > 18 and ≥ 80 ms since down | cancel timer; `sink.press(left, p0)`; `sink.move(p)` | dragging |
| firstDown | move | moved > 18, < 80 ms since down | wait for the grace timer; then as above if still one finger | firstDown |
| firstDown | timer | — | haptic medium; `sink.click(right, p0)` | consumed |
| firstDown | up | — | `sink.click(left, p0)` now | idle |
| dragging | move | — | `sink.move(clamp(p))` | dragging |
| dragging | up | — | `sink.release(left)` | idle |
| consumed | move / up | — | — | consumed / idle |
| any 1-finger state | 2nd down | — | as trackpad (release a drag) | navigate |
| navigate, scroll | | | as trackpad | |

A double tap in direct mode is two immediate clicks (a remote double-click).
A two-finger tap does nothing in either mode (D18; the release note says so).

**Not interactive** (`!input.ready`: view only, stale picture, control not yet
granted): one finger pans the picture locally when zoomed, two fingers pinch,
three fingers do nothing; nothing reaches the sink — today's rule
(`desktop_gesture_surface.dart:302`).

### 3.3 Mouse and touchpad (desktop; also a mouse on Android or an iPad)

| Input | Interactive | Not interactive |
| --- | --- | --- |
| Hover inside the picture | `sink.hover(p)`; original-size mode also `viewport.autoScrollTo(local)` | auto-scroll only |
| Button down inside the picture | `onFocusRequest()`; `sink.press(left/right/middle, p)` from `event.buttons` | focus only |
| Move with a button held (also outside the picture) | `sink.move(clamp(p))` | — |
| Button up | `sink.release(b)` | — |
| `PointerScrollEvent` (any modifiers) | `sink.hover(p)`; `accY += dy / 30`, `accX += dx / 30` → `sink.wheel(nx, ny)` for whole notches | Ctrl + wheel → `viewport.zoomAt(p, exp(−dy / 250))` |
| Touchpad `PointerPanZoomUpdate`, pan | `accY += −panDelta.dy / 30` → wheel (natural scrolling, like Flutter's own scrollables) (I10) | `viewport.panBy(panDelta)` |
| Touchpad pan-zoom, scale ≠ 1 | `viewport.zoomAt(focal, scaleDelta)` (local) | same |
| Touch on a Windows touchscreen | `TouchGestureMachine`, direct mode | — |

Behaviour change: Ctrl + wheel reaches the remote while controlling (zoom in
remote apps, RustDesk parity); local zoom on desktop is the Display menu (fit /
original) and touchpad pinch. Ctrl already went to the host as a real key down
through the keyboard path.

`ViewerScaleMode.original`: `scale = max(1, remoteWidthPx / (scene.width * dpr))`
(one remote pixel per physical pixel); the view follows the pointer,
`offset = −(pointer / visible) * (scene * scale − visible)` per axis (RustDesk
"scroll auto"). Local only.

Painted remote cursor (`RemoteCursorLayer`): from `desktop.view`
(`cursorX/Y ÷ displayWidth/Height`, `cursorVisible`), drawn by
`RemoteCursorPainter` (today's `DesktopCursorPainter`, 18×27, constant size)
and suppressed for 300 ms after any local move (the host stops echoing for
300 ms too, D11). While a trackpad stroke runs, the layer draws `input.cursor`,
the local prediction. The local OS pointer stays visible on desktop.

### 3.4 Modifier latches (D16) and how input leaves the viewer

```dart
enum LatchState { off, armed, locked }
final class ModifierLatch extends ChangeNotifier {
  static const order = [HostKey.control, HostKey.alt, HostKey.shift, HostKey.meta];
  LatchState stateOf(HostKey modifier);
  Set<HostKey> get active;          // armed and locked, in [order]
  void tap(HostKey modifier);       // off -> armed -> locked -> off
  Set<HostKey> take();              // returns active; armed -> off; locked stays
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
| any | `clear()` (§3.9 triggers) | off |

"Action" = a key from the modifier bar or the soft keyboard, a click (tap
gesture, right-click button, virtual mouse), a drag (at its press), a
three-finger scroll gesture (at its start), a virtual-mouse wheel burst.
Pointer moves never consume. Each latch change plays
`HapticFeedback.selectionClick()`.

`RemoteInput` (`remote_input.dart`) is the only door to the store:

```dart
enum RemoteButton { left, right, middle }
abstract interface class RemotePointerSink {
  void hover(Offset p); void move(Offset p);
  void press(RemoteButton b, Offset p); void release(RemoteButton b);
  void click(RemoteButton b, Offset p);
  void wheelStart(); void wheel(int horizontal, int vertical); void wheelEnd();
}
final class RemoteInput implements RemotePointerSink {
  RemoteInput({required DesktopStore desktop, required ModifierLatch latch});
  ValueListenable<bool> get ready;     // desktop.fresh && desktop.controlling (§4.2)
  Offset cursor;                       // local cursor prediction, normalised
  KeyEventResult handleHardwareKey(FocusNode node, KeyEvent event);   // §3.5.1
  void tapKey(HostKey key, {Set<HostKey> extra = const {}});         // latched + extra modifiers
  void pressKey(HostKey key); void releaseKey();                     // modifier-bar repeat
  void combo(RemoteShortcut shortcut);                               // menus; latches untouched
  void typeText(String text); void backspace(int count);             // §3.5.2
  Future<void> releaseAll();                                         // §3.9
}
```

Rules:

- Every method is a no-op unless `ready` (except `releaseAll`).
- Normalised points become display pixels with the current
  `view.displayWidth/Height` (floor, clamped to `[0, w − 1]`, as
  `desktop_viewer.dart:209-219` does). Generation, frame id and sequence are
  the store's (`send` reads `view`).
- `click`, `tapKey`, `typeText` (per key), `combo` and `backspace` each become
  **one** `desktop.sendSequence([...])`:
  `[m down for m in mods] + action + [m up, reversed]`. `sendSequence` is
  atomic (no move interleaves) and all-or-release: a failure after the first
  event sends `release` and ends control. Nothing can stay held on the host
  (D16).
- `press` sends `[m down..., button down]` and remembers
  `_heldMods`/`_heldButtons`; `release` sends `[button up, m up...]`;
  `wheelStart` sends `m down...`, `wheelEnd` `m up...`; `pressKey` sends
  `[m down..., key down]`, repeats `key down` every 60 ms after 400 ms, and
  `releaseKey` sends `[key up, m up...]`.
- `hover` and `move` go through `desktop.send` (the store coalesces moves).
- `releaseAll`: `surface.cancel()`; `latch.clear()`; if anything is held (keys
  from the hardware path, buttons, `_heldMods`) → `desktop.release()`.

### 3.5 Keyboards

`HostKey` (`host_keys.dart`) mirrors the Rust `Key` enum one to one (D17):
`shift, control, alt, meta, enter, escape, tab, backspace, space, insert,
delete, left, right, up, down, home, end, page_up, page_down, f1…f12, a…z,
digit0…digit9`; `.wire` is the snake_case name. Lock is a command
(`desktop.lock()`), not a key; Ctrl+Alt+Del is unrepresentable.

#### 3.5.1 Hardware keyboard (`RemoteInput.handleHardwareKey`)

Bound to the picture's `Focus.onKeyEvent` in both viewers (a Bluetooth keyboard
on a phone takes the same path). If `!ready` → `ignored`. Otherwise, the first
rule that matches:

1. **Special key** (`hostKeyForHardware(event.logicalKey)` returns one of
   Shift/Control/Alt/Meta left or right, F1–F12, Enter/NumpadEnter, Escape, Tab,
   Backspace, Delete, Insert, Home, End, PageUp, PageDown, arrows) →
   `key{down: event is! KeyUpEvent}` (repeats are downs), tracked in
   `_heldKeys` → `handled`. Tab never moves Flutter focus while the picture is
   focused.
2. **Letter, digit or space while Ctrl, Alt or Meta is physically down**
   (`HardwareKeyboard.instance.isControlPressed || isAltPressed || isMetaPressed`)
   **and not produced by AltGr** (`event.character` is null or equals the key's
   base character ignoring case) → `key{a…z | digit0…9 | space}` down/up,
   tracked → `handled` (I11: Ctrl+B, Ctrl+S, Alt+F, Win+E work).
3. **Printable character** on down or repeat (`event.character`, no C0/C1
   control characters) → `text{character}` → `handled`. Shift went as a real
   key under rule 1; the character already carries case and layout.
4. Otherwise `ignored`.

Known limitation (as today): characters typed this way are injected as
Unicode, so a remote input method does not compose them; CJK text goes through
`viewerTypeText` (desktop) or the soft keyboard (mobile).

#### 3.5.2 Soft keyboard (`RemoteTextInput`, mobile)

`RemoteTextInput implements DeltaTextInputClient`, attached only while the
keyboard is wanted:

- `TextInputConfiguration(inputType: TextInputType.multiline, inputAction: TextInputAction.newline, enableDeltaModel: true, autocorrect: false, enableSuggestions: false, enableIMEPersonalizedLearning: false, keyboardAppearance: Brightness.dark, smartDashesType: SmartDashesType.disabled, smartQuotesType: SmartQuotesType.disabled)`.
- Baseline editing state: `sentinel = '1' * 64`, caret at the end (RustDesk's
  pre-filled buffer, rd §3.12: Backspace always produces a deletion delta;
  digits draw no autocorrection).
- `updateEditingValueWithDeltas`:
  - an insertion with a collapsed `composing` range → `input.typeText(text)`
    (`'\n'` → `tapKey(enter)`);
  - a deletion before the caret → `input.backspace(codePoints)` (at most 64
    per call);
  - a replacement → `backspace(old)` then `typeText(new)` (committed text only);
  - a non-text update → ignored;
  - while `composing` is not empty nothing is sent; the uncommitted text is
    mirrored to `ComposingPreview`; on commit the committed segment is sent;
  - when not composing and the buffer drifted (`< 32` or `> 512` characters)
    → `setEditingState(sentinel)`.
- `performAction(TextInputAction.newline)` → `tapKey(enter)` (IMEs that send
  the action instead of `'\n'`). `connectionClosed()` (system back hid the IME)
  ends keyboard mode.
- `RemoteInput.typeText(s)`: while any latch is active, each character with
  `hostKeyForChar(c) = (key, shift)` (`a–z`; `A–Z` → letter + shift; `0–9`;
  space; `'\n'` → enter) goes as `tapKey(key, extra: shift ? {shift} : {})`; a
  character without a key goes as `text` and still consumes armed latches (they
  cannot apply and must not linger). With no latch active, text goes in
  ≤ 1024-byte `text` events.

### 3.6 Viewport and soft keyboard (P1-4)

```dart
enum ViewerScaleMode { fit, original }
class RemoteViewport extends ChangeNotifier {
  Size get layoutSize;          // from RemoteSurface's LayoutBuilder; ignores the IME
  EdgeInsets get occlusion;     // IME + modifier bar on mobile; zero on desktop
  double aspectRatio;           // picture video width / height
  Size get scene;               // fit(layoutSize, aspectRatio); independent of occlusion
  double get scale; Offset get offset;
  Rect get visibleRect;         // (Offset.zero & layoutSize) minus occlusion
  ValueListenable<Rect?> get visibleRegion;   // normalised; null when everything is visible
  Offset? toNormalized(Offset local, {bool clamp = false});
  Offset toLocal(Offset normalized);
  void layout(Size size);       // rotation/resize: keep scale and the point at the visible centre
  void setOcclusion(EdgeInsets occlusion);    // re-bounds the offset only
  void zoomAt(Offset focal, double factor); void panBy(Offset delta);
  void ensureVisible(Offset normalized, {double margin = kFollowMargin});   // minimal pan
  void beginKeyboard(); void endKeyboard();
  void setMode(ViewerScaleMode mode, {required Size remotePixels, required double dpr});
  void autoScrollTo(Offset pointer);
}
```

Bounding per axis: with `extent = scene * scale` and the visible span
`[v0, v1]`, the offset centres the picture in the span when
`extent <= v1 − v0`, else it is clamped to `[v1 − extent, v0]`. With the
keyboard up on an 844×390 phone the visible height is 390 − IME − 44, so the
fitted 390-tall picture becomes pannable vertically without any change of
scale.

Keyboard sequence (`TouchViewer` watches `MediaQuery.viewInsetsOf(context).bottom`):

1. inset becomes > 0: `beginKeyboard()` saves `(scale, offset)` and clears
   `_navigatedDuringKeyboard`;
2. every inset change: `setOcclusion(EdgeInsets.only(bottom: inset + 44))` —
   re-bound only; `scene` and `layoutSize` stay, so the picture does not
   relayout;
3. 100 ms after the last change: `ensureVisible(input.cursor)` (minimal pan);
4. inset returns to 0: `endKeyboard()` restores the saved `(scale, offset)`
   exactly, unless the user pinched or panned meanwhile (then it keeps the
   current scale and re-bounds).

Nothing here calls the store, changes encode parameters or rebuilds the
texture (P1-4). Trackpad strokes call `ensureVisible(cursor)` so the view
follows the cursor when zoomed (rd §3.13).

### 3.7 Toolbar visibility (`ToolbarController`)

```dart
class ToolbarController extends ChangeNotifier {
  ToolbarController({required bool pinned});
  bool get pinned; bool get expanded;
  void start();                       // viewer shown
  void setPinned(bool value);         // the caller also persists it (ViewerPrefs)
  void pointerEnter(); void pointerExit();   // region = toolbar + its 12-px top margin
  void touchEnded();                  // pointer up on the toolbar from touch or stylus (no hover)
  void handleEnter(); void handleExit(); void handleTap();
  void hold(); void unhold();         // a menu, the quality panel or a dialog is open
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

Pinned defaults to true, so a first-time user sees the toolbar. Auto-collapse
never writes the preference (RustDesk keeps manual and automatic collapse
apart, rd §3.1 step 7). No timer outlives `dispose`.

### 3.8 Connection state → overlay

```dart
enum ViewerOverlayKind { none, connecting, connectFailed, identityChanged, ended,
    reconnectWaiting, reconnecting, secureDesktop, waitingDesktop, recovering }
ViewerOverlayKind overlayFor(DesktopViewerState state);   // pure; first match wins
```

| # | Condition (`DesktopViewerState`, §4.2) | Overlay (spec) | Ground and hit testing | Actions |
| --- | --- | --- | --- | --- |
| 1 | `phase == identityChanged` | `IdentityCard` (§2.7.5) | opaque ground (the session is dead); absorbs | `viewerDisconnect` → `remote.disconnect()`; `viewerPasteNewInvite` → `showAddComputer(replacing: computer)` |
| 2 | `phase == ended` | `EndedCard` (proposal): `viewerEndedTitle` + reason (`viewerEndedStopped`, `viewerEndedByHost`, `viewerEndedVersion`, `viewerBusy`, `viewerDenied`, or the message) | opaque; absorbs | `back` → `remote.disconnect()` (the session is over: record it, close the viewer); `viewerReconnect` → `remote.reconnect()` (hidden for a version mismatch) |
| 3 | `phase == connecting && stage?.failureCode != null` | `ConnectingCard`, failed variant (stops at the failed step with its reason) | opaque; absorbs | `back` → `remote.disconnect()`; `retry` → `remote.reconnect()` |
| 4 | `phase == connecting` | `ConnectingCard` (§2.7.1): `viewerConnecting(name)`; steps `viewerStepFoundDirect`/`Relay`, `viewerStepIdentity`, `viewerStepFirstFrame` from `stage.completed`; without `stage` only the spinner and the title | opaque (no picture exists yet); absorbs | `cancel` → `remote.disconnect()` |
| 5 | `phase == retryWaiting` | `ReconnectPill`: `viewerRetry(attempt, seconds)` + `viewerInputPaused` + `viewerRetryNow`; `ReconnectStill` | still = `BackdropFilter(filter: ColorFilter.matrix(grey × 0.5))` clipped to the picture rect, a **sibling** above the picture; the pill absorbs, the rest passes (local zoom/pan stays) | `viewerRetryNow` → `desktop.retryNow()` |
| 6 | `phase == reconnecting` | `ReconnectPill`: `viewerReconnecting(attempt)`, no button; `ReconnectStill` | as 5 | — |
| 7 | `phase == waitingForDesktop && reason == secure_desktop` | `SecureDesktopCard` (§2.7.3): `AppIcons.secureDesktop`, `viewerSecureTitle`, `viewerSecureBody`, chip `viewerInputPaused` | `ColoredBox(Color(0xB80F0F10))` scrim in `IgnorePointer`; the card absorbs | — |
| 8 | `phase == waitingForDesktop` | same card with `viewerWaitingTitle` / `viewerWaitingBody` | as 7 | — |
| 9 | `phase == recovering` | `RecoveringPill`: `viewerRecovering` | none; `IgnorePointer` | — |
| 10 | otherwise | none | | |

Independent of the table: `ViewOnlyPill` whenever `!controlAllowed` (live,
recovering, waiting); `DegradedPill` when the toolbar is collapsed, the phase
is live and `stats.degraded`; `ControlHint` per §2.3.6. Today's phases map as
`identityMismatch → identityChanged`, `delayed → live` plus `degraded`,
`recoveringVideo → recovering`, `idle → ended`.

Invariant kept from `desktop_viewer.dart:492-493`: while a live picture can
still arrive (rows 5–9) nothing opaque covers the texture, and no overlay ever
becomes the picture's parent; the picture element stays identical across every
phase change (tested, §8).

### 3.9 Focus, release and visibility

| Event | `input.releaseAll()` | Latches | Viewer attached (stream) | Control grant |
| --- | --- | --- | --- | --- |
| Picture loses focus (menu, dialog, side panel) | yes (`release` is sent only if something is held) | cleared | unchanged | unchanged |
| Window blur / `AppLifecycleState.inactive` | yes | cleared | unchanged (I2) | unchanged |
| Minimised, hidden to the tray, `hidden` or `paused` | yes | cleared | the store pauses (perf design §6.4) | the store ends it and keeps "wanted" |
| Back to the list (`remote.back()`) | yes | cleared | `detachViewer` (pause, D13) | ends, keeps "wanted" |
| Back to the picture (`viewer.open()`) | — | — | `attachViewer` (resume + keyframe) | re-requested once fresh |
| Media epoch or generation change (reconnect) | `surface.cancel()` | cleared | — | — |
| `input.ready` true → false | `surface.cancel()` | cleared | — | — |
| Trackpad ↔ direct switch | `surface.cancel()` | kept | — | — |
| Pointer cancel (system edge gesture) | `surface.cancel()` | kept | — | — |
| Disconnect or viewer disposed | yes | cleared | store teardown | — |

Focus returns to the picture after every menu (`onClose`), dialog (`then`),
side panel and sheet (`whenComplete`). A pointer down on the picture requests
focus (today's `onFocus`). The viewers use an
`AppLifecycleListener(onInactive: …, onHide: …)` for local input only;
on-screen state belongs to `ViewerHost`; the app-hidden pause belongs to the
store.

### 3.10 Accessibility and motion

- Picture: `Semantics(container: true, label: viewerPictureLabel(name), hint: touchTrackpadHint or touchDirectHint)`.
- Overlay cards and pills: `Semantics(liveRegion: true)`. Toolbar and gutter
  controls carry tooltips/labels; the mode control reports its selection;
  `KeyCap` reports `toggled` with the value `modifierStateArmed` or
  `modifierStateLocked`; the minimap reports `minimapA11y(region)`.
- Reduced motion (`Motion.reduced`): toolbar slide/fade, the viewer's entrance
  fade, the scan line, pulsing dots and the touch-indicator fade are instant
  or static.
- The dark chrome clamps text scaling at 1.3 (§1.5); management pages scale
  normally.
- Haptics: long-press right click `mediumImpact`; drag start and latch changes
  `selectionClick`.
- Known limitation: while the picture has focus, Tab belongs to the remote, so
  keyboard users reach the toolbar only after focus leaves the picture
  (RustDesk behaves the same).

---

## 4. State

### 4.1 Notification scoping (D14)

`AppStore` stops re-broadcasting the desktop store (`app_store.dart:40-42`
`..addListener(_notify)` deleted). No widget listens to the `DesktopStore`
notifier for media-rate data; each consumer listens to the narrowest source:

| Consumer | Listens to | Rate |
| --- | --- | --- |
| Rail/bar dot, `SharingIndicator`, tray | `desktop.sharing` | on change |
| `HostCard` | `desktop.hostChanges` | on change (host poll ≤ 1 Hz while retained or hosting) |
| Remote list, featured card, rows | `computers`, `desktop.session`, `AppStore` (relay state, `desktopServiceNames`) | on change; relay ≈ 12 s |
| Viewing row meta, toolbar status chip, quality panel, gutter quality | `desktop.stats` | ≤ 1 Hz |
| `DesktopViewer` / `TouchViewer` `build` | nothing (fullscreen flag and title only) | — |
| Picture (`Texture` / `JpegPicture`) | `desktop.picture` | per texture or size change |
| Overlay layer, mode control, `ViewOnlyPill`, `ControlHint` | `desktop.viewerState` | per transition |
| Cursor layer; `input.ready` | `desktop.view`; `desktop.fresh` + `desktop.controlling` | per frame while attached / on change |
| Minimap | `viewport.visibleRegion` | on zoom/pan |
| Toolbar | `ToolbarController` | on expand/collapse |
| Modifier bar, status pill | `ModifierLatch` | per tap |
| `_PbMapperAppState` | `ViewerHost` (and `AppPrefs` as today) | open/close |

Polling: the Remote page holds `desktop.retain(DesktopInterest.remotePage)`
while mounted; the viewer attaches while on screen; hosting keeps the host
poll. With none of these there is no timer (today's 250 ms timer never stops,
`desktop_store.dart:149, 888`).

### 4.2 `DesktopStore` — the members the UI needs

The pipeline package owns the implementation. This is the UI's side of the
contract; the last two columns give the equivalent names in the two
performance designs (`perf-rustdesk.md` §6.3, `perf-minimal.md` §6.2) so the
lead can pick one spelling.

| Member | Type / semantics | perf-rustdesk | perf-minimal |
| --- | --- | --- | --- |
| `capabilities`, `trust`, `snapshot.background`, `invitation`, `restarting` | as today; `capabilities.helper.updateRequired` (D2) is new | `protocol_current` | — |
| `sharing` | `ValueListenable<DesktopSharing>`: `bool hosting`, `HostViewer viewer` (none/watching/controlling), `String? path`, `Duration? sessionAge` (D24) | `sharing` | `snapshot` (host) |
| `hostChanges` | `Listenable`, fires when any host-card fact changes | store notifier | store notifier |
| `hostAction` | `HostAction?` {enabling, stopping, revoking, helper, display} — replaces the page's use of the shared `busy` | — | — |
| `hostError` | `String?` (host-side failures only; the viewer has its own) | `error` | `error` |
| `enableHost({required String service, required int displayId, required bool allowControl, bool helper = true})` | D24: the permission is chosen at enable, never briefly granted | (changed) | (changed) |
| `resumeHost()`, `stopHost()`, `allowControl(bool)`, `revokeTrust()`, `helperAction(DesktopHelperAction)` | as today (`restoreHost`/background start unified as `resumeHost`); stop/revoke/allow(false) bypass `busy` | unchanged | unchanged |
| `disconnectViewer()` | **new** (D15): host closes the current viewer with reason `disconnected_by_host`; sharing continues | — | — |
| `setHostDisplay(int id)`, `renameHostService(String)`, `refreshHost()` | **new**: display switch (restart host if no live switch), rename (new invitation), user refresh | — | — |
| `retain(DesktopInterest.remotePage)` | **new**: `DesktopInterestLease` with `release()`; host poll ≤ 1 Hz while held | "Remote page visible" | "Remote page visible" |
| `session` | `ValueListenable<DesktopSession?>`: `service`, `computerId`, `name` | — | `connected` |
| `viewerState` | `ValueListenable<DesktopViewerState>`: `phase` (`connecting, live, waitingForDesktop, recovering, retryWaiting, reconnecting, identityChanged, ended`), `reason` (`secure_desktop`, `display_lost`, …), `controlAllowed`, `controlling`, `controlWanted`, `attempt`, `retryIn`, `stage` (`DesktopConnectStage{completed 0..3, path, failureCode, failureMessage}`, nullable), `end` (`DesktopEnd{hostStopped, disconnectedByHost, versionMismatch, busy, denied, failed}` + message). `ended` never auto-retries | `connectionPhase` + `reason` | `connectionPhase` |
| `picture` | `ValueListenable<DesktopPicture?>`: `int? textureId` (survives reconnect: re-bound, not re-created), `double aspectRatio`, `String codec` | `videoTexture` + `view.video*` | `videoTexture` + `geometry` |
| `jpegImage`, `presentedJpeg(...)` | JPEG fallback (non-Android/Windows): latest `ui.Image`; presentation acknowledged after paint | JPEG worker + `presentedJpeg` | `jpegImage` |
| `view` | `ValueListenable<DesktopView?>` per frame while attached: generation, presented frame id/age, display w/h, video w/h, cursor x/y/visible, controlling, controlAllowed (D11) | `view` | `readCursor()` + `geometry` |
| `fresh`, `controlling` | `ValueListenable<bool>`; `input.ready = fresh && controlling` | same | getters |
| `stats` | `ValueListenable<DesktopStats>` ≤ 1 Hz: `path`, `relayTransport`, `rttMs`, `pictureLatencyMs`, `presentedFps`, `receiveKbps`, `width`, `height`, `codec`, `hardwareDecoder`, `adaptation` (none/lowerBitrate/lowerFps), `degraded`, `paused` | `stats` | `stats` |
| `quality`, `setQuality(DesktopQuality)` | `enum DesktopQuality { quality, balanced, smooth }`; live, no reconnect (D12) | same | same |
| `connect(DesktopTarget)` | `DesktopTarget{invitation, relayOnly, quality, computerId, name}` | `connect(invite, …)` | `connect` |
| `disconnect()`, `control(bool)`, `release()`, `requestKeyframe()`, `lock()` | as named; `lock` = D17 host command | same | `lockRemote()` |
| `retryNow()` | **new**: cancel the retry timer and attempt now | — | — |
| `send(Map input)` | pointer moves and single events; the store reads `view` for generation/frame/sequence and coalesces moves | `send` | `send` |
| `sendSequence(List<Map> inputs)` | **new**: atomic, ordered, all-or-release (§3.4) | — | — |
| `attachViewer()`, `detachViewer()` | viewer on/off screen (D13 pause/resume); app-hidden pause is the store's own | same | `setViewerForeground` |
| `inspectInvitation(String)` | `Future<DesktopInvitationInfo{service, fingerprint, format}>`; errors `INVALID_ARGUMENT`, `SETUP_STRING` (D3) | same | same |
| `capturePicture(String path)` | **new**: `Future<bool>`, best effort, ≤ 400 ms, never blocks closing (D23) | `videoTexture.snapshot` inside `disconnect` | `texture.snapshot` |

Degradation thresholds belong to the performance design (perf-rustdesk:
`queue_delay_ms > 150 || picture_latency_ms > 250`); the UI only reads
`stats.degraded`.

### 4.3 Remembered computers (`state/remote_computers.dart`, D20)

```dart
class RemoteComputer {
  final String id;                    // random 128-bit hex
  final String name, service, fingerprint;   // fingerprint: short form for UI
  final bool relayOnly;
  final DateTime? lastConnectedAt; final String? lastPath; final bool? lastControlAllowed;
  final DesktopQuality quality;       // Display-menu choice, default balanced (D12)
  final bool hasThumbnail;
}
sealed class RememberSupport { available; unavailable(String reason) }
abstract interface class RemoteComputerVault {   // Windows: FFI over DPAPI; Android: Keystore channel; else unavailable
  RememberSupport get support;
  Future<String?> read(); Future<void> write(String json);
}
class RemoteComputerStore extends ChangeNotifier {
  RemoteComputerStore(this._vault, {required Directory thumbnailDir});
  RememberSupport get support;
  List<RemoteComputer> get computers;                // most recent first
  Future<void> load();
  RemoteComputer? sameIdentity(DesktopInvitationInfo info);
  RemoteComputer? sameServiceOtherIdentity(DesktopInvitationInfo info);
  Future<RemoteComputer> remember({required String invitation, required DesktopInvitationInfo info,
      required String name, required bool relayOnly, String? replaceId});
  Future<String> invitationOf(String id);
  Future<void> rename(String id, String name);
  Future<void> remove(String id);                    // also deletes the thumbnail
  Future<void> recordSession(String id, {String? path, bool? controlAllowed, required DateTime at});
  Future<void> setQuality(String id, DesktopQuality quality);
  String thumbnailPath(String id); File? thumbnailOf(String id);
}
```

At most 50 entries (mirrors `desktop_trust`'s bound). A vault failure turns
`support` into `unavailable(reason)`: Remember switches off with the reason;
nothing is ever written in plain text. Owned by `AppStore` (`computers`),
loaded in `start()`; tests inject an in-memory vault.

### 4.4 `RemoteDesktop` and `ViewerHost` (`state/remote_desktop.dart`)

```dart
class ViewerHost extends ChangeNotifier {
  ViewerHost(this._desktop);
  bool get showing;
  void open();     // idempotent; desktop.attachViewer()
  void close();    // idempotent; desktop.detachViewer()
}

class RemoteDesktop {
  RemoteDesktop({required this.desktop, required this.computers,
      required this.viewer, required this.prefs});
  final DesktopStore desktop; final RemoteComputerStore computers;
  final ViewerHost viewer; final ViewerPrefs prefs;

  Future<void> open(BuildContext context, RemoteComputer computer);
  // same computer active → viewer.open(); another active → confirm (remoteSwitch*) and
  // disconnect it; else connect(DesktopTarget(… computer.quality …)) without awaiting, viewer.open()
  Future<void> connectNew(AddComputerResult result);
  // connect + viewer.open(); a "remember" is pending until the first `live`
  // (the TLS pin succeeded), then computers.remember(...) — never before
  void back();                 // best-effort capturePicture (remembered, fresh) → viewer.close()
  Future<void> disconnect();   // capture → recordSession(path, controlAllowed) → desktop.disconnect() → viewer.close()
  Future<void> reconnect();    // `ended` → the same target
  Future<void> setQuality(DesktopQuality quality);   // desktop.setQuality + computers.setQuality
}
```

`RemoteDesktop` listens to `desktop.viewerState` for the pending remember and
never captures a thumbnail or touches the stored invitation after
`identityChanged` (only the explicit "paste new invitation" path replaces it).
One `RemoteDesktop` is owned by `_PbMapperAppState` and reached through
`AppScope.remote`; the acceptance harness does not need it.

### 4.5 `ViewerPrefs` (`state/viewer_prefs.dart`)

```dart
class ViewerPrefs extends ChangeNotifier {
  ViewerPrefs({this.persist = true});
  bool toolbarPinned = true;                 // 'viewer_toolbar_pinned'
  bool qualityPanel = false;                 // 'viewer_quality_panel'
  ViewerScaleMode scaleMode = ViewerScaleMode.fit;   // 'viewer_scale_mode'
  TouchMode touchMode = TouchMode.trackpad;  // 'viewer_touch_mode' (D18 default)
  bool virtualMouse = false;                 // 'viewer_virtual_mouse'
  bool gestureSheetSeen = false;             // 'viewer_gestures_seen'
  Future<void> load();                       // SharedPreferences; a failure costs only the choice
  Future<void> setToolbarPinned(bool v); …   // one setter per field
}
```

The quality preference is per computer (`RemoteComputer.quality`), as RustDesk
keeps it per peer (rd §4.4).

### 4.6 `AppStore` changes

- `late final DesktopStore desktop = DesktopStore(api.desktop, onRestartReady: onRestartReady);`
  (no listener); `activeDesktop` deleted (its only reader was the banner).
- `late final RemoteComputerStore computers` (vault from `api.desktop`), loaded
  in `start()`.
- `remoteServices` filters `isDesktop`; new `desktopServiceNames` (§1.9).

---

## 5. Icons (requirement 3)

### 5.1 `AppIcons` API (`lib/src/theme/app_icons.dart`)

```dart
/// Sizes the icon rules allow (Icons:125-127).
abstract final class AppIconSize {
  static const double inline = 16, button = 18, nav = 20, tile = 24;
}

/// Lucide (ISC), the app's one icon family. Weight follows size: every
/// constant is weight 400 for 16–20 px; the `…Large` ones are weight 300 for
/// 24 px and up. Named for what each glyph means here.
abstract final class AppIcons {
  // Destinations (20).
  static const overview = LucideIcons.orbit400;
  static const tunnels = LucideIcons.route400;            // was cable
  static const remote = LucideIcons.monitor400;           // new rail/bar entry
  static const relay = LucideIcons.radioTower400;
  static const logs = LucideIcons.scrollText400;
  static const settings = LucideIcons.settings400;
  // Tunnels and paths.
  share = arrowUpFromLine400; connect = arrowDownToLine400; direct = zap400;
  mixedPath = split400; carrierTcp = arrowRightLeft400; carrierQuic = layers400;
  computer = laptop400; service = server400; diagnostics = stethoscope400; router = router400;
  // Actions.
  add = plus400; close = x400; copy = copy400; edit = pencil400; delete = trash2400;
  start = play400; stop = square400; refresh = refreshCw400; search = search400;
  more = ellipsis400; export = fileUp400; import = clipboardPaste400;
  paste = clipboardPaste400 (new, by meaning); external = externalLink400;
  clear = eraser400; follow = arrowDown400;
  // State.
  check = check400; ok = circleCheck400; info = info400; warning = triangleAlert400;
  error = circleAlert400; online = cloud400 (was wifi); offline = cloudOff400 (was wifiOff);
  lock = lock400; key = keyRound400; link = link400; unlink = unlink400; show = eye400; hide = eyeOff400;
  // Settings.
  themeSystem = sunMoon400; themeLight = sun400; themeDark = moon400; star = star400;
  update = circleFadingArrowUp400; download = download400;
  // Disclosure.
  chevronDown = chevronDown400; chevronUp = chevronUp400 (new);
  chevronRight = chevronRight400Dir; back = arrowLeft400Dir; forward = arrowRight400Dir;
  // Remote desktop (the 19 of the brief, by meaning).
  screenShare = screenShare400; screenShareOff = screenShareOff400;
  remoteDevices = monitorSmartphone400; viewOnly = eye400; control = mousePointer2400;
  keyboard = keyboard400; shortcuts = command400; rightClick = mouse400;
  trackpad = touchpad400; directTouch = pointer400; qrCode = qrCode400;
  scanQrCode = scanQrCode400; fullscreen = maximize400; pin = pin400;
  disconnect = power400; helper = shieldCheck400; secureDesktop = lockKeyhole400;
  quality = gauge400; phone = smartphone400;
  // Remote desktop, companions.
  exitFullscreen = minimize400; unpin = pinOff400;
  // Weight 300, 24 px and up.
  relayLarge = radioTower300; remoteLarge = monitor300;
  remoteDevicesLarge = monitorSmartphone300; scanQrCodeLarge = scanQrCode300;
}
```

(The listing abbreviates `static const IconData x = LucideIcons.y;`.) Deleted
as unused: `latency`, `list`, `language` (0 references, ui-pages Appendix B).
Every glyph was checked in `lucide_icons_flutter-3.1.25/lib/lucide_icons.dart`
(both weights exist for all of them). Where each remote glyph is used: §2
(e.g. `phone` leads the compact `remoteMobileHostNote` line; `quality` titles
the quality panel and the More-panel row; `remoteDevices` is the viewer panel's
icon because the viewer's device type is unknown, D24).

### 5.2 Sizes and weights in existing code

| Where | Now | After |
| --- | --- | --- |
| `navigation.dart:147` rail | 18 | `AppIconSize.nav` (20) |
| `navigation.dart:296` bar | 20 | unchanged |
| `galaxy/galaxy_panel.dart:152` relay | 28, weight 300 | `AppIcons.relayLarge` at 24 |
| `app_theme.dart` base `iconButtonTheme` | M3 default 24 on touch | `iconSize: 18` (desktop already 18, `:611-616`) |
| `widgets/surfaces.dart:223` `EmptyNote` | 20 | 18 — a one-line note row, not an empty state (the 24/300 empty states are `RemoteEmptyCard`); alternative if the owner prefers: 24 with `…Large` glyphs passed by callers |
| 17 px buttons/menu items: `galaxy_panel.dart:66,339,344,400`, `overview_page.dart:306,313`, `relay/credentials_tab.dart:150,202,302`, `relay/services_tab.dart:341,353`, `settings_page.dart:323`, `tunnels_page.dart:381` | 17 | 18 |
| 17 px field prefix: `logs_page.dart:154` | 17 | 16 |
| 13–15 px inline/chips: `galaxy_panel.dart:424`, `logs_page.dart:158`, `relay/connections_tab.dart:212`, `relay/services_tab.dart:271`, `relay_page.dart:316,368,418`, `tunnel_diagnostics.dart:336,537,730`, `tunnel_editor.dart:583,599`, `update_dialog.dart:121`, `widgets/carrier.dart:40`, `widgets/connections.dart:76`, `widgets/path.dart:38,111` | 13–15 | 16 |

Literal sizes are replaced by `AppIconSize` constants as they are touched.
Chips with 12-px text get 16-px glyphs; check them in the previews.

### 5.3 Material icons removed

| Today | Replacement |
| --- | --- |
| `window_chrome.dart:111,117,117,126` `remove`, `filter_none`, `crop_square`, `close` | self-drawn `CaptionGlyph` (§6) |
| `tunnels_page.dart:37` `desktop_windows_outlined` | deleted (rail entry) |
| `desktop_page.dart:102,108,136,189,324` `copy`, `stop_circle_outlined`, `screen_share_outlined`, `desktop_windows_outlined`, `screen_share_outlined` | file deleted; new host card uses `copy`, `screenShareOff`, `screenShare`, `remote` |
| `desktop_viewer.dart:347,352,357,368,379,407` `keyboard_outlined`, `ads_click`, `mouse_outlined`, `arrow_upward`, `arrow_downward`, `pan_tool_outlined` | file deleted; `keyboard`, (tap gesture), `rightClick`, (three-finger scroll), (Shortcuts → Release all keys) |
| `desktop_connection_banner.dart:55,61` `check_circle`, `link_off` | file deleted; status dot / overlays |
| Implicit: `AppBar` back (`desktop_page.dart:57`, `desktop_viewer.dart:261`), `ExpansionTile` (`:194`), `DropdownButtonFormField` (`:207`) | widgets removed (`MenuAnchor` + `AppIcons.chevronDown`) |

After the change `grep -rn "Icons\." ui/lib` finds nothing. `cupertino_icons`
(`pubspec.yaml:43`, unused) is removed; `uses-material-design` stays (icon
tree-shaking drops the font from release builds). Optional cheap guard: one
line in `.github/workflows/syntax-check.yml`'s Flutter job,
`! grep -rnE '\bIcons\.' ui/lib`.

### 5.4 Test fonts and posters

- `test/support.dart` `loadRealFonts`: add
  `family('packages/lucide_icons_flutter/Lucide400', ['packages/lucide_icons_flutter/assets/build_font/LucideVariable-w400.ttf'])`;
  keep `Lucide300` (the `…Large` glyphs); delete the MaterialIcons loader and
  its `FLUTTER_ROOT` lookup (`:114-124`).
- `test/gen_poster_test.dart:110-140`: chips use `AppIcons.direct`,
  `AppIcons.relay`, `AppIcons.key`, and — two glyphs the app does not use —
  `LucideIcons.arrowLeftRight400`, `LucideIcons.code400` (16 px → weight 400).
  The Tunnels capture loses the `open-desktop` button. Regenerate
  `docs/assets/poster.png` and `poster.zh-CN.png`.

---

## 6. Title bar (requirement 4)

### 6.1 `page_frame.dart` `_TitleBar`

```dart
Padding(
  padding: EdgeInsetsDirectional.only(
    start: onBack == null ? lead : lead - 12,
    end: drawsCaptionButtons ? 16 : 8,                 // was 8 (:120)
  ),
  child: Row(children: [
    if (onBack != null) ...[ IconButton(key: Key('page-back'), …AppIcons.back…), const SizedBox(width: 4) ],
    Expanded(child: IgnorePointer(child: Text(title, …))),       // unchanged
    if (sharingIndicator) ...[ const SizedBox(width: 12), const SharingIndicator() ],
    ...actions,
    if (primary != null) ...[ const SizedBox(width: 6), /* FilledButton.icon / IconButton.filled, unchanged */ ],
    if (drawsCaptionButtons) ...[
      const SizedBox(width: 16),                       // was 10 (:167)
      const CaptionButtons(),                          // was CaptionButtons(height: 36) (:168)
    ],
  ]),
)
```

The bar stays 58 tall in the sheet (54 compact); the row is centred by the
existing `Stack(alignment: centerStart)`, so the 32-px buttons sit 13 px from
top and bottom. In the sheet the close button's right edge is 16 px inside the
sheet edge (8 px sheet inset + 16 = 24 px from the window edge) at every scale
factor and window state, which is the acceptance criterion by construction.

### 6.2 `window_chrome.dart`

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
```

- `_CaptionButtonsState with WindowListener`: as today, plus
  `onWindowRestore() => _sync()` (a restore from minimised can change the
  maximised state). Row of three 46×32 buttons, no gaps (Windows style):
  minimize, maximize/restore, close (still `windowManager.close()` so close
  hides to the tray, invariant ui-pages §5.9).
- `_CaptionButton({required CaptionGlyph glyph, required String tooltip, required VoidCallback onTap, required CaptionTone tone, bool close = false})`
  (`icon`, `iconSize`, `height` removed): `Tooltip` → `Semantics(button: true, label: tooltip)` →
  `MouseRegion` (hover) → `GestureDetector(opaque, onTapDown/Up/Cancel for pressed, onTap)` →
  `AnimatedContainer(duration: reduced ? 0 : 90 ms, width: 46, height: 32, BoxDecoration(color: bg, borderRadius: 6))`
  → `Center(CaptionGlyphIcon(glyph, color: fg))`.

| Tone | Idle fg | Hover bg / fg | Pressed bg |
| --- | --- | --- | --- |
| surface | `onSurfaceVariant` | `onSurface @ 0.08` / `onSurface` (today's) | `onSurface @ 0.12` |
| viewer | `#B4B4B8` | `white @ 0.09` / `#EDEDEF` | `white @ 0.14` |
| close, both | (tone idle) | `#C42B1C` / white (today's `:167`) | `#C42B1C @ 0.9` / `white @ 0.9` |

`CaptionGlyphIcon` is a `LeafRenderObjectWidget` (`RenderCaptionGlyph`, 10×10
logical) taking `glyph`, `color` and `MediaQuery.devicePixelRatioOf(context)`;
it repaints when any changes. Paint (pixel snapping):

1. `g = localToGlobal(Offset.zero)`; shift by
   `((g * dpr).round() − g * dpr) / dpr` so the box starts on a device pixel;
   `canvas.scale(1 / dpr)` and draw in device pixels.
2. Stroke `s = max(1, (1 * dpr).round())` (1 px at 100 % and 125 %, 2 px at
   150 % and 200 %); box `B = (10 * dpr).round()` (10, 13, 15, 20); half
   `h = s / 2`; corner `r = max(1, dpr.round())`; restore offset
   `o = (2 * dpr).round()`. `Paint(style: stroke, strokeWidth: s, isAntiAlias: true, strokeCap: butt)`.
3. Glyphs (in the artboard's 10-px grid, `Icons:72-89`, scaled to `B`):
   - minimize: line `(0, y)–(B, y)`, `y = (B / 2).floor() + (s.isOdd ? 0.5 : 0)`;
   - maximize: `RRect.fromLTRBR(h, h, B − h, B − h, r)`;
   - restore (not drawn on the artboard; spec §2.14 gives the path): front
     `RRect.fromLTRBR(h, o + h, B − o − h, B − h, r)`; back = the visible
     top-right part of a square offset by `o`: `M(o+h, o+h) L(o+h, h+r)` arc
     `(o+h+r, h)` `L(B−h−r, h)` arc `(B−h, h+r)` `L(B−h, B−o−h−r)` arc
     `(B−h−r, B−o−h)` `L(B−o−h, B−o−h)`;
   - close: `(h, h)–(B−h, B−h)` and `(B−h, h)–(h, B−h)`.

The viewer uses `CaptionButtons(tone: CaptionTone.viewer)` inside its 36-px
bar (2 px above and below, 4 px end padding; there is no sheet corner).
`setup_flow.dart:196-201` becomes `Positioned(top: 6, right: 8, child: CaptionButtons())`
(centred in its 44-px drag strip). The `Icons` import leaves the file.

---

## 7. Localisation

All new keys go in one contiguous block in both `app_en.arb` and `app_zh.arb`
(placeholders declared in `@key` metadata; `plural`/`select` where shown).
After the change no `desktop*` key remains and `l10n_untranslated.json` is
empty. Every Chinese value uses 共享 (never 分享) for sharing.

### 7.1 Deleted: the 42 unused `desktop*` keys

`desktopBackgroundActive, desktopBackgroundHelp, desktopBackgroundStartup,
desktopBackgroundStopped, desktopBackgroundTitle, desktopBusy,
desktopConnected, desktopHelperEnable, desktopHelperHelp,
desktopHelperMaintenance, desktopHelperRestart, desktopHelperRunning,
desktopHelperStopped, desktopHelperTitle, desktopHelperUse,
desktopHostUnsupported, desktopInteract, desktopInvitation,
desktopInvitationHelp, desktopNavigate, desktopOrdinaryOnly, desktopPreview,
desktopSecureActive, desktopSecureSetupReady, desktopSecureSetupRepair,
desktopSecureSetupRequired, desktopSecureSetupStopped,
desktopSecureSetupTitle, desktopShareNeedsSetup, desktopShowInvitation,
desktopTrustControl, desktopTrustOtherRelay, desktopTrustRemember,
desktopTrustRememberHelp, desktopTrustRestore, desktopTrustStartup,
desktopTrustStartupEnabled, desktopTrustStartupHelp, desktopTrustTitle,
desktopTrustView, desktopViewOnly, desktopWaiting`. Four of their meanings
return under new keys below (`hostUnsupported`, `hostHelperNone`,
`hostHelperStopped`, `inviteLabel`).

### 7.2 The 51 used `desktop*` keys

| Old key | Fate |
| --- | --- |
| `desktopTitle` | → `navRemoteDesktop` |
| `desktopAllowControl` | → `hostAllowControl` (new value) |
| `desktopDisplay` | → `hostDisplay` (new value) |
| `desktopEnable` | → `hostEnable` (en "Turn on remote control") |
| `desktopHelperInstall`, `desktopHelperDisable`, `desktopHelperUninstall` | → `hostHelperInstall`, `hostHelperDisable`, `hostHelperUninstall` (values kept) |
| `desktopService` | → `hostServiceName` (adds "…") |
| `desktopStop` | → `hostStop` (zh 停止共享) |
| `desktopStopHelp` | → `hostStopNote` (new value) |
| `desktopTrustRevoke` | → `hostRevoke` (zh 共享) |
| `desktopOpen` | → `remoteOpen` |
| `desktopRelayOnly` | → `remoteRelayOnly` (en "Relay only") |
| `desktopDisconnect` | → `viewerDisconnect` (zh 断开) |
| `desktopIdentityMismatch` | → `viewerIdentityTitle` (en "Host identity changed") |
| `desktopIdentityMismatchHelp` | → `viewerIdentityBody` (new value) |
| `desktopKeyboard` | → `viewerTypeText` ("Type text…" / 输入文字…) |
| `desktopSendText` | → `viewerSendText` ("Send" / 发送) |
| `desktopRelease` | → `shortcutReleaseKeys` ("Release all keys" / 释放所有按键) |
| `desktopRightClick` | → `touchRightClick` |
| `desktopTouchMode` | → `touchModeDirect` (en "Direct touch") |
| `desktopTrackpadMode` | → `touchModeTrackpad` |
| `desktopAckLatency`, `desktopAdvanced`, `desktopConnect`, `desktopConnecting`, `desktopCopy`, `desktopEnableHelp`, `desktopEnabledHelp`, `desktopEndControl`, `desktopFrameAge`, `desktopGestureHelp`, `desktopLeftClick`, `desktopOrdinaryOnlyHelp`, `desktopPaste`, `desktopRecoveryAttempt`, `desktopRequestControl`, `desktopScrollDown`, `desktopScrollUp`, `desktopShare`, `desktopSharing`, `desktopStale`, `desktopStateConnecting`, `desktopStateDelayed`, `desktopStateIdle`, `desktopStateLive`, `desktopStateReconnecting`, `desktopStateRecovering`, `desktopStateRetry`, `desktopStateWaitingDesktop`, `desktopStopped` | deleted (29): their widgets are gone; generic `connect`, `copy` and the overlay strings replace them. `desktopGestureHelp` described the old gestures |

Reused unchanged: `copy`, `cancel`, `close`, `connect`, `more`, `refresh`,
`retry`, `pathDirect`, `pathRelay`, `navLogs`, `window*`, `tray*`.

### 7.3 New keys (en / zh)

Shell and tray

| Key | en | zh |
| --- | --- | --- |
| `navRemoteDesktop` | Remote desktop | 远程桌面 |
| `navRemote` | Remote | 远程 |
| `back` | Back | 返回 |
| `settingsLogsHint` | What pb-mapper is doing, for troubleshooting | 运行日志，排查问题时查看 |
| `sharingBeingViewed` | Being viewed | 正在被查看 |
| `sharingBeingControlled` | Being controlled | 正在被控制 |
| `hostDisconnectViewer` | Disconnect viewer | 断开查看者 |
| `trayHostSharing` | Sharing this computer | 正在共享这台电脑 |
| `trayHostViewed` | Someone is viewing this computer | 有人正在查看这台电脑 |
| `trayHostControlled` | Someone is controlling this computer | 有人正在控制这台电脑 |

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

Add computer

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
| `addRememberUnavailable` | This device can't store invitations securely, so the computer is kept only until the app closes. | 此设备暂不能加密保存邀请，这台电脑只保留到应用关闭。 |
| `addMoreOptions` | More options (relay only) | 更多选项（仅使用中继） |
| `scanHint` | Point at the QR code on the computer's Remote desktop page | 对准电脑“远程桌面”页上的二维码 |
| `or` | or | 或 |
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
| `hostDisplaySwitchBody` | Switching displays reconnects the current viewer. | 切换显示器会让当前查看者重新连接。 |
| `hostBackground` | Runs in background | 后台运行 |
| `hostBackgroundService` | Windows service · keeps sharing after the window closes | Windows 服务 · 关闭窗口后继续共享 |
| `hostBackgroundApp` | With the app · keeps sharing from the tray after the window closes | 随应用运行 · 关闭窗口后在托盘继续共享 |
| `hostSecure` | UAC and lock screen | UAC 与锁屏 |
| `hostHelperReady` | Helper service ready | 辅助服务已就绪 |
| `hostHelperStopped` | Helper service stopped | 辅助服务已停止 |
| `hostHelperNone` | Ordinary desktop only | 仅普通桌面 |
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
| `hostRevoke` | Revoke invitation and stop sharing | 撤销邀请并停止共享 |
| `hostRevokeConfirmBody` | Everyone holding the current invitation loses access. A new invitation is created the next time you turn sharing on. | 持有当前邀请的人都会失去访问权限。下次开启共享时会生成新的邀请。 |
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
| `viewerRecovering` | Recovering the picture | 正在恢复画面 |
| `viewerIdentityTitle` | Host identity changed | 主机身份已变化 |
| `viewerIdentityBody` | This computer no longer matches the invitation, so reconnecting stopped. Ask for a new invitation and stop using the old one. | 这台电脑的身份与邀请里记录的不一致，已停止自动重连。请向对方要一条新的邀请，不要继续使用旧邀请。 |
| `viewerPasteNewInvite` | Paste new invitation | 粘贴新邀请 |
| `viewerEndedTitle` | Connection ended | 连接已结束 |
| `viewerEndedStopped` | The other computer stopped sharing. | 对方已停止共享。 |
| `viewerEndedByHost` | The other computer ended this session. | 对方结束了这次连接。 |
| `viewerEndedVersion` | The other computer runs a different pb-mapper version. Update both to the latest. | 对方的 pb-mapper 版本不同，请两边都更新到最新版本。 |
| `viewerBusy` | Another device is already viewing that computer. | 对方正在被另一台设备查看。 |
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

Key caps (`Ctrl`, `Alt`, `Shift`, `Win`, `Esc`, `Tab`, `Fn`, `F1`…`F12`,
`Del`, `Ctrl+C`, `Ctrl+V`, arrows) and shortcut hints (`Win + D` …) are mono
literals, identical in both languages, not ARB keys.

---

## 8. Tests

Minimal and deliberate: each test guards a state machine, a security or
performance boundary, or a flow that crosses layers. No wording, colour,
glyph or layout-matrix tests.

### 8.1 New or rewritten

| File | Test | Guards |
| --- | --- | --- |
| `test/remote_gesture_test.dart` (replaces `desktop_gesture_test.dart`) | "trackpad taps click at once and a held second tap drags without a remote double-click" — after the first `up()` the sink already has `click(left)` (no pump); a second touch within 300 ms held 300 ms produces no press before 520 ms from the first click, then `press, move…, release` in order; a quick second tap is a second immediate click | D18 immediacy, I4 guard |
| | "long press right-clicks, a two-finger tap does nothing and three fingers scroll without zooming" — 500 ms hold → one `click(right)`; two-finger tap → no sink call; a 72-px downward three-finger swipe → wheel events summing to −3, transform unchanged | D18 gesture set, class escalation |
| | "zoom stays local through pinch, reconnects and the soft keyboard" — pinch: no sink call, same `Texture` element, scale > 1.2, a later direct tap maps through the zoom; `inputEpoch` change keeps the transform; `beginKeyboard()` + `setOcclusion(bottom: 200)` keeps `scale` and `scene` and the element; `endKeyboard()` restores the transform exactly | P1-4 (keeps today's pinch/epoch test, adds the keyboard case the docs map asked for) |
| | "direct touch drags with one finger and a second finger releases the drag" | D18 direct mode, release on navigation |
| `test/remote_input_test.dart` | "latched modifiers wrap one action and never stay held" — Ctrl tapped once: `tapKey(c)` sends `[control↓, c↓, c↑, control↑]` and the latch is off; tapped twice: two keys both wrapped, still locked; a click is wrapped; `releaseAll` clears; a sequence failing after its first event sends `release` | D16 |
| | "hardware letters with Ctrl go as keys and AltGr characters stay text" | I11, D17 vocabulary |
| `test/viewer_chrome_test.dart` | "an unpinned toolbar collapses 2 s after the pointer leaves, never while a menu is open" — mouse enter/exit; 1.9 s expanded, 2.0 s handle; with the Display menu open, 3 s still expanded; pinned never collapses | §3.7 |
| | "connection overlays follow the phase and never replace the picture" — `ViewerStage` over `ValueNotifier`s: live → retryWaiting → waitingForDesktop → live keeps the same `Texture` element; the retry pill offers Retry now; identityChanged wins over retryWaiting | §3.8, ui-pages invariant §5.2 |
| `test/title_bar_test.dart` | "caption buttons keep 16 px from the sheet edge" — `debugDefaultTargetPlatformOverride = TargetPlatform.windows`, wide window at dpr 1.25: close button 46×32, `closeRect.right <= sheetRect.right − 16`, vertically centred in the 58-px bar | requirement 4 acceptance |
| `test/add_computer_test.dart` | "an invitation is recognised before connecting and never shown in full" — `nav-remote` → `remote-add`; a setup string shows `addSetupString` and Connect stays disabled; a `pbd1.` invite shows service and fingerprint, the token is nowhere on screen, Remember is on; Connect → `FakeDesktopApi` records the connect and the viewer opens on the Connecting overlay | D3 inspect flow, invite secrecy |
| `test/remote_session_test.dart` (replaces `desktop_viewer_navigation_test.dart`) | "leaving the viewer pauses the session and Back to picture resumes it" — back → `detachViewer` recorded, row shows `remoteBackToPicture`; tap → `attachViewer`; disconnect → list, disconnect recorded | D13, single session by construction |

### 8.2 Changed

- `test/app_test.dart`: restart test uses `openRemoteOnStartup` and expects
  `RemotePage` (no `pageBack`); rail expects six `nav-*` keys including
  `nav-remote`; the phone test expects five bar keys (no `nav-logs`) and
  reaches Logs through `settings-logs`, back through `page-back`; the
  small-phone overflow loop covers all six pages (Logs through Settings).
- `test/desktop_test.dart`: host flow through `nav-remote` → `host-enable` →
  `host-stop`; helper maintenance through `host-more` (still reachable while
  management is lost); the persistent-stop test becomes "while viewed, every
  page offers stop even when busy" (`sharing-indicator-stop` on Tunnels while
  `busy`); the viewer input-mapping/stale-blocking test is ported to the new
  viewer (it is the input-authorisation boundary); the store ordering test is
  the pipeline package's.
- `test/desktop_host_readiness_test.dart`: `desktop-share` → `host-enable`,
  `desktop-stop` → `host-stop`, page via `pumpApp(page: AppPage.remote)`.
- `test/tray_service_test.dart`: one assertion that a hosting status with a
  viewer renders Disconnect viewer and Stop sharing, and Stop calls
  `onStopSharing` (local stop boundary).
- `test/fake_desktop_api.dart`: `inspectInvitation` (`SETUP_STRING` for base64
  setup strings, info for `pbd1.`), records connect/attach/detach/sequences,
  and whatever the pipeline adds; `test/fake_pb_mapper_api.dart` gains an
  in-memory vault.
- `test/support.dart`: `pumpApp(openRemoteOnStartup:)`; fonts per §5.4.
- Deleted: `desktop_gesture_test.dart`, `desktop_viewer_navigation_test.dart`
  (replaced above).

### 8.3 Previews and posters

`test/preview_test.dart` (opt-in, `PB_MAPPER_UI_PREVIEW_DIR`) gains:
`remote_setup_light`, `remote_sharing_light`, `remote_sharing_dark`,
`remote_add_dialog_light`, `viewer_shortcuts_dark` (1440×900, Shortcuts menu
open), `viewer_reconnect`, `viewer_identity`, `viewer_collapsed_view_only`,
`title_bar_windows_125` (Windows override, dpr 1.25, close hovered),
`phone_remote_tab`, `phone_add_paste`, `phone_viewer_trackpad` (844×390),
`phone_viewer_keyboard` (simulated `viewInsets`, Ctrl locked),
`phone_gesture_sheet`. The picture is a fake remote screen painted with a
`PictureRecorder`, encoded to PNG and delivered through the fake JPEG path
(`instantiateImageCodec` decodes PNG too), so previews run on Linux CI-like
hosts without a native texture. Posters regenerate per §5.4.

Not automated (on-device checks before the merge gate, D28): IME insets in
immersive mode on Android; `BackdropFilter` greyscale over the external
texture (Android GL with Impeller off, Windows); scanner permission flows;
caption glyph crispness at 125 % and 150 %; the feel of the 520-ms drag guard
and the 24-px scroll notch.

---

## 9. Implementation sequence

Each step is its own commit (D27), ends with `flutter analyze --no-fatal-infos`
and the narrow tests named, and leaves the app building and usable.

1. **Title bar (R4, cherry-pickable).** `window_chrome.dart` (§6.2),
   `page_frame.dart` padding/gap/`CaptionButtons()` only, `setup_flow.dart`.
   `test/title_bar_test.dart`. Check by hand at 100/125/150 %, maximised and
   restored.
2. **Icons (R3).** 2a, cherry-pickable: `app_icons.dart` (400, `…Large`,
   swaps, the remote constants), §5.2 size audit, `IconButton` theme 18, rail
   20, `EmptyNote` 18, galaxy `relayLarge`, `support.dart` fonts, poster chips,
   `cupertino_icons` removed, optional CI grep. 2b, branch only: the PR #51
   desktop pages' Material icons replaced mechanically so the branch has zero
   `Icons.` before those files are rewritten. Regenerate previews and posters;
   run the full `flutter test` once (global visual change).
3. **State seams.** `AppStore` (no bridge, eager `desktop`, D26 filter,
   `desktopServiceNames`, `computers`), `ViewerPrefs`, `RemoteComputerStore`
   (with the vault reporting `unavailable` until the DPAPI/Keystore vault
   lands — never plain text), `ViewerHost`, `RemoteDesktop`, the §4.2 members
   on `DesktopStore` (where the pipeline has not landed yet, derived from
   today's fields: `viewerState` from `connectionPhase`, `stats` from
   `snapshot.viewer` and `presentFps`, `sharing` from `snapshot.host`;
   `stage` null), tray (§1.7). Tests: `store_test`, `desktop_recovery_test`,
   `tray_service_test`.
4. **Shell and Remote page.** `AppPage.remote`, navigation (§1.2), Logs in
   Settings + `PageFrame.onBack`, `SharingIndicator` (banner deleted),
   `openRemoteOnStartup`, `open-desktop` and `desktop_page.dart` deleted;
   `RemotePage` with `HostCard`, `InviteWell` + QR (`qr: 3.0.3`), wide and
   compact lists, add dialog/sheet in paste mode with `inspectInvitation`;
   in-shell viewer hosting through `RemoteViewerScreen`, which for this one
   step wraps the old viewer body under `ViewerTitleBar` (its `AppBar`
   removed). Tests: `app_test`, `add_computer_test`, `desktop_test` host parts,
   `desktop_host_readiness_test`.
5. **Viewer input core.** `host_keys.dart` (after the host `Key` vocabulary
   from the host package), `RemoteViewport`, `TouchGestureMachine`,
   `RemoteSurface`, `RemoteInput`, `ModifierLatch`; the old surface is
   replaced inside the transitional viewer. Tests: `remote_gesture_test`,
   `remote_input_test`.
6. **Desktop viewer.** `DesktopViewer`, `ViewerTitleBar`, toolbar and
   controller, menus, quality panel, overlays, hints, full screen,
   `ViewerStage`, `JpegPicture`; delete `pages/desktop_viewer.dart` and
   `desktop_connection_banner.dart`; update the acceptance tool. Needs the
   store's `viewerState`, `stats`, `view`, `retryNow`, `setQuality`, `lock`,
   `sendSequence`, `attachViewer/detachViewer`. Tests: `viewer_chrome_test`,
   `remote_session_test`, ported `desktop_test` viewer case.
7. **Mobile viewer.** `TouchViewer`, gutters, side panels,
   `RemoteTextInput`, `ModifierBar`, `Minimap`, `VirtualMousePad`,
   `GestureSheet`, immersive/orientation. (Covered by step 5's tests; no new
   test.)
8. **Mobile add and thumbnails.** `mobile_scanner: 7.5.0`, `QrScanView`,
   manifest and `Info.plist` (bump the Android Kotlin plugin only if the build
   requires it, D21); thumbnails through `capturePicture` (D23).
9. **Finish.** l10n sweep (no `desktop*` keys, zh complete), previews and
   posters, docs: `docs/ui-design.md` (six entries, Logs in Settings on
   phones, icon weight/size rules, caption sizes), `docs/remote-desktop.md`
   (gesture table, modifier latches, viewer states, sharing indicator),
   `CLAUDE.md` page list (Remote page, in-shell viewer), `CHANGELOG.md`
   (right click is now a long press; drag is double-tap-and-hold; two-finger
   tap no longer right-clicks; three-finger scroll; Ctrl + wheel goes to the
   remote; Logs moved into Settings on phones).

Steps 1–2 are independent of everything else; 3–4 depend only on
`inspectInvitation`, `disconnectViewer` and the host status fields (D24);
5–7 depend on the host key vocabulary and the store contract of §4.2.

---

## Appendix — widget keys tests rely on (old → new)

`open-desktop` → deleted (`nav-remote`); `desktop-share` → `host-enable`;
`desktop-stop` → `host-stop`; `desktop-invitation` → `add-invite`;
`desktop-connect` → `add-connect`; `desktop-helper-maintenance` →
`host-more`; `desktop-helper-disable` / `-uninstall` →
`host-helper-disable` / `-uninstall`; `desktop-touch-mode` →
`touch-mode-toggle`; `desktop-connection-state` → the overlay keys
(`viewer-overlay-<kind>`); `desktop-scene-transform` →
`remote-scene-transform`. New: `nav-remote`, `settings-logs`, `page-back`,
`sharing-indicator(-disconnect|-stop)`, `remote-add`, `remote-add-empty`,
`remote-refresh`, `remote-scan`, `remote-paste`, `computer-<id>`,
`computer-open-<id>`, `computer-more-<id>`, `computer-featured`,
`host-permission`, `host-copy`, `host-qr`, `host-display`,
`host-allow-control`, `host-enable-ordinary`, `host-start`,
`host-make-view-only`, `host-disconnect-viewer`, `host-helper-update`,
`host-rename`, `host-revoke`, `add-paste`, `add-invite-clear`,
`add-recognised`, `add-name`, `add-remember`, `add-more`, `add-relay-only`,
`add-replace`, `viewer-back`, `viewer-pin`, `viewer-quality-chip`,
`viewer-mode`, `viewer-display`, `viewer-shortcuts`, `viewer-more`,
`viewer-fullscreen`, `viewer-disconnect`, `viewer-toolbar-handle`,
`viewer-quality-panel`, `viewer-type-text`, `display-fit`,
`display-original`, `quality-quality|balanced|smooth`, `shortcut-*`,
`touch-back`, `touch-quality`, `touch-mode-toggle`, `touch-keyboard`,
`touch-shortcuts`, `touch-right-click`, `touch-more`, `modifier-bar`,
`modifier-control|alt|shift|meta|fn`, `modifier-collapse`, `minimap`,
`virtual-mouse`, `gesture-mode`, `gesture-got-it`.
