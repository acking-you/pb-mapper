import 'dart:convert';
import 'package:shared_preferences/shared_preferences.dart';

enum ClientStatus {
  running, // Green - client is connected normally
  retrying, // Yellow - in retry connection loop
  failed, // Red - connection failed or remote server unavailable
  stopped, // Grey - client is stopped
}

class ClientConfig {
  final String serviceKey;
  final String localAddress;
  final String protocol;
  final bool enableKeepAlive;
  final DateTime createdAt;
  DateTime updatedAt;
  ClientStatus status;
  String statusMessage;

  ClientConfig({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableKeepAlive,
    DateTime? createdAt,
    DateTime? updatedAt,
    this.status = ClientStatus.stopped,
    this.statusMessage = '',
  }) : createdAt = createdAt ?? DateTime.now(),
       updatedAt = updatedAt ?? DateTime.now();

  // JSON serialization
  Map<String, dynamic> toJson() => {
    'serviceKey': serviceKey,
    'localAddress': localAddress,
    'protocol': protocol,
    'enableKeepAlive': enableKeepAlive,
    'createdAt': createdAt.toIso8601String(),
    'updatedAt': updatedAt.toIso8601String(),
    'status': status.name,
    'statusMessage': statusMessage,
  };

  factory ClientConfig.fromJson(Map<String, dynamic> json) => ClientConfig(
    serviceKey: json['serviceKey'] ?? '',
    localAddress: json['localAddress'] ?? '',
    protocol: json['protocol'] ?? 'TCP',
    enableKeepAlive: json['enableKeepAlive'] ?? false,
    createdAt: DateTime.tryParse(json['createdAt'] ?? '') ?? DateTime.now(),
    updatedAt: DateTime.tryParse(json['updatedAt'] ?? '') ?? DateTime.now(),
    status: ClientStatus.values.firstWhere(
      (e) => e.name == json['status'],
      orElse: () => ClientStatus.stopped,
    ),
    statusMessage: json['statusMessage'] ?? '',
  );

  ClientConfig copyWith({
    String? serviceKey,
    String? localAddress,
    String? protocol,
    bool? enableKeepAlive,
    ClientStatus? status,
    String? statusMessage,
  }) => ClientConfig(
    serviceKey: serviceKey ?? this.serviceKey,
    localAddress: localAddress ?? this.localAddress,
    protocol: protocol ?? this.protocol,
    enableKeepAlive: enableKeepAlive ?? this.enableKeepAlive,
    createdAt: createdAt,
    updatedAt: DateTime.now(),
    status: status ?? this.status,
    statusMessage: statusMessage ?? this.statusMessage,
  );

  void updateStatus(ClientStatus newStatus, String message) {
    status = newStatus;
    statusMessage = message;
    updatedAt = DateTime.now();
  }
}

// Client configuration storage manager
class ClientConfigManager {
  static const String _storageKey = 'client_configurations';

  // In-memory cache of configurations
  static final Map<String, ClientConfig> _configs = {};
  static bool _initialized = false;

  static Future<void> initialize() async {
    if (_initialized) return;

    try {
      final prefs = await SharedPreferences.getInstance();
      final configsJson = prefs.getStringList(_storageKey) ?? [];

      _configs.clear();
      for (final configJson in configsJson) {
        try {
          final configData = jsonDecode(configJson) as Map<String, dynamic>;
          final config = ClientConfig.fromJson(configData);
          _configs[config.serviceKey] = config;
        } catch (e) {
          // Skip invalid configuration entries
          continue;
        }
      }
    } catch (e) {
      // If SharedPreferences fails, continue with empty configs
    }

    _initialized = true;
  }

  static Future<void> _saveToStorage() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final configsJson = _configs.values
          .map((config) => jsonEncode(config.toJson()))
          .toList();
      await prefs.setStringList(_storageKey, configsJson);
    } catch (e) {
      // Handle storage error silently
    }
  }

  static Future<List<ClientConfig>> getAllConfigs() async {
    await initialize();
    return _configs.values.toList()
      ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
  }

  static Future<ClientConfig?> getConfig(String serviceKey) async {
    await initialize();
    return _configs[serviceKey];
  }

  static Future<bool> saveConfig(ClientConfig config) async {
    await initialize();
    _configs[config.serviceKey] = config;
    await _saveToStorage();
    return true;
  }

  static Future<bool> deleteConfig(String serviceKey) async {
    await initialize();
    final removed = _configs.remove(serviceKey) != null;
    if (removed) {
      await _saveToStorage();
    }
    return removed;
  }

  static Future<bool> updateStatus(
    String serviceKey,
    ClientStatus status,
    String message,
  ) async {
    await initialize();
    final config = _configs[serviceKey];
    if (config != null) {
      config.updateStatus(status, message);
      await _saveToStorage();
      return true;
    }
    return false;
  }

  static Future<void> clearAll() async {
    await initialize();
    _configs.clear();
    await _saveToStorage();
  }
}
