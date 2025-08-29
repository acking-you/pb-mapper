import 'package:flutter/material.dart';
import 'package:ui/src/common/responsive_layout.dart';

class DesktopLayout extends StatefulWidget {
  final int selectedIndex;
  final Function(int) onNavigationChanged;
  final Widget child;

  const DesktopLayout({
    super.key,
    required this.selectedIndex,
    required this.onNavigationChanged,
    required this.child,
  });

  @override
  State<DesktopLayout> createState() => _DesktopLayoutState();
}

class _DesktopLayoutState extends State<DesktopLayout> {
  @override
  Widget build(BuildContext context) {
    if (ResponsiveLayout.isMobile(context)) {
      return widget.child;
    }

    return Scaffold(
      body: Row(
        children: [
          Column(
            children: [
              Container(
                width: ResponsiveLayout.isDesktop(context) ? 200 : 80,
                padding: const EdgeInsets.all(16.0),
                child: Row(
                  children: [
                    IconButton(
                      icon: const Icon(Icons.home),
                      onPressed: () => widget.onNavigationChanged(0),
                      tooltip: 'Home',
                    ),
                    if (ResponsiveLayout.isDesktop(context))
                      const Expanded(
                        child: Text(
                          'pb-mapper',
                          style: TextStyle(
                            fontSize: 18,
                            fontWeight: FontWeight.bold,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              const Divider(height: 1),
              Expanded(
                child: NavigationRail(
                  selectedIndex: widget.selectedIndex == 0
                      ? null
                      : widget.selectedIndex - 1,
                  onDestinationSelected: (index) =>
                      widget.onNavigationChanged(index + 1),
                  minWidth: 80,
                  minExtendedWidth: 200,
                  extended: ResponsiveLayout.isDesktop(context),
                  destinations: const [
                    NavigationRailDestination(
                      icon: Icon(Icons.dns_outlined),
                      selectedIcon: Icon(Icons.dns),
                      label: Text('Server'),
                    ),
                    NavigationRailDestination(
                      icon: Icon(Icons.app_registration_outlined),
                      selectedIcon: Icon(Icons.app_registration),
                      label: Text('Register'),
                    ),
                    NavigationRailDestination(
                      icon: Icon(Icons.cable_outlined),
                      selectedIcon: Icon(Icons.cable),
                      label: Text('Connect'),
                    ),
                    NavigationRailDestination(
                      icon: Icon(Icons.monitor_outlined),
                      selectedIcon: Icon(Icons.monitor),
                      label: Text('Status'),
                    ),
                    NavigationRailDestination(
                      icon: Icon(Icons.settings_outlined),
                      selectedIcon: Icon(Icons.settings),
                      label: Text('Config'),
                    ),
                  ],
                ),
              ),
            ],
          ),
          const VerticalDivider(thickness: 1, width: 1),
          Expanded(child: widget.child),
        ],
      ),
    );
  }
}

class ResponsiveScaffold extends StatelessWidget {
  final String? title;
  final Widget body;
  final List<Widget>? actions;
  final Widget? floatingActionButton;
  final Widget? bottomNavigationBar;
  final bool showBackButton;

  const ResponsiveScaffold({
    super.key,
    this.title,
    required this.body,
    this.actions,
    this.floatingActionButton,
    this.bottomNavigationBar,
    this.showBackButton = false,
  });

  @override
  Widget build(BuildContext context) {
    if (ResponsiveLayout.isMobile(context)) {
      return Scaffold(
        appBar: AppBar(
          title: title != null ? Text(title!) : null,
          actions: actions,
          automaticallyImplyLeading: showBackButton,
        ),
        body: body,
        floatingActionButton: floatingActionButton,
        bottomNavigationBar: bottomNavigationBar,
      );
    }

    return Scaffold(
      appBar: actions != null || title != null
          ? AppBar(
              title: title != null ? Text(title!) : null,
              actions: actions,
              automaticallyImplyLeading: false,
            )
          : null,
      body: ResponsiveLayout.wrapWithMaxWidth(context: context, child: body),
      floatingActionButton: floatingActionButton,
    );
  }
}
