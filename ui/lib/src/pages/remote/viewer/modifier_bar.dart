import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_input.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import 'modifier_latch.dart';
import 'remote_input.dart';
import 'viewer_overlays.dart';

/// The bar's height, directly above the soft keyboard.
const kModifierBarHeight = 44.0;

/// The ink of an armed or locked cap, on its light fill.
const _activeInk = Color(0xFF1D1D1F);

/// The modifier caps' faces, the same in every language.
String modifierCap(RemoteKey modifier) => switch (modifier) {
  RemoteKey.control => 'Ctrl',
  RemoteKey.alt => 'Alt',
  RemoteKey.shift => 'Shift',
  RemoteKey.meta => 'Win',
  _ => modifier.name,
};

/// What a soft keyboard lacks (ui §9.4): latched modifiers, fixed at the
/// leading edge; Esc, Tab, Fn, arrows, navigation and editing keys in a
/// strip that scrolls; and the key that hides the keyboard, fixed at the
/// trailing edge. With Fn on, the strip offers F1 to F12 instead.
class ModifierBar extends StatefulWidget {
  const ModifierBar({
    super.key = const Key('modifier-bar'),
    required this.input,
    required this.latch,
    required this.onCollapse,
    this.padding = EdgeInsets.zero,
  });

  final RemoteInput input;
  final ModifierLatch latch;
  final VoidCallback onCollapse;

  /// Kept clear at the sides: display cutouts.
  final EdgeInsets padding;

  @override
  State<ModifierBar> createState() => _ModifierBarState();
}

class _ModifierBarState extends State<ModifierBar> {
  bool _fn = false;

  RemoteInput get _input => widget.input;

  void _latch(RemoteKey modifier) {
    widget.latch.tap(modifier);
    unawaited(HapticFeedback.selectionClick());
  }

  KeyCap _tap(String label, RemoteKey key, {double minWidth = 0}) =>
      KeyCap(label: label, minWidth: minWidth, onTap: () => _input.tapKey(key));

