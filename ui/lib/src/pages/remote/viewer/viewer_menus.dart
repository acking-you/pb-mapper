import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../../l10n/app_localizations.dart';
import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import 'quality_panel.dart';
import 'remote_input.dart';
import 'remote_keys.dart';
import 'remote_viewport.dart';
import 'viewer_toolbar.dart';

/// One line of a viewer menu. Each menu is defined once, as entries, and
/// drawn by the desktop toolbar ([ViewerMenuButton]) or the touch side
/// panel.
sealed class ViewerMenuEntry {
  const ViewerMenuEntry();
}

final class ViewerMenuSection extends ViewerMenuEntry {
  const ViewerMenuSection(this.label);
  final String label;
}

final class ViewerMenuDivider extends ViewerMenuEntry {
  const ViewerMenuDivider();
}

final class ViewerMenuItem extends ViewerMenuEntry {
  const ViewerMenuItem({
    required this.key,
    required this.label,
    this.hint,
    this.detail,
    this.icon,
    this.enabled = true,
    this.checked,
    this.danger = false,
    this.disabledHint,
    required this.onSelected,
  });

  final Key key;
  final String label;

  /// Key caps at the end, in mono: "Win + D".
  final String? hint;

  /// A second line that says what the choice means.
  final String? detail;
  final IconData? icon;
  final bool enabled;

  /// Null: not a check or radio item. Otherwise a 16 px check slot leads.
  final bool? checked;
  final bool danger;

  /// Shown at the end instead of [hint] while disabled: why.
  final String? disabledHint;
  final FutureOr<void> Function() onSelected;
}

/// A choice of a few: a submenu on desktop, with [value] (the current
/// choice) at its end, and on a touch screen a section of its own, as its
/// [entries] laid out flat under [label].
final class ViewerMenuSubmenu extends ViewerMenuEntry {
  const ViewerMenuSubmenu({
    required this.key,
    required this.label,
    required this.value,
    required this.entries,
    this.note,
  });

  final Key key;
  final String label;
  final String value;
  final List<ViewerMenuEntry> entries;

  /// Above the choices: what choosing one does.
  final String? note;
}

/// What the menus and overlays act on, built by each viewer from its own
/// explicit dependencies.
final class ViewerActions {
  const ViewerActions({
    required this.desktop,
    required this.input,
    required this.prefs,
    required this.viewport,
    required this.onDisconnect,
    required this.onQuality,
    required this.onResolution,
    required this.onMaxFps,
    required this.onTypeText,
    required this.onDisplayMode,
    this.onBack,
    this.onGestureSheet,
    this.onReconnect,
    this.onPasteNewInvite,
  });

  final DesktopStore desktop;
  final RemoteInput input;
  final ViewerPrefs prefs;
  final RemoteViewport viewport;

  /// Ends the session and leaves the viewer.
  final Future<void> Function() onDisconnect;

  /// A live change of the picture preference (D12).
  final ValueChanged<DesktopQuality> onQuality;

  /// The resolution (D7) and the maximum frame rate, 0 for Auto (D8): each
  /// connects again at once.
  final ValueChanged<DesktopResolution?> onResolution;
  final ValueChanged<int> onMaxFps;
  final VoidCallback onTypeText;

  /// The remote display's real mode (R8): live, never a reconnect.
  final ValueChanged<DisplayModeTarget> onDisplayMode;

  /// Back to the list, the session kept; null where there is no list.
  final VoidCallback? onBack;
  final VoidCallback? onGestureSheet;
  final VoidCallback? onReconnect;

  /// Null: the action is hidden.
  final VoidCallback? onPasteNewInvite;
}

/// Scaling (local only, P1-4: the stage applies it to the viewport), the
/// picture preference (live, D12), what only a new session takes —
/// resolution, maximum frame rate and codec — and the quality panel and
/// monitor.
List<ViewerMenuEntry> displayMenu(AppLocalizations l10n, ViewerActions a) => [
  ViewerMenuSection(l10n.viewerScaleLabel),
  for (final (mode, key, label) in [
    (ViewerScaleMode.fit, 'display-fit', l10n.viewerScaleFit),
    (ViewerScaleMode.original, 'display-original', l10n.viewerScaleOriginal),
  ])
    ViewerMenuItem(
      key: Key(key),
      label: label,
      checked: a.prefs.scaleMode == mode,
      onSelected: () => a.prefs.setScaleMode(mode),
    ),
  const ViewerMenuDivider(),
  ViewerMenuSection(l10n.viewerQualityLabel),
  ..._qualityItems(l10n, a),
  ..._sessionChoices(l10n, a),
  ..._remoteResolution(l10n, a),
  const ViewerMenuDivider(),
  ..._figures(l10n, a, touch: false),
];

