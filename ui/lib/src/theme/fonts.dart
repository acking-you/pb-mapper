import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';

/// The typefaces, after POCOX's design system: Google Sans Flex for prose and
/// Geist Mono for addresses, keys and logs, both bundled (SIL OFL, see
/// `assets/fonts/OFL-*.txt`). Neither carries a single CJK glyph, so Chinese
/// always resolves down [uiCjkFallback].
///
/// Gated on [defaultTargetPlatform] rather than `dart:io`, so `flutter test`
/// (forced to android) takes the same branch as a phone.

/// The bundled prose face. Latin only.
const String appFontFamily = 'GoogleSansFlex';

/// The bundled code face. Latin only.
const String monoFontFamily = 'GeistMono';

/// The bundled Noto Sans SC subset (see `assets/fonts/README.md`).
const String bundledCjkFamily = 'NotoSansSC';

/// Whether this is a desktop OS, where the pointer idioms apply.
bool get isDesktop =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.windows ||
        defaultTargetPlatform == TargetPlatform.macOS ||
        defaultTargetPlatform == TargetPlatform.linux);

/// The UI face: Google Sans Flex everywhere but Windows.
///
/// Flutter draws text with grayscale antialiasing rather than ClearType, and at
/// 13–15 px on a 100–125 % display a bundled face's joins smear. Segoe UI is
/// hinted for exactly those sizes and ships with every Windows install.
String get uiFontFamily =>
    defaultTargetPlatform == TargetPlatform.windows && !kIsWeb
    ? 'Segoe UI'
    : appFontFamily;

/// The CJK chain behind [uiFontFamily].
///
/// macOS and iOS lead with PingFang SC, which has every weight the theme asks
/// for. Windows and Linux lead with the bundled subset: Microsoft YaHei has no
/// 500 or 600, so a Medium line renders half Regular, and Linux depends on
/// whatever fontconfig was given.
List<String> get uiCjkFallback {
  if (kIsWeb) return const [bundledCjkFamily, 'PingFang SC'];
  return switch (defaultTargetPlatform) {
    TargetPlatform.macOS ||
    TargetPlatform.iOS => const ['PingFang SC', bundledCjkFamily],
    TargetPlatform.windows => const [
      bundledCjkFamily,
      'Microsoft YaHei UI',
      'Microsoft YaHei',
      // Always present: a stripped install without Segoe UI still lands Latin
      // on a real face instead of tofu.
      appFontFamily,
    ],
    _ => const [
      bundledCjkFamily,
      'Noto Sans CJK SC',
      'Source Han Sans SC',
      'WenQuanYi Micro Hei',
    ],
  };
}

/// Fallback behind [monoFontFamily]: real monospaced faces first, then the
/// CJK chain, since a log line carries Chinese as readily as the rest.
const List<String> monoFallback = [
  'Cascadia Mono',
  'Consolas',
  'SF Mono',
  'Menlo',
  'DejaVu Sans Mono',
  bundledCjkFamily,
  'Microsoft YaHei UI',
  'PingFang SC',
];

/// A monospaced style for keys, addresses and log lines.
TextStyle mono({
  double? fontSize,
  FontWeight? fontWeight,
  Color? color,
  double? height,
}) => TextStyle(
  fontFamily: monoFontFamily,
  fontFamilyFallback: monoFallback,
  fontSize: fontSize,
  fontWeight: fontWeight,
  color: color,
  height: height,
);