  /// A tap sends one key on release, so a drag still scrolls the strip;
  /// held for [kKeyRepeatDelay], the key goes down and repeats every
  /// [kKeyRepeatInterval] until let go.
  KeyCap _repeat(
    String label,
    RemoteKey key, {
    double? width,
    String? semanticLabel,
  }) => KeyCap(
    label: label,
    width: width,
    semanticLabel: semanticLabel,
    onTap: () => _input.tapKey(key),
    onHold: () => _input.pressKey(key, delay: kKeyRepeatInterval),
    onRelease: _input.releaseKey,
  );

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final keys = [
      _tap('Esc', RemoteKey.escape, minWidth: 40),
      _tap('Tab', RemoteKey.tab, minWidth: 40),
      KeyCap(
        key: const Key('modifier-fn'),
        label: 'Fn',
        minWidth: 40,
        toggled: _fn,
        onTap: () => setState(() => _fn = !_fn),
      ),
    ];
    final groups = _fn
        ? [
            keys,
            [
              for (var i = 1; i <= 12; i++)
                _tap('F$i', RemoteKey.values.byName('f$i')),
            ],
          ]
        : [
            keys,
            [
              for (final (label, key, dir) in const [
                ('←', RemoteKey.left, 'left'),
                ('↑', RemoteKey.up, 'up'),
                ('↓', RemoteKey.down, 'down'),
                ('→', RemoteKey.right, 'right'),
              ])
                _repeat(
                  label,
                  key,
                  width: 36,
                  semanticLabel: l10n.keyArrow(dir),
                ),
            ],
            [
              _repeat('Home', RemoteKey.home),
              _repeat('End', RemoteKey.end),
              _repeat('PgUp', RemoteKey.pageUp),
              _repeat('PgDn', RemoteKey.pageDown),
            ],
            [
              _tap('Ins', RemoteKey.insert),
              _tap('Del', RemoteKey.delete),
              KeyCap(
                label: 'Ctrl+C',
                onTap: () => _input.tapKey(
                  RemoteKey.c,
                  extra: const {RemoteKey.control},
                ),
              ),
              KeyCap(
                label: 'Ctrl+V',
                onTap: () => _input.tapKey(
                  RemoteKey.v,
                  extra: const {RemoteKey.control},
                ),
              ),
            ],
          ];
    return Semantics(
      container: true,
      explicitChildNodes: true,
      label: l10n.touchModifierBar,
      child: Container(
        height: kModifierBarHeight,
        padding: EdgeInsets.only(
          left: 8 + widget.padding.left,
          right: 8 + widget.padding.right,
        ),
        decoration: const BoxDecoration(
          color: ViewerPalette.card,
          border: Border(top: BorderSide(color: ViewerPalette.edge)),
        ),
        child: Row(
          children: [
            ListenableBuilder(
              listenable: widget.latch,
              builder: (context, _) => Row(
                children: _spaced([
                  for (final m in ModifierLatch.order)
                    KeyCap(
                      key: Key('modifier-${m.name}'),
                      label: modifierCap(m),
                      minWidth: m == RemoteKey.control ? 46 : 42,
                      latch: widget.latch.stateOf(m),
                      onTap: () => _latch(m),
                    ),
                ]),
              ),
            ),
            const _Separator(),
            Expanded(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: Row(
                  children: [
                    for (var i = 0; i < groups.length; i++) ...[
                      if (i > 0) const _Separator(),
                      ..._spaced(groups[i]),
                    ],
                  ],
                ),
              ),
            ),
            const SizedBox(width: 6),
            Semantics(
              button: true,
              label: l10n.touchHideKeyboard,
              child: InkWell(
                key: const Key('modifier-collapse'),
                onTap: widget.onCollapse,
                borderRadius: const BorderRadius.all(Radius.circular(8)),
                child: const SizedBox(
                  width: 40,
                  height: 32,
                  child: Icon(
                    AppIcons.chevronDown,
                    size: AppIconSize.button,
                    color: ViewerPalette.text,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  static List<Widget> _spaced(List<Widget> caps) => [
    for (var i = 0; i < caps.length; i++) ...[
      if (i > 0) const SizedBox(width: 6),
      caps[i],
    ],
  ];
}

class _Separator extends StatelessWidget {
  const _Separator();

  @override
  Widget build(BuildContext context) => Container(
    width: 1,
    height: 20,
    margin: const EdgeInsets.symmetric(horizontal: 8),
    color: ViewerPalette.keySeparator,
  );
}

/// One key of the modifier bar, in mono. A modifier shows its latch: armed
/// is light, locked is light with a blue bar. [onHold] makes it repeat:
/// a tap is [onTap], a hold of [kKeyRepeatDelay] is [onHold] until
/// [onRelease], and a horizontal drag scrolls the strip instead.
class KeyCap extends StatefulWidget {
  const KeyCap({
    super.key,
    required this.label,
    this.semanticLabel,
    this.width,
    this.minWidth = 0,
    this.latch,
    this.toggled,
    this.onTap,
    this.onHold,
    this.onRelease,
  });

  final String label;
  final String? semanticLabel;

  /// Fixed; null sizes it to [label].
  final double? width;
  final double minWidth;

  /// A modifier's state; null for other keys.
  final LatchState? latch;

  /// A toggle's state (Fn); null for other keys.
  final bool? toggled;
  final VoidCallback? onTap;
  final VoidCallback? onHold;
  final VoidCallback? onRelease;

  @override
  State<KeyCap> createState() => _KeyCapState();
}

class _KeyCapState extends State<KeyCap> {
  bool _pressed = false, _holding = false;

  @override
  void dispose() {
    _release();
    super.dispose();
  }

  void _press(bool pressed) {
    if (pressed != _pressed && mounted) setState(() => _pressed = pressed);
  }

  void _hold() {
    _holding = true;
    _press(true);
    widget.onHold?.call();
  }

  void _release() {
    if (!_holding) return;
    _holding = false;
    widget.onRelease?.call();
    _press(false);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final latch = widget.latch ?? LatchState.off;
    final active = latch != LatchState.off || widget.toggled == true;
    final fill = active
        ? ViewerPalette.text
        : _pressed
        ? ViewerPalette.fillStrong
        : ViewerPalette.fill;
    final cap = Container(
      height: 32,
      width: widget.width,
      constraints: BoxConstraints(minWidth: widget.minWidth),
      padding: widget.width == null
          ? const EdgeInsets.symmetric(horizontal: 10)
          : null,
      decoration: BoxDecoration(
        color: fill,
        borderRadius: const BorderRadius.all(Radius.circular(8)),
      ),
      child: Stack(
        alignment: Alignment.center,
        children: [
          Text(
            widget.label,
            style: mono(
              fontSize: 13,
              fontWeight: active ? FontWeight.w600 : FontWeight.w400,
              color: active ? _activeInk : ViewerPalette.text,
            ),
          ),
          if (latch == LatchState.locked)
            const Positioned(
              left: 0,
              right: 0,
              bottom: 3,
              child: Center(
                child: SizedBox(
                  width: 18,
                  height: 2,
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      color: ViewerPalette.signal,
                      borderRadius: BorderRadius.all(Radius.circular(1)),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
    final toggle = widget.latch != null || widget.toggled != null;
    return Semantics(
      button: true,
      label: widget.semanticLabel ?? widget.label,
      toggled: toggle ? active : null,
      value: switch (latch) {
        LatchState.armed => l10n.modifierStateArmed,
        LatchState.locked => l10n.modifierStateLocked,
        LatchState.off => null,
      },
      onTap: widget.onTap,
      excludeSemantics: true,
      child: RawGestureDetector(
        behavior: HitTestBehavior.opaque,
        gestures: {
          TapGestureRecognizer:
              GestureRecognizerFactoryWithHandlers<TapGestureRecognizer>(
                () => TapGestureRecognizer(debugOwner: this),
                (r) => r
                  ..onTapDown = ((_) => _press(true))
                  ..onTapUp = ((_) => _press(false))
                  ..onTapCancel = (() {
                    if (!_holding) _press(false);
                  })
                  ..onTap = widget.onTap,
              ),
          if (widget.onHold != null)
            LongPressGestureRecognizer:
                GestureRecognizerFactoryWithHandlers<
                  LongPressGestureRecognizer
                >(
                  () => LongPressGestureRecognizer(
                    duration: kKeyRepeatDelay,
                    debugOwner: this,
                  ),
                  (r) => r
                    ..onLongPressStart = ((_) => _hold())
                    ..onLongPressEnd = ((_) => _release())
                    ..onLongPressCancel = _release,
                ),
        },
        child: cap,
      ),
    );
  }
}

/// Which modifiers apply to the next action, or stay on (ui §9.4).
class ModifierPill extends StatelessWidget {
  const ModifierPill({
    super.key = const Key('modifier-pill'),
    required this.latch,
  });

  final ModifierLatch latch;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: latch,
    builder: (context, _) {
      if (latch.isEmpty) return const SizedBox.shrink();
      final l10n = context.l10n;
      String names(LatchState state) => [
        for (final m in ModifierLatch.order)
          if (latch.stateOf(m) == state) modifierCap(m),
      ].join(' + ');
      final locked = names(LatchState.locked), armed = names(LatchState.armed);
      return IgnorePointer(
        child: ViewerPill(
          height: 30,
          leading: Container(
            width: 7,
            height: 7,
            decoration: const BoxDecoration(
              color: ViewerPalette.signal,
              shape: BoxShape.circle,
            ),
          ),
          text: [
            if (locked.isNotEmpty) l10n.modifierLocked(locked),
            if (armed.isNotEmpty) l10n.modifierArmed(armed),
          ].join(' · '),
        ),
      );
    },
  );
}