/// The remote display's real mode, as RustDesk's Resolution menu offers it
/// (R8, R18): on a desktop runner only, while this viewer controls a host
/// that offers more than one mode. Fit local where it differs from now, the
/// original where a viewer changed it, then every mode offered. Live: never
/// a reconnect; the host puts its own mode back when the session ends.
List<ViewerMenuEntry> _remoteResolution(
  AppLocalizations l10n,
  ViewerActions a,
) {
  final session = a.desktop.session.value;
  final modes = session.displayModes;
  if (!desktopRunner ||
      modes == null ||
      !session.controlling ||
      modes.supported.length < 2) {
    return const [];
  }
  final fit = modes.fitLocal;
  final local = a.desktop.localDisplay;
  return [
    ViewerMenuSubmenu(
      key: const Key('display-remote-resolution'),
      label: l10n.viewerRemoteResolution,
      value: modes.current.label,
      note: l10n.viewerRemoteResolutionNote,
      entries: [
        if (fit != null && local != null && fit != modes.current)
          ViewerMenuItem(
            key: const Key('remote-resolution-fit'),
            label: l10n.viewerRemoteResolutionFit(fit.label),
            onSelected: () =>
                a.onDisplayMode(DisplayModeTarget.fitLocal(local)),
          ),
        if (modes.changed)
          ViewerMenuItem(
            key: const Key('remote-resolution-original'),
            label: l10n.viewerRemoteResolutionOriginal(modes.original.label),
            onSelected: () =>
                a.onDisplayMode(const DisplayModeTarget.original()),
          ),
        for (final mode in modes.supported)
          ViewerMenuItem(
            key: Key('remote-resolution-${mode.width}x${mode.height}'),
            label: mode.label,
            checked: mode == modes.current,
            onSelected: () => a.onDisplayMode(DisplayModeTarget.size(mode)),
          ),
      ],
    ),
  ];
}

/// Windows, macOS and Linux: where a viewer sits at a display of its own
/// that Fit local can match.
bool get desktopRunner => const {
  TargetPlatform.windows,
  TargetPlatform.macOS,
  TargetPlatform.linux,
}.contains(defaultTargetPlatform);

/// The quality panel and the quality monitor, each shown or not.
List<ViewerMenuEntry> _figures(
  AppLocalizations l10n,
  ViewerActions a, {
  required bool touch,
}) => [
  ViewerMenuItem(
    key: Key(touch ? 'more-quality-panel' : 'display-quality-panel'),
    label: l10n.viewerQuality,
    checked: a.prefs.qualityPanel,
    onSelected: () => a.prefs.setQualityPanel(!a.prefs.qualityPanel),
  ),
  ViewerMenuItem(
    key: Key(touch ? 'more-quality-monitor' : 'display-quality-monitor'),
    label: l10n.viewerQualityMonitor,
    checked: a.prefs.qualityMonitor,
    onSelected: () => a.prefs.setQualityMonitor(!a.prefs.qualityMonitor),
  ),
];

