import 'package:flutter/material.dart';
import 'package:toastification/toastification.dart';

/// What a message means, without saying how it should look.
enum ToastKind { info, success, warning, error }

/// The app's one way of telling the user something happened.
///
/// This replaces `ScaffoldMessenger.showSnackBar`, which puts one bar at a
/// time across the bottom of the window: a second message replaces the first
/// mid-read, and there is no way to dismiss one you have finished with. These
/// stack, and each carries a close button.
///
/// Call sites take this function rather than the package, so swapping the
/// implementation is one file rather than thirty-seven.
void showToast(
  BuildContext context,
  String message, {
  ToastKind kind = ToastKind.info,
  String? description,
}) {
  toastification.show(
    context: context,
    type: switch (kind) {
      ToastKind.info => ToastificationType.info,
      ToastKind.success => ToastificationType.success,
      ToastKind.warning => ToastificationType.warning,
      ToastKind.error => ToastificationType.error,
    },
    style: ToastificationStyle.flatColored,
    title: Text(message, maxLines: 3, overflow: TextOverflow.ellipsis),
    description: description == null ? null : Text(description),
    alignment: Alignment.bottomRight,
    // Long enough to read a service key, short enough not to pile up while
    // a registration polls. Errors stay until dismissed: they are the ones
    // worth reading after you have looked away.
    autoCloseDuration: kind == ToastKind.error
        ? null
        : const Duration(seconds: 4),
    showProgressBar: false,
    closeOnClick: false,
    dragToClose: true,
    applyBlurEffect: false,
  );
}
