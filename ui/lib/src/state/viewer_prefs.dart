import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// How a touch screen drives the remote pointer: as a trackpad that moves
/// it, or by touching where to click.
enum TouchMode { trackpad, direct }

/// How the remote picture fits the viewer: scaled to the window, or one
/// remote pixel to one device pixel.
enum ViewerScaleMode { fit, original }

/// The viewer's own choices, apart from `AppPrefs` so that changing one never
/// rebuilds the whole app. Remembered across launches; a failed read or write
/// only costs the choice.
///
/// The picture's quality is not here: it belongs to each remembered computer.
class ViewerPrefs extends ChangeNotifier {
  ViewerPrefs({this.persist = true});

  /// Off in tests, which have no preference store.
  final bool persist;

  static const _toolbarPinnedKey = 'viewer_toolbar_pinned';
  static const _qualityPanelKey = 'viewer_quality_panel';
  static const _scaleModeKey = 'viewer_scale_mode';
  static const _touchModeKey = 'viewer_touch_mode';
  static const _virtualMouseKey = 'viewer_virtual_mouse';
  static const _gestureSheetSeenKey = 'viewer_gestures_seen';

  /// Whether the desktop toolbar stays out, rather than folding into a
  /// handle once the pointer leaves it.
  bool get toolbarPinned => _toolbarPinned;
  bool _toolbarPinned = true;

  /// Whether the connection-quality panel is open.
  bool get qualityPanel => _qualityPanel;
  bool _qualityPanel = false;

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

  Future<void> load() async {
    if (!persist) return;
    try {
      final prefs = await SharedPreferences.getInstance();
      _toolbarPinned = prefs.getBool(_toolbarPinnedKey) ?? true;
      _qualityPanel = prefs.getBool(_qualityPanelKey) ?? false;
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
    } catch (_) {}
    notifyListeners();
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
