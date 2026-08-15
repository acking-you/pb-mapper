import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'pb_mapper_ffi.dart';

/// The command-line half of the binary, which lives entirely in Rust.
///
/// Dart holds no list of verbs and formats no output. Rust decides whether
/// argv is a command, runs it, prints the result and returns an exit code —
/// which keeps the vocabulary defined in exactly one place, and works around
/// the fact that Dart's stdout does not reach a console here at all. See
/// `docs/ui-cli-mode-spec.md`.
class CliEntry {
  /// Runs [args] as a command, or returns null if this is a normal launch.
  ///
  /// Called before `WidgetsFlutterBinding.ensureInitialized()`, so nothing
  /// Flutter-side is running yet and the caller can simply `exit`.
  static int? run(List<String> args) {
    if (args.isEmpty) return null;

    final ffi = PbMapperFFI();
    final argv = calloc<Pointer<Utf8>>(args.length);
    final allocated = <Pointer<Utf8>>[];
    try {
      for (var i = 0; i < args.length; i++) {
        final arg = args[i].toNativeUtf8();
        allocated.add(arg);
        argv[i] = arg;
      }
      final code = ffi.pbMapperCliMain(args.length, argv);
      return code == kNotACommand ? null : code;
    } catch (_) {
      // A library that will not load is a broken install, but a launch should
      // still get as far as the window, where the failure can be shown.
      return null;
    } finally {
      for (final arg in allocated) {
        calloc.free(arg);
      }
      calloc.free(argv);
    }
  }
}
