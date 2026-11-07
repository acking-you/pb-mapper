import 'package:flutter/material.dart';
import '../common/l10n_extension.dart';
import '../state/desktop_store.dart';

/// This occupies stable space while the image/texture keeps rendering below it.
class DesktopConnectionBanner extends StatelessWidget {
  const DesktopConnectionBanner({super.key, required this.store});
  final DesktopStore store;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: store.session,
    builder: (context, session, _) {
      final l = context.l10n;
      final phase = session.phase;
      final label = switch (phase) {
        DesktopPhase.idle => l.desktopStateIdle,
        DesktopPhase.connecting when session.reconnectAttempt > 0 =>
          l.desktopStateReconnecting,
        DesktopPhase.connecting => l.desktopStateConnecting,
        DesktopPhase.live => l.desktopStateLive,
        DesktopPhase.recovering => l.desktopStateRecovering,
        DesktopPhase.waitingForDesktop => l.desktopStateWaitingDesktop,
        DesktopPhase.paused => l.desktopStale,
        DesktopPhase.reconnecting => l.desktopStateRetry,
        DesktopPhase.identityChanged => l.desktopIdentityMismatch,
        DesktopPhase.ended => l.desktopStopped,
      };
      final detail = phase == DesktopPhase.identityChanged
          ? l.desktopIdentityMismatchHelp
          : session.secureDesktop && phase == DesktopPhase.waitingForDesktop
          ? l.desktopOrdinaryOnlyHelp
          : session.failure?.toString() ?? '';
      return Container(
        key: const Key('desktop-connection-state'),
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        color: Theme.of(context).colorScheme.surfaceContainer,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                SizedBox(
                  width: 14,
                  height: 14,
                  child: phase == DesktopPhase.live
                      ? Icon(
                          Icons.check_circle,
                          size: 14,
                          color: Theme.of(context).colorScheme.primary,
                        )
                      : phase == DesktopPhase.idle ||
                            phase == DesktopPhase.ended ||
                            phase == DesktopPhase.identityChanged
                      ? const Icon(Icons.link_off, size: 14)
                      : const CircularProgressIndicator(strokeWidth: 2),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
                if (session.retryAt case final at?) _RetryCountdown(at: at),
              ],
            ),
            ValueListenableBuilder(
              valueListenable: store.stats,
              builder: (context, stats, _) => Text(
                [
                  if (session.path case final path?) path,
                  if (stats.presentedFps > 0)
                    '${stats.presentedFps.toStringAsFixed(0)} fps',
                  if (stats.pictureLatencyMs case final latency?)
                    '${l.desktopAckLatency} $latency ms',
                  if (session.reconnectAttempt > 0)
                    '${l.desktopRecoveryAttempt} ${session.reconnectAttempt}',
                ].join(' · '),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
            SizedBox(
              height: 18,
              child: Text(
                detail,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
          ],
        ),
      );
    },
  );
}

class _RetryCountdown extends StatelessWidget {
  const _RetryCountdown({required this.at});
  final DateTime at;

  @override
  Widget build(BuildContext context) {
    final left = at.difference(DateTime.now());
    final seconds = left.isNegative ? 0 : (left.inMilliseconds / 1000).ceil();
    return Text('$seconds s');
  }
}
