import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'desktop_models.dart';

/// How a touch screen drives the remote pointer: as a trackpad that moves
/// it, or by touching where to click.
enum TouchMode { trackpad, direct }

/// How the remote picture fits the viewer: scaled to the window, or one
/// remote pixel to one device pixel.
enum ViewerScaleMode { fit, original }

/// Whose input method composes what is typed on a computer.
enum KeyboardMode {
  /// This device's: the host gets the text it commits.
  local,

  /// The host's: keys go by their place on the keyboard, and the host's
  /// layout and input method type them.
  remote,
}

/// How this device's keyboard reaches one computer.
@immutable
class KeyboardSettings {
  const KeyboardSettings({this.mode = KeyboardMode.local, this.swap});

  factory KeyboardSettings.fromJson(Object? json) {
    if (json is! Map) return const KeyboardSettings();
    final swap = json['swap'];
    return KeyboardSettings(
      mode: KeyboardMode.values.asNameMap()[json['mode']] ?? KeyboardMode.local,
      swap: swap is bool ? swap : null,
    );
  }

  final KeyboardMode mode;

  /// Ctrl and Cmd (the Windows key) trade places on their way to the host.
  /// Null: the default, on when exactly one of the two computers is an
  /// Apple one, so that Cmd+C on a Mac is Ctrl+C on Windows and back.
  final bool? swap;

  bool swaps({required bool appleViewer, required bool appleHost}) =>
      swap ?? appleViewer != appleHost;

  KeyboardSettings copyWith({KeyboardMode? mode, bool? swap}) =>
      KeyboardSettings(mode: mode ?? this.mode, swap: swap ?? this.swap);

  Map<String, Object?> toJson() => {'mode': mode.name, 'swap': ?swap};

  @override
  bool operator ==(Object other) =>
      other is KeyboardSettings && other.mode == mode && other.swap == swap;
  @override
  int get hashCode => Object.hash(mode, swap);
}

/// The viewer's own choices, apart from `AppPrefs` so that changing one never
/// rebuilds the whole app. Remembered across launches; a failed read or write
/// only costs the choice.
///
/// The picture's quality, resolution and maximum frame rate are not here:
/// they belong to each remembered computer. The codec is: whether a picture
/// can be H.265 is this device's affair. So is the keyboard, which is chosen
/// per computer but kept here, by the computer's id: it is how this device's
/// keys and input method reach that computer.
class ViewerPrefs extends ChangeNotifier {
  ViewerPrefs({this.persist = true});

  /// Off in tests, which have no preference store.
  final bool persist;

  static const _toolbarPinnedKey = 'viewer_toolbar_pinned';
  static const _qualityPanelKey = 'viewer_quality_panel';
  static const _qualityMonitorKey = 'viewer_quality_monitor';
  static const _codecKey = 'viewer_codec';
  static const _scaleModeKey = 'viewer_scale_mode';
  static const _touchModeKey = 'viewer_touch_mode';
  static const _virtualMouseKey = 'viewer_virtual_mouse';
  static const _gestureSheetSeenKey = 'viewer_gestures_seen';
  static const _keyboardKey = 'viewer_keyboard';

  /// Computers whose keyboard is kept, the most recently set: more than
  /// this device remembers.
  static const keyboardsKept = 64;

  /// Whether the desktop toolbar stays out, rather than folding into a
  /// handle once the pointer leaves it.
  bool get toolbarPinned => _toolbarPinned;
  bool _toolbarPinned = true;

  /// Whether the connection-quality panel is open.
  bool get qualityPanel => _qualityPanel;
  bool _qualityPanel = false;

  /// Whether the quality monitor, RustDesk's small figures over the
  /// picture, shows (D11).
  bool get qualityMonitor => _qualityMonitor;
  bool _qualityMonitor = false;

  /// What a session offers to decode (D6); it takes it at connect.
  DesktopCodecPreference get codec => _codec;
  DesktopCodecPreference _codec = DesktopCodecPreference.auto;

  ViewerScaleMode get scaleMode => _scaleMode;
  ViewerScaleMode _scaleMode = ViewerScaleMode.fit;

  TouchMode get touchMode => _touchMode;
  TouchMode _touchMode = TouchMode.trackpad;

  /// Whether direct touch shows the on-screen mouse buttons and wheel.
  bool get virtualMouse => _virtualMouse;
  bool _virtualMouse = false;

  /// Whether the gesture sheet has been shown once on its own.
  bool get gestureSheetSeen => _gestureSheetSeen;
  bool _gestureSheetSeen = false;

