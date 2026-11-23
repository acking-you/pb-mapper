import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_pointer.dart';

/// The runner's `pb_mapper/desktop_pointer` as a test sees it: every call,
/// in order, as `create #n <w>x<h> @<x>,<y>`, `set #n` and `delete #n`, and
/// the cursors it holds. `#n` is the n-th id this fake saw, so a test reads
/// the same whatever ids earlier tests used.
class FakeDesktopPointer implements DesktopPointerCursors {
  final calls = <String>[];

  /// The cursors held, by `#n`.
  final held = <int, PointerBitmap>{};
  final _names = <int, int>{};

  /// Every call throws, as a runner that cannot make the cursor does.
  bool fail = false;

  int _name(int id) => _names.putIfAbsent(id, () => _names.length + 1);

  @override
  Future<void> create(int id, PointerBitmap bitmap) async {
    final n = _name(id);
    calls.add(
      'create #$n ${bitmap.width}x${bitmap.height} '
      '@${bitmap.hotX},${bitmap.hotY}',
    );
    if (fail) throw PlatformException(code: 'desktop_pointer');
    held[n] = bitmap;
  }

  @override
  Future<void> set(int id) async {
    final n = _name(id);
    calls.add('set #$n');
    if (fail || !held.containsKey(n)) {
      throw PlatformException(code: 'desktop_pointer');
    }
  }

  @override
  Future<void> delete(int id) async {
    final n = _name(id);
    calls.add('delete #$n');
    held.remove(n);
  }

  /// The calls since the last time this was asked.
  List<String> take() {
    final taken = List.of(calls);
    calls.clear();
    return taken;
  }
}
