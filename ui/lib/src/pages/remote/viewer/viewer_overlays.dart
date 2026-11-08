import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../shell/app_scope.dart';
import '../../../state/desktop_store.dart';
import '../../../state/remote_computers.dart'
    show ComputerPresence, listedPresence;
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import '../remote_format.dart';
import 'remote_viewport.dart';
import 'viewer_menus.dart';
import 'viewer_toolbar.dart' show kToolbarInsetExpanded;

/// How long the control hint stays on screen.
const kHintDuration = Duration(seconds: 3);

/// What lies over the picture, by the session's phase (ui §7.3).
enum ViewerOverlayKind {
  none,
  connecting,
  connectRetry,
  retryWaiting,
  reconnecting,
  secureDesktop,
  waitingDesktop,
  recovering,
  identityChanged,
  ended,
}

@immutable
class ViewerOverlay {
  const ViewerOverlay(this.kind, {this.opaque = false, this.greyStill = false});

  final ViewerOverlayKind kind;

  /// Only once the session is dead: an identity change, or its end.
  final bool opaque;

  /// The last picture stands, greyed, while a reconnect is waited for.
  final bool greyStill;

  @override
  bool operator ==(Object other) =>
      other is ViewerOverlay &&
      other.kind == kind &&
      other.opaque == opaque &&
      other.greyStill == greyStill;
  @override
  int get hashCode => Object.hash(kind, opaque, greyStill);
}

/// Pure; the first match wins. Nothing opaque covers the picture while a
/// frame can still arrive: Android returns presentation credit only once
/// Flutter has consumed the texture, so a cover would stall the stream.
ViewerOverlay viewerOverlayFor(DesktopSessionView s) => switch (s.phase) {
  DesktopPhase.identityChanged => const ViewerOverlay(
    ViewerOverlayKind.identityChanged,
    opaque: true,
  ),
  DesktopPhase.ended => const ViewerOverlay(
    ViewerOverlayKind.ended,
    opaque: true,
  ),
  DesktopPhase.reconnecting =>
    s.hasPicture
        ? const ViewerOverlay(ViewerOverlayKind.retryWaiting, greyStill: true)
        : const ViewerOverlay(ViewerOverlayKind.connectRetry),
  DesktopPhase.connecting when s.reconnectAttempt > 0 =>
    s.hasPicture
        ? const ViewerOverlay(ViewerOverlayKind.reconnecting, greyStill: true)
        : const ViewerOverlay(ViewerOverlayKind.connectRetry),
  DesktopPhase.connecting => const ViewerOverlay(ViewerOverlayKind.connecting),
  DesktopPhase.waitingForDesktop => ViewerOverlay(
    s.secureDesktop
        ? ViewerOverlayKind.secureDesktop
        : ViewerOverlayKind.waitingDesktop,
  ),
  DesktopPhase.recovering ||
  DesktopPhase.paused => const ViewerOverlay(ViewerOverlayKind.recovering),
  DesktopPhase.live ||
  DesktopPhase.idle => const ViewerOverlay(ViewerOverlayKind.none),
};

/// The host shows this viewer its desktop but does not let it control.
bool showsViewOnly(DesktopSessionView s) =>
    s.target != null && !s.controlAllowed && s.connected;

/// The link as the toolbar's status chip and the touch quality button show
/// it: slow only while live, and the colour of its dot.
({bool degraded, Color dot}) viewerLink(DesktopSessionView s, DesktopStats st) {
  final live = s.phase == DesktopPhase.live;
  final degraded = live && st.degraded;
  return (
    degraded: degraded,
    dot: !live
        ? ViewerPalette.textQuiet
        : degraded
        ? ViewerPalette.caution
        : ViewerPalette.success,
  );
}

/// A ring that turns, in the viewer's signal blue.
class ViewerSpinner extends StatelessWidget {
  const ViewerSpinner({super.key, this.size = 16});

