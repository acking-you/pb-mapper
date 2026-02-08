import 'dart:async';

import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

class LogManager {
  static final LogManager _instance = LogManager._internal();
  factory LogManager() => _instance;
  LogManager._internal();

  static const int _maxLogLines = 1000;
  static const List<String> logLevels = [
    'TRACE',
    'DEBUG',
    'INFO',
    'WARN',
    'ERROR',
  ];
  static const Map<String, int> _levelPriority = {
    'TRACE': 0,
    'DEBUG': 1,
    'INFO': 2,
    'WARN': 3,
    'ERROR': 4,
  };

  final List<LogMessage> _logMessages = [];
  final StreamController<List<LogMessage>> _logStreamController =
      StreamController<List<LogMessage>>.broadcast();

  StreamSubscription<LogMessage>? _logSubscription;
  bool _initialized = false;

  Stream<List<LogMessage>> get logStream => _logStreamController.stream;
  List<LogMessage> get logs => List.unmodifiable(_logMessages);
  int get logCount => _logMessages.length;
  int get maxLogLines => _maxLogLines;

  void initialize() {
    if (_initialized) {
      return;
    }
    _logSubscription = PbMapperService.logStream.listen(_handleLogMessage);
    _initialized = true;
    _emitSnapshot();
  }

  void dispose() {
    _logSubscription?.cancel();
    _logSubscription = null;
    _initialized = false;
  }

  void _handleLogMessage(LogMessage message) {
    _logMessages.add(message);
    if (_logMessages.length > _maxLogLines) {
      _logMessages.removeRange(0, _logMessages.length - _maxLogLines);
    }
    _emitSnapshot();
  }

  void _emitSnapshot() {
    if (!_logStreamController.isClosed) {
      _logStreamController.add(List.unmodifiable(_logMessages));
    }
  }

  void clearLogs() {
    _logMessages.clear();
    _emitSnapshot();
  }

  static String normalizeLevel(String level) {
    final normalized = level.trim().toUpperCase();
    if (_levelPriority.containsKey(normalized)) {
      return normalized;
    }
    return 'UNKNOWN';
  }

  static bool includesThreshold({
    required String thresholdLevel,
    required String entryLevel,
  }) {
    final threshold = _levelPriority[normalizeLevel(thresholdLevel)];
    final entry = _levelPriority[normalizeLevel(entryLevel)];
    if (threshold == null || entry == null) {
      return normalizeLevel(entryLevel) == normalizeLevel(thresholdLevel);
    }
    return entry >= threshold;
  }

  static String getThresholdLabel(String level) {
    final normalized = normalizeLevel(level);
    if (normalized == 'ERROR' || normalized == 'UNKNOWN') {
      return normalized;
    }
    return '$normalized+';
  }

  List<LogMessage> filterLogs({String? levelFilter, String keyword = ''}) {
    final normalizedLevelFilter =
        (levelFilter == null || levelFilter.trim().isEmpty)
        ? null
        : normalizeLevel(levelFilter);
    final normalizedKeyword = keyword.trim().toLowerCase();
    return _logMessages
        .where((log) {
          if (normalizedLevelFilter != null &&
              normalizedLevelFilter.isNotEmpty &&
              !includesThreshold(
                thresholdLevel: normalizedLevelFilter,
                entryLevel: log.level,
              )) {
            return false;
          }
          if (normalizedKeyword.isEmpty) {
            return true;
          }
          final message = log.message.toLowerCase();
          final level = log.level.toLowerCase();
          return message.contains(normalizedKeyword) ||
              level.contains(normalizedKeyword);
        })
        .toList(growable: false);
  }

  String formatLogLine(LogMessage log) {
    final timestamp = DateTime.fromMillisecondsSinceEpoch(
      log.timestamp * 1000,
    ).toString().split('.').first;
    return '[${log.level}] $timestamp : ${log.message}';
  }

  String getAllLogsAsText({String? levelFilter, String keyword = ''}) {
    return filterLogs(
      levelFilter: levelFilter,
      keyword: keyword,
    ).map(formatLogLine).join('\n');
  }
}
