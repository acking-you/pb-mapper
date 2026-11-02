import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_backdrop.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_painter.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'visible canvases own independent backdrops across resize and theme',
    () {
      final first = BackdropCache()..enabled = true;
      final second = BackdropCache()..enabled = true;
      addTearDown(first.dispose);
      addTearDown(second.dispose);
      void ready() {}
      final a = first.of(const Size(900, 600), true, 1, ready);
      final b = second.of(const Size(1200, 800), false, 2, ready);
      expect(first.of(const Size(900, 600), true, 1, ready), same(a));
      expect(second.of(const Size(1200, 800), false, 2, ready), same(b));
      final resized = first.of(const Size(800, 600), true, 1, ready);
      expect(resized, isNot(same(a)));
      expect(second.of(const Size(1200, 800), false, 2, ready), same(b));
      expect(
        first.of(const Size(800, 600), false, 1, ready),
        isNot(same(resized)),
      );
      first.enabled = false;
      expect(first.of(const Size(800, 600), false, 1, ready), isNull);
      expect(second.of(const Size(1200, 800), false, 2, ready), same(b));
    },
  );

  test(
    'unused label paragraphs are disposed and text styles remain distinct',
    () {
      final labels = LabelCache();
      addTearDown(labels.dispose);
      const normal = TextStyle(fontSize: 12);
      const strong = TextStyle(fontSize: 14, fontWeight: FontWeight.bold);
      labels.beginFrame();
      final old = labels.of('old', normal);
      final current = labels.of('current', normal);
      final emphasized = labels.of('current', strong);
      labels.endFrame();
      expect(current, isNot(same(emphasized)));
      labels.beginFrame();
      expect(labels.of('current', normal), same(current));
      labels.endFrame();
      expect(old.debugDisposed, isTrue);
      expect(emphasized.debugDisposed, isTrue);
      expect(current.debugDisposed, isFalse);
      labels.dispose();
      expect(current.debugDisposed, isTrue);
    },
  );

  testWidgets('a hidden canvas discards a pending backdrop', (tester) async {
    final cache = BackdropCache()..enabled = true;
    addTearDown(cache.dispose);
    var ready = 0;
    final backdrop = cache.of(const Size(800, 600), true, 1, () => ready++)!;
    cache.enabled = false;
    await tester.runAsync(() => backdrop.debugRendered);
    expect(ready, 0);
    expect(cache.of(const Size(800, 600), true, 1, () => ready++), isNull);
  });
}
