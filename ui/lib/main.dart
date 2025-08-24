import 'dart:ui';
import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';
import 'package:ui/src/views/client_connection_view.dart';
import 'package:ui/src/views/configuration_view.dart';
import 'package:ui/src/views/server_management_view.dart';
import 'package:ui/src/views/service_registration_view.dart';
import 'package:ui/src/views/status_monitoring_view.dart';
import 'src/bindings/bindings.dart';

Future<void> main() async {
  await initializeRust(assignRustSignal);
  createActors();
  runApp(MyApp());
}

void createActors() {
  CreateActors().sendSignalToRust();
}

class MyApp extends StatefulWidget {
  const MyApp({super.key});
  @override
  State<MyApp> createState() => _MyAppState();
}

class _MyAppState extends State<MyApp> {
  /// This `AppLifecycleListener` is responsible for the
  /// graceful shutdown of the async runtime in Rust.
  /// If you don't care about
  /// properly dropping Rust objects before shutdown,
  /// creating this listener is not necessary.
  late final AppLifecycleListener _listener;

  @override
  void initState() {
    super.initState();
    _listener = AppLifecycleListener(
      onExitRequested: () async {
        finalizeRust(); // This line shuts down the async Rust runtime.
        return AppExitResponse.exit;
      },
    );
  }

  @override
  void dispose() {
    _listener.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Rinf Demo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.blueGrey,
          brightness: MediaQuery.platformBrightnessOf(context),
        ),
        useMaterial3: true,
      ),
      home: MyHomePage(),
    );
  }
}

class MainScreen extends StatefulWidget {
  const MainScreen({super.key});

  @override
  State<MainScreen> createState() => _MainScreenState();
}

class _MainScreenState extends State<MainScreen> {
  int _currentIndex = 0;

  final List<Widget> _views = [
    ServerManagementView(),
    ServiceRegistrationView(),
    ClientConnectionView(),
    StatusMonitoringView(),
    ConfigurationView(),
  ];

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('pb-mapper UI')),
      body: _views[_currentIndex],
      bottomNavigationBar: BottomNavigationBar(
        currentIndex: _currentIndex,
        onTap: (index) => setState(() => _currentIndex = index),
        items: const [
          BottomNavigationBarItem(icon: Icon(Icons.settings), label: 'Server'),
          BottomNavigationBarItem(
            icon: Icon(Icons.add_circle),
            label: 'Register',
          ),
          BottomNavigationBarItem(
            icon: Icon(Icons.connect_without_contact),
            label: 'Connect',
          ),
          BottomNavigationBarItem(icon: Icon(Icons.list), label: 'Status'),
          BottomNavigationBarItem(icon: Icon(Icons.build), label: 'Config'),
        ],
      ),
    );
  }
}

class MyColumn extends StatelessWidget {
  const MyColumn({super.key});

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        StreamBuilder(
          stream: SampleNumberOutput.rustSignalStream,
          builder: (context, snapshot) {
            final signalPack = snapshot.data;
            if (signalPack == null) {
              return const Text('Initial value 0');
            }
            final currentNumber = signalPack.message.currentNumber;
            return Text('Current value is $currentNumber');
          },
        ),
      ],
    );
  }
}

class MyHomePage extends StatelessWidget {
  const MyHomePage({super.key});

  @override
  Widget build(BuildContext context) {
    return const MainScreen();
  }
}
