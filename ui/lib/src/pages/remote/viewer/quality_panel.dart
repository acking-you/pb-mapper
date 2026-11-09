import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_store.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import '../remote_format.dart';

/// A codec as people write it; never localised.
String codecLabel(String codec) => switch (codec) {
  'h264' => 'H.264',
  'h265' => 'H.265',
  '' => '—',
  _ => codec.toUpperCase(),
};

/// The decoded picture's chroma: the video codecs are 4:2:0 (Main
/// profile); JPEG's is not reported.
String? chromaOf(String codec) =>
    codec == 'h264' || codec == 'h265' ? '4:2:0' : null;

String _mbps(int bps) => '${(bps / 1e6).toStringAsFixed(1)} Mbps';

String _ms(int? ms) => ms == null ? '—' : '$ms ms';

/// One figure of the panel; [id] keys its row (`quality-<id>`).
class _Row {
  const _Row(
    this.id,
    this.label,
    this.value, {
    this.detail,
    this.mono = true,
    this.tint,
  });

  final String id;
  final String label;
  final String value;

  /// Smaller, under the value: what it is made of, or its bound.
  final String? detail;
  final bool mono;
  final Color? tint;
}

/// The connection's figures, refreshed with the store's stats (at most
/// once a second, and sampled only while something like this listens), in
/// four groups: the network, the picture, the host's pacing and, where a
/// native decoder reports them, the decoder's own figures. Its core rows
/// show dashes while the connection is not up; a row whose figure is not
/// reported (an older host, the relay, a platform without the key) is left
/// out. It scrolls within the height it is given.
class QualityPanel extends StatelessWidget {
  const QualityPanel({
    super.key = const Key('viewer-quality-panel'),
    required this.session,
    required this.stats,
    this.decoder,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;

  /// The native decoder's figures; read only while this panel shows.
  final ValueListenable<DesktopDecoderStats?>? decoder;

  static const width = 256.0;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([session, stats, ?decoder]),
    builder: (context, _) {
      final s = session.value;
      final st = s.connected ? stats.value : DesktopStats.none;
      final dec = s.connected ? decoder?.value : null;
      final sections = [
        (context.l10n.qualityNetwork, _network(context, s, st)),
        (context.l10n.qualityPicture, _picture(context, s, st)),
        if (s.connected) (context.l10n.qualityHost, _host(context, s, st)),
        if (dec != null) (context.l10n.qualityDecoder, _decoder(context, dec)),
      ];
      final footer = switch (st.adaptation) {
        DesktopAdaptation.loweringBitrate => context.l10n.qualityAdaptBitrate,
        DesktopAdaptation.loweringFps ||
        DesktopAdaptation.unresponsive => context.l10n.qualityAdaptFps,
        _ => null,
      };
      return Container(
        width: width,
        decoration: BoxDecoration(
          color: ViewerPalette.panel,
          borderRadius: BorderRadius.circular(14),
          border: Border.all(color: ViewerPalette.edge),
        ),
        child: SingleChildScrollView(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _header(context),
              for (final (title, rows) in sections)
                if (rows.isNotEmpty) ...[
                  _SectionTitle(title),
                  for (final row in rows) _RowView(row),
                ],
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
        ),
      );
    },
  );

  Widget _header(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 4),
    child: Row(
      children: [
        const Icon(
          AppIcons.quality,
          size: AppIconSize.inline,
          color: ViewerPalette.text,
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            context.l10n.viewerQuality,
            style: const TextStyle(
              fontSize: 13,
              fontWeight: FontWeight.w600,
              color: ViewerPalette.text,
            ),
          ),
        ),
      ],
    ),
  );

  /// The path in full: a LAN or an internet direct path with its carrier
  /// when that is TCP and its address family, or the relay with its
  /// carrier.
  static String _path(
    BuildContext context,
    DesktopSessionView s,
    DesktopStats st,
  ) {
    final l10n = context.l10n;
    final link = st.link;
    final transport = st.relayTransport;
    final family = switch (link?.family) {
      'ipv4' => 'IPv4',
      'ipv6' => 'IPv6',
      _ => null,
    };
    return switch (s.path) {
      'direct' => [
        st.lan ? l10n.qualityPathLan : l10n.pathDirect,
        if (st.tcp) 'TCP',
        ?family,
      ].join(' · '),
      'relay' when transport != null && transport.isNotEmpty =>
        '${l10n.pathRelay} · ${transport.toUpperCase()}',
      _ => pathLabel(context, s.path),
    };
  }

  static List<_Row> _network(
    BuildContext context,
    DesktopSessionView s,
    DesktopStats st,
  ) {
    final l10n = context.l10n;
    final link = st.link;
    // TCP's figures stay in the kernel: its link has only the handshake's
    // round trip, which is no measure of the network now.
    final linkRtt = st.tcp ? null : link?.rttMs, lowest = link?.minRttMs;
    final queued = st.queueDelayMs ?? 0;
    final sent = st.sentBitrateBps;
    final (down, up) = (st.speedRxBps, st.speedTxBps);
    return [
      _Row('path', l10n.qualityPath, _path(context, s, st), mono: false),
      if (linkRtt != null)
        _Row(
          'link-rtt',
          l10n.qualityRtt,
          _ms(linkRtt),
          detail: lowest == null ? null : l10n.qualityLowest(lowest),
        ),
      _Row(
        'stream-rtt',
        l10n.qualityStreamRtt,
        _ms(st.rttMs),
        detail: queued > 0 ? l10n.qualityQueued(queued) : null,
      ),
      // Everything the session moves, each way, as RustDesk counts it.
      if (!s.connected || down != null)
        _Row(
          'speed',
          l10n.qualitySpeed,
          down == null ? '—' : formatSpeed(down),
          detail: up == null ? null : l10n.qualitySpeedSent(formatSpeed(up)),
        ),
      _Row(
        'bitrate',
        l10n.qualityBitrate,
        s.connected ? _mbps(st.receivedBitrateBps) : '—',
      ),
      if (sent != null)
        _Row('sent-bitrate', l10n.qualitySentBitrate, _mbps(sent)),
    ];
  }

  static List<_Row> _picture(
    BuildContext context,
    DesktopSessionView s,
    DesktopStats st,
  ) {
    final l10n = context.l10n;
    final codec = codecLabel(st.codec);
    final chroma = chromaOf(st.codec);
    final (host, viewer) = (st.hostMs, st.viewerMs);
    String fps(double value) => s.connected ? '${value.round()} fps' : '—';
    return [
      _Row(
        'latency',
        l10n.qualityLatency,
        _ms(st.pictureLatencyMs),
        detail: host != null && viewer != null
            ? l10n.qualityLatencySplit(host, viewer)
            : null,
        tint: st.degraded ? ViewerPalette.caution : null,
      ),
      _Row('presented-fps', l10n.qualityFps, fps(st.presentedFps)),
      _Row('received-fps', l10n.qualityReceivedFps, fps(st.receivedFps)),
      _Row('capture-fps', l10n.qualityCaptureFps, fps(st.captureFps)),
      _Row(
        'resolution',
        l10n.qualityResolution,
        st.width == 0 || st.height == 0 ? '—' : '${st.width} × ${st.height}',
      ),
      _Row('codec', l10n.qualityCodec, switch (st.hardwareDecoder) {
        true => l10n.qualityHardware(codec),
        false => l10n.qualitySoftware(codec),
        null => codec,
      }, mono: false),
      if (chroma != null) _Row('chroma', l10n.qualityChroma, chroma),
      if (s.connected)
        _Row(
          'keyframes',
          l10n.qualityKeyframes,
          '${st.keyframes}',
          detail: st.keyframesRequested > 0
              ? l10n.qualityRequested(st.keyframesRequested)
              : null,
        ),
    ];
  }

  /// What the host was asked for and what it does: Quality's ceiling of 30
  /// is by design, so its 30 reads as expected beside it, unless the user
  /// chose a frame rate, which replaces it.
  static List<_Row> _host(
    BuildContext context,
    DesktopSessionView s,
    DesktopStats st,
  ) {
    final l10n = context.l10n;
    final preset = st.preset;
    final ceiling = st.ceilingFps(chosen: s.target?.maxFps ?? 0);
    final skipped = st.skippedPerSecond, dwell = st.encoderDwellMs;
    return [
      if (preset != null)
        _Row(
          'preset',
          l10n.viewerQualityLabel,
          switch (preset) {
            DesktopQuality.quality => l10n.viewerQualityBest,
            DesktopQuality.balanced => l10n.viewerQualityBalanced,
            DesktopQuality.smooth => l10n.viewerQualitySmooth,
          },
          detail: ceiling == null ? null : l10n.qualityCeiling(ceiling),
          mono: false,
        ),
      if (st.paceFps > 0 || st.targetFps > 0)
        _Row(
          'pace',
          l10n.qualityPace,
          '${st.paceFps} / ${st.targetFps} fps',
          detail: st.fpsCap > 0 ? l10n.qualityCapped(st.fpsCap) : null,
        ),
      if (skipped != null)
        _Row(
          'skipped',
          l10n.qualitySkipped,
          '${skipped.toStringAsFixed(1)} /s',
        ),
      if (dwell != null) _Row('dwell', l10n.qualityDwell, _ms(dwell)),
      if (st.encoderBitrateBps > 0)
        _Row(
          'encoder',
          l10n.qualityEncoder,
          _mbps(st.encoderBitrateBps),
          detail: st.rateModel.isEmpty ? null : st.rateModel,
        ),
      _Row(
        'adaptation',
        l10n.qualityAdaptation,
        l10n.qualityAdaptationState(st.adaptation.name),
        mono: false,
      ),
    ];
  }

  static List<_Row> _decoder(BuildContext context, DesktopDecoderStats d) {
    final l10n = context.l10n;
    final (decoded, dropped) = (d.decodedPerSecond, d.droppedPerSecond);
    final (surface, credits) = (d.surfaceConsumeMs, d.credits);
    final (lowLatency, refresh) = (d.lowLatency, d.refreshHz);
    return [
      if (decoded != null)
        _Row('decoded', l10n.qualityDecoded, '${decoded.round()} /s'),
      if (dropped != null)
        _Row('dropped', l10n.qualityDropped, '${dropped.round()} /s'),
      if (surface != null)
        _Row(
          'surface',
          l10n.qualitySurface,
          '${surface.toStringAsFixed(1)} ms',
        ),
      if (credits != null) _Row('credits', l10n.qualityCredits, '$credits'),
      if (lowLatency != null)
        _Row(
          'low-latency',
          l10n.qualityLowLatency,
          lowLatency ? l10n.qualityOn : l10n.qualityOff,
          mono: false,
        ),
      if (refresh != null)
        _Row('refresh', l10n.qualityRefresh, '${refresh.round()} Hz'),
    ];
  }
}

