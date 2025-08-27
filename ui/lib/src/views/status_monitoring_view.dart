import 'package:flutter/material.dart';
import 'package:ui/src/bindings/bindings.dart';
import 'package:ui/src/views/log_display_widget.dart';

class StatusMonitoringView extends StatefulWidget {
  const StatusMonitoringView({super.key});

  @override
  State<StatusMonitoringView> createState() => _StatusMonitoringViewState();
}

class _StatusMonitoringViewState extends State<StatusMonitoringView> {
  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16.0),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Server Status',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ServerStatusUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final status = snapshot.data!.message;
                          return Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text('Status: ${status.status}'),
                              Text(
                                'Active Connections: ${status.activeConnections}',
                              ),
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
            ),
            const SizedBox(height: 16),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Registered Services',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: RegisteredServicesUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final services = snapshot.data!.message.services;
                          if (services.isEmpty) {
                            return const Text('No services registered');
                          }
                          return ListView.builder(
                            shrinkWrap: true,
                            physics: const NeverScrollableScrollPhysics(),
                            itemCount: services.length,
                            itemBuilder: (context, index) {
                              final service = services[index];
                              return ListTile(
                                title: Text(service.serviceKey),
                                subtitle: Text(
                                  '${service.protocol} - ${service.localAddress}',
                                ),
                                trailing: Text(service.status),
                              );
                            },
                          );
                        }
                        return const CircularProgressIndicator();
                      },
                    ),
                  ],
                ),
              ),
            ),
            const SizedBox(height: 16),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16.0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Active Connections',
                      style: Theme.of(context).textTheme.titleLarge,
                    ),
                    const SizedBox(height: 16),
                    StreamBuilder(
                      stream: ActiveConnectionsUpdate.rustSignalStream,
                      builder: (context, snapshot) {
                        if (snapshot.hasData) {
                          final connections =
                              snapshot.data!.message.connections;
                          if (connections.isEmpty) {
                            return const Text('No active connections');
                          }
                          return ListView.builder(
                            shrinkWrap: true,
                            physics: const NeverScrollableScrollPhysics(),
                            itemCount: connections.length,
                            itemBuilder: (context, index) {
                              final connection = connections[index];
                              return ListTile(
                                title: Text(connection.serviceKey),
                                subtitle: Text(
                                  'Client: ${connection.clientId}',
                                ),
                                trailing: Text(connection.status),
                              );
                            },
                          );
                        }
                        return const CircularProgressIndicator();
                      },
                    ),
                  ],
                ),
              ),
            ),
            const SizedBox(height: 16),
            const LogDisplayWidget(),
          ],
        ),
      ),
    );
  }
}