/// What a session takes at connect, so a change connects again: the
/// computer's resolution (D7) and maximum frame rate (D8), and this
/// device's codec (D6). A size the decoder of the codec it would carry
/// cannot take at the remote display's size is shown disabled; the
/// default, which Rust fits inside that decoder, says how large it goes
/// here. A rate above the display's is not offered.
List<ViewerMenuEntry> _sessionChoices(AppLocalizations l10n, ViewerActions a) {
  final target = a.desktop.session.value.target;
  final resolution = target?.resolution;
  final maxFps = target?.maxFps ?? 0;
  final codec = a.prefs.codec;
  String resolutionLabel(DesktopResolution? r) => switch (r) {
    null => l10n.viewerResolutionDefault,
    DesktopResolution.native => l10n.viewerResolutionNative,
    _ => r.wire,
  };
  final limit = a.desktop.decodes(null) ? null : a.desktop.decoderLimit();
  String fpsLabel(int fps) => fps == 0 ? l10n.viewerMaxFpsAuto : '$fps fps';
  // Auto names the codec the session decodes, as RustDesk's menu does.
  final inUse = a.desktop.picture.value?.codec;
  final codecValue = codec == DesktopCodecPreference.h264
      ? 'H.264'
      : inUse == null || inUse == 'jpeg'
      ? l10n.viewerCodecAuto
      : '${l10n.viewerCodecAuto} · ${codecLabel(inUse)}';
  return [
    ViewerMenuSubmenu(
      key: const Key('display-resolution'),
      label: l10n.viewerResolutionLabel,
      value: resolutionLabel(resolution),
      note: l10n.viewerReconnectsToApply,
      entries: [
        ViewerMenuItem(
          key: const Key('resolution-default'),
          label: l10n.viewerResolutionDefaultNative,
          detail: limit == null
              ? l10n.viewerResolutionNativeHint
              : l10n.viewerResolutionLimited(
                  '${limit.maxWidth}×${limit.maxHeight}',
                ),
          checked: resolution == null,
          onSelected: () => a.onResolution(null),
        ),
        for (final r in DesktopResolution.values)
          if (a.desktop.decodes(r))
            ViewerMenuItem(
              key: Key('resolution-${r.wire}'),
              label: resolutionLabel(r),
              checked: resolution == r,
              onSelected: () => a.onResolution(r),
            )
          else
            // Why, on its own line: too long to stand at the row's end.
            ViewerMenuItem(
              key: Key('resolution-${r.wire}'),
              label: resolutionLabel(r),
              detail: l10n.viewerResolutionUnsupported,
              checked: resolution == r,
              enabled: false,
              onSelected: () {},
            ),
      ],
    ),
    ViewerMenuSubmenu(
      key: const Key('display-max-fps'),
      label: l10n.viewerMaxFpsLabel,
      value: fpsLabel(maxFps),
      note: l10n.viewerReconnectsToApply,
      entries: [
        for (final fps in [
          0,
          ...maxFpsChoicesFor(a.desktop.displayMaxHz, chosen: maxFps),
        ])
          ViewerMenuItem(
            key: Key('max-fps-$fps'),
            label: fpsLabel(fps),
            detail: fps == 0 ? l10n.viewerMaxFpsAutoHint : null,
            checked: maxFps == fps,
            onSelected: () => a.onMaxFps(fps),
          ),
      ],
    ),
    ViewerMenuSubmenu(
      key: const Key('display-codec'),
      label: l10n.viewerCodecLabel,
      value: codecValue,
      note: l10n.viewerReconnectsToApply,
      entries: [
        for (final (choice, label, detail) in [
          (
            DesktopCodecPreference.auto,
            l10n.viewerCodecAuto,
            l10n.viewerCodecAutoHint,
          ),
          (DesktopCodecPreference.h264, 'H.264', l10n.viewerCodecH264Hint),
        ])
          ViewerMenuItem(
            key: Key('codec-${choice.name}'),
            label: label,
            detail: detail,
            checked: codec == choice,
            onSelected: () {
              // The device's choice: kept, then taken by this session.
              unawaited(a.prefs.setCodec(choice));
              a.desktop.setCodec(choice);
            },
          ),
      ],
    ),
  ];
}

List<ViewerMenuEntry> _qualityItems(AppLocalizations l10n, ViewerActions a) => [
  for (final (quality, label, detail) in [
    (
      DesktopQuality.quality,
      l10n.viewerQualityBest,
      l10n.viewerQualityBestHint,
    ),
    (
      DesktopQuality.balanced,
      l10n.viewerQualityBalanced,
      l10n.viewerQualityBalancedHint,
    ),
    (
      DesktopQuality.smooth,
      l10n.viewerQualitySmooth,
      l10n.viewerQualitySmoothHint,
    ),
  ])
    ViewerMenuItem(
      key: Key('quality-${quality.name}'),
      label: label,
      detail: detail,
      checked: a.desktop.quality.value == quality,
      onSelected: () => a.onQuality(quality),
    ),
];

