import 'dart:async';

import 'package:flutter/material.dart';

import 'src/rust/api/yonder.dart';
import 'src/rust/frb_generated.dart';
import 'ui/devices.dart';
import 'ui/footprint.dart';
import 'ui/format.dart';
import 'ui/jobs.dart';
import 'ui/notify.dart';
import 'ui/pair.dart';
import 'ui/prepare.dart';
import 'ui/terminal.dart';

Future<void> main() async {
  await RustLib.init();
  runApp(const YonderAppView());
}

class YonderAppView extends StatelessWidget {
  const YonderAppView({super.key});

  @override
  Widget build(BuildContext context) {
    ThemeData theme(Brightness brightness) => ThemeData(
          colorSchemeSeed: const Color(0xFF3D6B5E),
          brightness: brightness,
          visualDensity: VisualDensity.compact,
        );
    return MaterialApp(
      title: 'Yonder',
      theme: theme(Brightness.light),
      darkTheme: theme(Brightness.dark),
      debugShowCheckedModeBanner: false,
      home: const Home(),
    );
  }
}

class Home extends StatefulWidget {
  const Home({super.key});

  @override
  State<Home> createState() => _HomeState();
}

class _HomeState extends State<Home> {
  YonderApp? app;
  String? openError;
  List<Machine> machines = [];
  final Map<String, MachineStatus?> status = {};
  final Map<String, String> statusError = {};
  final Map<String, DateTime> lastSeen = {};
  String? selected;
  Timer? refresh;
  final notifier = JobNotifier();

  @override
  void initState() {
    super.initState();
    _open();
  }

  Future<void> _open() async {
    try {
      await notifier.init();
      final opened = await YonderApp.open();
      setState(() => app = opened);
      _reload();
      refresh = Timer.periodic(const Duration(seconds: 15), (_) => _reload());
    } catch (e) {
      setState(() => openError = errorText(e));
    }
  }

  @override
  void dispose() {
    refresh?.cancel();
    super.dispose();
  }

  void _reload() {
    final app = this.app;
    if (app == null) return;
    setState(() {
      machines = app.machines();
      if (selected == null || !machines.any((m) => m.id == selected)) {
        selected = machines.isEmpty ? null : machines.first.id;
      }
    });
    for (final m in machines) {
      app.jobs(machineId: m.id).then((jobs) => notifier.check(m, jobs)).catchError((_) {});
      app.status(machineId: m.id).then((s) {
        if (!mounted) return;
        setState(() {
          status[m.id] = s;
          statusError.remove(m.id);
          lastSeen[m.id] = DateTime.now();
        });
      }).catchError((Object e) {
        if (!mounted) return;
        setState(() {
          status[m.id] = null;
          statusError[m.id] = errorText(e);
        });
      });
    }
  }

  Future<void> _pair() async {
    final app = this.app;
    if (app == null) return;
    final paired = await showDialog<Machine>(
      context: context,
      builder: (_) => PairDialog(app: app),
    );
    if (paired != null) {
      selected = paired.id;
      _reload();
    }
  }

  @override
  Widget build(BuildContext context) {
    final app = this.app;
    if (openError != null) {
      return Scaffold(body: Center(child: Text("Couldn't open Yonder: $openError")));
    }
    if (app == null) {
      return const Scaffold(body: Center(child: CircularProgressIndicator()));
    }
    final scheme = Theme.of(context).colorScheme;
    final machine = machines.where((m) => m.id == selected).firstOrNull;
    return Scaffold(
      body: Row(
        children: [
          Container(
            width: 240,
            color: scheme.surfaceContainerLow,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 28, 16, 4),
                  child: Text('Machines', style: Theme.of(context).textTheme.titleSmall),
                ),
                Expanded(
                  child: ListView(
                    children: [
                      for (final m in machines)
                        _MachineTile(
                          machine: m,
                          status: status[m.id],
                          error: statusError[m.id],
                          lastSeen: lastSeen[m.id],
                          selected: m.id == selected,
                          onTap: () => setState(() => selected = m.id),
                        ),
                    ],
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.all(12),
                  child: OutlinedButton.icon(
                    onPressed: _pair,
                    icon: const Icon(Icons.add, size: 18),
                    label: const Text('Pair a machine'),
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
                  child: Text(
                    '${app.deviceName()} · ${app.fingerprint()}',
                    style: Theme.of(context).textTheme.bodySmall,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ],
            ),
          ),
          Expanded(
            child: machine == null
                ? const Center(child: Text('Pair a machine to get started.'))
                : MachineView(
                    key: ValueKey(machine.id),
                    app: app,
                    machine: machine,
                    status: status[machine.id],
                    error: statusError[machine.id],
                    onChanged: _reload,
                  ),
          ),
        ],
      ),
    );
  }
}

