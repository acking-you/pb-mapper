import 'package:flutter/widgets.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';

/// `context.l10n` instead of `AppLocalizations.of(context)!`.
///
/// The generated `of` is nullable because a widget can sit outside a
/// Localizations scope. Everything here lives under MaterialApp, which installs
/// the delegate, so the null case would be a wiring bug rather than a state to
/// handle at each call site.
extension AppLocalizationsX on BuildContext {
  /// Falls back to English rather than throwing. A null here means a widget is
  /// reading strings from above MaterialApp, which is a wiring mistake — but a
  /// blank window is a far worse way to report it than English text plus the
  /// assert below.
  AppLocalizations get l10n {
    final localizations = AppLocalizations.of(this);
    assert(
      localizations != null,
      'No AppLocalizations above this context. Read strings from a context '
      'inside MaterialApp, not from the State that builds it.',
    );
    return localizations ?? lookupAppLocalizations(const Locale('en'));
  }
}
