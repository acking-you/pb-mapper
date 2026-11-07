import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'fake_desktop_api.dart';
import 'remote_viewer_harness.dart';

void main() {
  testWidgets(
    'zoom stays local through pinch, reconnects and the soft keyboard',
    (tester) async {
      final h = await ViewerHarness.live(tester, mode: TouchMode.direct);
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: 400,
              height: 300,
              child: RemoteSurface(
                viewport: h.viewport,
                input: h.input,
                machine: h.machine,
                touchMode: h.touchMode,
                child: const Texture(textureId: 17),
              ),
            ),
          ),
        ),
      );
      await tester.pump();
      final texture = tester.element(find.byType(Texture));
      Matrix4 transform() => tester
          .widget<Transform>(find.byKey(const Key('remote-scene-transform')))
          .transform;
      // A 16:9 picture fitted into 400×300: 400×225 at y 37.5.
      expect(h.viewport.scene, const Size(400, 225));
      h.sent();

      // A pinch zooms locally: nothing reaches the host.
      final a = await tester.startGesture(const Offset(150, 150), pointer: 1);
      final b = await tester.startGesture(const Offset(250, 150), pointer: 2);
      await a.moveTo(const Offset(100, 150));
      await b.moveTo(const Offset(300, 150));
      await a.up();
      await b.up();
      await tester.pump();
      expect(h.sent(), isEmpty);
      expect(h.viewport.scale, greaterThan(1.2));
      expect(tester.element(find.byType(Texture)), same(texture));
      final zoomed = (h.viewport.scale, h.viewport.offset, transform());

      // A later direct tap lands where the zoomed picture shows it.
      await tester.tapAt(h.viewport.toLocal(const Offset(0.375, 0.625)));
      expect(h.sent(), [
        ['move 720,675', 'left↓', 'left↑'],
      ]);

      // A reconnect (new generation, new geometry epoch) keeps the view.
      final epoch = h.store.geometry.value!.epoch;
      h.api.viewerStatusValue = liveStatus(controlling: true, generation: 2);
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.store.geometry.value!.epoch, isNot(epoch));
      expect((h.viewport.scale, h.viewport.offset, transform()), zoomed);
      expect(tester.element(find.byType(Texture)), same(texture));

      // The soft keyboard occludes: the scene and the scale stay, the
      // cursor is followed into view, and closing it restores the view.
      final scene = h.viewport.scene;
      h.viewport.beginKeyboard();
      h.viewport.setOcclusion(const EdgeInsets.only(bottom: 200));
      h.viewport.ensureVisible(const Offset(0.5, 0.9));
      await tester.pump();
      expect(h.viewport.scene, scene);
      expect(h.viewport.scale, zoomed.$1);
      expect(h.viewport.offset, isNot(zoomed.$2));
      expect(tester.element(find.byType(Texture)), same(texture));
      h.viewport.endKeyboard();
      await tester.pump();
      expect((h.viewport.scale, h.viewport.offset, transform()), zoomed);

      // Unless the user panned meanwhile: then the pan stays.
      h.viewport.beginKeyboard();
      h.viewport.setOcclusion(const EdgeInsets.only(bottom: 200));
      h.viewport.panBy(const Offset(0, -40));
      h.viewport.endKeyboard();
      await tester.pump();
      expect(h.viewport.scale, zoomed.$1);
      expect(h.viewport.offset, zoomed.$2 + const Offset(0, -40));
      expect(tester.element(find.byType(Texture)), same(texture));

      h.dispose();
    },
  );
}
