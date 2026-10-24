import 'dart:async' show unawaited;
import 'dart:io' show Platform, exit;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:path_provider/path_provider.dart';
import 'package:pb_mapper_ui/src/app.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/ffi/cli_entry.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:window_manager/window_manager.dart';

/// The bundled faces are under the SIL Open Font License, which asks that the
/// notice travel with the font. Registering them puts the text in the standard
/// licence listing beside every package's.
void _registerFontLicenses() {
  for (final (families, file) in [
    (['NotoSansSC'], 'assets/fonts/OFL.txt'),
    (['GoogleSansFlex'], 'assets/fonts/OFL-GoogleSansFlex.txt'),
    (['GeistMono'], 'assets/fonts/OFL-GeistMono.txt'),
  ]) {
    LicenseRegistry.addLicense(() async* {
      yield LicenseEntryWithLineBreaks(
        families,
        await rootBundle.loadString(file),
      );
    });
  }
}

bool get _isDesktop =>
    !kIsWeb && (Platform.isWindows || Platform.isLinux || Platform.isMacOS);

Future<void> main(List<String> args) async {
  // `pb_mapper_ui status` and friends are handled by Rust and never reach the
  // window. Rust also decides what counts as a command, so there is no verb
  // list here to fall out of step with the one in `Command`.
  final exitCode = CliEntry.run(args);
  if (exitCode != null) exit(exitCode);

  WidgetsFlutterBinding.ensureInitialized();
  _registerFontLicenses();
  if (_isDesktop) {
    await windowManager.ensureInitialized();
    // The native title bar is hidden so the app draws one continuous surface:
    // the rail sits on the backdrop up to the window's edge, and the page's
    // title bar is the window's. macOS keeps its traffic lights inside it;
    // Windows and Linux get caption buttons drawn by the app.
    final options = WindowOptions(
      size: const Size(1180, 780),
      minimumSize: const Size(720, 560),
      center: true,
      backgroundColor: Colors.transparent,
      title: 'pb-mapper',
      titleBarStyle: TitleBarStyle.hidden,
      windowButtonVisibility: Platform.isMacOS,
    );
    unawaited(
      windowManager.waitUntilReadyToShow(options, () async {
        await windowManager.show();
        await windowManager.focus();
      }),
    );
  }
  PbMapperService().initLogging();
  PbMapperService().initChangeEvents();
  // Buffer the Rust log from boot, so the Logs page shows history however
  // late it is first opened.
  LogManager().initialize();
  await _sendAppDirectory();
  // Accept commands from other processes. Failing here is not fatal: the app
  // is still a window, and refusing to open because a pipe is unavailable
  // would be a poor trade.
  PbMapperService().startControlServer();
  runApp(
    PbMapperApp(api: PbMapperApi(), desktop: _isDesktop, checkForUpdates: true),
  );
}

/// Mobile platforms keep their config under the app's own documents
/// directory; Rust waits for it before reading anything. Desktop sends an
/// empty path, which means "use the platform config directory".
Future<void> _sendAppDirectory() async {
  var path = '';
  if (Platform.isAndroid || Platform.isIOS) {
    try {
      path = (await getApplicationDocumentsDirectory()).path;
    } catch (e) {
      // An empty path still unblocks Rust; it falls back to its default.
      debugPrint('No documents directory: $e');
    }
  }
  await PbMapperService().setAppDirectoryPath(path);
}
