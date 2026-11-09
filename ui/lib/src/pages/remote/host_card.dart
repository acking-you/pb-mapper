import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_qr.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/sharing_indicator.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// Where this computer stands as a host, for its card.
enum HostCardPhase {
  loading,
  unsupported,
  notEnabled,
  enabling,
  restarting,
  sharing,
  paused,
}

/// Pure: one host value in, one phase out. Resuming a configured host keeps
/// its card; only a first enable shows the setup steps while it runs.
HostCardPhase hostCardPhase(DesktopHostView v) => switch (v) {
  _ when !v.loaded => HostCardPhase.loading,
  _ when !v.supported => HostCardPhase.unsupported,
  _ when v.restarting => HostCardPhase.restarting,
  _ when v.enabling && !v.sharing && !v.configured => HostCardPhase.enabling,
  _ when v.sharing => HostCardPhase.sharing,
  _ when v.configured => HostCardPhase.paused,
  _ => HostCardPhase.notEnabled,
};

/// `desktop:` and [hostname] reduced to what a service name may hold
/// (`valid_service`): lowercase `[a-z0-9._-]`, at most 120 characters.
String defaultServiceName(String hostname) {
  final name = hostname.toLowerCase().replaceAll(RegExp(r'[^a-z0-9._-]'), '');
  if (name.isEmpty) return 'desktop:computer';
  return 'desktop:${name.length > 120 ? name.substring(0, 120) : name}';
}

/// What a desktop service name may follow `desktop:` with.
final _serviceSuffix = RegExp(r'^[A-Za-z0-9._:-]{1,120}$');

/// This computer as a host (HostSetup, Main): the setup steps until it is
/// enabled, then its invitation, who is viewing, and how it shares. It
/// listens to the host value alone.
class HostCard extends StatelessWidget {
  const HostCard({super.key});

  @override
  Widget build(BuildContext context) {
    final desktop = AppScope.of(context).store.desktop;
    return ValueListenableBuilder<DesktopHostView>(
      valueListenable: desktop.hostView,
      builder: (context, v, _) => switch (hostCardPhase(v)) {
        HostCardPhase.loading => _LoadingCard(view: v),
        HostCardPhase.unsupported => const SizedBox.shrink(),
        HostCardPhase.notEnabled ||
        HostCardPhase.enabling ||
        HostCardPhase.restarting => _SetupCard(view: v),
        HostCardPhase.sharing || HostCardPhase.paused => _SharingCard(view: v),
      },
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({
    required this.padding,
    required this.gap,
    required this.children,
  });

  final double padding, gap;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) => RaisedSurface(
    radius: kCardRadius,
    child: Padding(
      padding: EdgeInsets.all(padding),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final (i, child) in children.indexed) ...[
            if (i > 0) SizedBox(height: gap),
            child,
          ],
        ],
      ),
    ),
  );
}

/// The card's first row: the computer's tile, its title and a line under
/// it, and a status pill.
class _HostHeader extends StatelessWidget {
  const _HostHeader({
    required this.active,
    required this.subtitle,
    this.monoSubtitle = false,
    this.trailing,
  });

  /// Blue: this computer is shared, or can resume sharing.
  final bool active;
  final String subtitle;
  final bool monoSubtitle;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      children: [
        Container(
          width: 44,
          height: 44,
          decoration: BoxDecoration(
            color: active ? scheme.tertiaryContainer : cardFieldFill(scheme),
            borderRadius: BorderRadius.circular(12),
          ),
          child: Icon(
            AppIcons.remoteLarge,
            size: AppIconSize.tile,
            color: active ? signalColor(scheme) : scheme.onSurfaceVariant,
          ),
        ),
        const SizedBox(width: 14),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                context.l10n.hostThisComputer,
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 2),
              Text(
                subtitle,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: monoSubtitle
                    ? mono(fontSize: 12.5, color: quietText(scheme))
                    : TextStyle(fontSize: 12.5, color: quietText(scheme)),
              ),
            ],
          ),
        ),
        if (trailing case final trailing?) ...[
          const SizedBox(width: 10),
          trailing,
        ],
      ],
    );
  }
}

/// The grey pill of a computer that is not sharing.
Widget notSharingPill(BuildContext context) => StatusChip(
  color: onSurfaceMuted(Theme.of(context).colorScheme),
  label: context.l10n.remoteNotSharing,
);

/// The host's error, under the header in every phase. A remembered grant
/// that cannot be read is mended only by revoking it, so its error carries
/// that action.
class _HostError extends StatelessWidget {
  const _HostError(this.view);
  final DesktopHostView view;

