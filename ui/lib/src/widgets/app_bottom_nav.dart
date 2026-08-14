import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_destination.dart';

/// The navigation for the compact layout: the same destinations the rail
/// draws, along the bottom of the screen.
///
/// This is Material 3's own [NavigationBar] rather than anything hand-rolled.
/// It already has the pill indicator, the label under the icon, the selection
/// animation and the accessibility wiring, and its indicator uses the same
/// `secondaryContainer` tone the rail fills a selected row with — so the two
/// layouts read as one app. The legacy `BottomNavigationBar` this replaces was
/// Material 2: a bare tinted icon, a hardcoded grey for everything unselected,
/// and no indicator at all.
class AppBottomNav extends StatelessWidget {
  const AppBottomNav({super.key, required this.destinations});

  final List<AppDestination> destinations;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final android = theme.platform == TargetPlatform.android;
    final selected = destinations.indexWhere((d) => d.selected);

    return NavigationBar(
      selectedIndex: selected < 0 ? 0 : selected,
      onDestinationSelected: (index) => destinations[index].onPressed(),
      // Android's system gesture bar sits right below this, so the stock 80px
      // is enough to crowd it. Same trim proxy-everything makes.
      height: android ? 64 : null,
      labelTextStyle: android
          ? WidgetStatePropertyAll(theme.textTheme.labelSmall)
          : null,
      // Every destination here is a place, not a mode — a form, a list, a log
      // — and none of them is obvious from its icon alone.
      labelBehavior: NavigationDestinationLabelBehavior.alwaysShow,
      destinations: [
        for (final destination in destinations)
          NavigationDestination(
            icon: Icon(destination.icon),
            selectedIcon: Icon(destination.selectedIcon),
            label: destination.fullLabel,
            tooltip: destination.fullLabel,
          ),
      ],
    );
  }
}
