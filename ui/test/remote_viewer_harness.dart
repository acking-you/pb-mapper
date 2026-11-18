import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_input.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_gestures.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'fake_desktop_api.dart';

/// The viewer's input core over a store whose session is live and in
/// control of a 1920×1080 display, wired as a viewer wires it.
class ViewerHarness {
  ViewerHarness._(this.api, this.store, this._demand, {required TouchMode mode})
    : touchMode = ValueNotifier(mode) {
    _open();
  }

  void _open() {
    final input = this.input = RemoteInput(
      desktop: store,
      latch: latch,
      onLocalCancel: () => machine.cancel(this.input.clock()),
    );
    machine = TouchGestureMachine(
      sink: input,
      viewport: viewport,
      mode: () => touchMode.value,
      interactive: () => input.ready.value,
      haptic: (_) {},
    );
  }

  /// The viewer closes and opens again over the same session, as Back and
  /// reopening it do: a new input core and touch machine.
  void reopen() {
    input.dispose();
    machine.dispose();
    _open();
  }

  /// [capabilities] are the host's.
  static Future<ViewerHarness> live(
    WidgetTester tester, {
    TouchMode mode = TouchMode.trackpad,
    List<String> capabilities = const [],
  }) async {
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    final demand = store.demand(DesktopDemand.viewer);
    await store.connect(
      const DesktopTarget(invitation: 'pbd1.viewer', name: 'Viewer'),
    );
    api.viewerStatusValue = liveStatus(
      controlling: true,
      capabilities: capabilities,
    );
    await tester.pump(const Duration(milliseconds: 500));
    expect(store.session.value.inputLive, isTrue);
    final harness = ViewerHarness._(api, store, demand, mode: mode);
    harness.viewport.aspectRatio = 1920 / 1080;
    expect(harness.input.ready.value, isTrue);
    return harness;
  }

  final FakeDesktopApi api;
  final DesktopStore store;
  final DesktopDemandHandle _demand;
  final latch = ModifierLatch();
  final viewport = RemoteViewport();
  final ValueNotifier<TouchMode> touchMode;
  late RemoteInput input;
  late TouchGestureMachine machine;

  int _seen = 0;

  /// The batches the store handed to Rust since the last call, each as
  /// readable events: `control↓`, `left↑`, `move 960,540`, `text "hi"`,
  /// `#e1↓` (a physical key, by its USB HID usage).
  List<List<String>> sent() {
    final batches = api.argsOf('input');
    final fresh = batches.skip(_seen).toList();
    _seen = batches.length;
    return [
      for (final batch in fresh)
        [for (final e in batch['inputs']! as List) describe(e as Map)],
    ];
  }

  static String describe(Map<dynamic, dynamic> e) => switch (e['kind']) {
    'key' => '${e['key']}${e['down'] == true ? '↓' : '↑'}',
    'button' =>
      '${e['button']}${e['down'] == true ? '↓' : '↑'}'
          '${e['drag_start'] == true ? ' drag' : ''}',
    'move' => 'move ${e['x']},${e['y']}',
    'wheel' => 'wheel ${e['horizontal']},${e['vertical']}',
    'text' => 'text "${e['text']}"',
    'physical_key' =>
      '#${(e['usage'] as int).toRadixString(16)}'
          '${e['down'] == true ? '↓' : '↑'}',
    final kind => '$kind',
  };

  /// How many times the store asked Rust to release everything.
  int get releases => api.ops.where((op) => op == 'release').length;

  void dispose() {
    input.dispose();
    machine.dispose();
    viewport.dispose();
    latch.dispose();
    touchMode.dispose();
    _demand.dispose();
    store.dispose();
  }
}