  @override
  Widget build(BuildContext context) {
    final error = view.error;
    if (error == null) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    final text = Text(
      error,
      key: const Key('host-error'),
      style: TextStyle(fontSize: 12.5, color: scheme.error),
    );
    if (!view.trustUnreadable) return text;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        text,
        const SizedBox(height: 10),
        OutlinedButton.icon(
          key: const Key('host-error-revoke'),
          style: OutlinedButton.styleFrom(
            minimumSize: const Size(0, 36),
            padding: const EdgeInsets.symmetric(horizontal: 14),
            foregroundColor: scheme.error,
            side: BorderSide(color: scheme.error),
            textStyle: buttonLabel(context, 13, weight: FontWeight.w500),
          ),
          onPressed: () =>
              unawaited(_revoke(context, AppScope.of(context).store.desktop)),
          icon: const Icon(AppIcons.screenShareOff, size: AppIconSize.inline),
          label: Text(context.l10n.hostRevokeUnreadable),
        ),
      ],
    );
  }
}

/// Revokes the invitation once the user confirms it.
Future<void> _revoke(BuildContext context, DesktopStore desktop) async {
  final l10n = context.l10n;
  final yes = await confirmDestructive(
    context,
    title: l10n.hostRevokeConfirmTitle,
    body: l10n.hostRevokeConfirmBody,
    action: l10n.hostRevokeAction,
  );
  if (yes) await desktop.revokeTrust();
}

class _LoadingCard extends StatelessWidget {
  const _LoadingCard({required this.view});
  final DesktopHostView view;

  @override
  Widget build(BuildContext context) => _Card(
    padding: 24,
    gap: 20,
    children: [
      const _HostHeader(
        active: false,
        subtitle: '',
        trailing: SizedBox.square(
          dimension: 16,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      ),
      if (view.error != null) _HostError(view),
    ],
  );
}

// ================================================================== setup

/// HostSetup: the three steps, the two choices, and the one action.
class _SetupCard extends StatefulWidget {
  const _SetupCard({required this.view});
  final DesktopHostView view;

  @override
  State<_SetupCard> createState() => _SetupCardState();
}

class _SetupCardState extends State<_SetupCard> {
  bool _allowControl = true;
  int? _displayId;

  int? get _display {
    final v = widget.view;
    final ids = {for (final d in v.displays) d.id};
    if (_displayId case final id? when ids.contains(id)) return id;
    if (v.displayId case final id? when ids.contains(id)) return id;
    return v.displays.firstOrNull?.id;
  }

  void _enable({bool ordinaryOnly = false}) {
    final display = _display;
    if (display == null) return;
    final v = widget.view;
    unawaited(
      AppScope.of(context).store.desktop.enableHost(
        displayId: display,
        allowControl: _allowControl,
        service: v.service ?? defaultServiceName(Platform.localHostname),
        ordinaryOnly: ordinaryOnly,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final v = widget.view;
    final phase = hostCardPhase(v);
    final busy = phase != HostCardPhase.notEnabled;
    // Enabling reads the remembered grant too, and would fail the same way.
    final canEnable = !busy && _display != null && !v.trustUnreadable;
    final hairline = BorderSide(color: scheme.outlineVariant);
    return _Card(
      padding: 28,
      gap: 24,
      children: [
        _HostHeader(
          active: false,
          subtitle: l10n.hostOff,
          trailing: notSharingPill(context),
        ),
        if (v.error != null) _HostError(v),
        Text(
          l10n.setupHeadline,
          style: const TextStyle(
            fontSize: 22,
            fontWeight: FontWeight.w600,
            height: 1.35,
          ),
        ),
        SetupSteps(
          active: phase == HostCardPhase.restarting ? 2 : 1,
          helperReady: v.helper == DesktopHelperState.ready,
        ),
        DecoratedBox(
          decoration: BoxDecoration(border: Border(top: hairline)),
          child: Column(
            children: [
              DecoratedBox(
                decoration: BoxDecoration(border: Border(bottom: hairline)),
                child: SwitchListTile(
                  key: const Key('host-allow-control'),
                  contentPadding: EdgeInsets.zero,
                  title: Text(
                    l10n.hostAllowControl,
                    style: const TextStyle(fontSize: 13.5),
                  ),
                  value: _allowControl,
                  onChanged: busy
                      ? null
                      : (allow) => setState(() => _allowControl = allow),
                ),
              ),
              _SettingRow(
                height: 52,
                label: l10n.hostDisplay,
                value: DisplayPicker(
                  displays: v.displays,
                  selected: _display,
                  onSelected: busy
                      ? null
                      : (id) => setState(() => _displayId = id),
                ),
              ),
            ],
          ),
        ),
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            FilledButton.icon(
              key: const Key('host-enable'),
              style: FilledButton.styleFrom(
                minimumSize: const Size.fromHeight(48),
                textStyle: buttonLabel(context, 15, weight: FontWeight.w600),
              ),
              onPressed: canEnable ? _enable : null,
              icon: busy
                  ? const SizedBox.square(
                      dimension: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(AppIcons.helper, size: AppIconSize.button),
              label: Text(switch (phase) {
                HostCardPhase.enabling => l10n.hostEnabling,
                HostCardPhase.restarting => l10n.hostRestarting,
                _ => l10n.hostEnable,
              }),
            ),
            const SizedBox(height: 8),
            Wrap(
              alignment: WrapAlignment.spaceBetween,
              crossAxisAlignment: WrapCrossAlignment.center,
              spacing: 12,
              children: [
                TextButton(
                  key: const Key('host-enable-ordinary'),
                  style: TextButton.styleFrom(
                    foregroundColor: signalColor(scheme),
                    padding: EdgeInsets.zero,
                    minimumSize: const Size(0, 36),
                    textStyle: buttonLabel(
                      context,
                      13,
                      weight: FontWeight.w400,
                    ),
                  ),
                  onPressed: canEnable
                      ? () => _enable(ordinaryOnly: true)
                      : null,
                  child: Text(l10n.setupOrdinaryOnly),
                ),
                Text(
                  l10n.setupBackgroundNote,
                  style: TextStyle(fontSize: 13, color: quietText(scheme)),
                ),
              ],
            ),
          ],
        ),
      ],
    );
  }
}

/// The three steps of a first enable; [active] is the one under way. With
/// the helper already installed and verified, only the third remains.
class SetupSteps extends StatelessWidget {
  const SetupSteps({super.key, required this.active, this.helperReady = false});

