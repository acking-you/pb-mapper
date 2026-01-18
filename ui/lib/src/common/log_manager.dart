import 'dart:async';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

class LogManager {
  static final LogManager _instance = LogManager._internal();
  factory LogManager() => _instance;
  LogManager._internal();

  final List<LogMessage> _logMessages = [];
  static const int _maxLogLines = 1000;

  final StreamController<List<LogMessage>> _logStreamController =
      StreamController<List<LogMessage>>.broadcast();

  Stream<List<LogMessage>> get logStream => _logStreamController.stream;
  List<LogMessage> get logs => List.unmodifiable(_logMessages);
  int get logCount => _logMessages.length;
  int get maxLogLines => _maxLogLines;

  late StreamSubscription<LogMessage> _logSubscription;

  void initialize() {
    _logSubscription = PbMapperService.logStream.listen(_handleLogMessage);
  }

  void dispose() {
    _logSubscription.cancel();
    _logStreamController.close();
  }

  void _handleLogMessage(LogMessage message) {
    _logMessages.add(message);

    // Limit log lines to prevent memory issues
    if (_logMessages.length > _maxLogLines) {
      _logMessages.removeRange(0, _logMessages.length - _maxLogLines);
    }

    // Notify all listeners
    _logStreamController.add(List.from(_logMessages));
  }

  void clearLogs() {
    _logMessages.clear();
    _logStreamController.add(List.from(_logMessages));
  }

  String getAllLogsAsText() {
    return _logMessages
        .map((log) {
          final timestamp = DateTime.fromMillisecondsSinceEpoch(
            log.timestamp.toInt() * 1000,
          ).toString().split('.')[0];
          return '[${log.level}] $timestamp : ${log.message}';
        })
        .join('\n');
  }

  List<LogMessage> getFilteredLogs(String? levelFilter) {
    if (levelFilter == null || levelFilter.isEmpty) {
      return _logMessages;
    }
    return _logMessages.where((log) => log.level == levelFilter).toList();
  }
}
