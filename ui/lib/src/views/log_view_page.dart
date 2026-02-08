import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

class LogViewPage extends StatefulWidget {
  final bool showScaffold;

  const LogViewPage({super.key, this.showScaffold = true});

  @override
  State<LogViewPage> createState() => _LogViewPageState();
}

class _LogViewPageState extends State<LogViewPage> {
  final LogManager _logManager = LogManager();
  final ScrollController _scrollController = ScrollController();
  final TextEditingController _keywordController = TextEditingController();

  StreamSubscription<List<LogMessage>>? _logSubscription;
  Timer? _keywordDebounce;
  String? _levelFilter;
  String _keywordFilter = '';
  bool _followTail = true;
  List<LogMessage> _filteredLogs = [];

  @override
  void initState() {
    super.initState();
    _logManager.initialize();
    _filteredLogs = _logManager.filterLogs();
    _logSubscription = _logManager.logStream.listen((_) {
      if (!mounted) {
        return;
      }
      _applyFilters(refreshScroll: true);
    });
  }

  @override
  void dispose() {
    _keywordDebounce?.cancel();
    _logSubscription?.cancel();
    _scrollController.dispose();
    _keywordController.dispose();
    super.dispose();
  }

  void _applyFilters({bool refreshScroll = false}) {
    final next = _logManager.filterLogs(
      levelFilter: _levelFilter,
      keyword: _keywordFilter,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _filteredLogs = next;
    });

