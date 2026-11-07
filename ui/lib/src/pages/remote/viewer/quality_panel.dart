import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import 'viewer_overlays.dart';

/// The connection's figures, refreshed with the store's stats (at most
/// once a second, and sampled only while something like this listens).
class QualityPanel extends StatelessWidget {
  const QualityPanel({
    super.key = const Key('viewer-quality-panel'),
    required this.stats,
  });

  final ValueListenable<DesktopStats> stats;

  static String _codec(String codec) => switch (codec) {
    'h264' => 'H.264',
    '' => '—',
    _ => codec.toUpperCase(),
  };

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: stats,
    builder: (context, st, _) {
      final l10n = context.l10n;
      final path = viewerPathLabel(l10n, st.path);
      final transport = st.relayTransport;
      final codec = _codec(st.codec);
      final rows = <(String, String, bool, Color?)>[
        (
          l10n.qualityPath,
          path.isEmpty
              ? '—'
              : st.path == 'relay' && transport != null && transport.isNotEmpty
              ? '$path · ${transport.toUpperCase()}'
              : path,
          false,
          null,
        ),
        (
          l10n.qualityRtt,
          st.rttMs == null ? '—' : '${st.rttMs} ms',
          true,
          null,
        ),
        (
          l10n.qualityLatency,
          st.pictureLatencyMs == null ? '—' : '${st.pictureLatencyMs} ms',
          true,
          st.degraded ? ViewerPalette.caution : null,
        ),
        (l10n.qualityFps, '${st.presentedFps.round()} fps', true, null),
        (
          l10n.qualityBitrate,
          '${(st.receivedBitrateBps / 1e6).toStringAsFixed(1)} Mbps',
          true,
          null,
        ),
        (
          l10n.qualityResolution,
          st.width == 0 || st.height == 0 ? '—' : '${st.width} × ${st.height}',
          true,
          null,
        ),
        (
          l10n.qualityCodec,
          switch (st.hardwareDecoder) {
            true => l10n.qualityHardware(codec),
            false => l10n.qualitySoftware(codec),
            null => codec,
          },
          false,
          null,
        ),
      ];
      final footer = switch (st.adaptation) {
        DesktopAdaptation.loweringBitrate => l10n.qualityAdaptBitrate,
        DesktopAdaptation.loweringFps ||
        DesktopAdaptation.unresponsive => l10n.qualityAdaptFps,
        _ => null,
      };
      return Container(
        width: 236,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
        decoration: BoxDecoration(
          color: ViewerPalette.panel,
          borderRadius: BorderRadius.circular(14),
          border: Border.all(color: ViewerPalette.edge),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                const Icon(
                  AppIcons.quality,
                  size: AppIconSize.inline,
                  color: ViewerPalette.text,
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    l10n.viewerQuality,
                    style: const TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: ViewerPalette.text,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 4),
            for (final (label, value, isMono, tint) in rows)
              Padding(
                padding: const EdgeInsets.only(top: 7),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        label,
                        style: const TextStyle(
                          fontSize: 12.5,
                          color: ViewerPalette.textSecondary,
                        ),
                      ),
                    ),
                    Text(
                      value,
                      style: isMono
                          ? mono(
                              fontSize: 12.5,
                              color: tint ?? ViewerPalette.text,
                            )
                          : TextStyle(
                              fontSize: 12.5,
                              color: tint ?? ViewerPalette.text,
                            ),
                    ),
                  ],
                ),
              ),
            if (footer != null) ...[
              const SizedBox(height: 11),
              Text(
                footer,
                style: const TextStyle(
                  fontSize: 12.5,
                  height: 1.5,
                  color: ViewerPalette.textSecondary,
                ),
              ),
            ],
          ],
        ),
      );
    },
  );
}
