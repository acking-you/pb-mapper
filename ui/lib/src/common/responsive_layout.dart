import 'package:flutter/material.dart';

class ResponsiveLayout {
  static const double mobileBreakpoint = 600;
  static const double tabletBreakpoint = 1024;
  static const double desktopBreakpoint = 1440;

  static bool isMobile(BuildContext context) =>
      MediaQuery.of(context).size.width < mobileBreakpoint;

  static bool isTablet(BuildContext context) =>
      MediaQuery.of(context).size.width >= mobileBreakpoint &&
      MediaQuery.of(context).size.width < tabletBreakpoint;

  static bool isDesktop(BuildContext context) =>
      MediaQuery.of(context).size.width >= tabletBreakpoint;

  static double getHorizontalPadding(BuildContext context) {
    if (isMobile(context)) return 16.0;
    if (isTablet(context)) return 24.0;
    return 32.0;
  }

  static double getVerticalSpacing(BuildContext context) {
    if (isMobile(context)) return 12.0;
    if (isTablet(context)) return 16.0;
    return 24.0;
  }

  static double getCardPadding(BuildContext context) {
    if (isMobile(context)) return 12.0;
    if (isTablet(context)) return 16.0;
    return 20.0;
  }

  static double getButtonHeight(BuildContext context) {
    if (isMobile(context)) return 48.0;
    return 56.0;
  }

  static double getButtonWidth(BuildContext context) {
    if (isMobile(context)) return double.infinity;
    return 300.0;
  }

  static double getFontSize(BuildContext context, double baseFontSize) {
    if (isMobile(context)) return baseFontSize * 0.9;
    return baseFontSize;
  }

  static int getCrossAxisCount(BuildContext context) {
    if (isMobile(context)) return 1;
    if (isTablet(context)) return 2;
    return 3;
  }

  static EdgeInsets getScreenPadding(BuildContext context) {
    final horizontalPadding = getHorizontalPadding(context);
    final verticalPadding = isMobile(context) ? 8.0 : 16.0;
    return EdgeInsets.symmetric(
      horizontal: horizontalPadding,
      vertical: verticalPadding,
    );
  }

  static Widget buildResponsiveRow({
    required BuildContext context,
    required List<Widget> children,
    MainAxisAlignment mainAxisAlignment = MainAxisAlignment.start,
    CrossAxisAlignment crossAxisAlignment = CrossAxisAlignment.center,
  }) {
    if (isMobile(context)) {
      return Column(
        mainAxisAlignment: mainAxisAlignment,
        crossAxisAlignment: crossAxisAlignment,
        children: children
            .map(
              (child) => Padding(
                padding: const EdgeInsets.only(bottom: 8.0),
                child: child,
              ),
            )
            .toList(),
      );
    }

    return Row(
      mainAxisAlignment: mainAxisAlignment,
      crossAxisAlignment: crossAxisAlignment,
      children: children,
    );
  }

  static Widget buildResponsiveGrid({
    required BuildContext context,
    required List<Widget> children,
    double childAspectRatio = 1.0,
  }) {
    final crossAxisCount = getCrossAxisCount(context);

    return GridView.count(
      shrinkWrap: true,
      physics: const NeverScrollableScrollPhysics(),
      crossAxisCount: crossAxisCount,
      childAspectRatio: childAspectRatio,
      mainAxisSpacing: getVerticalSpacing(context),
      crossAxisSpacing: getHorizontalPadding(context),
      children: children,
    );
  }

  static double getMaxContentWidth(BuildContext context) {
    if (isMobile(context)) return double.infinity;
    if (isTablet(context)) return 800;
    return 1200;
  }

  static Widget wrapWithMaxWidth({
    required BuildContext context,
    required Widget child,
  }) {
    final maxWidth = getMaxContentWidth(context);
    if (maxWidth == double.infinity) return child;

    return Center(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxWidth: maxWidth),
        child: child,
      ),
    );
  }
}
