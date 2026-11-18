import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_ffi.dart';

void main() {
  // Rust writes these structs and Dart reads them. Rust and the Windows runner
  // pin the C offsets at compile time; this pins Dart's mirrors to the same
  // ones (perf 7.4, 7.5).
  test('the view leaf and video meta mirror the 96/56-byte C layouts', () {
    expect(sizeOf<PbDesktopViewV1>(), 96);
    expect(sizeOf<PbDesktopVideoMetaV1>(), 56);

    final leaf = calloc<PbDesktopViewV1>();
    final meta = calloc<PbDesktopVideoMetaV1>();
    try {
      final bytes = leaf.cast<Uint8>().asTypedList(96).buffer.asByteData()
        ..setUint64(0, 7, Endian.host)
        ..setUint64(8, 42, Endian.host)
        ..setUint64(16, 3, Endian.host)
        ..setUint32(24, 0xFFFFFFFF, Endian.host)
        ..setUint32(28, 2560, Endian.host)
        ..setUint32(32, 1440, Endian.host)
        ..setUint32(36, 1920, Endian.host)
        ..setUint32(40, 1080, Endian.host)
        ..setInt32(44, -5, Endian.host)
        ..setInt32(48, 300, Endian.host)
        ..setUint8(52, 2)
        // cursor visible, controlling, fresh
        ..setUint8(53, 0x13)
        ..setUint16(54, 0xBEEF, Endian.host);
      final solo = desktopViewFromLeaf(leaf.ref);
      expect(
        solo,
        const DesktopView(
          generation: 7,
          presentedFrameId: 42,
          cursorSeq: 3,
          presentedAgeMs: DesktopView.noAge,
          displayWidth: 2560,
          displayHeight: 1440,
          videoWidth: 1920,
          videoHeight: 1080,
          cursorX: -5,
          cursorY: 300,
          cursorShape: 0xBEEF,
          state: 2,
          cursorVisible: true,
          controlling: true,
          fresh: true,
        ),
      );
      expect(identical(solo.peerCursors, const <DesktopPeerCursor>[]), isTrue);

      bytes
        ..setUint64(56, 11, Endian.host)
        ..setInt32(64, 640, Endian.host)
        ..setInt32(68, 1280, Endian.host)
        ..setInt32(76, 360, Endian.host)
        ..setInt32(80, 720, Endian.host)
        ..setUint8(88, 2)
        ..setUint8(89, 1)
        ..setUint8(90, 3);
      final shared = desktopViewFromLeaf(leaf.ref);
      expect(shared.peerCursorSeq, 11);
      expect(shared.peerCursors, const [
        DesktopPeerCursor(color: 1, x: 640, y: 360),
        DesktopPeerCursor(color: 3, x: 1280, y: 720),
      ]);

      meta.cast<Uint8>().asTypedList(56).buffer.asByteData()
        ..setUint64(8, 9, Endian.host)
        ..setUint32(48, 1234, Endian.host)
        ..setUint32(52, 4, Endian.host);
      expect((meta.ref.frameId, meta.ref.len, meta.ref.flags), (9, 1234, 4));
    } finally {
      calloc.free(leaf);
      calloc.free(meta);
    }
  });
}
