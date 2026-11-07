# Design spec: remote desktop redesign artboards → Flutter implementation

Scope: every `*.dc.html` artboard in
`scratchpad/design/project/` (listed in `canvas.json`), read in full, turned
into a per-screen spec, cross-checked against the current Flutter code in
`ui/lib/` so each element says whether today's app can feed it.

Citation conventions: `Main:52-59` = line 52-59 of `Main.dc.html` (artboard
files are named without `.dc.html`); code paths are relative to
`ui/lib/src/` unless they start with `crates/`, `ui/native/` or `docs/`.
**[GAP]** = data/API the current app cannot provide. **[PROPOSED]** = not
drawn on any artboard; a suggestion the implementer may change. **[DEVIATION]**
= the artboard disagrees with the existing shell or with another artboard;
the resolution is stated.

---

## 1. Overview

15 artboards (`canvas.json:7-23`), titled in Chinese, in three rows plus
icon/title-bar fixes:

| Artboard | Size | Canvas title | What it is |
|---|---|---|---|
| Main | 1440×900 | 桌面 · 远程桌面页（正在共享） | Desktop "Remote desktop" page while sharing: host card (left, 560 px) + remote-computer list (right) |
| HostSetup | 1440×900 | 桌面 · 首次开启远程控制 | Same page before the first enable: 3-step explanation + one button; empty remote list |
| AddComputer | 1440×900 | 桌面 · 添加远程电脑 | Modal dialog over the page: paste invite, recognised host, name, remember |
| Viewer | 1440×900 | 桌面 · 查看器（控制中，快捷键菜单） | Full-window viewer, dark, floating toolbar, Shortcuts menu open |
| ViewerStates | 1440×1400 | 桌面 · 查看器状态 | Six 680×383 thumbnails of overlay states |
| MobileHome | 390×844 | 手机 · 远程标签页 | Phone "Remote" tab |
| MobileAdd | 390×844 | 手机 · 扫码添加电脑 | Bottom sheet: camera QR scan first, paste second |
| MobileViewer | 844×390 | 手机 · 横屏查看器（触控板模式） | Landscape viewer, controls in the side gutters |
| MobileKeyboard | 844×390 | 手机 · 键盘与修饰键条 | Soft keyboard up: modifier bar, zoomed picture, minimap |
| MobileGestures | 390×844 | 手机 · 操作方式 | Bottom sheet: trackpad/direct touch + gesture list |
| AppRail | 232×900 | 组件 · 桌面侧栏 | Desktop rail with 6 entries |
| RemoteScreen | 1280×800 | 组件 · 远程画面占位 | Fake Windows desktop picture (placeholder only — do not implement; usable as a preview/poster fixture) |
| MobileTabBar | 390×84 | 组件 · 手机底栏 | Phone bottom bar with 5 entries |
| Icons | 1440×2440 | 图标系统 | Icon rules, weight comparison, caption glyphs, full AppIcons catalogue with real Lucide codepoints |
| TitleBarFix | 1440×820 | 待修 · 标题栏窗口按钮与内容页边缘重叠 | Caption-button crowding fix, before/after, table |

Sticky notes in `canvas.json:43-48` are part of the spec (quoted where used):
desktop rules (`canvas.json:43`), mobile rules (`:45`), icon rules (`:48`).

**Global reading rules for the artboards**

- The artboards were drawn in HTML with Inter/Noto Sans SC and hand-drawn
  SVG icons. Implement with the app's fonts (`theme/fonts.dart`: Segoe UI on
  Windows, Google Sans Flex elsewhere, Noto Sans SC fallback, Geist Mono via
  `mono()`) and with Lucide glyphs via `AppIcons` (§2.13). Only `Icons.dc.html`
  shows real Lucide glyphs (`Icons:21`, `canvas.json:48` last line).
- Shell chrome drawn on the artboards (rail 232 px, 64 px page header, 40 px
  rail rows) approximates the existing shell; keep the existing shell metrics
  (rail `kRailWidth = 212`, `navigation.dart:13`; page bar 58 px,
  `page_frame.dart:106`) and apply only the changes the brief/notes ask for
  (§2.1, §2.14). [DEVIATION, resolved in favour of the existing shell.]
- Desktop management pages are light (POCOX grouped sheet); both viewers are
  always dark regardless of app theme.
- Pixel values below are logical pixels at 100 %.

---

## 2. Components and responsibilities (per screen)

### 2.0 Token table (artboard hex → existing theme token)

Light (management pages; `theme/app_theme.dart`):

| Artboard value | Use | Token (file:line) |
|---|---|---|
| `#EAEAE8` | window backdrop; segmented track | `backdropBase` (`app_theme.dart:137-139`); `fieldFill` light (`:111`) |
| `#F4F4F2` | content sheet (grouped) | `sheetColor(grouped:true)` (`:167-170`) = `surfaceContainerLow` |
| `#FFFFFF` | cards, dialog, sheets | `raisedCardColor` (`:201-204`) |
| `#1D1D1F` | ink text, filled buttons | `onSurface` / `primary` (`:21,27`) |
| `#4A4A4D` | labels, secondary text, idle icons | `onSurfaceVariant` (`:49`) |
| `#6E6E73` | help/footnote text | **new token** `quietText(scheme)` light `#6E6E73` / dark `#8E8E93` [PROPOSED]; `onSurfaceMuted` (`:100`) is for glyphs only |
| `#8E8E93` | offline icon/dot, disabled text, chevrons | `onSurfaceMuted` (≈#838384) |
| `#F2F2F0` | wells inside white cards (fields, panels, tiles) | `cardFieldFill` (`:116-118`) exactly |
| `#E4E4E2` / `#EFEFED` | hairlines, row dividers | `outlineVariant` (`:62`) |
| `#C8C8C6` | dashed add-tiles, outlined small button border | `outline` (`:61`) |
| `#EDEDEB` | grey status pill fill | `surfaceContainer` (`:54`) |
| `#1B6FC8` | blue signal (this computer, viewing, links) | `tertiary` = `signalColor` (`:37,86`) |
| `#E5EFFA` / `#124A8C` | blue tile fill / blue pill text | `tertiaryContainer` / `onTertiaryContainer` (`:39-44`) |
| `#F4F8FD` | "viewing" row and "recognised host" card fill | `tertiaryContainer.withValues(alpha: 0.45)` over white (computes to #F3F8FD) [PROPOSED helper] |
| `#1A7F37` / `#E3F2E7` | green dot+text / green pill fill | `successColor` (`:89-91`) / `successColor@0.13` (StatusChip recipe, `widgets/status.dart:139-142`) |
| `#C42B1C` | close-button hover | literal already in `shell/window_chrome.dart:167` |
| `rgba(20,20,22,0.32)` | desktop dialog scrim | `barrierColor: Color(0x52141416)` |
| `#D1D1CF` 36×5 | sheet grabber | bottom-sheet theme drag handle (`app_theme.dart:549-551`, 36×4 `outline`) — keep theme |

Dark (viewers; always dark):

| Artboard value | Use | Token |
|---|---|---|
| `#0F0F10` | viewer ground, letterbox | `backdropBase` dark (`app_theme.dart:139`) |
| `#000000` | mobile viewer ground | literal black |
| `#EDEDEF` | primary text | ≈ dark `onSurface` `#E6E6E6` |
| `#B4B4B8` | secondary text, shortcut hints | ≈ dark `onSurfaceVariant` `#BDBDC0` |
| `#8E8E93` | disabled, "pb-mapper" in title | `quietText` dark [PROPOSED] |
| `#1C1C1E` | state cards, modifier bar | **viewer token** `viewerCard` |
| `#262628` | menus | dark `surfaceContainer` (`app_theme.dart:54`) |
| `rgba(32,32,35,0.9)` + blur 20 | toolbar, handle, small pills | **viewer token** `viewerChrome` (not `Glass` floating: its fill is `#28282B@0.62`, `glass.dart:126-133`) |
| `rgba(28,28,30,0.94–0.95)` | quality panel, reconnect pill | `viewerPanel` |
| `rgba(24,24,26,0.86–0.88)` | mobile hint pills | `viewerHint` |
| `rgba(255,255,255,0.08)` | hairlines on dark | `viewerEdge` |
| `rgba(255,255,255,0.06/0.10/0.12/0.14/0.16/0.18)` | segmented track / button fill & pressed / separators / key separators / selected segment / outlined button border | literals in one `viewer_palette.dart` [PROPOSED file] |
| `#57C27A` `#E0A647` `#F2B8B5` `#7DB6F2` | success, caution, error, signal | dark `successColor` (`:91`), `cautionColor` (`:96`), `error` (`:68`), `tertiary` (`:37`) |

Implement viewers inside `Theme(data: buildAppTheme(Brightness.dark))`
(`app_theme.dart:639-642`) so `ColorScheme` tokens resolve to the dark values;
keep the viewer-only translucent literals in one file.

Radii (`theme/metrics.dart`): cards 20 = `kCardRadius` (`:18`); rows/fields 14
= `kRowRadius`/`kFieldRadius` (`:15,30`); dialogs 24 = `kDialogRadius` (`:21`);
menus 14 = `kMenuRadius` (`:33`); toolbar & state cards 16 = `kPanelRadius`
(`:9`); small buttons 10 = `kControlRadius` (`:12`); sheet 18 = `kSheetRadius`;
new literals: menu items 9, key caps 8, segmented outer 12/inner 9 (light) and
11/8 (viewer), caption buttons 6.

Type sizes used across artboards: 28/700 (mobile large title), 22/600
(setup headline), 20/600 (page title / mobile sheet title), 18/600 (dialog
title), 17/600 (card title), 15–15/600 (row names, state-card titles), 14.5/600
(step titles), 13.5 (body/rows), 13 (labels, buttons), 12.5 (help, meta), 12
(pills, mono hints), 11.5/11/10.5 (badges, tab labels, gutter labels). Mono
(Geist Mono via `mono()`, `fonts.dart:80-92`): service names, invites, metrics,
key caps, shortcut hints, relay address.

Motion: use `Motion.fast/medium/slow` and curves (`theme/motion.dart:15-31`);
no artboard specifies durations.

### 2.1 AppRail (desktop rail) — `AppRail:16-52`

- 6 entries in this order (`AppRail:23-46`): 总览 (overview), 隧道 (tunnels),
  **远程桌面** (new, shown selected), 中继 (relay), 日志 (logs), 设置 (settings).
- Selected row: white 0.82 + soft shadow, weight 600, ink (`AppRail:31`) — this
  is today's glass pill (`navigation.dart:69-86`, `surfaceSelection`
  `app_theme.dart:160-163`). Keep the existing pill/spring.
- Icons per icon system: 20 px, weight 400 (`Icons:125-127`, brief §3). Today
  the rail draws 18 px (`navigation.dart:147`) → change to 20.
  [DEVIATION: the AppRail artboard draws 18 px SVGs; the icon system wins.]
- Brand row: keep `BrandMark(size: 26)` + "pb-mapper" 15/600
  (`navigation.dart:42-63`; artboard `AppRail:17-22`).
- Foot (`AppRail:48-51`): dot 7 green + "中继在线 · 23 ms" 12 px, then relay
  address mono 11.5 `#6E6E73` indented 15. Today `_RelayFoot` shows
  `RelayChip` + address (`navigation.dart:170-221`) **without the latency**.
  Latency is available: `AppStore.relay?.latencyMs` (`ffi/pb_mapper_api.dart:381`,
  already used in `pages/settings_page.dart:186`). Append " · {n} ms" when
  online and known.
- Metrics drawn (232 wide, padding 20/12/16, rows 40, radius 12, gap 4, font
  14) are NOT a change request; keep `kRailWidth` 212, rows 38, 13.5 px.

### 2.2 MobileTabBar (phone bottom bar) — `MobileTabBar:16-32`

- 5 entries (`:17-31`): 总览, 隧道, **远程** (selected), 中继, 设置. **日志 is
  removed from the bar** and moves into Settings (`canvas.json:45` "日志移到设置里").
- Bar: height 84 incl. bottom safe area (padding 6/8/26), fill
  `rgba(251,251,250,0.96)`, top hairline 1 px `#E4E4E2` (`:16`). Today `NavBar`
  uses `surfaceContainerLow` with no border (`navigation.dart:233-252`) — add a
  1 px `outlineVariant` top border; keep SafeArea for the inset.
- Item: icon pill 52×28 radius 14, selected fill `rgba(0,0,0,0.07)` (≈
  `softWash(strength:1.2)`); label 11 px, selected 600 ink, idle `#6E6E73`
  (`:17-31`). Today 54×30 pill, 11.5 px label (`navigation.dart:285-307`) —
  within 2 px; keep. Icon 20 px weight 400 (artboard draws 22; icon system
  says 20 — `Icons:125`).
- Label of the new entry is **"远程"** on the bar but **"远程桌面"** on the rail.

### 2.3 Main — desktop Remote desktop page while sharing (`Main:17-168`)

Layout (`Main:17-38`): rail | content sheet (margins 8/8/8/0, radius 18,
`#F4F4F2` grouped). Header 64 tall (`Main:22`) → **use `PageFrame`**
(58 px bar, `page_frame.dart:38-88`) with:
- title "远程桌面" (`Main:23`);
- `actions`: refresh icon button 36×36 radius 10, icon `refresh-cw` 18,
  `#4A4A4D`, aria "刷新" (`Main:24-26`);
- `primary`: `PrimaryAction(icon: add, label: "添加电脑")` — ink pill 36 tall,
  padding 0 16, 13.5/500, plus 16 (`Main:27-30`);
- caption buttons per TitleBarFix (§2.14) — the artboard already draws them
  46×32 with 16 px gap/right padding (`Main:22,31-35`).

Body grid (`Main:38`): padding 4/32/32/32, two columns `560px | 1fr`, gap 20,
`align-items: start` (cards are content-height). **[PROPOSED]** responsive
rule: two columns when the sheet's inner width ≥ 560 + 20 + 420; otherwise one
column (host card above the list, both full width, max 720). Scroll the whole
body (ListView/SingleChildScrollView) when taller than the window.

#### 2.3.1 Host card, sharing state (`Main:40-110`)

White card radius 20, padding 24, vertical gap 20, shadow `0 1 2 rgba(0,0,0,0.04)`
(≈ `RaisedSurface`/`GroupCard` with `kCardRadius`, `widgets/surfaces.dart:10-40,73-124`).
Top to bottom:

1. **Header row** (`Main:41-50`), gap 14:
   - tile 44×44 radius 12, fill `#E5EFFA` (tertiaryContainer), icon
     `monitor` 22 → use 20 px/400 or 24 px/300 per rules (prefer 24/300 "tile"),
     colour `#1B6FC8` (blue = this computer);
   - title "这台电脑" 17/600; below it service name mono 12.5 `#6E6E73`, e.g.
     `desktop:studio-pc`;
   - status pill right: 26 tall, padding 0 10, radius 13, fill `#E3F2E7`, dot 7
     `#1A7F37`, text "正在共享" 12.5/500 **in the status colour** (`Main:49`).
     [DEVIATION from `StatusChip`, whose label is `onSurfaceVariant`
     (`status.dart:122-127`)] → add a `StatusChip(inkLabel: true)` option.
   - Data: service = `DesktopBackground.service` (`ffi/desktop_api.dart:175`) or
     `DesktopTrust.saved.service` (`:197`); sharing = `DesktopStore.sharing`
     (`state/desktop_store.dart:122-123`). Available.
2. **Viewer panel** (`Main:52-59`), shown only while someone is connected
   [PROPOSED rule]: fill `#F2F2F0`, radius 14, padding 14/16, gap 12;
   icon `smartphone` 20 `#1B6FC8`; title "1 个查看者正在控制" 14.5/600; meta
   "直连 · 已连接 12 分钟" 12.5 `#4A4A4D`; button "改为仅查看" (32 tall, padding
   0 12, radius 10, 1 px `#C8C8C6`, white, 13 px).
   - Viewer count **[GAP]**: host status is one `SessionStatus`
     (`crates/pb-mapper-desktop/src/lib.rs:126-165`) with `state`
     (`waiting`/`streaming`/…) and `controlling`; no count. "1 个查看者正在…"
     can be derived as 1 when `snapshot.host.state == 'streaming'`, but a
     count, the viewer's **path** (host-side `path` is never set — only the
     viewer sets it, `crates/pb-mapper-desktop/src/viewer.rs:175-178,236`) and
     **connected-since** are absent → host status needs `viewers`,
     `viewer_path`, `viewer_since_ms` (or equivalent).
   - Viewer device type for the icon **[GAP]**: unknown; use `smartphone` only
     if known, else `monitor-smartphone` [PROPOSED].
   - Watching-not-controlling variant text "1 个查看者正在查看" [PROPOSED].
   - "改为仅查看" = `DesktopStore.allowControl(false)` (`desktop_store.dart:855-872`);
     it flips the permission segmented control below too. Hide the button when
     the viewer is not controlling and control is already disallowed.
