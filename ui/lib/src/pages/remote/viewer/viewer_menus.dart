import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../../l10n/app_localizations.dart';
import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
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
    required this.onTypeText,
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
  final VoidCallback onTypeText;

  /// Back to the list, the session kept; null where there is no list.
  final VoidCallback? onBack;
  final VoidCallback? onGestureSheet;
  final VoidCallback? onReconnect;

  /// Null: the action is hidden.
  final VoidCallback? onPasteNewInvite;
}

/// Scaling (local only, P1-4: the stage applies it to the viewport), the
/// picture preference (live, D12), and the quality panel.
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
  const ViewerMenuDivider(),
  ViewerMenuItem(
    key: const Key('display-quality-panel'),
    label: l10n.viewerQuality,
    checked: a.prefs.qualityPanel,
    onSelected: () => a.prefs.setQualityPanel(!a.prefs.qualityPanel),
  ),
];

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

/// The chords go as one sequence each; they neither apply nor consume a
/// latch. Ctrl+Alt+Del is shown and never enabled: it cannot be injected.
List<ViewerMenuEntry> shortcutsMenu(AppLocalizations l10n, ViewerActions a) {
  final ready = a.input.ready.value;
  final lockable = a.desktop.session.value.capabilities.contains('lock');
  ViewerMenuItem chord(
    RemoteShortcut s,
    String key,
    String label,
    String hint, {
    bool enabled = true,
    String? disabledHint,
  }) => ViewerMenuItem(
    key: Key(key),
    label: label,
    hint: hint,
    enabled: ready && enabled,
    disabledHint: disabledHint,
    onSelected: () => a.input.shortcut(s),
  );
  return [
    chord(
      RemoteShortcut.showDesktop,
      'shortcut-show-desktop',
      l10n.shortcutShowDesktop,
      'Win + D',
    ),
    chord(
      RemoteShortcut.switchWindow,
      'shortcut-switch-window',
      l10n.shortcutSwitchWindow,
      'Alt + Tab',
    ),
    chord(
      RemoteShortcut.taskManager,
      'shortcut-task-manager',
      l10n.shortcutTaskManager,
      'Ctrl + Shift + Esc',
    ),
    chord(
      RemoteShortcut.startMenu,
      'shortcut-start',
      l10n.shortcutStart,
      'Win',
    ),
    chord(
      RemoteShortcut.lock,
      'shortcut-lock',
      l10n.shortcutLock,
      'Win + L',
      enabled: lockable,
      disabledHint: lockable ? null : l10n.shortcutUnsupported,
    ),
    const ViewerMenuDivider(),
    ViewerMenuItem(
      key: const Key('shortcut-cad'),
      label: 'Ctrl + Alt + Del',
      enabled: false,
      disabledHint: l10n.shortcutUnsupported,
      onSelected: () {},
    ),
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
  final panel = ViewerMenuItem(
    key: Key(touch ? 'more-quality-panel' : 'viewer-more-quality'),
    label: l10n.viewerQuality,
    checked: a.prefs.qualityPanel,
    onSelected: () => a.prefs.setQualityPanel(!a.prefs.qualityPanel),
  );
  if (!touch) {
    return [
      ViewerMenuItem(
        key: const Key('viewer-type-text'),
        label: l10n.viewerTypeText,
        icon: AppIcons.keyboard,
        enabled: a.input.ready.value,
        onSelected: a.onTypeText,
      ),
      panel,
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
    panel,
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
};

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
      style: ButtonStyle(
        minimumSize: const WidgetStatePropertyAll(Size(0, 36)),
        padding: const WidgetStatePropertyAll(
          EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        ),
        shape: const WidgetStatePropertyAll(
          RoundedRectangleBorder(
            borderRadius: BorderRadius.all(Radius.circular(9)),
          ),
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
      ),
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
