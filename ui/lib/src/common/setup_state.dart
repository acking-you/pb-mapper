/// Whether this install has ever been configured.
///
/// The wizard is for someone who has nothing yet, so "configured" is judged by
/// evidence of use rather than by a flag: an install that only carries the
/// default server address and has no services or clients has never been set up,
/// no matter how many times the app has been opened.
class SetupState {
  const SetupState._();

  /// What the Rust side writes when it has no config file.
  static const String defaultServerAddress = 'localhost:7666';

  static bool isServerConfigured(String serverAddress) {
    final trimmed = serverAddress.trim();
    return trimmed.isNotEmpty && trimmed != defaultServerAddress;
  }

  /// True when the app should open the wizard instead of the role picker.
  ///
  /// Someone who has registered or connected has clearly been through this,
  /// even if they never moved off the default address.
  static bool needsSetup({
    required String serverAddress,
    required bool hasTunnels,
  }) => !isServerConfigured(serverAddress) && !hasTunnels;
}
