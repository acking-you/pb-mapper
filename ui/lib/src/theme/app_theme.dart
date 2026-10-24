import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';

/// Calm graphite neutrals, ink controls and one blue "signal".
///
/// The design follows POCOX: the neutrals carry no blue or violet cast (a
/// tint is what the eye keeps adapting to), the page is a soft off-white
/// rather than pure white, and controls are drawn in ink so colour stays free
/// for meaning. The blue is the logo's, and it marks only what is this
/// computer's or live — the home star, a tunnel carrying traffic, a focused
/// field. Running, retrying and failing keep their own hues so a healthy
/// tunnel never reads like a selection.
///
/// `onSurfaceVariant` stays above 4.5:1 for small secondary text on every
/// ground; `outline` is a border colour and must not carry text.
ColorScheme appColorScheme(Brightness brightness) {
  final light = brightness == Brightness.light;
  const ink = Color(0xFF1D1D1F);
  const paper = Color(0xFFE6E6E6);
  return ColorScheme.fromSeed(
    seedColor: const Color(0xFF1976D2),
    brightness: brightness,
  ).copyWith(
    primary: light ? ink : paper,
    onPrimary: light ? Colors.white : const Color(0xFF19191B),
    primaryContainer: light ? const Color(0xFFE6E6E4) : const Color(0xFF333336),
    onPrimaryContainer: light ? ink : paper,
    secondary: light ? const Color(0xFF4A4A4D) : const Color(0xFFBDBDC0),
    onSecondary: light ? Colors.white : const Color(0xFF19191B),
    secondaryContainer: light
        ? const Color(0xFFE6E6E4)
        : const Color(0xFF333336),
    onSecondaryContainer: light ? ink : paper,
    tertiary: light ? const Color(0xFF1B6FC8) : const Color(0xFF7DB6F2),
    onTertiary: light ? Colors.white : const Color(0xFF0B2A4D),
    tertiaryContainer: light
        ? const Color(0xFFE5EFFA)
        : const Color(0xFF1C2A3B),
    onTertiaryContainer: light
        ? const Color(0xFF124A8C)
        : const Color(0xFFD2E5FA),
    surface: light ? const Color(0xFFFBFBFA) : const Color(0xFF19191B),
    surfaceBright: light ? Colors.white : const Color(0xFF27272A),
    surfaceDim: light ? const Color(0xFFEDEDEB) : const Color(0xFF141416),
    onSurface: light ? ink : paper,
    onSurfaceVariant: light ? const Color(0xFF4A4A4D) : const Color(0xFFBDBDC0),
    surfaceContainerLowest: light ? Colors.white : const Color(0xFF141416),
    surfaceContainerLow: light
        ? const Color(0xFFF4F4F2)
        : const Color(0xFF202022),
    surfaceContainer: light ? const Color(0xFFEDEDEB) : const Color(0xFF262628),
    surfaceContainerHigh: light
        ? const Color(0xFFE6E6E4)
        : const Color(0xFF2E2E31),
    surfaceContainerHighest: light
        ? const Color(0xFFDEDEDC)
        : const Color(0xFF38383B),
    outline: light ? const Color(0xFFC8C8C6) : const Color(0xFF4C4C50),
    outlineVariant: light ? const Color(0xFFE4E4E2) : const Color(0xFF313134),
    inverseSurface: light ? const Color(0xFF2B2B2E) : const Color(0xFFE6E6E6),
    onInverseSurface: light ? const Color(0xFFF2F2F2) : ink,
    inversePrimary: light ? const Color(0xFFA9CBF2) : const Color(0xFF1B6FC8),
    // No elevation tint: a raised surface is told apart by tone and shadow.
    surfaceTint: Colors.transparent,
    error: light ? const Color(0xFFB3261E) : const Color(0xFFF2B8B5),
    onError: light ? Colors.white : const Color(0xFF601410),
    errorContainer: light ? const Color(0xFFFCE8E6) : const Color(0xFF3F1F1D),
    onErrorContainer: light ? const Color(0xFF8C1D18) : const Color(0xFFF9DEDC),
  );
}

