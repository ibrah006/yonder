import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'src/rust/api/yonder.dart';
import 'src/rust/frb_generated.dart';
import 'ui/devices.dart';
import 'ui/footprint.dart';
import 'ui/format.dart';
import 'ui/jobs.dart';
import 'ui/notify.dart';
import 'ui/pair.dart';
import 'ui/prepare.dart';
import 'ui/snapshot.dart';
import 'ui/terminal.dart';
import 'ui/theme.dart';
import 'ui/widgets.dart';

Future<void> main() async {
  await RustLib.init();
  registerSnapshot();
  runApp(RepaintBoundary(key: snapshotKey, child: const YonderAppView()));
}

/// Room for the window buttons: the window has no title bar of its own.
const titleBarHeight = 28.0;

class YonderAppView extends StatelessWidget {
  const YonderAppView({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Yonder',
      theme: buildTheme(Brightness.light),
      darkTheme: buildTheme(Brightness.dark),
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

  /// Status checks failed in a row, per machine.
  final Map<String, int> failures = {};
  String? selected;
  Timer? refresh;
  final notifier = JobNotifier();

  @override
  void initState() {
    super.initState();
    _open();
  }

  Future<void> _open() async {
    setState(() => openError = null);
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
          failures.remove(m.id);
          lastSeen[m.id] = DateTime.now();
        });
      }).catchError((Object e) {
        if (!mounted) return;
        // One dropped check doesn't make a machine offline: keep what it
        // said last time unless the one before failed too.
        final misses = (failures[m.id] ?? 0) + 1;
        failures[m.id] = misses;
        if (misses < 2 && status[m.id] != null) return;
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
      if (mounted) toast(context, 'Paired with ${paired.name}', icon: Icons.link);
    }
  }

  void _select(int index) {
    if (index < machines.length) setState(() => selected = machines[index].id);
  }

  @override
  Widget build(BuildContext context) {
    final app = this.app;
    if (openError != null) {
      return Scaffold(
        body: EmptyState(
          icon: Icons.error_outline,
          title: "Couldn't open Yonder",
          message: openError,
          action: FilledButton.icon(
            onPressed: _open,
            icon: const Icon(Icons.refresh, size: 18),
            label: const Text('Try again'),
          ),
        ),
      );
    }
    if (app == null) {
      return const Scaffold(body: Center(child: _Logo(size: 56, spinning: true)));
    }
    final machine = machines.where((m) => m.id == selected).firstOrNull;
    final meta = Theme.of(context).platform == TargetPlatform.macOS;
    return CallbackShortcuts(
      bindings: {
        SingleActivator(LogicalKeyboardKey.keyN, meta: meta, control: !meta): _pair,
        SingleActivator(LogicalKeyboardKey.keyR, meta: meta, control: !meta): _reload,
        for (var i = 0; i < 9; i++)
          SingleActivator(LogicalKeyboardKey(LogicalKeyboardKey.digit1.keyId + i), meta: meta, control: !meta):
              () => _select(i),
      },
      child: Focus(
        autofocus: true,
        child: Scaffold(
          body: Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _Sidebar(
                app: app,
                machines: machines,
                status: status,
                statusError: statusError,
                lastSeen: lastSeen,
                selected: selected,
                onSelect: (id) => setState(() => selected = id),
                onPair: _pair,
              ),
              const VerticalDivider(width: 1),
              Expanded(
                child: AnimatedSwitcher(
                  duration: const Duration(milliseconds: 220),
                  child: machine == null
                      ? _Welcome(app: app, onPair: _pair)
                      : MachineView(
                          key: ValueKey(machine.id),
                          app: app,
                          machine: machine,
                          status: status[machine.id],
                          error: statusError[machine.id],
                          lastSeen: lastSeen[machine.id],
                          onChanged: _reload,
                        ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Yonder's mark: a rounded square with arrows both ways.
class _Logo extends StatefulWidget {
  const _Logo({this.size = 22, this.spinning = false});

  final double size;
  final bool spinning;

  @override
  State<_Logo> createState() => _LogoState();
}

class _LogoState extends State<_Logo> with SingleTickerProviderStateMixin {
  late final controller = AnimationController(vsync: this, duration: const Duration(seconds: 2));

  @override
  void initState() {
    super.initState();
    if (widget.spinning) controller.repeat();
  }

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final s = widget.size;
    return RotationTransition(
      turns: controller,
      child: Container(
        width: s,
        height: s,
        decoration: BoxDecoration(
          borderRadius: BorderRadius.circular(s * 0.3),
          gradient: LinearGradient(
            begin: Alignment.topLeft,
            end: Alignment.bottomRight,
            colors: [scheme.primary, Color.lerp(scheme.primary, scheme.tertiary, 0.7)!],
          ),
        ),
        child: Icon(Icons.swap_horiz_rounded, size: s * 0.65, color: scheme.onPrimary),
      ),
    );
  }
}

/// How a machine is doing, in words and a colour.
({String text, Color color, bool pulse}) describe(
  BuildContext context,
  MachineStatus? status,
  String? error,
  DateTime? lastSeen,
) {
  final tones = Tones.of(context);
  if (error != null) {
    if (versionMismatch(error)) {
      return (text: 'Needs an agent update', color: tones.warn, pulse: false);
    }
    final seen = lastSeen != null ? ' · seen ${when(BigInt.from(lastSeen.millisecondsSinceEpoch))}' : '';
    return (text: 'Offline$seen', color: tones.idle, pulse: false);
  }
  if (status == null) return (text: 'Connecting…', color: tones.idle, pulse: true);
  if (status.paused) return (text: 'Paused', color: tones.warn, pulse: false);
  if (status.runningJobs > 0 || status.queuedJobs > 0) {
    final parts = [
      if (status.runningJobs > 0) '${status.runningJobs} running',
      if (status.queuedJobs > 0) '${status.queuedJobs} queued',
    ];
    return (text: parts.join(', '), color: tones.busy, pulse: true);
  }
  return (text: 'Online', color: tones.ok, pulse: false);
}

/// The machine answered in a form this app doesn't read: a different agent
/// version. A connection that closed before answering is just a drop.
bool versionMismatch(String error) =>
    error.contains('broke the protocol') && !error.contains('stream ended before');

IconData osIcon(String os) => switch (os) {
      'win' => Icons.window_rounded,
      'mac' => Icons.laptop_mac_rounded,
      _ => Icons.dns_rounded,
    };

String osName(String os) => switch (os) {
      'win' => 'Windows',
      'mac' => 'macOS',
      'linux' => 'Linux',
      _ => os,
    };

class _Sidebar extends StatelessWidget {
  const _Sidebar({
    required this.app,
    required this.machines,
    required this.status,
    required this.statusError,
    required this.lastSeen,
    required this.selected,
    required this.onSelect,
    required this.onPair,
  });

  final YonderApp app;
  final List<Machine> machines;
  final Map<String, MachineStatus?> status;
  final Map<String, String> statusError;
  final Map<String, DateTime> lastSeen;
  final String? selected;
  final ValueChanged<String> onSelect;
  final VoidCallback onPair;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final shortcut = Theme.of(context).platform == TargetPlatform.macOS ? '⌘' : 'Ctrl+';
    return Container(
      width: 256,
      color: scheme.surfaceContainerLow,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const SizedBox(height: titleBarHeight + 8),
          Padding(
            padding: const EdgeInsets.fromLTRB(18, 0, 18, 18),
            child: Row(
              children: [
                const _Logo(),
                const SizedBox(width: 10),
                Text('Yonder', style: text.titleMedium?.copyWith(letterSpacing: -0.2)),
              ],
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(18, 0, 12, 6),
            child: Row(
              children: [
                Text('MACHINES', style: text.labelSmall?.copyWith(color: scheme.onSurfaceVariant)),
                const SizedBox(width: 6),
                if (machines.isNotEmpty)
                  Text('${machines.length}', style: text.labelSmall?.copyWith(color: scheme.outline)),
                const Spacer(),
                SizedBox(
                  width: 26,
                  height: 26,
                  child: IconButton(
                    padding: EdgeInsets.zero,
                    iconSize: 18,
                    tooltip: 'Pair a machine (${shortcut}N)',
                    onPressed: onPair,
                    icon: const Icon(Icons.add_rounded),
                  ),
                ),
              ],
            ),
          ),
          Expanded(
            child: machines.isEmpty
                ? Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 8),
                    child: Text('No machines yet.', style: text.bodySmall?.copyWith(color: scheme.outline)),
                  )
                : ListView(
                    padding: const EdgeInsets.symmetric(horizontal: 8),
                    children: [
                      for (final (i, m) in machines.indexed)
                        _MachineTile(
                          machine: m,
                          state: describe(context, status[m.id], statusError[m.id], lastSeen[m.id]),
                          selected: m.id == selected,
                          shortcut: i < 9 ? '$shortcut${i + 1}' : null,
                          onTap: () => onSelect(m.id),
                        ),
                    ],
                  ),
          ),
          const Divider(),
          _ThisDevice(app: app),
        ],
      ),
    );
  }
}

class _MachineTile extends StatefulWidget {
  const _MachineTile({
    required this.machine,
    required this.state,
    required this.selected,
    required this.shortcut,
    required this.onTap,
  });

  final Machine machine;
  final ({String text, Color color, bool pulse}) state;
  final bool selected;
  final String? shortcut;
  final VoidCallback onTap;

  @override
  State<_MachineTile> createState() => _MachineTileState();
}

class _MachineTileState extends State<_MachineTile> {
  bool hover = false;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final selected = widget.selected;
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: MouseRegion(
        onEnter: (_) => setState(() => hover = true),
        onExit: (_) => setState(() => hover = false),
        child: Tooltip(
          message: widget.shortcut ?? '',
          waitDuration: const Duration(seconds: 1),
          child: Material(
            color: selected
                ? scheme.secondaryContainer
                : hover
                    ? scheme.onSurface.withValues(alpha: 0.05)
                    : Colors.transparent,
            borderRadius: BorderRadius.circular(10),
            child: InkWell(
              borderRadius: BorderRadius.circular(10),
              onTap: widget.onTap,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
                child: Row(
                  children: [
                    IconTile(osIcon(widget.machine.os),
                        size: 32, color: selected ? scheme.onSecondaryContainer : scheme.primary),
                    const SizedBox(width: 10),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            widget.machine.name,
                            overflow: TextOverflow.ellipsis,
                            style: text.bodyMedium?.copyWith(
                              fontWeight: FontWeight.w600,
                              color: selected ? scheme.onSecondaryContainer : null,
                            ),
                          ),
                          const SizedBox(height: 2),
                          Row(
                            children: [
                              StatusDot(color: widget.state.color, pulse: widget.state.pulse, size: 7),
                              const SizedBox(width: 3),
                              Flexible(
                                child: Text(
                                  widget.state.text,
                                  overflow: TextOverflow.ellipsis,
                                  style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                                ),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// This Mac's name and fingerprint: what a machine shows when pairing.
class _ThisDevice extends StatelessWidget {
  const _ThisDevice({required this.app});

  final YonderApp app;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 10, 10, 12),
      child: Row(
        children: [
          IconTile(Icons.laptop_chromebook_rounded, size: 28, color: scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(app.deviceName(),
                    overflow: TextOverflow.ellipsis,
                    style: text.bodySmall?.copyWith(fontWeight: FontWeight.w600)),
                CopyableMono(app.fingerprint(), size: 11, what: 'Fingerprint copied'),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// The first screen: how to pair the first machine.
class _Welcome extends StatelessWidget {
  const _Welcome({required this.app, required this.onPair});

  final YonderApp app;
  final VoidCallback onPair;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final muted = text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant);
    Widget step(int n, String title, Widget body) => Padding(
          padding: const EdgeInsets.only(bottom: 18),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              CircleAvatar(
                radius: 13,
                backgroundColor: scheme.primaryContainer,
                child: Text('$n', style: text.labelMedium?.copyWith(color: scheme.onPrimaryContainer)),
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: text.titleSmall),
                    const SizedBox(height: 4),
                    body,
                  ],
                ),
              ),
            ],
          ),
        );
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(40),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: FadeIn(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const _Logo(size: 48),
                const SizedBox(height: 20),
                Text('Welcome to Yonder', style: text.headlineSmall),
                const SizedBox(height: 6),
                Text(
                  'Run builds, open shells and check on your other computers from here, '
                  'directly and end-to-end encrypted. Start by pairing one.',
                  style: text.bodyLarge?.copyWith(color: scheme.onSurfaceVariant),
                ),
                const SizedBox(height: 28),
                Card(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(20, 20, 20, 4),
                    child: Column(
                      children: [
                        step(1, 'Install the agent on the other computer',
                            Text("Follow the install steps in Yonder's README for its system.", style: muted)),
                        step(
                          2,
                          'Ask it for a pairing ticket',
                          Row(children: [
                            Text('Run ', style: muted),
                            const CopyableMono('yonder-agent pair', size: 12.5),
                            Text(' there.', style: muted),
                          ]),
                        ),
                        step(3, 'Paste the ticket here and approve',
                            Text("The machine asks you to check this device's name and fingerprint.", style: muted)),
                      ],
                    ),
                  ),
                ),
                const SizedBox(height: 20),
                FilledButton.icon(
                  onPressed: onPair,
                  icon: const Icon(Icons.link_rounded, size: 18),
                  label: const Text('Pair a machine'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// One machine: its state, then jobs, terminal, footprint and devices.
class MachineView extends StatelessWidget {
  const MachineView({
    super.key,
    required this.app,
    required this.machine,
    required this.status,
    required this.error,
    required this.lastSeen,
    required this.onChanged,
  });

  final YonderApp app;
  final Machine machine;
  final MachineStatus? status;
  final String? error;
  final DateTime? lastSeen;
  final VoidCallback onChanged;

  @override
  Widget build(BuildContext context) {
    final status = this.status;
    final error = this.error;
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    final state = describe(context, status, error, lastSeen);
    final mismatch = error != null && versionMismatch(error);
    final busy = status != null ? status.runningJobs + status.queuedJobs : 0;
    Tab tab(IconData icon, String label, [Widget? badge]) => Tab(
          height: 40,
          child: Row(children: [
            Icon(icon, size: 18),
            const SizedBox(width: 6),
            Text(label),
            if (badge != null) ...[const SizedBox(width: 6), badge],
          ]),
        );
    return DefaultTabController(
      length: 4,
      child: Builder(builder: (context) {
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const SizedBox(height: titleBarHeight),
            Padding(
              padding: const EdgeInsets.fromLTRB(28, 6, 24, 0),
              child: Row(
                children: [
                  IconTile(osIcon(machine.os), size: 44),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(machine.name, style: text.headlineSmall, overflow: TextOverflow.ellipsis),
                        const SizedBox(height: 4),
                        Wrap(
                          spacing: 8,
                          runSpacing: 4,
                          crossAxisAlignment: WrapCrossAlignment.center,
                          children: [
                            Pill(state.text, color: state.color, dot: true, pulse: state.pulse),
                            Text(
                              [
                                osName(machine.os),
                                if (status != null) 'agent ${status.agentVersion}',
                                'paired ${date(machine.pairedAt)}',
                              ].join('  ·  '),
                              style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                            ),
                          ],
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 12),
                  IconButton(tooltip: 'Refresh', onPressed: onChanged, icon: const Icon(Icons.refresh_rounded)),
                  const SizedBox(width: 4),
                  OutlinedButton.icon(
                    onPressed: () => showDialog<void>(
                      context: context,
                      builder: (_) => _ScreenshotDialog(app: app, machine: machine),
                    ),
                    icon: const Icon(Icons.screenshot_monitor_outlined, size: 18),
                    label: const Text('Screen'),
                  ),
                  const SizedBox(width: 8),
                  OutlinedButton.icon(
                    onPressed: () async {
                      final job = await showDialog<Job>(
                        context: context,
                        builder: (_) => PrepareDialog(app: app, machine: machine),
                      );
                      if (job == null || !context.mounted) return;
                      DefaultTabController.of(context).animateTo(0);
                      toast(context, 'Preparing ${machine.name}: follow it in Jobs.', icon: Icons.build_outlined);
                    },
                    icon: const Icon(Icons.build_outlined, size: 18),
                    label: const Text('Prepare'),
                  ),
                ],
              ),
            ),
            if (mismatch)
              _notice(Notice(
                icon: Icons.system_update_alt_rounded,
                color: tones.warn,
                text: '${machine.name} runs a different version of the agent than this app. '
                    'Update it from a terminal here: yonder upgrade ${machine.name}',
                action: TextButton(
                  onPressed: () => copy(context, 'yonder upgrade ${machine.name}'),
                  child: const Text('Copy command'),
                ),
              ))
            else if (error != null)
              _notice(Notice(
                icon: Icons.cloud_off_rounded,
                color: scheme.outline,
                text: "Can't reach ${machine.name}: $error",
                action: TextButton(onPressed: onChanged, child: const Text('Retry')),
              ))
            else if (status != null && status.paused)
              _notice(Notice(
                icon: Icons.pause_circle_outline,
                color: tones.warn,
                text: 'The agent is paused: new jobs wait until it resumes (yonder-agent resume on the machine).',
              )),
            if (status != null && status.cpuPercent != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(24, 16, 24, 0),
                child: _Metrics(status: status),
              ),
            const SizedBox(height: 10),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: TabBar(
                isScrollable: true,
                tabAlignment: TabAlignment.start,
                dividerColor: Colors.transparent,
                labelStyle: text.labelLarge?.copyWith(fontWeight: FontWeight.w600),
                tabs: [
                  tab(Icons.play_circle_outline_rounded, 'Jobs',
                      busy > 0 ? Badge(label: Text('$busy'), backgroundColor: tones.busy) : null),
                  tab(Icons.terminal_rounded, 'Terminal'),
                  tab(Icons.inventory_2_outlined, 'Footprint'),
                  tab(Icons.devices_rounded, 'Devices'),
                ],
              ),
            ),
            const Divider(),
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
        );
      }),
    );
  }

  Widget _notice(Widget child) =>
      Padding(padding: const EdgeInsets.fromLTRB(24, 14, 24, 0), child: FadeIn(child: child));
}

/// CPU, memory, disk and uptime as small cards with meters.
class _Metrics extends StatelessWidget {
  const _Metrics({required this.status});

  final MachineStatus status;

  @override
  Widget build(BuildContext context) {
    final tones = Tones.of(context);
    final cpu = status.cpuPercent!;
    double? share(BigInt? part, BigInt? total) =>
        part == null || total == null || total == BigInt.zero ? null : part.toDouble() / total.toDouble();
    final memory = share(status.memoryUsed, status.memoryTotal);
    final diskUsed = status.diskTotal != null && status.diskFree != null ? status.diskTotal! - status.diskFree! : null;
    final disk = share(diskUsed, status.diskTotal);
    Color level(double v) => v > 0.9 ? tones.bad : v > 0.75 ? tones.warn : tones.ok;
    final restart = status.pendingRestart == true;
    final cards = [
      _Metric(icon: Icons.speed_rounded, label: 'CPU', value: '$cpu%', meter: cpu / 100, color: level(cpu / 100)),
      if (memory != null)
        _Metric(
          icon: Icons.memory_rounded,
          label: 'Memory',
          value: bytes(status.memoryUsed!),
          detail: 'of ${bytes(status.memoryTotal!)}',
          meter: memory,
          color: level(memory),
        ),
      if (disk != null)
        _Metric(
          icon: Icons.storage_rounded,
          label: 'Disk',
          value: '${bytes(status.diskFree!)} free',
          detail: 'of ${bytes(status.diskTotal!)}',
          meter: disk,
          color: level(disk),
        ),
      if (status.uptimeSecs != null)
        _Metric(
          icon: restart ? Icons.restart_alt_rounded : Icons.schedule_rounded,
          label: 'Up',
          value: uptime(status.uptimeSecs!),
          detail: restart ? 'updates wait for a restart' : null,
          color: restart ? tones.warn : null,
        ),
    ];
    return Row(
      children: [
        for (final (i, c) in cards.indexed) ...[
          if (i > 0) const SizedBox(width: 10),
          Expanded(child: c),
        ],
      ],
    );
  }
}

class _Metric extends StatelessWidget {
  const _Metric({
    required this.icon,
    required this.label,
    required this.value,
    this.detail,
    this.meter,
    this.color,
  });

  final IconData icon;
  final String label;
  final String value;
  final String? detail;
  final double? meter;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(icon, size: 14, color: color ?? scheme.onSurfaceVariant),
                const SizedBox(width: 5),
                Text(label.toUpperCase(), style: text.labelSmall?.copyWith(color: scheme.onSurfaceVariant)),
              ],
            ),
            const SizedBox(height: 4),
            Text.rich(
              TextSpan(children: [
                TextSpan(text: value, style: text.titleSmall),
                if (detail != null)
                  TextSpan(text: '  $detail', style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
              ]),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
            const SizedBox(height: 7),
            SizedBox(
              height: 4,
              child: meter == null
                  ? null
                  : TweenAnimationBuilder<double>(
                      tween: Tween(end: meter!.clamp(0, 1).toDouble()),
                      duration: const Duration(milliseconds: 600),
                      curve: Curves.easeOutCubic,
                      builder: (context, v, _) => ClipRRect(
                        borderRadius: BorderRadius.circular(4),
                        child: LinearProgressIndicator(
                          value: v,
                          minHeight: 4,
                          color: color,
                          backgroundColor: scheme.surfaceContainerHighest,
                        ),
                      ),
                    ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A picture of the machine's screen, taken when opened; "Retake" for a new one.
class _ScreenshotDialog extends StatefulWidget {
  const _ScreenshotDialog({required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<_ScreenshotDialog> createState() => _ScreenshotDialogState();
}

class _ScreenshotDialogState extends State<_ScreenshotDialog> {
  late Future<Uint8List> shot;
  DateTime taken = DateTime.now();

  @override
  void initState() {
    super.initState();
    _take();
  }

  void _take() {
    setState(() {
      taken = DateTime.now();
      shot = widget.app.screenshot(machineId: widget.machine.id).then(Uint8List.fromList);
    });
  }

  @override
  Widget build(BuildContext context) {
    final size = MediaQuery.sizeOf(context);
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    return Dialog(
      insetPadding: const EdgeInsets.all(32),
      clipBehavior: Clip.antiAlias,
      child: SizedBox(
        width: size.width * 0.85,
        height: size.height * 0.85,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 12, 12, 12),
              child: Row(
                children: [
                  Text("${widget.machine.name}'s screen", style: text.titleMedium),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      'taken ${when(BigInt.from(taken.millisecondsSinceEpoch))} · scroll or pinch to zoom',
                      overflow: TextOverflow.ellipsis,
                      style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                    ),
                  ),
                  TextButton.icon(
                    onPressed: _take,
                    icon: const Icon(Icons.refresh_rounded, size: 18),
                    label: const Text('Retake'),
                  ),
                  IconButton(
                    tooltip: 'Close',
                    onPressed: () => Navigator.pop(context),
                    icon: const Icon(Icons.close_rounded),
                  ),
                ],
              ),
            ),
            const Divider(),
            Expanded(
              child: Container(
                color: scheme.surfaceContainerLowest,
                child: FutureBuilder<Uint8List>(
                  future: shot,
                  builder: (context, snapshot) {
                    if (snapshot.connectionState != ConnectionState.done) {
                      return const EmptyState(icon: Icons.screenshot_monitor_outlined, title: 'Taking a screenshot…');
                    }
                    if (snapshot.hasError) {
                      return EmptyState(
                        icon: Icons.desktop_access_disabled_outlined,
                        title: "Couldn't take a screenshot",
                        message: errorText(snapshot.error!),
                        action: OutlinedButton(onPressed: _take, child: const Text('Try again')),
                      );
                    }
                    return InteractiveViewer(
                      maxScale: 6,
                      child: Center(child: Image.memory(snapshot.data!, fit: BoxFit.contain)),
                    );
                  },
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
