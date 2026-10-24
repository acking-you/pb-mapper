import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';

/// The five places the app has. Everything else opens over one of them.
enum AppPage {
  overview,
  tunnels,
  relay,
  logs,
  settings;

  IconData get icon => switch (this) {
    AppPage.overview => AppIcons.overview,
    AppPage.tunnels => AppIcons.tunnels,
    AppPage.relay => AppIcons.relay,
    AppPage.logs => AppIcons.logs,
    AppPage.settings => AppIcons.settings,
  };

  String label(BuildContext context) {
    final l10n = context.l10n;
    return switch (this) {
      AppPage.overview => l10n.navOverview,
      AppPage.tunnels => l10n.navTunnels,
      AppPage.relay => l10n.navRelay,
      AppPage.logs => l10n.navLogs,
      AppPage.settings => l10n.navSettings,
    };
  }

  /// A page of cards sits on the grouped grey sheet, so its white cards lift
  /// off it; a page you look at or read sits on white.
  bool get grouped =>
      this == AppPage.tunnels ||
      this == AppPage.relay ||
      this == AppPage.settings;

  static AppPage? parse(String? name) {
    for (final page in values) {
      if (page.name == name) return page;
    }
    return null;
  }
}

/// What every page reaches for: the data, the preferences, the galaxy's
/// state, and the way to another page.
class AppScope extends InheritedWidget {
  const AppScope({
    super.key,
    required this.store,
    required this.relay,
    required this.prefs,
    required this.updates,
    required this.galaxy,
    required this.page,
    required this.go,
    required super.child,
  });

  final AppStore store;

  /// The relay as a whole, read while the Relay page is on screen.
  final RelayStore relay;
  final AppPrefs prefs;

  /// The app's self-update, owned by the app so a background find is still
  /// there when Settings opens.
  final UpdateController updates;

  /// Owned by the app, so the galaxy keeps its camera across page changes.
  final GalaxyView galaxy;

  /// The page on screen.
  final AppPage page;

  /// Show another page.
  final ValueChanged<AppPage> go;

  static AppScope of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppScope>();
    assert(scope != null, 'No AppScope above this context.');
    return scope!;
  }

  @override
  bool updateShouldNotify(AppScope old) =>
      old.store != store ||
      old.relay != relay ||
      old.prefs != prefs ||
      old.updates != updates ||
      old.galaxy != galaxy ||
      old.page != page;
}