/// The logo's blues as a gradient, for the few moments that are about the app
/// itself — the brand tile, a first-run greeting. Never a control's fill.
LinearGradient brandGradient(ColorScheme scheme) => LinearGradient(
  begin: Alignment.topLeft,
  end: Alignment.bottomRight,
  colors: scheme.brightness == Brightness.light
      ? const [Color(0xFF1976D2), Color(0xFF42A5F5)]
      : const [Color(0xFF2B7FD6), Color(0xFF5AB0F2)],
);

/// The one signal: this computer, and a tunnel carrying traffic.
Color signalColor(ColorScheme scheme) => scheme.tertiary;

/// Healthy: a tunnel running.
Color successColor(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFF1A7F37)
    : const Color(0xFF57C27A);

/// Degraded but not failed: a tunnel retrying.
Color cautionColor(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFF9A5B00)
    : const Color(0xFFE0A647);

/// Ink for quiet glyphs — placeholders, idle icons. Text that must be read
/// uses `onSurfaceVariant`.
Color onSurfaceMuted(ColorScheme scheme) =>
    scheme.onSurface.withValues(alpha: 0.55);

/// A faint neutral wash: a highlighted option, a chip that is "on" without
/// being a status.
Color softWash(ColorScheme scheme, {double strength = 1}) =>
    scheme.onSurface.withValues(
      alpha: (scheme.brightness == Brightness.light ? 0.06 : 0.09) * strength,
    );

/// The well of a text field: a step darker than both the page and cards.
Color fieldFill(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFFEAEAE8)
    : const Color(0xFF29292C);

/// A field set on a white card, where [fieldFill] would read as a hole.
Color cardFieldFill(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFFF2F2F0)
    : const Color(0xFF2A2A2D);

/// The plate behind code and log lines: a half step off the page.
Color codeSurface(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFFF3F3F1)
    : const Color(0xFF151517);

/// The ground of what floats over the page: popovers, menus, dialogs.
Color floatingSurface(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? Colors.white
    : scheme.surfaceContainerHigh;

/// The hairline that outlines floating surfaces in dark mode.
Color floatingEdge(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? Colors.black.withValues(alpha: 0.05)
    : Colors.white.withValues(alpha: 0.07);

/// The window's own ground behind the rail and the content sheet.
Color backdropBase(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? const Color(0xFFEAEAE8)
    : const Color(0xFF0F0F10);

/// Soft light on [backdropBase]: brighter at the top, never a hue.
List<({Alignment center, double radius, Color color})> backdropGlows(
  ColorScheme scheme,
) => scheme.brightness == Brightness.light
    ? const [
        (center: Alignment(-1.0, -1.15), radius: 1.2, color: Color(0x73FFFFFF)),
        (center: Alignment(1.1, -0.9), radius: 0.9, color: Color(0x40FFFFFF)),
      ]
    : const [
        (center: Alignment(-1.0, -1.15), radius: 1.2, color: Color(0x0FFFFFFF)),
      ];

/// The pointer's wash over a row on the backdrop: toward glass, not grey.
Color backdropHover(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? Colors.white.withValues(alpha: 0.5)
    : Colors.white.withValues(alpha: 0.05);

/// A selected row on the backdrop: a pane of chrome glass. Selection is
/// position, not status, so it takes no colour.
Color surfaceSelection(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? Colors.white.withValues(alpha: 0.82)
    : Colors.white.withValues(alpha: 0.09);

/// The content sheet: white for a page you read, grouped grey for a page of
/// cards so the white cards lift off it.
Color sheetColor(ColorScheme scheme, {bool grouped = false}) =>
    scheme.brightness == Brightness.light
    ? (grouped ? const Color(0xFFF4F4F2) : scheme.surface)
    : (grouped ? const Color(0xFF151517) : scheme.surface);

/// The hairline that keeps the sheet's edge crisp against the backdrop.
Color sheetEdge(ColorScheme scheme) => scheme.brightness == Brightness.light
    ? Colors.black.withValues(alpha: 0.06)
    : Colors.white.withValues(alpha: 0.07);

/// The sheet's lift off the backdrop.
List<BoxShadow> sheetShadows(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? [
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.05),
          blurRadius: 2,
          offset: const Offset(0, 1),
        ),
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.07),
          blurRadius: 30,
          offset: const Offset(0, 10),
        ),
      ]
    : [
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.5),
          blurRadius: 30,
          offset: const Offset(0, 12),
        ),
      ];

