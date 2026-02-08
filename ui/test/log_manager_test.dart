import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';

void main() {
  group('LogManager threshold filter', () {
    test('INFO includes INFO and higher severity logs', () {
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'INFO',
          entryLevel: 'INFO',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'INFO',
          entryLevel: 'WARN',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'INFO',
          entryLevel: 'ERROR',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'INFO',
          entryLevel: 'DEBUG',
        ),
        isFalse,
      );
    });

    test('WARN includes WARN and ERROR only', () {
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'WARN',
          entryLevel: 'WARN',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'WARN',
          entryLevel: 'ERROR',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'WARN',
          entryLevel: 'INFO',
        ),
        isFalse,
      );
    });

    test('threshold labels use plus suffix except ERROR', () {
      expect(LogManager.getThresholdLabel('TRACE'), 'TRACE+');
      expect(LogManager.getThresholdLabel('DEBUG'), 'DEBUG+');
      expect(LogManager.getThresholdLabel('INFO'), 'INFO+');
      expect(LogManager.getThresholdLabel('WARN'), 'WARN+');
      expect(LogManager.getThresholdLabel('ERROR'), 'ERROR');
    });
  });
}
