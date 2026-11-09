# Map: Flutter UI — remote-desktop pages, app shell, theme, l10n, tests

Scope: `ui/lib/src/pages/desktop_page.dart`, `desktop_viewer.dart`,
`desktop_gesture_surface.dart`, `desktop_connection_banner.dart`;
`ui/lib/src/shell/*`; `ui/lib/src/app.dart`, `ui/lib/main.dart`;
`ui/lib/src/theme/*`; `ui/lib/l10n/*.arb` + generation; related widget tests,
`preview_test.dart`, `gen_poster_test.dart`. All paths below are relative to
`ui/` unless they start with `crates/` or `docs/`. Line numbers are at
`a41eaa7` (branch `claude/remote-desktop-overhaul`).

---

## 1. Overview

- The app is one `MaterialApp` (`lib/src/app.dart:300-338`) whose `home`
  is either a blank backdrop (deciding), `SetupFlow`, or
  `AppScope(AppShell(<page>))` for one of **five** `AppPage`s
  (`overview, tunnels, relay, logs, settings`, `lib/src/shell/app_scope.dart:11-16`).
  Page switching is **not** a route: it is `setState` on `_page` in
  `_PbMapperAppState` (`app.dart:189-193`), persisted as `AppPrefs.lastPage`.
- The remote-desktop feature (PR #51) is **not** part of the shell. It lives
  in two pushed `MaterialPageRoute`s on the root navigator:
  `DesktopPage` (management) and `DesktopViewer` (picture). Both use a stock
  Material `Scaffold` + `AppBar`, not `PageFrame`, so they cover the rail and
  sheet and have **no window chrome** (no drag area, no caption buttons) on
  frameless Windows/Linux/macOS windows.
- Entry points to `DesktopPage`: an `IconButton` (key `open-desktop`,
  Material `Icons.desktop_windows_outlined`) in the Tunnels title bar
  (`pages/tunnels_page.dart:33-38`) calling `openDesktop`
  (`pages/desktop_page.dart:9-14`); and the `--desktop` launch arg
  (`main.dart:39`) → `openDesktopOnStartup` → a post-frame push
  (`app.dart:124-135`). The viewer is pushed only from `DesktopPage._viewer`
  (`desktop_page.dart:289-302`).
- A persistent `DesktopSharingBanner` (`desktop_page.dart:305-338`) is
  inserted by `MaterialApp.builder` **above the Navigator** (`app.dart:321-336`),
  so while this computer is shared it shifts every page and route down.
- Session state is owned by `DesktopStore` (`lib/src/state/desktop_store.dart`,
  another subsystem's map), lazily created by `AppStore.desktop`
  (`state/app_store.dart:38-42`) and always created at boot by
  `AppStore.start()` → `desktop.restoreOnStartup()` (`app_store.dart:192`).
  `AppStore` forwards every `DesktopStore` notification to its own listeners
  (`..addListener(_notify)`, `app_store.dart:42`).
- Theme: POCOX design system — graphite neutrals, ink controls, one blue
  signal (`theme/app_theme.dart`), layered glass (`theme/glass.dart`),
  motion tokens (`theme/motion.dart`), radii (`theme/metrics.dart`), fonts
  (`theme/fonts.dart`), Lucide icons at weight 300 only (`theme/app_icons.dart`).
  The PR #51 desktop pages ignore most of this (stock Material widgets,
  Material `Icons.*` — 18 references on 17 lines in `lib/`: 14 from the
  desktop feature, 4 in the caption buttons — and raw `Colors.black/white`).

---

## 2. Components and responsibilities

### 2.1 `lib/main.dart` (104 lines)

- `main(args)` (`:36-88`): `openDesktop = args.length == 1 && args.single == '--desktop'`
  (`:39`) — a UI route flag used by the Rust helper restart hand-off;
  stripped before `CliEntry.run` (`:43`, Rust CLI parser; non-null exit
  code → `exit`).
- Desktop window (`_isDesktop`, `:33-34`, uses `dart:io Platform`):
  `windowManager.ensureInitialized()`; `WindowOptions(size: 1180x780,
  minimumSize: 720x560, center, backgroundColor: transparent, title:
  'pb-mapper', titleBarStyle: TitleBarStyle.hidden,
  windowButtonVisibility: Platform.isMacOS)` (`:54-62`);
  `waitUntilReadyToShow` → `show()` + `focus()` (`:63-68`).
- `PbMapperService().initLogging()`, `initChangeEvents()`,
  `LogManager().initialize()`, `_sendAppDirectory()` (mobile documents dir,
  `:93-104`), `startControlServer()` (`:70-79`).
- `runApp(PbMapperApp(api: PbMapperApi(), desktop: _isDesktop,
  checkForUpdates: true, openDesktopOnStartup: openDesktop))` (`:80-87`).
- No `SystemChrome` call anywhere in `lib/` (no orientation lock, no
  immersive mode, no wakelock).

### 2.2 `lib/src/app.dart` — `PbMapperApp` / `_PbMapperAppState` (370 lines)

- Constructor params (`:40-74`): `api` (`PbMapperApiClient`), `changes`,
  `prefs`, `needsSetup`, `initialPage`, `desktop` (tray + close-to-tray),
  `updates`, `checkForUpdates`, `openDesktopOnStartup`.
- Owns (`:81-98`): `AppStore _store` (with `onRestartReady: _quit`),
  `RelayStore _relay`, `AppPrefs _prefs`, `UpdateController _updates`,
  `UpdateSchedule?`, `GalaxyView _galaxy`, `AppLifecycleListener? _lifecycle`,
  `AppPage _page` (initial `widget.initialPage ?? overview`), `bool? _setup`,
  `_allowExit`, `GlobalKey<NavigatorState> _navigator`.
- `_boot()` (`:108-150`): `prefs.load()` → `store.start()` → await
  `store.ready` → `needsSetup` (or `SetupState.needsSetup(api)`) →
  `setState(_setup, _page = parse(prefs.lastPage) ?? overview)` → await
  start → if `openDesktopOnStartup`: post-frame
  `_navigator.currentState?.push(MaterialPageRoute(DesktopPage(store:
  _store.desktop)))` (`:124-135`) → update check schedule.
- `_go(page)` (`:189-193`): `setState(_page = page)` + `prefs.setLastPage(page.name)`.
- Desktop life (`:202-279`): `AppLifecycleListener(onExitRequested)` hides
  to tray unless `_allowExit`; `windowManager.addListener(this)`;
  `onWindowClose` → `TrayService.hideToTray()` (`:264-267`) (close button
  hides; `setPreventClose(true)` at `:222`); `_store.addListener(_refreshTray)`
  (`:217`) → `TrayService.refreshStatus()` on **every** AppStore notify
  (includes every DesktopStore notify, see §3.8). `_trayStatus()`
  (`:236-248`), `_trayStrings()` (`:251-262`). `_quit()` sets `_allowExit`,
  disposes tray/log/service, `exit(0)` (`:275-279`).
- `build` (`:298-339`): `MaterialApp(navigatorKey, onGenerateTitle,
  locale: prefs.locale, supportedLocales: LocaleController.supported,
  localizationsDelegates: [AppLocalizations.delegate, GlobalMaterial…,
  GlobalWidgets…, GlobalCupertino…], theme/darkTheme: buildAppTheme(...),
  themeMode, themeAnimationDuration 320 ms easeInOutCubic,
  builder: ToastificationWrapper(Column[ListenableBuilder(_store →
  activeDesktop == null ? shrink : DesktopSharingBanner(store)),
  Expanded(child)]), home: Builder(_home))`.
- `_home` (`:341-369`): `_setup == null` → `Scaffold(AppBackdrop(SizedBox.expand))`;
  `true` → `SetupFlow`; else `AppScope(..., child: AppShell(page, onSelect:
  _go, child: switch(_page) {OverviewPage, TunnelsPage, RelayPage,
  LogsPage, SettingsPage}))`. The page widget is rebuilt fresh each switch
  (const constructors; `EntranceMotion` keyed by page in `AppShell`).
- Doc comment `:37-38` still says "four pages".

### 2.3 Tray — `lib/src/common/tray/tray_service.dart` (425 lines)

Singleton `TrayService.instance` (`:64-66`), only on Windows/Linux/macOS
(`:123-125`). Menu items `status` (disabled) / `open` / `refresh` / `quit`
(`:86-101`), IDs retained across renders. Polls `statusProvider` every 6 s
(`:164-170`) plus on every `refreshStatus()`; renders icon
(`assets/tray/tray_{offline,active,idle}` `.ico`/`@2x.png`/`.png`,
`:307-329`), tooltip and menu labels only when changed (`:227-277`). Left
click shows the app (macOS: opens menu) (`:331-343`); `hideToTray` =
`windowManager.hide()`; `showFromTray` restores/shows/focuses with a macOS
retry (`:404-424`). Tray strings come from `lookupAppLocalizations(prefs.activeLocale)`.
Launcher/tray icons are out of scope of the icon requirement.

### 2.4 Shell — `lib/src/shell/`

#### `app_scope.dart` (100 lines)
- `enum AppPage { overview, tunnels, relay, logs, settings }` (`:11-16`).
  - `icon` (`:18-24`): overview→`AppIcons.overview`, tunnels→`AppIcons.tunnels`,
    relay→`AppIcons.relay`, logs→`AppIcons.logs`, settings→`AppIcons.settings`.
  - `label(context)` (`:26-35`): `navOverview/navTunnels/navRelay/navLogs/navSettings`.
  - `grouped` (`:39-42`): tunnels, relay, settings sit on the grey grouped sheet.
  - `parse(name)` (`:44-49`): unknown/null → null.
- `AppScope extends InheritedWidget` (`:54-100`): `store`, `relay`, `prefs`,
  `updates`, `galaxy`, `page`, `go`; `of(context)` asserts presence;
  `updateShouldNotify` compares all but `go`. Note `AppScope` is **below**
  the Navigator's home, so routes pushed on the root navigator
  (`DesktopPage`, `DesktopViewer`, dialogs) can still read it only through
  the pushing context's ancestors — they cannot (routes are siblings of
  home in the Overlay). `openDesktop` therefore reads `AppScope` from the
  caller and passes the store explicitly (`desktop_page.dart:10-13`).

#### `app_shell.dart` (107 lines)
- `usesRail(context)` (`:15-20`): touch platform (android/iOS by
  `Theme.of(context).platform`) needs width ≥ 900, otherwise ≥ 720.
- `AppShell(page, onSelect, child)` (`:25-107`): wraps the page in
  `EntranceMotion(key: ValueKey(page), offset: 8)` (`:45-49`).
  - Compact (`!wide`, `:50-58`): `ShellLayout(wide:false, Scaffold(backgroundColor:
    sheetColor(scheme, grouped: page.grouped), body: content,
    bottomNavigationBar: NavBar(page, onSelect)))`.
  - Wide (`:60-105`): `ShellLayout(wide:true, Scaffold(backgroundColor:
    backdropBase, body: AppBackdrop(Stack[SafeArea(Row[SizedBox(width:
    kRailWidth=212, NavRail), Expanded(Padding(fromLTRB(0, 8, 8, 8),
    ContentSheet(grouped, content)))]), if isFramelessDesktop
    Positioned(top 0, left 212, right 0, height kSheetInset=8,
    WindowDragArea)])))`.

