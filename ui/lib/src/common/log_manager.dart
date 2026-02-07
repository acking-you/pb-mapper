import 'dart:async';

import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

class LogManager {
  static final LogManager _instance = LogManager._internal();
  factory LogManager() => _instance;
  LogManager._internal();

  static const int _maxLogLines = 1000;
  static const List<String> logLevels = [
    'ERROR',
    'WARN',
    'INFO',
    'DEBUG',
    'TRACE',
  ];

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

  List<LogMessage> filterLogs({String? levelFilter, String keyword = ''}) {
    final normalizedKeyword = keyword.trim().toLowerCase();
    return _logMessages
        .where((log) {
          if (levelFilter != null &&
              levelFilter.isNotEmpty &&
              log.level != levelFilter) {
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