/// A white card on a grouped sheet.
Color raisedCardColor(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? Colors.white
    : const Color(0xFF212124);

/// The hairline around a [raisedCardColor] card.
Color raisedCardEdge(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? Colors.black.withValues(alpha: 0.05)
    : Colors.white.withValues(alpha: 0.06);

/// The soft lift under a [raisedCardColor] card.
List<BoxShadow> raisedCardShadows(ColorScheme scheme) =>
    scheme.brightness == Brightness.light
    ? [
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.04),
          blurRadius: 2,
          offset: const Offset(0, 1),
        ),
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.045),
          blurRadius: 16,
          offset: const Offset(0, 4),
        ),
      ]
    : [
        BoxShadow(
          color: Colors.black.withValues(alpha: 0.35),
          blurRadius: 12,
          offset: const Offset(0, 4),
        ),
      ];

/// The type scale on Material's roles: 11 (captions), 12 (secondary), 13
/// (controls and rows), 14 (reading) do nearly all the work, titles above.
/// Material's tracking is tuned for Roboto and spreads Google Sans Flex and
/// CJK apart, so every role carries its own.
TextTheme _textTheme(ColorScheme scheme) {
  TextStyle t(
    double size,
    double height, [
    FontWeight w = FontWeight.w400,
    double tracking = 0,
  ]) => TextStyle(
    fontSize: size,
    height: height,
    fontWeight: w,
    letterSpacing: tracking,
  );

  return TextTheme(
    displayLarge: t(48, 1.12, FontWeight.w400, -0.5),
    displayMedium: t(40, 1.15, FontWeight.w400, -0.4),
    displaySmall: t(34, 1.18, FontWeight.w400, -0.3),
    headlineLarge: t(30, 1.2, FontWeight.w600, -0.3),
    headlineMedium: t(26, 1.22, FontWeight.w600, -0.2),
    headlineSmall: t(22, 1.25, FontWeight.w600, -0.1),
    titleLarge: t(19, 1.3, FontWeight.w600, -0.1),
    titleMedium: t(16, 1.35, FontWeight.w600),
    titleSmall: t(13, 1.4, FontWeight.w600),
    bodyLarge: t(15, 1.6),
    bodyMedium: t(14, 1.5),
    bodySmall: t(12, 1.45, FontWeight.w400, 0.1),
  ).apply(
    fontFamily: uiFontFamily,
    fontFamilyFallback: uiCjkFallback,
    bodyColor: scheme.onSurface,
    displayColor: scheme.onSurface,
  );
}