  final int active;
  final bool helperReady;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final steps = [
      (l10n.setupStep1Title, l10n.setupStep1Body),
      (l10n.setupStep2Title, l10n.setupStep2Body),
      (l10n.setupStep3Title, l10n.setupStep3Body),
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final (i, (title, body)) in steps.indexed) ...[
          if (i > 0) const SizedBox(height: 16),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _StepDisc(
                number: i + 1,
                done: helperReady && i < 2,
                // With the helper ready, the third step is the one left.
                active: (helperReady ? 3 : active) == i + 1,
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      title,
                      style: const TextStyle(
                        fontSize: 14.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 3),
                    Text(
                      body,
                      style: TextStyle(
                        fontSize: 13,
                        height: 1.5,
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ],
      ],
    );
  }
}

class _StepDisc extends StatelessWidget {
  const _StepDisc({
    required this.number,
    required this.done,
    required this.active,
  });

  final int number;
  final bool done, active;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final (fill, ink) = done
        ? (successColor(scheme), Colors.white)
        : active
        ? (scheme.primary, scheme.onPrimary)
        : (backdropBase(scheme), scheme.onSurface);
    return Container(
      width: 26,
      height: 26,
      alignment: Alignment.center,
      decoration: BoxDecoration(color: fill, shape: BoxShape.circle),
      child: done
          ? Icon(AppIcons.check, size: AppIconSize.inline, color: ink)
          : Text(
              '$number',
              style: TextStyle(
                fontSize: 13,
                fontWeight: FontWeight.w600,
                color: ink,
              ),
            ),
    );
  }
}

// ================================================================ sharing

/// Main: this computer shared (or able to resume), its viewer, who may
/// control it, its invitation, and how it shares.
class _SharingCard extends StatelessWidget {
  const _SharingCard({required this.view});
  final DesktopHostView view;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final desktop = AppScope.of(context).store.desktop;
    final v = view;
    final sharing = hostCardPhase(v) == HostCardPhase.sharing;
    final viewer = v.viewer;
    final hairline = BorderSide(color: scheme.outlineVariant);
    return _Card(
      padding: 24,
      gap: 20,
      children: [
        _HostHeader(
          active: true,
          subtitle: v.service ?? '',
          monoSubtitle: true,
          trailing: sharing
              ? StatusChip(
                  inkLabel: true,
                  color: successColor(scheme),
                  label: l10n.hostSharing,
                )
              : notSharingPill(context),
        ),
        if (v.error != null) _HostError(v),
        if (v.otherRelay case final relay?)
          _OtherRelayNotice(relay: relay, busy: v.enabling, desktop: desktop),
        if (sharing && viewer != null)
          ViewerPanel(
            viewer: viewer,
            controlAllowed: v.controlAllowed,
            desktop: desktop,
          ),
        if (v.displayMode case final mode?)
          _DisplayModeNotice(mode: mode, desktop: desktop),
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              l10n.hostPermissionLabel,
              style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
            ),
            const SizedBox(height: 8),
            GlassSegmented<bool>(
              key: const Key('host-permission'),
              height: 40,
              selected: v.controlAllowed,
              onChanged: (allow) {
                if (allow != v.controlAllowed) {
                  unawaited(desktop.allowControl(allow));
                }
              },
              segments: [
                GlassSegment(
                  value: false,
                  label: l10n.hostPermissionView,
                  enabled: sharing,
                ),
                GlassSegment(
                  value: true,
                  label: l10n.hostPermissionControl,
                  enabled: sharing,
                ),
              ],
            ),
          ],
        ),
        if (v.invitation case final invitation?)
          InviteWell(invitation: invitation),
        DecoratedBox(
          decoration: BoxDecoration(border: Border(top: hairline)),
          child: Column(
            children: [
              _SettingRow(
                label: l10n.hostDisplay,
                divider: true,
                value: DisplayPicker(
                  displays: v.displays,
                  selected: v.displayId,
                  disabledHint: v.background
                      ? l10n.hostBackgroundStopFirst
                      : null,
                  onSelected: sharing && !v.background
                      ? (id) => _switchDisplay(context, desktop, v, id)
                      : null,
                ),
              ),
              _SettingRow(
                label: l10n.hostBackground,
                divider: true,
                value: Text(
                  v.background
                      ? l10n.hostBackgroundService
                      : l10n.hostBackgroundApp,
                  textAlign: TextAlign.end,
                ),
              ),
              _SettingRow(
                label: l10n.hostSecure,
                value: _HelperValue(helper: v.helper, desktop: desktop),
              ),
            ],
          ),
        ),
        Wrap(
          spacing: 10,
          runSpacing: 8,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            if (sharing)
              OutlinedButton(
                key: const Key('host-stop'),
                style: OutlinedButton.styleFrom(
                  minimumSize: const Size(0, 40),
                  padding: const EdgeInsets.symmetric(horizontal: 18),
                  side: BorderSide(color: scheme.onSurface),
                  textStyle: buttonLabel(context, 14, weight: FontWeight.w500),
                ),
                onPressed: desktop.stopHost,
                child: Text(l10n.hostStop),
              )
            // On another relay, starting again moves it: the notice asks.
            else if (v.otherRelay == null)
              FilledButton.icon(
                key: const Key('host-start'),
                style: FilledButton.styleFrom(
                  minimumSize: const Size(0, 40),
                  padding: const EdgeInsets.symmetric(horizontal: 18),
                  textStyle: buttonLabel(context, 14, weight: FontWeight.w500),
                ),
                onPressed: v.enabling ? null : desktop.resumeHost,
                icon: v.enabling
                    ? const SizedBox.square(
                        dimension: 16,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(
                        AppIcons.screenShare,
                        size: AppIconSize.inline,
                      ),
                label: Text(v.enabling ? l10n.hostEnabling : l10n.hostStart),
              ),
            HostMoreMenu(view: v),
            if (sharing)
              Text(
                l10n.hostStopNote,
                style: TextStyle(fontSize: 12.5, color: quietText(scheme)),
              ),
          ],
        ),
      ],
    );
  }

  static Future<void> _switchDisplay(
    BuildContext context,
    DesktopStore desktop,
    DesktopHostView v,
    int id,
  ) async {
    if (id == v.displayId) return;
    final l10n = context.l10n;
    final yes = await confirmDestructive(
      context,
      title: l10n.hostDisplaySwitchTitle,
      body: l10n.hostDisplaySwitchBody,
      action: l10n.hostDisplaySwitchAction,
    );
    if (yes) await desktop.setHostDisplay(id);
  }
}

