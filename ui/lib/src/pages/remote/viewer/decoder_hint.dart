import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../../../l10n/app_localizations.dart';
import '../../../common/app_toast.dart';
import '../../../common/l10n_extension.dart';
import '../../../state/desktop_models.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/fonts.dart';
import '../../../theme/viewer_palette.dart';
import '../../settings_page.dart' show kRepositoryUrl;

/// The Microsoft Store's HEVC Video Extensions, by Microsoft, a paid app:
/// the decoder MFT the Windows runner looks for (`SyncDecoders` in
/// desktop_video.cpp). A PC whose maker included the extensions has the MFT
/// already and is never told of them.
const hevcExtensionStoreId = '9NMZLZ57R3T7';

/// [hint] in one line: what is missing and what installing it gives. The
/// runner probes once a run (it loads GStreamer once, and the store keeps
/// its answer), so the app takes what was installed only once it is opened
/// again ([AppLocalizations.decoderHintRestart], said beside it).
String decoderHintText(AppLocalizations l10n, DecoderInstallHint hint) =>
    switch (hint) {
      DecoderInstallHint.hevcExtension => l10n.decoderHintHevc,
      DecoderInstallHint.gstreamer => l10n.decoderHintGstreamer,
      DecoderInstallHint.gpuDriver => l10n.decoderHintGpuDriver,
    };

/// What [hint]'s button does ([openDecoderHint]).
String decoderHintAction(AppLocalizations l10n, DecoderInstallHint hint) =>
    hint == DecoderInstallHint.hevcExtension
    ? l10n.decoderHintHevcAction
    : l10n.decoderHintGuide;

IconData decoderHintIcon(DecoderInstallHint hint) =>
    hint == DecoderInstallHint.hevcExtension
    ? AppIcons.download
    : AppIcons.external;

/// Where [hint]'s button goes, each tried until one opens: the HEVC Video
/// Extensions in the Store app, then on the Store's web page where Windows
/// has no Store app; for Linux, the user guide's remote desktop section,
/// whose table lists the packages per distribution and GPU, in the language
/// of [localeName] (`AppLocalizations.localeName`).
List<Uri> decoderHintTargets(DecoderInstallHint hint, String localeName) =>
    switch (hint) {
      DecoderInstallHint.hevcExtension => [
        Uri.parse('ms-windows-store://pdp/?ProductId=$hevcExtensionStoreId'),
        Uri.parse('https://apps.microsoft.com/detail/$hevcExtensionStoreId'),
      ],
      DecoderInstallHint.gstreamer || DecoderInstallHint.gpuDriver => [
        localeName.startsWith('zh')
            ? Uri.parse(
                '$kRepositoryUrl/blob/master/docs/user-guide.zh-CN.md',
              ).replace(fragment: '应用里的远程桌面')
            : Uri.parse(
                '$kRepositoryUrl/blob/master/docs/user-guide.md'
                '#remote-desktop-in-the-app',
              ),
      ],
    };

/// Opens the first of [hint]'s targets ([decoderHintTargets]) that
/// [launch] opens. A target that is not a web page is tried only where
/// [canLaunch] finds an app registered for its scheme: Windows may answer a
/// scheme no app handles, such as the Store's where the Store app was
/// removed, with its own dialog instead of failing (url_launcher_windows
/// calls `ShellExecuteW` without `SEE_MASK_FLAG_NO_UI`), and the Store's
/// web page then opens directly.
Future<void> openDecoderHint(
  DecoderInstallHint hint,
  String localeName, {
  Future<bool> Function(Uri uri) launch = _launch,
  Future<bool> Function(Uri uri) canLaunch = _canLaunch,
}) async {
  for (final uri in decoderHintTargets(hint, localeName)) {
    final web = uri.isScheme('https') || uri.isScheme('http');
    if (!web && !await canLaunch(uri)) continue;
    if (await launch(uri)) return;
  }
}

/// Whether an app is registered for [uri]'s scheme (on Windows, a
/// `URL Protocol` value under its key in `HKEY_CLASSES_ROOT`), which asks
/// the system without opening anything; false where that cannot be read.
Future<bool> _canLaunch(Uri uri) async {
  try {
    return await canLaunchUrl(uri);
  } catch (_) {
    return false;
  }
}

/// False where nothing handles [uri]'s scheme.
Future<bool> _launch(Uri uri) async {
  try {
    return await launchUrl(uri, mode: LaunchMode.externalApplication);
  } catch (_) {
    return false;
  }
}

/// The command that installs GStreamer's decoders on the Linux
/// distribution [osRelease] (an os-release file) names, or the first one it
/// derives from that the user guide's table covers (Debian and Ubuntu,
/// Fedora, Arch): its "Always" and "Software decoding" rows. Null for any
/// other.
String? gstreamerInstallCommand(String osRelease) {
  String? id;
  var like = const <String>[];
  for (final line in const LineSplitter().convert(osRelease)) {
    final eq = line.indexOf('=');
    if (eq < 0) continue;
    var value = line.substring(eq + 1).trim();
    if (value.length >= 2 &&
        (value.startsWith('"') || value.startsWith("'")) &&
        value.endsWith(value[0])) {
      value = value.substring(1, value.length - 1);
    }
    switch (line.substring(0, eq).trim()) {
      case 'ID':
        id = value;
      case 'ID_LIKE':
        like = value.split(' ');
    }
  }
  for (final name in [?id, ...like]) {
    switch (name) {
      case 'debian' || 'ubuntu':
        return 'sudo apt install gstreamer1.0-plugins-base '
            'gstreamer1.0-plugins-bad gstreamer1.0-libav';
      case 'fedora':
        return 'sudo dnf install gstreamer1-plugins-base '
            'gstreamer1-plugins-bad-free gstreamer1-plugin-openh264';
      case 'arch':
        return 'sudo pacman -S gst-plugins-base gst-plugins-bad gst-libav';
    }
  }
  return null;
}