/// The shared Material 3 theme for [scheme]: every button a pill, every icon
/// button a circle, panels generously rounded and lifted by tone rather than
/// outline. [desktopize] re-tunes it for a pointer.
ThemeData _base(ColorScheme scheme) {
  final light = scheme.brightness == Brightness.light;
  TextStyle ui(
    double size, {
    FontWeight weight = FontWeight.w500,
    Color? color,
  }) => TextStyle(
    fontSize: size,
    fontWeight: weight,
    color: color,
    letterSpacing: 0.1,
    fontFamily: uiFontFamily,
    fontFamilyFallback: uiCjkFallback,
  );
  const pill = StadiumBorder();
  final floatShadow = Colors.black.withValues(alpha: light ? 0.14 : 0.5);
  final menuShape = RoundedRectangleBorder(
    borderRadius: BorderRadius.circular(kMenuRadius),
    side: BorderSide(color: floatingEdge(scheme)),
  );
  final fieldRadius = BorderRadius.circular(kFieldRadius);
  return ThemeData(
    colorScheme: scheme,
    useMaterial3: true,
    fontFamily: uiFontFamily,
    fontFamilyFallback: uiCjkFallback,
    textTheme: _textTheme(scheme),
    scaffoldBackgroundColor: scheme.surface,
    canvasColor: scheme.surface,
    pageTransitionsTheme: appPageTransitions,
    hoverColor: scheme.onSurface.withValues(alpha: 0.05),
    highlightColor: scheme.onSurface.withValues(alpha: 0.06),
    dividerTheme: DividerThemeData(
      color: scheme.outlineVariant,
      thickness: 1,
      space: 1,
    ),
    // Buttons are lit capsules: ink for the filled action, a pane of chrome
    // glass for the secondary one, each with a sheen ([LitFace]) and a soft
    // shadow that rises on hover and settles when pressed.
    filledButtonTheme: FilledButtonThemeData(
      style:
          FilledButton.styleFrom(
            minimumSize: const Size(48, 46),
            padding: const EdgeInsets.symmetric(horizontal: 22),
            shape: pill,
            textStyle: ui(14.5, weight: FontWeight.w600),
          ).copyWith(
            elevation: WidgetStateProperty.resolveWith(
              (states) =>
                  states.contains(WidgetState.disabled) ||
                      states.contains(WidgetState.pressed)
                  ? 0
                  : states.contains(WidgetState.hovered)
                  ? 3
                  : 1,
            ),
            shadowColor: WidgetStatePropertyAll(
              light ? Colors.black.withValues(alpha: 0.38) : Colors.black,
            ),
            backgroundBuilder: (context, states, child) =>
                LitFace(states: states, child: child),
          ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style:
          OutlinedButton.styleFrom(
            minimumSize: const Size(48, 46),
            padding: const EdgeInsets.symmetric(horizontal: 20),
            foregroundColor: scheme.onSurface,
            shape: pill,
            textStyle: ui(14.5),
          ).copyWith(
            backgroundColor: WidgetStateProperty.resolveWith((states) {
              if (states.contains(WidgetState.disabled)) {
                return Colors.transparent;
              }
              final strong =
                  states.contains(WidgetState.hovered) ||
                  states.contains(WidgetState.pressed);
              return light
                  ? Colors.white.withValues(alpha: strong ? 0.95 : 0.7)
                  : Colors.white.withValues(alpha: strong ? 0.12 : 0.07);
            }),
            side: WidgetStateProperty.resolveWith(
              (states) => BorderSide(
                color: states.contains(WidgetState.disabled)
                    ? scheme.outlineVariant
                    : light
                    ? Colors.black.withValues(alpha: 0.09)
                    : Colors.white.withValues(alpha: 0.12),
              ),
            ),
            elevation: WidgetStateProperty.resolveWith(
              (states) =>
                  states.contains(WidgetState.hovered) &&
                      !states.contains(WidgetState.pressed)
                  ? 2
                  : 0,
            ),
            shadowColor: WidgetStatePropertyAll(
              light ? Colors.black.withValues(alpha: 0.22) : Colors.black,
            ),
            overlayColor: WidgetStateProperty.resolveWith(
              (states) => states.contains(WidgetState.pressed)
                  ? scheme.onSurface.withValues(alpha: 0.06)
                  : Colors.transparent,
            ),
            backgroundBuilder: (context, states, child) =>
                LitFace(states: states, glass: true, child: child),
          ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        minimumSize: const Size(48, 44),
        padding: const EdgeInsets.symmetric(horizontal: 16),
        foregroundColor: scheme.onSurface,
        shape: pill,
        textStyle: ui(14.5),
      ),
    ),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        minimumSize: const Size(44, 44),
        foregroundColor: scheme.onSurfaceVariant,
        shape: const CircleBorder(),
      ),
    ),
    chipTheme: ChipThemeData(
      backgroundColor: Colors.transparent,
      selectedColor: scheme.surfaceContainerHigh,
      checkmarkColor: scheme.onSurface,
      side: BorderSide(color: scheme.outlineVariant),
      shape: pill,
      labelStyle: ui(12.5, color: scheme.onSurface),
      padding: const EdgeInsets.symmetric(horizontal: 6),
    ),
    // Fields are soft filled wells with no outline at rest; focus draws a
    // firm ink ring with a faint signal glow.
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: fieldFill(scheme),
      hoverColor: scheme.onSurface.withValues(alpha: 0.03),
      border: FieldBorder(borderRadius: fieldRadius),
      enabledBorder: FieldBorder(borderRadius: fieldRadius),
      disabledBorder: FieldBorder(borderRadius: fieldRadius),
      focusedBorder: FieldBorder(
        borderRadius: fieldRadius,
        borderSide: BorderSide(
          color: scheme.onSurface.withValues(alpha: light ? 0.5 : 0.55),
          width: 1.5,
        ),
        glow: scheme.tertiary.withValues(alpha: light ? 0.16 : 0.28),
      ),
      errorBorder: FieldBorder(
        borderRadius: fieldRadius,
        borderSide: BorderSide(color: scheme.error.withValues(alpha: 0.7)),
      ),
      focusedErrorBorder: FieldBorder(
        borderRadius: fieldRadius,
        borderSide: BorderSide(color: scheme.error, width: 1.5),
      ),
      contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      hintStyle: TextStyle(color: onSurfaceMuted(scheme), fontSize: 14),
      labelStyle: TextStyle(color: scheme.onSurfaceVariant),
      floatingLabelStyle: TextStyle(
        color: scheme.onSurfaceVariant,
        fontWeight: FontWeight.w500,
      ),
      helperStyle: TextStyle(
        color: scheme.onSurfaceVariant,
        fontSize: 12.5,
        height: 1.45,
      ),
      errorStyle: TextStyle(color: scheme.error, fontSize: 12.5, height: 1.45),
      helperMaxLines: 3,
      errorMaxLines: 3,
      prefixIconColor: scheme.onSurfaceVariant,
      suffixIconColor: scheme.onSurfaceVariant,
    ),
    textSelectionTheme: TextSelectionThemeData(
      cursorColor: scheme.onSurface,
      selectionColor: scheme.tertiary.withValues(alpha: light ? 0.22 : 0.35),
      selectionHandleColor: scheme.tertiary,
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(
      color: scheme.onSurfaceVariant,
      linearTrackColor: scheme.surfaceContainerHighest,
      circularTrackColor: Colors.transparent,
    ),
    switchTheme: SwitchThemeData(
      thumbColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected)
            ? scheme.onPrimary
            : scheme.outline,
      ),
      trackColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected)
            ? scheme.primary
            : scheme.surfaceContainerHighest,
      ),
      trackOutlineColor: const WidgetStatePropertyAll(Colors.transparent),
    ),
    listTileTheme: ListTileThemeData(
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(kControlRadius),
      ),
      iconColor: scheme.onSurfaceVariant,
      titleTextStyle: ui(
        14.5,
        weight: FontWeight.w400,
        color: scheme.onSurface,
      ),
      subtitleTextStyle: ui(
        12.5,
        weight: FontWeight.w400,
        color: scheme.onSurfaceVariant,
      ),
      minLeadingWidth: 20,
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: light
          ? Colors.white.withValues(alpha: 0.94)
          : const Color(0xFF252528).withValues(alpha: 0.96),
      surfaceTintColor: Colors.transparent,
      elevation: 24,
      shadowColor: floatShadow,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(kDialogRadius),
        side: BorderSide(
          color: light ? Colors.white : Colors.white.withValues(alpha: 0.1),
        ),
      ),
      titleTextStyle: ui(18, weight: FontWeight.w600, color: scheme.onSurface),
      contentTextStyle: ui(
        14,
        weight: FontWeight.w400,
        color: scheme.onSurfaceVariant,
      ).copyWith(height: 1.55),
      actionsPadding: const EdgeInsets.fromLTRB(20, 4, 20, 20),
      insetPadding: const EdgeInsets.symmetric(horizontal: 24, vertical: 24),
    ),
    popupMenuTheme: PopupMenuThemeData(
      elevation: 10,
      shadowColor: floatShadow,
      color: floatingSurface(scheme),
      surfaceTintColor: Colors.transparent,
      shape: menuShape,
      menuPadding: const EdgeInsets.all(6),
      textStyle: ui(14, weight: FontWeight.w400, color: scheme.onSurface),
    ),
    menuTheme: MenuThemeData(
      style: MenuStyle(
        elevation: const WidgetStatePropertyAll(10),
        shadowColor: WidgetStatePropertyAll(floatShadow),
        surfaceTintColor: const WidgetStatePropertyAll(Colors.transparent),
        backgroundColor: WidgetStatePropertyAll(floatingSurface(scheme)),
        shape: WidgetStatePropertyAll(menuShape),
        padding: const WidgetStatePropertyAll(EdgeInsets.all(6)),
      ),
    ),
    tooltipTheme: TooltipThemeData(
      waitDuration: const Duration(milliseconds: 450),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
      decoration: BoxDecoration(
        color: scheme.inverseSurface,
        borderRadius: BorderRadius.circular(8),
      ),
      textStyle: ui(12, color: scheme.onInverseSurface),
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: light ? Colors.white : scheme.surfaceContainer,
      surfaceTintColor: Colors.transparent,
      showDragHandle: true,
      dragHandleColor: scheme.outline,
      dragHandleSize: const Size(36, 4),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(28)),
      ),
    ),
    scrollbarTheme: ScrollbarThemeData(
      thickness: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.hovered) ? 8.0 : 6.0,
      ),
      radius: const Radius.circular(4),
      crossAxisMargin: 2,
      mainAxisMargin: 2,
    ),
    // A flat bar that blends into the page: with the native title bar hidden
    // it reads as part of the window itself.
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      foregroundColor: scheme.onSurface,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      scrolledUnderElevation: 0,
      titleTextStyle: ui(17, weight: FontWeight.w600, color: scheme.onSurface),
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      color: raisedCardColor(scheme),
      surfaceTintColor: Colors.transparent,
      margin: EdgeInsets.zero,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(kCardRadius),
        side: BorderSide(color: raisedCardEdge(scheme)),
      ),
    ),
    iconTheme: IconThemeData(color: scheme.onSurfaceVariant, size: 20),
  );
}

