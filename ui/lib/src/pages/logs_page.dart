import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/log_manager.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

/// The Rust side's log, newest at the bottom, filtered by a minimum level and
/// a word. Follows new lines until the reader scrolls up, and again once they
/// scroll back to the end.
class LogsPage extends StatefulWidget {
  const LogsPage({super.key, this.logs});

  /// Defaults to the app's one log buffer; tests pass their own.
  final LogManager? logs;

  @override
  State<LogsPage> createState() => _LogsPageState();
}

class _LogsPageState extends State<LogsPage> {
  late final LogManager _logs = widget.logs ?? LogManager();
  final ScrollController _scroll = ScrollController();
  final TextEditingController _search = TextEditingController();
  StreamSubscription<List<LogMessage>>? _subscription;
  Timer? _debounce;

  /// Minimum level, or null for every line.
  String? _level;
  String _keyword = '';
  bool _follow = true;
  List<LogMessage> _shown = const [];

  @override
  void initState() {
    super.initState();
    _logs.initialize();
    _shown = _logs.filterLogs();
    _subscription = _logs.logStream.listen((_) => _apply(scroll: true));
    WidgetsBinding.instance.addPostFrameCallback((_) => _toEnd(jump: true));
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _subscription?.cancel();
    _scroll.dispose();
    _search.dispose();
    super.dispose();
  }

  void _apply({bool scroll = false}) {
    if (!mounted) return;
    setState(() {
      _shown = _logs.filterLogs(levelFilter: _level, keyword: _keyword);
    });
    if (scroll && _follow) _toEnd(jump: true);
  }

