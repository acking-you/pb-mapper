# UI architecture — remote desktop redesign, icons, title bar (structure-first)

Scope: BRIEF requirements 2 (redesign), 3 (icons), 4 (title bar) under
DECISIONS D14-D26. Visual spec = `maps/design-spec.md` (cited as `DS §x`).
All paths are relative to `ui/` unless they start with `crates/`, `docs/` or
`test/` (= `ui/test/`). Line numbers are at `a41eaa7`.

Angle: one shared viewer core for desktop and mobile, one host/remote model
for both management layouts, the smallest set of new widgets, and the
existing POCOX shell patterns (`PageFrame`, `GroupCard`, `GlassSegmented`,
`showAdaptivePanel`, `confirmDestructive`) reused rather than shadowed.

---

## 0. Structural decisions at a glance

| # | Decision | Why |
|---|---|---|
| S1 | **Remote desktop is a shell page** (`AppPage.remote`) built on `PageFrame`. | Gets the title bar, drag area, caption buttons, rail/bar and sheet for free; deletes the two `MaterialPageRoute`s and the AppBar. |
| S2 | **The viewer is an app mode, not a route**: `_PbMapperAppState._viewing` swaps `AppShell` for `DesktopViewer` inside the same `AppScope`. | Rail hidden by construction; exactly one viewer mounted by construction (deletes the `_viewerOpen`/serial guard and its test); `AppScope` reachable (routes are siblings of `home`, `ui-pages §2.4`); the shell under the viewer is unmounted, so nothing behind the picture rebuilds (`ui-pages §3.8`). |
| S3 | **One viewer core, two layouts**: `ViewerController` + `ViewerPicture` + `ViewerStateLayer` + `QualityPanel` + `ViewerMenu` entries are shared; `DesktopViewerLayout` and `MobileViewerLayout` only arrange them. | Input gating, modifier latch, shortcuts, overlays, quality metrics exist once. |
| S4 | **Pure models for every state machine** (gesture machine, modifier latch, key routing, toolbar collapse, overlay mapping, host-card state). Widgets render them. | The deliberate tests target pure classes with explicit timestamps; widgets stay thin. |
| S5 | **DesktopStore exposes slices** (`ValueListenable`s) instead of a whole-store `ChangeNotifier`; `AppStore` stops re-broadcasting (D14). Polling is driven by explicit demand handles. | Each widget rebuilds only on the slice it shows; no 4 Hz app-wide rebuild. |
| S6 | Remembered computers live in a sibling **`RemoteComputerStore`** owned by `AppStore`; encryption stays behind `DesktopApiClient` (D20). | Widgets never see the vault mechanics or the Keystore hop. |
| S7 | One `ViewerCard`, one `ViewerPill`, one `ViewerMenuEntry` model with two renderers (desktop `MenuAnchor`, mobile sheet). | Seven pills, four cards and three menus come from three widgets. |

Interpretations and deviations (flagged so the lead can overrule):

1. **D15 "always-on-top"** = topmost inside the app (a layer above the
   Navigator in `MaterialApp.builder`, above every page, dialog and the
   viewer) plus the tray, which is the out-of-window surface when the window
   is hidden. The app owns exactly one native window through
   `window_manager` 0.5.1 (`main.dart:54-68`); `setAlwaysOnTop` would pin the
   whole app, and a separate topmost tool window needs a multi-window plugin
   or Win32 code — out of this cut. Not unworkable; say so if a window-level
   bar is required.
2. **Desktop viewer back button** (`AppIcons.back` 16 in the dark title bar,
   left of the `monitor` glyph). Not drawn on `Viewer.dc.html`, but without
   it a desktop user cannot reach "Back to picture" without disconnecting.
3. **Touch `IconButton`s default to 20 px** (the bar size, as the mobile
   artboards draw their 44 px circles), desktop stays 18. A 24 px default
   would violate "≥ 24 px → 300".
4. **Toolbar default unpinned** (the sticky note's main behaviour is the 2 s
   collapse; DS §2.6 proposed pinned). Persisted per user.
5. **No orientation lock** on mobile: controls go where the letterbox is
   (side gutters in landscape, top/bottom bars in portrait), same widget.
