import 'package:flutter/material.dart';

/// Corner radii and the pointer cursor, shared by the theme and the widgets
/// that draw their own surfaces. Spacing is deliberately not tokenised — each
/// widget bakes its own — but radii recur often enough that a literal at every
/// site drifts.

/// Cards, panels, code plates.
const double kPanelRadius = 16;

/// Small controls: chips, menu items.
const double kControlRadius = 10;

/// List rows and selections.
const double kRowRadius = 14;

/// Grouped cards on a page.
const double kCardRadius = 20;

/// Dialogs and modal panels.
const double kDialogRadius = 24;

/// The content sheet laid over the window's backdrop.
const double kSheetRadius = 18;

/// The backdrop left showing around the content sheet.
const double kSheetInset = 8;

/// Text fields.
const double kFieldRadius = 14;

/// Popup menus.
const double kMenuRadius = 14;

/// A pointing hand while enabled, the plain arrow while disabled.
///
/// Flutter's `WidgetStateMouseCursor.adaptiveClickable` resolves to a hand
/// only on the web; on a native desktop build every button would hover like
/// inert text. `InkWell` hardcodes that cursor, so it is passed explicitly.
const WidgetStateMouseCursor clickable = WidgetStateMouseCursor.resolveWith(
  _clickableCursor,
  debugDescription: 'desktopClickable',
);

MouseCursor _clickableCursor(Set<WidgetState> states) =>
    states.contains(WidgetState.disabled)
    ? SystemMouseCursors.basic
    : SystemMouseCursors.click;

/// The width of one line of [text] in [style], at the current text scale.
Size measureTextLine(BuildContext context, String text, TextStyle? style) {
  final painter = TextPainter(
    text: TextSpan(
      text: text,
      style: DefaultTextStyle.of(context).style.merge(style),
    ),
    maxLines: 1,
    textDirection: Directionality.maybeOf(context) ?? TextDirection.ltr,
    textScaler: MediaQuery.maybeTextScalerOf(context) ?? TextScaler.noScaling,
  )..layout();
  final size = painter.size;
  painter.dispose();
  return size;
}