    if (refreshScroll && _followTail) {
      _scrollToBottom(jump: true);
    }
  }

  void _onKeywordChanged(String value) {
    _keywordDebounce?.cancel();
    _keywordDebounce = Timer(const Duration(milliseconds: 150), () {
      if (!mounted) {
        return;
      }
      _keywordFilter = value.trim();
      _applyFilters();
    });
  }

  void _scrollToBottom({bool jump = false}) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted ||
          !_scrollController.hasClients ||
          !_scrollController.position.hasContentDimensions) {
        return;
      }
      final target = _scrollController.position.maxScrollExtent;
      if (jump) {
        _scrollController.jumpTo(target);
        return;
      }
      _scrollController.animateTo(
        target,
        duration: const Duration(milliseconds: 160),
        curve: Curves.easeOut,
      );
    });
  }

  Future<void> _copyCurrentLogs() async {
    final text = _logManager.getAllLogsAsText(
      levelFilter: _levelFilter,
      keyword: _keywordFilter,
    );
    await Clipboard.setData(ClipboardData(text: text));
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text('Copied ${_filteredLogs.length} logs')),
    );
  }

  void _clearLogs() {
    _logManager.clearLogs();
    _applyFilters();
  }

  void _toggleFollowTail() {
    setState(() {
      _followTail = !_followTail;
    });
    if (_followTail) {
      _scrollToBottom();
    }
  }

  Color _levelColor(String level, bool isDarkMode) {
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
        return isDarkMode ? Colors.grey.shade400 : Colors.grey.shade700;
      default:
        return isDarkMode ? Colors.white : Colors.black;
    }
  }

  Widget _buildToolbar(BuildContext context) {
    final isMobile = ResponsiveLayout.isMobile(context);
    return Padding(
      padding: ResponsiveLayout.getScreenPadding(context),
      child: Column(
        children: [
          Row(
            children: [
              Expanded(
                child: DropdownButtonFormField<String?>(
                  initialValue: _levelFilter,
                  isExpanded: true,
                  decoration: const InputDecoration(
                    labelText: 'Log Level',
                    border: OutlineInputBorder(),
                  ),
                  items: [
                    const DropdownMenuItem<String?>(
                      value: null,
                      child: Text('All'),
                    ),
                    ...LogManager.logLevels.map(
                      (level) => DropdownMenuItem<String?>(
                        value: level,
                        child: Text(LogManager.getThresholdLabel(level)),
                      ),
                    ),
                  ],
                  onChanged: (value) {
                    _levelFilter = value;
                    _applyFilters();
                  },
                ),
              ),
              if (!isMobile) ...[
                const SizedBox(width: 12),
                Expanded(
                  child: TextField(
                    controller: _keywordController,
                    onChanged: _onKeywordChanged,
                    decoration: InputDecoration(
                      labelText: 'Keyword',
                      hintText: 'error, timeout, service-key...',
                      prefixIcon: const Icon(Icons.search),
                      suffixIcon: _keywordFilter.isEmpty
                          ? null
                          : IconButton(
                              onPressed: () {
                                _keywordController.clear();
                                _keywordFilter = '';
                                _applyFilters();
                              },
                              icon: const Icon(Icons.clear),
                            ),
                      border: const OutlineInputBorder(),
                    ),
                  ),
                ),
              ],
            ],
          ),
          if (isMobile) ...[
            const SizedBox(height: 12),
            TextField(
              controller: _keywordController,
              onChanged: _onKeywordChanged,
              decoration: InputDecoration(
                labelText: 'Keyword',
                hintText: 'error, timeout, service-key...',
                prefixIcon: const Icon(Icons.search),
                suffixIcon: _keywordFilter.isEmpty
                    ? null
                    : IconButton(
                        onPressed: () {
                          _keywordController.clear();
                          _keywordFilter = '';
                          _applyFilters();
                        },
                        icon: const Icon(Icons.clear),
                      ),
                border: const OutlineInputBorder(),
              ),
            ),
          ],
        ],
      ),
    );
  }

  Widget _buildLogList(BuildContext context) {
    final isDarkMode = Theme.of(context).brightness == Brightness.dark;
    return Expanded(
      child: Container(
        margin: ResponsiveLayout.getScreenPadding(context),
        decoration: BoxDecoration(
          border: Border.all(
            color: isDarkMode ? Colors.grey.shade700 : Colors.grey.shade300,
          ),
          borderRadius: BorderRadius.circular(6),
        ),
        child: NotificationListener<ScrollNotification>(
          onNotification: (notification) {
            if (!_scrollController.hasClients ||
                !_scrollController.position.hasContentDimensions) {
              return false;
            }
            final distanceToBottom =
                _scrollController.position.maxScrollExtent -
                _scrollController.position.pixels;
            if (distanceToBottom > 40 && _followTail) {
              setState(() {
                _followTail = false;
              });
            } else if (distanceToBottom <= 4 && !_followTail) {
              setState(() {
                _followTail = true;
              });
            }
            return false;
          },
          child: Scrollbar(
            controller: _scrollController,
            child: ListView.builder(
              controller: _scrollController,
              padding: ResponsiveLayout.getScreenPadding(context),
              itemCount: _filteredLogs.length,
              itemBuilder: (context, index) {
                final log = _filteredLogs[index];
                final timestamp = DateTime.fromMillisecondsSinceEpoch(
                  log.timestamp * 1000,
                ).toString().split('.').first;
                return Padding(
                  padding: const EdgeInsets.only(bottom: 4),
                  child: SelectableText.rich(
                    TextSpan(
                      children: [
                        TextSpan(
                          text: '[${log.level.padRight(5)}] ',
                          style: TextStyle(
                            fontFamily: 'monospace',
                            fontWeight: FontWeight.bold,
                            color: _levelColor(log.level, isDarkMode),
                          ),
                        ),
                        TextSpan(
                          text: '$timestamp : ',
                          style: TextStyle(
                            fontFamily: 'monospace',
                            color: isDarkMode
                                ? Colors.grey.shade500
                                : Colors.grey.shade700,
                          ),
                        ),
                        TextSpan(
                          text: log.message,
                          style: TextStyle(
                            fontFamily: 'monospace',
                            color: isDarkMode ? Colors.white : Colors.black,
                          ),
                        ),
                      ],
                    ),
                  ),
                );
              },
            ),
          ),
        ),
      ),
    );
  }

  Widget _buildBottomBar(BuildContext context) {
    final filterParts = <String>[];
    if (_levelFilter != null) {
      filterParts.add('level=${LogManager.getThresholdLabel(_levelFilter!)}');
    }
    if (_keywordFilter.isNotEmpty) {
      filterParts.add('keyword="$_keywordFilter"');
    }
    final filterText = filterParts.isEmpty ? 'none' : filterParts.join(', ');

    return Container(
      padding: ResponsiveLayout.getScreenPadding(context),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '${_filteredLogs.length}/${_logManager.logCount} visible · filters: $filterText',
              style: TextStyle(
                color: Theme.of(context).brightness == Brightness.dark
                    ? Colors.grey.shade400
                    : Colors.grey.shade700,
              ),
            ),
          ),
          IconButton(
            onPressed: _copyCurrentLogs,
            icon: const Icon(Icons.copy),
            tooltip: 'Copy visible logs',
          ),
          IconButton(
            onPressed: _clearLogs,
            icon: const Icon(Icons.clear_all),
            tooltip: 'Clear logs',
          ),
          IconButton(
            onPressed: _toggleFollowTail,
            icon: Icon(_followTail ? Icons.link : Icons.link_off),
            tooltip: _followTail
                ? 'Following newest logs'
                : 'Follow newest logs',
          ),
          IconButton(
            onPressed: _scrollToBottom,
            icon: const Icon(Icons.arrow_downward),
            tooltip: 'Scroll to bottom',
          ),
        ],
      ),
    );
  }

  Widget _buildContent(BuildContext context) {
    return Column(
      children: [
        _buildToolbar(context),
        _buildLogList(context),
        _buildBottomBar(context),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    if (!widget.showScaffold) {
      return _buildContent(context);
    }
    return Scaffold(
      appBar: AppBar(title: const Text('Runtime Logs')),
      body: _buildContent(context),
    );
  }
}