/// This computer is shared, or paused, on another relay than the settings
/// name: it stays there until moved. Moving it changes its invitation, so
/// the action asks first.
/// A viewer changed this display's mode, and the session keeps it while
/// the viewer pauses (Q3): the owner may put it back here. It goes back by
/// itself when the session ends.
class _DisplayModeNotice extends StatelessWidget {
  const _DisplayModeNotice({required this.mode, required this.desktop});

  final DisplayModeList mode;
  final DesktopStore desktop;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return Row(
      key: const Key('host-display-mode'),
      children: [
        Icon(
          AppIcons.remote,
          size: AppIconSize.inline,
          color: scheme.onSurfaceVariant,
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Text(
            l10n.hostDisplayModeChanged(mode.current.label),
            style: const TextStyle(fontSize: 13, height: 1.5),
          ),
        ),
        TextButton(
          key: const Key('host-display-mode-restore'),
          style: TextButton.styleFrom(
            minimumSize: const Size(0, 36),
            textStyle: buttonLabel(context, 13, weight: FontWeight.w600),
          ),
          onPressed: () => unawaited(desktop.restoreHostDisplayMode()),
          child: Text(l10n.hostDisplayModeRestore),
        ),
      ],
    );
  }
}

class _OtherRelayNotice extends StatelessWidget {
  const _OtherRelayNotice({
    required this.relay,
    required this.busy,
    required this.desktop,
  });

  final String relay;
  final bool busy;
  final DesktopStore desktop;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final caution = cautionColor(Theme.of(context).colorScheme);
    return Container(
      key: const Key('host-other-relay'),
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 6),
      decoration: BoxDecoration(
        color: caution.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(kRowRadius),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 2),
                child: Icon(
                  AppIcons.relay,
                  size: AppIconSize.inline,
                  color: caution,
                ),
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  l10n.hostOtherRelay(relay),
                  style: const TextStyle(fontSize: 13, height: 1.5),
                ),
              ),
            ],
          ),
          // Under the words, past the icon.
          Padding(
            padding: const EdgeInsetsDirectional.only(start: 28),
            child: TextButton(
              key: const Key('host-other-relay-move'),
              style: TextButton.styleFrom(
                padding: EdgeInsets.zero,
                minimumSize: const Size(0, 36),
                textStyle: buttonLabel(context, 13, weight: FontWeight.w600),
              ),
              onPressed: busy
                  ? null
                  : () => unawaited(_shareOnCurrentRelay(context, desktop)),
              child: Text(l10n.hostOtherRelayMove),
            ),
          ),
        ],
      ),
    );
  }

  static Future<void> _shareOnCurrentRelay(
    BuildContext context,
    DesktopStore desktop,
  ) async {
    final l10n = context.l10n;
    final yes = await confirmDestructive(
      context,
      title: l10n.hostOtherRelayConfirmTitle,
      body: l10n.hostOtherRelayConfirmBody,
      action: l10n.hostOtherRelayConfirmAction,
    );
    if (yes) await desktop.shareOnCurrentRelay();
  }
}