/// How this computer's keyboard reaches the remote one, chosen for that
/// computer: whose input method types, and, where exactly one of the two
/// is a Mac, whether Ctrl and Cmd trade places.
List<ViewerMenuEntry> _keyboardItems(AppLocalizations l10n, ViewerActions a) {
  final input = a.input;
  final keyboard = input.keyboard;
  final physical = a.desktop.session.value.capabilities.contains(
    'physical_keys',
  );
  void choose(KeyboardSettings next) => unawaited(
    a.prefs.setKeyboard(a.desktop.session.value.target?.computerId, next),
  );
  final local = keyboard.mode == KeyboardMode.local;
  return [
    ViewerMenuSubmenu(
      key: const Key('keyboard-mode'),
      label: l10n.keyboardInputMethod,
      value: local ? l10n.keyboardLocal : l10n.keyboardRemote,
      note: l10n.keyboardInputMethodNote,
      entries: [
        for (final (mode, label, detail) in [
          (KeyboardMode.local, l10n.keyboardLocal, l10n.keyboardLocalHint),
          (
            KeyboardMode.remote,
            l10n.keyboardRemote,
            physical ? l10n.keyboardRemoteHint : l10n.keyboardRemoteOlderHost,
          ),
        ])
          ViewerMenuItem(
            key: Key('keyboard-${mode.name}'),
            label: label,
            detail: detail,
            checked: keyboard.mode == mode,
            onSelected: () => choose(keyboard.copyWith(mode: mode)),
          ),
      ],
    ),
    if (RemoteInput.appleViewer != input.appleHost)
      ViewerMenuItem(
        key: const Key('keyboard-swap'),
        label: RemoteInput.appleViewer
            ? l10n.keyboardSwapCmd
            : l10n.keyboardSwapCtrl,
        checked: input.swapsModifiers,
        onSelected: () =>
            choose(keyboard.copyWith(swap: !input.swapsModifiers)),
      ),
  ];
}

/// The chords go as one sequence each; they neither apply nor consume a
/// latch. Each is the host's own, named as its keyboard names it, and one
/// the host has none for is left out. Ctrl+Alt+Del is shown on Windows and
/// never enabled: it cannot be injected. The keyboard's own choices lead.
List<ViewerMenuEntry> shortcutsMenu(AppLocalizations l10n, ViewerActions a) {
  final ready = a.input.ready.value;
  final apple = a.input.appleHost;
  final lockable = a.desktop.session.value.capabilities.contains('lock');
  final cad = RemoteShortcut.ctrlAltDel.keys(appleHost: apple);
  return [
    ..._keyboardItems(l10n, a),
    const ViewerMenuDivider(),
    for (final (s, key, label) in [
      (
        RemoteShortcut.showDesktop,
        'shortcut-show-desktop',
        l10n.shortcutShowDesktop,
      ),
      (
        RemoteShortcut.switchWindow,
        'shortcut-switch-window',
        apple ? l10n.shortcutSwitchApp : l10n.shortcutSwitchWindow,
      ),
      (
        RemoteShortcut.taskManager,
        'shortcut-task-manager',
        apple ? l10n.shortcutForceQuit : l10n.shortcutTaskManager,
      ),
      (
        RemoteShortcut.startMenu,
        'shortcut-start',
        apple ? l10n.shortcutSpotlight : l10n.shortcutStart,
      ),
      (
        RemoteShortcut.switchInputMethod,
        'shortcut-input-method',
        l10n.shortcutSwitchInputMethod,
      ),
      (RemoteShortcut.lock, 'shortcut-lock', l10n.shortcutLock),
    ])
      if (s.keys(appleHost: apple) case final keys when keys.isNotEmpty)
        ViewerMenuItem(
          key: Key(key),
          label: label,
          hint: hostChordName(keys, appleHost: apple),
          enabled: ready && (s != RemoteShortcut.lock || lockable),
          disabledHint: s == RemoteShortcut.lock && !lockable
              ? l10n.shortcutUnsupported
              : null,
          onSelected: () => a.input.shortcut(s),
        ),
    if (cad.isNotEmpty) ...[
      const ViewerMenuDivider(),
      ViewerMenuItem(
        key: const Key('shortcut-cad'),
        label: hostChordName(cad),
        enabled: false,
        disabledHint: l10n.shortcutUnsupported,
        onSelected: () {},
      ),
    ],
    const ViewerMenuDivider(),
    ..._refreshAndRelease(l10n, a),
  ];
}