/// RustDesk's quality monitor (D11): the few figures that say at a glance
/// how the session goes, small and translucent at the picture's top right,
/// in the viewer's dark palette. It takes no pointer, so a stroke may start
/// under it, and shows dashes while the connection is not up. Listening to
/// [stats] is what samples them, as the panel's does.
class QualityMonitor extends StatelessWidget {
  const QualityMonitor({
    super.key = const Key('viewer-quality-monitor'),
    required this.session,
    required this.stats,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;

  static const width = 196.0;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([session, stats]),
    builder: (context, _) {
      final l10n = context.l10n;
      final s = session.value;
      final st = s.connected ? stats.value : DesktopStats.none;
      final speed = st.speedRxBps;
      final rows = [
        (l10n.qualitySpeed, speed == null ? '—' : formatSpeed(speed)),
        (l10n.monitorFps, s.connected ? '${st.presentedFps.round()}' : '—'),
        (l10n.monitorDelay, _ms(st.rttMs)),
        (
          l10n.monitorTarget,
          st.encoderBitrateBps > 0 ? _mbps(st.encoderBitrateBps) : '—',
        ),
        (l10n.qualityCodec, codecLabel(st.codec)),
        (l10n.qualityChroma, chromaOf(st.codec) ?? '—'),
        (
          l10n.qualityResolution,
          st.width > 0 && st.height > 0 ? '${st.width}×${st.height}' : '—',
        ),
      ];
      return IgnorePointer(
        child: Semantics(
          container: true,
          label: l10n.viewerQualityMonitor,
          child: Container(
            width: width,
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            decoration: const BoxDecoration(
              // Translucent, yet its quietest text keeps 4.5:1 over white.
              color: ViewerPalette.gutter,
              borderRadius: BorderRadius.all(Radius.circular(8)),
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                for (final (label, value) in rows)
                  Padding(
                    padding: const EdgeInsets.symmetric(vertical: 1.5),
                    child: Row(
                      children: [
                        Expanded(
                          flex: 6,
                          child: Text(
                            label,
                            textAlign: TextAlign.end,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              fontSize: 11,
                              color: ViewerPalette.textSecondary,
                            ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          flex: 5,
                          child: Text(
                            value,
                            maxLines: 1,
                            style: mono(
                              fontSize: 11,
                              color: ViewerPalette.text,
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
              ],
            ),
          ),
        ),
      );
    },
  );
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 14, bottom: 1),
    child: Semantics(
      header: true,
      child: Text(
        title,
        style: const TextStyle(
          fontSize: 11,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.4,
          color: ViewerPalette.textQuiet,
        ),
      ),
    ),
  );
}

class _RowView extends StatelessWidget {
  _RowView(this.row) : super(key: Key('quality-${row.id}'));

  final _Row row;

  static const _valueMax = 140.0;

  @override
  Widget build(BuildContext context) {
    final color = row.tint ?? ViewerPalette.text;
    final detail = row.detail;
    return Padding(
      padding: const EdgeInsets.only(top: 7),
      child: MergeSemantics(
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Text(
                row.label,
                style: const TextStyle(
                  fontSize: 12.5,
                  color: ViewerPalette.textSecondary,
                ),
              ),
            ),
            const SizedBox(width: 10),
            // The label keeps at least a third: a long value wraps instead.
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: _valueMax),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Text(
                    row.value,
                    textAlign: TextAlign.end,
                    style: row.mono
                        ? mono(fontSize: 12.5, color: color)
                        : TextStyle(fontSize: 12.5, color: color),
                  ),
                  if (detail != null)
                    Text(
                      detail,
                      textAlign: TextAlign.end,
                      style: const TextStyle(
                        fontSize: 11,
                        color: ViewerPalette.textQuiet,
                      ),
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
