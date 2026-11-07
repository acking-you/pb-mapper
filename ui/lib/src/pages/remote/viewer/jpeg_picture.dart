import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

/// The picture on platforms without a native decoder: the store's latest
/// decoded JPEG, stretched to the picture's box (the box has its aspect).
class JpegPicture extends StatelessWidget {
  const JpegPicture({super.key, required this.image});

  final ValueListenable<ui.Image?> image;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: image,
    builder: (context, image, _) => image == null
        ? const SizedBox.expand()
        : RawImage(
            image: image,
            fit: BoxFit.fill,
            filterQuality: FilterQuality.low,
          ),
  );
}
