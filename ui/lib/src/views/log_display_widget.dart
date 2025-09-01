import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/bindings/bindings.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';

class LogDisplayWidget extends StatefulWidget {
  const LogDisplayWidget({super.key});

  @override
  State<LogDisplayWidget> createState() => _LogDisplayWidgetState();
}

class _LogDisplayWidgetState extends State<LogDisplayWidget>
    with WidgetsBindingObserver {
  final LogManager _logManager = LogManager();
  final ScrollController _scrollController = ScrollController();
  final TextEditingController _selectAllController = TextEditingController();
  bool _isExpanded = true;
  bool _userScrolling = false;
  Timer? _debounceTimer;
  List<LogMessage> _currentLogs = [];
  String? _levelFilter;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);

    // Initialize log manager if not already done
    _logManager.initialize();

    // Listen for log updates from the manager
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

    // Initialize with existing logs
    _currentLogs = _logManager.logs;
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _scrollController.dispose();
    _selectAllController.dispose();
    _debounceTimer?.cancel();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    setState(() {});
  }

  void _handleAutoScroll() {
    // Auto-scroll to bottom if user hasn't manually scrolled up
    if (!_userScrolling && _isExpanded && mounted) {
      // Use a microtask to ensure scroll happens after build
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted &&
            _scrollController.hasClients &&
            _scrollController.position.hasContentDimensions) {
          try {
            _scrollController.jumpTo(
              _scrollController.position.maxScrollExtent,
            );
          } catch (e) {
            // Ignore scroll errors during widget disposal or rebuild
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
        // Ignore animation errors during widget disposal
      }
    }
  }

  void _toggleExpand() {
    setState(() {
      _isExpanded = !_isExpanded;
      if (_isExpanded) {
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) {
            _scrollToBottom();
          }
        });
      }
    });
  }

  void _selectAllLogs() {
    // Update the hidden text field with all log content
    final allLogsText = _currentLogs
        .map((log) {
          final timestamp = DateTime.fromMillisecondsSinceEpoch(
            log.timestamp.toInt() * 1000,
          ).toString().split('.')[0];
          return '[${log.level}] $timestamp : ${log.message}';
        })
        .join('\n');

    _selectAllController.text = allLogsText;
    _selectAllController.selection = TextSelection(
      baseOffset: 0,
      extentOffset: allLogsText.length,
    );

    // Copy to clipboard automatically
    Clipboard.setData(ClipboardData(text: allLogsText));

    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('${_currentLogs.length} logs selected and copied'),
          duration: const Duration(seconds: 2),
        ),
      );
    }
  }

  void _copySelectedLogs() async {
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

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16.0),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text('Logs', style: Theme.of(context).textTheme.titleLarge),
                Row(
                  children: [
                    // Level filter dropdown
                    DropdownButton<String?>(
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
                    const SizedBox(width: 8),
                    IconButton(
                      icon: const Icon(Icons.select_all),
                      onPressed: _selectAllLogs,
                      tooltip: 'Select all logs',
                    ),
                    IconButton(
                      icon: const Icon(Icons.copy),
                      onPressed: _copySelectedLogs,
                      tooltip: 'Copy all logs to clipboard',
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
                child: Stack(
                  children: [
                    // Hidden text field for "Select All" functionality
                    Positioned(
                      top: 0,
                      left: 0,
                      width: 0,
                      height: 0,
                      child: TextField(
                        controller: _selectAllController,
                        style: const TextStyle(fontSize: 0),
                        decoration: const InputDecoration.collapsed(
                          hintText: '',
                        ),
                        maxLines: null,
                      ),
                    ),
                    // Main log display
                    Positioned.fill(
                      child: NotificationListener<ScrollNotification>(
                        onNotification: (notification) {
                          if (notification is UserScrollNotification) {
                            if (notification.direction ==
                                    ScrollDirection.forward ||
                                notification.direction ==
                                    ScrollDirection.reverse) {
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
                                    if (_scrollController.hasClients &&
                                        _scrollController
                                            .position
                                            .hasContentDimensions) {
                                      try {
                                        final maxScroll = _scrollController
                                            .position
                                            .maxScrollExtent;
                                        final currentScroll =
                                            _scrollController.position.pixels;
                                        setState(() {
                                          _userScrolling =
                                              maxScroll - currentScroll > 20;
                                        });
                                      } catch (e) {
                                        // Handle position access errors
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
                            itemBuilder: (context, index) {
                              final log = _currentLogs[index];
                              final timestamp =
                                  DateTime.fromMillisecondsSinceEpoch(
                                    log.timestamp.toInt() * 1000,
                                  ).toString().split('.')[0];

                              return SelectableText.rich(
                                TextSpan(
                                  children: [
                                    TextSpan(
                                      text: '[${log.level.padRight(5)}] ',
                                      style: TextStyle(
                                        fontSize: 14,
                                        fontFamily: 'monospace',
                                        color: _getLevelColor(
                                          log.level,
                                          isDarkMode,
                                        ),
                                        fontWeight: FontWeight.bold,
                                      ),
                                    ),
                                    TextSpan(
                                      text: '$timestamp : ',
                                      style: TextStyle(
                                        fontSize: 14,
                                        fontFamily: 'monospace',
                                        color: _getTimestampColor(isDarkMode),
                                      ),
                                    ),
                                    TextSpan(
                                      text: log.message,
                                      style: TextStyle(
                                        fontSize: 14,
                                        fontFamily: 'monospace',
                                        color: _getTextColor(isDarkMode),
                                      ),
                                    ),
                                  ],
                                ),
                                style: const TextStyle(height: 1.2),
                              );
                            },
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            if (_isExpanded) const SizedBox(height: 8),
            if (_isExpanded)
              Text(
                '${_currentLogs.length}/${_logManager.maxLogLines} logs${_levelFilter != null ? ' (filtered by $_levelFilter)' : ''}',
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
