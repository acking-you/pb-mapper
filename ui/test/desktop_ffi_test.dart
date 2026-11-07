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
  test('the view leaf and video meta mirror the 56-byte C layouts', () {
    expect(sizeOf<PbDesktopViewV1>(), 56);
    expect(sizeOf<PbDesktopVideoMetaV1>(), 56);

    final leaf = calloc<PbDesktopViewV1>();
    final meta = calloc<PbDesktopVideoMetaV1>();
    try {
      leaf.cast<Uint8>().asTypedList(56).buffer.asByteData()
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
        ..setUint8(53, 0x13);
      expect(
        desktopViewFromLeaf(leaf.ref),
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
          state: 2,
          cursorVisible: true,
          controlling: true,
          fresh: true,
        ),
      );

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