6. **Minimap has no picture** (DS §2.11), not a second `Texture` (the
   native-decoders map's alternative): cheaper and no double consumption.
7. **Recognised host shows the fingerprint, not a permission** — the invite
   carries no permission (D3).
8. Strings unified where the artboards differ only by device noun:
   "未在共享"→"未共享", "邀请加密保存在这台手机上"→ the desktop text.
9. **No backdrop blur under viewer chrome.** `rgba(32,32,35,0.9)` is already
   near-opaque; a `BackdropFilter` over a live `Texture` re-blurs every video
   frame on the GPU (P1-1 cost on phones).

---

## 1. App shell

### 1.1 `AppPage` (`lib/src/shell/app_scope.dart`)

```dart
/// The six places the app has. Everything else opens over one of them.
enum AppPage {
  overview, tunnels, remote, relay, logs, settings;

  IconData get icon => switch (this) { ..., AppPage.remote => AppIcons.remote, ... };

  /// Rail label ("Remote desktop") vs bar label ("Remote").
  String label(BuildContext context, {bool short = false});
  //   remote: short ? l10n.navRemote : l10n.navRemoteDesktop

  bool get grouped => tunnels || remote || relay || settings;

  /// What the rail lists, top to bottom (6).
  static const railPages = [overview, tunnels, remote, relay, logs, settings];

  /// What the bottom bar lists (5): Logs lives inside Settings.
  static const barPages = [overview, tunnels, remote, relay, settings];

  /// The bar entry that is lit for this page.
  AppPage get barEntry => this == logs ? settings : this;

  static AppPage? parse(String? name); // unchanged; 'remote' is just a new name
}
```

`AppScope` gains:

| Member | Type | Use |
|---|---|---|
| `desktopPrefs` | `DesktopPrefs` | viewer pin, pointer mode, virtual mouse, gesture sheet seen, quality |
| `viewing` | `bool` | whether the viewer mode is on |
| `openViewer` | `VoidCallback` | Remote page / add flow → viewer |
| `closeViewer` | `VoidCallback` | viewer back / disconnect / session gone |

`updateShouldNotify` adds `desktopPrefs` and `viewing`. Doc comments that say
"four/five pages" (`app.dart:37-38`, `navigation.dart:18,169,223`,
`app_scope.dart:10`) are corrected in the same commit.

### 1.2 Navigation (`lib/src/shell/navigation.dart`)

- `NavRail` iterates `AppPage.railPages`; selection index =
  `AppPage.railPages.indexOf(page)` (`:31`). Rail icons 18 → **20**
  (`:147`, BRIEF R3). Geometry otherwise unchanged (`kRailWidth` 212, rows 38).
- `NavBar` iterates `AppPage.barPages`; `selected: entry == page.barEntry`;
  label `entry.label(context, short: true)`; adds a 1 px `outlineVariant` top
  border (DS §2.2).
- `_RelayFoot` appends " · {n} ms" from `store.relay?.latencyMs` when online
  (DS §2.1, reuse `relayLatency`).
- **Sharing dot (D15)**: `_RailRow`/`_BarItem` take `Widget? badge`; for
  `AppPage.remote` the badge is `RemoteShareDot`, defined in
  `navigation.dart`:

  ```dart
  /// Whether this computer is shared, on the Remote desktop entry: a dot while
  /// sharing, pulsing while a viewer is connected. Listens to the host slice only.
  class RemoteShareDot extends StatelessWidget {
    // ValueListenableBuilder(AppScope.of(context).store.desktop.hostView)
    // !sharing → SizedBox.shrink(); viewer == null → StatusDot(signalColor, 7)
    // else → PulsingDot(signalColor, 7)
  }
  ```
  Rail: trailing after the label. Bar: `Positioned(top: 2, right: 12)` over the
  icon pill. Blue = "this computer" (R3 colour rule).

### 1.3 Logs into Settings (compact only)

- `SettingsPage` (`pages/settings_page.dart:28-57`): when
  `!ShellLayout.wideOf(context)`, insert after `RelayCard` a
  `GroupCard` with one `ListTile(key: Key('settings-logs'), leading:
  Icon(AppIcons.logs), title: l10n.navLogs, subtitle: l10n.settingsLogsBody,
  trailing: Icon(AppIcons.chevronRight, size: 16), onTap: () =>
  scope.go(AppPage.logs))`.
- `PageFrame` gains `final VoidCallback? onBack;` (§5.1). `LogsPage` passes
  `onBack: ShellLayout.wideOf(context) ? null : () => scope.go(AppPage.settings)`.
- `_home` wraps the shell in `PopScope(canPop: !(compact && _page ==
  AppPage.logs), onPopInvokedWithResult: (did, _) { if (!did)
  _go(AppPage.settings); })` so Android back returns to Settings. `compact` =
  `!usesRail(context)`.
- Resizing wide→compact on Logs keeps the page; the bar lights Settings and
  the back arrow appears. A persisted `last_page = logs` behaves the same.

### 1.4 Hosting the Remote page and the viewer (`lib/src/app.dart`)

```dart
class _PbMapperAppState ... {
  late final DesktopPrefs _desktopPrefs = widget.desktopPrefs ?? DesktopPrefs();
  bool _viewing = false;
  void _openViewer() { if (!_viewing) setState(() => _viewing = true); }
  void _closeViewer() { if (_viewing) setState(() => _viewing = false); }
  ...
  Widget _home(BuildContext context) {
    ... setup / deciding unchanged ...
    return AppScope(
      store: _store, relay: _relay, prefs: _prefs, desktopPrefs: _desktopPrefs,
      updates: _updates, galaxy: _galaxy, page: _page, go: _go,
      viewing: _viewing, openViewer: _openViewer, closeViewer: _closeViewer,
      child: _viewing
          ? const DesktopViewer()
          : PopScope(..., child: AppShell(page: _page, onSelect: _go, child: switch (_page) {
              AppPage.overview => const OverviewPage(),
              AppPage.tunnels => const TunnelsPage(),
              AppPage.remote => const RemotePage(),
              AppPage.relay => const RelayPage(),
              AppPage.logs => const LogsPage(),
              AppPage.settings => const SettingsPage(),
            })),
    );
  }
}
```

- `_boot` loads `_desktopPrefs.load()` beside `_prefs.load()`.
- **`--desktop`** (Rust helper restart hand-off, `main.dart:39`): `main.dart`
  passes `initialPage: openDesktop ? AppPage.remote : null`.
  `PbMapperApp.initialPage` already overrides `last_page` (`app.dart:119-121`).
  **Delete** `openDesktopOnStartup` (param `:50,74`, the post-frame push
  `:124-135`, the import of `pages/desktop_page.dart` `:6`) and the
  `openDesktopOnStartup` parameter of `pumpApp` (`test/support.dart:34,57`).
- `_finishSetup` unchanged (lands on Overview).
- The viewer leaves the mode itself (`DesktopViewer` §2.4) when the session
  target becomes null; the Remote page and add flow call `scope.openViewer()`
  right after starting a connection (the viewer shows the "connecting" card).
- `_viewing` is not persisted; a relaunch lands on the remembered page.
- **No window title bar of the shell is reused by the viewer**: the viewer
  draws `ViewerTitleBar` (36 px, dark, §2.5.1) with `WindowDragArea` and
  `CaptionButtons(tone: CaptionTone.viewer)`; hidden in full screen.

`DesktopPage`, `openDesktop()`, `DesktopSharingBanner` (`pages/desktop_page.dart`),
`DesktopViewer` (`pages/desktop_viewer.dart`), `DesktopConnectionBanner`
(`pages/desktop_connection_banner.dart`) and the Tunnels `open-desktop` button
(`pages/tunnels_page.dart:33-38`) are deleted.

### 1.5 D15 sharing bar (`lib/src/shell/sharing_bar.dart`, new)

`MaterialApp.builder` changes from `Column[banner, Expanded(child)]`
(`app.dart:321-336`) to a **Stack**, so nothing shifts:

```dart
builder: (context, child) => ToastificationWrapper(
  child: Stack(children: [
    Positioned.fill(child: child ?? const SizedBox.shrink()),
    SharingBarLayer(store: _store),          // builder is above AppScope: pass the store
  ]),
),
```

```dart
/// While a remote viewer is connected to this computer: who is in, how, and
/// the two ways to end it. Above every page, dialog and the viewer.
class SharingBarLayer extends StatelessWidget {
  // ValueListenableBuilder<DesktopHostView>(store.desktop.hostView):
  //   view.viewer == null → SizedBox.shrink()
  //   else Positioned(bottom: inset, left: 0, right: 0, child: Center(child: SharingBar(...)))
  //   inset = usesRail(context) ? 20 : 20 + kNavBarHeight(context) (+ MediaQuery.padding.bottom)
}

class SharingBar extends StatelessWidget {
  // Glass(depth: floating) pill, height 44, radius 22, padding 6/6/6/14:
  // PulsingDot(signalColor) · Text(controlling ? sharingControlling : sharingViewing)
  // · Text(meta: "{path} · {span}", onSurfaceVariant)
  // · TextButton(key: 'sharing-bar-disconnect', viewerDisconnect → store.desktop.endViewerSession())
  // · FilledButton.tonalIcon(key: 'sharing-bar-stop', AppIcons.stopSharing, hostStop → store.desktop.stopHost())
}
```

- Both actions bypass `busy` (store invariant 10, `dart-pipeline §5`).
- Sharing state otherwise lives on `RemoteShareDot` (§1.2), the host card
  (§2.1.2) and the tray (§1.6). `DesktopSharingBanner` is deleted.
- Host is Windows-only, so on phones the layer never shows; on a narrow
  desktop window it sits above the bottom bar.

### 1.6 Tray (`lib/src/common/tray/tray_service.dart`, `app.dart`)

- `TrayStatus` gains `final DesktopTrayState desktop;` with
  `enum DesktopTrayState { off, sharing, viewing, controlling }`.
- `TrayStrings` gains `sharing` (`hostSharing`), `viewing`
  (`sharingViewing`), `controlling` (`sharingControlling`),
  `disconnectViewer` (`trayDisconnectViewer`), `stopSharing` (`hostStop`).
- `initialize(...)` gains `VoidCallback disconnectViewer, VoidCallback stopSharing`.
- New retained items (stable IDs, as `:86-89`): `_sharingItem =
  MenuItem(key: 'sharing', disabled: true)`, `_disconnectViewerItem =
  MenuItem(key: 'disconnect-viewer')`, `_stopSharingItem =
  MenuItem(key: 'stop-sharing')`. `_menu.items` is rebuilt from the retained
  objects: `[status, if (desktop != off) sharing, sep, open, refresh, if
  (viewing|controlling) disconnectViewer, if (desktop != off) stopSharing,
  sep, quit]`. `_renderedMenu` becomes a record that also carries
  `desktop`, so a state change re-renders. `onTrayMenuItemClick` handles the
  two new keys.
- `app.dart`: `_trayStatus()` maps `_store.desktop.hostView.value`;
  `_startDesktop` adds `_store.desktop.hostView.addListener(_refreshTray)`
  (the `AppStore` bridge is gone, D14); callbacks call
  `endViewerSession()` / `stopHost()`.
- Tray icons stay (out of scope, R3).

### 1.7 D26 — desktop services leave Tunnels and the galaxy

`lib/src/state/remote_service.dart`: `bool isDesktopService(String name) =>
name.startsWith('desktop:');` (mirrors `valid_service`,
`crates/pb-mapper-desktop/src/lib.rs:49-56`). `AppStore.remoteServices`
(`app_store.dart:139-148`) skips them, which covers the galaxy
(`overview_page.dart:42`), the Tunnels count/list (`tunnels_page.dart:97,133`)
and the editor's suggestions (`tunnel_editor.dart:279`). `relayServices`
keeps them (Relay page is the relay's own view; online status needs them).

---

## 2. Files and widgets

### 2.0 Tree (new = +, moved = →, deleted = −)

```
lib/src/
  shell/
    app_scope.dart            (AppPage.remote, rail/bar lists, AppScope members)
    navigation.dart           (+ RemoteShareDot; lists; icon 20; bar border)
    page_frame.dart           (title-bar fix §5, onBack)
    window_chrome.dart        (CaptionButtons rewrite §5)
  + shell/sharing_bar.dart    SharingBarLayer, SharingBar
  pages/
  − desktop_page.dart, desktop_viewer.dart, desktop_connection_banner.dart
  → desktop_gesture_surface.dart  → pages/viewer/gesture_surface.dart
  + remote/
      remote_page.dart        RemotePage
      remote_models.dart      HostCardState, hostCardState(), maskInvitation(),
                              defaultServiceName(), formatWhen()
      host_card.dart          HostCard (+ private parts)
      invite_qr.dart          InviteQrPanel
      computer_list.dart      ComputerListCard, ComputerRow, ComputerStatusPill,
                              AddComputerTile, ComputerListEmpty
      mobile_remote.dart      FeaturedComputerCard, PhoneComputerList, AddComputerButtons
      add_computer.dart       openAddComputer(), AddComputerForm
      invite_scanner.dart     InviteScanner
  + viewer/
      desktop_viewer.dart     DesktopViewer (root of the viewer mode)
      viewer_controller.dart  ViewerController
      viewer_picture.dart     ViewerPicture (+ cursor layer, JPEG picture, GreyStill)
      gesture_surface.dart    DesktopGestureSurface (moved + extended)
      touch_gestures.dart     TouchGestureMachine, GestureSink          (pure)
      remote_keys.dart        RemoteKey, KeyRouting, RemoteShortcut     (pure)
      modifier_latch.dart     ModifierLatch, RemoteModifier             (pure)
      toolbar_collapse.dart   ToolbarCollapse                           (pure)
      viewer_state.dart       ViewerOverlayKind, viewerOverlayFor()     (pure)
      viewer_overlays.dart    ViewerStateLayer, ViewerCard, ViewerPill
      viewer_menu.dart        ViewerMenuEntry, ViewerMenuButton, showViewerMenuSheet,
                              displayMenu(), shortcutsMenu(), moreMenu()
      quality_panel.dart      QualityPanel
      desktop_viewer_layout.dart DesktopViewerLayout, ViewerTitleBar
      floating_toolbar.dart   FloatingToolbar, ToolbarHandle, ViewerStatusChip
      mobile_viewer_layout.dart MobileViewerLayout, MobileViewerGeometry (pure)
      modifier_bar.dart       ModifierBar
      soft_keyboard.dart      SoftKeyboardBridge
      minimap.dart            Minimap
      virtual_mouse.dart      VirtualMouse
      gesture_sheet.dart      showGestureSheet, GestureSheet, GestureGlyph
  state/
    desktop_store.dart        (slices §3.1)
  + state/desktop_input.dart  DesktopInput (typed input events)
  + state/desktop_prefs.dart  DesktopPrefs, RemotePointerMode, DesktopQuality
  + state/remote_computers.dart RemoteComputerStore, RemoteComputer, ComputerPresence, presenceOf()
  theme/
    app_icons.dart            (§4)
    app_theme.dart            (+ quietText(); touch IconButton 20)
  + theme/viewer_palette.dart ViewerPalette, viewerTheme()
  widgets/
    status.dart               (StatusChip.coloredLabel)
  + widgets/qr_code.dart      QrCodeView
  + widgets/dashed_border.dart DashedBorder
  theme/glass.dart            (GlassSegment.enabled)
```

Widget inventory: 19 new public widgets (+5 private helpers each in
`host_card.dart`), 8 pure model files. The seven pills (control hint, mode
hint, view-only, degraded, reconnect, recovering, modifier status) are all
`ViewerPill`; the four cards (connecting, secure/waiting desktop, identity
changed, ended) are all `ViewerCard`; Display/Shortcuts/More on both
platforms are `ViewerMenuEntry` lists.

### 2.1 Remote page — desktop (`pages/remote/remote_page.dart`)

```dart
class RemotePage extends StatefulWidget { const RemotePage({super.key}); }
class _RemotePageState extends State<RemotePage> {
  DesktopDemandHandle? _demand;    // store.desktop.demand(DesktopDemand.page) in initState, dispose()
  // initState also: unawaited(scope.store.checkRelay()) for fresh online status (D22),
  //                 unawaited(scope.store.computers.load())
}
```

`PageFrame(title: l10n.navRemoteDesktop (phone: navRemote), actions:
[if computers.isNotEmpty IconButton(key: 'remote-refresh', AppIcons.refresh →
store.checkRelay(force: true))], primary: computers.isEmpty && !phone ? null :
PrimaryAction(key: Key('remote-add'), icon: AppIcons.add, label:
l10n.remoteAddComputer, onPressed: () => openAddComputer(context)), body: ...)`
— header actions only with ≥ 1 computer (DS §2.4), always on phone.

Layout (`LayoutBuilder` on the body):

| `RemoteLayout` | When | Body |
|---|---|---|
| `wide` | `!isTouchPlatform` and inner width ≥ 1000 (560+20+420) | `SingleChildScrollView(padding 4/32/32/32)` → `Row(crossAxisAlignment: start)[SizedBox(560, HostCard or HostUnsupportedNote), gap 20, Expanded(ComputerListCard or ComputerListEmpty)]` |
| `single` | `!isTouchPlatform`, narrower | `ListView(maxWidth 720, padding 4/18/28/18)`: host card, gap 16, list card |
| `phone` | `isTouchPlatform(context)` | §2.2 |

`isTouchPlatform(BuildContext)` is extracted from `usesRail`
(`shell/app_shell.dart:15-20`) and reused by `usesRail`, `RemotePage`,
`openAddComputer` and `DesktopViewer`.

#### 2.1.1 Host-card state machine (`pages/remote/remote_models.dart`, pure)

```dart
enum HostCardState { unsupported, notEnabled, enabling, sharing, paused }

HostCardState hostCardState(DesktopHostView v) => switch (v) {
  _ when !v.supported => HostCardState.unsupported,
  _ when v.enabling || v.restarting => HostCardState.enabling,
  _ when v.sharing => HostCardState.sharing,
  _ when v.configured => HostCardState.paused,
  _ => HostCardState.notEnabled,
};
```

`v.error` is orthogonal: an inline error line under the header in every
state. Mapping to cards (DS §2.3.1 table):

| State | Card | Header pill | Primary |
|---|---|---|---|
| unsupported | none; `HostUnsupportedNote` (one `EmptyNote`, `AppIcons.info`, `hostUnsupported`) and the list takes the full width | — | — |
| notEnabled | setup card (§2.1.3) | grey "未共享" | `host-enable` "开启远程控制" (`AppIcons.verified` 18) |
| enabling | setup card, button spinner + `hostEnabling`; step discs highlight step 2 while `restarting` | grey | disabled |
| sharing | sharing card (§2.1.2) | green "正在共享" | `host-stop` outlined "停止共享" |
| paused | sharing card minus viewer panel | grey "未共享" | `host-start` filled "开启共享" (`AppIcons.startSharing`) → `resumeSharing()` |

#### 2.1.2 `HostCard` (`pages/remote/host_card.dart`)

`HostCard({super.key})` → `ValueListenableBuilder<DesktopHostView>(desktop.hostView)`
→ `RaisedSurface(radius: kCardRadius)` with padding 24 (setup 28), gap 20.
Private parts, top to bottom (DS §2.3.1):

| Part | Content | Data / action |
|---|---|---|
| `_HostHeader` | tile 44 r12 (`AppIcons.remoteLarge` 24/300; signal on `tertiaryContainer` when sharing, grey otherwise) · `hostThisComputer` 17/600 · service mono 12.5 `quietText` (setup: `hostOff`) · `StatusChip(coloredLabel: true)` | `v.service` |
| `_ViewerPanel` (sharing, `v.viewer != null`) | well `cardFieldFill` r14: `AppIcons.devices` 20 signal · `hostViewerControlling`/`hostViewerWatching` 14.5/600 · `hostViewerMeta(path, span)` · outlined `host-make-view-only` `hostMakeViewOnly` (hidden when not controlling and `!v.controlAllowed`) | D24: `viewer.controlling`, `viewer.path` (omit when null), `viewer.since` → `allowControl(false)` |
| `_PermissionPicker` | label `hostPermissionLabel`; `GlassSegmented<bool>(key: 'host-permission', height: 40)` "仅查看画面"/"查看并控制" (capsule, D19) | `v.controlAllowed` → `allowControl(bool)` |
| `_InviteWell` | label `hostInvitation`; well 48 r14: `maskInvitation(v.invitation)` mono 13 · outlined small `host-copy` (`AppIcons.copy` 16, `copy`) · `host-qr` (`AppIcons.qrCode` 16, `hostQr`/`hostQrHide`); help `hostInviteHelp` 12.5 `quietText` | copy → `Clipboard.setData` + toast `copied(hostInvitation)` |
| `InviteQrPanel` (§2.1.5) | under the well when `_showQr` | |
| `_HostSettingsList` | three 48 px rows with hairlines: `hostDisplay` → `MenuAnchor` button "显示器 n · w × h" + `chevronDown` 16 (`host-display`) · `hostBackground` → `hostBackgroundService`/`hostBackgroundApp` · `hostSecure` → `AppIcons.check` green + `hostHelperReady` / `hostHelperStopped` / `hostHelperNone` / `hostHelperOutdated` (+ "Update" link → `helperAction('install')`, D2) | `v.displays`, `v.displayId` → `setDisplay(id)`; `v.background`; `v.helper` |
| `_HostActions` | stop/start · ghost "更多" (`AppIcons.more` 16) → `MenuAnchor(key: 'host-more')`: `host-service-name` (`hostServiceName…`, disabled while sharing with tooltip `hostServiceNameBusy`), `host-helper-install`, `host-helper-disable` + `host-helper-uninstall` (when installed), divider, `host-revoke` (error colour → `confirmDestructive(hostRevokeTitle, hostRevokeBody, hostRevoke)` → `revokeTrust()`) · right note `hostStopNote` | `stopHost()`, `resumeSharing()`, `helperAction`, `renameService` |

`_showQr` is `HostCard` state: reset in `deactivate`, and by an
`AppLifecycleListener(onInactive/onHide/onPause: hide)` so the code never sits
in the task switcher (deps §7.1). Leaving the page disposes the card (pages
are rebuilt per switch, `ui-pages §2.2`). The invite is never logged.

#### 2.1.3 Setup card (notEnabled / enabling)

Same `HostCard` file, `_SetupBody`: headline `hostSetupHeadline` 22/600;
`_SetupSteps` (three rows, disc 26: next step ink, others `fieldFill`; when
`v.helper == ready` steps 1-2 show a green `AppIcons.check` disc); switch
`host-allow-control` `hostSetupAllowControl` (default on, local state); the
display row (shared with §2.1.2); full-width 48 r24 ink
`FilledButton.icon(key: 'host-enable', AppIcons.verified 18, hostEnable)` →
`enableHost(displayId: _display, allowControl: _allow)`; under it a link
`host-ordinary-only` `hostOrdinaryOnly` → `enableHost(..., ordinaryOnly:
true)` and the note `hostBackgroundNote`. The service name is
`defaultServiceName(Platform.localHostname)` (`desktop:` + lowercase
`[a-z0-9._-]`, ≤ 120, fallback `desktop:computer`) unless one is saved.

#### 2.1.4 Remote computer list (`pages/remote/computer_list.dart`)

```dart
class ComputerListCard extends StatelessWidget   // title "远程电脑" + count, rows, divider, AddComputerTile, footnote
class ComputerListEmpty extends StatelessWidget  // min-height 520: RemoteEmptyPainter 120×84, title, body, outlined add (key 'remote-add-empty')
class ComputerRow extends StatelessWidget {
  const ComputerRow({required this.computer, required this.presence,
                     required this.viewing, this.compact = false});
  // desktop: tile 40 r11 (AppIcons.remote 20) | name 15/600 + service mono 12 | meta 170 | ComputerStatusPill 86 | button 96×34
  // compact (phone): tile | name + sub with 6 px dot | chevronRight 18; whole row taps
}
class ComputerStatusPill extends StatelessWidget // viewing / online / notSharing / unknown / otherRelay
class AddComputerTile extends StatelessWidget    // dashed (DashedBorder) 56 r14 → openAddComputer(context)
```

Rows come from `RemoteComputerStore.rows(session)` (§3.3): the viewing row
first (including a transient row for an unremembered session), then online,
then a divider, then the rest. Listens to
`Listenable.merge([computers, appStore, desktop.session])`; the viewing row's
meta ("直连 · 16 ms · 60 fps") is a nested `ValueListenableBuilder` on
`desktop.stats`, built only for that row. Row actions: button
`computer-open-<id>` → viewing ? `scope.openViewer()` : `computers.open(c)` +
`scope.openViewer()`; offline → button disabled (desktop) / toast
`remoteNotSharingToast` (phone); trailing `MenuAnchor` `computer-more-<id>`
(desktop) or long-press sheet (phone): `remoteRename` (dialog with name field),
`remoteForget` (`confirmDestructive` → `computers.forget(id)`). Footnote
`remoteRememberNote`. Last-seen text: `formatWhen(DateTime, now, locale)`
("今天 HH:mm" / "昨天 HH:mm" / `DateFormat.MMMd` / `DateFormat.yMd`).

#### 2.1.5 QR (`pages/remote/invite_qr.dart`, `widgets/qr_code.dart`)

```dart
/// A QR code drawn dark-on-light on a white plate whatever the theme, so
/// any scanner reads it. Built with package:qr 3.0.3 (D21).
class QrCodeView extends StatelessWidget {
  const QrCodeView({super.key, required this.data, this.size = 200});
  // QrCode.fromData(data: data, errorCorrectLevel: QrErrorCorrectLevel.M) → QrImage;
  // CustomPainter: 4-module quiet zone, modules snapped to whole logical px.
}
class InviteQrPanel extends StatelessWidget {   // white plate, QrCodeView(200), caption hostQrCaption
  const InviteQrPanel({super.key, required this.invitation});
}
```

A `pbd1.` invite (~140 chars, D3) encodes at about version 7-M, readable at
200 px.

### 2.2 Remote tab — phone (`pages/remote/mobile_remote.dart`)

Same `RemotePage`, `RemoteLayout.phone` body (`ListView`, padding 4/16/24/16,
gap 12), shared `PageFrame` header (D19):

1. `FeaturedComputerCard(computer)` — the viewing computer, else the most
   recently opened (`computers.first`): thumbnail 16:9 `Image.file(
   computers.thumbnailFor(id), gaplessPlayback: true, key: ValueKey(mtime))`
   or placeholder `#1F2328` + `AppIcons.remoteLarge` 24/300; badge
   `remoteLastPicture(when)`; name 17/600; sub `remotePathPermission` /
   `remotePathViewOnly` (stored `lastPath`, `lastControlAllowed`);
   `ComputerStatusPill`; full-width 46 r23 ink button `featured-open`
   (`remoteBackToPicture` when viewing, else `remoteOpen`).
2. `remoteOthers` label + `PhoneComputerList` (a `RaisedSurface` of
   `ComputerRow(compact: true)`, dividers inset 68).
3. `AddComputerButtons`: two dashed 52 r16 (`DashedBorder`):
   `remote-scan` (`AppIcons.scanQr` 18, `remoteScanAdd`, hidden when
   `!InviteScanner.supported`) → `openAddComputer(context, start: scan)`;
   `remote-paste` (`AppIcons.paste` 18, `remotePasteInvite`) →
   `openAddComputer(context, start: paste)` (reads the clipboard at once).
4. Note `remotePhoneNote` 12 `quietText` (the phone's host card stand-in).

Empty: items 3-4 only.

### 2.3 Add computer (`pages/remote/add_computer.dart`, `invite_scanner.dart`)

```dart
enum AddComputerStart { auto, scan, paste }

/// Desktop: a 560-wide dialog. Touch: a bottom sheet that scans first.
Future<void> openAddComputer(BuildContext context,
    {AddComputerStart start = AddComputerStart.auto, RemoteComputer? replacing});

class AddComputerForm extends StatefulWidget {
  const AddComputerForm({super.key, required this.sheet, this.replacing, this.initialText});
}
```

`AddComputerForm` state machine (one per open panel):

```
empty ──text/paste/scan──▶ inspecting ──ok──▶ recognised(info)
                              │                   │ edit text → inspecting
                              └─error──▶ rejected(SETUP_STRING | invalid)
recognised ──Connect──▶ connecting ──returns──▶ close panel; scope.openViewer()
```

- Inspect = `desktop.inspectInvitation(text.trim())` (D3; debounced 250 ms
  after typing, immediate after paste/scan). `DesktopCallException.code ==
  'SETUP_STRING'` → `addSetupString`; other `INVALID_ARGUMENT` →
  `addInvalid`. Validation is never re-implemented in Dart.
- Fields: `InviteField` (key `add-invitation`, 3 lines, mono 13, `cardFieldFill`,
  `autocorrect: false, enableSuggestions: false, smartQuotesType/DashesType:
  disabled`, as `desktop_page.dart:161-171`; once recognised the field shows
  `maskInvitation(text)` read-only with an "edit" affordance; inline
  `add-paste` button `AppIcons.paste` 16); `_RecognisedHost` (key
  `add-recognised`, `AppIcons.verified` 20 signal, service mono 13,
  `addRecognised(fingerprint)`); name (`add-name`, default service without
  `desktop:`); switch `add-remember` (`addRemember`/`addRememberHelp`, default
  **on**; disabled with `addRememberUnavailable` when
  `!computers.supported`, D20); disclosure `addMoreOptions` → switch
  `add-relay-only` (`addRelayOnly`).
- Connect (`add-connect`, reuse `connect`) → `computers.openNew(invitation,
  info, name, relayOnly, remember, quality: prefs.quality, replacing:
  widget.replacing)` → pop → `scope.openViewer()`. Remembering is written by
  the store after the first presented picture (§3.3), never before the TLS pin
  succeeded.
- `replacing` (identity changed → "Paste new invitation", DS §2.7.5): title
  `addReplaceTitle(name)`, name prefilled, remember forced on; the store
  replaces the entry's invitation (never merges, invariant 6).
- Desktop (`!isTouchPlatform`): `showDialog` with `Dialog(child: SizedBox(width:
  560, ...))`, barrier `Color(0x52141416)`, header `addTitle` + close,
  footer Cancel / Connect.
- Touch: `showModalBottomSheet(isScrollControlled: true, constraints:
  maxHeight 0.92 h)`: header + close; until recognised: `InviteScanner`
  (start ≠ paste and supported) · `scanOr` divider · outlined full-width
  `remotePasteInvite`; then the same form fields with a full-width Connect.
  Switches stay visible in both phases (DS §2.9).

```dart
/// Live camera QR reading. Android/iOS only (mobile_scanner 7.5.0, D21):
/// returnImage false, QR only, no duplicates; nothing is stored.
class InviteScanner extends StatefulWidget {
  const InviteScanner({super.key, required this.onDetect});
  final ValueChanged<String> onDetect;
  static bool get supported => debugSupportedOverride ?? (Platform.isAndroid || Platform.isIOS);
  @visibleForTesting static bool? debugSupportedOverride;
}
```

Viewfinder 330 tall r20 `#161618`; `_ReticlePainter` (four L corners, scan
line animated 1.6 s, static under `Motion.reduced`); hint pill `scanHint`;
`errorBuilder` → denied state (`AppIcons.scanQrLarge` 24/300 + `scanDenied`;
paste stays available). The controller is stopped on detect, on sheet close
and on app pause (`AppLifecycleListener`). Footer `scanCameraNote`. Tests set
`debugSupportedOverride = false` (no camera plugin under `flutter test`);
previews draw the static viewfinder via a `debugFakeCamera` flag.

### 2.4 Viewer core (shared)

#### 2.4.1 `DesktopViewer` (`pages/viewer/desktop_viewer.dart`) — root of the mode

```dart
class DesktopViewer extends StatefulWidget { const DesktopViewer({super.key}); }
class _DesktopViewerState extends State<DesktopViewer> with WidgetsBindingObserver {
  late final ViewerController _controller;
  DesktopDemandHandle? _demand;            // DesktopDemand.viewer
}
```

- `initState`: `_controller = ViewerController(store: desktop, prefs:
  desktopPrefs, computers: store.computers)`; `_demand = desktop.demand(
  DesktopDemand.viewer)`; `desktop.setViewerForeground(true)`; touch:
  `SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky)`;
  desktop: `windowManager.setTitle('${target.name} · pb-mapper')`.
- Lifecycle → `_controller.lifecycle(state)`:
  `resumed` → `setViewerForeground(true)`; `inactive` → release local input
  only (window blur, notification shade — video keeps flowing);
  `hidden`/`paused`/`detached` → `releaseLocal()` + `setViewerForeground(false)`
  (D13 pause; the store resumes with a keyframe request).
- `dispose`: `releaseLocal()`, `setViewerForeground(false)`, demand dispose,
  `setFullScreen(false)` if set, restore system UI mode
  (`SystemUiMode.edgeToEdge`) and window title.
- Listens to `desktop.session`: when `target == null` and the phase is not
  `ended` → post-frame `scope.closeViewer()`.
- Builds `Theme(data: viewerTheme(), child: PopScope(canPop: false,
  onPopInvokedWithResult: (_, _) => scope.closeViewer(), child:
  isTouchPlatform(context) ? MobileViewerLayout(controller) :
  DesktopViewerLayout(controller)))`.
- The gesture sheet opens automatically on the first touch session
  (`!prefs.gesturesSeen`).

#### 2.4.2 `ViewerController` (`pages/viewer/viewer_controller.dart`)

A plain class (not a widget), created and disposed by `DesktopViewer`. It is
the only object the layouts call for input.

| Member | Meaning |
|---|---|
| `DesktopStore store`, `DesktopPrefs prefs`, `RemoteComputerStore computers` | collaborators |
| `ValueListenable<bool> live` | `session.inputLive && geometry != null` — the input gate; every send below re-checks it |
| `ModifierLatch modifiers` | D16 latch (§2.6.4) |
| `ValueNotifier<bool> qualityPanel`, `ValueNotifier<ViewScale> scale` | local UI state |
| `GlobalKey<DesktopGestureSurfaceState> surface` | for fit/actual size, minimap, cancelInput |
| `void move(Offset normalized)` | px = floor(n × display) clamped, `store.send(DesktopInput.move(x, y), geometryEpoch: g.epoch)` |
| `void button(RemoteButton b, bool down)` | wraps active modifiers around the first down / last up (§2.6.4) |
| `void click(RemoteButton b, {Offset? at})` | move (if `at`) + down + up as one `sendSequence` |
| `void wheel(double dy, {double dx = 0})` | notches chunked to ±100 per event |
| `void key(RemoteKey k)`, `void chord(Set<RemoteModifier> m, RemoteKey k)` | `sendSequence([m↓…, k↓, k↑, …m↑])` |
| `void text(String s)` | `DesktopInput.text`, split ≤ 1024 bytes |
| `bool onHardwareKey(KeyEvent e)` | `KeyRouting` (§2.6.5) |
| `Future<void> runShortcut(RemoteShortcut s)` | §2.5.4 |
| `void releaseLocal()` | `surface.cancelInput()`, `modifiers.releaseAll()`, `store.release()` if anything was held |
| `Future<void> disconnect()` | `store.disconnect(snapshotTo: computers.snapshotTarget())` (D23, best effort ≤ 300 ms, never blocks) then `scope.closeViewer()` |
| `void setMode(RemotePointerMode m)`, `setQuality(DesktopQuality q)` | prefs + `store.setQuality` (live, D12) |
| `void lifecycle(AppLifecycleState s)`, `dispose()` | §2.4.1 |

Input epochs: the controller listens to `store.geometry` and `store.session`;
any change of geometry epoch, generation, `inputLive` going false or a new
target calls `releaseLocal()` (D16: modifiers release on reconnect). The
gesture surface's `inputEpoch` is `(geometry.epoch, geometry.generation)`.

#### 2.4.3 `ViewerPicture` (`pages/viewer/viewer_picture.dart`)

```dart
class ViewerPicture extends StatelessWidget {
  const ViewerPicture({super.key, required this.controller,
                       this.fitRect, this.obstruction = EdgeInsets.zero});
}
```

- `ValueListenableBuilder(store.textureId)` + `ValueListenableBuilder(
  store.geometry)` (aspect only) → `DesktopGestureSurface(key:
  controller.surface, aspectRatio, enabled: live, trackpad: mode, fitRect,
  obstruction, inputEpoch, sink: controller, child: textureId != null ?
  Texture(key: ValueKey(textureId), textureId, filterQuality: low) :
  _JpegPicture(store))`.
- `_JpegPicture` (iOS/macOS/Linux, D9): listens to `store.jpegFrame`, decodes
  capped to 1600×1000 as today (`desktop_viewer.dart:111-165`) and calls
  `store.acknowledgePresented(...)` post-frame. Only this widget rebuilds per
  JPEG frame.
- `_RemoteCursorLayer`: a `Ticker` that reads `store.latestCursor()` (D11,
  non-blocking) every vsync and forwards to
  `surface.updateRemoteCursor(normalized)` only when it changed and its
  generation equals `geometry.generation`. No rebuild; the surface's
  `_cursorPaint` `ValueNotifier` repaints the cursor alone. Keeps the 500 ms
  local-echo suppression (`desktop_gesture_surface.dart:76-84`).
- `GreyStill`: while `session.phase` is reconnecting and `hasPicture`,
  `Positioned.fill(ClipRect(BackdropFilter(filter: ColorFilter.matrix(
  greyHalf))))` over the texture. Nothing is inserted into the texture's
  subtree, so the `Texture` element is never re-parented, and the filter is
  translucent, so Flutter keeps compositing the texture (`desktop_viewer.dart:492-493`).
- The texture subtree is never rebuilt by session/stats changes: those
  listeners sit in sibling layers.

#### 2.4.4 `DesktopGestureSurface` (moved to `pages/viewer/gesture_surface.dart`)

Kept: mouse/trackpad paths (hover, buttons, wheel, Ctrl+wheel zoom), the
`Transform(key: 'desktop-scene-transform')` over `RepaintBoundary(child)`,
local-echo suppression, `cancelInput`, `didUpdateWidget` releases on
`enabled`/`trackpad`/`inputEpoch` change while keeping scale and offset.

Changed API:

```dart
class DesktopGestureSurface extends StatefulWidget {
  const DesktopGestureSurface({super.key, required this.aspectRatio,
    required this.enabled, required this.trackpad, required this.inputEpoch,
    required this.sink, required this.child,
    this.fitRect,                       // null: fit into the viewport (desktop)
    this.obstruction = EdgeInsets.zero, // soft keyboard + modifier bar (mobile)
  });
  final GestureSink sink;               // replaces onMove/onButton/onWheel/onRelease/onFocus
}
class DesktopGestureSurfaceState extends State<DesktopGestureSurface> {
  ValueListenable<Rect> get visibleRegion;      // normalised [0..1]² part on screen, for Minimap
  ValueListenable<Offset?> get touchIndicator;  // finger position (trackpad), fades 150 ms after lift
  void fitToWindow();                           // scale 1, centred
  void actualSize(int videoWidth, double dpr);  // scale = max(1, videoWidth / dpr / scene.width)
  void cancelInput();
  void updateRemoteCursor(Offset? normalized);
}
```

- **Fit rect (P1-4)**: the surface always fills its parent; the scene is
  fitted to `fitRect` (mobile: the letterboxed picture rect), else to the
  viewport. Soft keyboard, modifier bar and gutter visibility change only
  `obstruction`, never the surface size, so nothing re-fits.
- **Obstruction**: on `obstruction` growing from zero, save `_offset`, then
  pan (scale untouched) so the remote cursor (or the visible centre) lies in
  `viewport.deflate(obstruction)`; on returning to zero, restore the saved
  offset (clamped). Never calls the store.
- **Rotation/resize** re-fit through `_layout` keeping the normalised centre
  and scale (today's behaviour, local only).
- `_scaleUpdate` navigates only for `pointerCount == 2`, or `0` (trackpad
  pan-zoom), or `1` when `!enabled`; never for ≥ 3 (three-finger scroll).
- Touch pointer events are forwarded to a `TouchGestureMachine` (§2.6.7); the
  surface owns one `Timer` and calls `machine.timeout(now)` when it fires.

### 2.5 Desktop viewer

#### 2.5.1 `DesktopViewerLayout` (`pages/viewer/desktop_viewer_layout.dart`)

```
ColoredBox(ViewerPalette.ground)
 Column[
   if (!_fullscreen) ViewerTitleBar(controller)                 // 36
   Expanded(Focus(autofocus, onKeyEvent: controller.onHardwareKey,
                  onFocusChange: lost → controller.releaseLocal(),
     Stack[
       Positioned.fill(ViewerPicture(controller)),
       ViewerStateLayer(controller),                            // overlays §2.5.5
       _ControlHint(controller),                                // ViewerPill, bottom 64, 3 s on control start
       _ViewOnlyPill(controller),                               // bottom-left 12/34
       FloatingToolbar(controller),                             // top 12 / handle top 0
       QualityPanelAnchor(controller),                          // top 68, right 14
     ]))
 ]
```

`_fullscreen` follows `WindowListener.onWindowEnterFullScreen/
onWindowLeaveFullScreen`.

```dart
class ViewerTitleBar extends StatelessWidget {
  // SizedBox(height 36) Stack[Positioned.fill(WindowDragArea()),
  //   Padding(start 8, end 8) Row[
  //     IconButton(key 'viewer-back', AppIcons.back 16, tooltip back → scope.closeViewer()) 32×32,
  //     gap 6, Icon(AppIcons.remote, 16, ViewerPalette.signal), gap 8,
  //     Text(target.name, 13/600), gap 8, Text('pb-mapper', 12.5, textMuted),
  //     Spacer (drag), if (drawsCaptionButtons) CaptionButtons(tone: CaptionTone.viewer)]]
}
```

Caption buttons keep 46×32 and are centred in 36 (2 px above/below, D25).

#### 2.5.2 `FloatingToolbar` (`pages/viewer/floating_toolbar.dart`)

`ValueListenableBuilder<bool>(collapse)` → expanded toolbar (48 tall, r16,
`ViewerPalette.chrome`, 1 px `edge`, shadow `0 8 30 #66000000`, padding 0 6,
gap 2, separators 1×22 white@0.12) or `ToolbarHandle` (72×20 at top 0,
radius 0 0 10 10, `AppIcons.chevronDown` 14; hover or tap → `collapse.expand()`;
key `viewer-toolbar-handle`). Wrapped in `MouseRegion(onEnter:
collapse.pointerEntered, onExit: collapse.pointerLeft)`. Items left to right
(DS §2.6, keys in brackets):

1. Pin `[viewer-pin]` 36×36 `AppIcons.pin` 18, pressed fill white@0.10 when
   pinned → `prefs.setToolbarPinned`, tooltip `viewerPin`/`viewerUnpin`.
2. `ViewerStatusChip` `[viewer-status]` — `ValueListenableBuilder(stats)`:
   dot 8 (`success` / `caution` when `stats.degraded` / grey while
   connecting) + path label + mono "16 ms · 60 fps" (or "画面延迟 420 ms" amber
   when degraded); tap toggles `controller.qualityPanel`.
3. Mode `[viewer-mode]` — `GlassSegmented<bool>(height: 36, expand: false)`:
   `AppIcons.view` `viewerModeView` / `AppIcons.control` `viewerModeControl`
   (segment `enabled: session.controlAllowed`) → `store.control(bool)`.
4. `ViewerMenuButton(key: 'viewer-display', icon: AppIcons.remote, label:
   viewerDisplay, entries: displayMenu(controller))`.
5. `ViewerMenuButton(key: 'viewer-shortcuts', icon: AppIcons.shortcuts,
   label: viewerShortcuts, entries: shortcutsMenu(controller))`.
6. More `[viewer-more]` `AppIcons.more` 18 → `moreMenu(controller)`.
7. Full screen `[viewer-fullscreen]` `AppIcons.fullscreen` /
   `AppIcons.exitFullscreen` 18 → `windowManager.setFullScreen(!_fullscreen)`.
   Esc is not intercepted (it belongs to the remote).
8. Disconnect `[viewer-disconnect]` `AppIcons.disconnect` 16 + `viewerDisconnect`,
   `ViewerPalette.error` → `controller.disconnect()`.

When collapsed and `stats.degraded`, a standalone degraded `ViewerPill` sits
under the handle (DS §2.7.2), so degradation is never hidden.

#### 2.5.3 `ToolbarCollapse` (`pages/viewer/toolbar_collapse.dart`, pure)

```dart
class ToolbarCollapse extends ValueNotifier<bool> {   // value = expanded
  ToolbarCollapse({required bool pinned, Duration delay = const Duration(seconds: 2)});
  set pinned(bool v);            // pinned → expand, cancel timer
  void pointerEntered();         // cancel timer, expand
  void pointerLeft();            // !pinned && holds.isEmpty → start delay timer
  void expand();
  void hold(Object token);       // menu / quality panel / text dialog open: never collapse
  void release(Object token);    // last release → behaves as pointerLeft if pointer is outside
  void pictureShown();           // first presented picture: start the timer if pointer is outside
  @override void dispose();      // cancel timer
}
```

`ViewerMenuButton`/`MenuAnchor(onOpen/onClose)`, the quality panel and the
type-text dialog call `hold`/`release`. Collapse is not persisted (only the
pin is).

#### 2.5.4 Menus (`pages/viewer/viewer_menu.dart`) — shared catalogue

```dart
sealed class ViewerMenuEntry {}
final class ViewerMenuSection extends ViewerMenuEntry { final String label; }
final class ViewerMenuDivider extends ViewerMenuEntry {}
final class ViewerMenuItem extends ViewerMenuEntry {
  final Key key; final String label; final String? hint;   // mono key caps
  final IconData? icon; final bool enabled, checked; final String? disabledHint;
  final FutureOr<void> Function() onSelected;
}

/// Desktop: a toolbar button that opens [entries] in a MenuAnchor styled per DS §2.6.2
/// (300 wide, r14, ViewerPalette.menu, items 36 r9, hover white@0.08).
class ViewerMenuButton extends StatelessWidget { ... }
/// Touch: the same entries as a dark sheet list (max width 420).
Future<void> showViewerMenuSheet(BuildContext context, {required String title, required List<ViewerMenuEntry> entries});

List<ViewerMenuEntry> displayMenu(ViewerController c);
List<ViewerMenuEntry> shortcutsMenu(ViewerController c);
List<ViewerMenuEntry> moreMenu(ViewerController c, {required bool touch});
```

| Menu | Entries |
|---|---|
| Display | section `viewerScaleLabel`: `viewerScaleFit` ✓ / `viewerScaleActual` (local transforms, P1-4) · divider · section `viewerQualityLabel`: `qualityBest` / `qualityBalanced` / `qualitySmooth` (✓ = `store.quality`, → `controller.setQuality`, live, D12) · divider · `qualityTitle` ✓ (toggles panel) |
| Shortcuts | from `RemoteShortcut.values` (below) |
| More (desktop) | `viewerTypeText…` (dark dialog; long text → `controller.text`) · `qualityTitle` ✓ |
| More (touch) | `gestureTitle` (→ `showGestureSheet`) · `viewerModeView`/`viewerModeControl` toggle · quality preference (3 items) · `qualityTitle` ✓ · `shortcutRefresh` · `shortcutRelease` · divider · `viewerDisconnect` (error colour) |

`RemoteShortcut` (`pages/viewer/remote_keys.dart`):

```dart
enum RemoteShortcut {
  showDesktop,   // Win + D        chord({meta}, d)
  switchWindows, // Alt + Tab      chord({alt}, tab)
  taskManager,   // Ctrl+Shift+Esc chord({control, shift}, escape)
  startMenu,     // Win            key(meta)
  lock,          // Win + L        store.lockRemote() (D17 host command; error → toast shortcutLockFailed)
  ctrlAltDel,    // disabled, hint shortcutUnsupported (security boundary)
  refresh,       // store.requestKeyframe()
  releaseKeys;   // controller.releaseLocal()
  bool get needsControl => index <= lock.index;
  String label(AppLocalizations l10n); String? hint;   // hints are literal key caps
}
```

Items needing control are disabled unless `controller.live`. Each chord is one
`sendSequence` (keys are ordering barriers; never interleaved with moves).

#### 2.5.5 Overlays (`viewer_state.dart` pure, `viewer_overlays.dart` widgets)

```dart
enum ViewerOverlayKind { none, connecting, reconnecting, recovering,
                         secureDesktop, waitingDesktop, identityChanged, ended }

ViewerOverlayKind viewerOverlayFor(DesktopSessionView s) => switch (s.phase) {
  DesktopConnectionPhase.identityMismatch => identityChanged,
  DesktopConnectionPhase.ended => ended,
  DesktopConnectionPhase.connecting => connecting,
  DesktopConnectionPhase.reconnecting || DesktopConnectionPhase.retryWaiting => reconnecting,
  DesktopConnectionPhase.waitingForDesktop => s.secureDesktop ? secureDesktop : waitingDesktop,
  DesktopConnectionPhase.recoveringVideo => recovering,
  DesktopConnectionPhase.live || DesktopConnectionPhase.delayed || DesktopConnectionPhase.idle => none,
};
bool showsViewOnly(DesktopSessionView s) => s.target != null && !s.controlAllowed;
```

| ViewerStates artboard | Kind / condition | Widget (all translucent; the texture keeps compositing) |
|---|---|---|
| 正在连接 | `connecting` | `ViewerCard(width 320, leading: spinner 20)` "正在连接 {name}" + `_SetupSteps(s.setup)` (done ✓ / current dot / pending dot / failed `AppIcons.error` + reason) + `Cancel` → `controller.disconnect()` |
| 画面变慢 · 连接质量 | `phase == delayed` / `stats.degraded` | no card: toolbar chip amber; standalone `ViewerPill` when collapsed; `QualityPanel` on demand |
| 等待远程桌面 | `secureDesktop` / `waitingDesktop` | scrim `#B80F0F10` (0.72) + `ViewerCard(360, AppIcons.secureDesktop caution)` `viewerSecureTitle`/`viewerSecureBody` (else `viewerWaitingTitle`/`viewerWaitingBody`) + chip `viewerInputPaused` |
| 正在重连 | `reconnecting` | `GreyStill` (if `hasPicture`) + `ViewerPill(48, spinner)` `viewerRetryIn(n, s)` (countdown from `s.retryAt` by a local 1 s `Timer`, no store notify) or `viewerRetrying(n)` + `viewerInputPaused` + white `viewerRetryNow` → `store.retryNow()` |
| 主机身份已变化 | `identityChanged` | `ViewerCard(380, alert border, AppIcons.warning error)` `viewerIdentityTitle`/`viewerIdentityBody` + `viewerDisconnect` / `viewerPasteNewInvite` → `openAddComputer(context, replacing: computer)` |
| 工具栏收起 · 仅查看 | `showsViewOnly` | `ViewerPill(AppIcons.view)` `viewerViewOnlyByHost`, bottom-left, independent of the toolbar |
| (proposed) recovering | `recovering` | `ViewerPill(spinner 16)` `viewerRecovering`, top centre, no scrim |
| (proposed) ended | `ended` | scrim + `ViewerCard` `viewerEndedTitle` + failure text (`viewerVersionMismatch` for `failure.code == 'VERSION_MISMATCH'`, D1) + `back` / `viewerReconnect` |

```dart
class ViewerStateLayer extends StatelessWidget {  // listens to session + geometry; maps; IgnorePointer except cards/pill buttons
  const ViewerStateLayer({super.key, required this.controller, this.maxCardWidth});
}
class ViewerCard extends StatelessWidget {        // r16, ViewerPalette.card, edge, padding 20/22
  const ViewerCard({super.key, required this.leading, required this.title,
    this.body, this.children = const [], this.actions = const [], this.width = 320, this.alert = false});
}
class ViewerPill extends StatelessWidget {        // chrome fill, edge, r = height/2
  const ViewerPill({super.key, this.leading, required this.text, this.secondary,
    this.trailing, this.height = 32, this.tint});
}
```

#### 2.5.6 `QualityPanel` (`pages/viewer/quality_panel.dart`)

236 wide, r14, `ViewerPalette.panel`, rows (label `textSecondary` | value,
mono where numeric), refreshed by `ValueListenableBuilder(store.stats)` (≤ 1 Hz
by store contract): `qualityPath` (`pathDirect`/`pathRelay` + " · IPv6" when
known) · `qualityRtt` (`rttMs`) · `qualityLatency` (`pictureLatencyMs`, amber
when degraded) · `qualityFps` (`displayFps`) · `qualityBitrate`
(`bitrateBps` → "x.x Mbps") · `qualityResolution` (`width × height`) ·
`qualityCodec` (`qualityHardware(codec)`/`qualitySoftware(codec)`/codec) ·
footer from `stats.adaptation` (`qualityLoweredBitrate`/`qualityLoweredFps`,
hidden for `hold`). Missing values show "—". Desktop anchor: top 68, right 14;
mobile: top-left of the picture rect.

### 2.6 Mobile viewer

#### 2.6.1 `MobileViewerLayout` and `MobileViewerGeometry`

```dart
/// Where the picture and the controls go: the controls take the letterbox.
@immutable class MobileViewerGeometry {
  factory MobileViewerGeometry.of({required Size window, required EdgeInsets padding,
                                   required double aspect});
  final Rect picture;          // fitted, centred
  final Axis gutterAxis;       // horizontal = side gutters (landscape 16:9), vertical = top/bottom bars
  final double gutterExtent;   // (W - H·aspect)/2 or (H - W/aspect)/2, minus padding
  final bool overlaid;         // gutterExtent < 64 → controls overlay the picture edges on viewerHint plates
}
```

```
Scaffold(resizeToAvoidBottomInset: false, backgroundColor: black)
 LayoutBuilder → geometry = MobileViewerGeometry.of(...); keyboard = viewInsets.bottom > 0
 Stack[
   Positioned.fill(ViewerPicture(controller, fitRect: geometry.picture,
        obstruction: keyboard ? EdgeInsets.only(bottom: inset + 44) : EdgeInsets.zero)),
   if (!keyboard) _Gutter(start: [back, quality, mode]) / _Gutter(end: [keyboard, shortcuts, rightClick, more]),
   _ModeHint (ViewerPill, 3 s on open and after mode change),
   Minimap(controller.surface) (top 10, right 12 of the visible region),
   QualityPanelAnchor (top-left of the picture),
   ViewerStateLayer(controller, maxCardWidth: picture.width - 32),
   _ViewOnlyPill,
   if (keyboard || modifiers.any) ModifierBar(controller) at bottom: inset, + modifier ViewerPill,
   if (mode == direct && prefs.virtualMouse) VirtualMouse(controller),
   SoftKeyboardBridge(controller)   // always mounted, offstage
 ]
```

Gutter buttons (`_GutterButton`: 44 circle white@0.10, icon 20, label 10.5;
disabled 40 % when `!live`): back `[viewer-back]` `AppIcons.back` →
`scope.closeViewer()` (session continues, paused, D13) ·
`_QualityGutterButton` `[viewer-status]` (dot + path + mono fps, a11y
`viewerQualityA11y`) → panel · mode `[viewer-mode]` (`AppIcons.trackpad` on
signal tint / `AppIcons.directTouch`; label `viewerTrackpad`/`viewerDirectTouch`;
tap toggles; long press → gesture sheet) · keyboard `[viewer-keyboard]` →
`SoftKeyboardBridge.show()` · shortcuts `[viewer-shortcuts]` →
`showViewerMenuSheet(shortcutsMenu)` · right click `[viewer-right-click]` →
`controller.click(right)` at the cursor (trackpad) / last touch point (direct)
· more `[viewer-more]` → `showViewerMenuSheet(moreMenu(touch: true))`. Each
gutter respects `MediaQuery.padding` (cutouts). Buttons are opaque hit
targets above the surface, so a button press is never also a remote click
(Stack hit-testing stops at the topmost hit; RustDesk needs blocked rects for
the same).

#### 2.6.2 Keyboard handling (P1-4)

The surface keeps its size; showing the keyboard only adds `obstruction`
(fit rect unchanged → no re-fit; scale unchanged; offset panned to keep the
cursor visible, restored on hide — §2.4.4). Gutters hide while the keyboard
is up (the strip is full width). Nothing reaches the store or native code.
Android keeps `adjustResize` and Impeller off; the Scaffold no longer
resizes.

#### 2.6.3 `SoftKeyboardBridge` (`pages/viewer/soft_keyboard.dart`)

An always-mounted, zero-size `EditableText`-backed `TextField` (offstage,
`autocorrect: false`, `enableSuggestions: false`,
`enableIMEPersonalizedLearning: false`, `keyboardType: text`, multiline) whose
controller holds a sentinel of 64 `​` so Backspace always changes the
text (RustDesk's `'1' * 1024` idea, `rustdesk-ux §3.12`). `onChanged` diffs
against the sentinel after the IME composing range is committed: removed →
`key(backspace)` × n; `\n` → `key(enter)`; inserted characters → through
`KeyRouting.forCharacter` (latched modifiers → chord, else `text`); then the
sentinel is restored. `show()` focuses it; collapse (`modifier-bar-hide`)
unfocuses. Physical keyboards go through `controller.onHardwareKey`.

#### 2.6.4 `ModifierLatch` (`pages/viewer/modifier_latch.dart`, pure, D16) and `ModifierBar`

```dart
enum RemoteModifier { control, alt, shift, meta }
enum LatchState { off, once, locked }

class ModifierLatch extends ValueNotifier<Map<RemoteModifier, LatchState>> {
  void tap(RemoteModifier m);         // off→once, once→locked, locked→off
  Set<RemoteModifier> get active;     // once ∪ locked
  /// The events for one key or click, wrapped so nothing stays held:
  /// [m↓ for active] + body + [m↑ reversed]; consumes `once` latches.
  List<DesktopInput> wrap(List<DesktopInput> body);
  void releaseAll();                  // reconnect, viewer close, focus loss, view-only
}
```

A drag with active modifiers sends `m↓` before the button down and `m↑`
after the button up; the latch is consumed at drag end. `releaseLocal()` →
`store.release()` covers a drag interrupted mid-way.

`ModifierBar` (44 tall, `ViewerPalette.card`, top edge, mono 13,
horizontally scrollable, keys `mod-ctrl|alt|shift|win|esc|tab|fn|left|up|down|right|del|ctrl-c|ctrl-v`,
`modifier-bar-hide`): caps 32 r8 white@0.10; `once` = outline signal, `locked` =
filled `#EDEDEF` with ink text (both readable states). `Fn` toggles the middle
groups to F1-F12 (local UI state). The status `ViewerPill` reads
`modifierOnce(keys)` / `modifierLocked(keys)`.

#### 2.6.5 Key routing (`pages/viewer/remote_keys.dart`, pure)

```dart
enum RemoteKey { shift, control, alt, meta, enter, escape, tab, backspace, delete,
  insert, space, left, right, up, down, home, end, pageUp, pageDown,
  f1, …, f12, a, …, z, digit0, …, digit9;
  String get wireName;   // MUST equal the Rust `Key` v2 serde names (D17)
}
abstract final class KeyRouting {
  static RemoteKey? forLogical(LogicalKeyboardKey k);
  static RemoteKey? forCharacter(String c);          // [a-z0-9] only
  /// Hardware key → inputs: non-printables are keys; printables are `text`
  /// unless Ctrl/Alt/Win (held or latched) is active, then a key chord.
  static List<DesktopInput>? route(KeyEvent e, Set<RemoteModifier> latched);
}
```

Replaces `_keyNames` (`desktop_viewer.dart:541-568`). Shift alone keeps
characters as `text` (layout-correct). Ctrl+B, Win+E, F-keys now reach the host
(they were dropped, `ui-pages §2.5`). D1's ALPN bump means every v2 host
understands the full vocabulary; no feature gate.

#### 2.6.6 `Minimap`, `VirtualMouse`, gesture sheet

- `Minimap(surface)`: `ValueListenableBuilder(surface.visibleRegion)`; hidden
  when the region is the whole screen (± 0.5 %); 96 × 96/aspect, r6,
  `rgba(20,20,22,0.8)`, white@0.25 border, region rect 1.5 px signal r2;
  `IgnorePointer`; repaints only on transform change; a11y `viewerMinimap`.
- `VirtualMouse(controller)` (direct touch + `prefs.virtualMouse`, D18):
  floating pad 72×120 r16 `ViewerPalette.chrome`: grip strip (drag moves the
  pad), body (drag → `controller.move` relative, like a trackpad), left /
  right buttons (tap = click, hold = button held until lift), scroll strip
  (vertical drag → `wheel`). Session-local position; opaque, so touches under
  it never reach the surface.
- `showGestureSheet(context, controller)` / `GestureSheet`: title
  `gestureTitle` + close; `GlassSegmented<RemotePointerMode>` (height 44);
  description; seven rows with `GestureGlyph(GestureGlyphKind)` (one
  `CustomPainter`, 24 px box, paths from DS §2.12); info box (direct touch);
  switch `gesture-virtual-mouse` (enabled only in direct touch); `gotIt`.
  Marks `prefs.gesturesSeen`.

#### 2.6.7 `TouchGestureMachine` (`pages/viewer/touch_gestures.dart`, pure, D18)

```dart
enum RemoteButton { left, right, middle }

abstract interface class GestureSink {
  void pointTo(Offset local);                 // direct: absolute
  void moveBy(Offset localDelta);             // trackpad: relative
  void click(RemoteButton b, {Offset? at});   // at = local point (direct) or null (cursor)
  void button(RemoteButton b, bool down, {Offset? at});
  void scroll(int notches);                   // negative = wheel up
  void releaseHeld();
  void haptic();
}

class TouchGestureMachine {
  TouchGestureMachine({required this.sink, required this.trackpad});
  void down(int pointer, Offset p, Duration t);
  void move(int pointer, Offset p, Duration t);
  void up(int pointer, Offset p, Duration t);
  void cancel();
  Duration? get nextTimeout;                  // surface schedules one Timer
  void timeout(Duration t);
}
```

Constants: `kTouchSlop` 18, `kDoubleTapTimeout` 300 ms, `kDoubleTapSlop` 100,
`kLongPressTimeout` 500 ms, drag hold 300 ms, scroll 4 px per notch.

| State | Event | → / output |
|---|---|---|
| idle | 1 down | `pressed(second: false)` |
| tapped(t₀, p₀) (click already sent) | 1 down within 300 ms and 100 px | `pressed(second: true)` |
| tapped | down later/farther, or 300 ms | idle / `pressed(false)` |
| pressed(false) | up < 500 ms, no slop | `click(left)` **immediately** → `tapped` |
| pressed(false) | 500 ms, no slop | `click(right)` + haptic → `longPressed` |
| pressed(true) | up before hold, no slop | `click(left)` (double-click) → idle |
| pressed(true) | moved > slop or held 300 ms | `button(left, down)` → `dragging` |
| pressed(false), trackpad | moved > slop | `moveBy` → `moving` |
| pressed(false), direct | moved > slop | `button(left, down, at: start)` → `directDrag` (absolute `pointTo`) |
| dragging / directDrag | up | `button(left, up)` → idle |
| any | 2nd finger | cancel pending; held → `releaseHeld()`; → `navigating` (no output; the scale recogniser zooms/pans locally) |
| any | 3rd finger | → `scrolling`: vertical Δ integrated, every 4 px → `scroll(±1)` (finger down = content down = wheel up = negative) |
| navigating / scrolling | fingers lift | stay until all are up (no 2→1 jump), → idle |

Direct touch: tap → `click(left, at)`, long press → `click(right, at)`.
Two-finger tap does nothing (D18 drops it). `cancel()` (pointer cancel, epoch
change, mode change) releases held buttons. The surface converts local
points/deltas to normalised coordinates and calls the controller.

### 2.7 Viewer palette (`lib/src/theme/viewer_palette.dart`)

```dart
abstract final class ViewerPalette {
  static const ground = Color(0xFF0F0F10), card = Color(0xFF1C1C1E), menu = Color(0xFF262628),
    chrome = Color(0xE6202023), panel = Color(0xF01C1C1E), hint = Color(0xDB18181A),
    edge = Color(0x14FFFFFF), text = Color(0xFFEDEDEF), textSecondary = Color(0xFFB4B4B8),
    textMuted = Color(0xFF8E8E93), success = Color(0xFF57C27A), caution = Color(0xFFE0A647),
    error = Color(0xFFF2B8B5), signal = Color(0xFF7DB6F2), keyCap = Color(0x1AFFFFFF);
}
/// The dark theme every viewer uses whatever the app theme (DS §2.0).
ThemeData viewerTheme();   // buildAppTheme(Brightness.dark) + menuTheme/tooltip/sheet tuned to ViewerPalette
```

Light additions in `app_theme.dart`: `Color quietText(ColorScheme)` (#6E6E73 /
#8E8E93, help text); `StatusChip(coloredLabel: true)` (`widgets/status.dart`);
`GlassSegment.enabled` (`theme/glass.dart`: disabled ink `onSurfaceMuted`, no
tap); `DashedBorder extends OutlinedBorder` (`widgets/dashed_border.dart`,
dash 4/3, 1.5 px `outline`, usable as `Material(shape:)` so `InkWell` clips).

---

## 3. State

### 3.1 `DesktopStore` UI contract (the perf/FFI designs must match these names)

`DesktopStore` stops extending `ChangeNotifier`; widgets listen to slices.
All value classes are `@immutable` with `==`, so a `ValueNotifier` only
notifies on a real change.

| Member | Type | Rate | Read by |
|---|---|---|---|
| `hostView` | `ValueListenable<DesktopHostView>` | on host change (poll ≤ 1 Hz page / 2 s sharing) | `HostCard`, `RemoteShareDot`, `SharingBarLayer`, tray |
| `session` | `ValueListenable<DesktopSessionView>` | on phase/target/control/setup change | viewer root, overlays, mode segment, list rows, `ViewerController.live` |
| `geometry` | `ValueListenable<DesktopGeometry?>` | on generation/display/video-size change, never per frame | `ViewerPicture` (aspect), controller (pixel mapping), surface `inputEpoch` |
| `stats` | `ValueListenable<DesktopStats>` | ≤ 1 Hz, sampled only while it has listeners | status chip, quality panel, mobile quality button, viewing row meta |
| `textureId` | `ValueListenable<int?>` | per session | `ViewerPicture` |
| `jpegFrame` | `ValueListenable<DesktopFrame?>` | per frame (JPEG platforms only) | `_JpegPicture` |
| `quality` | `ValueListenable<DesktopQuality>` | on change | Display menu |
| `latestCursor()` | `DesktopCursorSample?` | pull, per vsync, non-blocking (D11) | `_RemoteCursorLayer` |
| `demand(DesktopDemand d)` | `DesktopDemandHandle` (`dispose()`) | — | `RemotePage` (`page`), `DesktopViewer` (`viewer`) |

```dart
@immutable class DesktopHostView {
  final bool supported, configured, sharing, enabling, restarting, background, controlAllowed;
  final String? service, invitation, error;
  final List<DesktopDisplay> displays; final int? displayId;
  final DesktopHelperState helper;            // none | ready | stopped | outdated (D2) | unverified
  final DesktopHostViewer? viewer;            // D24
}
@immutable class DesktopHostViewer { final bool controlling; final String? path; final DateTime since; }

@immutable class DesktopSessionView {
  final DesktopTarget? target;                // {service, name, computerId?, relayOnly}
  final DesktopConnectionPhase phase;         // + `ended`
  final DesktopSetupProgress? setup;          // {step: finding|verifying|firstPicture, path?, failedStep?, reason?}
  final int reconnectAttempt; final DateTime? retryAt;
  final bool controlAllowed, controlWanted, controlling, inputLive, secureDesktop, hasPicture;
  final String? path;
  final DesktopFailure? failure;              // {code: IDENTITY_CHANGED|VERSION_MISMATCH|INVALID_INVITATION|HOST_STOPPED|…, message}
}
@immutable class DesktopGeometry { final int epoch, generation, displayWidth, displayHeight, videoWidth, videoHeight; }
@immutable class DesktopStats {
  final String? path, ipFamily, decoder; final bool? hardware;
  final int? rttMs, pictureLatencyMs, bitrateBps; final double displayFps;
  final int width, height; final String codec;
  final DesktopAdaptation adaptation;          // hold | lowerBitrate | lowerFps
  final bool degraded;                        // threshold owned by the perf design (≥ 150 ms picture latency or fps < 50 % target for 2 s)
}
enum DesktopDemand { page, viewer }           // sharing keeps its own slow poll inside the store
```

Methods the UI calls:

| Host | Viewer |
|---|---|
| `enableHost({required int displayId, required bool allowControl, bool ordinaryOnly = false})` | `connect(DesktopConnectRequest r)` — `{invitation? , computerId?, name, relayOnly, quality}` |
| `resumeSharing()` | `disconnect({File? snapshotTo})` (D23 native snapshot, ≤ 300 ms, best effort) |
| `stopHost()` (bypasses `busy`) | `retryNow()` |
| `allowControl(bool)` (D24 live) | `control(bool)` |
| `endViewerSession()` (D15 Disconnect; new host op) | `send(DesktopInput, {required int geometryEpoch})`, `sendSequence(List<DesktopInput>, {required int geometryEpoch})` — the store stamps generation/frame id from its latest presented frame and drops on epoch/generation mismatch |
| `revokeTrust()` | `release()`, `requestKeyframe()`, `lockRemote()` (D17) |
| `helperAction(String)` | `setQuality(DesktopQuality)` (D12 live), `setViewerForeground(bool)` (D13) |
| `setDisplay(int)`, `renameService(String)` | `inspectInvitation(String) → Future<DesktopInvitationInfo>` (`{service, fingerprint, legacy}`, D3) |

Deleted from the public surface: `addListener`/`notifyListeners`,
`connectionPhase`/`retryIn`/`pictureAge`/`presentFps`/`reconnectAttempt`/
`recoveryReason` getters (folded into `session`/`stats`), `frame`,
`videoPresented`/`videoPresentation`, `cursorStatus`, `mediaEpoch`
(→ `geometry.epoch`), `load()` (→ demand), public mutable `error`/`busy`
(→ `hostView.error`/`enabling`), `startHost`, `backgroundAction`.

`DesktopInput` (`state/desktop_input.dart`): `sealed class DesktopInput {
Map<String, Object?> toJson(); }` with `move(x, y)`, `button(RemoteButton,
down)`, `wheel(horizontal, vertical)` (asserts |v| ≤ 100), `key(RemoteKey,
down)`, `text(String)` (asserts ≤ 1024 UTF-8 bytes). Replaces the raw maps
(`desktop_viewer.dart:202-228`).

`DesktopApiClient` additions for the UI (Rust ops named by the FFI design;
`FakeDesktopApi` implements each): `inspectInvitation`, `enableHost(service,
displayId, allowControl, ordinaryOnly)`, `endViewerSession`, `setDisplay`,
`lockRemote`, `setQuality`, `computers`, `saveComputer`, `updateComputer`,
`forgetComputer`; `DesktopCapabilities.rememberComputers` (bool + reason).
The Android Keystore hop (D20) lives inside `DesktopApi`, never in stores or
widgets. `DesktopVideoTexture.snapshot(path, maxWidth)` on the
`pb_mapper/desktop_video` channel (D23).

### 3.2 Notification scoping (D14)

- `AppStore.desktop` drops `..addListener(_notify)` (`app_store.dart:42`);
  `activeDesktop` is deleted (its only user was the banner, `app.dart:327`).
- Polling: interval = the fastest live demand — `viewer` 250 ms, `page` 1 s,
  host sharing with no demand 2 s (to see a viewer arrive for the bar and
  tray), nothing → stopped. `stats` sampling runs only while `stats` has
  listeners.

| Widget | Listens to | Never listens to |
|---|---|---|
| Overview, Tunnels, Relay, Logs, Settings, rail foot | `AppStore` (unchanged) | anything desktop |
| `RemoteShareDot`, `SharingBarLayer`, tray | `desktop.hostView` | session, stats |
| `HostCard` | `desktop.hostView` | session, stats |
| computer list | `computers`, `AppStore`, `desktop.session`; viewing row meta: `desktop.stats` | hostView |
| `DesktopViewer` root | `desktop.session` (target null → close) | stats |
| `ViewerPicture` | `textureId`, `geometry` | session, stats |
| cursor layer | ticker + `latestCursor()` | everything |
| overlays, mode segment, view-only pill | `desktop.session` | stats |
| status chip, `QualityPanel`, mobile quality button | `desktop.stats` | session |

### 3.3 `RemoteComputerStore` (`lib/src/state/remote_computers.dart`)

```dart
@immutable class RemoteComputer {
  final String id, name, service, fingerprint, relay;
  final int namespace; final bool relayOnly, hasThumbnail;
  final String? lastPath; final DateTime? lastConnectedAt; final bool? lastControlAllowed;
}
enum ComputerPresence { online, offline, unknown, otherRelay }
ComputerPresence presenceOf(RemoteComputer c, AppStore app);
//  otherRelay: c.relay != app.config.serverAddress (or namespace ≠ ownNamespace for a temporary credential)
//  unknown: relay not online; online: app.relayServices has (service, namespace) (D22); else offline

class RemoteComputerStore extends ChangeNotifier {   // owned lazily by AppStore as `computers`; no bridge
  RemoteComputerStore({required DesktopApiClient api, required DesktopStore desktop});
  bool get supported; String? get unsupportedReason;       // D20: Windows + Android only
  List<RemoteComputer> get computers;                       // lastConnectedAt desc
  List<ComputerRowData> rows(DesktopSessionView s);         // + transient unremembered viewing row
  Future<void> load();
  Future<void> open(RemoteComputer c, {required DesktopQuality quality});   // connect by id (secret stays in Rust)
  Future<void> openNew({required String invitation, required DesktopInvitationInfo info,
      required String name, required bool relayOnly, required bool remember,
      required DesktopQuality quality, RemoteComputer? replacing});
  Future<void> rename(String id, String name);
  Future<void> forget(String id);                           // deletes the thumbnail too
  File thumbnailFor(String id);                             // app support dir / desktop-thumbnails/<id>.jpg
  File? snapshotTarget();                                   // current session, remembered, picture live, no mismatch
}
```

It listens to `desktop.session`: first `hasPicture && inputLive|live` of an
`openNew(remember: true)` session → `saveComputer` (D20, after the pin
succeeded) and sets `target.computerId`; a session ending → `updateComputer(
lastPath, lastConnectedAt, lastControlAllowed, hasThumbnail)`. An identity
mismatch never updates the entry. Store failures turn Remember off with a
reason; there is no plaintext fallback.

### 3.4 `DesktopPrefs` (`lib/src/state/desktop_prefs.dart`)

Separate from `AppPrefs` because `AppPrefs` notifies `_onPrefs` →
`setState` on the app root (`app.dart:181-187`): a viewer toggle must not
rebuild the whole app.

| Field | Key | Default |
|---|---|---|
| `bool toolbarPinned` | `desktop_toolbar_pinned` | false |
| `RemotePointerMode pointerMode` (`trackpad`, `direct`) | `desktop_pointer_mode` | trackpad (D18) |
| `bool virtualMouse` | `desktop_virtual_mouse` | false |
| `bool gesturesSeen` | `desktop_gestures_seen` | false |
| `DesktopQuality quality` (`best`, `balanced`, `smooth`; wire names per the perf design) | `desktop_quality` | balanced (D12) |

`DesktopPrefs({this.persist = true})`, `load()`, one setter per field; same
try/catch pattern as `AppPrefs`. `PbMapperApp` takes `DesktopPrefs?
desktopPrefs` for tests.

### 3.5 `AppStore`

`late final RemoteComputerStore computers`; `desktop` without the bridge;
`remoteServices` skips `isDesktopService` (§1.7). `start()` still calls
`desktop.restoreOnStartup()` (host resume is independent of pages).

---

## 4. Icons (requirement 3)

### 4.1 `AppIcons` (`lib/src/theme/app_icons.dart`)

Doc comment: "Lucide (ISC). Weight follows size: up to 20 px at 400, 24 px and
up at 300 (the `…Large` constants). Sizes: 16 inline, 18 buttons and menus,
20 rail and bar, 24 tiles and large empty states. Colour follows text; state
is a coloured dot."

- Every existing constant moves `…300` → `…400` (`…300Dir` → `…400Dir` for
  `chevronRight`, `back`, `forward`).
- Swaps: `tunnels = LucideIcons.route400`, `online = cloud400`,
  `offline = cloudOff400`.
- Deleted (0 uses): `latency`, `list`, `language`. `check` stays (Display
  menu, helper-ready row).
- New (all verified in `lucide_icons_flutter-3.1.25`, codepoints per DS §2.13):

| Constant | Glyph | Where |
|---|---|---|
| `remote` | `monitor400` | rail/bar entry, list tiles, Display button, viewer title |
| `startSharing` | `screenShare400` | paused host "Start sharing" |
| `stopSharing` | `screenShareOff400` | sharing bar Stop |
| `devices` | `monitorSmartphone400` | host viewer panel (device unknown) |
| `view` | `eye400` | View mode, view-only pill (same glyph as `show`, named for the meaning) |
| `control` | `mousePointer2400` | Control mode |
| `keyboard` | `keyboard400` | mobile keyboard, control hint |
| `shortcuts` | `command400` | Shortcuts (both viewers) |
| `rightClick` | `mouse400` | mobile right click |
| `trackpad` | `touchpad400` | mode toggle |
| `directTouch` | `pointer400` | mode toggle |
| `qrCode` | `qrCode400` | host QR button |
| `scanQr` | `scanQrCode400` | scan to add |
| `fullscreen` | `maximize400` | toolbar |
| `exitFullscreen` | `minimize400` | toolbar in full screen (one extra glyph beyond the 19; same family) |
| `pin` | `pin400` | toolbar pin |
| `disconnect` | `power400` | Disconnect |
| `verified` | `shieldCheck400` | Enable remote control, recognised host |
| `secureDesktop` | `lockKeyhole400` | secure-desktop card |
| `quality` | `gauge400` | quality panel entry |
| `phone` | `smartphone400` | reserved for a known phone viewer |
| `paste` | `clipboardPaste400` | paste buttons (same glyph as `import`) |

- Large (300) constants, only for ≥ 24 px: `relayLarge = radioTower300`
  (`galaxy/galaxy_panel.dart:152`, 28 px), `remoteLarge = monitor300` (host
  tile, thumbnail placeholder), `scanQrLarge = scanQrCode300` (camera denied).
  Separate `const` constants, not a runtime weight swap: a non-const
  `IconData` breaks icon-font tree shaking in release builds.
- `EmptyNote` stays 20 px/400 (an inline note row, not a large empty state).

### 4.2 Sizes and theme

- Rail 18 → 20 (`navigation.dart:147`); bar 20 (`:296`) unchanged.
- `_base` `iconButtonTheme` (`app_theme.dart:397-403`) adds `iconSize: 20`
  (touch); `desktopize` keeps 18 (`:611-616`). `iconTheme` default 20 stays.
- Viewer toolbar buttons 18, segment/menu icons 16, gutter buttons 20,
  modifier-bar collapse 18.

### 4.3 Every Material `Icons.*` (18 references, `ui-pages` Appendix A)

| Today | Replacement |
|---|---|
| `window_chrome.dart:111,117(×2),126` `remove`/`filter_none`/`crop_square`/`close` | self-drawn `CaptionGlyph` (§5.2) |
| `tunnels_page.dart:37` `desktop_windows_outlined` | deleted (rail entry) |
| `desktop_page.dart:102,108,136,189,324` | deleted with the file; new code uses `copy`, `stopSharing`, `startSharing`/`verified`, `remote` |
| `desktop_viewer.dart:347,352,357,368,379,407` | deleted with the file; `keyboard`, `rightClick`, three-finger scroll, `RemoteShortcut.releaseKeys` |
| `desktop_connection_banner.dart:55,61` | deleted; overlays use dots and `ViewerCard` icons |

Implicit Material glyphs (`AppBar` back, `ExpansionTile`, `DropdownButtonFormField`,
`desktop_page.dart:57,194,207`, `desktop_viewer.dart:261`) disappear with the
rewrite; the new display pickers use `MenuAnchor` with an explicit
`AppIcons.chevronDown`. `uses-material-design: true` stays (harmless, tree
shaken). `gen_poster_test.dart:110-140` switches `LucideIcons.*300` to
`AppIcons`.

### 4.4 Test fonts (`test/support.dart:111-124`)

Add `family('packages/lucide_icons_flutter/Lucide400',
['packages/lucide_icons_flutter/assets/build_font/LucideVariable-w400.ttf'])`;
keep `Lucide300` (large constants). Delete the `MaterialIcons` loader: with
no Material glyph left, a stray one shows up as a box in previews, which is
the signal we want.

---

## 5. Title bar (requirement 4)

### 5.1 `page_frame.dart` `_TitleBar`

```dart
padding: EdgeInsetsDirectional.only(start: lead, end: drawsCaptionButtons ? 16 : 8),  // :120
...
if (drawsCaptionButtons) ...[
  const SizedBox(width: 16),        // :167, was 10
  const CaptionButtons(),           // :168, fixed geometry, no height
],
```

- The Row is centred in the 58 px (54 compact) bar by the existing
  `Stack(alignment: centerStart)`, so the 32 px buttons sit 13 px from top and
  bottom.
- `PageFrame.onBack` (§1.3): when set, a 36×36 `IconButton(key:
  Key('page-back'), tooltip: l10n.back, icon: Icon(AppIcons.back, size: 18))`
  precedes the title and `lead` drops by 10.
- Acceptance holds by construction: close button's right edge = sheet edge −
  16 at any scale, maximised or restored.

### 5.2 `window_chrome.dart`

```dart
const Size kCaptionButtonSize = Size(46, 32);
const double kCaptionButtonRadius = 6;

enum CaptionTone { surface, viewer }
enum CaptionGlyph { minimize, maximize, restore, close }

class CaptionButtons extends StatefulWidget {
  const CaptionButtons({super.key, this.tone = CaptionTone.surface});
}
// _CaptionButtonsState with WindowListener: as today (isMaximized sync, onWindowMaximize/Unmaximize)
// builds Row(min)[ _CaptionButton(key: 'caption-minimize', glyph: minimize),
//   _CaptionButton(key: 'caption-maximize', glyph: _maximized ? restore : maximize),
//   _CaptionButton(key: 'caption-close', glyph: close, close: true) ]  — close still windowManager.close()

class _CaptionButton extends StatefulWidget { glyph, tooltip, onTap, tone, close }
// AnimatedContainer(90 ms, width 46, height 32, radius 6, color: bg) → _CaptionGlyphBox(glyph, color: fg)
```

Colours:

| Tone | Rest fg | Hover bg / fg | Close hover |
|---|---|---|---|
| surface | `onSurfaceVariant` | `softWash(scheme)` / `onSurface` | `#C42B1C` / white |
| viewer | `ViewerPalette.textSecondary` | white@0.10 / `ViewerPalette.text` | `#C42B1C` / white |

**Glyph (self-drawn, 10 px box, 1 px stroke, pixel-snapped).** A leaf
render object, so it can see its global position:

```dart
class _CaptionGlyphBox extends LeafRenderObjectWidget { glyph, color }  // size 10×10
class _RenderCaptionGlyph extends RenderBox {
  // paint(context, offset):
  //   dpr = devicePixelRatio of the view
  //   g = localToGlobal(Offset.zero) * dpr                       // device px
  //   snap = Offset(g.dx.roundToDouble() - g.dx, g.dy.roundToDouble() - g.dy) / dpr
  //   s = math.max(1, dpr.floor()) / dpr                         // stroke: 1 device px at 100/125/150 %, 2 at 200 %
  //   box = (10 * dpr).roundToDouble() / dpr
  //   canvas.translate(offset + snap); then, in a box×box frame:
  //   minimize: fillRect(0, snapY(box/2) - s/2, box, s)
  //   maximize: strokeRRect(rect(s/2, s/2, box - s, box - s), radius 1, width s)
  //   restore:  front strokeRRect(s/2, 2.5*k + s/2, 7*k - s, 7*k - s, r1)
  //             + back path M2.5,2.5 V1.5 a1 1 0 0 1 1-1 h5 a1 1 0 0 1 1 1 v5 a1 1 0 0 1-1 1 h-1  (k = box/10)
  //   close:    two lines (s/2,s/2)→(box-s/2,box-s/2) and (box-s/2,s/2)→(s/2,box-s/2), butt caps, antialias
  //   Horizontal/vertical edges land on device-pixel centres (+0.5 px for odd widths);
  //   diagonals stay antialiased (no exact snapping exists for them).
}
```

Assumes no rotation or scale between the glyph and the window (true for title
bars). `iconSize`/`height` parameters are deleted. Tooltips
(`windowMinimize/Maximize/Restore/Close`) stay. The listener also handles
`onWindowEnterFullScreen/LeaveFullScreen` so a restored window shows the right
glyph after leaving full screen.

### 5.3 Other users

- `SetupFlow` (`pages/setup_flow.dart:196-201`): `Positioned(top: 6, right: 16,
  child: CaptionButtons())` — top 6 centres 32 px in its 44 px drag strip
  (6 + 32 + 6); 16 px from the window edge as in the shell.
- Viewer: `CaptionButtons(tone: CaptionTone.viewer)` in the 36 px
  `ViewerTitleBar` (§2.5.1).
- `docs/ui-design.md` caption sizes (`:193-195`).

---

## 6. Localisation (`lib/l10n/app_en.arb`, `app_zh.arb`)

Rules: all new keys in one block per area, placed after the existing `nav*`
and `window*` groups (not at the file's top/end as PR #51 did). zh uses
**共享** throughout (DS Appendix A). Placeholders typed in the en ARB
(`@key` metadata).

### 6.1 Delete — all 93 `desktop*` keys

The 42 unused (`ui-pages §4.6`): `desktopBackgroundActive, BackgroundHelp,
BackgroundStartup, BackgroundStopped, BackgroundTitle, Busy, Connected,
HelperEnable, HelperHelp, HelperMaintenance, HelperRestart, HelperRunning,
HelperStopped, HelperTitle, HelperUse, HostUnsupported, Interact, Invitation,
InvitationHelp, Navigate, OrdinaryOnly, Preview, SecureActive,
SecureSetupReady, SecureSetupRepair, SecureSetupRequired, SecureSetupStopped,
SecureSetupTitle, ShareNeedsSetup, ShowInvitation, TrustControl,
TrustOtherRelay, TrustRemember, TrustRememberHelp, TrustRestore,
TrustStartup, TrustStartupEnabled, TrustStartupHelp, TrustTitle, TrustView,
ViewOnly, Waiting` — and the 51 used ones, whose widgets are rewritten
(`desktopAckLatency … desktopTrustRevoke`, `ui-pages §4.6`). Where the old text
is still right it is **carried** into a new key (column "from" below);
nothing keeps the `desktop` prefix. `desktopGestureHelp` (old gestures) has no
successor: the gesture sheet replaces it.

### 6.2 Reuse unchanged

`cancel, close, copy, copied, connect, refresh, more, retry, delete,
moreOptions, pathDirect, pathRelay, spanSeconds, spanMinutes, spanHours,
timeAgo, justNow, navLogs, relayLatency, windowMinimize, windowMaximize,
windowRestore, windowClose`.

### 6.3 New keys

Shell and settings

| Key | en | zh | from |
|---|---|---|---|
| navRemoteDesktop | Remote desktop | 远程桌面 | desktopTitle |
| navRemote | Remote | 远程 | |
| back | Back | 返回 | |
| settingsLogsBody | Read the app's log | 查看应用日志 | |

Remote page and list

| Key | en | zh | from |
|---|---|---|---|
| remoteAddComputer | Add computer | 添加电脑 | |
| remoteListTitle | Remote computers | 远程电脑 | |
| remoteEmptyTitle | No remote computers yet | 还没有远程电脑 | |
| remoteEmptyBody | Turn on remote control in pb-mapper on the other computer, copy its private invitation and paste it here. | 在另一台电脑的 pb-mapper 里开启远程控制，复制它的私密邀请，粘贴到这里。 | |
| remoteAddHint | Paste another computer's private invitation | 粘贴另一台电脑的私密邀请 | |
| remoteRememberNote | Only invitations you choose to remember are saved on this device, encrypted by the system. | 只有选择“记住此电脑”的邀请会保存在本机，并由系统加密。 | |
| remoteViewing | Viewing | 正在查看 | |
| remoteOnline | Online | 在线 | |
| remoteNotSharing | Not sharing | 未共享 | |
| remoteUnknown | Status unknown | 状态未知 | |
| remoteOtherRelay | On another relay | 属于其他中继 | |
| remoteBackToPicture | Back to picture | 回到画面 | |
| remoteOpen | Open | 打开 | desktopOpen |
| remoteLastSeen | Last {when} | 上次 {when} | |
| remoteLastSeenPath | Last {when} · {path} | 上次 {when} · {path} | |
| remoteToday | Today {time} | 今天 {time} | |
| remoteYesterday | Yesterday {time} | 昨天 {time} | |
| remoteRename | Rename | 重命名 | |
| remoteForget | Remove this computer | 移除此电脑 | |
| remoteForgetTitle | Remove {name}? | 移除“{name}”？ | |
| remoteForgetBody | The saved invitation and last picture are deleted from this device. | 保存的邀请和上次画面会从本机删除。 | |
| remoteNotSharingToast | {name} isn't sharing right now | {name} 当前未共享 | |
| remoteOthers | Other computers | 其他电脑 | |
| remoteLastPicture | Last picture · {when} | 上次画面 · {when} | |
| remotePathPermission | {path} · can view and control | {path} · 可以查看和控制 | |
| remotePathViewOnly | {path} · view only | {path} · 仅查看 | |
| remoteOnlineLastPath | Online · last via {path} | 在线 · 上次走{path} | |
| remoteScanAdd | Scan to add | 扫码添加 | |
| remotePasteInvite | Paste invitation | 粘贴邀请 | |
| remotePhoneNote | This phone can view and control Windows computers; it can't be controlled itself. | 手机可以查看并控制 Windows 电脑，暂不能被远程控制。 | |

Host card

| Key | en | zh | from |
|---|---|---|---|
| hostThisComputer | This computer | 这台电脑 | |
| hostOff | Remote control is off | 尚未开启远程控制 | |
| hostSharing | Sharing | 正在共享 | |
| hostViewerControlling | 1 viewer in control | 1 个查看者正在控制 | |
| hostViewerWatching | 1 viewer watching | 1 个查看者正在查看 | |
| hostViewerMeta | {path} · connected {span} | {path} · 已连接 {span} | |
| hostMakeViewOnly | Make view-only | 改为仅查看 | |
| hostPermissionLabel | People with the invitation can | 持有邀请的人可以 | |
| hostPermissionView | View only | 仅查看画面 | |
| hostPermissionControl | View and control | 查看并控制 | |
| hostInvitation | Private invitation | 私密邀请 | desktopInvitation |
| hostQr | QR code | 二维码 | |
| hostQrHide | Hide QR code | 隐藏二维码 | |
| hostQrCaption | Scan with your phone to add this computer | 用手机扫描，添加这台电脑 | |
| hostInviteHelp | Send it only to devices you trust. Anyone holding it can watch, and control when you allow it. | 只发给你信任的设备。持有邀请的人可以查看，允许控制时也可以操作。 | |
| hostDisplay | Shared display | 共享的显示器 | |
| hostDisplayValue | Display {n} · {width} × {height} | 显示器 {n} · {width} × {height} | |
| hostBackground | Runs in background | 后台运行 | |
| hostBackgroundService | Windows service · keeps sharing after the window closes | Windows 服务 · 关闭窗口后继续共享 | |
| hostBackgroundApp | With the app · keeps sharing in the tray | 随应用运行 · 关闭窗口后在托盘继续共享 | |
| hostSecure | UAC and lock screen | UAC 与锁屏 | |
| hostHelperReady | Helper service ready | 辅助服务已就绪 | |
| hostHelperStopped | Helper service is stopped | 辅助服务已停止 | desktopHelperStopped |
| hostHelperNone | Ordinary desktop only | 仅普通桌面 | desktopOrdinaryOnly |
| hostHelperOutdated | Helper service needs an update | 需要更新辅助服务 | |
| hostHelperUpdate | Update | 更新 | |
| hostStop | Stop sharing | 停止共享 | desktopStop (分享→共享) |
| hostStart | Start sharing | 开启共享 | |
| hostStopNote | The invitation is kept and works again when you start sharing | 停止后邀请保留，下次开启继续可用 | |
| hostServiceName | Service name… | 桌面服务名称… | desktopService |
| hostServiceNameBusy | Stop sharing to rename | 停止共享后才能改名 | |
| hostHelperInstall | Install / update and restart app | 安装／更新并重启应用 | desktopHelperInstall |
| hostHelperDisable | Disable helper | 停用辅助服务 | desktopHelperDisable |
| hostHelperUninstall | Uninstall helper | 卸载辅助服务 | desktopHelperUninstall |
| hostRevoke | Revoke invitation and stop sharing | 撤销邀请并停止共享 | desktopTrustRevoke (分享→共享) |
| hostRevokeTitle | Revoke the invitation? | 撤销邀请？ | |
| hostRevokeBody | Everyone holding it loses access, and computers that remembered it will see that this computer's identity changed. Sharing again creates a new invitation. | 持有邀请的人都会失去访问权限，记住这台电脑的设备会看到“主机身份已变化”。再次共享时会生成新邀请。 | |
| hostUnsupported | Sharing currently requires Windows. You can view remote computers on this device. | 目前仅 Windows 可共享桌面。此设备可以查看远程电脑。 | desktopHostUnsupported |
| hostSetupHeadline | See and control this computer from your phone or another computer, anytime | 用手机或另一台电脑，\n随时看到并操作这台电脑 | |
| hostSetupStep1Title / Body | Windows asks for administrator approval once / Installs the protected helper so UAC prompts and the lock screen work remotely. | Windows 请求一次管理员批准 / 安装受保护的辅助服务，远程时也能操作 UAC 提示和锁屏。 | |
| hostSetupStep2Title / Body | The app restarts and comes back here / Your tunnels and settings stay as they are. | 应用自动重启并回到这里 / 已有的隧道和配置保持不变。 | |
| hostSetupStep3Title / Body | You get a private invitation / Copy it to another computer or scan it with a phone. It works until you revoke it. | 得到一条私密邀请 / 复制给另一台电脑，或让手机扫码。之后一直有效，直到你撤销。 | |
| hostSetupAllowControl | Allow remote mouse and keyboard | 允许远程操作鼠标和键盘 | |
| hostEnable | Enable remote control | 开启远程控制 | desktopEnable |
| hostEnabling | Turning on… | 正在开启… | |
| hostOrdinaryOnly | Share the ordinary desktop only (no helper service) | 只共享普通桌面（不安装辅助服务） | |
| hostBackgroundNote | Keeps sharing in the background after the window closes | 关闭窗口后仍在后台共享 | |

Add computer and scan

| Key | en | zh | from |
|---|---|---|---|
| addTitle | Add a remote computer | 添加远程电脑 | |
| addReplaceTitle | New invitation for {name} | 为“{name}”粘贴新邀请 | |
| addPaste | Paste | 粘贴 | |
| addRecognised | Host identity recognised · {fingerprint} · via the current relay | 已识别主机身份 · {fingerprint} · 通过当前中继连接 | |
| addSetupString | This is a pb-mapper setup string, not a remote desktop invitation. Paste it in Settings instead. | 这是 pb-mapper 的配置字符串，不是远程桌面邀请。请到设置里粘贴。 | |
| addInvalid | This isn't a valid remote desktop invitation | 这不是有效的远程桌面邀请 | |
| addName | Name | 名称 | |
| addRemember | Remember this computer | 记住此电脑 | |
| addRememberHelp | Saved encrypted on this device; remove it anytime | 邀请加密保存在本机，可以随时移除 | |
| addRememberUnavailable | This device can't store invitations securely, so it won't remember this computer | 这台设备无法安全保存邀请，因此不会记住这台电脑 | |
| addMoreOptions | More options (relay only) | 更多选项（仅使用中继） | |
| addRelayOnly | Use relay only | 仅使用中继 | desktopRelayOnly |
| scanHint | Point at the QR code on the computer's Remote desktop page | 对准电脑“远程桌面”页上的二维码 | |
| scanOr | or | 或 | |
| scanCameraNote | The camera only reads the QR code; nothing is saved or uploaded. | 相机只用来识别二维码，画面不会保存或上传。 | |
| scanDenied | Camera access is needed to scan | 需要相机权限才能扫码 | |

Viewer

| Key | en | zh | from |
|---|---|---|---|
| viewerPin / viewerUnpin | Pin toolbar / Unpin toolbar | 固定工具栏 / 取消固定工具栏 | |
| viewerExpand | Show toolbar | 展开工具栏 | |
| viewerModeView / viewerModeControl | View / Control | 查看 / 控制 | |
| viewerDisplay | Display | 显示 | |
| viewerShortcuts | Shortcuts | 快捷键 | |
| viewerFullscreen / viewerExitFullscreen | Full screen / Exit full screen | 全屏 / 退出全屏 | |
| viewerDisconnect | Disconnect | 断开 | desktopDisconnect |
| viewerControlHint | Keyboard and mouse go to {name} | 键盘和鼠标正发往{name} | |
| viewerControlHintSub | Send system shortcuts from the toolbar | 系统快捷键请从工具栏发送 | |
| viewerTypeText | Type text… | 输入文字… | desktopKeyboard |
| viewerSendText | Send | 发送 | desktopSendText |
| viewerScaleLabel / viewerScaleFit / viewerScaleActual | Scale / Fit to window / Actual size | 缩放方式 / 适应窗口 / 原始大小 | |
| viewerQualityLabel | Picture preference | 画质偏好 | |
| qualityBest / qualityBalanced / qualitySmooth | Best picture / Balanced / Smoothest | 画质优先 / 平衡 / 流畅优先 | |
| qualityTitle | Connection quality | 连接质量 | |
| qualityPath / qualityRtt / qualityLatency / qualityFps / qualityBitrate / qualityResolution / qualityCodec | Path / Network round trip / Picture delay / Display rate / Bitrate / Resolution / Codec | 路径 / 网络往返 / 画面延迟 / 显示帧率 / 码率 / 分辨率 / 编码 | |
| qualityHardware / qualitySoftware | {codec} hardware / {codec} software | {codec} 硬件 / {codec} 软件 | |
| qualityMbps / qualityFpsValue | {value} Mbps / {n} fps | {value} Mbps / {n} fps | |
| qualityLoweredBitrate | Bitrate lowered automatically to keep input responsive | 已自动降低码率，优先保证操作跟手 | |
| qualityLoweredFps | Frame rate lowered until the network recovers | 已降低帧率，等待网络恢复 | |
| viewerPathDelay | {path} · picture delay | {path} · 画面延迟 | |
| shortcutShowDesktop / SwitchWindows / TaskManager / Start / Lock | Show desktop / Switch windows / Task Manager / Start menu / Lock remote computer | 显示桌面 / 切换窗口 / 任务管理器 / 开始菜单 / 锁定远程电脑 | |
| shortcutUnsupported | Not supported | 暂不支持 | |
| shortcutRefresh | Refresh picture | 刷新画面 | |
| shortcutRelease | Release all keys | 释放所有按键 | desktopRelease |
| shortcutLockFailed | Couldn't lock the remote computer: {reason} | 无法锁定远程电脑：{reason} | |
| viewerConnecting | Connecting to {name} | 正在连接 {name} | |
| viewerStepFind / viewerStepDirect / viewerStepRelay | Finding the host / Host found, going direct / Host found, going through the relay | 正在寻找主机 / 找到主机，走直连路径 / 找到主机，走中继路径 | |
| viewerStepIdentity / viewerStepPicture | Checking the host's identity / Waiting for the first picture | 校验主机身份 / 等待第一帧画面 | |
| viewerSecureTitle | The remote computer is on its secure desktop | 远程电脑正在显示安全桌面 | |
| viewerSecureBody | A UAC prompt or the lock screen is showing. Without the helper service it can't be shown; viewing resumes when the ordinary desktop returns. | 出现了 UAC 提示或锁屏。这台电脑没有启用辅助服务，无法显示这一画面；恢复到普通桌面后自动继续。 | |
| viewerWaitingTitle / viewerWaitingBody | Waiting for the remote desktop / Waiting for the host desktop to become available | 等待远程桌面 / 等待被控端桌面恢复可用 | body: desktopStateWaitingDesktop |
| viewerInputPaused | Input paused | 已暂停输入 | |
| viewerRetryIn | Connection lost — retry {n} in {s} s | 连接中断，{s} 秒后第 {n} 次重连 | |
| viewerRetrying | Reconnecting, attempt {n}… | 正在第 {n} 次重连… | |
| viewerRetryNow | Retry now | 立即重试 | |
| viewerIdentityTitle | The host identity has changed | 主机身份已变化 | desktopIdentityMismatch |
| viewerIdentityBody | This computer no longer matches the invitation, so reconnecting stopped. Ask for a new invitation and stop using the old one. | 这台电脑的身份与邀请里记录的不一致，已停止自动重连。请向对方要一条新的邀请，不要继续使用旧邀请。 | |
| viewerPasteNewInvite | Paste new invitation | 粘贴新邀请 | |
| viewerViewOnlyByHost | View only · control not allowed | 仅查看 · 对方未允许控制 | |
| viewerRecovering | Recovering the picture | 正在恢复画面 | |
| viewerEndedTitle / viewerReconnect | The connection ended / Reconnect | 连接已结束 / 重新连接 | |
| viewerVersionMismatch | The other computer runs an older pb-mapper. Update both computers. | 对方的 pb-mapper 版本较旧，请把两台电脑都更新。 | |
| viewerQualityA11y | Connection quality: {path}, {fps} fps | 连接质量：{path}，{fps} 帧每秒 | |
| viewerModeA11y | Switch input mode, now {mode} | 切换操作方式，当前为{mode} | |
| viewerTrackpad / viewerDirectTouch | Trackpad / Direct touch | 触控板 / 直接触摸 | desktopTrackpadMode / desktopTouchMode |
| viewerTrackpadHint | Trackpad: slide one finger to move, tap to click | 触控板模式：单指滑动移动光标，轻点单击 | |
| viewerDirectHint | Direct touch: tap where you want to click | 直接触摸模式：点到哪里就点击哪里 | |
| viewerKeyboard / viewerRightClick / viewerHideKeyboard | Keyboard / Right click / Hide keyboard | 键盘 / 右键 / 收起键盘 | |
| viewerMinimap | Minimap: the part of the remote screen on view | 小地图：当前显示的远程屏幕区域 | |
| modifierOnce | {keys} · applies to the next key | {keys}：作用于下一个键 | |
| modifierLocked | {keys} locked · tap again to release | {keys} 已锁定 · 再点一次解除 | |

Gestures, sharing bar, tray

| Key | en | zh |
|---|---|---|
| gestureTitle | Input mode | 操作方式 |
| gestureTrackpadBody | Slide anywhere to move the pointer — best for precise work. | 手指在屏幕任意位置滑动来移动光标，适合精细操作。 |
| gestureDirectBody | Direct touch: tap where you want to click, drag with one finger; other gestures are the same. Best for buttons and menus. | 直接触摸：点到哪里就在哪里点击，单指拖动即拖拽，其余手势相同。适合点按钮和菜单。 |
| gestureSlide / gestureSlideDoes | One-finger slide / Move pointer | 单指滑动 / 移动光标 |
| gestureTap / gestureTapDoes | Tap / Left click | 单指轻点 / 左键单击 |
| gestureLongPress / gestureLongPressDoes | Long press / Right click | 长按 / 右键单击 |
| gestureDoubleTapHold / gestureDoubleTapHoldDoes | Double-tap, hold and move / Drag | 轻点两下并按住移动 / 拖拽 |
| gestureThreeFinger / gestureThreeFingerDoes | Three-finger swipe up or down / Scroll | 三指上下滑动 / 滚轮 |
| gestureTwoFinger / gestureTwoFingerDoes | Two-finger move / Pan picture | 双指移动 / 移动画面 |
| gesturePinch / gesturePinchDoes | Pinch / Zoom picture | 双指捏合 / 缩放画面 |
| gestureVirtualMouse / gestureVirtualMouseHelp | Show virtual mouse / Left/right buttons and a wheel in direct touch | 显示虚拟鼠标 / 直接触摸时提供左右键和滚轮 |
| gotIt | Got it | 知道了 |
| sharingViewing | Someone is viewing this computer | 有人正在查看这台电脑 |
| sharingControlling | Someone is controlling this computer | 有人正在控制这台电脑 |
| sharingMeta | {path} · {span} | {path} · {span} |
| trayDisconnectViewer | Disconnect viewer | 断开查看者 |

Verification: `flutter gen-l10n` writes `l10n_untranslated.json`; it must be
empty, and `grep -c '"desktop' lib/l10n/*.arb` must be 0.

---

## 7. Tests (minimal, deliberate)

New:

| File | Test | Boundary |
|---|---|---|
| `test/touch_gestures_test.dart` (pure, explicit timestamps) | 1. "a tap clicks at once, a long press right-clicks, a two-finger tap does nothing" | D18, no 300 ms tap delay |
| | 2. "a held second tap drags and a quick one double-clicks" | double-tap-hold conversion |
| | 3. "three fingers scroll without navigating; direct touch drags with one finger" | scroll sign/chunking; direct drag |
| `test/desktop_gesture_test.dart` (rewritten, widget) | "pinch, an input epoch change and the soft keyboard keep the texture, the scale and the offset" — same `Texture` element; `desktop-scene-transform` survives the epoch; obstruction pans and restores the offset exactly, scale unchanged; the sink sees no host call | P1-4 |
| `test/modifier_latch_test.dart` (pure) | 1. "a tap applies to one key, a second tap locks, a third releases, and every key is wrapped" (event order `m↓ k↓ k↑ m↑`; `releaseAll` leaves nothing) | D16 |
| | 2. "printable keys go as text unless Ctrl, Alt or Win is held or latched" (`KeyRouting.route`) | keys reaching the host |
| `test/viewer_test.dart` (widget, desktop layout over `FakePbMapperApi`) | 1. "the toolbar collapses 2 s after the pointer leaves, not while a menu is open, never when pinned" | toolbar collapse |
| | 2. "overlays follow the session phase" — `viewerOverlayFor` for each phase once + `showsViewOnly` | six states |
| | 3. "pointer maps to display pixels and nothing is sent unless input is live" (from `desktop_test.dart:98-224`) | input authorisation |
| `test/window_chrome_test.dart` | "caption buttons are 46×32, centred in the bar, and 16 px inside the sheet" — `debugDefaultTargetPlatformOverride = TargetPlatform.windows`, `pumpApp`, rects of `caption-close` vs the `ContentSheet`; also asserts the setup screen's buttons | R4 acceptance geometry |
| `test/remote_page_test.dart` | 1. "add computer recognises an invitation, rejects a setup string, and remembers only after the first picture" | D3 inspect flow, D20 write timing |
| | 2. "enabling shares view-only or with control, and Stop works while busy" (from `desktop_test.dart:19-96,280-309` and `desktop_host_readiness_test.dart:70-119`) | host permission, local stop |
| | 3. "the sharing bar shows only while a viewer is connected, and its Stop and Disconnect work" | D15 |
| `test/remote_computers_test.dart` | "forgetting removes the thumbnail; a new identity replaces the invitation instead of merging" | invariant 6 |

Changed:

- `test/app_test.dart`: `:25-37` → "`initialPage: remote` (the `--desktop`
  hand-off) lands on Remote desktop"; `:39-52` six rail keys incl.
  `nav-remote`; `:54-63` five bar keys, no `nav-logs`; `:90-113` loop over
  `AppPage.barPages` plus Logs through `settings-logs` with `page-back`.
- `test/desktop_test.dart`: widget tests move to the files above; the store
  test `:225-278` (ordering/view-only/release) stays, adapted to `send(DesktopInput,
  geometryEpoch:)`.
- `test/desktop_host_readiness_test.dart`: keeps the store tests `:121-182`;
  `:70-119` moves to `remote_page_test`.
- `test/tray_service_test.dart`: new `TrayStrings`/`initialize` arguments;
  the existing stable-ID test also asserts the sharing items keep their IDs.
- `test/fake_desktop_api.dart`: implements every new `DesktopApiClient`
  member; `inspectInvitation` returns per input (`pbd1.` → info, setup string
  → `SETUP_STRING`).
- `test/support.dart`: Lucide400 font, no MaterialIcons, no
  `openDesktopOnStartup`, `pumpApp(desktopPrefs:)`.
- Deleted: `test/desktop_viewer_navigation_test.dart` (single viewer is now
  structural; invitation-text retention has no successor).
- Store/FFI tests (`desktop_lifecycle_test`, `desktop_recovery_test`) belong to
  the perf design; they only follow the slice API.

Previews (`test/preview_test.dart`, opt-in): `remote sharing {light,dark}`,
`remote setup`, `remote empty`, `add computer`, `viewer toolbar`, `viewer
shortcuts`, `viewer states` (one capture per overlay kind), `phone remote
zh`, `phone add`, `phone viewer landscape`, `phone keyboard`, `phone
gestures`. The picture is a JPEG-path fake frame painted by a
`RemoteScreenFixture` (`PictureRecorder` → PNG bytes; `DesktopVideoTexture`
is unsupported under `flutter test`). Posters (`gen_poster_test.dart`):
regenerate (rail gains Remote desktop, Tunnels loses `open-desktop`, icons
thicken).

No wording tests, no input matrices, no icon-scan test.

---

## 8. Implementation sequence (each step builds, analyzes and passes its tests)

Checkpoint for every step: `flutter analyze --no-fatal-infos` clean, the
named tests green, `flutter gen-l10n` with an empty untranslated file. Steps
1-2 touch only files that exist on `master` (D27, cherry-pickable).

1. **Icons** — `app_icons.dart` (400, swaps, deletions, new and `…Large`
   constants), `navigation.dart:147` 20 px, `galaxy_panel.dart:152`
   `relayLarge`, `_base` touch `iconSize: 20`, `test/support.dart` fonts,
   `gen_poster_test.dart` → `AppIcons`, `docs/ui-design.md:204` icon rules.
   Tests: `app_test`. Commit "Draw icons at Lucide 400 with size-led weights".
2. **Title bar** — `window_chrome.dart` rewrite (`CaptionTone`,
   `_RenderCaptionGlyph`, 46×32 r6), `page_frame.dart` 16/16 and `onBack`,
   `setup_flow.dart` placement, `test/window_chrome_test.dart`,
   `docs/ui-design.md:193-195`. Manual check at 100/125/150 %, maximised and
   restored. Commit "Give caption buttons room and self-drawn glyphs".
3. **Shell IA** — `AppPage.remote`, rail/bar lists, `RemoteShareDot` stub
   (hidden until step 4), Logs via Settings + `PopScope`, `initialPage` for
   `--desktop` (delete `openDesktopOnStartup`), D26 filter, `RemotePage` as a
   `PageFrame` hosting the **existing** management body temporarily (moved,
   AppBar removed); old viewer route still pushed from it; delete Tunnels
   `open-desktop`. l10n: `navRemoteDesktop`, `navRemote`, `back`,
   `settingsLogsBody`. Tests: `app_test`, desktop tests switched to
   `nav-remote`.
4. **Store contract + D14/D15** — `DesktopStore` slices, `demand`,
   `DesktopInput`, bridge removal, `DesktopPrefs`, `SharingBarLayer` in a
   builder `Stack`, tray items, delete `DesktopSharingBanner`. Coordinated
   with the perf design (same commit series or right after its store
   refactor). Needs host ops `endViewerSession` and viewer info (D24).
   Tests: `remote_page_test` #3, `tray_service_test`.
5. **Remote page** — `pages/remote/*` (host card, setup card, list, empty,
   add dialog paste-only, QR), `RemoteComputerStore`, `qr: 3.0.3`,
   `StatusChip.coloredLabel`, `GlassSegment.enabled`, `DashedBorder`,
   `quietText`; delete `pages/desktop_page.dart` and its `desktop*` keys.
   Needs `inspect_invitation`, computer vault ops, `host_enable{allow_control}`.
   Tests: `remote_page_test` #1-2, `remote_computers_test`.
6. **Desktop viewer** — viewer mode in `_home`, `pages/viewer/` core and
   desktop layout (title bar, toolbar, collapse, menus, overlays, quality
   panel, key routing, shortcuts, type text), `ViewerPalette`; delete
   `pages/desktop_viewer.dart`, `desktop_connection_banner.dart`,
   `desktop_viewer_navigation_test.dart` and the remaining `desktop*` viewer
   keys. Needs keys v2, `lockRemote`, `setQuality`, `retryNow`, `stats`,
   `latestCursor`, setup progress. Tests: `viewer_test`, `modifier_latch_test`.
7. **Mobile** — phone `RemotePage` layout, add sheet + `InviteScanner`
   (`mobile_scanner: 7.5.0`, `NSCameraUsageDescription` in
   `ios/Runner/Info.plist` + zh `InfoPlist.strings`), `MobileViewerLayout`,
   gesture machine and surface changes, `SoftKeyboardBridge`, `ModifierBar`,
   `Minimap`, `VirtualMouse`, gesture sheet, thumbnail display. Tests:
   `touch_gestures_test`, `desktop_gesture_test`. Also
   `flutter build apk --debug` (new plugin, Kotlin plugin bump only if the
   build requires it, D21).
8. **Finish** — l10n sweep (zero `desktop*` keys, zh/en parity), previews,
   posters, docs: `docs/ui-design.md` (six places, Logs in Settings on phones,
   icon weights/sizes, caption geometry, viewer chrome), `docs/remote-desktop.md`
   (gestures and their change from PR #51, management page, remembered
   computers, `:28` and `:44-52`), `ui/README.md:3`, `CLAUDE.md` (pages list:
   Remote desktop and the viewer mode; `pages/remote/`, `pages/viewer/`;
   `state/remote_computers.dart`, `state/desktop_prefs.dart`), `CHANGELOG.md`
   (right click = long press; drag = double-tap-and-hold; two-finger tap no
   longer right-clicks; Ctrl+Alt+Del still unsupported). One full `flutter
   test` at the end.

---

## 9. Invariants this structure keeps

1. Input only against a fresh presented frame: `ViewerController.live` =
   `session.inputLive`; every send carries `geometryEpoch`; the store stamps
   frame id/generation and drops mismatches.
2. The texture is never fully covered by an opaque layer: scrims are 0.72,
   the grey still is a `BackdropFilter`, cards are centred.
3. Zoom, pan, scale mode, minimap, soft keyboard, rotation, window resize,
   toolbar collapse and gutter changes never reach the store or native code;
   the `Texture` element survives all of them (tested).
4. Release on loss of locality: focus loss, `inactive`, background, epoch or
   generation change, mode change, view-only, viewer close → `releaseLocal()`.
5. One viewer at a time: structural (one `_home` child).
6. Local stop always reachable while a viewer is connected (sharing bar,
   bypassing `busy`), and while sharing (host card, tray).
7. Ctrl+Alt+Del shown disabled "Not supported".
8. Invitations are secrets: masked on screen, QR only on tap and hidden on
   leave/background, never logged, camera frames never reach Dart
   (`returnImage: false`), remembered only encrypted (D20), never plaintext.
9. Identity mismatch stops retries; "Paste new invitation" replaces, never
   merges.