/// One setting of the card: its name, and its value or control at the end.
class _SettingRow extends StatelessWidget {
  const _SettingRow({
    required this.label,
    required this.value,
    this.height = 48,
    this.divider = false,
  });

  final String label;
  final Widget value;
  final double height;
  final bool divider;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      constraints: BoxConstraints(minHeight: height),
      decoration: divider
          ? BoxDecoration(
              border: Border(bottom: BorderSide(color: scheme.outlineVariant)),
            )
          : null,
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: DefaultTextStyle.merge(
        style: TextStyle(fontSize: 13.5, color: scheme.onSurface),
        child: Row(
          children: [
            Expanded(
              child: Text(
                label,
                style: TextStyle(color: scheme.onSurfaceVariant),
              ),
            ),
            const SizedBox(width: 12),
            Flexible(
              flex: 2,
              child: Align(alignment: Alignment.centerRight, child: value),
            ),
          ],
        ),
      ),
    );
  }
}

/// The helper service's state on the card, with Update when it is too old
/// for this app (D2).
class _HelperValue extends StatelessWidget {
  const _HelperValue({required this.helper, required this.desktop});

  final DesktopHelperState helper;
  final DesktopStore desktop;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final text = switch (helper) {
      DesktopHelperState.ready => l10n.hostHelperReady,
      DesktopHelperState.stopped => l10n.hostHelperStopped,
      DesktopHelperState.none => l10n.hostHelperNone,
      DesktopHelperState.unverified => l10n.hostHelperUnverified,
      DesktopHelperState.outdated => l10n.hostHelperUpdate,
    };
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (helper == DesktopHelperState.ready) ...[
          Icon(
            AppIcons.check,
            size: AppIconSize.inline,
            color: successColor(scheme),
          ),
          const SizedBox(width: 6),
        ],
        Flexible(child: Text(text, textAlign: TextAlign.end)),
        if (helper == DesktopHelperState.outdated) ...[
          const SizedBox(width: 4),
          TextButton(
            key: const Key('host-helper-update'),
            style: TextButton.styleFrom(
              minimumSize: const Size(0, 32),
              padding: const EdgeInsets.symmetric(horizontal: 10),
              textStyle: buttonLabel(context, 13, weight: FontWeight.w600),
            ),
            onPressed: () => unawaited(desktop.helperAction('install')),
            child: Text(l10n.hostHelperUpdateAction),
          ),
        ],
      ],
    );
  }
}

/// Who is viewing this computer, with Make view-only and Disconnect viewer.
/// While it is in view, the title bar needs no capsule for the same
/// controls (D15); scrolled out of view, the fallback pill offers them.
class ViewerPanel extends StatefulWidget {
  const ViewerPanel({
    super.key,
    required this.viewer,
    required this.controlAllowed,
    required this.desktop,
  });

  final DesktopHostViewer viewer;
  final bool controlAllowed;
  final DesktopStore desktop;

  @override
  State<ViewerPanel> createState() => _ViewerPanelState();
}

class _ViewerPanelState extends State<ViewerPanel> {
  Timer? _tick;
  SharingChrome? _chrome;

  /// The list the panel scrolls in: mounted is not the same as in view.
  ScrollableState? _scrollable;

  /// Its position, which the list replaces when its own dependencies
  /// change; listened to for as long as it is the list's.
  ScrollPosition? _position;
  bool _inView = true, _checkScheduled = false;

