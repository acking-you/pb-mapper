import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';

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
  /// Errors count as configured: failing to read state is not evidence of a new
  /// install, and dropping an existing user into a wizard is the worse mistake.
  static Future<bool> needsSetup(PbMapperApi api) async {
    try {
      final results = await Future.wait([
        api.fetchConfig(),
        api.getServiceConfigs(),
        api.getClientConfigs(),
      ]);
      final config = results[0] as ConfigStatus;
      final services = results[1] as List<ServiceConfigInfo>;
      final clients = results[2] as List<ClientConfigInfo>;

      if (isServerConfigured(config.serverAddress)) return false;
      // Someone who has registered or connected has clearly been through this,
      // even if they never moved off the default address.
      return services.isEmpty && clients.isEmpty;
    } catch (_) {
      return false;
    }
  }
}