/// This Linux's [gstreamerInstallCommand], read once, when a hint first
/// asks; null on any other platform or where os-release cannot be read.
final String? linuxGstreamerInstallCommand = _readInstallCommand();

String? _readInstallCommand() {
  if (!Platform.isLinux) return null;
  for (final path in const ['/etc/os-release', '/usr/lib/os-release']) {
    try {
      return gstreamerInstallCommand(File(path).readAsStringSync());
    } catch (_) {
      // Not there, or not text: the next one, which systemd reads too.
    }
  }
  return null;
}

/// The command [hint] would have typed, where one is known.
String? decoderHintCommand(DecoderInstallHint hint) =>
    hint == DecoderInstallHint.gstreamer ? linuxGstreamerInstallCommand : null;

/// Puts [command] on the clipboard and says so, beside [context], or
/// without one from a menu, whose context is gone once it closes.
Future<void> copyDecoderHintCommand(
  BuildContext? context,
  AppLocalizations l10n,
  String command,
) async {
  await Clipboard.setData(ClipboardData(text: command));
  if (context == null) {
    showToast(null, l10n.decoderHintCopied, kind: ToastKind.success);
  } else if (context.mounted) {
    showToast(context, l10n.decoderHintCopied, kind: ToastKind.success);
  }
}

/// [hint] in the quality panel, above its figures: its line, the install
/// command
/// where this distribution's is known ([command]), where to get it, and
/// Don't show again ([onDismiss]), which hides it on this device.
class DecoderHintCard extends StatelessWidget {
  const DecoderHintCard({
    super.key = const Key('quality-decoder-hint'),
    required this.hint,
    required this.onDismiss,
    this.command,
  });

  final DecoderInstallHint hint;
  final VoidCallback onDismiss;
  final String? command;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final command = this.command;
    // A button's text style replaces the ambient one: start from the
    // theme's, which carries the typeface and its CJK fallback.
    final base = Theme.of(context).textTheme.bodyMedium ?? const TextStyle();
    ButtonStyle button(Color ink) => TextButton.styleFrom(
      foregroundColor: ink,
      iconColor: ink,
      visualDensity: VisualDensity.compact,
      padding: const EdgeInsets.symmetric(horizontal: 8),
      textStyle: base.copyWith(fontSize: 12.5, fontWeight: FontWeight.w500),
    );
    return Container(
      margin: const EdgeInsets.only(top: 14),
      padding: const EdgeInsets.fromLTRB(12, 10, 6, 4),
      decoration: BoxDecoration(
        color: ViewerPalette.fill,
        borderRadius: BorderRadius.circular(10),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsetsDirectional.only(end: 6),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Padding(
                  padding: EdgeInsets.only(top: 2),
                  child: Icon(
                    AppIcons.info,
                    size: AppIconSize.inline,
                    color: ViewerPalette.signal,
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        decoderHintText(l10n, hint),
                        style: const TextStyle(
                          fontSize: 12.5,
                          height: 1.45,
                          color: ViewerPalette.text,
                        ),
                      ),
                      Text(
                        l10n.decoderHintRestart,
                        style: const TextStyle(
                          fontSize: 11.5,
                          height: 1.45,
                          color: ViewerPalette.textSecondary,
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          if (command != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Expanded(
                    child: SelectableText(
                      command,
                      key: const Key('quality-decoder-hint-command'),
                      style: mono(
                        fontSize: 11,
                        color: ViewerPalette.textSecondary,
                      ),
                    ),
                  ),
                  IconButton(
                    key: const Key('quality-decoder-hint-copy'),
                    tooltip: l10n.decoderHintCopy,
                    visualDensity: VisualDensity.compact,
                    onPressed: () =>
                        copyDecoderHintCommand(context, l10n, command),
                    icon: const Icon(
                      AppIcons.copy,
                      size: AppIconSize.inline,
                      color: ViewerPalette.textSecondary,
                    ),
                  ),
                ],
              ),
            ),
          Wrap(
            children: [
              TextButton.icon(
                key: const Key('quality-decoder-hint-open'),
                style: button(ViewerPalette.signal),
                onPressed: () => openDecoderHint(hint, l10n.localeName),
                icon: Icon(decoderHintIcon(hint), size: AppIconSize.inline),
                label: Text(decoderHintAction(l10n, hint)),
              ),
              TextButton(
                key: const Key('quality-decoder-hint-dismiss'),
                style: button(ViewerPalette.textSecondary),
                onPressed: onDismiss,
                child: Text(l10n.decoderHintDismiss),
              ),
            ],
          ),
        ],
      ),
    );
  }
}
