import 'dart:convert';

import 'package:pb_mapper_ui/src/ffi/relay_models.dart' show RelayTransport;

/// What a setup string carries: everything another computer needs to use a
/// relay.
class SetupPayload {
  const SetupPayload({
    required this.address,
    required this.credential,
    this.keepAlive = true,
    this.relayTransport = RelayTransport.tcp,
  });

  final String address;
  final String credential;
  final bool keepAlive;

  /// How to reach the relay. A string from before there was a choice says
  /// nothing, which is TCP.
  final RelayTransport relayTransport;
}

/// The same base64 JSON the app has always exported, so a string copied from
/// an older build still pastes, and an older build ignores the carrier.
String encodeSetupString(SetupPayload payload) => base64Encode(
  utf8.encode(
    jsonEncode({
      'version': 1,
      'serverAddress': payload.address.trim(),
      'keepAliveEnabled': payload.keepAlive,
      'msgHeaderKey': payload.credential.trim(),
      'relayTransport': payload.relayTransport.name,
      'exportedAt': DateTime.now().toIso8601String(),
    }),
  ),
);

/// [raw] as a setup string. Throws a [FormatException] when it is not one;
/// checking the address and credential is the caller's.
SetupPayload decodeSetupString(String raw) {
  final normalized = raw.replaceAll(RegExp(r'\s+'), '');
  List<int> bytes;
  try {
    bytes = base64Decode(normalized);
  } catch (_) {
    bytes = base64Url.decode(base64Url.normalize(normalized));
  }
  final decoded = jsonDecode(utf8.decode(bytes));
  if (decoded is! Map) throw const FormatException('not a JSON object');
  return SetupPayload(
    address: (decoded['serverAddress'] ?? '').toString().trim(),
    credential: (decoded['msgHeaderKey'] ?? '').toString().trim(),
    keepAlive: switch (decoded['keepAliveEnabled']) {
      final bool b => b,
      final num n => n != 0,
      final String s => s.toLowerCase() == 'true' || s == '1',
      _ => true,
    },
    relayTransport: RelayTransport.parse(decoded['relayTransport']),
  );
}
