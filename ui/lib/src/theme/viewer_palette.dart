import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

/// The remote desktop viewer's colours. The viewer is dark whatever the
/// app's theme, so a picture is never framed in white, and its chrome is
/// translucent graphite over the picture rather than the app's glass: a blur
/// over a live video texture would redo itself every frame.
abstract final class ViewerPalette {
  /// The ground around the picture on desktop, and its letterbox.
  static const Color ground = Color(0xFF0F0F10);

  /// The ground around the picture on a phone.
  static const Color black = Color(0xFF000000);

  /// State cards and the modifier bar.
  static const Color card = Color(0xFF1C1C1E);
  static const Color menu = Color(0xFF262628);

  /// The toolbar, its collapsed handle and the small pills.
  static const Color chrome = Color(0xE6202023);

  /// The quality panel and the reconnect pill.
  static const Color panel = Color(0xF01C1C1E);

  /// Hints laid over the picture on a phone.
  static const Color hint = Color(0xDB18181A);

  /// Hairlines on the dark chrome.
  static const Color edge = Color.fromRGBO(255, 255, 255, 0.08);

  /// A pressed or open button.
  static const Color fill = Color.fromRGBO(255, 255, 255, 0.10);

  /// The chosen segment.
  static const Color fillStrong = Color.fromRGBO(255, 255, 255, 0.16);
  static const Color separator = Color.fromRGBO(255, 255, 255, 0.12);
  static const Color keySeparator = Color.fromRGBO(255, 255, 255, 0.14);

  /// An outlined button's border.
  static const Color outline = Color.fromRGBO(255, 255, 255, 0.18);

  static const Color text = Color(0xFFEDEDEF);
  static const Color textSecondary = Color(0xFFB4B4B8);

  /// Disabled items, and "pb-mapper" beside the computer's name.
  static const Color textQuiet = Color(0xFF8E8E93);

  /// The live session: this is the one blue in the viewer.
  static const Color signal = Color(0xFF7DB6F2);
  static const Color success = Color(0xFF57C27A);
  static const Color caution = Color(0xFFE0A647);
  static const Color danger = Color(0xFFF2B8B5);

  /// The viewer's menus: Display, Shortcuts, More.
  static const MenuStyle menuStyle = MenuStyle(
    backgroundColor: WidgetStatePropertyAll(menu),
    surfaceTintColor: WidgetStatePropertyAll(Colors.transparent),
    shadowColor: WidgetStatePropertyAll(Colors.black),
    elevation: WidgetStatePropertyAll(12),
    padding: WidgetStatePropertyAll(EdgeInsets.all(6)),
    shape: WidgetStatePropertyAll(
      RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radius.circular(kMenuRadius)),
        side: BorderSide(color: edge),
      ),
    ),
  );
}

/// The app's dark theme on the viewer's ground, with menus, tooltips and
/// dialogs in the viewer's palette. The platform follows [context]'s theme.
ThemeData viewerTheme(BuildContext context) {
  final base = buildAppTheme(Brightness.dark);
  TextStyle ui(double size, Color color, [FontWeight w = FontWeight.w400]) =>
      TextStyle(
        fontSize: size,
        fontWeight: w,
        color: color,
        fontFamily: uiFontFamily,
        fontFamilyFallback: uiCjkFallback,
      );
  const panelShape = RoundedRectangleBorder(
    borderRadius: BorderRadius.all(Radius.circular(kPanelRadius)),
    side: BorderSide(color: ViewerPalette.edge),
  );
  return base.copyWith(
    platform: Theme.of(context).platform,
    scaffoldBackgroundColor: ViewerPalette.ground,
    canvasColor: ViewerPalette.ground,
    menuTheme: const MenuThemeData(style: ViewerPalette.menuStyle),
    popupMenuTheme: base.popupMenuTheme.copyWith(
      color: ViewerPalette.menu,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radius.circular(kMenuRadius)),
        side: BorderSide(color: ViewerPalette.edge),
      ),
      textStyle: ui(13.5, ViewerPalette.text),
    ),
    tooltipTheme: base.tooltipTheme.copyWith(
      decoration: BoxDecoration(
        color: ViewerPalette.menu,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: ViewerPalette.edge),
      ),
      textStyle: ui(12, ViewerPalette.text, FontWeight.w500),
    ),
    dialogTheme: base.dialogTheme.copyWith(
      backgroundColor: ViewerPalette.card,
      shape: panelShape,
      titleTextStyle: ui(17, ViewerPalette.text, FontWeight.w600),
      contentTextStyle: ui(
        14,
        ViewerPalette.textSecondary,
      ).copyWith(height: 1.55),
    ),
  );
}
