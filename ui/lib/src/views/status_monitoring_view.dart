import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/widgets/connection_view.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';

// Custom notification for service connection
class ServiceConnectionNotification extends Notification {
  final String serviceKey;

  ServiceConnectionNotification(this.serviceKey);
}

// Global service key manager
class ServiceKeyManager {
  static String? _selectedServiceKey;

  static String? getSelectedServiceKey() => _selectedServiceKey;
  static void setSelectedServiceKey(String? key) => _selectedServiceKey = key;
  static void clearSelectedServiceKey() => _selectedServiceKey = null;
}

// Global navigation manager
class AppNavigationManager {
  static void Function(int)? _navigateToPage;

  static void setNavigationFunction(void Function(int) navigationFunction) {
    _navigateToPage = navigationFunction;
  }

  static void navigateToConnectPage() {
    if (_navigateToPage != null) {
      _navigateToPage!(2); // 2 = connect page
    }
  }

  static void navigateToRegisterPage() {
    if (_navigateToPage != null) {
      _navigateToPage!(1); // 1 = register page
    }
  }

  static void navigateToConfigPage() {
    if (_navigateToPage != null) {
      _navigateToPage!(4); // 4 = config page
    }
  }
}

class StatusMonitoringView extends StatefulWidget {
  const StatusMonitoringView({super.key, this.api});

  /// Defaults to the real FFI-backed client; tests pass a fake.
  final PbMapperApiClient? api;

  @override
  State<StatusMonitoringView> createState() => _StatusMonitoringViewState();
}

class _StatusMonitoringViewState extends State<StatusMonitoringView> {
  late final PbMapperApiClient _api = widget.api ?? PbMapperApi();
  ServerStatusDetail? _status;

  @override
  void initState() {
    super.initState();
    _loadStatus();
  }

  Future<void> _loadStatus() async {
    try {
      final status = await _api.forceRefreshServerStatus();
      if (!mounted) return;
      setState(() {
        _status = status;
      });
    } catch (_) {
      if (!mounted) return;
      setState(() {
        _status = const ServerStatusDetail(
          serverAvailable: false,
          registeredServices: [],
          serverMap: '',
          activeConnections: '',
          idleConnections: '',
        );
      });
    }
  }

  // Tapping a service to connect moved with the list, to
  // RegisteredServicesView.

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: ResponsiveLayout.getScreenPadding(context),
      child: SingleChildScrollView(
        child: ResponsiveLayout.isMobile(context)
            ? _buildMobileLayout(context)
            : _buildDesktopLayout(context),
      ),
    );
  }

  // The registered-services list left this page for its own destination. It
  // was the longer of the two things here and the one you scroll, and putting
  // it beside the status made one screen answer two questions.
  Widget _buildMobileLayout(BuildContext context) =>
      _buildServerStatusCard(context);

  Widget _buildDesktopLayout(BuildContext context) =>
      _buildServerStatusCard(context);

  Widget _buildServerStatusCard(BuildContext context) {
    return Card(
      child: Padding(
        padding: EdgeInsets.all(ResponsiveLayout.getCardPadding(context)),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Text(
                  context.l10n.serverStatusTitle,
                  style: Theme.of(context).textTheme.headlineSmall?.copyWith(
                    fontSize: ResponsiveLayout.getFontSize(context, 22),
                    fontWeight: FontWeight.bold,
                  ),
                ),
                const Spacer(),
                ElevatedButton.icon(
                  onPressed: _loadStatus,
                  icon: const Icon(Icons.refresh, size: 18),
                  label: Text(context.l10n.refresh),
                  style: ElevatedButton.styleFrom(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 16,
                      vertical: 12,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 20),
            _status != null
                ? Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _buildStatusIndicator(context, _status!.serverAvailable),
                      const SizedBox(height: 16),
                      _buildServerDetails(context, _status!),
                    ],
                  )
                : const Center(
                    child: Padding(
                      padding: EdgeInsets.all(20),
                      child: CircularProgressIndicator(),
                    ),
                  ),
          ],
        ),
      ),
    );
  }

  Widget _buildStatusIndicator(BuildContext context, bool isAvailable) {
    final isDark = Theme.of(context).brightness == Brightness.dark;
    final availableColor = isDark
        ? Colors.green.shade400
        : Colors.green.shade600;
    final unavailableColor = isDark ? Colors.red.shade400 : Colors.red.shade600;

    return Row(
      children: [
        Container(
          width: 16,
          height: 16,
          decoration: BoxDecoration(
            color: isAvailable ? availableColor : unavailableColor,
            shape: BoxShape.circle,
          ),
        ),
        const SizedBox(width: 12),
        Text(
          isAvailable
              ? context.l10n.statusAvailable
              : context.l10n.statusUnavailable,
          style: TextStyle(
            color: isAvailable ? availableColor : unavailableColor,
            fontWeight: FontWeight.bold,
            fontSize: 18,
          ),
        ),
      ],
    );
  }

  Widget _buildServerDetails(BuildContext context, ServerStatusDetail status) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _buildDetailRow(
          context.l10n.registeredServicesTitle,
          status.registeredServices.length.toString(),
        ),
        const SizedBox(height: 16),
        // The pool of connection ids, as chips you can copy rather than the
        // `count:… max:… list:[…]` line the server formats. What each service
        // is actually holding lives on the Services page, which asks the
        // protocol's structured query instead of reading a Debug dump.
        if (status.activeConnections.isNotEmpty) ...[
          ConnectionIdChips(
            label: context.l10n.connectionsActive,
            raw: status.activeConnections,
          ),
          const SizedBox(height: 16),
        ],
        if (status.idleConnections.isNotEmpty)
          ConnectionIdChips(
            label: context.l10n.connectionsIdle,
            raw: status.idleConnections,
          ),
      ],
    );
  }

  Widget _buildDetailRow(String label, String value) {
    return Container(
      padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 12),
      decoration: BoxDecoration(
        color: Theme.of(context).brightness == Brightness.dark
            ? Colors.grey.shade800.withValues(alpha: 0.5)
            : Colors.grey.shade100,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.spaceBetween,
        children: [
          Text(
            label,
            style: TextStyle(
              fontSize: 16,
              color: Theme.of(context).textTheme.bodyLarge?.color,
            ),
          ),
          Text(
            value,
            style: TextStyle(
              fontWeight: FontWeight.bold,
              fontSize: 16,
              color: Theme.of(context).brightness == Brightness.dark
                  ? Colors.blue.shade300
                  : Colors.blue.shade700,
            ),
          ),
        ],
      ),
    );
  }
}