#### `navigation.dart` (315 lines)
- Constants: `kRailWidth = 212` (`:13`), `_rowHeight = 38` (`:15`),
  `_rowGap = 2` (`:16`).
- `NavRail(page, onSelect)` (`:21-115`): `Material(transparency, Column[`
  - top drag strip `SizedBox(height: isFramelessDesktop ? 46 : 18,
    WindowDragArea)` (`:38-41`) — room for macOS traffic lights;
  - `WindowDragArea(Padding(fromLTRB(20,0,12,18), Row[BrandMark(26),
    gap 10, appTitle 15/w600/-0.1]))` (`:42-64`);
  - `Padding(h10, Stack[AnimatedPositioned(duration Motion.of(520 ms),
    curve Motion.spring, top: index*(step+_rowGap), height: step,
    IgnorePointer(Glass(depth: chrome, radius 999)))` (`:69-86`) — the
    selection glass; `index = AppPage.values.indexOf(page)` (`:31`);
    `step = textScaler.scale(38)` (`:30`);
  - `Column[for entry in AppPage.values: SizedBox(height: step,
    _RailRow(...)), SizedBox(height: 2)]` (`:87-102`) — **one row per
    `AppPage.values` entry; 5 today**;
  - `Expanded(WindowDragArea)` (`:106`), `_RelayFoot` (`:107-110`).
- `_RailRow` (`:117-167`): `Semantics(selected, button)`, `InkWell(key:
  Key('nav-${page.name}'), mouseCursor: clickable, hoverColor:
  backdropHover(scheme), radius 999, onTap: selected ? null : onTap)`,
  `Padding(h14, Row[Icon(page.icon, size: 18, color: ink), gap 11,
  Text(label, 13.5, w600 if selected else w400, color ink)])`; ink =
  `onSurface` selected / `onSurfaceVariant` otherwise.
- `_RelayFoot` (`:170-221`): `ListenableBuilder(store)` → `Glass(chrome,
  radius kRowRadius=14)` → `InkWell(key: 'rail-relay-status')` →
  `RelayChip(state, filled:false, shrinks:true)` + mono relay address
  11.5 `onSurfaceMuted`; tap → `onSelect(AppPage.relay)` (doc comment at
  `:169` wrongly says Settings).
- `NavBar(page, onSelect)` (`:224-255`): `Material(color:
  surfaceContainerLow, SafeArea(top:false, Padding(fromLTRB(8,6,8,6),
  Row[for entry in AppPage.values: Expanded(_BarItem)])))` — **5 entries**.
- `_BarItem` (`:257-315`): `InkWell(key: 'nav-${page.name}', radius 16)`,
  `Column[AnimatedContainer(Motion.medium, Motion.move, 54x30, pill,
  color: selected ? softWash(scheme, strength: 1.6) : transparent,
  Icon(page.icon, size: 20, ink)), gap 3, Text(label, 11.5, w600/w400)]`.
- Doc comments (`:18`, `:223`) say "four pages" (stale).

#### `page_frame.dart` (179 lines)
- `ShellLayout extends InheritedWidget { bool wide }` (`:7-17`),
  `wideOf(context)` defaults to false (so the desktop routes, which sit
  outside `AppShell`, would be "compact" if they used `PageFrame`).
- `PrimaryAction(key, icon, label, onPressed)` (`:21-33`).
- `PageFrame(title, body, actions = [], primary, overlay = false)`
  (`:38-88`): `barHeight(context)` = 58 wide / `54 + paddingOf.top` compact
  (`:60-61`); `overlay` stacks the bar over the body (galaxy) (`:72-79`),
  otherwise `Column[bar, Expanded(body)]`.
- `_TitleBar` (`:90-179`), in detail:
  - `height = inSheet ? 58 : 54` (`:106`);
  - `lead = (inSheet ? 0 : trafficLightInset) + (inSheet ? 22 : 16)` (`:109`);
  - `roomy = MediaQuery.width >= 560` (`:110`);
  - `SizedBox(height, Stack(alignment: centerStart, [Positioned.fill(WindowDragArea),
    Padding(EdgeInsetsDirectional.only(start: lead, end: 8), Row[…])]))`
    (`:112-175`) — **end padding 8** (`:120`);
  - Row: `Expanded(IgnorePointer(Text(title, titleLarge, fontSize 19 in
    sheet / 18 compact)))` (`:123-134`), `...actions` (`:135`), then if
    `primary`: `SizedBox(width: 6)` (`:137`) + roomy
    `FilledButton.icon(minimumSize Size(0,38), padding STEB(14,0,18,0),
    icon Icon(primary.icon, size 18), label)` (`:139-153`) or
    `IconButton.filled(tooltip: label, bg primary, fg onPrimary)`
    (`:155-164`);
  - if `drawsCaptionButtons`: `SizedBox(width: 10)` (`:167`, **gap 10**) +
    `CaptionButtons(height: 36)` (`:168`);
  - wide returns the bar as-is; compact wraps `SafeArea(bottom:false)` (`:176-177`).
  - Geometry today (Windows, wide): sheet inset 8 from window edge
    (`kSheetInset`), close button's right edge 8 px inside the sheet
    edge, button 44x36 vertically centred in 58 → top at 11 px, under
    the sheet's 18 px corner radius. This is the crowding the brief fixes.

#### `window_chrome.dart` (199 lines)
- `isFramelessDesktop` (`:13-17`): `!kIsWeb && defaultTargetPlatform in
  {windows, macOS, linux}` (by `defaultTargetPlatform`, so `flutter test`
  — android — never touches a window).
- `drawsCaptionButtons` (`:20-21`): frameless and not macOS.
- `trafficLightInset` (`:24-27`): 76 on macOS, else 0.
- `WindowDragArea({child})` (`:31-48`): non-frameless → child or
  `SizedBox.expand`; frameless → `DragToMoveArea(GestureDetector(translucent,
  onDoubleTap: _toggleMaximize))`. `_toggleMaximize` (`:50-60`) catches all errors.
- `CaptionButtons({height = 40})` (`:64-71`), `_CaptionButtonsState with
  WindowListener` (`:73-135`): `windowManager.addListener(this)` in
  initState, `_sync()` reads `isMaximized` (`:89-96`); `onWindowMaximize/
  onWindowUnmaximize` set `_maximized` (`:98-102`) (no fullscreen
  handling). Builds `Row(min)[`
  - `_CaptionButton(icon: Icons.remove, tooltip: windowMinimize,
    onTap: windowManager.minimize)` (`:110-115`);
  - `_CaptionButton(icon: _maximized ? Icons.filter_none :
    Icons.crop_square, iconSize: _maximized ? 12.5 : 14, tooltip:
    windowRestore/windowMaximize, onTap: _toggleMaximize)` (`:116-122`);
  - `_CaptionButton(icon: Icons.close, tooltip: windowClose, close: true,
    onTap: windowManager.close)` (`:125-131`) — close goes through
    `close()` so `setPreventClose` + `onWindowClose` hide to tray.
- `_CaptionButton(icon, tooltip, onTap, height, close = false, iconSize = 15)`
  (`:137-156`), state (`:158-198`): `_hover` via `MouseRegion`
  enter/exit; colours (`:163-175`):
  - hover + close: bg `Color(0xFFC42B1C)`, fg `Colors.white`;
  - hover: bg `scheme.onSurface @ alpha 0.08`, fg `scheme.onSurface`;
  - rest: bg transparent, fg `scheme.onSurfaceVariant`;
  - `Tooltip(MouseRegion(GestureDetector(opaque, onTap,
    AnimatedContainer(90 ms, width: 44, height: widget.height,
    BoxDecoration(color: bg, borderRadius: 10), center,
    Icon(icon, size: iconSize, color: fg)))))` (`:176-197`).
- Users of `CaptionButtons`: `_TitleBar` (height 36, `page_frame.dart:168`)
  and `SetupFlow` (`Positioned(top: 6, right: 6, CaptionButtons(height:
  34))`, `pages/setup_flow.dart:196-201`; drag strip 44 px at `:188-195`).

#### `app_prefs.dart` (123 lines)
`AppPrefs extends ChangeNotifier` (`:10`), `persist` off in tests. Keys
(`:16-20`): `theme_mode`, `last_page`, `update_auto_check`,
`update_last_check_ms`, `update_announced`; locale via
`LocaleController` key `app_locale` (`common/locale_controller.dart:10`).
`setLastPage(String)` (`:86-93`) does **not** notify. No desktop-related
preference exists today (no quality preference, no toolbar pin, no
trackpad-mode memory, no remembered computers).

### 2.5 Remote-desktop pages — `lib/src/pages/`

#### `desktop_page.dart` (338 lines) — management route

- `openDesktop(context)` (`:9-14`): `AppScope.of(context).store.desktop`,
  `Navigator.of(context).push(MaterialPageRoute(DesktopPage(store)))`.
- `DesktopPage({required DesktopStore store})` (`:16-21`).
- `_DesktopPageState` (`:23-303`) local state:
  `_service = TextEditingController(text: 'desktop:my-computer')` (`:24`),
  `_invitation` controller (`:25`), `int? _display` (`:26`),
  `bool _relayOnly = false, _viewerOpen = false` (`:27`),
  `int _viewerOpenSerial` (`:28`).
- `initState` (`:29-35`): `scheduleMicrotask(() => store.load())` — every
  time the page opens: `capabilities()`, `trustStatus()`,
  `backgroundAction('status')`, then starts the store's **250 ms poll
  timer, never cancelled until dispose/restart** (`desktop_store.dart:142-151`).
