import 'dart:ui' show Locale, PlatformDispatcher;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// The user's choices about the app itself: theme, language, and the page it
/// last showed. Remembered across launches; a failed read or write only costs
/// the choice, never the launch.
class AppPrefs extends ChangeNotifier {
  AppPrefs({this.persist = true});

  /// Off in tests, which have no preference store.
  final bool persist;

  static const _themeKey = 'theme_mode';
  static const _pageKey = 'last_page';

  ThemeMode themeMode = ThemeMode.system;

  /// Null follows the system language.
  Locale? locale;

  /// The page shown when the app last closed.
  String? lastPage;

  Future<void> load() async {
    if (!persist) return;
    try {
      final prefs = await SharedPreferences.getInstance();
      themeMode = switch (prefs.getString(_themeKey)) {
        'light' => ThemeMode.light,
        'dark' => ThemeMode.dark,
        _ => ThemeMode.system,
      };
      lastPage = prefs.getString(_pageKey);
    } catch (_) {}
    locale = await LocaleController.load();
    notifyListeners();
  }

  /// The language actually shown.
  Locale get activeLocale =>
      locale ?? LocaleController.resolve(PlatformDispatcher.instance.locale);

  Future<void> setThemeMode(ThemeMode mode) async {
    if (mode == themeMode) return;
    themeMode = mode;
    notifyListeners();
    if (!persist) return;
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_themeKey, mode.name);
    } catch (_) {}
  }

  /// [next] null follows the system again.
  Future<void> setLocale(Locale? next) async {
    if (next == locale) return;
    locale = next;
    notifyListeners();
    if (!persist) return;
    await LocaleController.save(next);
  }

  Future<void> setLastPage(String page) async {
    lastPage = page;
    if (!persist) return;
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_pageKey, page);
    } catch (_) {}
  }
}