List<ViewerMenuEntry> _refreshAndRelease(
  AppLocalizations l10n,
  ViewerActions a,
) => [
  ViewerMenuItem(
    key: const Key('shortcut-refresh'),
    label: l10n.shortcutRefresh,
    onSelected: () => a.input.shortcut(RemoteShortcut.refresh),
  ),
  ViewerMenuItem(
    key: const Key('shortcut-release'),
    label: l10n.shortcutReleaseKeys,
    onSelected: () => a.input.shortcut(RemoteShortcut.releaseKeys),
  ),
];

/// The desktop's More menu, or the touch viewer's, which also carries what
/// a phone has no toolbar for.
List<ViewerMenuEntry> moreMenu(
  AppLocalizations l10n,
  ViewerActions a, {
  required bool touch,
}) {
  if (!touch) {
    return [
      ViewerMenuItem(
        key: const Key('viewer-type-text'),
        label: l10n.viewerTypeText,
        icon: AppIcons.keyboard,
        enabled: a.input.ready.value,
        onSelected: a.onTypeText,
      ),
      ViewerMenuItem(
        key: const Key('viewer-more-quality'),
        label: l10n.viewerQuality,
        checked: a.prefs.qualityPanel,
        onSelected: () => a.prefs.setQualityPanel(!a.prefs.qualityPanel),
      ),
      if (a.onBack case final back?)
        ViewerMenuItem(
          key: const Key('viewer-more-back'),
          label: l10n.viewerBackToList,
          icon: AppIcons.back,
          onSelected: back,
        ),
    ];
  }
  final session = a.desktop.session.value;
  return [
    if (a.onGestureSheet case final gestures?)
      ViewerMenuItem(
        key: const Key('more-gestures'),
        label: l10n.gestureTitle,
        icon: AppIcons.trackpad,
        onSelected: gestures,
      ),
    ViewerMenuSection(l10n.viewerQualityLabel),
    ..._qualityItems(l10n, a),
    ..._sessionChoices(l10n, a),
    const ViewerMenuDivider(),
    ..._figures(l10n, a, touch: true),
    const ViewerMenuDivider(),
    for (final (control, label) in [
      (false, l10n.viewerModeView),
      (true, l10n.viewerModeControl),
    ])
      ViewerMenuItem(
        key: Key('more-mode-${control ? 'control' : 'view'}'),
        label: label,
        checked: session.controlWanted == control,
        enabled: !control || session.controlAllowed,
        onSelected: () => a.desktop.control(control),
      ),
    ViewerMenuItem(
      key: const Key('more-virtual-mouse'),
      label: l10n.virtualMouse,
      checked: a.prefs.virtualMouse,
      enabled: a.prefs.touchMode == TouchMode.direct,
      onSelected: () => a.prefs.setVirtualMouse(!a.prefs.virtualMouse),
    ),
    ..._refreshAndRelease(l10n, a),
    const ViewerMenuDivider(),
    ViewerMenuItem(
      key: const Key('more-disconnect'),
      label: l10n.viewerDisconnect,
      icon: AppIcons.disconnect,
      danger: true,
      onSelected: a.onDisconnect,
    ),
  ];
}

/// A toolbar button that opens a viewer menu. While the menu is open it
/// holds the toolbar out; when it closes the picture takes the keyboard
/// back (§10.8). The first item takes focus, so the picture lets go of
/// held keys and Esc closes the menu rather than reaching the remote.
class ViewerMenuButton extends StatelessWidget {
  const ViewerMenuButton({
    super.key,
    required this.buttonKey,
    required this.icon,
    this.label,
    this.tooltip,
    required this.entries,
    required this.toolbar,
    required this.pictureFocus,
    this.width = 300,
  });

  final Key buttonKey;
  final IconData icon;