  @override
  void initState() {
    super.initState();
    // The connected time is in minutes: a refresh twice a minute keeps it.
    _tick = Timer.periodic(const Duration(seconds: 30), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final chrome = AppScope.maybeOf(context)?.sharing;
    if (!identical(chrome, _chrome)) {
      _chrome?.setReachable(this, false);
      // Taken to be in view until the first layout says otherwise, so
      // arriving on the page never flashes the fallback.
      _inView = true;
      _chrome = chrome?..setReachable(this, true);
    }
    // Depends on the list's scope, so a new position lands here too.
    _scrollable = Scrollable.maybeOf(context);
    final position = _scrollable?.position;
    if (!identical(position, _position)) {
      _position?.removeListener(_scheduleCheck);
      _position = position?..addListener(_scheduleCheck);
    }
  }

  /// Whether the whole panel lies inside its scrollable's viewport.
  void _checkInView() {
    if (!mounted) return;
    final box = context.findRenderObject();
    final view = _scrollable?.context.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    var inView = true;
    if (view is RenderBox && view.hasSize) {
      final top = box.localToGlobal(Offset.zero, ancestor: view).dy;
      inView = top >= 0 && top + box.size.height <= view.size.height;
    }
    if (inView == _inView) return;
    _inView = inView;
    _chrome?.setReachable(this, inView);
  }

  /// Checks once the frame is laid out: after a scroll, or a build that may
  /// have moved the panel.
  void _scheduleCheck() {
    if (_checkScheduled) return;
    _checkScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _checkScheduled = false;
      _checkInView();
    });
  }

  @override
  void dispose() {
    _tick?.cancel();
    _position?.removeListener(_scheduleCheck);
    _chrome?.setReachable(this, false);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    _scheduleCheck();
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final viewer = widget.viewer;
    final duration = formatSessionDuration(
      context,
      DateTime.now().difference(viewer.since),
    );
    final meta = viewer.path == 'direct' || viewer.path == 'relay'
        ? l10n.hostViewerMeta(pathLabel(context, viewer.path), duration)
        : duration;
    final small = ButtonStyle(
      minimumSize: const WidgetStatePropertyAll(Size(0, 32)),
      padding: const WidgetStatePropertyAll(
        EdgeInsets.symmetric(horizontal: 12),
      ),
      shape: WidgetStatePropertyAll(
        RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(kControlRadius),
        ),
      ),
      textStyle: WidgetStatePropertyAll(
        buttonLabel(context, 13, weight: FontWeight.w400),
      ),
    );
    final actions = [
      if (widget.controlAllowed)
        OutlinedButton(
          key: const Key('host-make-view-only'),
          style: small,
          onPressed: () => unawaited(widget.desktop.allowControl(false)),
          child: Text(l10n.hostMakeViewOnly),
        ),
      TextButton(
        key: const Key('host-disconnect-viewer'),
        style: small,
        onPressed: widget.desktop.endViewerSession,
        child: Text(l10n.hostDisconnectViewer),
      ),
    ];
    final text = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          viewer.paused
              ? l10n.sharingViewerPaused
              : viewer.controlling
              ? l10n.hostViewersControlling(1)
              : l10n.hostViewersWatching(1),
          style: const TextStyle(fontSize: 14.5, fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 2),
        Text(
          meta,
          style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
        ),
      ],
    );
    return Container(
      key: const Key('host-viewer'),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: cardFieldFill(scheme),
        borderRadius: BorderRadius.circular(kRowRadius),
      ),
      child: LayoutBuilder(
        builder: (context, box) {
          final icon = Icon(
            AppIcons.remoteDevices,
            size: AppIconSize.nav,
            color: signalColor(scheme),
          );
          // The buttons go under the words when beside them the words would
          // keep less than 160 px.
          final label = buttonLabel(context, 13, weight: FontWeight.w400);
          final buttons =
              [
                if (widget.controlAllowed) l10n.hostMakeViewOnly,
                l10n.hostDisconnectViewer,
              ].fold(
                0.0,
                (sum, s) =>
                    sum + 8 + 24 + measureTextLine(context, s, label).width,
              );
          if (box.maxWidth - 32 - buttons < 160) {
            return Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    icon,
                    const SizedBox(width: 12),
                    Expanded(child: text),
                  ],
                ),
                const SizedBox(height: 10),
                Wrap(spacing: 8, runSpacing: 8, children: actions),
              ],
            );
          }
          return Row(
            children: [
              icon,
              const SizedBox(width: 12),
              Expanded(child: text),
              for (final action in actions) ...[
                const SizedBox(width: 8),
                action,
              ],
            ],
          );
        },
      ),
    );
  }
}

/// The shared display: its number and size, never the OS's name for it.
class DisplayPicker extends StatelessWidget {
  const DisplayPicker({
    super.key,
    required this.displays,
    required this.selected,
    required this.onSelected,
    this.disabledHint,
  });

  final List<DesktopDisplay> displays;
  final int? selected;

  /// Null: the picker shows the display but cannot change it.
  final ValueChanged<int>? onSelected;

  /// Why it cannot change, as its tooltip.
  final String? disabledHint;

