import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

/// The refresh rate of the display the app's window is on, which a desktop
/// viewer follows (the FFI's `display_refresh`): the computer it views is
/// asked for no more frames a second than that display shows.
///
/// Flutter's view reports its display's rate, and on macOS a metrics change
/// when the window moves to another screen or a screen's mode changes.
/// Elsewhere that is not enough: the Windows and Linux embedders move the
/// view to another display only when its size or scale changes, Linux's
/// never sees a mode change, and on Windows the rate is the mode's whole
/// hertz, 59 for a 59.94 Hz mode. There the runner's `refresh_hz`
/// (`pb_mapper/desktop_video`) answers the rate of the display the window is
/// on now, exactly, and its `display_changed` says when the window may have
/// moved to another monitor or a display's mode changed. A runner that does
/// says so in its `decoders` answer (`DesktopDecoders.windowRefresh`), so
/// nothing waits on a runner that never answers, such as a test's.
class DisplayRefresh {
  const DisplayRefresh({required this.follows});

  /// This platform's: followed on macOS, Windows and Linux. Android connects
  /// at 60 whatever its display reports, and iOS keeps its connect-time
  /// rate.
  factory DisplayRefresh.platform() => DisplayRefresh(
    follows: Platform.isMacOS || Platform.isWindows || Platform.isLinux,
  );

  /// The viewer follows the window's display as it changes.
  final bool follows;

  static const _channel = MethodChannel('pb_mapper/desktop_video');

  /// The longest [read] waits for the runner before it takes Flutter's
  /// rate.
  static const runnerWait = Duration(milliseconds: 200);

  /// The rate Flutter reports for the window's display; 60 where it says
  /// nothing usable.
  static double flutterHz() {
    try {
      final views = WidgetsBinding.instance.platformDispatcher.views;
      final hz = views.isEmpty ? 60.0 : views.first.display.refreshRate;
      return hz.isFinite && hz > 0 ? hz : 60;
    } catch (_) {
      return 60;
    }
  }

  /// The window's display's rate: the runner's where [runner] (it said it
  /// answers), within [runnerWait], else, and failing that, Flutter's.
  Future<double> read({required bool runner}) async {
    final flutter = flutterHz();
    if (!runner) return flutter;
    try {
      final hz = await _channel
          .invokeMethod<double>('refresh_hz')
          .timeout(runnerWait);
      return hz != null && hz.isFinite && hz > 1 ? hz : flutter;
    } catch (_) {
      return flutter;
    }
  }

  /// Calls [changed] whenever the window's display may refresh at another
  /// rate: Flutter's rate for it changed (a metrics change), the app came
  /// back to the front, or, where [runner], the runner said the window moved
  /// to another monitor or a display's mode changed. Returns what stops it.
  VoidCallback watch(VoidCallback changed, {required bool runner}) {
    final watch = _Watch(changed);
    WidgetsBinding.instance.addObserver(watch);
    if (runner) {
      if (_runnerWatches.isEmpty) _channel.setMethodCallHandler(_fromRunner);
      _runnerWatches.add(watch);
    }
    return () {
      WidgetsBinding.instance.removeObserver(watch);
      if (_runnerWatches.remove(watch) && _runnerWatches.isEmpty) {
        _channel.setMethodCallHandler(null);
      }
    };
  }

  /// The watches the runner's `display_changed` reaches.
  static final _runnerWatches = <_Watch>[];

  static Future<void> _fromRunner(MethodCall call) async {
    if (call.method != 'display_changed') {
      throw MissingPluginException('pb_mapper/desktop_video: ${call.method}');
    }
    for (final watch in List.of(_runnerWatches)) {
      watch.changed();
    }
  }
}

class _Watch with WidgetsBindingObserver {
  _Watch(this.changed) : _flutterHz = DisplayRefresh.flutterHz();

  final VoidCallback changed;

  /// What Flutter said last: a metrics change that leaves it, such as each
  /// step of a resize, calls nothing.
  double _flutterHz;

  @override
  void didChangeMetrics() {
    final hz = DisplayRefresh.flutterHz();
    if (hz == _flutterHz) return;
    _flutterHz = hz;
    changed();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) changed();
  }
}