  /// Null: an icon-only button.
  final String? label;
  final String? tooltip;
  final List<ViewerMenuEntry> entries;
  final ToolbarController toolbar;
  final FocusNode pictureFocus;
  final double width;

  @override
  Widget build(BuildContext context) {
    final first = entries.indexWhere((e) => e is ViewerMenuItem && e.enabled);
    return MenuAnchor(
      style: ViewerPalette.menuStyle,
      alignmentOffset: const Offset(0, 14),
      onOpen: toolbar.hold,
      onClose: () {
        toolbar.unhold();
        pictureFocus.requestFocus();
      },
      menuChildren: [
        for (var i = 0; i < entries.length; i++)
          viewerMenuChild(entries[i], width: width - 12, autofocus: i == first),
      ],
      builder: (context, controller, _) => ToolbarButton(
        key: buttonKey,
        icon: icon,
        label: label,
        tooltip: tooltip,
        chevron: label != null,
        selected: controller.isOpen,
        onPressed: () =>
            controller.isOpen ? controller.close() : controller.open(),
      ),
    );
  }
}

const _hover = Color.fromRGBO(255, 255, 255, 0.08);
const _pressed = Color.fromRGBO(255, 255, 255, 0.12);

/// One entry drawn in a desktop menu, [width] wide.
Widget viewerMenuChild(
  ViewerMenuEntry entry, {
  required double width,
  bool autofocus = false,
}) => switch (entry) {
  ViewerMenuSection(:final label) => SizedBox(
    width: width,
    child: Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: Text(
        label,
        style: const TextStyle(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          color: ViewerPalette.textQuiet,
        ),
      ),
    ),
  ),
  ViewerMenuDivider() => Container(
    width: width - 16,
    height: 1,
    margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
    color: ViewerPalette.edge,
  ),
  final ViewerMenuItem item => SizedBox(
    width: width,
    child: _MenuItem(item: item, autofocus: autofocus),
  ),
  final ViewerMenuSubmenu submenu => SizedBox(
    width: width,
    child: _Submenu(submenu: submenu, width: width + 12),
  ),
};

/// A menu row on the graphite: 36 tall, r9, white fills for hover and
/// press, in [ink].
ButtonStyle _rowStyle(TextStyle base, Color ink) => ButtonStyle(
  minimumSize: const WidgetStatePropertyAll(Size(0, 36)),
  padding: const WidgetStatePropertyAll(
    EdgeInsets.symmetric(horizontal: 12, vertical: 6),
  ),
  shape: const WidgetStatePropertyAll(
    RoundedRectangleBorder(borderRadius: BorderRadius.all(Radius.circular(9))),
  ),
  backgroundColor: WidgetStateProperty.resolveWith(
    (states) => states.contains(WidgetState.pressed)
        ? _pressed
        : states.contains(WidgetState.hovered) ||
              states.contains(WidgetState.focused)
        ? _hover
        : Colors.transparent,
  ),
  overlayColor: const WidgetStatePropertyAll(Colors.transparent),
  foregroundColor: WidgetStatePropertyAll(ink),
  iconColor: WidgetStatePropertyAll(ink),
  textStyle: WidgetStatePropertyAll(base.copyWith(fontSize: 13.5)),
);

/// A submenu row: its label, then the current choice and a chevron; the
/// choices open beside it, in a menu as wide as this one.
class _Submenu extends StatelessWidget {
  const _Submenu({required this.submenu, required this.width});

  final ViewerMenuSubmenu submenu;
  final double width;

  @override
  Widget build(BuildContext context) {
    final base = Theme.of(context).textTheme.bodyMedium ?? const TextStyle();
    final note = submenu.note;
    return SubmenuButton(
      key: submenu.key,
      menuStyle: ViewerPalette.menuStyle,
      style: _rowStyle(base, ViewerPalette.text),
      menuChildren: [
        if (note != null)
          viewerMenuChild(ViewerMenuSection(note), width: width - 12),
        for (final entry in submenu.entries)
          viewerMenuChild(entry, width: width - 12),
      ],
      trailingIcon: Text(
        submenu.value,
        style: base.copyWith(fontSize: 12, color: ViewerPalette.textSecondary),
      ),
      submenuIcon: const WidgetStatePropertyAll(
        Icon(AppIcons.chevronRight, size: AppIconSize.inline),
      ),
      child: Text(submenu.label),
    );
  }
}

