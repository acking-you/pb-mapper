import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:rinf/rinf.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/common/function.dart';

class LogDisplayWidget extends StatefulWidget {
  const LogDisplayWidget({super.key});

  @override
  State<LogDisplayWidget> createState() => _LogDisplayWidgetState();
}

class _LogDisplayWidgetState extends State<LogDisplayWidget>
    with WidgetsBindingObserver {
  final List<LogMessage> _logMessages = [];
  final ScrollController _scrollController = ScrollController();
  static const int _maxLogLines = 100;
  bool _isExpanded = true;
  bool _userScrolling = false;
  Timer? _debounceTimer;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    // Listen for log messages from Rust
    LogMessage.rustSignalStream.listen(_handleLogMessage);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _scrollController.dispose();
    _debounceTimer?.cancel();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    setState(() {});
  }

  void _handleLogMessage(RustSignalPack<LogMessage> signalPack) {
    _logMessages.add(signalPack.message);

    // Limit to 5000 log lines
    if (_logMessages.length > _maxLogLines) {
      _logMessages.removeRange(0, _logMessages.length - _maxLogLines);
    }

    // Throttle UI updates for performance - only update every 1000 logs or when near limits
    if (_logMessages.length % 1000 == 0 ||
        _logMessages.length == _maxLogLines ||
        _logMessages.length < 100) {
      if (mounted) {
        setState(() {});
      }
    }

    // Auto-scroll to bottom if user hasn't manually scrolled up
    if (!_userScrolling && _isExpanded) {
      // Use a microtask to ensure scroll happens after build
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (_scrollController.hasClients) {
          _scrollController.jumpTo(_scrollController.position.maxScrollExtent);
        }
      });
    }
  }

  void _scrollToBottom() {
    if (_scrollController.hasClients) {
      _scrollController.animateTo(
        _scrollController.position.maxScrollExtent,
        duration: const Duration(milliseconds: 100),
        curve: Curves.easeInOut,
      );
    }
  }

  void _toggleExpand() {
    setState(() {
      _isExpanded = !_isExpanded;
      if (_isExpanded) {
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (_scrollController.hasClients) {
            _scrollToBottom();
          }
        });
      }
    });
  }

  Color _getLogColor(String level, bool isDarkMode) {
    switch (level) {
      case 'ERROR':
        return isDarkMode ? Colors.red.shade400 : Colors.red.shade900;
      case 'WARN':
        return isDarkMode ? Colors.orange.shade400 : Colors.orange.shade700;
      case 'INFO':
        return isDarkMode ? Colors.blue.shade400 : Colors.blue.shade600;
      case 'DEBUG':
        return isDarkMode ? Colors.green.shade400 : Colors.green.shade600;
      case 'TRACE':
        return isDarkMode ? Colors.grey.shade500 : Colors.grey.shade600;
      default:
        return isDarkMode ? Colors.white : Colors.black;
    }
  }

  Color _getTextColor(bool isDarkMode) {
    return isDarkMode ? Colors.white : Colors.black;
  }

  Color _getTimestampColor(bool isDarkMode) {
    return isDarkMode ? Colors.grey.shade500 : Colors.grey.shade700;
  }

  @override
  Widget build(BuildContext context) {
    final isDarkMode = Theme.of(context).brightness == Brightness.dark;

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16.0),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text('Logs', style: Theme.of(context).textTheme.titleLarge),
                Row(
                  children: [
                    IconButton(
                      icon: const Icon(Icons.arrow_downward),
                      onPressed: _scrollToBottom,
                      tooltip: 'Scroll to bottom',
                    ),
                    IconButton(
                      icon: Icon(
                        _isExpanded ? Icons.expand_less : Icons.expand_more,
                      ),
                      onPressed: _toggleExpand,
                      tooltip: _isExpanded ? 'Collapse logs' : 'Expand logs',
                    ),
                  ],
                ),
              ],
            ),
            const SizedBox(height: 16),
            if (_isExpanded)
              Container(
                height: 300,
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
                        // User is scrolling
                        _userScrolling = true;
                        // Cancel any existing timer
                        _debounceTimer?.cancel();
                      } else if (notification.direction ==
                          ScrollDirection.idle) {
                        // User stopped scrolling, start a timer to reset flag
                        _debounceTimer?.cancel();
                        _debounceTimer = Timer(
                          const Duration(milliseconds: 300),
                          () {
                            if (mounted) {
                              // Check if we're at the bottom
                              if (_scrollController.hasClients) {
                                final maxScroll =
                                    _scrollController.position.maxScrollExtent;
                                final currentScroll =
                                    _scrollController.position.pixels;
                                setState(() {
                                  _userScrolling =
                                      maxScroll - currentScroll > 20;
                                });
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
                      itemCount: _logMessages.length,
                      // Remove fixed item extent to allow multi-line selection
                      cacheExtent: 100,
                      itemBuilder: (context, index) {
                        // Performance optimization: only build what's needed
                        final log = _logMessages[index];
                        return Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 8.0,
                            vertical: 6.0,
                          ),
                          decoration: BoxDecoration(
                            border: Border(
                              bottom: BorderSide(
                                color: isDarkMode
                                    ? Colors.grey.shade800
                                    : Colors.grey.shade200,
                                width: 0.5,
                              ),
                            ),
                          ),
                          child: SelectableText.rich(
                            TextSpan(
                              children: [
                                TextSpan(
                                  text: '[${log.level}]'.padRight(8),
                                  style: TextStyle(
                                    fontSize: 18,
                                    color: _getLogColor(log.level, isDarkMode),
                                    fontWeight: FontWeight.bold,
                                    fontFamily: 'monospace',
                                  ),
                                ),
                                TextSpan(
                                  text: ' ',
                                  style: TextStyle(
                                    fontSize: 18,
                                    fontFamily: 'monospace',
                                  ),
                                ),
                                TextSpan(
                                  text:
                                      '${DateTime.fromMillisecondsSinceEpoch(log.timestamp.toInt() * 1000).toString().split('.')[0].padRight(20)}',
                                  style: TextStyle(
                                    color: _getTimestampColor(isDarkMode),
                                    fontSize: 18,
                                    fontFamily: 'monospace',
                                  ),
                                ),
                                TextSpan(
                                  text: ' : ',
                                  style: TextStyle(
                                    color: _getTimestampColor(isDarkMode),
                                    fontSize: 18,
                                    fontFamily: 'monospace',
                                  ),
                                ),
                                TextSpan(
                                  text: log.message,
                                  style: TextStyle(
                                    fontSize: 18,
                                    color: _getTextColor(isDarkMode),
                                    fontFamily: 'monospace',
                                  ),
                                ),
                              ],
                            ),
                            style: const TextStyle(height: 1.4),
                          ),
                        );
                      },
                    ),
                  ),
                ),
              ),
            if (_isExpanded) const SizedBox(height: 8),
            if (_isExpanded)
              Text(
                '${_logMessages.length}/$_maxLogLines logs',
                style: TextStyle(
                  fontSize: 12,
                  color: _getTimestampColor(isDarkMode),
                ),
              ),
          ],
        ),
      ),
    );
  }
}