  String _label(BuildContext context, int index) {
    final d = displays[index];
    return context.l10n.hostDisplayValue(
      index + 1,
      '${d.width}',
      '${d.height}',
    );
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final index = displays.indexWhere((d) => d.id == selected);
    if (index < 0) return const Text('—');
    final onSelected = this.onSelected;
    const key = Key('host-display');
    if (onSelected == null || displays.length < 2) {
      final value = Text(_label(context, index), key: key);
      return disabledHint == null
          ? value
          : Tooltip(message: disabledHint, child: value);
    }
    return MenuAnchor(
      menuChildren: [
        for (final (i, d) in displays.indexed)
          MenuItemButton(
            key: Key('host-display-${d.id}'),
            leadingIcon: Icon(
              d.id == selected ? AppIcons.check : null,
              size: AppIconSize.inline,
            ),
            onPressed: () => onSelected(d.id),
            child: Text(_label(context, i)),
          ),
      ],
      builder: (context, menu, _) => TextButton(
        key: key,
        style: TextButton.styleFrom(
          minimumSize: const Size(0, 36),
          padding: const EdgeInsets.symmetric(horizontal: 8),
          foregroundColor: scheme.onSurface,
          textStyle: buttonLabel(context, 13.5, weight: FontWeight.w400),
        ),
        onPressed: () => menu.isOpen ? menu.close() : menu.open(),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Flexible(
              child: Text(
                _label(context, index),
                overflow: TextOverflow.ellipsis,
              ),
            ),
            const SizedBox(width: 6),
            const Icon(AppIcons.chevronDown, size: AppIconSize.inline),
          ],
        ),
      ),
    );
  }
}

/// More: the service name, the helper's maintenance, and revoking the
/// invitation.
class HostMoreMenu extends StatelessWidget {
  const HostMoreMenu({super.key, required this.view});
  final DesktopHostView view;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final desktop = AppScope.of(context).store.desktop;
    final v = view;
    final helperBusy = v.enabling || v.restarting;
    // A rename restarts the running local host under the new name: the
    // background service's name is fixed, and a stopped host has none.
    final renameHint = v.background
        ? l10n.hostBackgroundStopFirst
        : v.sharing
        ? null
        : l10n.hostRenameStartFirst;
    Widget rename = MenuItemButton(
      key: const Key('host-rename'),
      leadingIcon: const Icon(AppIcons.edit, size: AppIconSize.inline),
      onPressed: renameHint == null ? () => _rename(context, desktop, v) : null,
      child: Text(l10n.hostServiceName),
    );
    if (renameHint != null) {
      rename = Tooltip(message: renameHint, child: rename);
    }
    return MenuAnchor(
      menuChildren: [
        rename,
        MenuItemButton(
          key: const Key('host-helper-install'),
          leadingIcon: const Icon(AppIcons.helper, size: AppIconSize.inline),
          onPressed: helperBusy
              ? null
              : () => unawaited(desktop.helperAction('install')),
          child: Text(l10n.hostHelperInstall),
        ),
        if (v.helper != DesktopHelperState.none) ...[
          MenuItemButton(
            key: const Key('host-helper-disable'),
            leadingIcon: const Icon(AppIcons.stop, size: AppIconSize.inline),
            onPressed: helperBusy
                ? null
                : () => unawaited(desktop.helperAction('disable')),
            child: Text(l10n.hostHelperDisable),
          ),
          MenuItemButton(
            key: const Key('host-helper-uninstall'),
            leadingIcon: const Icon(AppIcons.delete, size: AppIconSize.inline),
            onPressed: helperBusy
                ? null
                : () => unawaited(desktop.helperAction('uninstall')),
            child: Text(l10n.hostHelperUninstall),
          ),
        ],
        const Divider(height: 9),
        MenuItemButton(
          key: const Key('host-revoke'),
          leadingIcon: Icon(
            AppIcons.screenShareOff,
            size: AppIconSize.inline,
            color: scheme.error,
          ),
          style: MenuItemButton.styleFrom(foregroundColor: scheme.error),
          onPressed: () => _revoke(context, desktop),
          child: Text(l10n.hostRevoke),
        ),
      ],
      builder: (context, menu, _) => TextButton.icon(
        key: const Key('host-more'),
        style: TextButton.styleFrom(
          minimumSize: const Size(0, 40),
          padding: const EdgeInsets.symmetric(horizontal: 14),
          textStyle: buttonLabel(context, 14, weight: FontWeight.w400),
        ),
        onPressed: () => menu.isOpen ? menu.close() : menu.open(),
        icon: const Icon(AppIcons.more, size: AppIconSize.inline),
        label: Text(l10n.more),
      ),
    );
  }

  static Future<void> _rename(
    BuildContext context,
    DesktopStore desktop,
    DesktopHostView v,
  ) async {
    final service = await showDialog<String>(
      context: context,
      builder: (_) => _RenameServiceDialog(
        current: v.service ?? defaultServiceName(Platform.localHostname),
      ),
    );
    if (service != null && service != v.service) {
      await desktop.renameHostService(service);
    }
  }
}

class _RenameServiceDialog extends StatefulWidget {
  const _RenameServiceDialog({required this.current});

  final String current;

  @override
  State<_RenameServiceDialog> createState() => _RenameServiceDialogState();
}

class _RenameServiceDialogState extends State<_RenameServiceDialog> {
  late final _suffix = TextEditingController(
    text: widget.current.startsWith('desktop:')
        ? widget.current.substring('desktop:'.length)
        : widget.current,
  );

  @override
  void dispose() {
    _suffix.dispose();
    super.dispose();
  }

  bool get _valid => _serviceSuffix.hasMatch(_suffix.text.trim());

