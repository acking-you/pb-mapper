import 'package:flutter/material.dart';
import '../common/l10n_extension.dart';
import '../state/desktop_store.dart';

/// This occupies stable space while the image/texture keeps rendering below it.
class DesktopConnectionBanner extends StatelessWidget {
  const DesktopConnectionBanner({super.key, required this.store});
  final DesktopStore store;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: store,
    builder: (context, _) {
      final l = context.l10n;
      final phase = store.connectionPhase;
      final status = store.snapshot.viewer;
      final active = phase == DesktopConnectionPhase.live;
      final label = switch (phase) {
        DesktopConnectionPhase.idle => l.desktopStateIdle,
        DesktopConnectionPhase.connecting => l.desktopStateConnecting,
        DesktopConnectionPhase.live => l.desktopStateLive,
        DesktopConnectionPhase.delayed => l.desktopStateDelayed,
        DesktopConnectionPhase.waitingForDesktop =>
          l.desktopStateWaitingDesktop,
        DesktopConnectionPhase.recoveringVideo => l.desktopStateRecovering,
        DesktopConnectionPhase.retryWaiting => l.desktopStateRetry,
        DesktopConnectionPhase.reconnecting => l.desktopStateReconnecting,
        DesktopConnectionPhase.identityMismatch => l.desktopIdentityMismatch,
      };
      final age = store.pictureAge;
      final message = status?.message ?? '';
      final needsHelper =
          message.contains('secure_desktop_requires_helper') ||
          message.contains('Secure/locked desktop unavailable');
      final detail = needsHelper
          ? l.desktopOrdinaryOnlyHelp
          : phase == DesktopConnectionPhase.identityMismatch
          ? l.desktopIdentityMismatchHelp
          : store.recoveryReason ?? store.error ?? message;
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
                  child: active
                      ? Icon(
                          Icons.check_circle,
                          size: 14,
                          color: Theme.of(context).colorScheme.primary,
                        )
                      : phase == DesktopConnectionPhase.idle ||
                            phase == DesktopConnectionPhase.identityMismatch
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
                if (phase == DesktopConnectionPhase.retryWaiting)
                  Text('${(store.retryIn.inMilliseconds / 1000).ceil()} s'),
              ],
            ),
            Text(
              [
                if (status?.path.isNotEmpty == true) status!.path,
                if (status?.codec == 'h264')
                  '${store.presentFps.toStringAsFixed(0)} fps',
                if (age != null)
                  '${l.desktopFrameAge} ${(age.inMilliseconds / 1000).toStringAsFixed(1)} s',
                if ((status?.frameAckMs ?? 0) > 0)
                  '${l.desktopAckLatency} ${status!.frameAckMs.round()} ms',
                if (store.reconnectAttempt > 0)
                  '${l.desktopRecoveryAttempt} ${store.reconnectAttempt}',
              ].join(' · '),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: Theme.of(context).textTheme.bodySmall,
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
