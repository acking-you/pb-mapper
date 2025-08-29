import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/views/log_view_page.dart';
import 'package:ui/src/common/responsive_layout.dart';

class StatusMonitoringView extends StatefulWidget {
  const StatusMonitoringView({super.key});

  @override
  State<StatusMonitoringView> createState() => _StatusMonitoringViewState();
}

class _StatusMonitoringViewState extends State<StatusMonitoringView> {
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
        SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
        _buildConnectionsCard(context),
        SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
        _buildLogsCard(context),
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
        SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
        _buildConnectionsCard(context),
        SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
        _buildLogsCard(context),
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
            Text(
              'Server Status',
              style: Theme.of(context).textTheme.titleLarge?.copyWith(
                fontSize: ResponsiveLayout.getFontSize(context, 18),
              ),
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
            StreamBuilder(
              stream: ServerStatusUpdate.rustSignalStream,
              builder: (context, snapshot) {
                if (snapshot.hasData) {
                  final status = snapshot.data!.message;
                  return Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('Status: ${status.status}'),
                      Text('Active Connections: ${status.activeConnections}'),
                      Text('Uptime: ${status.uptime}'),
                    ],
                  );
                }
                return const Text('Server status not available');
              },
            ),
          ],
        ),
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
              style: Theme.of(context).textTheme.titleLarge?.copyWith(
                fontSize: ResponsiveLayout.getFontSize(context, 18),
              ),
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
            StreamBuilder(
              stream: RegisteredServicesUpdate.rustSignalStream,
              builder: (context, snapshot) {
                if (snapshot.hasData && snapshot.data != null) {
                  final services = snapshot.data!.message.services;
                  if (services.isEmpty) {
                    return const Text('No services registered');
                  }
                  return Column(
                    children: services.map((service) {
                      return ListTile(
                        title: Text(service.serviceKey),
                        subtitle: Text(
                          '${service.protocol} - ${service.localAddress}',
                        ),
                        trailing: Text(service.status),
                        dense: ResponsiveLayout.isMobile(context),
                      );
                    }).toList(),
                  );
                }
                return const CircularProgressIndicator();
              },
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildConnectionsCard(BuildContext context) {
    return Card(
      child: Padding(
        padding: EdgeInsets.all(ResponsiveLayout.getCardPadding(context)),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'Active Connections',
              style: Theme.of(context).textTheme.titleLarge?.copyWith(
                fontSize: ResponsiveLayout.getFontSize(context, 18),
              ),
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context)),
            StreamBuilder(
              stream: ActiveConnectionsUpdate.rustSignalStream,
              builder: (context, snapshot) {
                if (snapshot.hasData && snapshot.data != null) {
                  final connections = snapshot.data!.message.connections;
                  if (connections.isEmpty) {
                    return const Text('No active connections');
                  }
                  return Column(
                    children: connections.map((connection) {
                      return ListTile(
                        title: Text(connection.serviceKey),
                        subtitle: Text('Client: ${connection.clientId}'),
                        trailing: Text(connection.status),
                        dense: ResponsiveLayout.isMobile(context),
                      );
                    }).toList(),
                  );
                }
                return const CircularProgressIndicator();
              },
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildLogsCard(BuildContext context) {
    return Card(
      child: Padding(
        padding: EdgeInsets.all(ResponsiveLayout.getCardPadding(context)),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Text(
                  'Logs',
                  style: Theme.of(context).textTheme.titleLarge?.copyWith(
                    fontSize: ResponsiveLayout.getFontSize(context, 18),
                  ),
                ),
                ElevatedButton.icon(
                  onPressed: () {
                    Navigator.of(context).push(
                      MaterialPageRoute(
                        builder: (context) => const LogViewPage(),
                      ),
                    );
                  },
                  icon: const Icon(Icons.open_in_new),
                  label: Text(
                    ResponsiveLayout.isMobile(context) ? 'Logs' : 'View Logs',
                  ),
                ),
              ],
            ),
            SizedBox(height: ResponsiveLayout.getVerticalSpacing(context) / 2),
            Text(
              'Click "${ResponsiveLayout.isMobile(context) ? 'Logs' : 'View Logs'}" to see detailed log output',
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                color: Colors.grey.shade600,
                fontSize: ResponsiveLayout.getFontSize(context, 14),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
