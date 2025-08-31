import 'dart:convert';
import 'package:shared_preferences/shared_preferences.dart';

enum ServiceStatus {
  running, // Green - service is running normally
  retrying, // Yellow - in retry connection loop (matching pb-mapper retry logic)
  failed, // Red - connection failed or remote server unavailable
  stopped, // Grey - service is stopped
}

class ServiceConfig {
  final String serviceKey;
  final String localAddress;
  final String protocol;
  final bool enableEncryption;
  final bool enableKeepAlive;
  final DateTime createdAt;
  DateTime updatedAt;
  ServiceStatus status;
  String statusMessage;

  ServiceConfig({
    required this.serviceKey,
    required this.localAddress,
    required this.protocol,
    required this.enableEncryption,
    required this.enableKeepAlive,
    DateTime? createdAt,
    DateTime? updatedAt,
    this.status = ServiceStatus.stopped,
    this.statusMessage = '',
  }) : createdAt = createdAt ?? DateTime.now(),
       updatedAt = updatedAt ?? DateTime.now();

  // JSON serialization
  Map<String, dynamic> toJson() => {
    'serviceKey': serviceKey,
    'localAddress': localAddress,
    'protocol': protocol,
    'enableEncryption': enableEncryption,
    'enableKeepAlive': enableKeepAlive,
    'createdAt': createdAt.toIso8601String(),
    'updatedAt': updatedAt.toIso8601String(),
    'status': status.name,
    'statusMessage': statusMessage,
  };

  factory ServiceConfig.fromJson(Map<String, dynamic> json) => ServiceConfig(
    serviceKey: json['serviceKey'] ?? '',
    localAddress: json['localAddress'] ?? '',
    protocol: json['protocol'] ?? 'TCP',
    enableEncryption: json['enableEncryption'] ?? false,
    enableKeepAlive: json['enableKeepAlive'] ?? false,
    createdAt: DateTime.tryParse(json['createdAt'] ?? '') ?? DateTime.now(),
    updatedAt: DateTime.tryParse(json['updatedAt'] ?? '') ?? DateTime.now(),
    status: ServiceStatus.values.firstWhere(
      (e) => e.name == json['status'],
      orElse: () => ServiceStatus.stopped,
    ),
    statusMessage: json['statusMessage'] ?? '',
  );

  ServiceConfig copyWith({
    String? serviceKey,
    String? localAddress,
    String? protocol,
    bool? enableEncryption,
    bool? enableKeepAlive,
    ServiceStatus? status,
    String? statusMessage,
  }) => ServiceConfig(
    serviceKey: serviceKey ?? this.serviceKey,
    localAddress: localAddress ?? this.localAddress,
    protocol: protocol ?? this.protocol,
    enableEncryption: enableEncryption ?? this.enableEncryption,
    enableKeepAlive: enableKeepAlive ?? this.enableKeepAlive,
    createdAt: createdAt,
    updatedAt: DateTime.now(),
    status: status ?? this.status,
    statusMessage: statusMessage ?? this.statusMessage,
  );

  void updateStatus(ServiceStatus newStatus, String message) {
    status = newStatus;
    statusMessage = message;
    updatedAt = DateTime.now();
  }
}

// Service configuration storage manager
class ServiceConfigManager {
  static const String _storageKey = 'service_configurations';

  // In-memory cache of configurations
  static final Map<String, ServiceConfig> _configs = {};
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
          final config = ServiceConfig.fromJson(configData);
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

  static Future<List<ServiceConfig>> getAllConfigs() async {
    await initialize();
    return _configs.values.toList()
      ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
  }

  static Future<ServiceConfig?> getConfig(String serviceKey) async {
    await initialize();
    return _configs[serviceKey];
  }

  static Future<bool> saveConfig(ServiceConfig config) async {
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
    ServiceStatus status,
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
