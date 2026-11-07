import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../shell/sharing_indicator.dart';
import '../../../shell/window_chrome.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/metrics.dart';
import '../../../theme/viewer_palette.dart';

/// The desktop viewer's own title bar (Viewer:17-27): the computer's name,
/// the sharing capsule while this computer is viewed too (D15), and the
/// caption buttons in their dark variant (D25). The window drags from
/// anywhere that is not a control. Hidden in full screen.
class ViewerTitleBar extends StatelessWidget {
  const ViewerTitleBar({super.key, required this.title, this.onBack});

  final String title;

  /// Back to the list; null: no Back control.
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: 36,
    child: Stack(
      children: [
        const Positioned.fill(child: WindowDragArea()),
        // The capsule's form follows the whole bar's width, as a page's
        // title bar does, not the padded row's.
        LayoutBuilder(
          builder: (context, box) => Padding(
            padding: EdgeInsetsDirectional.only(
              start: trafficLightInset + (onBack == null ? 16 : 8),
              end: drawsCaptionButtons ? 4 : 12,
            ),
            child: Row(
              children: [
                if (onBack case final back?) ...[
                  _TitleBack(onTap: back),
                  const SizedBox(width: 8),
                ],
                const Icon(
                  AppIcons.remote,
                  size: AppIconSize.inline,
                  color: ViewerPalette.signal,
                ),
                const SizedBox(width: 10),
                // The title takes what it needs of the free space and the
                // trailing group keeps the right edge: a loose Flexible
                // beside a Spacer would split it and leave both mid-bar.
                Expanded(
                  child: IgnorePointer(
                    child: Row(
                      children: [
                        Flexible(
                          child: Text(
                            title,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              fontSize: 13,
                              fontWeight: FontWeight.w600,
                              color: ViewerPalette.text,
                            ),
                          ),
                        ),
                        const SizedBox(width: 10),
                        const Text(
                          'pb-mapper',
                          maxLines: 1,
                          style: TextStyle(
                            fontSize: 12.5,
                            color: ViewerPalette.textQuiet,
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
                SharingIndicator(
                  form: sharingFormFor(box.maxWidth),
                  tone: CaptionTone.viewer,
                ),
                const SizedBox(width: 12),
                if (drawsCaptionButtons)
                  const CaptionButtons(tone: CaptionTone.viewer),
              ],
            ),
          ),
        ),
      ],
    ),
  );
}

class _TitleBack extends StatelessWidget {
  const _TitleBack({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    const radius = BorderRadius.all(Radius.circular(6));
    return Tooltip(
      message: context.l10n.viewerBackToList,
      child: Material(
        type: MaterialType.transparency,
        child: InkWell(
          key: const Key('viewer-back'),
          onTap: onTap,
          borderRadius: radius,
          mouseCursor: clickable,
          hoverColor: ViewerPalette.fill,
          child: const SizedBox.square(
            dimension: 28,
            child: Icon(
              AppIcons.back,
              size: AppIconSize.inline,
              color: ViewerPalette.textSecondary,
            ),
          ),
        ),
      ),
    );
  }
}
