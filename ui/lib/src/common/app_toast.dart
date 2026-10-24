import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:toastification/toastification.dart' show toastification;

/// What a message means, without saying how it should look.
enum ToastKind { info, success, warning, error }

/// The app's one way of telling the user something happened: a pane of glass
/// that stacks with the others and can be dismissed. Errors stay until closed —
/// they are the ones worth reading after looking away.
///
/// [context] may be null, or no longer mounted: an outcome often arrives after
/// the panel that started it closed. The wrapper sits inside `MaterialApp`
/// (see `main.dart`), so a toast raised without one still wears the theme.
void showToast(
  BuildContext? context,
  String message, {
  ToastKind kind = ToastKind.info,
  String? description,
}) {
  final live = context != null && context.mounted ? context : null;
  final narrow = live != null && MediaQuery.sizeOf(live).width < 600;
  try {
    toastification.showCustom(
      context: live,
      alignment: narrow ? Alignment.topCenter : Alignment.bottomRight,
      autoCloseDuration: kind == ToastKind.error
          ? null
          : const Duration(seconds: 4),
      builder: (context, item) => _Toast(
        message: message,
        description: description,
        kind: kind,
        onClose: () => toastification.dismiss(item),
      ),
    );
  } catch (_) {
    // No app to show it in: an outcome that arrived while the window was
    // closing. There is nobody left to tell.
  }
}

class _Toast extends StatelessWidget {
  const _Toast({
    required this.message,
    required this.kind,
    required this.onClose,
    this.description,
  });

  final String message;
  final String? description;
  final ToastKind kind;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final color = switch (kind) {
      ToastKind.info => signalColor(scheme),
      ToastKind.success => successColor(scheme),
      ToastKind.warning => cautionColor(scheme),
      ToastKind.error => scheme.error,
    };
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 400),
        child: Glass(
          depth: GlassDepth.overlay,
          borderRadius: BorderRadius.circular(16),
          padding: const EdgeInsets.fromLTRB(16, 12, 6, 12),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: StatusDot(color: color),
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      message,
                      maxLines: 3,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontWeight: FontWeight.w500,
                        fontSize: 13.5,
                      ),
                    ),
                    if (description != null) ...[
                      const SizedBox(height: 2),
                      Text(
                        description!,
                        maxLines: 4,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ],
                ),
              ),
              IconButton(
                onPressed: onClose,
                visualDensity: VisualDensity.compact,
                iconSize: 15,
                icon: const Icon(AppIcons.close),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
