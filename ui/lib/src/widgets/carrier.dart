/// How this computer reaches the relay — TCP, or QUIC — as an icon, a chip,
/// and the one flow that changes it for the tunnels already running.
library;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart' show CarrierMove;
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';

/// The icon of a way to reach the relay: a stream per tunnel, or every
/// stream layered on one connection.
IconData carrierIcon(RelayTransport carrier) => switch (carrier) {
  RelayTransport.tcp => AppIcons.carrierTcp,
  RelayTransport.quic => AppIcons.carrierQuic,
};

/// [carrier] as a quiet pill. Neutral, like the relay's path: how the relay
/// is reached is not a state to watch.
class CarrierChip extends StatelessWidget {
  const CarrierChip({super.key, required this.carrier});

  final RelayTransport carrier;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      key: Key('carrier-${carrier.name}'),
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        color: scheme.outline.withValues(alpha: 0.13),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(carrierIcon(carrier), size: 13, color: scheme.onSurfaceVariant),
          const SizedBox(width: 5),
          Text(
            carrier.label,
            style: TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.w600,
              letterSpacing: 0.3,
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

/// Switch this computer to [to] for reaching the relay it uses, then say
/// how that went and offer to move the tunnels still on the old carrier.
///
/// What asked for it may well be gone by then, a control that offered the
/// carrier no longer offered, so the rest happens over the navigator.
Future<void> useCarrier(
  BuildContext context,
  AppStore store,
  RelayTransport to,
) async {
  final l10n = context.l10n;
  final here = Navigator.of(context).context;
  final from = store.config.relayTransport;
  final result = await store.saveRelay(
    address: store.relayAddress,
    credential: store.config.msgHeaderKey,
    keepAlive: store.config.keepAliveEnabled,
    relayTransport: to,
  );
  if (!here.mounted) return;
  if (!result.success) {
    showToast(here, result.message, kind: ToastKind.error);
    return;
  }
  if (store.relayState != RelayState.online) {
    showToast(
      here,
      l10n.relayTestFailed(store.relay?.error ?? l10n.relayNoReason),
      kind: ToastKind.warning,
    );
    return;
  }
  final move = CarrierMove.fromMap(result.data['move']);
  if (move.isEmpty) {
    showToast(here, l10n.carrierInUse(to.label), kind: ToastKind.success);
    return;
  }
  await offerCarrierMove(here, store, move, from: from, to: to);
}

/// Ask whether to start [move]'s tunnels again so they reach the relay over
/// [to], and do it on a yes. Offered only once the relay has answered over
/// [to]: moving tunnels to a carrier that does not get through would stop
/// them.
Future<void> offerCarrierMove(
  BuildContext context,
  AppStore store,
  CarrierMove move, {
  required RelayTransport from,
  required RelayTransport to,
}) async {
  final l10n = context.l10n;
  final here = Navigator.of(context).context;
  final now = await showDialog<bool>(
    context: here,
    builder: (c) => AlertDialog(
      key: const Key('carrier-move'),
      icon: Icon(carrierIcon(to)),
      title: Text(l10n.carrierMoveTitle(to.label)),
      content: SizedBox(
        width: 420,
        child: Text(l10n.carrierMoveBody(move.length, from.label, to.label)),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(c).pop(false),
          child: Text(l10n.carrierMoveLater),
        ),
        FilledButton(
          key: const Key('carrier-move-now'),
          onPressed: () => Navigator.of(c).pop(true),
          child: Text(l10n.carrierMoveNow),
        ),
      ],
    ),
  );
  if (now != true || !here.mounted) return;
  final failed = await store.moveCarrier(move);
  if (!here.mounted) return;
  if (failed.isEmpty) {
    showToast(
      here,
      l10n.carrierMoved(move.length, to.label),
      kind: ToastKind.success,
    );
  } else {
    showToast(
      here,
      l10n.carrierMoveFailed(failed.join(', ')),
      kind: ToastKind.warning,
    );
  }
}