class _MenuItem extends StatelessWidget {
  const _MenuItem({required this.item, required this.autofocus});

  final ViewerMenuItem item;
  final bool autofocus;

  @override
  Widget build(BuildContext context) {
    final ink = !item.enabled
        ? ViewerPalette.textQuiet
        : item.danger
        ? ViewerPalette.danger
        : ViewerPalette.text;
    // A button's text style replaces the ambient one: start from the
    // theme's, which carries the typeface and its CJK fallback.
    final base = Theme.of(context).textTheme.bodyMedium ?? const TextStyle();
    final checked = item.checked;
    final trailing = item.enabled
        ? item.hint
        : (item.disabledHint ?? item.hint);
    final Widget? leading = checked != null
        ? SizedBox.square(
            dimension: AppIconSize.inline,
            child: checked
                ? Icon(AppIcons.check, size: AppIconSize.inline, color: ink)
                : null,
          )
        : item.icon == null
        ? null
        : Icon(item.icon, size: AppIconSize.inline, color: ink);
    return MenuItemButton(
      key: item.key,
      autofocus: autofocus,
      onPressed: item.enabled ? () => item.onSelected() : null,
      leadingIcon: leading,
      trailingIcon: trailing == null
          ? null
          : Padding(
              padding: const EdgeInsetsDirectional.only(start: 16),
              child: Text(
                trailing,
                style: item.enabled && item.hint != null
                    ? mono(fontSize: 12, color: ViewerPalette.textSecondary)
                    : base.copyWith(
                        fontSize: 12,
                        color: ViewerPalette.textQuiet,
                      ),
              ),
            ),
      style: _rowStyle(base, ink),
      child: switch (item.detail) {
        null => Text(item.label),
        final detail => Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(item.label),
            Text(
              detail,
              style: base.copyWith(
                fontSize: 12,
                color: ViewerPalette.textSecondary,
              ),
            ),
          ],
        ),
      },
    );
  }
}

/// Text to type on the remote computer, at most [kTextMaxBytes] of UTF-8.
/// The caller sends it ([RemoteInput.typeText]) and gives the picture its
/// focus back.
class TypeTextDialog extends StatefulWidget {
  const TypeTextDialog({super.key});

  /// The text to send, or null when cancelled.
  static Future<String?> show(BuildContext context) => showDialog<String>(
    context: context,
    builder: (context) => const TypeTextDialog(),
  );

  @override
  State<TypeTextDialog> createState() => _TypeTextDialogState();
}

class _TypeTextDialogState extends State<TypeTextDialog> {
  final _field = TextEditingController();

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  void _send() => Navigator.pop(context, _field.text);

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return AlertDialog(
      title: Text(l10n.viewerTypeText),
      content: SizedBox(
        width: 420,
        child: TextField(
          controller: _field,
          autofocus: true,
          minLines: 3,
          maxLines: 6,
          inputFormatters: const [_Utf8Limit(kTextMaxBytes)],
          style: const TextStyle(color: ViewerPalette.text),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(l10n.cancel),
        ),
        FilledButton(onPressed: _send, child: Text(l10n.viewerSendText)),
      ],
    );
  }
}

/// Cuts what does not fit in [maxBytes] of UTF-8, at a code point.
class _Utf8Limit extends TextInputFormatter {
  const _Utf8Limit(this.maxBytes);

  final int maxBytes;

  @override
  TextEditingValue formatEditUpdate(
    TextEditingValue oldValue,
    TextEditingValue newValue,
  ) {
    if (utf8.encode(newValue.text).length <= maxBytes) return newValue;
    final kept = StringBuffer();
    var bytes = 0;
    for (final rune in newValue.text.runes) {
      final length = utf8.encode(String.fromCharCode(rune)).length;
      if (bytes + length > maxBytes) break;
      bytes += length;
      kept.writeCharCode(rune);
    }
    final text = kept.toString();
    return TextEditingValue(
      text: text,
      selection: TextSelection.collapsed(offset: text.length),
    );
  }
}