/// Pointer density and hover feedback over the shared touch-ready theme:
/// hover instead of ripples, a pointing hand over anything clickable.
ThemeData desktopize(ThemeData base) {
  final scheme = base.colorScheme;
  const density = EdgeInsets.symmetric(horizontal: 16);
  ButtonStyle tune(ButtonStyle? style, {EdgeInsets padding = density}) =>
      ButtonStyle(
        visualDensity: VisualDensity.standard,
        minimumSize: const WidgetStatePropertyAll(Size(36, 36)),
        padding: WidgetStatePropertyAll(padding),
        textStyle: WidgetStatePropertyAll(
          style?.textStyle
              ?.resolve(const <WidgetState>{})
              ?.copyWith(fontSize: 13.5),
        ),
        mouseCursor: clickable,
      ).merge(style);

  return base.copyWith(
    visualDensity: VisualDensity.compact,
    splashFactory: NoSplash.splashFactory,
    highlightColor: scheme.onSurface.withValues(alpha: 0.08),
    focusColor: scheme.onSurface.withValues(alpha: 0.08),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        minimumSize: const Size(34, 34),
        iconSize: 18,
      ).copyWith(mouseCursor: clickable).merge(base.iconButtonTheme.style),
    ),
    listTileTheme: base.listTileTheme.copyWith(mouseCursor: clickable),
    switchTheme: base.switchTheme.copyWith(mouseCursor: clickable),
    filledButtonTheme: FilledButtonThemeData(
      style: tune(base.filledButtonTheme.style),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: tune(base.outlinedButtonTheme.style),
    ),
    textButtonTheme: TextButtonThemeData(
      style: tune(
        base.textButtonTheme.style,
        padding: const EdgeInsets.symmetric(horizontal: 12),
      ),
    ),
    inputDecorationTheme: base.inputDecorationTheme.copyWith(
      isDense: true,
      contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
    ),
  );
}

/// The theme for [brightness], pointer-tuned on desktop.
ThemeData buildAppTheme(Brightness brightness) {
  final base = _base(appColorScheme(brightness));
  return isDesktop ? desktopize(base) : base;
}