3. **Permission** (`Main:61-67`): label "持有邀请的人可以" 13 `#4A4A4D`; segmented
   control track `#EAEAE8` radius 12 padding 3 gap 3, segments 34 tall radius 9,
   selected white + `0 1 2 rgba(0,0,0,0.08)` 13.5/600 ink, idle transparent
   13.5 `#4A4A4D`; options **"仅查看画面" | "查看并控制"**.
   Use `GlassSegmented` (`theme/glass.dart:718-848`, height 34, inset 3) —
   [DEVIATION: it draws capsule corners, the artboard draws radius 12/9; accept
   capsule or add a `radius` parameter]. Value = `snapshot.host.controlAllowed`
   (or `trust.saved.allowControl`); change → `DesktopStore.allowControl(bool)`.
   Available.
4. **Private invite** (`Main:69-83`): label "私密邀请"; well 48 tall radius 14
   fill `#F2F2F0` padding 0/8/0/16, gap 8:
   - masked invite, mono 13, letter-spacing 0.2, single line, clipped:
     `pbd1·k7Hq••••••••••••••••••••Zx2Q` — i.e. format prefix `pbd1·` + first 4 +
     20 bullets + last 4 of the payload. **[GAP]** today's invite is the raw
     JSON of `Invitation {version, service, certificate, token}`
     (`crates/pb-mapper-desktop/src/lib.rs:91-122`, certificate = base64url DER,
     `security.rs:41`, ~650 chars). A `pbd1` compact text form is implied by the
     design (and needed for a QR that scans reliably, §2.8). Never render the
     full secret on screen; mask both ends as above.
   - button "复制" (copy 16, 32 tall, padding 0 12, radius 10, 1 px `#E4E4E2`,
     white, 13 px) → `Clipboard.setData(store.invitation)` (as today
     `pages/desktop_page.dart:96-104`). Toast "已复制" via existing toast helper.
   - button "二维码" (`qr-code` 16, same style) → **reveals the QR** **[GAP:
     no QR generator dependency in `ui/pubspec.yaml:35-55`]**. Rules from the
     brief: shown only after tapping, hidden when leaving the page, only on
     this screen. Presentation not drawn **[PROPOSED]**: expand an inline
     panel under the well (white plate, 200×200 QR, 16 px quiet zone, caption
     "用手机扫描，添加这台电脑", button toggles to "隐藏二维码"); dispose on
     page leave/app background.
   - help text 12.5/1.5 `#6E6E73`: "只发给你信任的设备。持有邀请的人可以查看，允许控制时也可以操作。"
5. **Settings list** (`Main:85-100`): top hairline, rows 48 tall, 13.5 px,
   label `#4A4A4D` flexible, value right; row dividers 1 px `#E4E4E2`:
   - "共享的显示器" → dropdown text "显示器 1 · 2560 × 1440" + `chevron-down` 16.
     Data: `DesktopCapabilities.displays` (id, name, width, height;
     `desktop_api.dart:13-25,27-38`). Label = "显示器 {index+1} · {w} × {h}"
     (do not show the raw OS device name). Changing it **while sharing
     [GAP]**: the display is fixed at `enableHost(service, display)`
     (`desktop_store.dart:769-815`); there is no live switch op. Either restart
     the host with the new display or add an op.
   - "后台运行" → "Windows 服务 · 关闭窗口后继续共享". Data:
     `snapshot.background.enabled` (`desktop_api.dart:161-184`). When not
     background: [PROPOSED] "随应用运行 · 关闭窗口后在托盘继续共享".
   - "UAC 与锁屏" → `check` 16 green + "辅助服务已就绪". Data:
     `capabilities.helper` (installed/running/identityVerified,
     `desktop_api.dart:40-57`). Other values [PROPOSED, reuse ARB]:
     stopped → "辅助服务已停止" (`desktopHelperStopped`); none → "仅普通桌面"
     (`desktopOrdinaryOnly`); unverified → "需要更新辅助服务".
6. **Actions row** (`Main:102-109`), gap 10:
   - "停止共享" outlined ink: 40 tall, padding 0 18, radius 20, 1 px `#1D1D1F`,
     white, 14/500 → `DesktopStore.stopHost()` (`desktop_store.dart:817-834`);
   - "更多" ghost: `ellipsis` 16 + text, 40 tall, padding 0 14 → menu. Contents
     per brief ("helper service, service name, revoke live under More")
     [PROPOSED layout]: "桌面服务名称…" (rename; today editable only before
     sharing, `desktop_page.dart:200-206`), "安装／更新并重启应用"
     (`desktopHelperInstall`), "停用辅助服务", "卸载辅助服务"
     (`desktopHelperDisable/Uninstall`), divider, "撤销邀请并停止共享" (error
     colour, confirm via `confirmDestructive`, `widgets/surfaces.dart:284-315`)
     → `revokeTrust()` (`desktop_store.dart:295-314`). Operations exist
     (`helperAction`, `revokeTrust`).
   - right-aligned note 12.5 `#6E6E73`: "停止后邀请保留，下次开启继续可用".
     This is only true for the remembered/background grant (ARB
     `desktopInvitationHelp`: temporary invites die on stop). The new enable
     flow must always create the remembered grant.

**Host card state machine** (drawn: Sharing = Main, NotEnabled = HostSetup;
others [PROPOSED]):

