import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';

void main() {
  group('LogManager threshold filter', () {
    test('DEBUG includes DEBUG and higher severity logs', () {
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'DEBUG',
          entryLevel: 'DEBUG',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'DEBUG',
          entryLevel: 'INFO',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'DEBUG',
          entryLevel: 'WARN',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'DEBUG',
          entryLevel: 'ERROR',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'DEBUG',
          entryLevel: 'TRACE',
        ),
        isFalse,
      );
    });

    test('TRACE includes all standard levels', () {
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'TRACE',
          entryLevel: 'TRACE',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'TRACE',
          entryLevel: 'DEBUG',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'TRACE',
          entryLevel: 'INFO',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'TRACE',
          entryLevel: 'WARN',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'TRACE',
          entryLevel: 'ERROR',
        ),
        isTrue,
      );
    });

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

    test('normalizeLevel supports wrapped and alias values', () {
      expect(LogManager.normalizeLevel('[warn]'), 'WARN');
      expect(LogManager.normalizeLevel('warning'), 'WARN');
      expect(LogManager.normalizeLevel(' level=error '), 'ERROR');
      expect(LogManager.normalizeLevel('unknown-level'), 'UNKNOWN');
    });

    test('invalid threshold falls back to no-op filtering', () {
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'all',
          entryLevel: 'TRACE',
        ),
        isTrue,
      );
      expect(
        LogManager.includesThreshold(
          thresholdLevel: 'all',
          entryLevel: 'DEBUG',
        ),
        isTrue,
      );
    });
  });
}