  void _onSearch(String value) {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 150), () {
      _keyword = value.trim();
      _apply();
    });
  }

  void _toEnd({bool jump = false}) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_scroll.hasClients) return;
      final target = _scroll.position.maxScrollExtent;
      if (jump) {
        _scroll.jumpTo(target);
      } else {
        _scroll.animateTo(
          target,
          duration: const Duration(milliseconds: 180),
          curve: Curves.easeOut,
        );
      }
    });
  }

  bool _onScroll(ScrollNotification notification) {
    if (!_scroll.hasClients) return false;
    final fromEnd = _scroll.position.maxScrollExtent - _scroll.position.pixels;
    if (fromEnd > 40 && _follow) {
      setState(() => _follow = false);
    } else if (fromEnd <= 4 && !_follow) {
      setState(() => _follow = true);
    }
    return false;
  }

  Future<void> _copy() async {
    await Clipboard.setData(
      ClipboardData(
        text: _logs.getAllLogsAsText(levelFilter: _level, keyword: _keyword),
      ),
    );
    if (!mounted) return;
    showToast(
      context,
      context.l10n.logsCopied(_shown.length),
      kind: ToastKind.success,
    );
  }

  Color _levelColor(String level, ColorScheme scheme) =>
      switch (LogManager.normalizeLevel(level)) {
        'ERROR' => scheme.error,
        'WARN' => cautionColor(scheme),
        'INFO' => signalColor(scheme),
        'DEBUG' => successColor(scheme),
        _ => onSurfaceMuted(scheme),
      };

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final narrow = MediaQuery.sizeOf(context).width < 640;
    final levels = GlassSegmented<String?>(
      key: const Key('log-level'),
      expand: false,
      height: 34,
      selected: _level,
      onChanged: (level) {
        _level = level;
        _apply();
      },
      segments: [
        GlassSegment(value: null, label: l10n.logLevelAll),
        const GlassSegment(value: 'INFO', label: 'INFO+'),
        const GlassSegment(value: 'WARN', label: 'WARN+'),
        const GlassSegment(value: 'ERROR', label: 'ERROR'),
      ],
    );
    final search = TextField(
      key: const Key('log-search'),
      controller: _search,
      onChanged: _onSearch,
      decoration: InputDecoration(
        hintText: l10n.logsFilter,
        prefixIcon: const Icon(AppIcons.search, size: 17),
        suffixIcon: _keyword.isEmpty
            ? null
            : IconButton(
                icon: const Icon(AppIcons.close, size: 15),
                onPressed: () {
                  _search.clear();
                  _keyword = '';
                  _apply();
                },
              ),
      ),
    );
    return PageFrame(
      title: l10n.navLogs,
      actions: [
        IconButton(
          key: const Key('logs-copy'),
          tooltip: l10n.copy,
          onPressed: _shown.isEmpty ? null : _copy,
          icon: const Icon(AppIcons.copy),
        ),
        IconButton(
          key: const Key('logs-clear'),
          tooltip: l10n.clearLogs,
          onPressed: () {
            _logs.clearLogs();
            _apply();
          },
          icon: const Icon(AppIcons.clear),
        ),
      ],
      body: Padding(
        padding: const EdgeInsets.fromLTRB(18, 0, 18, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (narrow) ...[
              search,
              const SizedBox(height: 10),
              SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: levels,
              ),
            ] else
              Row(
                children: [
                  Expanded(child: search),
                  const SizedBox(width: 12),
                  levels,
                ],
              ),
            const SizedBox(height: 12),
            Expanded(
              child: Stack(
                children: [
                  Positioned.fill(
                    child: Container(
                      decoration: BoxDecoration(
                        color: codeSurface(scheme),
                        borderRadius: BorderRadius.circular(kPanelRadius),
                        border: Border.all(color: scheme.outlineVariant),
                      ),
                      clipBehavior: Clip.antiAlias,
                      child: _shown.isEmpty
                          ? Center(
                              child: Text(
                                _logs.logCount == 0
                                    ? l10n.logsEmpty
                                    : l10n.logsNoMatch,
                                style: theme.textTheme.bodySmall?.copyWith(
                                  color: scheme.onSurfaceVariant,
                                ),
                              ),
                            )
                          : NotificationListener<ScrollNotification>(
                              onNotification: _onScroll,
                              child: Scrollbar(
                                controller: _scroll,
                                child: SelectionArea(
                                  child: ListView.builder(
                                    key: const Key('log-lines'),
                                    controller: _scroll,
                                    padding: const EdgeInsets.fromLTRB(
                                      14,
                                      12,
                                      14,
                                      12,
                                    ),
                                    itemCount: _shown.length,
                                    itemBuilder: (context, i) =>
                                        _line(_shown[i], scheme),
                                  ),
                                ),
                              ),
                            ),
                    ),
                  ),
                  if (!_follow)
                    Positioned(
                      right: 14,
                      bottom: 14,
                      child: Glass(
                        depth: GlassDepth.floating,
                        borderRadius: BorderRadius.circular(999),
                        child: TextButton.icon(
                          key: const Key('logs-follow'),
                          onPressed: () {
                            setState(() => _follow = true);
                            _toEnd();
                          },
                          icon: const Icon(AppIcons.follow, size: 16),
                          label: Text(l10n.followLogs),
                        ),
                      ),
                    ),
                ],
              ),
            ),
            const SizedBox(height: 8),
            Text(
              l10n.logsShown(_shown.length, _logs.logCount),
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _line(LogMessage log, ColorScheme scheme) {
    final time = DateTime.fromMillisecondsSinceEpoch(log.timestamp * 1000);
    String two(int v) => v.toString().padLeft(2, '0');
    final stamp = '${two(time.hour)}:${two(time.minute)}:${two(time.second)}';
    return Padding(
      padding: const EdgeInsets.only(bottom: 3),
      child: Text.rich(
        TextSpan(
          children: [
            TextSpan(
              text: '$stamp  ',
              style: mono(fontSize: 12, color: onSurfaceMuted(scheme)),
            ),
            TextSpan(
              text: '${log.level.padRight(5)}  ',
              style: mono(
                fontSize: 12,
                fontWeight: FontWeight.w600,
                color: _levelColor(log.level, scheme),
              ),
            ),
            TextSpan(
              text: log.message,
              style: mono(fontSize: 12, color: scheme.onSurface, height: 1.45),
            ),
          ],
        ),
      ),
    );
  }
}