| State | Condition (today's data) | Card |
|---|---|---|
| Unsupported | `!capabilities.host` (`desktop_api.dart:34`) | No host card on desktop; list goes full width; one note line "目前仅 Windows 可分享桌面。此设备可以查看远程桌面。" (`desktopHostUnsupported`) [PROPOSED] |
| NotEnabled | host supported, `trust.saved == null`, `!background.configured` | HostSetup card (§2.4) |
| Enabling | `store.busy` during `enableHost` / `restarting` | HostSetup card, button shows spinner + "正在开启…" [PROPOSED]; step 1/2 highlight follows |
| Sharing | `store.sharing` | Main card (above) |
| Paused | `!sharing && (trust.saved != null \|\| background.configured)` | Main card minus viewer panel; pill "未共享"; primary "开启共享" (`screen-share`) instead of "停止共享" → `restoreHost()` / `backgroundAction('start')` (`desktop_store.dart:282-293,213-223`) [PROPOSED] |
| Error | `store.error != null` | inline error line under the header in `error` colour [PROPOSED] |

#### 2.3.2 Remote computer list (`Main:112-165`)

White card radius 20, padding 20/20/24, gap 4.
- Header (`:113-116`): "远程电脑" 17/600 + count badge (min 20×20, radius 10,
  `#EAEAE8`, 12 px `#4A4A4D`) — `SectionHeader`-like count pill
  (`surfaces.dart:167-185`).
- **Row** (`:118-157`): padding 12, radius 14, gap 14, columns: tile 40×40
  radius 11 (icon `monitor` 20) | name 15/600 + service mono 12 `#6E6E73` (flex)
  | meta 12.5 `#4A4A4D` **fixed 170 px** | status pill **fixed 86 px**, 24 tall,
  radius 12, 12/500, dot 6, centered | action button **fixed 96×34**, radius 17,
  13.5/500.
  Variants:

  | Variant | Row fill | Tile | Meta example | Pill | Button |
  |---|---|---|---|---|---|
  | Viewing (`:118-129`) | `#F4F8FD` | fill `#E5EFFA`, icon `#1B6FC8` | "直连 · 16 ms · 60 fps" | "正在查看": fill `#E5EFFA`, text `#124A8C`, dot `#1B6FC8` | ink "回到画面" |
  | Online (`:131-142`) | none | `#F2F2F0`, icon `#4A4A4D` | "上次 昨天 22:40 · 中继" | "在线": `#E3F2E7`/`#1A7F37` | ink "打开" |
  | Not sharing (`:146-157`) | none | `#F2F2F0`, icon `#8E8E93` | "上次 11 月 2 日" | "未共享": `#EDEDEB`/`#4A4A4D`, dot `#8E8E93` | disabled outlined "打开" (1 px `#E4E4E2`, white, text `#8E8E93`) |

  Ordering [inferred]: viewing, online…, then a 1 px `#E4E4E2` divider inset
  12 (`:144`), then not-sharing rows.
- **Add tile** (`:159-163`): margin-top 12, 56 tall, radius 14, dashed 1.5 px
  `#C8C8C6` (CustomPainter; Flutter has no dashed border), centre: plus 18 +
  "添加电脑" 14/500 + "粘贴另一台电脑的私密邀请" 13 `#6E6E73` → opens AddComputer.
- Footnote (`:164`) 12.5 `#6E6E73`: "只有选择“记住此电脑”的邀请会保存在本机，并由系统加密。"
- Row actions not drawn [PROPOSED]: secondary click / long-press / trailing
  overflow → "重命名", "移除此电脑" (deletes stored invite + thumbnail).

Data for the list — **mostly [GAP]**:
- A remembered-computer list does not exist. Today the viewer side has only a
  text field (`desktop_page.dart:161-171`) and `DesktopStore._target`
  (`desktop_store.dart:78,881`). `DesktopTrust` is the HOST's grant
  (`desktop_api.dart:186-232`). Needed model [PROPOSED]:
  `RemoteComputer { id, name, service, invite (secret, encrypted at rest),
  relayOnly, lastConnectedAt, lastPath ('direct'|'relay'), lastControlAllowed,
  thumbnail? }`, persisted only when "记住此电脑" is on; encryption: Windows
  DPAPI (code exists only for host trust, `ui/native/pb_mapper_ffi/src/desktop_trust.rs:2,200`),
  Android Keystore (absent). Unremembered computers live in memory for the
  session only.
- Online pill: membership of `service` in `AppStore.relayServices`
  (`state/app_store.dart:122`, built from the relay's registered services) —
  available, refreshed by the slow relay check; namespace caveat
  (`state/remote_service.dart:9-10`): match by name in the viewer's own
  namespace. Unknown (relay offline) → no pill or grey "未知" [PROPOSED].
- Viewing row: the remembered entry whose service equals the active session's
  service (`DesktopStore.connected`, `desktop_store.dart:124`).
- Viewing meta "直连 · 16 ms · 60 fps": path = `snapshot.viewer.path`
  (`desktop_api.dart:98`, values `direct`/`relay`, map to ARB `pathDirect`
  "直连"/`pathRelay` "中继"); fps = `DesktopStore.presentFps`
  (`desktop_store.dart:55-59`); **"16 ms" network RTT [GAP]** (the viewer has
  no RTT; `frameAckMs` is a host frame→presented round trip incl. phone work,
  `crates/pb-mapper-desktop/src/lib.rs:156-160` — not the same metric).
- "上次 {when} · {path}": stored on session end [GAP until the store exists].
  Date format [PROPOSED]: today "今天 HH:mm", yesterday "昨天 HH:mm", this
  year "M 月 D 日", else "YYYY/M/D".

### 2.4 HostSetup — first enable (`HostSetup:17-118`)

Same page frame; **no refresh/添加电脑 in the header** while the list is empty
(`HostSetup:22-29`) — the empty state carries the add button. [PROPOSED rule:
show header actions only when ≥1 remembered computer.]

Setup card (`HostSetup:33-94`): radius 20, padding 28, gap 24.
1. Header (`:34-43`): tile 44 radius 12 **grey** (`#F2F2F0`, icon `#4A4A4D`
   `monitor`); "这台电脑" 17/600; subtitle "尚未开启远程控制" 12.5 `#6E6E73`;
   grey pill "未共享" (26 tall, `#EDEDEB`, dot `#8E8E93`, text `#4A4A4D`).
2. Headline (`:45`) 22/600 line-height 1.35, two lines:
   "用手机或另一台电脑，" / "随时看到并操作这台电脑".
3. Steps (`:47-69`), vertical gap 16; each: number disc 26 (13/600) + title
   14.5/600 + body 13 `#4A4A4D` lh 1.5, margin-top 3. Disc 1 is ink-filled
   (white numeral) = the next step; discs 2-3 `#EAEAE8` with ink numeral.
   1. "Windows 请求一次管理员批准" — "安装受保护的辅助服务，远程时也能操作 UAC 提示和锁屏。"
   2. "应用自动重启并回到这里" — "已有的隧道和配置保持不变。"
   3. "得到一条私密邀请" — "复制给另一台电脑，或让手机扫码。之后一直有效，直到你撤销。"
   [PROPOSED] when the helper is already installed and verified, steps 1-2
   show a `check` in a green disc (or are hidden) — only step 3 remains.
4. Settings (`:71-82`): rows 52 tall: switch "允许远程操作鼠标和键盘" (default
   **on**, ink switch = theme `switchTheme`, `app_theme.dart:466-478`); dropdown
   "共享的显示器" "显示器 1 · 2560 × 1440".
   **[GAP]** `host_enable` takes only `service` and `display_id`
   (`ui/native/pb_mapper_ffi/src/desktop.rs:203-206`); the allow-control
   choice needs a parameter or a follow-up `allowControl(false)`.
5. Primary (`:84-88`): full-width 48 tall, radius 24, ink, 15/600, icon
   `shield-check` 18 + "开启远程控制" (ARB `desktopEnable` already "开启远程控制")
   → `DesktopStore.enableHost(service, display)` (`desktop_store.dart:769-815`);
   it may return `restart` (step 2: app restarts and comes back here —
   `onRestartReady`, `:795-805`).
6. Under it (`:89-92`), 13 px, space-between: link (colour `#1B6FC8`, hover
   `#124A8C`) "只共享普通桌面（不安装辅助服务）" → ordinary path without helper
   (today `startHost(... privileged:false, remember:true, autoStart:true)`,
   `desktop_store.dart:743-767` — exact flags to be chosen by the host
   subsystem); right note `#6E6E73` "关闭窗口后仍在后台共享".
- Service name is not shown [PROPOSED]: default `desktop:` + sanitised
  `Platform.localHostname` (lowercase, `[a-z0-9._-]`, ≤120 chars, must satisfy
  `valid_service`, `crates/pb-mapper-desktop/src/lib.rs:49-56`); today the
  default is the literal `desktop:my-computer` (`desktop_page.dart:24`).

Empty list card (`HostSetup:96-115`): radius 20, padding 20/20/24, min-height
520; header "远程电脑" (no count); centred column gap 14, padding 40/24:
- illustration 120×84, stroke `#C8C8C6` 1.5 round: monitor rect (8,6,76×50,
  r5), stand `M34 72h24 M46 56v16`, phone rect (88,26,26×50,r5, white fill),
  phone slot `M98 70h6`, dashed link `M60 31h20` in `#1B6FC8` dash 3/4
  (`:101-107`) → CustomPainter; fallback `monitor-smartphone` 48/300.
- "还没有远程电脑" 16/600; body 13.5/1.6 `#4A4A4D`, max-width 360:
  "在另一台电脑的 pb-mapper 里开启远程控制，复制它的私密邀请，粘贴到这里。"
- outlined ink button 40 tall, padding 0 18, radius 20, 14/500, plus 16 + "添加电脑".

### 2.5 AddComputer — desktop dialog (`AddComputer:26-77`)

Scrim `rgba(20,20,22,0.32)`; dialog 560 wide, radius 24, white, padding
24/28/28, gap 20, shadows `0 2 6 rgba(0,0,0,0.06), 0 24 60 rgba(0,0,0,0.18)`
(≈ `dialogTheme`, `app_theme.dart:496-517`; use `showDialog` not
`showAdaptivePanel` on desktop).
1. Header (`:28-33`): "添加远程电脑" 18/600; close 32×32 radius 10, `x` 18.
2. Invite (`:35-44`): label "私密邀请" 13 `#4A4A4D`; textarea 3 rows, radius 14,
   fill `#F2F2F0`, padding 12/88/12/14, mono 13 lh 1.6; focused ring 1.5 px ink
   (theme `FieldBorder`, `glass.dart:593-695`). After a valid paste it shows
   the **masked** form `pbd1·k7HqT2v9••••…Zx2Q` (first 8, last 4)
   [inferred: the mock is read-only]. Inline "粘贴" button at right 10/top 10:
   30 tall, padding 0 10, radius 9, 1 px `#E4E4E2`, white, 12.5 px,
   `clipboard-paste` 16 → reads clipboard. Keep `autocorrect: false`,
   `enableSuggestions: false`, smart quotes/dashes off (as `desktop_page.dart:161-171`).
3. Recognised host (`:46-52`), shown only after a valid invite: radius 14,
   padding 12/14, fill `#F4F8FD`, `shield-check` 20 `#1B6FC8`; service mono 13
   `desktop:office-pc`; meta 12.5 `#4A4A4D`
   "已识别主机身份 · 权限：查看并控制 · 通过当前中继连接".
   - Service: parseable from the invite (JSON today; `pbd1` later) — needs a
     local parse/validate op **[GAP]** (no FFI op parses without connecting;
     ops listed at `ui/native/pb_mapper_ffi/src/desktop.rs:193-260`).
   - "权限：查看并控制" **[GAP]**: the invite carries no permission
     (`Invitation` fields, `lib.rs:91-96`); only known after connecting
     (`controlAllowed`). [PROPOSED] omit the segment until known, or add the
     permission to the compact invite (host can change it later, so it is a
     hint only).
   - "通过当前中继连接": static (relay credentials are deliberately not in the
     invite, `lib.rs:86-88`); use the configured relay.
4. Name (`:54-57`): label "名称"; input 44 tall radius 14 fill `#F2F2F0` padding
   0 14, 14 px. Default [PROPOSED]: service without `desktop:`.
5. Options list (`:59-71`): hairline top; switch row (padding 12/0, bottom
   hairline) "记住此电脑" 14 + "邀请加密保存在本机，可以随时移除" 12.5 `#6E6E73`,
   **default on**; disclosure row 44 tall `chevron-right` 16 +
   "更多选项（仅使用中继）" 13.5 `#4A4A4D` → expands a switch "仅使用中继"
   (ARB `desktopRelayOnly`) — today an `ExpansionTile` switch
   (`desktop_page.dart:271-278`).
6. Footer (`:73-76`) right-aligned gap 10: text "取消" (40, padding 0 18);
   ink "连接" (40, padding 0 22, 14/600) → `DesktopStore.connect(invite,
   relayOnly)` (`desktop_store.dart:874-891`), then open the viewer; save when
   remembered.
- States [PROPOSED]: empty (recognised card hidden, 连接 disabled); invalid
  invite → error text under the field "这不是有效的远程桌面邀请" (today an
  `INVALID_ARGUMENT` returns to input, `desktop_store.dart:932-938`); connecting
  → 连接 shows a spinner, dialog closes when the viewer opens. Also opened from
  the identity-mismatch state "粘贴新邀请" to replace a remembered computer's
  invite (name prefilled).

### 2.6 Viewer — desktop full-window viewer (`Viewer:16-96`)

Route: full window, **rail hidden** (`canvas.json:43` point 3); ground
`#0F0F10`; always dark.

1. **Title bar** (`Viewer:17-27`): 36 tall, padding-left 16, gap 10; `monitor`
   16 `#7DB6F2`; remote name 13/600 ("办公室电脑"); "pb-mapper" 12.5 `#8E8E93`;
   spacer (drag area: `WindowDragArea`, `shell/window_chrome.dart:31-48`);
   caption buttons fg `#B4B4B8`. [DEVIATION: drawn 44×36 with 12 px glyphs;
   reuse the fixed `CaptionButtons` (46×32, radius 6, 10 px glyph), centred
   in the 36 px bar.] Hidden in full screen.
2. **Picture** (`Viewer:29-31`): fit (letterbox) inside the area below the
   title bar, centred: at 1440×864 a 16:9 picture is 1440×810 at y = 63. This
   is today's `DesktopGestureSurface` fit (`pages/desktop_gesture_surface.dart:275-293`).
3. **Floating toolbar** (`Viewer:33-76`), role toolbar "远程桌面工具栏":
   top 48 (12 below the title bar), horizontally centred; 48 tall, radius 16,
   padding 0 6, gap 2; fill `rgba(32,32,35,0.9)` + backdrop blur 20; border 1 px
   `rgba(255,255,255,0.08)`; shadow `0 8 30 rgba(0,0,0,0.4)`; single line.
   Buttons 36 tall radius 10; separators 1×22 `rgba(255,255,255,0.12)` margin
   0 4. Left to right:
   1. **Pin** 36×36, `pin` 18, aria "固定工具栏", pressed fill
      `rgba(255,255,255,0.1)` (`:34-36`).
   2. sep. **Status** (`:38-42`): padding 0 10, gap 8: dot 8 (green
      `#57C27A` normal; amber `#E0A647` degraded; [PROPOSED] grey while
      connecting) + path "直连"/"中继" 13 + mono 12 `#B4B4B8`
      "16 ms · 60 fps". Click toggles the **connection-quality panel** (§2.7.2).
      Degraded form: "直连 · 画面延迟" + mono amber "420 ms" (`ViewerStates:40-43`).
   3. sep. **Mode** radiogroup "操作方式" (`:44-53`): track padding 3 radius 11
      fill `rgba(255,255,255,0.06)`, gap 2; segments 30 tall padding 0 10
      radius 8 gap 6, icon 16; selected fill `rgba(255,255,255,0.16)` white
      13/600; idle `#B4B4B8` 13. Options: `eye` "查看" | `mouse-pointer-2`
      "控制". → `DesktopStore.control(false/true)` (`desktop_store.dart:989-1007`);
      "控制" disabled when `snapshot.viewer.controlAllowed != true`
      (`desktop_api.dart:89`) and then the view-only pill (§2.7.6) explains why.
   4. sep. **"显示"** (`:55-59`): icon (see §2.13: `monitor`) 16 + label 13 +
      `chevron-down` 13→16 → Display menu (§2.6.1).
   5. **"快捷键"** (`:60-64`), aria-expanded; open state fill
      `rgba(255,255,255,0.1)`; icon `command` 16 [DEVIATION: drawn as a
      keyboard; icon system maps 快捷键 → `command`, `Icons:137`] + chevron →
      Shortcuts menu (§2.6.2).
   6. **More** 36×36 `ellipsis` 18, aria "更多" (`:65-67`) → [PROPOSED] menu:
      "输入文字…" (today's text dialog, `desktop_viewer.dart:519-538`, ARB
      `desktopKeyboard` "输入文字"), "连接质量" (toggle panel).
   7. sep. **Full screen** 36×36 `maximize` 18, aria "全屏" (`:69-71`) →
      `windowManager.setFullScreen(!isFullScreen)`; [PROPOSED] Esc does NOT
      leave full screen (Esc belongs to the remote); the button toggles.
   8. **Disconnect** (`:72-75`): padding 0 12, text+icon `#F2B8B5`, `power` 16
      + "断开" 13 → `DesktopStore.disconnect()` (`desktop_store.dart:952-987`)
      then pop.
   **Pin / collapse** (`canvas.json:43`, `ViewerStates:107-113`): pinned → always
   shown. Unpinned → collapses **2 s after the mouse leaves the toolbar** into
   a top handle: 72×20 at top 0 (top of the picture area / below the title
   bar), centred, radius 0 0 10 10, border 1 px `rgba(255,255,255,0.1)` without
   top, fill `rgba(32,32,35,0.9)`, `chevron-down` 14 (stroke 2), aria
   "展开工具栏". Expand on handle hover or click. Never collapse while a menu,
   the quality panel or a text dialog is open. [PROPOSED] persist the pin in
   `AppPrefs` (`shell/app_prefs.dart`); default pinned on first use.
4. **Control hint** (`Viewer:91-95`), role status: bottom 64, centred, 40 tall,
   padding 0 16, radius 20, fill `rgba(32,32,35,0.9)`, border
   `rgba(255,255,255,0.08)`, 13 px, gap 10: `keyboard` 16 `#7DB6F2` +
   "键盘和鼠标正发往办公室电脑" (name interpolated) + `#B4B4B8`
   "系统快捷键请从工具栏发送". [PROPOSED] shown for ~3 s whenever control
   becomes active (and on viewer focus while controlling), IgnorePointer.
   Meaning: local system shortcuts (Win, Alt+Tab) are **not captured**; they go
   through the Shortcuts menu.

#### 2.6.1 Display menu — **not drawn** (only `canvas.json:43`)

Sticky note: "“显示”菜单含缩放方式和画质偏好（画质优先 / 平衡 / 流畅优先）；可打开连接质量浮层".
[PROPOSED] same chrome as the Shortcuts menu (§2.6.2), 260 wide:
- section label "缩放方式" (12 `#8E8E93`, padding 6/12): radio items
  "适应窗口" (default; gesture-surface scale 1) and "原始大小" (1 remote px = 1
  physical px). Both are local transforms only (P1-4 invariant: never message
  the host, `desktop_gesture_surface.dart:378-383`).
- divider; section "画质偏好": radio "画质优先" / "平衡" (default) / "流畅优先" →
  `VideoPreferences` targets (`crates/pb-mapper-desktop/src/lib.rs:180-195`,
  sent only at connect today, `ffi/desktop_api.dart:428-447`) **[GAP: no
  mid-session op]**; numbers owned by the P0-2/P0-4 work.
- divider; check item "连接质量" (toggles the panel).

#### 2.6.2 Shortcuts menu (`Viewer:78-89`)

role menu "快捷键"; anchored under the "快捷键" button (drawn at top 104 =
toolbar bottom + 8, left 792); width 300, padding 6, radius 14, fill `#262628`,
border `rgba(255,255,255,0.08)`, shadow `0 12 40 rgba(0,0,0,0.5)`, 13.5 px.
Item 36 tall, padding 0 12, radius 9, label left `#EDEDEF`, hint right mono
12 `#B4B4B8`; hover/focus fill `rgba(255,255,255,0.08)` (first item drawn
hovered). Dividers 1 px `rgba(255,255,255,0.08)` margin 6/8.

| # | Label | Hint | Sends (key names of the host `Key` enum, `crates/pb-mapper-desktop/src/lib.rs:264-286`) | Host support today |
|---|---|---|---|---|
| 1 | 显示桌面 | Win + D | meta↓ d↓ d↑ meta↑ | **[GAP]** no `D` key |
| 2 | 切换窗口 | Alt + Tab | alt↓ tab↓ tab↑ alt↑ | yes |
| 3 | 任务管理器 | Ctrl + Shift + Esc | control↓ shift↓ escape↓ escape↑ shift↑ control↑ | yes |
| 4 | 开始菜单 | Win | meta↓ meta↑ | yes |
| 5 | 锁定远程电脑 | Win + L | **[GAP]** no `L`; and Windows does not lock from injected Win+L — needs a host-side lock action (LockWorkStation), a security-reviewed new input kind |
| — | divider | | | |
| 6 | Ctrl + Alt + Del (disabled, `#8E8E93`) | "暂不支持" (12 px, not mono) | nothing — keeps the existing security boundary (brief) | n/a |
| — | divider | | | |
| 7 | 刷新画面 | — | `DesktopStore` keyframe request (`api.keyframe()`, `ffi/desktop_api.dart:333-335`; today only internal, `desktop_store.dart:418-425`) | yes (needs public method) |
| 8 | 释放所有按键 | — | `DesktopStore.release()` (`desktop_store.dart:1009-1022`) + clear local modifier latches | yes |

Items 1-5 require control (disabled in 查看 mode or when not `_interactive`,
`pages/desktop_viewer.dart:193-201`); 7-8 always enabled. Each combo must be
enqueued as one ordered sequence through `DesktopStore.send` (keys are ordering
barriers, `desktop_store.dart:1058-1060`), never interleaved with pointer motion.

### 2.7 ViewerStates — six overlays (`ViewerStates:16-120`)

Thumbnails are 680×383 (the 1440×810 picture at ×0.472) but **overlay
elements are drawn at real size**; centre/anchor them the same way in the
real viewer. Replace `DesktopConnectionBanner`
(`pages/desktop_connection_banner.dart:1-106`, which pushes the picture down)
— "连接状态叠加在画面上，不再把画面往下推" (`canvas.json:43` point 4).

Mapping to `DesktopConnectionPhase` (`state/desktop_store.dart:6-16,100-120`):

| Overlay | Phase / condition | Drawn |
|---|---|---|
| Connecting card | `connecting` (attempt 0, no picture yet) | yes |
| Slow + quality | `delayed` (re-thresholded, see below) / panel toggled | yes |
| Secure desktop | `waitingForDesktop` + helper-missing message | yes |
| Reconnecting pill | `retryWaiting` (countdown) / `reconnecting` | yes (countdown form) |
| Identity changed | `identityMismatch` | yes |
| Collapsed toolbar + view only | toolbar collapsed; `controlAllowed == false` | yes |
| Recovering video | `recoveringVideo` | **no** [PROPOSED: status pill at top "正在恢复画面" (ARB `desktopStateRecovering` shortened), spinner 16, no scrim] |
| Ended | `idle` after the host stopped / `state == 'stopped'` | **no** [PROPOSED: card like Connecting: title "连接已结束", body from message, buttons "返回" / "重新连接"] |

#### 2.7.1 正在连接 (`ViewerStates:18-34`)
Caption: "每一步完成就打勾，失败时停在那一步并说明原因". No picture (`#0F0F10`).
Card 320 wide, padding 20/22, radius 16, fill `#1C1C1E`, border
`rgba(255,255,255,0.08)`, gap 14. Header: spinner 20 (track
`rgba(255,255,255,0.15)` 2.5, arc `#7DB6F2` 2.5 round) + "正在连接 办公室电脑"
15/600. Steps list gap 8, 13 px: done = `check` 16 `#57C27A` (stroke heavy) +
text `#B4B4B8`; current = 7 px `#7DB6F2` dot centred in a 15 box + text
`#EDEDEF`; [PROPOSED] pending = 7 px `rgba(255,255,255,0.2)` dot + `#8E8E93`
text; failed = `circle-alert` 16 `#F2B8B5` + text, reason 12.5 `#B4B4B8` under it.
1. "找到主机，走直连路径" ([PROPOSED] relay variant "找到主机，走中继路径")
2. "校验主机身份"
3. "等待第一帧画面"
Button "取消" right-aligned: 32 tall, padding 0 14, radius 16, 1 px
`rgba(255,255,255,0.18)`, 13 px → `disconnect()` + pop.
**[GAP]** step granularity: `api.connect` is one call
(`desktop_store.dart:913`) that returns after relay+direct+TLS pin; path is
known only from frame statuses (`viewer.rs:236`); first presented frame is
known (`videoPresented`, `desktop_store.dart:577-597`). Needs connect progress
(stage + path) from FFI, or collapse steps 1-2 into one completion.

#### 2.7.2 画面变慢 · 连接质量 (`ViewerStates:36-56`)
Caption: "点工具栏上的状态打开，数字每秒刷新".
- Degraded status pill (`:40-43`) top 12 centred: 34 tall, padding 0 12, radius
  12, fill `rgba(32,32,35,0.92)`, border `rgba(255,255,255,0.08)`, 12.5 px, gap
  8: dot 8 `#E0A647` + "直连 · 画面延迟" + mono `#E0A647` "420 ms". This is the
  toolbar status segment; [PROPOSED] when the toolbar is collapsed and quality
  is degraded, show it standalone under the handle so degradation is never
  hidden.
- **Quality panel** (`:44-54`): anchored top-right (drawn top 56, right 14 —
  [PROPOSED] in the real viewer: toolbar bottom + 8, right 14); 236 wide,
  padding 14/16, radius 14, fill `rgba(28,28,30,0.94)`, border
  `rgba(255,255,255,0.08)`, 12.5 px, gap 7. Title "连接质量" 13/600. Rows
  (label `#B4B4B8` flex, value right; mono where noted). Refresh every 1 s.

| Label | Example | Mono | Source today |
|---|---|---|---|
| 路径 | 直连 · IPv6 | no | `status.path` direct/relay (`desktop_api.dart:98`) available; **IP family [GAP]** |
| 网络往返 | 48 ms | yes | **[GAP]** — transport RTT not exposed (only host-side `baseline_ms` in logs; `roundtrip_probe` is echoed but never measured on the viewer, `viewer.rs:226-234`) |
| 画面延迟 | 420 ms (amber when slow) | yes | **[GAP/semantic]** `frameAckMs` (`desktop_api.dart:116`) includes presentation + ACK coalescing; the perf work must define picture latency (capture→present) |
| 显示帧率 | 38 fps | yes | `DesktopStore.presentFps` (h264 path) — available |
| 码率 | 3.1 Mbps | yes | `status.bitrate` (`desktop_api.dart:110`) — available (host target); format `{x.x} Mbps` |
| 分辨率 | 1920 × 1080 | yes | `status.videoWidth × videoHeight` (`desktop_api.dart:103-104`) — available |
| 编码 | H.264 硬件 | no | `status.codec` 'h264'/'jpeg' available; **hardware/software [GAP]** |

  Footer 12.5/1.5 `#B4B4B8`, margin-top 4: "已自动降低码率，优先保证操作跟手" —
  the last adaptation decision **[GAP]** (host feedback `action` exists only in
  `desktop_feedback_interval` logs, brief §1). [PROPOSED] other texts:
  "已降低帧率，等待网络恢复", hide when no adaptation.
- Degraded threshold: today `delayed` = `frameAckMs > 500`
  (`desktop_store.dart:113-115`). [PROPOSED] amber when picture latency ≥
  150 ms (P0-2 congestion boundary) or display fps < 50 % of target for 2 s;
  owned by the perf work.

#### 2.7.3 等待远程桌面 (`ViewerStates:58-73`)
Caption: "UAC 或锁屏无法显示时，保留画面并暂停输入". Last picture kept;
scrim `rgba(15,15,16,0.72)` full; card 360 wide, padding 20/22, radius 16,
`#1C1C1E`, border `rgba(255,255,255,0.08)`, gap 10:
`lock-keyhole` 20 `#E0A647` + "远程电脑正在显示安全桌面" 15/600; body 13/1.6
`#B4B4B8` "出现了 UAC 提示或锁屏。这台电脑没有启用辅助服务，无法显示这一画面；恢复到普通桌面后自动继续。";
chip "已暂停输入" 24 tall, padding 0 10, radius 12, fill
`rgba(255,255,255,0.08)`, 12 px.
Data: `phase == waitingForDesktop` + message contains
`secure_desktop_requires_helper` (`desktop_connection_banner.dart:32-34`) —
available. [PROPOSED] other `capture_unavailable` reasons: title "等待远程桌面",
body = ARB `desktopStateWaitingDesktop`. Scrim is translucent, so the texture
keeps compositing (`desktop_viewer.dart:492-493` constraint holds).

#### 2.7.4 正在重连 (`ViewerStates:75-88`)
Caption: "画面变灰，表示这是断开前的最后一帧". Last frame with
`grayscale(1) brightness(0.5)` (`ColorFiltered` matrix: luminance × 0.5).
Centred pill, role status: 48 tall, padding 0 8 0 16, radius 24, fill
`rgba(28,28,30,0.95)`, border `rgba(255,255,255,0.1)`, 13.5 px, gap 12:
spinner 18 + "连接中断，3 秒后第 2 次重连" + `#B4B4B8` "已暂停输入" + white
button "立即重试" (34 tall, padding 0 14, radius 17, ink text 13/600).
Data: `retryIn` (`desktop_store.dart:95-99`, ceil seconds) and
`reconnectAttempt` (`:87`) available; **"立即重试" [GAP]** no public method
(needs `retryNow()`: cancel `_retryTimer`, `_openViewer(serial)`,
`desktop_store.dart:428-469,893-950`). [PROPOSED] attempt-running variant:
"正在第 {n} 次重连…" without the button.
**[GAP]** "last frame": `_openViewer` calls `_clearVideo()` which disposes the
texture at the start of every attempt (`desktop_store.dart:906,688-726`); the
viewer also drops `_image` on epoch change (`desktop_viewer.dart:57-70`). A
retained still is needed (shared with the mobile last-picture thumbnail).

#### 2.7.5 主机身份已变化 (`ViewerStates:90-105`)
Caption: "安全问题：停止自动重连，必须由人处理". No picture. role alert;
card 380 wide, padding 22, radius 16, `#1C1C1E`, border
`rgba(242,184,181,0.35)`, gap 12: `triangle-alert` 20 `#F2B8B5` +
"主机身份已变化" 15/600 (ARB `desktopIdentityMismatch` matches); body 13/1.6
`#B4B4B8` "这台电脑的身份与邀请里记录的不一致，已停止自动重连。请向对方要一条新的邀请，不要继续使用旧邀请。"
(replaces ARB `desktopIdentityMismatchHelp`); buttons right gap 8: outlined
"断开" (34, padding 0 14, radius 17, 1 px `rgba(255,255,255,0.18)`) →
disconnect + pop; white "粘贴新邀请" (13/600) → AddComputer prefilled for this
remembered computer (replaces its invite). Data: `identityMismatch` phase —
available (`desktop_store.dart:432-443`).

#### 2.7.6 工具栏收起 · 仅查看 (`ViewerStates:107-119`)
Caption: "未固定时，鼠标离开 2 秒收起成顶部把手". Handle as in §2.6.
View-only pill: bottom-left (left 12, bottom 34 within the picture), 30 tall,
padding 0 12, radius 15, fill `rgba(32,32,35,0.9)`, border
`rgba(255,255,255,0.08)`, 12.5 px, gap 8: `eye` 16 + "仅查看 · 对方未允许控制".
Shown whenever `controlAllowed == false` (independent of toolbar state).
[PROPOSED] when the user chose 查看 while control is allowed, no pill.

### 2.8 MobileHome — phone Remote tab (`MobileHome:16-79`)

Ground `#F4F4F2`; content padding 54(status bar)/16/0, gap 12; tab bar at
bottom (§2.2).
1. Header 48 (`:18-23`): large title "远程" 28/700; add button 40×40 circle ink
   with `plus` 20 white, aria "添加电脑" → MobileAdd.
   [DEVIATION: other mobile pages use `PageFrame` 54 px bar with an 18 px title
   (`page_frame.dart:60-61,106`); the artboard uses an iOS-style large title.
   Implement as drawn; flag for consistency review.]
2. **Featured computer card** (`:25-40`): white radius 20, clip.
   - Thumbnail 358×201 (16:9) of the last picture, with badge top-left 10/10:
     24 tall, padding 0 10, radius 12, fill `rgba(20,20,22,0.7)`, white 11.5
     "上次画面 · 今天 09:12". **[GAP]** last-picture capture on disconnect,
     local only, deleted with the computer (brief). [PROPOSED] placeholder
     when absent: `#1F2328` box with `monitor` 24/300 `#8E8E93`.
   - Body padding 14/16/16, gap 12: name 17/600; sub 12.5 `#4A4A4D`
     "直连 · 可以查看和控制" = last path + last permission
     (**[GAP]** stored per computer); pill "在线" (24 tall, 12/500, green).
   - Button full width 46 tall radius 23 ink 15/600 "打开" (or "回到画面" when
     this is the active session [PROPOSED]).
   - Which computer is featured [PROPOSED]: the most recently opened one.
3. Section label "其他电脑" 13 `#4A4A4D`, padding 6/4/0 (`:42`).
4. List card (`:43-61`): white radius 20, padding 4/0; rows 64 tall, padding
   0 16, gap 12: tile 40 radius 11 `#F2F2F0` (`monitor` 20; `#4A4A4D` online,
   `#8E8E93` offline); name 15/600; sub 12.5 `#4A4A4D` with a leading 6 px dot:
   "在线 · 上次走中继" (green dot) / "未在共享 · 上次 11 月 2 日" (grey dot);
   trailing `chevron-right` 18 `#8E8E93`. Divider 1 px `#E4E4E2` inset 68.
   Row tap → open viewer (offline row [PROPOSED]: tap shows a toast "这台电脑
   未在共享" instead of connecting).
   [DEVIATION: desktop pill says "未共享", phone row says "未在共享"; keep as
   drawn, both strings in ARB.]
5. Two dashed buttons (`:63-72`): 2 columns gap 10, 52 tall, radius 16, dashed
   1.5 `#C8C8C6`, 14/500, gap 8: `scan-qr-code` 18 "扫码添加" → MobileAdd with
   camera; `clipboard-paste` 18 "粘贴邀请" → MobileAdd in paste mode
   [PROPOSED: read clipboard immediately and show the confirm step].
6. Note 12 `#6E6E73` (`:73`): "手机可以查看并控制 Windows 电脑，暂不能被远程控制。"
   (mobile replacement for the host card; Android/iOS have no host).
- Empty state not drawn [PROPOSED]: no featured card, no list; show the two
  dashed buttons, then the note.

### 2.9 MobileAdd — scan / paste sheet (`MobileAdd:16-75`)

Modal bottom sheet from top 48 to bottom, radius 24 24 0 0, white, padding
8/20/34, gap 16 (theme sheet radius is 28, `app_theme.dart:552-554` — use 24
here or accept 28). Grabber 36×5 radius 3 `#D1D1CF`.
1. Header: "添加电脑" 20/600 + close 36×36 circle fill `#F2F2F0` `x` 18 `#4A4A4D`.
2. **Viewfinder** (`:26-50`), aria "相机取景框": 330 tall, radius 20, fill
   `#161618`, clip. Live camera preview fills it. Reticle 220×220 centred: four
   L-corners 28×28, 3 px white, outer corner radius 10; scan line inset 14 at
   58 % height, 2 px `#7DB6F2` + glow `0 0 10 #7DB6F2` ([PROPOSED] animate
   top↔bottom 1.6 s, static under reduced motion). Hint pill bottom 14 centred:
   13 px white, fill `rgba(0,0,0,0.45)`, padding 6/12, radius 14:
   "对准电脑“远程桌面”页上的二维码".
   **[GAP]** no camera/scan dependency (`ui/pubspec.yaml:35-55`); needs a
   scanner package, Android `CAMERA` permission, iOS
   `NSCameraUsageDescription`; frames never stored or uploaded (footer text).
   [PROPOSED] permission-denied state: viewfinder shows `scan-qr-code` 24/300 +
   "需要相机权限才能扫码" + button "打开设置"; paste stays available.
3. Divider (`:52-54`): two hairlines with "或" 12.5 `#8E8E93` between, gap 12.
4. "粘贴邀请" outlined ink full width 48 tall radius 24, 15/500,
   `clipboard-paste` 18.
5. Options (`:61-73`): switch "记住此电脑" 15 + "邀请加密保存在这台手机上"
   12.5 `#6E6E73` (default on); disclosure "更多选项（仅使用中继）" 14 `#4A4A4D`
   `chevron-right` 16.
6. Footer 12 `#6E6E73` lh 1.5: "相机只用来识别二维码，画面不会保存或上传。"
- After a successful scan/paste — **not drawn** [PROPOSED]: replace the
  viewfinder with the desktop dialog's confirm section (recognised host card,
  名称 field) and a full-width ink "连接" button; keep the switches.

### 2.10 MobileViewer — landscape viewer (`MobileViewer:16-65`)

844×390, ground `#000`, no system bars ([PROPOSED] immersive sticky +
landscape lock while the viewer is on screen; restore on exit).
- **Picture** (`:17-19`): 16:9 fit by height → 694×390 at x 75; gutters =
  (W − H·aspect)/2 = 75 px each. [PROPOSED] if a gutter is < 64 px (tablet,
  4:3 remote, wide cutout inset), overlay the gutter controls on the picture
  edges with `viewerHint` backgrounds; respect `MediaQuery.padding` (cutouts)
  inside the gutters.
- **Touch indicator** (`:22`): 44 circle, fill `rgba(255,255,255,0.22)`, 1.5 px
  `rgba(255,255,255,0.8)` at the finger in trackpad mode (drawn at picture
  x 470, y 262); [PROPOSED] fades out 150 ms after lift; never in the texture's
  layer (overlay only).
- **Mode hint** (`:23-26`) top 12 centred over the picture: 32 tall, padding
  0 14, radius 16, `rgba(24,24,26,0.86)`, 12.5 px, gap 8, `touchpad` 16
  `#7DB6F2` + "触控板模式：单指滑动移动光标，轻点单击". [PROPOSED] shown 3 s on
  viewer open and after each mode switch; direct-touch text
  "直接触摸模式：点到哪里就点击哪里".
- **Left gutter** (`:29-45`), 75 wide, column space-between, padding 16/0:
  1. back 44×44 circle fill `rgba(255,255,255,0.1)`, icon (§2.13: `back`) 20,
     aria "返回远程列表" → pop (session continues, like desktop "回到画面").
  2. quality button 56 wide, padding 8/0, radius 14, gap 3, aria
     "连接质量：直连，60 帧每秒": dot 8 `#57C27A` (amber when degraded); "直连"
     11 px; "60" mono 14/500; "fps" 10.5 `#B4B4B8` → toggles the quality panel
     (§2.7.2 content; [PROPOSED] anchored to the picture's top-left).
  3. mode toggle, aria "切换操作方式，当前为触控板": 44 circle fill
     `rgba(125,182,242,0.2)`, icon `touchpad` 20 `#7DB6F2` (direct touch:
     `pointer`, neutral fill [PROPOSED]); label "触控板" / "直接触摸" 10.5.
     Tap toggles mode; [PROPOSED] long-press opens MobileGestures.
- **Right gutter** (`:47-64`), role toolbar "远程操作", 75 wide, centred
  column gap 14; each = 44 circle fill `rgba(255,255,255,0.1)` + label 10.5:
  1. `keyboard` "键盘" → show soft keyboard + modifier bar (§2.11).
  2. `command` "快捷键" (drawn as a ⌘ text glyph) → Shortcuts list (§2.6.2) as a
     [PROPOSED] side sheet/popover.
  3. `mouse` "右键" → right click at the current cursor (trackpad) / last
     touch point (direct).
  4. `ellipsis` "更多" → [PROPOSED] sheet: "操作方式" (MobileGestures), "画质偏好"
     (3 options), "连接质量", "查看/控制" toggle, "刷新画面", "释放所有按键",
     "断开" (error colour).
  Disabled (40 % opacity) when not interactive (view-only, stale picture).
- Portrait orientation **not drawn** [PROPOSED: gutters become a top and a
  bottom bar with the same buttons]; ViewerStates overlays on mobile not drawn
  [PROPOSED: same cards, max width = picture width − 32].
- Minimap is not on this artboard (it appears in §2.11) — despite the brief's
  description, show it only when the visible region is smaller than the whole
  remote screen.

### 2.11 MobileKeyboard — modifier bar (`MobileKeyboard:16-69`)

Soft keyboard up: **gutters disappear**; picture strip = full width ×
(H − keyboard − 44) (here 844×156); modifier bar 44 directly above the
keyboard; system keyboard 190 (mock, `:54-69`, not implemented).
- **Picture strip** (`:17-28`): the picture stays zoomed (remote drawn at
  1600×900, offset −880/−430, i.e. the cursor's area). [PROPOSED behaviour,
  ties to P1-4]: keyboard show/hide keeps the scene's absolute scale (does
  not re-fit the scene to the short strip — today `_layout` re-fits on any
  viewport change, `desktop_gesture_surface.dart:275-293`) and pans so the
  remote cursor stays visible; restore the previous offset on hide; never
  message the host.
- **Minimap** (`:21-23`), aria "小地图：当前显示远程屏幕的右下部分": top 10,
  right 12, 96×54 (width 96, height 96/aspect), radius 6, fill
  `rgba(20,20,22,0.8)`, border 1 px `rgba(255,255,255,0.25)`; visible-region
  rectangle 1.5 px `#7DB6F2` radius 2, clamped to the box (drawn 38×10 at
  53,25). No picture inside (cheap; no texture reuse). Shown when visible
  region < whole screen (zoomed or keyboard). [PROPOSED] IgnorePointer;
  optional drag-to-pan later. Needs the gesture surface to expose its visible
  normalised rect (`_offset`, `_scale`, `_scene`, `_viewport`,
  `desktop_gesture_surface.dart:45-50`).
- **Modifier status pill** (`:24-27`): bottom 10, left 12, 30 tall, padding
  0 12, radius 15, `rgba(24,24,26,0.88)`, 12.5 px, gap 8, dot 7 `#7DB6F2` +
  "Ctrl 已锁定：下一个键会和 Ctrl 一起发送" (for several:
  "Ctrl + Shift 已锁定：…" [PROPOSED]).
- **Modifier bar** (`:30-52`), role toolbar "修饰键与常用键": 44 tall, padding
  0 8, gap 6, fill `#1C1C1E`, top border `rgba(255,255,255,0.08)`, mono 13,
  horizontally scrollable if it overflows [PROPOSED]. Key caps 32 tall,
  radius 8, padding 0 10, fill `rgba(255,255,255,0.1)`, text `#EDEDEF`;
  latched/locked cap: fill `#EDEDEF`, text `#1D1D1F`, 600. Separators 1×20
  `rgba(255,255,255,0.14)` margin 0 2. Order:
  `Ctrl`(min 46) `Alt` `Shift` `Win` (min 42; toggles) | `Esc` `Tab` (min 40)
  `Fn` (min 40; toggle) | `←` `↑` `↓` `→` (36 wide; aria 左/上/下/右) | `Del`
  `Ctrl+C` `Ctrl+V` | spacer | collapse 40×32 transparent `chevron-down` 18,
  aria "收起键盘".
  - **Fn** replaces the middle groups with `F1`…`F12` while on
    (`canvas.json:45` "Fn 切换 F1–F12"; RustDesk `KeyHelpTools`,
    `<rustdesk checkout>\flutter\lib\mobile\pages\remote_page.dart:1007-1047`).
  - Modifier semantics: RustDesk toggles stay on until tapped again
    (`remote_page.dart:995-1006`); the pill text says "下一个键" (one-shot).
    **Open decision** — recommended: Windows Sticky-Keys model: tap = latch
    for the next key (pill text as drawn), double-tap = lock until tapped
    again (pill "Ctrl 已锁定" without "下一个键" [PROPOSED]). Either way: wrap
    each sent key as `mods↓ key↓ key↑ mods↑` in one ordered sequence
    (`DesktopStore.send`), never hold a modifier down on the host between
    keys; clear all latches on release/disconnect/background/view-only.
  - Host key support: Esc, Tab, arrows, Del, Ctrl+C/V, Home/End exist
    (`lib.rs:264-286`); **F1–F12 [GAP]**; typed characters go through the
    `text` input kind (`lib.rs:253`) and cannot carry modifiers — a latched
    Ctrl + typed letter needs letter `Key`s (today only A, C, V, X, Z) **[GAP]**.

### 2.12 MobileGestures — "操作方式" sheet (`MobileGestures:16-74`)

Sheet chrome as MobileAdd (top 48, radius 24, padding 8/20/34, gap 14). In
the landscape viewer [PROPOSED]: present as a centred panel max 390 wide,
scrollable.
1. Title "操作方式" 20/600 + close (36 circle `#F2F2F0`, `x` 18).
2. Segmented (`:26-29`): track `#EAEAE8` radius 12 padding 3 gap 3, segments
   38 tall radius 9, 14 px: "触控板" (default selected) | "直接触摸".
3. Description 13/1.5 `#4A4A4D` (`:30`): "手指在屏幕任意位置滑动来移动光标，适合精细操作。"
4. Gesture list (`:32-61`): rows 54 tall, bottom hairline `#EFEFED` (none on
   the last), gap 14: glyph tile 40 radius 12 `#F2F2F0` with a 24 px custom
   glyph (stroke `#1D1D1F` 1.5; see below); name 15/600 flex; result 14
   `#4A4A4D` right:

   | Gesture | Result | Glyph (24 box, from the artboard) |
   |---|---|---|
   | 单指滑动 | 移动光标 | filled dot r3 at (7,16) + arrow `M10 13l9-7M19 6h-4M19 6v4` |
   | 单指轻点 | 左键单击 | filled dot r3 (12,12) + dashed ring r7 (dash 2/3) |
   | 长按 | 右键单击 | filled dot r3 + ring r6.5 + ring r9.5 at 40 % opacity |
   | 轻点两下并按住移动 | 拖拽 | filled dot r3 (7,16) + dashed ring r5.5 (2/2.5) + arrow `M12 11l7-6M19 5h-4M19 5v4` |
   | 三指上下滑动 | 滚轮 | three filled dots r2.2 at x 6/12/18 y 12 + up/down arrows `M12 7V2M10 4l2-2 2 2M12 17v5M10 20l2 2 2-2` |
   | 双指移动 | 移动画面 | dots r2.4 at (9,13),(15,13) + up arrow and side chevrons `M12 3v4M10 5l2-2 2 2M3 13h2M21 13h-2M4 11l-1 2 1 2M20 11l1 2-1 2` |
   | 双指捏合 | 缩放画面 | dots r2.4 at (9,15),(15,9) + diagonal arrows `M6 18l-3 3M3 18v3h3M18 6l3-3M18 3h3v3` |

   Implement as one `CustomPainter` per glyph (no SVG package in pubspec).
5. Info box (`:63`): fill `#F4F4F2`, radius 14, padding 12/14, 13/1.6
   `#4A4A4D`: "直接触摸：点到哪里就在哪里点击，单指拖动即拖拽，其余手势相同。适合点按钮和菜单。"
6. Switch (`:65-71`): "显示虚拟鼠标" 15 + "直接触摸时提供左右键和滚轮" 12.5
   `#6E6E73`, default off; [PROPOSED] enabled only in direct touch.
   **Virtual mouse visuals not drawn** [PROPOSED: draggable floating pad 72×120,
   `viewerChrome` fill, radius 16: left button, scroll strip, right button].
7. "知道了" full width 48 radius 24 ink 15/600, pinned to the bottom
   (margin-top auto) → closes.
[PROPOSED] show once automatically on the first mobile viewer session; then
from More / long-press on the mode toggle.

**Gesture semantics required (brief §2 "Gestures") vs today's
`DesktopGestureSurface`:**

| Gesture | Required | Today (`desktop_gesture_surface.dart`) |
|---|---|---|
| tap | left click (trackpad: at cursor; direct: at touch) | same (`:256-260`) |
| long press (no movement, `kLongPressTimeout`) | **right click** on timer fire, haptic | starts a left **drag** (`:190-203`) → change |
| double-tap and hold, then move | **left drag** until lift (trackpad) | not supported → add (second down within `kDoubleTapTimeout` of a tap) |
| two-finger tap | **nothing** (navigation only) | right click (`:260`) → remove |
| two fingers move / pinch | local pan/zoom only | same (`:295-320`) |
| three fingers vertical | wheel (RustDesk: integral of dy/4, ±1 step, `remote_input.dart:517-529`) | not supported → add; must not also pan/zoom |
| direct touch: one-finger drag | **left drag** | moves pointer without button (`:234-238`) → change |
| direct touch: long press | right click at point | drag → change |
| mouse/trackpad on desktop | unchanged (hover move, buttons, wheel, Ctrl+wheel zoom, `:149-163,322-341`) | keep |

### 2.13 Icons — icon system rules (`Icons:18-118`, `canvas.json:48`)

Rules (`Icons:24-41`):
1. **One family**: everything through `AppIcons` (`theme/app_icons.dart`),
   Lucide `lucide_icons_flutter` 3.1.25 (`ui/pubspec.yaml:48`); window caption
   glyphs are self-drawn Windows-style, not icons; no Material `Icons.*`.
2. **Weight follows size**: ≤ 20 px → 400 (the 300 stroke is ~1.25 px at
   20 px and looks grey, `Icons:46`); ≥ 24 px → 300. Package families: 300 =
   `Lucide300` (stroke 1.5/24), 400 = `Lucide400` (stroke 2.0/24); identifiers
   `LucideIcons.<name>300` / `<name>400` (verified in the package source;
   `Dir` variants for direction-sensitive glyphs, e.g. `chevronRight400Dir`).
3. **Four sizes**: 16 labels/inline · 18 buttons/menus · 20 rail/bottom bar ·
   24 empty states/tiles. (Artboard SVGs at 13/14/15/17/22 px round to these.)
4. **Colour follows text**; status is told by coloured dots; blue only for
   "this computer" and actively transferring tunnels (and the live remote
   session's signal elements in the viewer).

Weight comparison artboard (`Icons:43-61`): nav glyphs at 16/20/24 in 300 vs
400 — informational.

Full catalogue (`Icons:124-157`; codepoints verified against the package):

| Section | Label | Lucide name | Codepoint | Badge |
|---|---|---|---|---|
| 导航 (20 px, 400) | 总览 | orbit | 58343 | |
| | 隧道 | route | 58686 | 改 (was cable) |
| | 远程桌面 | monitor | 57629 | 新 |
| | 中继 | radio-tower | 58372 | |
| | 日志 | scroll-text | 58463 | |
| | 设置 | settings | 57684 | |
| 隧道与路径 | 共享 / 连接 / 直连 / 部分直连 | arrow-up-from-line / arrow-down-to-line / zap / split | 58458 / 58453 / 57780 / 58432 | |
| | TCP / QUIC 载体 / 延迟 | arrow-right-left / layers / activity | 58391 / 58665 / 57400 | |
| | 这台电脑 / 服务 / 路由器 / 诊断 | laptop / server / router / stethoscope | 57805 / 57683 / 58303 / 58097 | |
| | 中继在线 / 中继离线 | cloud / cloud-off | 57480 / 57485 | 改 (was wifi / wifi-off) |
| 远程桌面 (all 新) | 开启共享 | screen-share | 57679 | |
| | 停止共享 | screen-share-off | 57680 | |
| | 远程电脑 | monitor-smartphone | 58274 | |
| | 仅查看 | eye | 57530 | |
| | 控制 | mouse-pointer-2 | 57795 | |
| | 键盘 | keyboard | 57988 | |
| | 快捷键 | command | 57498 | |
| | 右键 | mouse | 57998 | |
| | 触控板 | touchpad | 58441 | |
| | 直接触摸 | pointer | 57832 | |
| | 二维码 | qr-code | 57823 | |
| | 扫码 | scan-qr-code | 58870 | |
| | 全屏 | maximize | 57618 | |
| | 固定工具栏 | pin | 57945 | |
| | 断开 | power | 57664 | |
| | 辅助服务 | shield-check | 57855 | |
| | 安全桌面 | lock-keyhole | 58673 | |
| | 连接质量 | gauge | 57791 | |
| | 手机 | smartphone | 57699 | |
| 操作 (18 px, 400) | 新建 关闭 复制 编辑 删除 启动 停止 刷新 搜索 更多 导出 粘贴导入 打开链接 清空 跟随最新 列表 | plus x copy pencil trash-2 play square refresh-cw search ellipsis file-up clipboard-paste external-link eraser arrow-down list | (unchanged constants) | |
| 状态 | 完成 正常 提示 警告 错误 加密 凭据 关联 解除关联 显示 隐藏 | check circle-check info triangle-alert circle-alert lock key-round link unlink eye eye-off | | |
| 设置与方向 | 跟随系统 浅色 深色 语言 收藏 有更新 下载 展开 进入 返回 前进 | sun-moon sun moon languages star circle-fading-arrow-up download chevron-down chevron-right arrow-left arrow-right | | |

Where each new icon is used (this spec): `monitor` rail/bar entry, host and
remote tiles, viewer title, Display button; `screen-share` "开启共享"
(paused host) and the sharing indicator; `screen-share-off` stop-sharing menu
items/indicator action; `monitor-smartphone` empty state fallback/unknown
viewer; `eye` 查看 + view-only pill; `mouse-pointer-2` 控制; `keyboard` mobile
键盘 + control hint; `command` 快捷键 (both viewers); `mouse` 右键; `touchpad`
/ `pointer` mobile mode; `qr-code` 二维码; `scan-qr-code` 扫码添加 + camera
denied state; `maximize` 全屏; `pin` toolbar pin; `power` 断开; `shield-check`
开启远程控制 + recognised host; `lock-keyhole` secure-desktop card; `gauge`
quality panel title/More entry [PROPOSED]; `smartphone` viewer panel.
Not in the catalogue: the "显示" button glyph (drawn as a custom
rect-with-bars) → use `monitor` [DEVIATION]; mobile back drawn as
chevron-left → use `back` (arrow-left, per "返回") [DEVIATION].

Caption buttons sub-artboard (`Icons:63-93`): 46×32 each, glyph 10 px with
1 px stroke, close hover `#C42B1C` — see §2.14.

### 2.14 TitleBarFix — caption-button crowding (`TitleBarFix:16-76`)

Problem (`:19`): close button 8 px from the sheet edge, sheet 8 px from the
window edge; hover red nearly touches the rounded corner.

| Item | Now | Fix | Code |
|---|---|---|---|
| Right padding after caption buttons | 8 | **16** when caption buttons are shown (8 otherwise) | `shell/page_frame.dart:120` `EdgeInsetsDirectional.only(start: lead, end: 8)` |
| Gap primary action → caption buttons | 10 | **16** | `page_frame.dart:166-169` `SizedBox(width: 10)` + `CaptionButtons(height: 36)` |
| Button size / radius | 44×36, r10 | **46×32, r6**, vertically centred in the 58 px bar (13 px above/below) | `shell/window_chrome.dart:184-191` (`width: 44`, `height`, `circular(10)`) |
| Glyph | Material `Icons.remove` / `crop_square` / `filter_none` / `close` at 14-15 px | **self-drawn 10 px, 1 px stroke, pixel-snapped** | `window_chrome.dart:110-131,193` |
| Also | — | Main / HostSetup artboards are already drawn with the fix (`TitleBarFix:73`) | |

Glyph geometry in a 10×10 box (`Icons:72-89`):
- minimize: `M0 5h10`;
- maximize: rect (0.5, 0.5, 9×9, rx 1);
- restore (maximised): front rect (0.5, 2.5, 7×7, rx 1) + back path
  `M2.5 2.5V1.5a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1`;
- close: `M0.5 0.5l9 9M9.5 0.5l-9 9`.
Pixel snapping [PROPOSED algorithm]: `dpr = MediaQuery.devicePixelRatioOf`;
stroke = `max(1, (1*dpr).round()) / dpr`; glyph box = `(10*dpr).round()/dpr`;
translate so the box origin lands on a device pixel and odd-width strokes are
centred on pixel centres (+0.5 device px). Colours: idle fg `onSurfaceVariant`
(`#4A4A4D`), hover bg `rgba(0,0,0,0.06)` light (≈ `softWash`, dark
`white@0.09`) with fg `onSurface`; close hover bg `#C42B1C`, fg white
(`Icons:86-90`, as `window_chrome.dart:166-175`). Keep tooltips
(`windowMinimize/Maximize/Restore/Close`) and the maximize listener
(`window_chrome.dart:73-102`).

Acceptance (brief §4): at 100/125/150 %, maximised and restored, the hovered
close button's red stays ≥ 16 px from the sheet's right edge. With end
padding 16 inside the sheet this holds by construction.

### 2.15 RemoteScreen (`RemoteScreen:16-77`)

A mock Windows desktop (icons 此电脑/回收站/项目, a document window
"季度计划.docx — 文档", a terminal, a cursor toggled by prop `cursor`, a taskbar
with "14:32 2026/11/7"). Not a UI component. Use only as a painted fixture for
`test/preview_test.dart` / posters if a fake frame is wanted.

---

## 3. Data / control flow (step by step, as the design implies)

### 3.1 Desktop: first enable → sharing
1. Open rail "远程桌面" → page reads `DesktopStore` (`AppStore.desktop`,
   `state/app_store.dart:38-41`; `load()` polls every 250 ms,
   `desktop_store.dart:142-151`). Host card = NotEnabled (§2.3.1 table).
2. User toggles "允许远程操作鼠标和键盘", picks display, clicks "开启远程控制"
   → `enableHost(service, display)`. If the helper must be installed:
   Windows UAC (step 1) → `result.restart` → app restarts (step 2) →
   `restoreOnStartup()` resumes (`desktop_store.dart:153-203`) → Sharing card
   with the invite (step 3).
3. "复制" copies the full invite; "二维码" reveals a QR of it (compact form);
   leaving the page hides it.
4. A viewer connects: host status `streaming` (+ new viewer fields) → viewer
   panel; "改为仅查看" → `allowControl(false)`.
5. "停止共享" → `stopHost()`; the remembered grant stays → Paused card.

### 3.2 Desktop: add → view → back to page
1. "添加电脑" → AddComputer dialog → paste → local parse → recognised host →
   name → remember (default on) → "连接".
2. `DesktopStore.connect(invite, relayOnly)` → push full-window viewer route
   (single instance; today `_viewerOpen` guard, `desktop_page.dart:289-302`).
3. Viewer overlays follow `connectionPhase`; toolbar status follows path/fps.
4. Closing the viewer route (no disconnect) → page shows the row as
   "正在查看" with "回到画面"; viewer foreground false → control suspended
   (`setViewerForeground`, `desktop_store.dart:1024-1042`).
5. "断开" → `disconnect()` → record lastConnectedAt/lastPath (and the still for
   mobile) on the remembered computer.

### 3.3 Mobile: Remote tab → scan → viewer
1. Remote tab lists remembered computers (featured + others).
2. "扫码添加" → MobileAdd: camera → QR text → parse → confirm → connect.
3. Viewer opens landscape/immersive; gutters; trackpad default; hint 3 s.
4. "键盘" → soft keyboard + modifier bar; picture strip keeps zoom, minimap.
5. Back (left gutter) → pop to the tab; session continues as on desktop.

### 3.4 Input path (unchanged contract)
Every pointer/key leaves through `DesktopStore.send(rendered, event)`
(`desktop_store.dart:1044-1100`), carrying the presented frame's
generation/frame id; moves coalesce, buttons/keys are barriers; `_inFlight ≥ 16`
ends control. New UI (shortcuts, modifier bar, virtual mouse, three-finger
scroll) must use this path only.

---

## 4. Contracts and formats the design touches

- Input event JSON (`ffi/desktop_api.dart:466-480`, Rust `Input`,
  `crates/pb-mapper-desktop/src/lib.rs:248-261`): `{"kind":"move","x","y"}`,
  `{"kind":"button","button":"left|right|middle","down"}`,
  `{"kind":"wheel","horizontal","vertical"}`,
  `{"kind":"key","key":<Key snake_case>,"down"}`, `{"kind":"text","text"}`.
  `Key` allowlist: shift control alt meta enter escape tab backspace delete
  left right up down home end page_up page_down a c v x z
  (`lib.rs:264-286`); Dart map `_keyNames` (`pages/desktop_viewer.dart:541-568`).
  Design needs add: `d`, `l` (or a lock action), `f1`…`f12`, more letters for
  latched-modifier typing.
- Connect video preferences (`desktop_api.dart:436-446`): `max_width`,
  `max_height`, `max_fps`, `bitrate` — the quality preference maps here.
- Viewer status fields consumed by the overlays (`desktop_api.dart:59-132`):
  `state`, `message`, `controlling`, `control_allowed`, `generation`,
  `frame_id`, `display{id,name,width,height}`, `path`, `video{codec,width,
  height,keyframe,pts_us}`, `capture_fps`, `receive_fps`, `dropped_frames`,
  `bitrate`, `cursor{x,y,visible}`, `frame_ack_ms`, `direct_stage`,
  `direct_failure`. New fields the design implies [GAP]: network RTT, picture
  latency, IP family, encoder kind (hw/sw), last adaptation action; host side:
  viewer count/path/since.
- Invitation (`lib.rs:91-122`): JSON `{version:1, service, certificate, token}`,
  ≤ 8192 chars, token 43 chars; design shows `pbd1·…` → a compact,
  versioned text form (also for QR).
- Desktop FFI ops (`ui/native/pb_mapper_ffi/src/desktop.rs:193-260`):
  capabilities, status, trust_status, trust_revoke, host_restore,
  host_enable{service,display_id}, helper_action{action}, background_action
  {action}, host_start{…}, host_stop{on_exit}, host_permission{allow_control},
  connect{invitation,relay_only,native_video,video}, disconnect,
  control{enabled}, release, keyframe, presented{generation,frame_id},
  input{event}. Missing for the design: invite inspect/parse, remembered
  computers (encrypted CRUD), video-preference update mid-session, host
  display switch, lock-workstation action, connect progress.

---

## 5. Invariants and security boundaries the UI must keep

1. Input is sent only while `_interactive` (fresh presented frame of the
   current generation and epoch, control granted;
   `pages/desktop_viewer.dart:193-201`, `desktop_store.dart:125-140`). Every
   new control (shortcut items, modifier bar, virtual mouse, right-click
   button) is disabled otherwise; overlays that say "已暂停输入" must
   correspond to this state.
2. Ctrl+Alt+Del stays a disabled "暂不支持" item.
3. Zoom, pan, scale mode, minimap, keyboard show/hide, rotation, window
   resize, toolbar collapse are local; they never message the host, never
   rebuild the decoder/texture (`desktop_gesture_surface.dart:9-10,378-383`;
   tested in `test/desktop_gesture_test.dart`).
4. Never cover the native texture with an opaque layer
   (`desktop_viewer.dart:492-493`): scrims are translucent (0.72), the gray
   reconnect still is a separate retained image.
5. Invites are secrets: masked on screen (prefix + 4…4), never logged; QR only
   on explicit tap on the Remote desktop page, hidden on leave; camera frames
   never stored/uploaded; remembered invites encrypted at rest or not stored.
6. Identity mismatch stops auto-retry and requires a human
   (`desktop_store.dart:432-443`); "粘贴新邀请" must replace, never merge.
7. Persistent local stop: today `DesktopSharingBanner` (`pages/desktop_page.dart:305-338`,
   mounted above every route in `app.dart:321-336`) guarantees a stop button
   on every page, tested by `test/desktop_test.dart:280-308`. **No artboard
   draws it.** Open decision [PROPOSED]: keep a compact indicator — a pulsing
   blue dot on the 远程桌面 rail/bar entry + tray menu "停止共享" + the host
   card — or keep a slim banner outside the viewer. Do not silently drop the
   guarantee.
8. Modifier latches never leave keys held on the host; release on
   focus loss / background / disconnect (today `_releaseLocalInput`,
   `desktop_viewer.dart:418-424`).

---

## 6. Tests covering this scope today

- `ui/test/app_test.dart:39-63` rail with **five** pages (`nav-$page` keys),
  phone bottom navigation; `:90-115` no overflow on a small phone.
- `ui/test/desktop_test.dart` — background sharing stoppable (`:19-97`),
  viewer input mapping/blocking (`:98-224`), ordering/view-only/release
  (`:225-279`), compact host flow + persistent stop (`:280-308`, uses keys
  `open-desktop`, `desktop-share`, `desktop-stop` and text
  "This desktop is being shared").
- `ui/test/desktop_gesture_test.dart` — "touch taps wait for the gesture and
  two fingers click only right" (`:37`), "pinch keeps the texture mounted…"
  (`:77`), "trackpad moves without dragging and releases a held drag on pinch"
  (`:125`).
- `ui/test/desktop_viewer_navigation_test.dart:32` single viewer route.
- `ui/test/desktop_host_readiness_test.dart`, `desktop_lifecycle_test.dart`,
  `desktop_recovery_test.dart` — store logic (keep).
- `ui/test/preview_test.dart`, `gen_poster_test.dart` — headless page renders
  (no desktop pages yet).
No test covers caption buttons or icons.

---

## 7. Impact on the planned overhaul

### R2 — Remote desktop UI/UX redesign

**Information architecture**
- `shell/app_scope.dart:11-50`: add `AppPage.remote` between `tunnels` and
  `relay`; icon `AppIcons.remoteDesktop` (monitor); labels `navRemoteDesktop`
  (rail) / `navRemote` (bar); `grouped => true`.
- `shell/navigation.dart`: rail iterates 6 values (selection index math at
  `:31,75` still works); icon size 18→20; `_RelayFoot` adds latency. `NavBar`
  iterates `AppPage.values.where((p) => p != AppPage.logs)`; add top hairline.
- `app.dart:360-366` switch adds the page; `app.dart:124-135` `--desktop`
  start opens `AppPage.remote` instead of pushing `DesktopPage`;
  `app.dart:120` a remembered `logs` page on a compact layout maps to
  settings; `MaterialApp.builder` banner (`:321-336`) per decision in §5.7.
- `pages/settings_page.dart`: add a "日志" entry on compact layouts that
  pushes `LogsPage` (not drawn).
- `pages/tunnels_page.dart:32-38`: delete the `open-desktop` button.
- Delete `pages/desktop_page.dart` (both `DesktopPage` and the old banner, after
  §5.7 decision) and `pages/desktop_connection_banner.dart`; rewrite
  `pages/desktop_viewer.dart`.
- Tests: update `app_test.dart` nav lists (6 rail / 5 bar); rewrite
  `desktop_test.dart` navigation (`open-desktop` → `nav-remote`), keep its
  security assertions; add remote pages to `preview_test.dart`.
- Risk: `lastPage` prefs (`shell/app_prefs.dart:28,86`) and
  `AppPage.parse` stay compatible (new name only added).

**Remote page (Main/HostSetup/AddComputer/MobileHome/MobileAdd)**
- New `pages/remote/…` widgets; host card state machine (§2.3.1); list;
  dialogs/sheets; reuse `GroupCard`/`RaisedSurface`, `GlassSegmented`,
  `StatusChip(inkLabel)`, `confirmDestructive`, `showAdaptivePanel`.
- New state: a remembered-computers store (+ encrypted persistence via FFI;
  Windows DPAPI code exists only for host trust in
  `ui/native/pb_mapper_ffi/src/desktop_trust.rs`; Android Keystore is new) —
  coupling with the FFI/trust subsystem.
- Host card needs new host-status fields (viewer count/path/since), an
  allow-control parameter on enable, and optionally a live display switch —
  coupling with `crates/pb-mapper-desktop` host/worker.
- Invite: compact `pbd1` format + QR generator + mobile scanner + permissions
  — coupling with `crates/pb-mapper-desktop/src/lib.rs` `Invitation` and with
  release packaging (new Flutter plugins on Android/iOS).
- Online status from `AppStore.relayServices` — coupling with the relay check
  cadence and namespaces.

**Desktop viewer (Viewer/ViewerStates)**
- Full-window route, own dark title bar, toolbar (pin/collapse 2 s), Display
  and Shortcuts menus, quality panel, overlays replacing the banner.
- `DesktopStore` additions: `retryNow()`, public `refreshPicture()` (keyframe),
  quality preference + mid-session apply (FFI op), expose metrics; retained
  still across reconnect (conflicts with `_clearVideo` disposing the texture
  at attempt start, `desktop_store.dart:906`) — coupling with the media
  pipeline (P1-1 may move frames out of Dart; the still must then come from
  the native decoder).
- Shortcuts need host `Key` additions (`d`, `l`/lock action, F-keys) —
  coupling with the input subsystem and its allowlist/security review.

**Mobile viewer (MobileViewer/MobileKeyboard/MobileGestures)**
- Gutter layout computed from picture aspect; immersive/landscape handling;
  modifier bar with latch semantics; minimap from the surface's transform;
  keyboard-aware viewport that keeps absolute scale.
- `pages/desktop_gesture_surface.dart` gesture rewrite (§2.12 table): long
  press → right click, double-tap-hold drag, three-finger scroll, two-finger
  tap removed, direct-touch drag = left drag, touch indicator, expose visible
  rect + scale-mode API. Update `test/desktop_gesture_test.dart` (the
  two-finger-right-click and long-press-drag expectations become obsolete);
  add one deliberate test per new gesture boundary (no matrices).
- Release notes/docs: gesture changes (brief "Docs to update").

### R3 — Icons
- `theme/app_icons.dart`: switch every constant from `…300` to `…400` (and
  `chevronRight300Dir`/`back`/`forward` to `…400Dir`); swaps `tunnels =
  route400`, `online = cloud400`, `offline = cloudOff400`; add
  `remoteDesktop = monitor400` and the 19 remote-desktop constants; add 300
  "large" constants only where ≥ 24 px is used: today only
  `galaxy/galaxy_panel.dart:152` (`relay`, 28 px) plus new empty-state/tile
  uses. Update the class doc comment (says "at its 300 stroke",
  `app_icons.dart:4-6`) and `docs/ui-design.md:204`.
- Replace all 18 Material icon uses: `shell/window_chrome.dart:111,117(×2),126`
  (→ self-drawn glyphs), `pages/desktop_page.dart:102,108,136,189,324`,
  `pages/desktop_viewer.dart:347,352,357,368,379,407`,
  `pages/tunnels_page.dart:37`, `pages/desktop_connection_banner.dart:55,61`
  (most disappear with the rewrite).
- Rail/bar icon sizes 18→20 (`navigation.dart:147`); `iconTheme` default size
  stays 20 (`app_theme.dart:584`).
- Risk: global visual change (every icon thickens); goldens/posters
  (`test/gen_poster_test.dart`) regenerate.

### R4 — Title-bar fix
- `shell/page_frame.dart:120,166-169` (end padding 16 when
  `drawsCaptionButtons`, gap 16, `CaptionButtons()` without height);
  `shell/window_chrome.dart:64-199` (fixed 46×32 r6, `CustomPainter` glyphs,
  pixel snapping). The viewer's 36 px title bar reuses `CaptionButtons`.
- [PROPOSED] one geometry test with `debugDefaultTargetPlatformOverride =
  TargetPlatform.windows`: close button right edge ≥ 16 px from the
  `ContentSheet` right edge (window_manager calls are already try/caught,
  `window_chrome.dart:89-96`).

### R1 — Performance (only where the design shows it)
- Quality preference (画质优先/平衡/流畅优先) and the quality panel metrics
  (RTT, picture latency, display fps, bitrate, resolution, codec, adaptation
  note) depend on P0-1/P0-2/P0-4/P1-2 producing those numbers; the UI must
  read them without a display-path dependency (poll a "latest metrics" value
  ≤ 1 Hz, not per frame). The "画面变慢" threshold must use transport-based
  picture latency, not `frameAckMs` (which includes phone processing).
- Minimap, touch indicator and overlays must not add per-frame Dart work on
  the texture path (they repaint only on transform/state change).

### Open decisions (blockers to settle before implementing)
1. Persistent stop indicator now that the banner is not in the design (§5.7).
2. Modifier latch model: one-shot vs toggle (§2.11).
3. `pbd1` compact invite format (needed for QR size) vs QR of the raw JSON.
4. Win+L: host lock action vs leaving it out of the first cut.
5. Mobile large-title header vs shared `PageFrame` (§2.8).
6. Whether to keep `GlassSegmented` capsule corners or add the 12/9 radius.
7. Undrawn screens: Display menu, desktop QR panel, More menus, virtual
   mouse, mobile portrait viewer, mobile empty Remote tab, post-scan confirm,
   camera-denied state, Paused/Unsupported host card, Ended/Recovering
   overlays — proposals above; confirm with the owner if fidelity matters.

---

## Appendix A — String table (zh exact from artboards; en draft; ARB reuse)

Reuse existing keys where noted (`ui/lib/l10n/app_zh.arb`). Existing ARB
uses 分享 ("停止分享", "本机桌面正在分享"); the design uses **共享** throughout —
update those values.

| Key [PROPOSED] | zh (exact) | en draft | Source |
|---|---|---|---|
| navRemoteDesktop | 远程桌面 | Remote desktop | AppRail:33 (= `desktopTitle`) |
| navRemote | 远程 | Remote | MobileTabBar:25, MobileHome:19 |
| remoteAddComputer | 添加电脑 | Add computer | Main:29, MobileAdd:20 |
| refresh | 刷新 | Refresh | existing `refresh` |
| hostThisComputer | 这台电脑 | This computer | Main:46 |
| hostOff | 尚未开启远程控制 | Remote control is off | HostSetup:40 |
| hostSharing | 正在共享 | Sharing | Main:49 |
| hostNotSharing | 未共享 | Not sharing | HostSetup:42, Main:155 |
| hostViewersControlling | {n} 个查看者正在控制 | {n} viewer(s) in control | Main:55 |
| hostViewerMeta | {path} · 已连接 {minutes} 分钟 | {path} · connected {minutes} min | Main:56 |
| hostMakeViewOnly | 改为仅查看 | Make view-only | Main:58 |
| hostPermissionLabel | 持有邀请的人可以 | People with the invitation can | Main:62 |
| hostPermissionView | 仅查看画面 | View only | Main:64 |
| hostPermissionControl | 查看并控制 | View and control | Main:65, AddComputer:50 |
| desktopInvitation | 私密邀请 | Private invitation | existing |
| copy | 复制 | Copy | existing `copy` |
| hostQr | 二维码 | QR code | Main:79 |
| hostInviteHelp | 只发给你信任的设备。持有邀请的人可以查看，允许控制时也可以操作。 | Send it only to devices you trust. Anyone holding it can watch, and control when you allow it. | Main:82 |
| hostDisplay | 共享的显示器 | Shared display | Main:87 |
| hostDisplayValue | 显示器 {n} · {w} × {h} | Display {n} · {w} × {h} | Main:88 |
| hostBackground | 后台运行 | Runs in background | Main:93 |
| hostBackgroundValue | Windows 服务 · 关闭窗口后继续共享 | Windows service · keeps sharing after the window closes | Main:94 |
| hostSecure | UAC 与锁屏 | UAC and lock screen | Main:97 |
| hostHelperReady | 辅助服务已就绪 | Helper service ready | Main:98 |
| hostStop | 停止共享 | Stop sharing | Main:103 (replace `desktopStop` value) |
| more | 更多 | More | Main:106, Viewer:65 |
| hostStopNote | 停止后邀请保留，下次开启继续可用 | The invitation is kept and works again when you turn sharing back on | Main:108 |
| setupHeadline | 用手机或另一台电脑，\n随时看到并操作这台电脑 | See and control this computer from your phone or another computer, anytime | HostSetup:45 |
| setupStep1Title / Body | Windows 请求一次管理员批准 / 安装受保护的辅助服务，远程时也能操作 UAC 提示和锁屏。 | Windows asks for administrator approval once / Installs the protected helper so UAC prompts and the lock screen work remotely. | HostSetup:51-52 |
| setupStep2Title / Body | 应用自动重启并回到这里 / 已有的隧道和配置保持不变。 | The app restarts and comes back here / Your tunnels and settings stay as they are. | HostSetup:58-59 |
| setupStep3Title / Body | 得到一条私密邀请 / 复制给另一台电脑，或让手机扫码。之后一直有效，直到你撤销。 | You get a private invitation / Copy it to another computer or scan it with a phone. It works until you revoke it. | HostSetup:65-66 |
| setupAllowControl | 允许远程操作鼠标和键盘 | Allow remote mouse and keyboard | HostSetup:73 |
| desktopEnable | 开启远程控制 | Turn on remote control | existing |
| setupOrdinaryOnly | 只共享普通桌面（不安装辅助服务） | Share the ordinary desktop only (no helper service) | HostSetup:90 |
| setupBackgroundNote | 关闭窗口后仍在后台共享 | Keeps sharing in the background after the window closes | HostSetup:91 |
| remoteListTitle | 远程电脑 | Remote computers | Main:114 |
| remoteEmptyTitle | 还没有远程电脑 | No remote computers yet | HostSetup:108 |
| remoteEmptyBody | 在另一台电脑的 pb-mapper 里开启远程控制，复制它的私密邀请，粘贴到这里。 | Turn on remote control in pb-mapper on the other computer, copy its private invitation and paste it here. | HostSetup:109 |
| remoteViewing | 正在查看 | Viewing | Main:127 |
| remoteOnline | 在线 | Online | Main:140, MobileHome:36 |
| remoteBackToPicture | 回到画面 | Back to picture | Main:128 |
| desktopOpen | 打开 | Open | existing |
| remoteLast / remoteLastPath | 上次 {when} / 上次 {when} · {path} | Last {when} / Last {when} · {path} | Main:139,154 |
| pathDirect / pathRelay | 直连 / 中继 | Direct / Relay | existing |
| remoteAddHint | 粘贴另一台电脑的私密邀请 | Paste another computer's private invitation | Main:162 |
| remoteRememberNote | 只有选择“记住此电脑”的邀请会保存在本机，并由系统加密。 | Only invitations you choose to remember are saved on this device, encrypted by the system. | Main:164 |
| addTitle | 添加远程电脑 | Add a remote computer | AddComputer:29 |
| paste | 粘贴 | Paste | AddComputer:41 |
| addRecognized | 已识别主机身份 · 权限：{permission} · 通过当前中继连接 | Host identity recognised · {permission} · via the current relay | AddComputer:50 |
| addName | 名称 | Name | AddComputer:55 |
| addRemember | 记住此电脑 | Remember this computer | AddComputer:62, MobileAdd:64 |
| addRememberHelp / Mobile | 邀请加密保存在本机，可以随时移除 / 邀请加密保存在这台手机上 | Saved encrypted on this device; remove it anytime / Saved encrypted on this phone | AddComputer:63, MobileAdd:65 |
| addMoreOptions | 更多选项（仅使用中继） | More options (relay only) | AddComputer:69, MobileAdd:71 |
| desktopRelayOnly | 仅使用中继 | Relay only | existing |
| cancel | 取消 | Cancel | existing |
| addConnect | 连接 | Connect | AddComputer:75 |
| viewerPin / viewerExpand | 固定工具栏 / 展开工具栏 | Pin toolbar / Show toolbar | Viewer:34, ViewerStates:111 |
| viewerModeView / Control | 查看 / 控制 | View / Control | Viewer:47,51 |
| viewerDisplay | 显示 | Display | Viewer:57 |
| viewerShortcuts | 快捷键 | Shortcuts | Viewer:62, MobileViewer:54 |
| viewerFullscreen | 全屏 | Full screen | Viewer:69 |
| viewerDisconnect | 断开 | Disconnect | Viewer:74, ViewerStates:100 |
| shortcutShowDesktop … | 显示桌面 / 切换窗口 / 任务管理器 / 开始菜单 / 锁定远程电脑 | Show desktop / Switch windows / Task Manager / Start menu / Lock remote computer | Viewer:79-83 |
| shortcutUnsupported | 暂不支持 | Not supported | Viewer:85 |
| viewerRefreshPicture | 刷新画面 | Refresh picture | Viewer:87 |
| viewerReleaseKeys | 释放所有按键 | Release all keys | Viewer:88 |
| viewerControlHint / Sub | 键盘和鼠标正发往{name} / 系统快捷键请从工具栏发送 | Keyboard and mouse go to {name} / Send system shortcuts from the toolbar | Viewer:93-94 |
| stateConnecting | 正在连接 {name} | Connecting to {name} | ViewerStates:24 |
| stepFoundDirect / stepIdentity / stepFirstFrame | 找到主机，走直连路径 / 校验主机身份 / 等待第一帧画面 | Host found, going direct / Checking the host's identity / Waiting for the first picture | ViewerStates:27-29 |
| stateSlow | {path} · 画面延迟 | {path} · picture delay | ViewerStates:41 |
| qualityTitle + rows | 连接质量 / 路径 / 网络往返 / 画面延迟 / 显示帧率 / 码率 / 分辨率 / 编码 / H.264 硬件 | Connection quality / Path / Network round trip / Picture delay / Display rate / Bitrate / Resolution / Codec / H.264 hardware | ViewerStates:45-52 |
| qualityAdaptedBitrate | 已自动降低码率，优先保证操作跟手 | Bitrate lowered automatically to keep input responsive | ViewerStates:53 |
| stateSecureTitle / Body | 远程电脑正在显示安全桌面 / 出现了 UAC 提示或锁屏。这台电脑没有启用辅助服务，无法显示这一画面；恢复到普通桌面后自动继续。 | The remote computer is on its secure desktop / A UAC prompt or the lock screen is showing. Without the helper service it can't be shown; viewing resumes when the ordinary desktop returns. | ViewerStates:66-68 |
| inputPaused | 已暂停输入 | Input paused | ViewerStates:69,83 |
| stateRetry | 连接中断，{s} 秒后第 {n} 次重连 | Connection lost — retry {n} in {s} s | ViewerStates:82 |
| retryNow | 立即重试 | Retry now | ViewerStates:84 |
| desktopIdentityMismatch / Help | 主机身份已变化 / 这台电脑的身份与邀请里记录的不一致，已停止自动重连。请向对方要一条新的邀请，不要继续使用旧邀请。 | Host identity changed / This computer no longer matches the invitation, so reconnecting stopped. Ask for a new invitation and stop using the old one. | ViewerStates:96-98 (replace Help value) |
| pasteNewInvite | 粘贴新邀请 | Paste new invitation | ViewerStates:101 |
| viewOnlyByHost | 仅查看 · 对方未允许控制 | View only · control not allowed | ViewerStates:116 |
| mobileLastPicture | 上次画面 · {when} | Last picture · {when} | MobileHome:28 |
| mobilePathPermission | {path} · 可以查看和控制 | {path} · can view and control | MobileHome:34 |
| mobileOthers | 其他电脑 | Other computers | MobileHome:42 |
| mobileOnlineLastRelay | 在线 · 上次走中继 | Online · last via relay | MobileHome:48 |
| mobileOfflineLast | 未在共享 · 上次 {when} | Not sharing · last {when} | MobileHome:57 |
| mobileScanAdd / mobilePasteInvite | 扫码添加 / 粘贴邀请 | Scan to add / Paste invitation | MobileHome:66,70 |
| mobileHostNote | 手机可以查看并控制 Windows 电脑，暂不能被远程控制。 | This phone can view and control Windows computers; it can't be controlled itself. | MobileHome:73 |
| scanHint | 对准电脑“远程桌面”页上的二维码 | Point at the QR code on the computer's Remote desktop page | MobileAdd:49 |
| or | 或 | or | MobileAdd:53 |
| cameraNote | 相机只用来识别二维码，画面不会保存或上传。 | The camera only reads the QR code; nothing is saved or uploaded. | MobileAdd:74 |
| mobileBack (a11y) | 返回远程列表 | Back to remote list | MobileViewer:30 |
| mobileQuality (a11y) | 连接质量：{path}，{fps} 帧每秒 | Connection quality: {path}, {fps} fps | MobileViewer:33 |
| mobileModeToggle (a11y) | 切换操作方式，当前为{mode} | Switch input mode, now {mode} | MobileViewer:39 |
| desktopTrackpadMode / desktopTouchMode | 触控板 / 直接触摸 | Trackpad / Direct touch | existing |
| trackpadHint | 触控板模式：单指滑动移动光标，轻点单击 | Trackpad: slide one finger to move, tap to click | MobileViewer:25 |
| mobileKeyboard / mobileRightClick | 键盘 / 右键 | Keyboard / Right click | MobileViewer:50,58 |
| hideKeyboard (a11y) | 收起键盘 | Hide keyboard | MobileKeyboard:49 |
| modifierLatched | {keys} 已锁定：下一个键会和 {keys} 一起发送 | {keys} held: the next key is sent with {keys} | MobileKeyboard:26 |
| minimap (a11y) | 小地图：当前显示远程屏幕的{region}部分 | Minimap: showing the {region} of the remote screen | MobileKeyboard:21 |
| gestureTitle | 操作方式 | Input mode | MobileGestures:20 |
| trackpadDescription | 手指在屏幕任意位置滑动来移动光标，适合精细操作。 | Slide anywhere to move the pointer — best for precise work. | MobileGestures:30 |
| gesture rows | 单指滑动/移动光标 · 单指轻点/左键单击 · 长按/右键单击 · 轻点两下并按住移动/拖拽 · 三指上下滑动/滚轮 · 双指移动/移动画面 · 双指捏合/缩放画面 | One-finger slide/Move pointer · Tap/Left click · Long press/Right click · Double-tap, hold and move/Drag · Three-finger swipe up or down/Scroll · Two-finger move/Pan picture · Pinch/Zoom picture | MobileGestures:35-59 |
| directTouchDescription | 直接触摸：点到哪里就在哪里点击，单指拖动即拖拽，其余手势相同。适合点按钮和菜单。 | Direct touch: tap where you want to click, drag with one finger; other gestures are the same. Best for buttons and menus. | MobileGestures:63 |
| virtualMouse / Help | 显示虚拟鼠标 / 直接触摸时提供左右键和滚轮 | Show virtual mouse / Left/right buttons and a wheel in direct touch | MobileGestures:67-68 |
| gotIt | 知道了 | Got it | MobileGestures:73 |

Obsolete ARB keys after the rewrite (delete with the code): `desktopGestureHelp`
(describes two-finger right click and long-press drag), `desktopLeftClick`,
`desktopScrollUp/Down`, `desktopNavigate`, `desktopInteract`,
`desktopAckLatency`, `desktopFrameAge`, `desktopRecoveryAttempt`,
`desktopShowInvitation`, `desktopPaste`, `desktopConnect`, `desktopShare`
(verify each with a grep before deleting).
