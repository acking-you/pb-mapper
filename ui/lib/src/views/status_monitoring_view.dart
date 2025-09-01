import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/bindings/bindings.dart';
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
      _navigateToPage!(3); // 3 = connect page
    }
  }

  static void navigateToConfigPage() {
    if (_navigateToPage != null) {
      _navigateToPage!(5); // 5 = config page
    }
  }
}

class StatusMonitoringView extends StatefulWidget {
  const StatusMonitoringView({super.key});

  @override
  State<StatusMonitoringView> createState() => _StatusMonitoringViewState();
}

class _StatusMonitoringViewState extends State<StatusMonitoringView> {
  @override
  void initState() {
    super.initState();
    // Request detailed status when view loads
    RequestServerStatus().sendSignalToRust();
  }

  void _navigateToConnection(BuildContext context, String serviceKey) {
    // Store service key for later use
    ServiceKeyManager.setSelectedServiceKey(serviceKey);

    // Navigate to Connect page
    AppNavigationManager.navigateToConnectPage();

    // Show a snackbar to inform user about the action
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text('Navigated to Connect with service key "$serviceKey"'),
        duration: const Duration(seconds: 2),
        backgroundColor: Colors.green,
      ),
    );
  }

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

  Widget _buildMobileLayout(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _buildServerStatusCard(context),
        SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
        _buildServicesCard(context),
      ],
    );
  }

  Widget _buildDesktopLayout(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(child: _buildServerStatusCard(context)),
            SizedBox(width: ResponsiveLayout.getHorizontalPadding(context)),
            Expanded(child: _buildServicesCard(context)),
          ],
        ),
      ],
    );
  }

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
                  'Server Status',
                  style: Theme.of(context).textTheme.headlineSmall?.copyWith(
                    fontSize: ResponsiveLayout.getFontSize(context, 22),
                    fontWeight: FontWeight.bold,
                  ),
                ),
                const Spacer(),
                ElevatedButton.icon(
                  onPressed: () => RequestServerStatus().sendSignalToRust(),
                  icon: const Icon(Icons.refresh, size: 18),
                  label: const Text('Refresh'),
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
            StreamBuilder(
              stream: ServerStatusDetailUpdate.rustSignalStream,
              builder: (context, snapshot) {
                if (snapshot.hasData) {
                  final status = snapshot.data!.message;
                  return Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _buildStatusIndicator(context, status.serverAvailable),
                      const SizedBox(height: 16),
                      _buildServerDetails(context, status),
                    ],
                  );
                }
                return const Center(
                  child: Padding(
                    padding: EdgeInsets.all(20),
                    child: CircularProgressIndicator(),
                  ),
                );
              },
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
          isAvailable ? 'Available' : 'Unavailable',
          style: TextStyle(
            color: isAvailable ? availableColor : unavailableColor,
            fontWeight: FontWeight.bold,
            fontSize: 18,
          ),
        ),
      ],
    );
  }

  Widget _buildServerDetails(BuildContext context, dynamic status) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _buildDetailRow(
          'Registered Services',
          status.registeredServices.length.toString(),
        ),
        const SizedBox(height: 16),
        if (status.serverMap.isNotEmpty)
          ExpansionTile(
            title: Text(
              'Server Map Details',
              style: TextStyle(fontSize: 16, fontWeight: FontWeight.w600),
            ),
            children: [
              Container(
                width: double.infinity,
                padding: const EdgeInsets.all(16.0),
                margin: const EdgeInsets.symmetric(horizontal: 16.0),
                decoration: BoxDecoration(
                  color: Theme.of(context).brightness == Brightness.dark
                      ? Colors.grey.shade800
                      : Colors.grey.shade100,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Text(
                  status.serverMap,
                  style: TextStyle(
                    fontFamily: 'Courier',
                    fontSize: 14,
                    color: Theme.of(context).brightness == Brightness.dark
                        ? Colors.green.shade300
                        : Colors.green.shade700,
                  ),
                ),
              ),
              const SizedBox(height: 8),
            ],
          ),
        if (status.activeConnections.isNotEmpty ||
            status.idleConnections.isNotEmpty)
          ExpansionTile(
            title: Text(
              'Connection Details',
              style: TextStyle(fontSize: 16, fontWeight: FontWeight.w600),
            ),
            children: [
              Container(
                width: double.infinity,
                padding: const EdgeInsets.all(16.0),
                margin: const EdgeInsets.symmetric(horizontal: 16.0),
                decoration: BoxDecoration(
                  color: Theme.of(context).brightness == Brightness.dark
                      ? Colors.grey.shade800
                      : Colors.grey.shade100,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (status.activeConnections.isNotEmpty) ...[
                      Text(
                        'Active:',
                        style: TextStyle(
                          fontWeight: FontWeight.bold,
                          fontSize: 14,
                        ),
                      ),
                      const SizedBox(height: 4),
                      Text(
                        status.activeConnections,
                        style: TextStyle(
                          fontFamily: 'Courier',
                          fontSize: 12,
                          color: Theme.of(context).brightness == Brightness.dark
                              ? Colors.blue.shade300
                              : Colors.blue.shade700,
                        ),
                      ),
                      const SizedBox(height: 12),
                    ],
                    if (status.idleConnections.isNotEmpty) ...[
                      Text(
                        'Idle:',
                        style: TextStyle(
                          fontWeight: FontWeight.bold,
                          fontSize: 14,
                        ),
                      ),
                      const SizedBox(height: 4),
                      Text(
                        status.idleConnections,
                        style: TextStyle(
                          fontFamily: 'Courier',
                          fontSize: 12,
                          color: Theme.of(context).brightness == Brightness.dark
                              ? Colors.orange.shade300
                              : Colors.orange.shade700,
                        ),
                      ),
                    ],
                  ],
                ),
              ),
              const SizedBox(height: 8),
            ],
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

  Widget _buildServicesCard(BuildContext context) {
    return Card(
      child: Padding(
        padding: EdgeInsets.all(ResponsiveLayout.getCardPadding(context)),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'Registered Services',
              style: Theme.of(context).textTheme.headlineSmall?.copyWith(
                fontSize: ResponsiveLayout.getFontSize(context, 20),
                fontWeight: FontWeight.bold,
              ),
            ),
            const SizedBox(height: 16),
            StreamBuilder(
              stream: ServerStatusDetailUpdate.rustSignalStream,
              builder: (context, snapshot) {
                if (snapshot.hasData && snapshot.data != null) {
                  final services = snapshot.data!.message.registeredServices;
                  if (services.isEmpty) {
                    return Container(
                      padding: const EdgeInsets.all(24),
                      child: Center(
                        child: Column(
                          children: [
                            Icon(
                              Icons.info_outline,
                              size: 48,
                              color: Colors.grey.shade400,
                            ),
                            const SizedBox(height: 12),
                            Text(
                              'No services registered',
                              style: TextStyle(
                                fontSize: 16,
                                color: Colors.grey.shade600,
                              ),
                            ),
                          ],
                        ),
                      ),
                    );
                  }
                  return Column(
                    children: services.map<Widget>((serviceKey) {
                      final isDark =
                          Theme.of(context).brightness == Brightness.dark;
                      final availableColor = isDark
                          ? Colors.green.shade400
                          : Colors.green.shade600;

                      return Card(
                        margin: const EdgeInsets.symmetric(vertical: 6),
                        color: Theme.of(context).brightness == Brightness.dark
                            ? Colors.grey.shade800.withValues(alpha: 0.7)
                            : Colors.green.shade50,
                        child: ListTile(
                          leading: Container(
                            width: 12,
                            height: 12,
                            decoration: BoxDecoration(
                              color: availableColor,
                              shape: BoxShape.circle,
                            ),
                          ),
                          title: Text(
                            serviceKey,
                            style: TextStyle(
                              fontSize: 16,
                              fontWeight: FontWeight.w600,
                            ),
                          ),
                          subtitle: Text(
                            'Tap to connect to this service',
                            style: TextStyle(
                              fontSize: 14,
                              color: availableColor,
                            ),
                          ),
                          trailing: Icon(
                            Icons.arrow_forward_ios,
                            size: 16,
                            color: availableColor,
                          ),
                          contentPadding: const EdgeInsets.symmetric(
                            horizontal: 16,
                            vertical: 8,
                          ),
                          onTap: () =>
                              _navigateToConnection(context, serviceKey),
                        ),
                      );
                    }).toList(),
                  );
                }
                return const Center(
                  child: Padding(
                    padding: EdgeInsets.all(20),
                    child: CircularProgressIndicator(),
                  ),
                );
              },
            ),
          ],
        ),
      ),
    );
  }
}
