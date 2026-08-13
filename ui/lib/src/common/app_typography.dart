import 'dart:io' show Platform;

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';

/// The font choices that have to differ per platform.
///
/// Flutter takes the default family from the platform — Segoe UI on Windows,
/// SF Pro on macOS — and lets the shaper fall back run by run for whatever
/// that family cannot draw. No such family has a Chinese glyph, so every
/// Chinese run in this app is drawn by a fallback face, and which face that is
/// decides whether the theme comes out right.
///
/// macOS falls back to PingFang SC, which ships six real weights, so 500 and
/// 600 land on genuine faces and nothing here needs to intervene. Windows
/// falls back to Microsoft YaHei, which has Light, Regular and Bold and
/// nothing between: a run asking for 500 comes back Regular while the Latin
/// beside it gets Segoe UI Semibold, which does exist, and the line renders at
/// two weights. Linux is worse still, in that the answer depends on whatever
/// fontconfig was set up with. Both of those get a bundled face instead.
abstract final class AppTypography {
  /// The families consulted for glyphs the platform default cannot draw.
  ///
  /// Only the fallback is replaced, never the primary family: English text
  /// keeps rendering in Segoe UI, which is both what the platform looks like
  /// and already correct at every weight. The bundled family is named first so
  /// the system CJK font is only reached for characters outside the subset
  /// (see `tool/gen_cjk_subset.mjs`), which is what keeps that subset from
  /// having to be exhaustive.
  static List<String>? get uiFallback {
    if (kIsWeb) {
      return null;
    }
    if (Platform.isWindows) {
      return const <String>[
        _bundledCjk,
        'Microsoft YaHei UI',
        'Microsoft YaHei',
      ];
    }
    if (Platform.isLinux) {
      return const <String>[
        _bundledCjk,
        'Noto Sans CJK SC',
        'Source Han Sans SC',
        'WenQuanYi Micro Hei',
      ];
    }
    // macOS reaches PingFang SC on its own and it has every weight the theme
    // asks for, so there is nothing to add.
    return null;
  }

  /// Declared in `pubspec.yaml` with one file per weight. Static instances
  /// rather than the variable font they were cut from, because Flutter never
  /// maps [TextStyle.fontWeight] onto a `wght` axis — the only writes to that
  /// axis in the framework are explicit [FontVariation]s in the icon widgets.
  static const String _bundledCjk = 'NotoSansSC';

  /// A monospaced style for log lines and server dumps.
  ///
  /// `monospace` is a CSS generic rather than a family name, and bare `Courier`
  /// on Windows is a legacy bitmap font: DirectWrite matches neither, so both
  /// quietly return the default proportional face and the columns stop lining
  /// up. Name families that actually exist and let the fallback list sort out
  /// which platform is which.
  static TextStyle mono({
    double? fontSize,
    FontWeight? fontWeight,
    Color? color,
  }) => TextStyle(
    fontFamily: _monoFamily,
    fontFamilyFallback: _monoFallback,
    fontSize: fontSize,
    fontWeight: fontWeight,
    color: color,
  );

  static String get _monoFamily {
    if (kIsWeb) {
      return 'monospace';
    }
    if (Platform.isWindows) {
      return 'Consolas';
    }
    if (Platform.isMacOS) {
      return 'Menlo';
    }
    return 'monospace';
  }

  static const List<String> _monoFallback = <String>[
    'Cascadia Mono',
    'Consolas',
    'SF Mono',
    'Menlo',
    'DejaVu Sans Mono',
    'Liberation Mono',
    'monospace',
    // Log lines carry Chinese as readily as the rest of the app, and none of
    // the families above can draw it.
    _bundledCjk,
    'Microsoft YaHei UI',
    'PingFang SC',
  ];
}
