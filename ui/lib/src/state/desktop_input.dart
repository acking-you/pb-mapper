// What the viewer sends to the host. Dart never stamps a frame id, a
// sequence or a generation on it: Rust does, at the moment it accepts the
// batch (`pb_mapper_desktop_viewer_call_json` op `input`).
import 'dart:convert';

import 'package:flutter/foundation.dart';

enum RemoteButton { left, right, middle }

/// The host's key vocabulary (D17). Letters and digits reach the host as
/// virtual keys, so shortcuts work on any host layout; text goes as
/// [DesktopInput.text].
enum RemoteKey {
  shift,
  control,
  alt,
  meta,
  enter,
  escape,
  tab,
  backspace,
  delete,
  insert,
  space,
  left,
  right,
  up,
  down,
  home,
  end,
  pageUp,
  pageDown,
  a,
  b,
  c,
  d,
  e,
  f,
  g,
  h,
  i,
  j,
  k,
  l,
  m,
  n,
  o,
  p,
  q,
  r,
  s,
  t,
  u,
  v,
  w,
  x,
  y,
  z,
  digit0,
  digit1,
  digit2,
  digit3,
  digit4,
  digit5,
  digit6,
  digit7,
  digit8,
  digit9,
  f1,
  f2,
  f3,
  f4,
  f5,
  f6,
  f7,
  f8,
  f9,
  f10,
  f11,
  f12;

  /// The host's snake_case name.
  String get wire => switch (this) {
    pageUp => 'page_up',
    pageDown => 'page_down',
    _ => name,
  };
}

/// One input event, as the host's `Input` (tag `kind`).
@immutable
sealed class DesktopInput {
  const DesktopInput();

  const factory DesktopInput.move(int x, int y) = DesktopMove;
  const factory DesktopInput.button(
    RemoteButton button,
    bool down, {
    bool dragStart,
  }) = DesktopButton;

  /// [horizontal] and [vertical] are wheel notches, at most 100 each way.
  factory DesktopInput.wheel(int horizontal, int vertical) = DesktopWheel;
  const factory DesktopInput.key(RemoteKey key, bool down) = DesktopKey;

  /// At most [DesktopText.maxBytes] of UTF-8.
  factory DesktopInput.text(String text) = DesktopText;

  /// Lock the host (Win+L, D17); control-gated like any input.
  const factory DesktopInput.lock() = DesktopLock;

  Map<String, Object?> toJson();
}

final class DesktopMove extends DesktopInput {
  const DesktopMove(this.x, this.y);
  final int x, y;
  @override
  Map<String, Object?> toJson() => {'kind': 'move', 'x': x, 'y': y};
  @override
  bool operator ==(Object other) =>
      other is DesktopMove && other.x == x && other.y == y;
  @override
  int get hashCode => Object.hash(x, y);
}

final class DesktopButton extends DesktopInput {
  /// [dragStart] marks a press that starts a drag: the host holds it past
  /// the double-click time of a preceding click, so the two never merge
  /// into a double click (R-UI-3). It only ever delays.
  const DesktopButton(this.button, this.down, {this.dragStart = false});
  final RemoteButton button;
  final bool down, dragStart;
  @override
  Map<String, Object?> toJson() => {
    'kind': 'button',
    'button': button.name,
    'down': down,
    if (dragStart) 'drag_start': true,
  };
  @override
  bool operator ==(Object other) =>
      other is DesktopButton &&
      other.button == button &&
      other.down == down &&
      other.dragStart == dragStart;
  @override
  int get hashCode => Object.hash(button, down, dragStart);
}

final class DesktopWheel extends DesktopInput {
  DesktopWheel(int horizontal, int vertical)
    : horizontal = horizontal.clamp(-maxNotches, maxNotches),
      vertical = vertical.clamp(-maxNotches, maxNotches);
  static const maxNotches = 100;
  final int horizontal, vertical;
  @override
  Map<String, Object?> toJson() => {
    'kind': 'wheel',
    'horizontal': horizontal,
    'vertical': vertical,
  };
  @override
  bool operator ==(Object other) =>
      other is DesktopWheel &&
      other.horizontal == horizontal &&
      other.vertical == vertical;
  @override
  int get hashCode => Object.hash(horizontal, vertical);
}

/// A key going down or up on the host: [DesktopKey] or
/// [DesktopPhysicalKey].
sealed class DesktopKeyInput extends DesktopInput {
  const DesktopKeyInput();
  bool get down;

  /// The same key, released.
  DesktopKeyInput get up;
}

final class DesktopKey extends DesktopKeyInput {
  const DesktopKey(this.key, this.down);
  final RemoteKey key;
  @override
  final bool down;
  @override
  DesktopKey get up => DesktopKey(key, false);
  @override
  Map<String, Object?> toJson() => {
    'kind': 'key',
    'key': key.wire,
    'down': down,
  };
  @override
  bool operator ==(Object other) =>
      other is DesktopKey && other.key == key && other.down == down;
  @override
  int get hashCode => Object.hash(key, down);
}

/// A key by its place on the keyboard, only to a host that listed
/// `physical_keys`: [usage] is its USB HID usage on the keyboard page, and
/// the host's layout and input method decide what it types.
final class DesktopPhysicalKey extends DesktopKeyInput {
  const DesktopPhysicalKey(this.usage, this.down)
    : assert(usage >= minUsage && usage <= maxUsage);

  /// The keyboard page's keys: below are error codes, above reserved.
  static const minUsage = 0x04, maxUsage = 0xE7;
  final int usage;
  @override
  final bool down;
  @override
  DesktopPhysicalKey get up => DesktopPhysicalKey(usage, false);
  @override
  Map<String, Object?> toJson() => {
    'kind': 'physical_key',
    'usage': usage,
    'down': down,
  };
  @override
  bool operator ==(Object other) =>
      other is DesktopPhysicalKey && other.usage == usage && other.down == down;
  @override
  int get hashCode => Object.hash(usage, down);
}

final class DesktopText extends DesktopInput {
  DesktopText(this.text)
    : assert(text.isNotEmpty && utf8.encode(text).length <= maxBytes);
  static const maxBytes = 1024;
  final String text;
  @override
  Map<String, Object?> toJson() => {'kind': 'text', 'text': text};
  @override
  bool operator ==(Object other) => other is DesktopText && other.text == text;
  @override
  int get hashCode => text.hashCode;
}

final class DesktopLock extends DesktopInput {
  const DesktopLock();
  @override
  Map<String, Object?> toJson() => const {'kind': 'lock'};
  @override
  bool operator ==(Object other) => other is DesktopLock;
  @override
  int get hashCode => (DesktopLock).hashCode;
}