class _MachineTile extends StatelessWidget {
  const _MachineTile({
    required this.machine,
    required this.status,
    required this.error,
    required this.lastSeen,
    required this.selected,
    required this.onTap,
  });

  final Machine machine;
  final MachineStatus? status;
  final String? error;
  final DateTime? lastSeen;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final status = this.status;
    final online = status != null;
    final busy = online && (status.runningJobs > 0 || status.queuedJobs > 0);
    final color = error != null
        ? Colors.grey
        : !online
            ? Colors.transparent
            : status.paused
                ? Colors.orange
                : busy
                    ? Colors.blue
                    : Colors.green;
    final seen = lastSeen;
    final subtitle = error != null
        ? seen != null
            ? 'offline · seen ${when(BigInt.from(seen.millisecondsSinceEpoch))}'
            : 'offline'
        : !online
            ? 'checking…'
            : status.paused
                ? 'paused'
                : busy
                    ? '${status.runningJobs} running, ${status.queuedJobs} queued'
                    : status.pendingRestart == true
                        ? 'online · restart pending'
                        : 'online, idle';
    return ListTile(
      selected: selected,
      dense: true,
      leading: Icon(
        machine.os == 'win'
            ? Icons.window
            : machine.os == 'mac'
                ? Icons.laptop_mac
                : Icons.computer,
      ),
      title: Text(machine.name),
      subtitle: Row(
        children: [
          Container(
            width: 8,
            height: 8,
            decoration: BoxDecoration(color: color, shape: BoxShape.circle),
          ),
          const SizedBox(width: 6),
          Flexible(child: Text(subtitle, overflow: TextOverflow.ellipsis)),
        ],
      ),
      onTap: onTap,
    );
  }
}

/// One machine: jobs, terminal, footprint and devices.
class MachineView extends StatelessWidget {
  const MachineView({
    super.key,
    required this.app,
    required this.machine,
    required this.status,
    required this.error,
    required this.onChanged,
  });

  final YonderApp app;
  final Machine machine;
  final MachineStatus? status;
  final String? error;
  final VoidCallback onChanged;

  @override
  Widget build(BuildContext context) {
    final status = this.status;
    final text = Theme.of(context).textTheme;
    return DefaultTabController(
      length: 4,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(24, 24, 24, 0),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Expanded(child: Text(machine.name, style: text.headlineSmall)),
                    Builder(
                      builder: (context) => OutlinedButton.icon(
                        onPressed: () async {
                          final job = await showDialog<Job>(
                            context: context,
                            builder: (_) => PrepareDialog(app: app, machine: machine),
                          );
                          if (job == null || !context.mounted) return;
                          // Follow it in the Jobs tab.
                          DefaultTabController.of(context).animateTo(0);
                          ScaffoldMessenger.of(context).showSnackBar(
                            SnackBar(content: Text('Preparing ${machine.name}: see the Jobs tab.')),
                          );
                        },
                        icon: const Icon(Icons.build_outlined, size: 18),
                        label: const Text('Prepare…'),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 4),
                Text(
                  [
                    machine.os,
                    if (status != null) 'agent ${status.agentVersion}',
                    if (error != null) "can't reach it: $error",
                    'paired ${date(machine.pairedAt)}',
                  ].join(' · '),
                  style: text.bodySmall,
                ),
                if (status != null && status.cpuPercent != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 6),
                    child: Wrap(
                      spacing: 16,
                      children: [
                        _Stat('CPU', '${status.cpuPercent}%'),
                        if (status.memoryTotal != null)
                          _Stat('Memory', '${bytes(status.memoryUsed!)} of ${bytes(status.memoryTotal!)}'),
                        if (status.diskTotal != null)
                          _Stat('Disk', '${bytes(status.diskFree!)} free of ${bytes(status.diskTotal!)}'),
                        if (status.uptimeSecs != null) _Stat('Up', uptime(status.uptimeSecs!)),
                        if (status.pendingRestart == true)
                          const _Stat('Updates', 'waiting for a restart'),
                      ],
                    ),
                  ),
              ],
            ),
          ),
          const TabBar(
            isScrollable: true,
            tabAlignment: TabAlignment.start,
            tabs: [
              Tab(text: 'Jobs'),
              Tab(text: 'Terminal'),
              Tab(text: 'Footprint'),
              Tab(text: 'Devices'),
            ],
          ),
          Expanded(
            child: TabBarView(
              physics: const NeverScrollableScrollPhysics(),
              children: [
                JobsTab(app: app, machine: machine),
                TerminalTab(app: app, machine: machine),
                FootprintTab(app: app, machine: machine, onUnpaired: onChanged),
                DevicesTab(app: app, machine: machine),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Stat extends StatelessWidget {
  const _Stat(this.label, this.value);

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Text.rich(
      TextSpan(children: [
        TextSpan(text: '$label ', style: text.bodySmall),
        TextSpan(text: value, style: text.bodyMedium),
      ]),
    );
  }
}