- `build` (`:44-288`): `ListenableBuilder(Listenable.merge([store,
  _invitation]))` → `Scaffold(appBar: AppBar(title: desktopTitle), body:
  SafeArea(Align(topCenter, ConstrainedBox(maxWidth 800, ListView(padding
  20, [...])))))`:
  1. `store.error` as red text (`:66-75`).
  2. If `store.capabilities.host` (Windows host) (`:76-140`): title
     `desktopShare` (titleLarge); help `fullyEnabled ? desktopEnabledHelp :
     desktopEnableHelp` where `fullyEnabled = background.enabled &&
     background.autoStart && host.controlAllowed` (`:52-55`).
     If `store.sharing`: host message, background message (if different),
     `Wrap[OutlinedButton.icon(Icons.copy, desktopCopy → Clipboard
     invitation; disabled if null), TextButton.icon(key 'desktop-stop',
     Icons.stop_circle_outlined, desktopStop → store.stopHost)]` (`:86-113`).
     If `!fullyEnabled`: `FilledButton.icon(key 'desktop-share', disabled
     when busy or no displays, onPressed: store.enableHost(service =
     background.service ?? trust.saved.service ?? _service.text.trim(),
     display = _display ?? host.display.id ?? trust.saved.displayId ??
     capabilities.displays.first.id), icon: busy ? 18 px spinner :
     Icons.screen_share_outlined, label desktopEnable)` (`:114-138`).
     `Divider(height: 40)`.
  3. Title `desktopConnect` (`:141-144`). If `store.connected`:
     `Wrap[FilledButton(desktopOpen → _viewer), TextButton(desktopDisconnect
     → store.disconnect)]` (`:146-159`) — this is today's "back to
     picture". Else: `TextField(key 'desktop-invitation', 2-4 lines,
     autocorrect/suggestions/smart quotes/dashes off, label desktopPaste)`
     + `FilledButton.icon(key 'desktop-connect', disabled when busy or
     empty; onPressed: serial = _viewerOpenSerial; await store.connect(text,
     _relayOnly); if mounted && store.connected && serial unchanged →
     _viewer(context), icon Icons.desktop_windows_outlined, label
     desktopConnect)` (`:160-192`).
  4. `ExpansionTile(key 'desktop-helper-maintenance', title desktopAdvanced)`
     (`:194-280`): if host capability — when not sharing: service-name
     `TextField` (desktopService) and `DropdownButtonFormField<int>`
     of displays (`'${name} · ${w} × ${h}'`, desktopDisplay); when sharing:
     `SwitchListTile(desktopAllowControl, value host.controlAllowed,
     onChanged: store.allowControl)`; `Text(desktopStopHelp)`; revoke
     `TextButton(desktopTrustRevoke → store.revokeTrust)` if background
     configured or trust saved; if `capabilities.helper != null`: install
     (`helperAction('install')`), and if installed: disable (key
     'desktop-helper-disable'), uninstall (key 'desktop-helper-uninstall').
     Always: `SwitchListTile(desktopRelayOnly, _relayOnly)`.
- `_viewer(context)` (`:289-302`): guard `_viewerOpen`; increments
  `_viewerOpenSerial`; pushes `MaterialPageRoute(DesktopViewer(store))`;
  awaits `push` **and** `route.completed` (exit transition finished, old
  viewer disposed) before clearing `_viewerOpen`.
- `DesktopSharingBanner({store})` (`:305-338`): `ListenableBuilder(store)` →
  if `store.sharing`: `Material(color: secondaryContainer, SafeArea(bottom:
  false, Padding(h12 v4, Row[Icon(Icons.screen_share_outlined), gap 8,
  Expanded(Text(desktopSharing)), TextButton(desktopStop →
  store.stopHost)])))`, else `SizedBox.shrink`.
- Implicit Material icons drawn by stock widgets here: `AppBar` back button
  (`BackButtonIcon` → `Icons.arrow_back` / `arrow_back_ios_new` on Apple),
  `ExpansionTile` (`Icons.expand_more`), `DropdownButtonFormField`
  (`Icons.arrow_drop_down`).
- No view-only choice at enable time (enable always grants control —
  fake: `fake_desktop_api.dart:6-18`); no QR, no remembered computers list,
  no online status, no host viewer info. The unused strings in §4.6 are
  leftovers from earlier iterations.

#### `desktop_viewer.dart` (568 lines) — viewer route

- `DesktopViewer({required DesktopStore store})` (`:12-17`);
  `_DesktopViewerState with WidgetsBindingObserver` (`:19-539`).
- Fields (`:21-31`): `ui.Image? _image` (JPEG path), `DesktopFrame?
  _decoded`, `DesktopStatus? _rendered, _videoGeometry`, `int _viewEpoch
  = -1`, `int? _renderedEpoch`, `DateTime? _decodedAt`, `bool _decoding,
  _trackpad = false, _foreground = true`, `double _lastAspectRatio = 16/9`,
  `FocusNode _focus`, `Set<LogicalKeyboardKey> _keys` (held mapped keys),
  `GlobalKey<DesktopGestureSurfaceState> _surface`.
- `initState` (`:33-44`): add lifecycle observer; `_foreground =
  lifecycleState == null || resumed`; `store.setViewerForeground(_foreground)`;
  `store.addListener(_update)`; `store.cursorStatus.addListener(_updateCursor)`;
  `_update()`.
- `didChangeAppLifecycleState` (`:46-54`): leaving `resumed` →
  `_releaseLocalInput()`; `store.setViewerForeground(state == resumed)`.
  On desktop Flutter reports `inactive` on focus loss, so alt-tab away ends
  control (store sends `control(false)`, keeps `_controlWanted`).
- `_update()` (`:56-92`), called on **every** store notification:
  epoch change (`store.mediaEpoch`) clears image/decoded/rendered/geometry/
  keys and disposes the old `ui.Image` post-frame (`:57-70`); H.264:
  `_videoGeometry = frame.status` if `videoWidth/Height >= 2`,
  `_lastAspectRatio = w/h`, `_rendered = store.videoPresented`,
  `_renderedEpoch` (`:71-79`); then **unconditional `setState`** (`:80`),
  `_updateCursor()`; JPEG: start `_decode` if a new non-empty non-H.264
  frame (`:82-91`).
- `_updateCursor()` (`:94-109`): cursor from `store.cursorStatus.value`
  only if its `generation == _rendered.generation`; normalized by
  `_rendered.display.width/height` → `_surface.currentState?.updateRemoteCursor`.
- `_decode(frame, epoch)` (`:111-165`): `ImmutableBuffer` →
  `instantiateImageCodecWithSize` capped to 1600x1000 (`:117-123`); drops
  the result if unmounted, epoch/generation changed, codec became H.264,
  or ≥ 2 s old (`:127-134`); `setState(_image, _decoded, _rendered=null,
  _decodedAt)`; post-frame (i.e. after it was painted) sets `_rendered =
  frame.status` and calls `store.acknowledgePresented(status, epoch)`
  (`:144-155`). Errors → `store.error`. `finally` re-runs `_update` only for
  a newer frame (no retry loop) (`:160-164`).
- `didUpdateWidget` (`:167-180`) re-binds listeners if the store changes;
  `dispose` (`:182-191`): remove listeners, **`store.setViewerForeground(false)`**,
  dispose focus and image.
- `_interactive` (`:193-201`): `store.controlling && _rendered != null &&
  _renderedEpoch == store.mediaEpoch && (codec == 'h264' || decodedAt <
  2 s ago) && _rendered.generation == store.frame.status.generation`.
- `_send(event)` (`:202-207`): only if `_interactive`; `store.send(rendered,
  event, mediaEpoch: _renderedEpoch)`.
- `_move(normalized)` (`:209-219`): `x = floor(nx * display.width)` clamped
  `[0, width-1]`, same for y, using `_rendered.display`.
- `_click(button)` (`:220-223`), `_key(key)` (`:225-228`): down then up.
- `_keyboard(node, KeyEvent)` (`:230-253`): ignored unless `_interactive`;
  mapped key (`_keyNames`) → `{'kind':'key','key':name,'down': event is!
  KeyUpEvent}` (repeats are downs), tracked in `_keys`; else a printable
  `event.character` on down/repeat with no Ctrl/Alt/Meta pressed →
  `{'kind':'text','text': character}`; everything else ignored (so
  Ctrl+B, F-keys, Insert, digits-with-Ctrl never reach the host).
- `build` (`:255-416`): `Scaffold(appBar: AppBar(title: desktopTitle,
  actions: [TextButton(desktopDisconnect → await store.disconnect();
  Navigator.pop)]), body: SafeArea(Column[`
  - `DesktopConnectionBanner(store)` (`:276`);
  - `Wrap[FilledButton.tonal(hasGrant ? desktopEndControl →
    control(false) : desktopRequestControl → control(true), enabled only
    if !busy && store.fresh && viewer.controlAllowed),
    SegmentedButton<bool>(key 'desktop-touch-mode', segments false=
    desktopTouchMode, true=desktopTrackpadMode, on change
    _releaseLocalInput + setState(_trackpad))]` (`:277-317`);
  - `Text(desktopGestureHelp, bodySmall)` (`:318-324`);
  - `Expanded(Focus(focusNode: _focus, autofocus: true, onKeyEvent:
    _keyboard, onFocusChange: !focused → _releaseLocalInput,
    ColoredBox(Colors.black, _screen(...))))` (`:325-338`);
  - bottom `SingleChildScrollView(horizontal, padding 8, Row[`
    `IconButton(desktopKeyboard, Icons.keyboard_outlined → _text dialog)`,
    `IconButton(desktopLeftClick, Icons.ads_click → _click('left'))`,
    `IconButton(desktopRightClick, Icons.mouse_outlined → _click('right'))`,
    `IconButton(desktopScrollUp, Icons.arrow_upward → wheel vertical -3)`,
    `IconButton(desktopScrollDown, Icons.arrow_downward → wheel +3)`,
    `TextButton('escape'|'tab'|'enter'|'backspace')` (raw, unlocalized
    labels), `TextButton('Ctrl+A')` (control down, a, control up),
    `IconButton(desktopRelease, Icons.pan_tool_outlined → store.release,
    always enabled)]` (`:339-411`).
- `_releaseLocalInput()` (`:418-424`): `surface.cancelInput()`; if any
  mapped key held → clear and `store.release()`.
- `_screen(context, status)` (`:426-517`): `texture = store.videoTexture`,
  `video = texture == null ? null : _videoGeometry`; cursor from
  `status.cursor` if same generation as `_rendered`, else
  `_rendered.cursor`; `Stack(expand)[DesktopGestureSurface(key: _surface,
  aspectRatio: _lastAspectRatio, enabled: _interactive, trackpad,
  inputEpoch: (store, store.mediaEpoch, store.frame?.status.generation),
  cursor: normalized or null, onFocus: _focus.requestFocus, onMove: _move,
  onButton: button event, onWheel: {'kind':'wheel', horizontal:
  round(dx/30) clamp ±100, vertical: round(dy/30) clamp ±100}, onRelease:
  store.release, child: Stack[Texture(key: ValueKey(textureId),
  textureId, filterQuality: low) | RawImage(image, BoxFit.fill, low)])`,
  then overlays: no picture → centred white text desktopStopped (state
  'stopped') / desktopConnecting; picture but `!store.fresh` → bottom
  pill (black 65 %, radius 8) `desktopStale`. Comment `:492-493`:
  **never cover the whole native texture with an opaque layer** — Flutter
  must keep compositing it so the next recovered frame is presented and
  acknowledged.
- `_text(context)` (`:519-538`): `AlertDialog(title desktopKeyboard,
  TextField(autofocus, maxLength 256), action desktopSendText)` →
  `{'kind':'text','text': text}`. This is the only soft-keyboard path on
  mobile (no inline IME capture, no modifier bar).
- `_keyNames` (`:541-568`): shiftLeft/Right→`shift`, controlLeft/Right→
  `control`, altLeft/Right→`alt`, metaLeft/Right→`meta`, enter, escape,
  tab, backspace, delete, arrowLeft/Right/Up/Down→`left/right/up/down`,
  home, end, pageUp→`page_up`, pageDown→`page_down`, keyA/C/V/X/Z→
  `a/c/v/x/z`. Exactly mirrors Rust `Key` (`crates/pb-mapper-desktop/src/lib.rs:264-287`,
  mapped in `crates/pb-mapper-desktop/src/windows.rs:538-565`).

#### `desktop_gesture_surface.dart` (435 lines) — local navigation + pointer input

- `DesktopGestureSurface({aspectRatio, enabled, trackpad, inputEpoch:
  Object, onMove(Offset normalized), onButton(String, bool),
  onWheel(Offset delta), onRelease(), onFocus(), child, cursor: Offset?})`
  (`:11-40`). All callbacks return `Future<void>` and are fire-and-forget
  (`unawaited`).
- State (`:42-56`): `_touches: Map<pointer, (start, last)>`,
  `_mouseButtons: Map<pointer, String>`, `_viewport`, `_scene` (fitted
  picture size), `_offset`, `_scaleAnchor`, `_remoteCursor`,
  `_cursorPaint: ValueNotifier<Offset?>`, `_lastLocalMove`, `_scale = 1`,
  `_startScale`, `_gestureScale`, `_scalePointers`, `_maxTouches`,
  `_touchStarted`, `_tapPoint`, `_moved`, `_touchEligible`, `_dragging`,
  `Timer? _hold`.
- `didUpdateWidget` (`:64-74`): change of `enabled`, `trackpad` or
  `inputEpoch` → `cancelInput()` and forget `_remoteCursor`;
  then `updateRemoteCursor(widget.cursor)`. Scale/offset are **kept**.
- `updateRemoteCursor` (`:76-84`): ignored while touching or within 500 ms
  of a local move (local echo suppression); else updates `_cursorPaint`.
- `cancelInput()` (`:86-98`): cancel hold timer; if dragging or a mouse
  button held → `onRelease()`; clear state.
- `_point(local, clamp)` (`:110-121`): `(local - offset) / (scene * scale)`;
  outside [0,1] → null unless `clamp`.
- `_move(point)` (`:123-129`): only if enabled; sets `_remoteCursor`,
  `_lastLocalMove`, `_cursorPaint`, `onMove`.
- `_click(button, local)` (`:131-142`): trackpad → at `_remoteCursor ??
  widget.cursor ?? (0.5,0.5)`; touch → at `_point(local)`; enqueue move,
  down, up synchronously.
- `_down` (`:149-204`): mouse/non-touch → focus, button from
  `event.buttons` (secondary→right, middle→middle, else left), move +
  down. Touch: first finger resets gesture, `_touchEligible = enabled &&
  (trackpad || inside picture)`; second finger cancels hold, ends a drag
  (`onRelease`), records two-finger midpoint as `_tapPoint`; single
  eligible finger starts `_hold = Timer(kLongPressTimeout /*500 ms*/)` →
  if still one finger and not moved: `_dragging = true`, move to cursor
  (trackpad) or tap point (touch), `onButton('left', true)`.
- `_movePointer` (`:206-239`): mouse with held button → clamped move.
  Touch: beyond `kTouchSlop` → `_moved`, cancel hold; one-finger eligible
  moved stroke: trackpad → relative move `delta / (scene * scale)` from
  `_remoteCursor`; touch → absolute clamped move (hover-like, no button,
  unless the hold already pressed left).
- `_up` (`:241-262`): mouse → button up. Touch, last finger: if dragging
  → left up; else if eligible, not moved, within `kLongPressTimeout`:
  1 finger → `_click('left', pos)`, **2 fingers → `_click('right',
  midpoint)`** (`:259-260`). 3+ fingers → nothing.
- `_boundedOffset` (`:264-273`): centre the scene on an axis if smaller
  than the viewport, else clamp so it covers the viewport.
- `_layout(viewport)` (`:275-293`), called in `LayoutBuilder` during build:
  `scene = fit(aspectRatio)`; on change keeps the normalized point at the
  viewport centre and `_scale`, recomputes `_offset`. A soft keyboard
  (`adjustResize`, Scaffold resize) therefore **shrinks the scene and
  re-derives the offset** — scale is kept but the visible zoom changes.
- `_scaleUpdate` (`:299-320`): navigates when `pointerCount != 1 ||
  !enabled` (trackpad pan-zoom has `pointerCount == 0`; when not
  interactive a single finger pans); `scale = clamp(start * s / s0, 1, 8)`,
  offset anchored at the focal point.
- `_signal` (`:322-341`): scroll wheel without Ctrl while enabled → move
  + `onWheel(scrollDelta)`; otherwise local zoom `scale *
  exp(-dy/250)` clamped 1..8 about the pointer.
- `build` (`:343-406`): `LayoutBuilder → Listener(opaque, down/move/up,
  cancel → cancelInput, hover → move if inside, signal) →
  GestureDetector(opaque, supportedDevices {touch, stylus,
  invertedStylus, trackpad}, onScaleStart/Update/End) → ClipRect(Stack[
  Positioned(0,0, scene size, Transform(key 'desktop-scene-transform',
  Matrix4.diagonal3Values(scale) ..setTranslationRaw(offset),
  RepaintBoundary(child))), ValueListenableBuilder(_cursorPaint →
  Positioned(cursor * scene * scale + offset, IgnorePointer(CustomPaint(18x27,
  DesktopCursorPainter))))])`. The texture child is the same element across
  zoom/pan (only the `Transform` changes).
- `DesktopCursorPainter` (`:409-435`): black arrow path with 1.5 px white
  stroke; constant screen size; `shouldRepaint` false.
- There is no three-finger scroll, no double-tap-and-hold drag, no
  long-press right click, no virtual mouse, no minimap.

#### `desktop_connection_banner.dart` (106 lines)

`DesktopConnectionBanner({store})` → `ListenableBuilder(store)` →
`Container(key 'desktop-connection-state', full width, padding h12 v6,
color surfaceContainer, Column[`
- `Row[14x14: live → Icon(Icons.check_circle, 14, primary); idle or
  identityMismatch → Icon(Icons.link_off, 14); else
  CircularProgressIndicator(2), gap 8, Expanded(label), retryWaiting →
  '${ceil(retryIn s)} s']` (`:48-75`);
- metrics line (`:76-91`): `[viewer.path, if h264 '${presentFps} fps',
  '${desktopFrameAge} ${age s}', if frameAckMs > 0 '${desktopAckLatency}
  ${ms} ms', if reconnectAttempt > 0 '${desktopRecoveryAttempt} n'].join(' · ')`;
- detail line, fixed 18 px tall (`:92-100`): `needsHelper` (message
  contains `secure_desktop_requires_helper` or `Secure/locked desktop
  unavailable`, `:32-34`) → `desktopOrdinaryOnlyHelp`; identity mismatch →
  `desktopIdentityMismatchHelp`; else `recoveryReason ?? error ?? message`.
- Phase → label (`:18-29`), `DesktopConnectionPhase` from
  `desktop_store.dart:6-16,100-120`: idle→`desktopStateIdle`,
  connecting→`desktopStateConnecting`, live→`desktopStateLive`,
  delayed (frameAckMs > 500)→`desktopStateDelayed`,
  waitingForDesktop (`capture_unavailable`)→`desktopStateWaitingDesktop`,
  recoveringVideo→`desktopStateRecovering`, retryWaiting→`desktopStateRetry`,
  reconnecting→`desktopStateReconnecting`, identityMismatch→`desktopIdentityMismatch`.
- Parsed but **never shown** anywhere in the UI: `DesktopStatus.directStage`,
  `directFailure`, `captureFps`, `receiveFps`, `droppedFrames`, `bitrate`,
  `desktop` (`ffi/desktop_api.dart:59-132`); `DesktopVideoTexture.stats()`
  (`ffi/desktop_video.dart:100-106`) has no caller.

### 2.6 Theme — `lib/src/theme/`

- `app_theme.dart` (642 lines): `appColorScheme(brightness)` (`:19-73`,
  ink `0xFF1D1D1F`, paper `0xFFE6E6E6`, tertiary = signal blue light
  `0xFF1B6FC8` / dark `0xFF7DB6F2`, surfaces, error). Helpers:
  `brandGradient` (`:77`), `signalColor` (`:86`), `successColor`
  (`:89-91`), `cautionColor` (`:94-96`), `onSurfaceMuted` (`:100-101`),
  `softWash` (`:105-108`), `fieldFill`/`cardFieldFill`/`codeSurface`
  (`:111-123`), `floatingSurface`/`floatingEdge` (`:126-134`),
  `backdropBase` (`:137-139`), `backdropGlows` (`:142-151`),
  `backdropHover` (`:154-156`), `surfaceSelection` (`:160-163`),
  `sheetColor` (`:167-170`), `sheetEdge`, `sheetShadows`, `raisedCard*`
  (`:173-233`). Type scale `_textTheme` (`:239-271`: titleLarge 19/w600,
  bodySmall 12). `_base` ThemeData (`:276-586`): pill buttons with
  `LitFace`, `IconButton` min 44 circle fg `onSurfaceVariant` (`:397-403`),
  `AppBarTheme` (`:566-573`), `iconTheme size 20` (`:584`).
  `desktopize` (`:590-636`): compact density, no splash, IconButton min 34
  **iconSize 18** (`:611-616`), buttons min 36 and 13.5 text.
  `buildAppTheme` (`:639-642`) desktopizes when `isDesktop`. There is **no
  dark "viewer" palette**; desktop viewer code uses raw `Colors.black/white`.
- `fonts.dart` (92 lines): `appFontFamily 'GoogleSansFlex'`,
  `monoFontFamily 'GeistMono'`, `bundledCjkFamily 'NotoSansSC'`;
  `isDesktop` (`:22-26`); `uiFontFamily` = 'Segoe UI' on Windows
  (`:33-36`); CJK fallback chains (`:44-64`); `mono()` (`:80-92`).
- `glass.dart` (848 lines): `GlassDepth {chrome, floating, overlay}`
  (`:20-29`), `glassRecipe` (`:65-145`), `Glass` (`:149-209`,
  `BackdropFilter` blur for floating/overlay), `OuterShadowPainter`,
  `AppBackdrop` (`:346-369`), `ContentSheet` (`:401-427`, radius 18,
  edge, shadows), `EntranceMotion` (`:431-495`, 420 ms emphasized),
  `LitFace` (`:500-535`), `FieldBorder` (`:593-695`), `GlassSegmented<T>`
  (`:718-848`, gliding glass segment, height 34, icons 16) — the
  POCOX-native replacement for the viewer's `SegmentedButton`.
- `motion.dart` (99 lines): `Motion.fast 120 ms`, `medium 220 ms`,
  `slow 320 ms`, curves `move`, `emphasized`, `spring` (`:13-41`);
  `Motion.of/reduced` honour reduced motion; `appPageTransitions` (fade on
  desktop, `FadeForwards` Android, Cupertino iOS) (`:65-73`).
- `metrics.dart` (64 lines): `kPanelRadius 16`, `kControlRadius 10`,
  `kRowRadius 14`, `kCardRadius 20`, `kDialogRadius 24`, `kSheetRadius 18`,
  `kSheetInset 8`, `kFieldRadius 14`, `kMenuRadius 14` (`:9-33`);
  `clickable` cursor (`:40-48`); `measureTextLine` (`:51-64`).
- `app_icons.dart` (84 lines): see Appendix B.

### 2.7 Localisation

- `l10n.yaml`: `arb-dir: lib/l10n`, `template-arb-file: app_en.arb`,
  `output-localization-file: app_localizations.dart`,
  `untranslated-messages-file: l10n_untranslated.json`.
- `pubspec.yaml:111-113` `flutter: generate: true` → `flutter pub get` /
  `flutter gen-l10n` writes `lib/l10n/app_localizations{,_en,_zh}.dart`.
  Those and `ui/l10n_untranslated.json` are **gitignored**
  (`ui/.gitignore:61-62`); only `app_en.arb` (679 lines) and `app_zh.arb`
  (575 lines) are tracked. CI (`.github/workflows/syntax-check.yml:225-232`)
  runs `flutter pub get`, `flutter analyze --no-fatal-infos`, `flutter test`.
- Access: `context.l10n` (`common/l10n_extension.dart:10-24`, falls back
  to English); outside widgets `lookupAppLocalizations(locale)`
  (tray, toasts in `app.dart`). `LocaleController.supported = [en, zh]`.
- Desktop keys: 93 `desktop*` keys in each ARB, at the top of the file
  (`app_en.arb:2-60`, `app_zh.arb:2-60`) and the end (`app_en.arb:640-678`,
  `app_zh.arb:538-574`) — added out of the file's grouping. 51 are used,
  42 are dead (list in §4.6). Shell keys: `navOverview/navTunnels/navLogs/
  navSettings` (`app_en.arb:65-68`), `navRelay` (`:429`), `windowMinimize/
  Maximize/Restore/Close` (`:363-366`), tray keys (`:368-376`).

---

## 3. Data / control flow (step by step)

### 3.1 Boot to shell
1. `main` → window options (hidden title bar) → service init → `runApp`.
2. `_PbMapperAppState.initState` → `_boot` (§2.2) and, on desktop,
   `_startDesktop` (tray, close-to-tray, `_store.addListener(_refreshTray)`).
3. `AppStore.start()` reads config/tunnels and calls
   `desktop.restoreOnStartup()` → `DesktopStore` created and may start its
   250 ms poll timer (background sharing configured / auto-start trust).
4. `_home` renders `AppScope(AppShell(page))`; `AppShell` chooses rail vs
   bar by `usesRail` on every build (window resize flips layout live).

### 3.2 Page switch
`_RailRow`/`_BarItem` tap → `onSelect(page)` = `_PbMapperAppState._go` →
`setState` + `prefs.setLastPage`. `AppScope.page` updates; `AppShell`
keys `EntranceMotion` by page so the new page fades/slides 8 px; the rail's
selection glass animates `top` with `Motion.spring` 520 ms. `_RelayFoot`
→ `AppPage.relay`; Tunnels' relay link → `scope.go(AppPage.relay)`;
several "settings" links → `scope.go(AppPage.settings)`.

### 3.3 Opening the desktop management page
- From Tunnels: `IconButton('open-desktop')` → `openDesktop(context)` →
  push `MaterialPageRoute(DesktopPage(store: AppScope.store.desktop))` on the
  **root navigator** (over rail + sheet; Material AppBar with back arrow).
- From launch arg `--desktop` (helper install/uninstall restart,
  `DesktopStore.enableHost/helperAction` → `onRestartReady` → `_quit`, Rust
  relaunches with `--desktop`): post-frame push of the same route.
- `DesktopPage.initState` → microtask `store.load()`.
- The route below (the shell) stays mounted (`maintainState`) and keeps
  rebuilding on AppStore notifies while offstage.

### 3.4 Hosting (this computer shared)
`desktop-share` → `store.enableHost(service, display)` (`run()` sets
`busy`) → FFI `host_enable` → possibly `restart` (helper install → app
quits and is relaunched with `--desktop`) → otherwise invitation +
background snapshot → `trustStatus` → `poll`. The page then shows Copy and
Stop. `DesktopSharingBanner` appears above every route. Stop (page or
banner) → `store.stopHost()` (`host_stop`). Advanced: allow-control switch
→ `allowControl` (`host_permission`), revoke → `revokeTrust`
(`trust_revoke`), helper install/disable/uninstall → `helperAction`.

### 3.5 Connecting and viewing
1. User pastes invitation → `desktop-connect` → `store.connect(invite,
   relayOnly)`; store sets `_viewerForeground = true`, `_controlWanted =
   true`, opens the viewer session (`_openViewer`: `_clearVideo`,
   `api.connect` with video prefs from `desktop_api.dart:428-447`, frame
   `StreamIterator`, `_consumeFrames`).
2. On return, if `store.connected` and `_viewerOpenSerial` unchanged,
   `_viewer` pushes `DesktopViewer`. If the user pressed "Open" meanwhile,
   the serial changed and no second route is pushed.
3. Frames: `DesktopStore._acceptFrame` (H.264) sets `frame`, `snapshot`,
   `cursorStatus`, creates `DesktopVideoTexture` once, `reset` on new
   generation, `await texture.submit(...)` per frame; native presentation
   events → `_presentVideo` → `videoPresented` + `presentFps` +
   cumulative `presented` feedback (≥ 33 ms apart). JPEG: viewer decodes
   and calls `acknowledgePresented` post-frame.
4. Viewer `_update` on every notify: geometry from the H.264 status,
   `_rendered = videoPresented` (the geometry input is authorised against),
   `setState` → rebuild of the whole Scaffold; `Texture(textureId)`
   (same `ValueKey`) inside the gesture surface's `RepaintBoundary`.
5. `cursorStatus` notifies per frame → `_updateCursor` → surface
   `_cursorPaint` (only the cursor `Positioned` rebuilds).
6. Control: store auto-requests control when fresh and `_controlWanted`
   (`desktop_store.dart:394-410`); manual Request/End control button.

### 3.6 Input path (gesture → host)
`Listener`/`GestureDetector` in `DesktopGestureSurface` →
`onMove/onButton/onWheel/onRelease` → viewer `_move/_send` (pixel
coordinates from `_rendered.display`) → `_interactive` gate →
`DesktopStore.send(rendered, event, mediaEpoch)` (coalesces consecutive
moves, ≤ 16 in flight, ordered via `_inputTail`, carries
`generation/frameId/sequence`) → `DesktopApi.input` → `desktopCall({op:
'input', event: {generation, frame_id, sequence, input}})` →
`PbMapperService.desktopCall` routes `presented/keyframe/input/control/
release` to the **frame worker isolate** if one accepts calls
(`ffi/pb_mapper_service.dart:288-301`) — the isolate that also blocks in
`wait_frame` (P1-3). Keyboard: `Focus.onKeyEvent` → `_keyboard`. Toolbar
buttons → `_click/_key/_send`. Release: `store.release()` (`release` op).

### 3.7 Leaving the viewer
- AppBar back / system back: pops the route; `dispose` →
  `setViewerForeground(false)` → store ends control (`control(false)`),
  cancels reconnect timers; **the stream keeps running** (frames still
  submitted to the texture, bandwidth used) until Disconnect. DesktopPage
  shows Open/Disconnect ("back to picture").
- Disconnect in the viewer AppBar: `await store.disconnect()` then
  `Navigator.pop`. Disconnect on DesktopPage: `store.disconnect()` only.
- App backgrounded / window unfocused: `didChangeAppLifecycleState` →
  `_releaseLocalInput` + `setViewerForeground(false)`; on resume → true and
  an immediate `poll`.
- Single-viewer invariant: `_viewerOpen` + awaiting `route.completed`
  ensures the old viewer's `dispose` (which sets foreground false) runs
  before a new viewer sets it true.

### 3.8 Rebuild / notification fan-out (performance coupling)
- `DesktopStore` notifies: every poll (`finally _notify()`, 250 ms timer,
  `desktop_store.dart:149,363`), media notifies throttled to ≥ 100 ms
  (`:484-487`, `:555-559`, `:599-604`), plus state changes.
- Each notify → `AppStore._notify` (bridge `app_store.dart:42`) → every
  AppStore listener: `OverviewPage` rebuilds the galaxy scene
  (`_sceneFor`, `pages/overview_page.dart:89-92`) even while offstage
  under the viewer route; `TunnelsPage` list (`tunnels_page.dart:52-53`);
  rail `_RelayFoot` (`navigation.dart:180-181`); `RelayPage`
  (`relay_page.dart:75`); Settings `_reseed` (`settings_page.dart:90`);
  the builder's banner (`app.dart:324-332`); tray `_refreshTray`
  (`app.dart:217,234`).
- Viewer: unconditional `setState` per notify (`desktop_viewer.dart:80`);
  `DesktopConnectionBanner` and `DesktopPage` (if under the viewer) also
  rebuild. Once `load()` ran, the 250 ms poll + 4 Hz app-wide rebuild
  continue forever (timer only cancelled in `dispose`/restart).

---

## 4. Contracts and formats (exact names)

### 4.1 Input events produced by the UI (`input` field of `op: 'input'`)
Must deserialize as Rust `Input` (`crates/pb-mapper-desktop/src/lib.rs:246-254`,
`#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]`):
- `{'kind':'move','x':int,'y':int}` — physical display pixels of
  `_rendered.display` (`DesktopDisplay.width/height`), clamped to
  `[0, w-1]` / `[0, h-1]`.
- `{'kind':'button','button':'left'|'right'|'middle','down':bool}`.
- `{'kind':'wheel','horizontal':int,'vertical':int}` — gesture: `round(delta/30)`
  clamped ±100; toolbar: vertical ∓3.
- `{'kind':'key','key':<Key>,'down':bool}` — `Key` is a closed snake_case
  enum: `shift, control, alt, meta, enter, escape, tab, backspace, delete,
  left, right, up, down, home, end, page_up, page_down, a, c, v, x, z`
  (`lib.rs:264-287`; Windows mapping `windows.rs:538-565`). An unknown
  name fails deserialization on the host.
- `{'kind':'text','text':String}` — Unicode insertion (≤ 256 chars from
  the dialog; per character from hardware keys).

### 4.2 `DesktopStatus` fields the UI reads
`state` (`'stopped'|'connecting'|'streaming'|'stale'|'capture_unavailable'|…`),
`message`, `controlling`, `controlAllowed`, `generation`, `frameId`,
`display {id,name,width,height}`, `path`, `codec` (`video.codec`),
`videoWidth/videoHeight` (`video.width/height`), `cursor {x,y,visible}`,
`frameAckMs` (`frame_ack_ms`). JSON names in `ffi/desktop_api.dart:86-119`.
Store getters used by the pages: `snapshot.{host,viewer,background}`,
`capabilities.{host,displays,helper{installed,…}}`, `trust.saved
{service,displayId,…}`, `invitation`, `error`, `busy`, `sharing`,
`connected`, `fresh`, `controlling`, `connectionPhase`, `retryIn`,
`pictureAge`, `presentFps`, `reconnectAttempt`, `recoveryReason`,
`mediaEpoch`, `frame`, `videoTexture.textureId`, `videoPresented`,
`cursorStatus` (ValueNotifier). Methods: `load, enableHost, stopHost,
allowControl, revokeTrust, helperAction, connect, disconnect, control,
release, send, acknowledgePresented, setViewerForeground`.

### 4.3 Native video channel (`MethodChannel('pb_mapper/desktop_video')`)
`create` → textureId; `submit {textureId, bytes, generation, frameId,
ptsUs, width, height, keyframe}`; `reset {textureId}`; `stats {textureId}`;
`dispose {textureId}`; native → Dart `event {textureId, generation,
frameId, width, height, presented, needKeyframe, message}`
(`ffi/desktop_video.dart`). `supported` only on Android/Windows; other
platforms use the JPEG `RawImage` path.

### 4.4 Widget keys tests rely on
Shell: `nav-overview|tunnels|relay|logs|settings`, `rail-relay-status`,
`new-tunnel`, `tunnels-refresh`, `open-desktop`. Desktop: `desktop-share`,
`desktop-stop`, `desktop-invitation`, `desktop-connect`,
`desktop-helper-maintenance`, `desktop-helper-disable`,
`desktop-helper-uninstall`, `desktop-touch-mode`,
`desktop-connection-state`, `desktop-scene-transform`. Test text finders:
`'Open'`, `'Disconnect'`, `'Request control'`, `'Stop sharing'`,
`'This desktop is being shared'`.

### 4.5 Window / platform
`WindowOptions` (`main.dart:54-62`); `windowManager` calls: `minimize`,
`maximize/unmaximize/isMaximized`, `close`, `hide/show/focus/restore/
isMinimized/isVisible`, `setPreventClose(true)`; `DragToMoveArea`. No
fullscreen API used. Android: `EnableImpeller` meta-data and
`android:configChanges` include orientation/keyboard/screenSize,
`windowSoftInputMode="adjustResize"` (`android/app/src/main/AndroidManifest.xml:20,28,30`).
iOS: portrait + both landscapes (`ios/Runner/Info.plist:31-36`).

### 4.6 Desktop l10n keys
Used (51): `desktopAckLatency, Advanced, AllowControl, Connect,
Connecting, Copy, Disconnect, Display, Enable, EnableHelp, EnabledHelp,
EndControl, FrameAge, GestureHelp, HelperDisable, HelperInstall,
HelperUninstall, IdentityMismatch, IdentityMismatchHelp, Keyboard,
LeftClick, Open, OrdinaryOnlyHelp, Paste, RecoveryAttempt, RelayOnly,
Release, RequestControl, RightClick, ScrollDown, ScrollUp, SendText,
Service, Share, Sharing, Stale, StateConnecting, StateDelayed, StateIdle,
StateLive, StateReconnecting, StateRecovering, StateRetry,
StateWaitingDesktop, Stop, StopHelp, Stopped, Title, TouchMode,
TrackpadMode, TrustRevoke` (all `desktop…`).
Dead (42): `desktopBackgroundActive, BackgroundHelp, BackgroundStartup,
BackgroundStopped, BackgroundTitle, Busy, Connected, HelperEnable,
HelperHelp, HelperMaintenance, HelperRestart, HelperRunning,
HelperStopped, HelperTitle, HelperUse, HostUnsupported, Interact,
Invitation, InvitationHelp, Navigate, OrdinaryOnly, Preview,
SecureActive, SecureSetupReady, SecureSetupRepair, SecureSetupRequired,
SecureSetupStopped, SecureSetupTitle, ShareNeedsSetup, ShowInvitation,
TrustControl, TrustOtherRelay, TrustRemember, TrustRememberHelp,
TrustRestore, TrustStartup, TrustStartupEnabled, TrustStartupHelp,
TrustTitle, TrustView, ViewOnly, Waiting` (all `desktop…`).
`desktopGestureHelp` (`app_en.arb:4`, `app_zh.arb:4`) documents the
**old** gestures ("two-finger tap to right-click… Hold to drag").

---

## 5. Invariants and security boundaries (UI side)

1. **Input only against a presented frame.** `_interactive` requires
   `store.controlling` (which requires `fresh`: a frame < 2 s old and, for
   H.264, a native *presented* event for the current generation < 2 s
   old) and `_rendered.generation == frame.generation` and the rendered
   epoch to be current. Every input carries the rendered frame's
   `generation`/`frameId`; coordinates use the rendered frame's display
   geometry. Keep this when the viewer is rebuilt (P0-1 keeps presentation
   ACKs for input authorisation).
2. **Never fully occlude the native texture** with an opaque widget
   (`desktop_viewer.dart:492-493`): presentation (and so ACK + input
   authorisation) depends on Flutter compositing it. State overlays
   (ViewerStates "grey last frame") must be translucent or a colour filter
   over the texture, not an opaque cover.
3. **Release on loss of locality.** Focus loss, app backgrounding, mode
   switch, enabled/epoch change, pointer cancel, second finger during a
   drag → `cancelInput`/`_releaseLocalInput` → `store.release()`; never
   replay queued input across reconnect (store increments `_inputEpoch`).
4. **One viewer at a time.** `DesktopStore.setViewerForeground` is a single
   flag set by viewer init/dispose; `DesktopPage._viewer` serialises routes.
5. **Zoom/pan local.** `DesktopGestureSurface` only transforms the same
   `RepaintBoundary(Texture)`; zoom/pan never call the store (test asserts
   texture element identity and transform survival across epoch change).
6. **Local stop always reachable** while sharing (banner + page), even
   when `busy` (test `desktop_test.dart:280-309`).
7. **Ctrl+Alt+Del not offered** (only via Windows helper policy; string
   `desktopHelperHelp` says unsupported). The viewer has no CAD action.
8. Invitation is a secret: copy-only, never logged by the UI; the text
   field disables autocorrect/suggestions.
9. Window chrome: close hides to tray (`setPreventClose`); caption close
   must call `windowManager.close()`, not exit.

---

## 6. Tests covering this scope

- `test/app_test.dart` (236 lines): helper restart reopens `DesktopPage`
  via `openDesktopOnStartup` and back lands on Overview (`:25-37`); shell:
  wide window has `NavRail` with `nav-{overview,tunnels,relay,logs,
  settings}` (`:39-52`); phone uses `NavBar` (`:54-63`); rail relay status
  opens Relay (`:65-80`); Chinese chrome (`:82-88`); no overflow at
  320x640 zh for each of the five pages via `nav-$page` (`:90-113`); first
  run / SetupState / LocaleController.
- `test/desktop_gesture_test.dart` (182 lines): touch tap → move+left
  down/up; **two-finger tap → right click** at midpoint; disabled ignores
  taps (`:37-75`); pinch keeps the same `Texture` element, later taps map
  through zoom, epoch change keeps the transform (`:77-123`); trackpad
  relative motion, no backwards jump from a lagging cursor, **hold →
  left down**, second finger → release, mouse down/up (`:125-181`).
- `test/desktop_viewer_navigation_test.dart` (125 lines): slow connect +
  "Open" keeps a single viewer route; back releases the guard; viewer
  Disconnect pops and keeps the invitation text; reconnect pushes once.
- `test/desktop_test.dart` (310 lines): host flow from Tunnels
  `open-desktop` → `desktop-share` → background/auto-start/control;
  maintenance disable/uninstall reachable while management lost; Stop
  sharing (`:19-96`); viewer at 390x844 maps mouse hover/tap to pixel
  coordinates (960,540)/(480,270), blocks input before control and when
  stale; optional `DESKTOP_QA` screenshot to `../target/desktop-phone-viewer.png`
  (`:98-224`); store input ordering/release (`:225-278`); compact host flow
  with persistent banner stop while busy (`:280-309`).
- `test/desktop_host_readiness_test.dart` (183 lines): one-click enable via
  `DesktopPage` (`desktop-share`, `desktop-stop`), revoke failure keeps
  sharing (`:70-119`); store-only resume/restart cases.
- `test/desktop_lifecycle_test.dart`, `test/desktop_recovery_test.dart`:
  store/FFI level (no page widgets), owned by the store/FFI maps.
- `test/fake_desktop_api.dart` (209 lines): `FakeDesktopApi` implements
  `DesktopApiClient`; `FakePbMapperApi.desktop` (`fake_pb_mapper_api.dart:14`).
- `test/support.dart`: `pumpApp(…, openDesktopOnStartup)`, `settle`,
  `finish`, `loadRealFonts()` loads GoogleSansFlex, GeistMono, NotoSansSC,
  **only `packages/lucide_icons_flutter/Lucide300`** (`:111-113`) and
  MaterialIcons from `FLUTTER_ROOT` (`:114-124`).
- `test/preview_test.dart` (582 lines): opt-in via
  `PB_MAPPER_UI_PREVIEW_DIR`; captures overview/tunnels/settings/editor/
  setup light+dark, relay sections, phone variants, port mapping. **No
  desktop page or viewer preview.**
- `test/gen_poster_test.dart` (677 lines): opt-in via
  `PB_MAPPER_REGEN_POSTER=1`; renders Overview (dark) + Tunnels (macOS,
  1040x720) and a phone (iOS 393x852) Tunnels→new connect; uses
  `LucideIcons.*300` directly for chips (`:110-140`). The Tunnels capture
  includes the `open-desktop` Material icon today.
- No test covers `window_chrome.dart` / `CaptionButtons` / `PageFrame`
  geometry (flutter test is android, so `drawsCaptionButtons` is false).

---

## 7. Impact on the planned overhaul

### 7.1 Requirement 2 — information architecture (6 rail entries; mobile bar with Remote; Logs into Settings)

What must change:
- `AppPage` (`shell/app_scope.dart:11-50`): add a remote-desktop value
  (e.g. `remote`) between `tunnels` and `relay` (artboard order: Overview,
  Tunnels, Remote desktop, Relay, Logs, Settings — `AppRail.dc.html`);
  `icon` → new `AppIcons.remote` = `monitor`; `label` → new key (rail
  "Remote desktop"/"远程桌面", bar "Remote"/"远程" — two strings);
  `grouped` → true (host card + list on grey sheet, like Tunnels).
- Rail vs bar entry lists must diverge: today both iterate
  `AppPage.values` (`navigation.dart:89`, `:241`) and the rail selection
  index is `AppPage.values.indexOf(page)` (`:31`). Introduce explicit
  lists (rail: 6; bar: overview, tunnels, remote, relay, settings) and
  compute the glass index from the rail list. Decide what a compact window
  does when `_page == logs` (persisted `last_page` = 'logs', or a resize
  from wide to compact while on Logs): map to Settings, and give Settings a
  "Logs" row that opens `LogsPage` (as a pushed route or an in-page
  sub-view) on compact only.
- `_home` switch (`app.dart:360-366`) gains the remote page; the
  `openDesktopOnStartup` push (`app.dart:124-135`) becomes "start on the
  remote page" (`_page = AppPage.remote`, overriding `lastPage`) — keep
  the `--desktop` arg name (`main.dart:39`): the Rust restart hand-off
  emits it. Delete `openDesktop()` (`desktop_page.dart:9-14`) and the
  Tunnels `open-desktop` button (`tunnels_page.dart:33-38`).
- `DesktopPage` becomes a shell page built on `PageFrame` (title bar with
  drag area + caption buttons; primary action "Add computer"), reading the
  store from `AppScope.of(context).store.desktop`. Its `initState →
  store.load()` must not start a forever 250 ms poll: the store needs an
  explicit "page visible / viewer active" lifecycle (start polling on
  page show or active session, stop when neither) — coupling with the
  store map.
- `DesktopSharingBanner` in `MaterialApp.builder` (`app.dart:321-336`):
  the design puts sharing state in the host card (Main artboard). Either
  delete the banner or move it into the shell (e.g. a rail/bar indicator
  or a strip inside the sheet) — it must not sit above the Navigator,
  where it shifts the frameless chrome and sits under macOS traffic
  lights. Keep invariant §5.6 (stop reachable from every page while
  sharing, even when busy) — `desktop_test.dart:280-309` encodes it.
- Doc comments with "four/five pages" (`app.dart:37-38`,
  `navigation.dart:18,169,223`, `app_scope.dart:10`) and
  `docs/ui-design.md:10-11,21-27,197-199` + `CLAUDE.md` page list.
- Rail geometry vs artboard: code `kRailWidth 212`, row 38, gap 2, icon
  18, text 13.5; artboard 232 wide, row 40, gap 4, icon 18, text 14,
  selected row radius 12. The brief fixes rail/bar icons at **20 px**
  (Requirement 3) — artboard shows 18; follow the brief unless the owner
  says otherwise. `kRailWidth` is also the drag-strip left edge in
  `app_shell.dart:96`.
- Tests: update `app_test.dart:39-52` and `:90-113` (6 rail keys; bar has
  no `nav-logs`; overflow loop must reach Logs via Settings on phone);
  `desktop_test.dart`, `desktop_host_readiness_test.dart`,
  `desktop_viewer_navigation_test.dart` switch from `open-desktop`/route
  push to `nav-remote`/`pumpApp(page: AppPage.remote)`; `app_test.dart:25-37`
  becomes "restart lands on the Remote page".

### 7.2 Requirement 2 — host sharing card, remote computer list, add computer

- Host card (HostSetup/Main): one "Enable remote control" button + 3-step
  explanation (first enable); permission level segmented "View only /
  View and control" → `store.allowControl(bool)` (`host_permission`); the
  enable call must accept the chosen level (today `enableHost(service,
  display)` always grants control — FFI/store change); invitation
  masked display + Copy + QR (QR shown only after tap, hidden on leaving
  the page — page-local state, never persisted; needs a QR library, e.g.
  `qr_flutter`, through a single widget); display picker; background/UAC
  status rows; Stop + "More" (service name, helper install/disable/
  uninstall, revoke) replacing the `ExpansionTile`. Host viewer info
  ("1 viewer controlling · direct · 12 min") needs new host-status
  fields from Rust (not in `DesktopStatus` today).
- Remote computer list (Main/MobileHome): remembered computers (name,
  service, last path, online status, last seen); the one being viewed
  shows "Back to picture" (today's `store.connected` → "Open" button).
  Requires a new persisted model (encrypted invites: DPAPI on Windows
  exists in Rust trust; Android Keystore new), online status via relay
  service lookup (AppStore already has `relayServices`/`remoteServices` —
  reuse it rather than polling per computer), and a last-frame thumbnail
  (mobile; captured on disconnect, deleted with the computer — note the
  native texture cannot be read back by `RepaintBoundary.toImage` on all
  platforms; plan a native snapshot).
- Add computer (AddComputer/MobileAdd): desktop dialog (use
  `showAdaptivePanel`, `widgets/surfaces.dart:249-282`) with paste field
  showing the recognised host; mobile scan-QR first (camera permission +
  scanner package, frames never stored), paste second; "Remember this
  computer" default on. Reuse the invitation field config from
  `desktop_page.dart:161-171` (no autocorrect, etc.).
- Strings: add new keys grouped in one block in both ARBs; delete the 42
  dead `desktop*` keys (§4.6) and whatever the new pages obsolete.

### 7.3 Requirement 2 — desktop viewer (full window, floating toolbar, menus, six states)

- The viewer must stop being a Material `AppBar` route. Options: keep a
  root-navigator route (it naturally hides the rail) but build it as a
  dark full-window surface with its own 36 px header strip containing
  title, drag area and caption buttons (Viewer artboard draws 44x36
  flush caption buttons on the dark strip) — `CaptionButtons` needs a
  variant parameter (size, radius, dark-ground colours) rather than a
  second implementation. Keep the single-viewer serialisation (§5.4)
  wherever the viewer lives; if it becomes an in-shell state instead of
  a route, `setViewerForeground` must follow visibility the same way.
- Floating toolbar (Glass `floating` over the picture): pin toggle,
  view/control radiogroup (replaces `SegmentedButton` + control button;
  use `GlassSegmented`), Display menu (quality preference Quality/
  Balanced/Smooth → new store/FFI API mapping to P0-2/P0-4 targets;
  display switch; scale), Shortcuts menu (Win+D, Alt+Tab,
  Ctrl+Shift+Esc, Win, Win+L, Ctrl+Alt+Del shown disabled "not
  supported", refresh picture → `keyframe`, release all keys →
  `store.release`), path/latency chip opening the quality overlay, more,
  fullscreen (`windowManager.setFullScreen` — new; `CaptionButtons` must
  then handle `onWindowEnterFullScreen/LeaveFullScreen`), disconnect.
  Unpinned: collapse to a top handle 2 s after the mouse leaves
  (`MouseRegion` + `Timer`); persist pin in `AppPrefs` (new key).
- Six overlay states (ViewerStates): connecting with steps + Cancel
  (→ `disconnect`), slow/quality, waiting for remote desktop (secure
  desktop; maps `waitingForDesktop` + the `needsHelper` message test in
  `desktop_connection_banner.dart:32-34`), reconnecting (grey last frame,
  countdown from `retryIn`, "Retry now" — **no store API today**, add
  one), identity changed (Disconnect / Paste new invitation; maps
  `identityMismatch`), collapsed toolbar view-only notice. Map the nine
  `DesktopConnectionPhase` values onto these explicitly; `recoveringVideo`
  and `delayed` need a decision. Connecting steps need `directStage`/
  `directFailure` (already parsed, unused). Respect §5.2 (no opaque cover).
- Quality overlay metrics: path (`status.path`), network RTT (**not
  available**; needs Rust field, P0-1 periodic probe), picture latency
  (today `frameAckMs` — semantics change with P0-1), display fps
  (`store.presentFps`), bitrate (`status.bitrate`, parsed, unused),
  resolution (`videoWidth x videoHeight`), codec (`status.codec`; HW/SW
  unknown). Update once per second via a narrow listenable, not the
  whole-viewer `setState`.
- Keyboard: extend `_keyNames` and the Rust `Key` enum together
  (`crates/pb-mapper-desktop/src/lib.rs:264-287`,
  `windows.rs:538-565`): F1–F12, Insert, all letters/digits (so Ctrl/Alt/
  Win + any key works), PrintScreen, etc.; today Ctrl+B etc. are dropped
  (`desktop_viewer.dart:244-252`). Lockable (sticky) modifiers are UI state
  that must still be released by `_releaseLocalInput`.
- Remove: AppBar, `DesktopConnectionBanner` as a strip (its info moves
  into the toolbar chip / overlays), `desktopGestureHelp` line, bottom
  toolbar with raw `escape/tab/enter/backspace/Ctrl+A` labels.
- Rebuild discipline (P1-1/P1-3 UI side): replace `_update`'s
  unconditional `setState` with targeted listenables (geometry/epoch,
  phase, metrics) so the `Texture` subtree is not rebuilt per notify; and
  break or narrow the `DesktopStore → AppStore` notify bridge
  (`app_store.dart:42`) — today every media/poll notify rebuilds the
  galaxy scene, Tunnels list, rail foot and refreshes the tray while the
  viewer runs (§3.8). Likely only `sharing`/`activeDesktop` changes need
  to reach AppStore listeners.
- Cursor (P1-3): today cursor rides each video frame status
  (`cursorStatus` set in `_acceptFrame`); the planned "latest cursor"
  FFI read polled per rendered frame would replace the
  `cursorStatus.addListener` path (`desktop_viewer.dart:42,94-109`); keep
  the surface's 500 ms local-echo suppression (`desktop_gesture_surface.dart:76-84`).

### 7.4 Requirement 2 — mobile viewer and gestures (P1-4 invariants)

- Landscape viewer with controls in the side gutters of a 16:9 picture
  and a minimap top-right (MobileViewer): needs `SystemChrome`
  orientation + immersive mode on enter, restored on exit (none exists
  today); trackpad mode **default** (`_trackpad = false` today,
  `desktop_viewer.dart:27`; persist choice in `AppPrefs`).
- Modifier key bar above the soft keyboard (MobileKeyboard): replace the
  `_text` dialog with an always-mounted hidden text input (a
  `TextInputClient`/offstage `TextField` with `enableSuggestions:false`)
  so the IME stays open over the picture; send `text` for characters,
  `key` for Backspace/Enter; sticky Ctrl/Alt/Shift/Win, Fn row, Ctrl+C/V.
- P1-4 invariant (keep scale and offset across soft-keyboard show/hide):
  today the keyboard resizes the viewport (`adjustResize` + Scaffold
  `resizeToAvoidBottomInset`) and `_layout` (`desktop_gesture_surface.dart:275-293`)
  recomputes `_scene` and `_offset`. Lay the picture out against the full
  window (`resizeToAvoidBottomInset: false`, place the modifier bar by
  `MediaQuery.viewInsetsOf`) or make `_layout` ignore inset-only changes.
  Rotation/resize also go through `_layout` — they must never touch the
  store (true today). Keep `RepaintBoundary(Texture)` identity (tested).
- Gestures (RustDesk parity; release-note change): rewrite
  `_down/_movePointer/_up` state machine:
  - tap = left click (exists);
  - **long press = right click** (today long press starts a drag,
    `:191-202`);
  - **double-tap-and-hold then move = drag** (new; needs a double-tap
    window, e.g. `kDoubleTapTimeout` 300 ms / `kDoubleTapSlop`);
  - **three-finger vertical = scroll** (new; today 3 fingers only
    navigate via the scale recogniser — must exclude 3-finger from
    `_scaleUpdate` navigation);
  - two fingers move/pinch = navigate picture (exists, keep);
  - **two-finger tap no longer right-clicks** (`:259-260`);
  - direct-touch optional virtual mouse (new widget over the picture).
  Update `desktop_gesture_test.dart` cases (two-finger right click,
  hold-to-drag) rather than adding matrices; update
  `desktopGestureHelp` (or delete it in favour of the gesture sheet,
  MobileGestures) and `docs/remote-desktop.md:44-47`.
- Mobile bar must hide while viewing (viewer is a full-screen route today,
  so that already holds if it stays a route).

### 7.5 Requirement 3 — icons

- Every Material `Icons.*` in `ui/lib` (Appendix A) must move to
  `AppIcons`. Additionally the stock widgets in the desktop pages draw
  Material glyphs implicitly (AppBar back button, `ExpansionTile`,
  `DropdownButtonFormField`); the redesign removes those widgets or
  passes Lucide icons explicitly (`leading:`, `icon:`, `trailing:`).
  All three `PopupMenuButton`s already pass `AppIcons.more`.
- `AppIcons` (Appendix B): default constants become weight **400**
  (`LucideIcons.<name>400`, `…400Dir` for the four directional ones); add
  separate 300 constants for ≥ 24 px uses. Every current glyph has a 400
  variant in `lucide_icons_flutter-3.1.25` (verified), and all 23 new
  glyph names exist in both 300 and 400 (`route, cloud, cloudOff, monitor,
  screenShare, screenShareOff, monitorSmartphone, eye, mousePointer2,
  keyboard, command, mouse, touchpad, pointer, qrCode, scanQrCode,
  maximize, pin, power, shieldCheck, lockKeyhole, gauge, smartphone`).
  Note `eye` already exists as `AppIcons.show`.
- Swaps: `tunnels` `cable300` → `route`; `online` `wifi300` → `cloud`
  (one use, `pages/tunnel_diagnostics.dart:640`); `offline` `wifiOff300`
  → `cloudOff` (`relay/connections_tab.dart:30`, `relay/services_tab.dart:61`,
  `tunnels_page.dart:126`); new `remote` = `monitor`.
- Current ≥ 24 px uses that need the 300 constant: `galaxy/galaxy_panel.dart:152`
  (`AppIcons.relay`, 28). Empty states (`EmptyNote`, `widgets/surfaces.dart:223`)
  draw at 20 today; the brief sets empty states/tiles to 24/300 — decide
  whether `EmptyNote` grows to 24 (all its call sites follow).
  `IconButton`s without an explicit size inherit 18 (desktop theme,
  `app_theme.dart:614`) or 24 (mobile M3 default) — on mobile those would
  be 24 px at weight 400 unless sized; set the mobile `IconButtonTheme`
  iconSize to 20/18 per the size rules. Rail icon 18 → 20
  (`navigation.dart:147`); bar 20 (`:296`) stays.
- Unused AppIcons members: `latency`, `list`, `check`, `language` (0 uses)
  — delete or use.
- Test fonts: `loadRealFonts` (`test/support.dart:111-113`) must also load
  `packages/lucide_icons_flutter/Lucide400` from
  `assets/build_font/LucideVariable-w400.ttf`, otherwise previews/posters
  draw boxes; the MaterialIcons loader (`:114-124`) can go once no
  Material glyph remains. `gen_poster_test.dart:110-140` uses
  `LucideIcons.*300` directly (poster chips, 16 px) — switch to AppIcons
  or 400 to obey "through app_icons.dart".
- `docs/ui-design.md:204` ("Icons are Lucide at its 300 stroke") must be
  rewritten with the weight/size rules.

### 7.6 Requirement 4 — caption buttons

- `page_frame.dart` `_TitleBar`: end padding 8 → 16 **when
  `drawsCaptionButtons`** (`:120`), gap to primary action 10 → 16
  (`:167`); keep 8 otherwise (macOS/mobile).
- `window_chrome.dart` `_CaptionButton`: width 44 → 46, height 36 → 32
  (`CaptionButtons(height: 36)` at `page_frame.dart:168`; default 40;
  SetupFlow passes 34 at `setup_flow.dart:200`), radius 10 → 6, vertically
  centred in the 58 px bar (Row in a `centerStart` Stack already centres).
  Replace `Icon(Icons.remove/crop_square/filter_none/close)` with a
  `CustomPainter` glyph: 10 px box, 1 px stroke, pixel-snapped (use
  `MediaQuery.devicePixelRatioOf` to snap the stroke to device pixels at
  100/125/150 %). TitleBarFix artboard: minimize `M0 5h10`, maximize
  `rect(0.5,0.5,9,9,rx1)`, close `M0.5 0.5l9 9M9.5 0.5l-9 9`; restore
  (two offset squares) is **not drawn** in the artboard — design it in
  the same 10 px box. Remove `iconSize` param. Hover colours stay
  (`0xFFC42B1C` close; onSurface 8 % others).
- Acceptance (≥ 16 px from sheet edge to hovered red) is geometry only;
  no automated test exists — add one focused widget test with
  `debugDefaultTargetPlatformOverride = TargetPlatform.windows` measuring
  the close button rect against the `ContentSheet` rect (one test, not a
  scaling matrix), or verify manually at 100/125/150 %.
- `docs/ui-design.md:193-195` caption sizes.

### 7.7 Requirement 1 touchpoints in this scope

- P0-4 targets are set in `DesktopApi.connect` (`ffi/desktop_api.dart:428-447`,
  FFI map); the UI adds the Display-menu quality preference that feeds
  it (persist in `AppPrefs`; Quality / Balanced / Smooth). The viewer must
  not reconnect to apply it if the store can renegotiate in-session.
- P1-1: UI-side rebuild coupling (§3.8, §7.3) adds main-isolate work per
  frame and per poll; fix alongside the pipeline change. The JPEG path
  (`_decode`) is iOS/macOS/Linux only and can stay.
- P1-3: input calls from the viewer go through `store.send` → frame worker
  isolate (`pb_mapper_service.dart:288-301`); the UI does not need to change
  for a routing fix, but sticky modifiers / shortcut combos add bursts of
  key events — keep the store's ordering barrier and 16-in-flight bound.
- P1-4: invariants in §7.4; keep `desktop_gesture_test.dart` pinch/epoch
  case.
- Presentation display fps (`presentFps`) feeds "cap host fps at 90 % of
  measured" — surfaced in the quality overlay.

### 7.8 Cross-subsystem coupling summary

| Change | Couples with |
| --- | --- |
| New `Key` names, shortcut combos | `crates/pb-mapper-desktop/src/lib.rs:264`, `windows.rs:538` (host input map) |
| View-only at enable, host viewer info | FFI `host_enable`/`status` (`ui/native/pb_mapper_ffi/src/desktop.rs`), host session status |
| Quality preference, RTT metric | `desktop_api.dart` connect prefs, Rust congestion/probe (P0-1/P0-2) |
| Retry-now, polling lifecycle, notify bridge | `state/desktop_store.dart`, `state/app_store.dart:38-42` |
| Remembered computers, Keystore, online status | new store + FFI; `AppStore.remoteServices` |
| QR show / scan | new packages (pubspec), camera permission (Android/iOS manifests) |
| Last-frame thumbnail | native texture readback (`DesktopVideo.kt` / Windows plugin) |
| Latest-cursor read | FFI (`pb_mapper_ffi/src/desktop.rs`), replaces `cursorStatus` listener |
| Gesture change | `docs/remote-desktop.md:44-47`, release notes, `desktopGestureHelp` |

---

## Appendix A — every Material `Icons.*` use in `ui/lib` (18 references on 17 lines)

| File:line | Icon | Context | Proposed AppIcons |
| --- | --- | --- | --- |
| `lib/src/shell/window_chrome.dart:111` | `Icons.remove` | caption minimize | self-drawn glyph (Req 4) |
| `lib/src/shell/window_chrome.dart:117` | `Icons.filter_none` | caption restore (maximized) | self-drawn glyph |
| `lib/src/shell/window_chrome.dart:117` | `Icons.crop_square` | caption maximize | self-drawn glyph |
| `lib/src/shell/window_chrome.dart:126` | `Icons.close` | caption close | self-drawn glyph |
| `lib/src/pages/tunnels_page.dart:37` | `Icons.desktop_windows_outlined` | Tunnels title-bar `open-desktop` | delete (rail entry `monitor`) |
| `lib/src/pages/desktop_viewer.dart:347` | `Icons.keyboard_outlined` | type text | `keyboard` |
| `lib/src/pages/desktop_viewer.dart:352` | `Icons.ads_click` | left click | `mousePointer2` / `pointer` |
| `lib/src/pages/desktop_viewer.dart:357` | `Icons.mouse_outlined` | right click | `mouse` |
| `lib/src/pages/desktop_viewer.dart:368` | `Icons.arrow_upward` | scroll up | removed (three-finger scroll) |
| `lib/src/pages/desktop_viewer.dart:379` | `Icons.arrow_downward` | scroll down | removed |
| `lib/src/pages/desktop_viewer.dart:407` | `Icons.pan_tool_outlined` | release keys/buttons | Shortcuts-menu item (text) |
| `lib/src/pages/desktop_connection_banner.dart:55` | `Icons.check_circle` | live state | status dot |
| `lib/src/pages/desktop_connection_banner.dart:61` | `Icons.link_off` | idle / identity mismatch | `unlink` (state overlays per ViewerStates) |
| `lib/src/pages/desktop_page.dart:102` | `Icons.copy` | copy invitation | `copy` |
| `lib/src/pages/desktop_page.dart:108` | `Icons.stop_circle_outlined` | stop sharing | `screenShareOff` |
| `lib/src/pages/desktop_page.dart:136` | `Icons.screen_share_outlined` | enable remote control | `screenShare` |
| `lib/src/pages/desktop_page.dart:189` | `Icons.desktop_windows_outlined` | connect | `monitor` |
| `lib/src/pages/desktop_page.dart:324` | `Icons.screen_share_outlined` | sharing banner | `screenShare` |

(18 rows: line 117 carries two icons.) No `CupertinoIcons` use; `cupertino_icons`
is still a dependency (`pubspec.yaml:43`). `uses-material-design: true`
(`pubspec.yaml:118`) is needed as long as any stock widget draws a Material
glyph. Implicit Material glyphs: `desktop_page.dart:57` (AppBar back),
`:194` (ExpansionTile), `:207` (Dropdown); `desktop_viewer.dart:261`
(AppBar back). Tests: no `Icons.*` use; `gen_poster_test.dart:111-140`
uses `LucideIcons.*300` directly.

## Appendix B — every `AppIcons` member (`lib/src/theme/app_icons.dart`, 56 members, all weight 300)

| Member (line) | Lucide glyph | Uses in `lib/` |
| --- | --- | --- |
| `overview` (9) | `orbit300` | 2 |
| `tunnels` (10) | `cable300` → swap `route` | 1 (`app_scope.dart:20`) |
| `logs` (11) | `scrollText300` | 1 |
| `settings` (12) | `settings300` | 1 |
| `share` (15) | `arrowUpFromLine300` | 5 |
| `connect` (16) | `arrowDownToLine300` | 6 |
| `relay` (17) | `radioTower300` | 18 (one at 28 px, `galaxy_panel.dart:152`) |
| `direct` (20) | `zap300` | 5 |
| `mixedPath` (23) | `split300` | 2 |
| `carrierTcp` (27) | `arrowRightLeft300` | 1 |
| `carrierQuic` (28) | `layers300` | 3 |
| `latency` (29) | `activity300` | 0 |
| `computer` (30) | `laptop300` | 6 |
| `service` (31) | `server300` | 5 |
| `diagnostics` (34) | `stethoscope300` | 3 |
| `router` (35) | `router300` | 2 |
| `add` (38) | `plus300` | 4 |
| `close` (39) | `x300` | 6 |
| `copy` (40) | `copy300` | 10 |
| `edit` (41) | `pencil300` | 2 |
| `delete` (42) | `trash2300` | 3 |
| `start` (43) | `play300` | 2 |
| `stop` (44) | `square300` | 3 |
| `refresh` (45) | `refreshCw300` | 5 |
| `search` (46) | `search300` | 3 |
| `more` (47) | `ellipsis300` | 3 |
| `export` (48) | `fileUp300` | 5 |
| `import` (49) | `clipboardPaste300` | 2 |
| `external` (50) | `externalLink300` | 1 |
| `clear` (51) | `eraser300` | 2 |
| `follow` (52) | `arrowDown300` | 1 |
| `list` (53) | `list300` | 0 |
| `check` (56) | `check300` | 0 |
| `ok` (57) | `circleCheck300` | 6 |
| `info` (58) | `info300` | 4 |
| `warning` (59) | `triangleAlert300` | 8 |
| `error` (60) | `circleAlert300` | 2 |
| `online` (61) | `wifi300` → swap `cloud` | 1 (`tunnel_diagnostics.dart:640`) |
| `offline` (62) | `wifiOff300` → swap `cloudOff` | 3 |
| `lock` (63) | `lock300` | 4 |
| `key` (64) | `keyRound300` | 13 |
| `link` (65) | `link300` | 5 |
| `unlink` (66) | `unlink300` | 2 |
| `show` (67) | `eye300` | 3 |
| `hide` (68) | `eyeOff300` | 1 |
| `themeSystem` (71) | `sunMoon300` | 2 |
| `themeLight` (72) | `sun300` | 1 |
| `themeDark` (73) | `moon300` | 1 |
| `language` (74) | `languages300` | 0 |
| `star` (75) | `star300` | 1 |
| `update` (76) | `circleFadingArrowUp300` | 3 |
| `download` (77) | `download300` | 1 |
| `chevronDown` (80) | `chevronDown300` | 2 |
| `chevronRight` (81) | `chevronRight300Dir` | 5 |
| `back` (82) | `arrowLeft300Dir` | 1 |
| `forward` (83) | `arrowRight300Dir` | 1 |

Total `AppIcons.*` references in `lib/`: 177 across 21 files. Font family per
weight: `packages/lucide_icons_flutter/Lucide{100..600}` →
`assets/build_font/LucideVariable-w{N}.ttf` (package pubspec).
