import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// The native macOS drag-to-authorize card (`ui/macos/Runner/PermissionHelper.swift`):
/// shown below System Settings' Screen Recording or Accessibility pane, it
/// holds pb-mapper's icon so the owner can drag it straight into the privacy
/// list and flip the switch on without restarting the app.
///
/// Every call is best effort: when no native handler is registered (in tests
/// or on other platforms) it quietly does nothing.
class PermissionCard {
  PermissionCard._();

  static const channel = MethodChannel('pb_mapper/permission_helper');

  static String? _shownPermission;
  static void Function(String permission)? _onGranted;
  static bool _listening = false;

  /// Which permission (`'screen_capture'` or `'input'`) the card is currently
  /// shown for, or `null` when hidden.
  static String? get shownPermission => _shownPermission;

  /// Whether the card is currently shown.
  static bool get shown => _shownPermission != null;

  static void _ensureHandler() {
    if (_listening) return;
    _listening = true;
    channel.setMethodCallHandler((call) async {
      if (call.method == 'granted') {
        final perm = call.arguments as String? ?? '';
        if (_shownPermission == perm) _shownPermission = null;
        _onGranted?.call(perm);
      }
    });
  }

  /// Shows the card for [permission] and follows the System Settings window.
  static Future<void> show({
    required String permission,
    required String title,
    required String subtitle,
    required String appName,
    void Function(String permission)? onGranted,
  }) async {
    _ensureHandler();
    _shownPermission = permission;
    _onGranted = onGranted;
    await _invoke('show', {
      'permission': permission,
      'title': title,
      'subtitle': subtitle,
      'appName': appName,
    });
  }

  /// Hides the card if it is currently shown.
  static Future<void> hide() async {
    if (_shownPermission == null) return;
    _shownPermission = null;
    await _invoke('hide');
  }

  @visibleForTesting
  static void resetForTest() {
    _shownPermission = null;
    _onGranted = null;
    _listening = false;
  }

  static Future<void> _invoke(String method, [Object? arguments]) async {
    try {
      await channel.invokeMethod<void>(method, arguments);
    } on MissingPluginException {
      // No native handler registered (widget test or non-macOS platform).
    } on PlatformException catch (error) {
      debugPrint('Permission card call failed: $error');
    }
  }
}