  final double size;

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: size,
    child: CircularProgressIndicator(
      strokeWidth: size >= 20 ? 2.5 : 2,
      color: ViewerPalette.signal,
      backgroundColor: Colors.white.withValues(alpha: 0.15),
      strokeCap: StrokeCap.round,
    ),
  );
}

/// A capsule button on the dark chrome: outlined, or filled white for the
/// card's one recommended action.
class ViewerButton extends StatelessWidget {
  const ViewerButton({
    super.key,
    required this.label,
    required this.onPressed,
    this.filled = false,
    this.height = 34,
  });

  final String label;
  final VoidCallback? onPressed;
  final bool filled;
  final double height;

  @override
  Widget build(BuildContext context) {
    final radius = BorderRadius.circular(height / 2);
    return Material(
      type: MaterialType.transparency,
      child: InkWell(
        onTap: onPressed,
        borderRadius: radius,
        hoverColor: filled
            ? Colors.black.withValues(alpha: 0.06)
            : Colors.white.withValues(alpha: 0.08),
        child: Ink(
          height: height,
          padding: const EdgeInsets.symmetric(horizontal: 14),
          decoration: BoxDecoration(
            color: filled ? Colors.white : null,
            borderRadius: radius,
            border: filled ? null : Border.all(color: ViewerPalette.outline),
          ),
          child: Center(
            widthFactor: 1,
            child: Text(
              label,
              maxLines: 1,
              style: TextStyle(
                fontSize: 13,
                fontWeight: filled ? FontWeight.w600 : FontWeight.w500,
                color: filled ? const Color(0xFF1D1D1F) : ViewerPalette.text,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A state card over the picture: connecting, waiting, ended.
class ViewerCard extends StatelessWidget {
  const ViewerCard({
    super.key,
    required this.title,
    this.leading,
    this.body,
    this.children = const [],
    this.actions = const [],
    this.width = 320,
    this.alert = false,
  });

  final String title;
  final Widget? leading;
  final String? body;
  final List<Widget> children;
  final List<Widget> actions;

  /// The most it takes; a narrow stage gets less.
  final double width;

  /// A security problem: the edge turns red.
  final bool alert;

  @override
  Widget build(BuildContext context) => Semantics(
    container: true,
    liveRegion: true,
    child: ConstrainedBox(
      constraints: BoxConstraints(maxWidth: width),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: ViewerPalette.card,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(
            color: alert
                ? ViewerPalette.danger.withValues(alpha: 0.35)
                : ViewerPalette.edge,
          ),
        ),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 22, vertical: 20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  if (leading case final leading?) ...[
                    leading,
                    const SizedBox(width: 10),
                  ],
                  Expanded(
                    child: Text(
                      title,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                        color: ViewerPalette.text,
                      ),
                    ),
                  ),
                ],
              ),
              if (body case final body?) ...[
                const SizedBox(height: 12),
                Text(
                  body,
                  style: const TextStyle(
                    fontSize: 13,
                    height: 1.6,
                    color: ViewerPalette.textSecondary,
                  ),
                ),
              ],
              for (final child in children) ...[
                const SizedBox(height: 14),
                child,
              ],
              if (actions.isNotEmpty) ...[
                const SizedBox(height: 16),
                Align(
                  alignment: AlignmentDirectional.centerEnd,
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    alignment: WrapAlignment.end,
                    children: actions,
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    ),
  );
}

/// A one-line capsule over the picture: a hint, a state, a notice.
class ViewerPill extends StatelessWidget {
  const ViewerPill({
    super.key,
    required this.text,
    this.leading,
    this.secondary,
    this.trailing,
    this.height = 32,
    this.tint,
  });

  final String text;
  final Widget? leading;
  final String? secondary;
  final Widget? trailing;
  final double height;
  final Color? tint;

  @override
  Widget build(BuildContext context) {
    final size = height >= 48
        ? 13.5
        : height >= 40
        ? 13.0
        : 12.5;
    final side = height >= 40 ? 16.0 : 12.0;
    final gap = height >= 48 ? 12.0 : (height >= 40 ? 10.0 : 8.0);
    return Semantics(
      container: true,
      liveRegion: true,
      child: Container(
        height: height,
        padding: EdgeInsetsDirectional.only(
          start: side,
          end: trailing != null && height >= 48 ? 8 : side,
        ),
        decoration: BoxDecoration(
          color: tint ?? ViewerPalette.hint,
          borderRadius: BorderRadius.circular(height / 2),
          border: Border.all(color: ViewerPalette.edge),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (leading case final leading?) ...[leading, SizedBox(width: gap)],
            Flexible(
              child: Text(
                text,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: size, color: ViewerPalette.text),
              ),
            ),
            if (secondary case final secondary?) ...[
              SizedBox(width: gap),
              Flexible(
                child: Text(
                  secondary,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    fontSize: size,
                    color: ViewerPalette.textSecondary,
                  ),
                ),
              ),
            ],
            if (trailing case final trailing?) ...[
              SizedBox(width: gap),
              trailing,
            ],
          ],
        ),
      ),
    );
  }
}

/// What lies between the picture and a state card: see-through while a
/// frame can arrive, the ground once the session is dead. A translucent
/// scrim lets every pointer through; only its card takes them.
class ViewerScrim extends StatelessWidget {
  const ViewerScrim.translucent({super.key}) : opaque = false;
  const ViewerScrim.opaque({super.key}) : opaque = true;

  final bool opaque;

  /// The ground at 72 %.
  static const translucentColor = Color(0xB80F0F10);

  @override
  Widget build(BuildContext context) => opaque
      ? const AbsorbPointer(
          child: ColoredBox(
            color: ViewerPalette.ground,
            child: SizedBox.expand(),
          ),
        )
      : const IgnorePointer(
          child: ColoredBox(color: translucentColor, child: SizedBox.expand()),
        );
}

/// Greys the last picture while a reconnect is awaited, as a sibling over
/// the picture's rect — never its parent, so the texture is not rebuilt.
/// A colour filter, not a blur.
class GreyStill extends StatelessWidget {
  const GreyStill({super.key, required this.session, required this.viewport});

  final ValueListenable<DesktopSessionView> session;
  final RemoteViewport viewport;

  /// Luminance at half brightness.
  static const _grey = ColorFilter.matrix(<double>[
    0.1063, 0.3576, 0.0361, 0, 0, //
    0.1063, 0.3576, 0.0361, 0, 0, //
    0.1063, 0.3576, 0.0361, 0, 0, //
    0, 0, 0, 1, 0, //
  ]);

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: session,
    builder: (context, s, _) {
      if (!viewerOverlayFor(s).greyStill) return const SizedBox.shrink();
      // A backdrop filter reaches as far as the nearest clip: the inner one
      // keeps it to the picture, the outer one to the stage.
      return IgnorePointer(
        child: ClipRect(
          child: ListenableBuilder(
            listenable: viewport,
            builder: (context, _) => Stack(
              children: [
                Positioned.fromRect(
                  rect: viewport.offset & viewport.scene * viewport.scale,
                  child: const ClipRect(
                    child: BackdropFilter(
                      filter: _grey,
                      child: SizedBox.expand(),
                    ),
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

/// The three steps of a connect: found, verified, first picture. A failed
/// connect marks the step it stopped at.
class ConnectSteps extends StatelessWidget {
  const ConnectSteps({
    super.key,
    required this.setup,
    required this.path,
    this.failed = false,
  });

  final DesktopSetupStep? setup;
  final String? path;
  final bool failed;

  static int _row(DesktopSetupStep? step) => switch (step) {
    null || DesktopSetupStep.finding => 0,
    DesktopSetupStep.verifying || DesktopSetupStep.authorizing => 1,
    DesktopSetupStep.firstPicture => 2,
  };

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final current = _row(setup);
    final found = switch (path) {
      'direct' when current > 0 => l10n.viewerStepFoundDirect,
      'relay' when current > 0 => l10n.viewerStepFoundRelay,
      _ => l10n.viewerStepFind,
    };
    final labels = [found, l10n.viewerStepIdentity, l10n.viewerStepFirstFrame];
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (var i = 0; i < labels.length; i++)
          Padding(
            padding: EdgeInsets.only(top: i == 0 ? 0 : 8),
            child: _step(labels[i], i < current, i == current),
          ),
      ],
    );
  }

  Widget _step(String label, bool done, bool here) {
    final Widget glyph;
    final Color ink;
    if (done) {
      glyph = const Icon(
        AppIcons.check,
        size: AppIconSize.inline,
        color: ViewerPalette.success,
      );
      ink = ViewerPalette.textSecondary;
    } else if (here && failed) {
      glyph = const Icon(
        AppIcons.close,
        size: AppIconSize.inline,
        color: ViewerPalette.danger,
      );
      ink = ViewerPalette.text;
    } else {
      glyph = _dot(here ? ViewerPalette.signal : ViewerPalette.outline);
      ink = here ? ViewerPalette.text : ViewerPalette.textQuiet;
    }
    return Row(
      children: [
        SizedBox.square(dimension: 16, child: Center(child: glyph)),
        const SizedBox(width: 8),
        Expanded(
          child: Text(label, style: TextStyle(fontSize: 13, color: ink)),
        ),
      ],
    );
  }

  static Widget _dot(Color color) => Container(
    width: 7,
    height: 7,
    decoration: BoxDecoration(color: color, shape: BoxShape.circle),
  );
}

/// The overlays over the picture, by phase (ui §7.3), plus the view-only
/// pill, the notice pill and, on desktop, the degraded pill. Listens to the
/// session; the degraded pill alone reads the stats, and a retry or an end
/// alone reads, through [AppScope], whether the relay lists the computer.
class ViewerOverlayLayer extends StatelessWidget {
  const ViewerOverlayLayer({
    super.key,
    required this.session,
    required this.stats,
    required this.title,
    required this.actions,
    this.compact = false,
    this.toolbarInset,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;
  final String title;
  final ViewerActions actions;

  /// A phone: the top pills sit 12 below the picture's top.
  final bool compact;

  /// Desktop: where the top pills go, below the toolbar or its handle.
  final ValueListenable<double>? toolbarInset;

  @override
  Widget build(BuildContext context) => Stack(
    fit: StackFit.expand,
    children: [
      ValueListenableBuilder(
        valueListenable: session,
        builder: (context, s, _) => Stack(
          fit: StackFit.expand,
          children: [
            ..._phase(context, s),
            if (showsViewOnly(s))
              const PositionedDirectional(
                start: 12,
                bottom: 34,
                child: _ViewOnlyPill(),
              ),
            if (s.notice case final notice?)
              Positioned(
                left: 12,
                right: 12,
                bottom: 64,
                child: IgnorePointer(
                  child: Center(
                    child: ViewerPill(
                      height: 40,
                      text: switch (notice.code) {
                        DesktopNoticeCode.locked => context.l10n.shortcutLocked,
                        DesktopNoticeCode.lockFailed =>
                          context.l10n.shortcutLockFailed(notice.message),
                      },
                    ),
                  ),
                ),
              ),
          ],
        ),
      ),
      if (toolbarInset case final inset?)
        _DegradedPill(session: session, stats: stats, inset: inset),
    ],
  );

  List<Widget> _phase(BuildContext context, DesktopSessionView s) {
    final l10n = context.l10n;
    final kind = viewerOverlayFor(s).kind;
    final key = Key('viewer-overlay-${kind.name}');
    final busy = s.failure?.code == 'HOST_BUSY';
    // A frame shorter than the card (a phone above its keyboard, a short
    // window) scrolls it rather than cutting it off.
    Widget centred(Widget card) => Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(16),
        child: KeyedSubtree(key: key, child: card),
      ),
    );
    final cancel = ViewerButton(
      label: l10n.cancel,
      onPressed: actions.onDisconnect,
    );
    // Whether the relay answers without listing the computer, read again as
    // its services change: AppScope notifies only when its store changes.
    Widget presence(Widget Function(bool notSharing) build) {
      final scope = AppScope.maybeOf(context);
      if (scope == null) return build(false);
      return ListenableBuilder(
        listenable: scope.store,
        builder: (context, _) => build(!busy && _notSharing(scope)),
      );
    }

    // A retry pill's second line, the same while waiting and while trying.
    String retryReason(bool notSharing) => busy
        ? l10n.viewerBusy
        : notSharing
        ? l10n.remoteOfflineToast(title)
        : l10n.viewerInputPaused;

    switch (kind) {
      case ViewerOverlayKind.none:
        return const [];
      case ViewerOverlayKind.connecting:
        return [
          const ViewerScrim.translucent(),
          centred(
            ViewerCard(
              leading: const ViewerSpinner(size: 20),
              title: l10n.viewerConnecting(title),
              actions: [cancel],
              children: [ConnectSteps(setup: s.setup, path: s.path)],
            ),
          ),
        ];
      case ViewerOverlayKind.connectRetry:
        final waiting = s.phase == DesktopPhase.reconnecting;
        return [
          const ViewerScrim.translucent(),
          centred(
            presence(
              (notSharing) => ViewerCard(
                leading: const ViewerSpinner(size: 20),
                title: l10n.viewerConnecting(title),
                actions: [
                  cancel,
                  if (waiting)
                    ViewerButton(
                      label: l10n.viewerRetryNow,
                      filled: true,
                      onPressed: actions.desktop.retryNow,
                    ),
                ],
                children: [
                  _RetryCountdown(
                    session: s,
                    builder: (context, line) =>
                        _quiet(line, ViewerPalette.text),
                  ),
                  if (busy) _quiet(l10n.viewerBusy),
                  if (notSharing)
                    KeyedSubtree(
                      key: const Key('viewer-retry-not-sharing'),
                      child: _quiet(l10n.remoteOfflineToast(title)),
                    ),
                ],
              ),
            ),
          ),
        ];
      case ViewerOverlayKind.retryWaiting:
        return [
          centred(
            presence(
              (notSharing) => _RetryCountdown(
                session: s,
                builder: (context, line) => ViewerPill(
                  height: 48,
                  tint: ViewerPalette.panel,
                  leading: const ViewerSpinner(size: 16),
                  text: line,
                  secondary: retryReason(notSharing),
                  trailing: ViewerButton(
                    label: l10n.viewerRetryNow,
                    filled: true,
                    onPressed: actions.desktop.retryNow,
                  ),
                ),
              ),
            ),
          ),
        ];
      case ViewerOverlayKind.reconnecting:
        return [
          IgnorePointer(
            child: centred(
              presence(
                (notSharing) => ViewerPill(
                  height: 48,
                  tint: ViewerPalette.panel,
                  leading: const ViewerSpinner(size: 16),
                  text: l10n.viewerReconnecting(s.reconnectAttempt),
                  secondary: retryReason(notSharing),
                ),
              ),
            ),
          ),
        ];
      case ViewerOverlayKind.secureDesktop || ViewerOverlayKind.waitingDesktop:
        final secure = kind == ViewerOverlayKind.secureDesktop;
        return [
          const ViewerScrim.translucent(),
          centred(
            ViewerCard(
              width: 360,
              leading: const Icon(
                AppIcons.secureDesktop,
                size: AppIconSize.nav,
                color: ViewerPalette.caution,
              ),
              title: secure ? l10n.viewerSecureTitle : l10n.viewerWaitingTitle,
              body: secure ? l10n.viewerSecureBody : l10n.viewerWaitingBody,
              children: [_Chip(l10n.viewerInputPaused)],
            ),
          ),
        ];
      case ViewerOverlayKind.recovering:
        final pill = IgnorePointer(
          child: Center(
            child: KeyedSubtree(
              key: key,
              child: ViewerPill(
                leading: const ViewerSpinner(size: 16),
                text: l10n.viewerRecovering,
              ),
            ),
          ),
        );
        final inset = toolbarInset;
        return [
          if (inset == null || compact)
            Positioned(left: 0, right: 0, top: 12, child: pill)
          else
            ValueListenableBuilder(
              valueListenable: inset,
              builder: (context, top, child) =>
                  Positioned(left: 0, right: 0, top: top, child: child!),
              child: pill,
            ),
        ];
      case ViewerOverlayKind.identityChanged:
        return [
          const ViewerScrim.opaque(),
          centred(
            ViewerCard(
              width: 380,
              alert: true,
              leading: const Icon(
                AppIcons.warning,
                size: AppIconSize.nav,
                color: ViewerPalette.danger,
              ),
              title: l10n.viewerIdentityTitle,
              body: l10n.viewerIdentityBody,
              actions: [
                ViewerButton(
                  label: l10n.viewerDisconnect,
                  onPressed: actions.onDisconnect,
                ),
                if (actions.onPasteNewInvite case final paste?)
                  ViewerButton(
                    label: l10n.viewerPasteNewInvite,
                    filled: true,
                    onPressed: paste,
                  ),
              ],
            ),
          ),
        ];
      case ViewerOverlayKind.ended:
        return [
          const ViewerScrim.opaque(),
          centred(presence((notSharing) => _ended(context, s, notSharing))),
        ];
    }
  }

  /// The relay answers and does not list the service the open computer
  /// shares under: it is not sharing, as `presenceOf` decides (D22). Of a
  /// computer opened through another relay or credential than this
  /// computer's now, that relay's list says nothing.
  static bool _notSharing(AppScope scope) {
    final open = scope.remote.sharing;
    final store = scope.store;
    return open != null &&
        listedPresence(
              service: open.service,
              relay: open.relay,
              namespace: open.namespace,
              relayState: store.relayState,
              relayAddress: store.relayAddress,
              credentialKeyId: store.credentialKeyId,
              desktopServices: store.desktopServiceNames,
            ) ==
            ComputerPresence.notSharing;
  }

  Widget _ended(BuildContext context, DesktopSessionView s, bool notSharing) {
    final l10n = context.l10n;
    final end = s.end;
    final code = s.failure?.code;
    final body = switch (end) {
      DesktopEndReason.hostStopped => l10n.viewerEndedStopped,
      DesktopEndReason.disconnectedByHost => l10n.viewerEndedByHost,
      DesktopEndReason.revoked => l10n.viewerEndedRevoked,
      DesktopEndReason.versionMismatch => l10n.viewerEndedVersion,
      DesktopEndReason.denied => l10n.viewerDenied,
      DesktopEndReason.invalidInvitation => l10n.addInvalid,
      DesktopEndReason.decoderFailed => l10n.viewerEndedDecoder,
      DesktopEndReason.unreachable when code == 'HOST_BUSY' =>
        l10n.viewerEndedBusy,
      DesktopEndReason.unreachable when notSharing => l10n.remoteOfflineToast(
        title,
      ),
      DesktopEndReason.unreachable => l10n.viewerEndedUnreachable,
      _ when code == 'VAULT_UNAVAILABLE' || code == 'NOT_FOUND' =>
        l10n.viewerVaultUnavailable,
      _ => switch (s.failure?.message) {
        final message? when message.isNotEmpty => message,
        _ => l10n.viewerEndedFailed,
      },
    };
    // The invitation itself is no good: connecting again cannot help.
    final hopeless = switch (end) {
      DesktopEndReason.versionMismatch ||
      DesktopEndReason.denied ||
      DesktopEndReason.revoked ||
      DesktopEndReason.invalidInvitation => true,
      _ => false,
    };
    final paste =
        end == DesktopEndReason.denied || end == DesktopEndReason.revoked
        ? actions.onPasteNewInvite
        : null;
    return ViewerCard(
      title: l10n.viewerEndedTitle,
      body: body,
      actions: [
        ViewerButton(label: l10n.back, onPressed: actions.onDisconnect),
        if (end == DesktopEndReason.decoderFailed)
          ViewerButton(
            label: l10n.retry,
            filled: true,
            onPressed: actions.desktop.retryNow,
          )
        else if (!hopeless && actions.onReconnect != null)
          ViewerButton(
            label: l10n.viewerReconnect,
            filled: true,
            onPressed: actions.onReconnect,
          ),
        if (paste != null)
          ViewerButton(
            label: l10n.viewerPasteNewInvite,
            filled: true,
            onPressed: paste,
          ),
      ],
      children: [
        if (!s.hasPicture)
          ConnectSteps(setup: s.setup, path: s.path, failed: true),
      ],
    );
  }

  static Widget _quiet(
    String text, [
    Color color = ViewerPalette.textSecondary,
  ]) => Text(text, style: TextStyle(fontSize: 13, color: color));
}

class _ViewOnlyPill extends StatelessWidget {
  const _ViewOnlyPill();

  @override
  Widget build(BuildContext context) => IgnorePointer(
    child: ViewerPill(
      key: const Key('viewer-view-only'),
      tint: ViewerPalette.chrome,
      leading: const Icon(
        AppIcons.viewOnly,
        size: AppIconSize.inline,
        color: ViewerPalette.text,
      ),
      text: context.l10n.viewerViewOnlyByHost,
    ),
  );
}

/// Input paused, as a small chip in a card.
class _Chip extends StatelessWidget {
  const _Chip(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Align(
    alignment: AlignmentDirectional.centerStart,
    child: Container(
      height: 24,
      padding: const EdgeInsets.symmetric(horizontal: 10),
      decoration: BoxDecoration(
        color: Colors.white.withValues(alpha: 0.08),
        borderRadius: BorderRadius.circular(12),
      ),
      child: Center(
        widthFactor: 1,
        child: Text(
          text,
          style: const TextStyle(fontSize: 12, color: ViewerPalette.text),
        ),
      ),
    ),
  );
}

/// The retry's line: the countdown to the next attempt, or the attempt in
/// progress. It rebuilds itself once a second while it counts down.
class _RetryCountdown extends StatefulWidget {
  const _RetryCountdown({required this.session, required this.builder});

  final DesktopSessionView session;
  final Widget Function(BuildContext context, String line) builder;

  @override
  State<_RetryCountdown> createState() => _RetryCountdownState();
}

class _RetryCountdownState extends State<_RetryCountdown> {
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _arm();
  }

  @override
  void didUpdateWidget(covariant _RetryCountdown oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.session.retryAt != widget.session.retryAt) _arm();
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  void _arm() {
    _timer?.cancel();
    _timer = widget.session.retryAt == null
        ? null
        : Timer.periodic(const Duration(seconds: 1), (_) => setState(() {}));
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final s = widget.session;
    final at = s.retryAt;
    if (s.phase != DesktopPhase.reconnecting || at == null) {
      return widget.builder(
        context,
        l10n.viewerReconnecting(s.reconnectAttempt),
      );
    }
    final left = at.difference(DateTime.now()).inMilliseconds;
    final seconds = left <= 0 ? 0 : (left / 1000).ceil();
    if (seconds == 0) _timer?.cancel();
    return widget.builder(
      context,
      l10n.viewerRetry(s.reconnectAttempt, seconds),
    );
  }
}

/// While the toolbar is folded away, a live but slow picture says so at
/// the top (ViewerStates: degraded).
class _DegradedPill extends StatelessWidget {
  const _DegradedPill({
    required this.session,
    required this.stats,
    required this.inset,
  });

  final ValueListenable<DesktopSessionView> session;
  final ValueListenable<DesktopStats> stats;
  final ValueListenable<double> inset;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([session, inset]),
    builder: (context, _) {
      final collapsed = inset.value < kToolbarInsetExpanded;
      if (!collapsed || session.value.phase != DesktopPhase.live) {
        return const SizedBox.shrink();
      }
      return ValueListenableBuilder(
        valueListenable: stats,
        builder: (context, st, _) {
          if (!st.degraded) return const SizedBox.shrink();
          return Positioned(
            left: 0,
            right: 0,
            top: inset.value,
            child: IgnorePointer(
              child: Center(
                child: ViewerPill(
                  height: 34,
                  tint: ViewerPalette.chrome,
                  leading: _dot(ViewerPalette.caution),
                  text: context.l10n.viewerSlow(
                    pathLabel(context, session.value.path, lan: st.lan),
                  ),
                  trailing: Text(
                    st.pictureLatencyMs == null
                        ? '—'
                        : '${st.pictureLatencyMs} ms',
                    style: mono(fontSize: 12, color: ViewerPalette.caution),
                  ),
                ),
              ),
            ),
          );
        },
      );
    },
  );

  static Widget _dot(Color color) => Container(
    width: 8,
    height: 8,
    decoration: BoxDecoration(color: color, shape: BoxShape.circle),
  );
}

/// "Keyboard and mouse go to …", for [kHintDuration] whenever control
/// starts, and when a viewer that already controls is shown. A notice
/// replaces it.
class ControlHint extends StatefulWidget {
  const ControlHint({super.key, required this.session, required this.title});

  final ValueListenable<DesktopSessionView> session;
  final String title;

  @override
  State<ControlHint> createState() => _ControlHintState();
}

class _ControlHintState extends State<ControlHint> {
  Timer? _timer;
  bool _controlling = false, _shown = false;

  @override
  void initState() {
    super.initState();
    widget.session.addListener(_changed);
    _controlling = widget.session.value.controlling;
    if (_controlling) _show();
  }

  @override
  void didUpdateWidget(covariant ControlHint oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.session != widget.session) {
      oldWidget.session.removeListener(_changed);
      widget.session.addListener(_changed);
    }
  }

  @override
  void dispose() {
    widget.session.removeListener(_changed);
    _timer?.cancel();
    super.dispose();
  }

  void _changed() {
    final s = widget.session.value;
    final controlling = s.controlling;
    if (controlling && !_controlling) _show();
    _controlling = controlling;
    if (!controlling && _shown) _hide();
    if (s.notice != null && _shown) _hide();
  }

  void _show() {
    _timer?.cancel();
    _timer = Timer(kHintDuration, _hide);
    if (!_shown) setState(() => _shown = true);
  }

  void _hide() {
    _timer?.cancel();
    _timer = null;
    if (_shown && mounted) setState(() => _shown = false);
  }

  @override
  Widget build(BuildContext context) {
    if (!_shown) return const SizedBox.shrink();
    final l10n = context.l10n;
    return IgnorePointer(
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0, end: 1),
        duration: Motion.of(context, Motion.medium),
        builder: (context, t, child) => Opacity(opacity: t, child: child),
        child: ViewerPill(
          key: const Key('viewer-control-hint'),
          height: 40,
          tint: ViewerPalette.chrome,
          leading: const Icon(
            AppIcons.keyboard,
            size: AppIconSize.inline,
            color: ViewerPalette.signal,
          ),
          text: l10n.viewerControlHint(widget.title),
          secondary: l10n.viewerControlHintSub,
        ),
      ),
    );
  }
}
