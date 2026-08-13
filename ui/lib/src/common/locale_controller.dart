import 'dart:ui' show Locale;

import 'package:shared_preferences/shared_preferences.dart';

/// Remembers the user's language choice.
///
/// A null locale means "follow the system", which is the first-run default so
/// a Chinese system starts in Chinese without being asked.
class LocaleController {
  static const String _storageKey = 'app_locale';

  /// The languages the app ships. Order is the order in the switcher.
  static const List<Locale> supported = [Locale('en'), Locale('zh')];

  static Future<Locale?> load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final code = prefs.getString(_storageKey);
      if (code == null || code.isEmpty) return null;
      return supported.firstWhere(
        (locale) => locale.languageCode == code,
        orElse: () => supported.first,
      );
    } catch (_) {
      return null;
    }
  }

  static Future<void> save(Locale locale) async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_storageKey, locale.languageCode);
    } catch (_) {
      // Losing the preference only costs the choice on next launch.
    }
  }

  /// The next locale in the cycle, used by the title bar button.
  ///
  /// When following the system, resolve that first so the first press moves
  /// away from whatever the user is actually seeing rather than to it.
  static Locale next(Locale? current, Locale systemLocale) {
    final active = current ?? _resolveSystem(systemLocale);
    final index = supported.indexWhere(
      (locale) => locale.languageCode == active.languageCode,
    );
    return supported[(index + 1) % supported.length];
  }

  /// The label for the *current* language, shown on the button.
  static String labelFor(Locale? current, Locale systemLocale) {
    final active = current ?? _resolveSystem(systemLocale);
    return active.languageCode == 'zh' ? '中' : 'EN';
  }

  /// The supported locale that a system locale maps onto.
  static Locale resolve(Locale systemLocale) {
    return supported.firstWhere(
      (locale) => locale.languageCode == systemLocale.languageCode,
      orElse: () => supported.first,
    );
  }

  static Locale _resolveSystem(Locale systemLocale) => resolve(systemLocale);
}
