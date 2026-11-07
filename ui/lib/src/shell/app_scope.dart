import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/sharing_indicator.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';

/// The six places the app has. Everything else opens over one of them.
enum AppPage {
  overview,
  tunnels,
  remote,
  relay,
  logs,
  settings;

  /// The rail, top to bottom.
  static const railEntries = [overview, tunnels, remote, relay, logs, settings];

  /// The bottom bar: Logs lives in Settings there.
  static const barEntries = [overview, tunnels, remote, relay, settings];

  /// The bar entry that stands for this page on a compact window.
  AppPage get barEntry => this == logs ? settings : this;

  IconData get icon => switch (this) {
    AppPage.overview => AppIcons.overview,
    AppPage.tunnels => AppIcons.tunnels,
    AppPage.remote => AppIcons.remote,
    AppPage.relay => AppIcons.relay,
    AppPage.logs => AppIcons.logs,
    AppPage.settings => AppIcons.settings,
  };

  /// [short]: the bottom bar's label; the rail and the page title use the
  /// long one.
  String label(BuildContext context, {bool short = false}) {
    final l10n = context.l10n;
    return switch (this) {
      AppPage.overview => l10n.navOverview,
      AppPage.tunnels => l10n.navTunnels,
      AppPage.remote => short ? l10n.navRemote : l10n.navRemoteDesktop,
      AppPage.relay => l10n.navRelay,
      AppPage.logs => l10n.navLogs,
      AppPage.settings => l10n.navSettings,
    };
  }

  /// A page of cards sits on the grouped grey sheet, so its white cards lift
  /// off it; a page you look at or read sits on white.
  bool get grouped =>
      this == AppPage.tunnels ||
      this == AppPage.remote ||
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
/// state, the remote desktop, and the way to another page.
class AppScope extends InheritedWidget {
  const AppScope({
    super.key,
    required this.store,
    required this.relay,
    required this.prefs,
    required this.updates,
    required this.galaxy,
    required this.remote,
    required this.sharing,
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

  /// Viewing other computers, and the viewer on screen.
  final RemoteDesktop remote;

  /// Where the controls for a viewer of this computer are reachable (D15).
  final SharingChrome sharing;

  /// The page on screen.
  final AppPage page;

  /// Show another page.
  final ValueChanged<AppPage> go;

  static AppScope of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppScope>();
    assert(scope != null, 'No AppScope above this context.');
    return scope!;
  }

  /// The scope, if there is one: the D15 controls are also drawn where
  /// there is none, over first-run setup.
  static AppScope? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<AppScope>();

  @override
  bool updateShouldNotify(AppScope old) =>
      old.store != store ||
      old.relay != relay ||
      old.prefs != prefs ||
      old.updates != updates ||
      old.galaxy != galaxy ||
      old.remote != remote ||
      old.sharing != sharing ||
      old.page != page;
}
