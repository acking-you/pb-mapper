import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/bindings/bindings.dart';
import 'dart:async';

class LogViewPage extends StatefulWidget {
  const LogViewPage({super.key});

  @override
  State<LogViewPage> createState() => _LogViewPageState();
}

class _LogViewPageState extends State<LogViewPage> {
  final LogManager _logManager = LogManager();
  final ScrollController _scrollController = ScrollController();
  bool _userScrolling = false;
  Timer? _debounceTimer;
  List<LogMessage> _currentLogs = [];
  String? _levelFilter;

  @override
  void initState() {
    super.initState();

    _logManager.initialize();

    _logManager.logStream.listen((logs) {
      if (mounted) {
        setState(() {
          _currentLogs = _levelFilter != null
              ? _logManager.getFilteredLogs(_levelFilter)
              : logs;
        });
        _handleAutoScroll();
      }
    });

    _currentLogs = _logManager.logs;
  }

  @override
  void dispose() {
    _scrollController.dispose();
    _debounceTimer?.cancel();
    super.dispose();
  }

  void _handleAutoScroll() {
    if (!_userScrolling && mounted) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted &&
            _scrollController.hasClients &&
            _scrollController.position.hasContentDimensions) {
          try {
            _scrollController.jumpTo(
              _scrollController.position.maxScrollExtent,
            );
          } catch (e) {
            // Ignore scroll errors
          }
        }
      });
    }
  }

  void _scrollToBottom() {
    if (mounted &&
        _scrollController.hasClients &&
        _scrollController.position.hasContentDimensions) {
      try {
        _scrollController.animateTo(
          _scrollController.position.maxScrollExtent,
          duration: const Duration(milliseconds: 100),
          curve: Curves.easeInOut,
        );
      } catch (e) {
        // Ignore animation errors
      }
    }
  }

  void _copyAllLogs() async {
    final allLogsText = _currentLogs
        .map((log) {
          final timestamp = DateTime.fromMillisecondsSinceEpoch(
            log.timestamp.toInt() * 1000,
          ).toString().split('.')[0];
          return '[${log.level}] $timestamp : ${log.message}';
        })
        .join('\n');

    await Clipboard.setData(ClipboardData(text: allLogsText));

    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('${_currentLogs.length} logs copied to clipboard'),
          duration: const Duration(seconds: 2),
        ),
      );
    }
  }

  void _clearLogs() {
    _logManager.clearLogs();
    setState(() {
      _currentLogs.clear();
    });
  }

  Color _getTextColor(bool isDarkMode) {
    return isDarkMode ? Colors.white : Colors.black;
  }

  Color _getTimestampColor(bool isDarkMode) {
    return isDarkMode ? Colors.grey.shade500 : Colors.grey.shade700;
  }

  Color _getLevelColor(String level, bool isDarkMode) {
    switch (level.toUpperCase()) {
      case 'ERROR':
        return isDarkMode ? Colors.red.shade300 : Colors.red.shade700;
      case 'WARN':
        return isDarkMode ? Colors.orange.shade300 : Colors.orange.shade700;
      case 'INFO':
        return isDarkMode ? Colors.blue.shade300 : Colors.blue.shade700;
      case 'DEBUG':
        return isDarkMode ? Colors.green.shade300 : Colors.green.shade700;
      case 'TRACE':
        return isDarkMode ? Colors.grey.shade400 : Colors.grey.shade600;
      default:
        return _getTextColor(isDarkMode);
    }
  }

  @override
  Widget build(BuildContext context) {
    final isDarkMode = Theme.of(context).brightness == Brightness.dark;

    return Scaffold(
      appBar: AppBar(
        title: const Text('Logs'),
        actions: [
          if (!ResponsiveLayout.isMobile(context)) ...[
            // Level filter dropdown
            Padding(
              padding: const EdgeInsets.only(right: 8.0),
              child: DropdownButton<String?>(
                value: _levelFilter,
                hint: const Text('All'),
                items: const [
                  DropdownMenuItem(value: null, child: Text('All')),
                  DropdownMenuItem(value: 'ERROR', child: Text('ERROR')),
                  DropdownMenuItem(value: 'WARN', child: Text('WARN')),
                  DropdownMenuItem(value: 'INFO', child: Text('INFO')),
                  DropdownMenuItem(value: 'DEBUG', child: Text('DEBUG')),
                  DropdownMenuItem(value: 'TRACE', child: Text('TRACE')),
                ],
                onChanged: (value) {
                  if (mounted) {
                    setState(() {
                      _levelFilter = value;
                      _currentLogs = _levelFilter != null
                          ? _logManager.getFilteredLogs(_levelFilter)
                          : _logManager.logs;
                    });
                  }
                },
              ),
            ),
            IconButton(
              icon: const Icon(Icons.copy),
              onPressed: _copyAllLogs,
              tooltip: 'Copy all logs',
            ),
            IconButton(
              icon: const Icon(Icons.clear),
              onPressed: _clearLogs,
              tooltip: 'Clear logs',
            ),
            IconButton(
              icon: const Icon(Icons.arrow_downward),
              onPressed: _scrollToBottom,
              tooltip: 'Scroll to bottom',
            ),
          ] else ...[
            PopupMenuButton<String>(
              onSelected: (value) {
                switch (value) {
                  case 'copy':
                    _copyAllLogs();
                    break;
                  case 'clear':
                    _clearLogs();
                    break;
                  case 'scroll':
                    _scrollToBottom();
                    break;
                }
              },
              itemBuilder: (context) => [
                const PopupMenuItem(value: 'copy', child: Text('Copy Logs')),
                const PopupMenuItem(value: 'clear', child: Text('Clear Logs')),
                const PopupMenuItem(
                  value: 'scroll',
                  child: Text('Scroll to Bottom'),
                ),
              ],
            ),
          ],
        ],
      ),
      body: Column(
        children: [
          if (ResponsiveLayout.isMobile(context))
            Container(
              padding: ResponsiveLayout.getScreenPadding(context),
              child: Row(
                children: [
                  Expanded(
                    child: DropdownButton<String?>(
                      value: _levelFilter,
                      hint: const Text('Filter by level'),
                      isExpanded: true,
                      items: const [
                        DropdownMenuItem(value: null, child: Text('All')),
                        DropdownMenuItem(value: 'ERROR', child: Text('ERROR')),
                        DropdownMenuItem(value: 'WARN', child: Text('WARN')),
                        DropdownMenuItem(value: 'INFO', child: Text('INFO')),
                        DropdownMenuItem(value: 'DEBUG', child: Text('DEBUG')),
                        DropdownMenuItem(value: 'TRACE', child: Text('TRACE')),
                      ],
                      onChanged: (value) {
                        if (mounted) {
                          setState(() {
                            _levelFilter = value;
                            _currentLogs = _levelFilter != null
                                ? _logManager.getFilteredLogs(_levelFilter)
                                : _logManager.logs;
                          });
                        }
                      },
                    ),
                  ),
                ],
              ),
            ),
          Expanded(
            child: Container(
              margin: ResponsiveLayout.getScreenPadding(context),
              decoration: BoxDecoration(
                border: Border.all(
                  color: isDarkMode
                      ? Colors.grey.shade700
                      : Colors.grey.shade300,
                ),
                borderRadius: BorderRadius.circular(4),
              ),
              child: NotificationListener<ScrollNotification>(
                onNotification: (notification) {
                  if (notification is UserScrollNotification) {
                    if (notification.direction == ScrollDirection.forward ||
                        notification.direction == ScrollDirection.reverse) {
                      _userScrolling = true;
                      _debounceTimer?.cancel();
                    } else if (notification.direction == ScrollDirection.idle) {
                      _debounceTimer?.cancel();
                      _debounceTimer = Timer(
                        const Duration(milliseconds: 300),
                        () {
                          if (mounted) {
                            if (_scrollController.hasClients &&
                                _scrollController
                                    .position
                                    .hasContentDimensions) {
                              try {
                                final maxScroll =
                                    _scrollController.position.maxScrollExtent;
                                final currentScroll =
                                    _scrollController.position.pixels;
                                setState(() {
                                  _userScrolling =
                                      maxScroll - currentScroll > 20;
                                });
                              } catch (e) {
                                setState(() {
                                  _userScrolling = false;
                                });
                              }
                            } else {
                              setState(() {
                                _userScrolling = false;
                              });
                            }
                          }
                        },
                      );
                    }
                  }
                  return false;
                },
                child: Scrollbar(
                  controller: _scrollController,
                  child: ListView.builder(
                    controller: _scrollController,
                    itemCount: _currentLogs.length,
                    physics: const AlwaysScrollableScrollPhysics(),
                    padding: ResponsiveLayout.getScreenPadding(context),
                    itemBuilder: (context, index) {
                      final log = _currentLogs[index];
                      final timestamp = DateTime.fromMillisecondsSinceEpoch(
                        log.timestamp.toInt() * 1000,
                      ).toString().split('.')[0];

                      final baseFontSize = ResponsiveLayout.isMobile(context)
                          ? 14.0
                          : 16.0;

                      return Padding(
                        padding: EdgeInsets.only(
                          bottom:
                              ResponsiveLayout.getVerticalSpacing(context) / 3,
                        ),
                        child: SelectableText.rich(
                          TextSpan(
                            children: [
                              TextSpan(
                                text: '[${log.level.padRight(5)}] ',
                                style: TextStyle(
                                  fontSize: baseFontSize,
                                  fontFamily: 'monospace',
                                  color: _getLevelColor(log.level, isDarkMode),
                                  fontWeight: FontWeight.bold,
                                ),
                              ),
                              TextSpan(
                                text: '$timestamp : ',
                                style: TextStyle(
                                  fontSize: baseFontSize,
                                  fontFamily: 'monospace',
                                  color: _getTimestampColor(isDarkMode),
                                ),
                              ),
                              TextSpan(
                                text: log.message,
                                style: TextStyle(
                                  fontSize: baseFontSize,
                                  fontFamily: 'monospace',
                                  color: _getTextColor(isDarkMode),
                                ),
                              ),
                            ],
                          ),
                          style: const TextStyle(height: 1.3),
                        ),
                      );
                    },
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
      bottomNavigationBar: Container(
        padding: ResponsiveLayout.getScreenPadding(context),
        child: Text(
          '${_currentLogs.length}/${_logManager.maxLogLines} logs${_levelFilter != null ? ' (filtered by $_levelFilter)' : ''}',
          style: TextStyle(
            fontSize: ResponsiveLayout.getFontSize(context, 12),
            color: _getTimestampColor(isDarkMode),
          ),
          textAlign: TextAlign.center,
        ),
      ),
    );
  }
}