  void _save() {
    if (_valid) Navigator.of(context).pop('desktop:${_suffix.text.trim()}');
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return AlertDialog(
      title: Text(l10n.hostServiceName),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          TextField(
            key: const Key('host-rename-field'),
            controller: _suffix,
            autofocus: true,
            style: mono(fontSize: 13.5),
            inputFormatters: [LengthLimitingTextInputFormatter(120)],
            decoration: InputDecoration(
              prefixText: 'desktop:',
              prefixStyle: mono(fontSize: 13.5, color: quietText(scheme)),
            ),
            onSubmitted: (_) => _save(),
          ),
          const SizedBox(height: 12),
          Text(
            l10n.hostServiceRenameBody,
            style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        ListenableBuilder(
          listenable: _suffix,
          builder: (context, _) => FilledButton(
            key: const Key('host-rename-save'),
            onPressed: _valid ? _save : null,
            child: Text(l10n.save),
          ),
        ),
      ],
    );
  }
}

// ================================================================= invite

/// The private invitation, masked, with Copy and QR code. Nothing at all
/// without one. The QR shows only on request and hides again whenever the
/// page is left or the app stops being in front (§6.3).
class InviteWell extends StatefulWidget {
  const InviteWell({super.key, required this.invitation});

  /// A bearer secret: masked on screen, copied on request, never logged.
  final String invitation;

  @override
  State<InviteWell> createState() => _InviteWellState();
}

class _InviteWellState extends State<InviteWell> {
  bool _showQr = false;
  late final AppLifecycleListener _lifecycle;

  @override
  void initState() {
    super.initState();
    // Never in a task-switcher snapshot.
    _lifecycle = AppLifecycleListener(
      onInactive: _hide,
      onHide: _hide,
      onPause: _hide,
    );
  }

  void _hide() {
    if (_showQr && mounted) setState(() => _showQr = false);
  }

  @override
  void didUpdateWidget(InviteWell old) {
    super.didUpdateWidget(old);
    if (old.invitation != widget.invitation) _showQr = false;
  }

  @override
  void deactivate() {
    _showQr = false;
    super.deactivate();
  }

  @override
  void dispose() {
    _lifecycle.dispose();
    super.dispose();
  }

  Future<void> _copy() async {
    final copied = context.l10n.hostInviteCopied;
    await Clipboard.setData(ClipboardData(text: widget.invitation));
    if (mounted) showToast(context, copied, kind: ToastKind.success);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final small = OutlinedButton.styleFrom(
      minimumSize: const Size(0, 32),
      padding: const EdgeInsets.symmetric(horizontal: 12),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(kControlRadius),
      ),
      textStyle: buttonLabel(context, 13, weight: FontWeight.w400),
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          l10n.inviteLabel,
          style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
        ),
        const SizedBox(height: 8),
        LayoutBuilder(
          builder: (context, box) {
            // A narrow well keeps the mask readable: the buttons lose their
            // words and keep their tooltips.
            final compact = box.maxWidth < 380;
            Widget button(
              Key key,
              IconData icon,
              String label,
              VoidCallback f,
            ) => compact
                ? Tooltip(
                    message: label,
                    child: OutlinedButton(
                      key: key,
                      style: small.copyWith(
                        padding: const WidgetStatePropertyAll(
                          EdgeInsets.symmetric(horizontal: 8),
                        ),
                      ),
                      onPressed: f,
                      child: Icon(icon, size: AppIconSize.inline),
                    ),
                  )
                : OutlinedButton.icon(
                    key: key,
                    style: small,
                    onPressed: f,
                    icon: Icon(icon, size: AppIconSize.inline),
                    label: Text(label),
                  );
            return Container(
              height: 48,
              padding: const EdgeInsetsDirectional.fromSTEB(16, 0, 8, 0),
              decoration: BoxDecoration(
                color: cardFieldFill(scheme),
                borderRadius: BorderRadius.circular(kFieldRadius),
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      maskInvitation(widget.invitation),
                      maxLines: 1,
                      softWrap: false,
                      overflow: TextOverflow.fade,
                      style: mono(fontSize: 13),
                    ),
                  ),
                  const SizedBox(width: 8),
                  button(
                    const Key('host-copy'),
                    AppIcons.copy,
                    l10n.copy,
                    () => unawaited(_copy()),
                  ),
                  const SizedBox(width: 8),
                  button(
                    const Key('host-qr'),
                    AppIcons.qrCode,
                    _showQr ? l10n.hostQrHide : l10n.hostQr,
                    () => setState(() => _showQr = !_showQr),
                  ),
                ],
              ),
            );
          },
        ),
        if (_showQr) ...[
          const SizedBox(height: 12),
          Center(child: InviteQrPanel(invitation: widget.invitation)),
        ],
        const SizedBox(height: 8),
        Text(
          l10n.hostInviteHelp,
          style: TextStyle(
            fontSize: 12.5,
            height: 1.5,
            color: quietText(scheme),
          ),
        ),
      ],
    );
  }
}