  /// By remembered computer id, oldest first. A computer this device does
  /// not remember keeps its choice under '' until the app closes.
  final _keyboards = <String, KeyboardSettings>{};

  /// How the keyboard reaches computer [computerId].
  KeyboardSettings keyboardFor(String? computerId) =>
      _keyboards[computerId ?? ''] ?? const KeyboardSettings();

  Future<void> load() async {
    if (!persist) return;
    try {
      final prefs = await SharedPreferences.getInstance();
      _toolbarPinned = prefs.getBool(_toolbarPinnedKey) ?? true;
      _qualityPanel = prefs.getBool(_qualityPanelKey) ?? false;
      _qualityMonitor = prefs.getBool(_qualityMonitorKey) ?? false;
      _codec = _named(
        DesktopCodecPreference.values,
        prefs.getString(_codecKey),
        DesktopCodecPreference.auto,
      );
      _scaleMode = _named(
        ViewerScaleMode.values,
        prefs.getString(_scaleModeKey),
        ViewerScaleMode.fit,
      );
      _touchMode = _named(
        TouchMode.values,
        prefs.getString(_touchModeKey),
        TouchMode.trackpad,
      );
      _virtualMouse = prefs.getBool(_virtualMouseKey) ?? false;
      _gestureSheetSeen = prefs.getBool(_gestureSheetSeenKey) ?? false;
      final keyboards = prefs.getString(_keyboardKey);
      if (keyboards != null) {
        final kept = jsonDecode(keyboards);
        if (kept is Map) {
          for (final MapEntry(:key, :value) in kept.entries) {
            if (key is String && key.isNotEmpty) {
              _keyboards[key] = KeyboardSettings.fromJson(value);
            }
          }
        }
      }
    } catch (_) {}
    notifyListeners();
  }

  /// Remembered for [computerId]; for a computer not remembered (null),
  /// only until the app closes.
  Future<void> setKeyboard(String? computerId, KeyboardSettings v) async {
    final id = computerId ?? '';
    if (v == keyboardFor(id)) return;
    _keyboards
      ..remove(id)
      ..[id] = v;
    while (_keyboards.length > keyboardsKept) {
      _keyboards.remove(_keyboards.keys.first);
    }
    await _changed(
      (prefs) => prefs.setString(
        _keyboardKey,
        jsonEncode({
          for (final MapEntry(:key, :value) in _keyboards.entries)
            if (key.isNotEmpty) key: value.toJson(),
        }),
      ),
    );
  }

  Future<void> setToolbarPinned(bool v) async {
    if (v == _toolbarPinned) return;
    _toolbarPinned = v;
    await _changed((prefs) => prefs.setBool(_toolbarPinnedKey, v));
  }

  Future<void> setQualityPanel(bool v) async {
    if (v == _qualityPanel) return;
    _qualityPanel = v;
    await _changed((prefs) => prefs.setBool(_qualityPanelKey, v));
  }

  Future<void> setQualityMonitor(bool v) async {
    if (v == _qualityMonitor) return;
    _qualityMonitor = v;
    await _changed((prefs) => prefs.setBool(_qualityMonitorKey, v));
  }

  Future<void> setCodec(DesktopCodecPreference v) async {
    if (v == _codec) return;
    _codec = v;
    await _changed((prefs) => prefs.setString(_codecKey, v.name));
  }

  Future<void> setScaleMode(ViewerScaleMode v) async {
    if (v == _scaleMode) return;
    _scaleMode = v;
    await _changed((prefs) => prefs.setString(_scaleModeKey, v.name));
  }

  Future<void> setTouchMode(TouchMode v) async {
    if (v == _touchMode) return;
    _touchMode = v;
    await _changed((prefs) => prefs.setString(_touchModeKey, v.name));
  }

  Future<void> setVirtualMouse(bool v) async {
    if (v == _virtualMouse) return;
    _virtualMouse = v;
    await _changed((prefs) => prefs.setBool(_virtualMouseKey, v));
  }

  Future<void> setGestureSheetSeen(bool v) async {
    if (v == _gestureSheetSeen) return;
    _gestureSheetSeen = v;
    await _changed((prefs) => prefs.setBool(_gestureSheetSeenKey, v));
  }

  /// Tell the viewer at once, then remember the choice.
  Future<void> _changed(
    Future<bool> Function(SharedPreferences prefs) write,
  ) async {
    notifyListeners();
    if (!persist) return;
    try {
      await write(await SharedPreferences.getInstance());
    } catch (_) {}
  }

  static T _named<T extends Enum>(List<T> values, String? name, T fallback) =>
      values.asNameMap()[name] ?? fallback;
}
