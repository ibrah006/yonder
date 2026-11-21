import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

/// What this device has left on a machine, and removing it.
class FootprintTab extends StatefulWidget {
  const FootprintTab({
    super.key,
    required this.app,
    required this.machine,
    required this.onUnpaired,
  });

  final YonderApp app;
  final Machine machine;
  final VoidCallback onUnpaired;

  @override
  State<FootprintTab> createState() => _FootprintTabState();
}

class _FootprintTabState extends State<FootprintTab> {
  Footprint? footprint;
  String? error;
  bool busy = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final fp = await widget.app.footprint(machineId: widget.machine.id);
      if (mounted) {
        setState(() {
          footprint = fp;
          error = null;
        });
      }
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  Future<void> _clean(String what, {required String title, required String message}) async {
    if (!await confirm(context, title: title, message: message, action: 'Remove')) return;
    setState(() => busy = true);
    try {
      final r = await widget.app.clean(machineId: widget.machine.id, what: what);
      if (mounted) toast(context, _describe(r), icon: Icons.delete_sweep_outlined);
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
    if (mounted) setState(() => busy = false);
    _load();
  }

  Future<void> _unpair() async {
    if (!await confirm(
      context,
      title: 'Unpair from ${widget.machine.name}?',
      message: "Everything of this device's is removed from ${widget.machine.name} first. "
          "You'd need a new ticket from the machine to pair again.",
      action: 'Remove and unpair',
    )) {
      return;
    }
    setState(() => busy = true);
    try {
      await widget.app.unpair(machineId: widget.machine.id, keep: false);
      widget.onUnpaired();
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
    if (mounted) setState(() => busy = false);
  }

  Future<void> _setCleanup(int? days) async {
    try {
      await widget.app.setCleanup(machineId: widget.machine.id, afterDays: days);
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
    _load();
  }

  String _describe(Removed r) {
    final parts = [
      if (r.secrets > 0) '${r.secrets} secrets',
      if (r.projects > 0) '${r.projects} project copies',
      if (r.jobs > 0) '${r.jobs} jobs',
      if (r.shells > 0) '${r.shells} shells',
    ];
    final what = parts.isEmpty ? 'Nothing to remove' : 'Removed ${parts.join(', ')} (${bytes(r.bytes)} freed)';
    return [what, ...r.kept.map((k) => 'Kept: $k')].join('\n');
  }

  @override
  Widget build(BuildContext context) {
    final fp = footprint;
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    if (fp == null) {
      return error != null
          ? EmptyState(
              icon: Icons.cloud_off_rounded,
              title: "Couldn't load the footprint",
              message: error,
              action: OutlinedButton(onPressed: _load, child: const Text('Try again')),
            )
          : const Center(child: CircularProgressIndicator());
    }
    final projectBytes = fp.projects.fold(BigInt.zero, (sum, p) => sum + p.bytes);
    final total = projectBytes + fp.jobBytes;
    final empty = fp.secrets.isEmpty && fp.projects.isEmpty && fp.jobs == 0 && fp.shells.isEmpty;
    final encryption = fp.diskEncryption;
    final remove = busy ? null : _clean;
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 18, 24, 32),
      children: [
        // Summary.
        Card(
          child: Padding(
            padding: const EdgeInsets.all(18),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    Text(empty ? 'Nothing' : bytes(total), style: text.headlineSmall),
                    const SizedBox(width: 8),
                    Padding(
                      padding: const EdgeInsets.only(bottom: 4),
                      child: Text("of this device's on ${widget.machine.name}",
                          style: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant)),
                    ),
                    const Spacer(),
                    IconButton(tooltip: 'Refresh', onPressed: _load, icon: const Icon(Icons.refresh_rounded)),
                  ],
                ),
                if (total > BigInt.zero) ...[
                  const SizedBox(height: 12),
                  _Breakdown(parts: [
                    (label: 'Project copies', value: projectBytes, color: scheme.primary),
                    (label: 'Job logs', value: fp.jobBytes, color: scheme.tertiary),
                  ]),
                ],
                const SizedBox(height: 14),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    Pill(
                      switch (encryption) {
                        'on' => 'Disk encrypted',
                        'off' => 'Disk NOT encrypted',
                        _ => 'Disk encryption unknown',
                      },
                      icon: encryption == 'off' ? Icons.lock_open_rounded : Icons.lock_rounded,
                      color: switch (encryption) {
                        'on' => tones.ok,
                        'off' => tones.bad,
                        _ => tones.idle,
                      },
                    ),
                    Pill(count(fp.secrets.length, 'secret'), icon: Icons.key_rounded, color: tones.idle),
                    Pill(count(fp.projects.length, 'project'), icon: Icons.folder_outlined, color: tones.idle),
                    Pill(count(fp.jobs, 'job'), icon: Icons.history_rounded, color: tones.idle),
                    Pill(count(fp.shells.length, 'shell'), icon: Icons.terminal_rounded, color: tones.idle),
                  ],
                ),
                if (encryption == 'off') ...[
                  const SizedBox(height: 12),
                  Notice(
                    icon: Icons.lock_open_rounded,
                    color: tones.bad,
                    text: 'Secrets and project copies sit unencrypted on its disk. Turn on BitLocker or FileVault there.',
                  ),
                ],
              ],
            ),
          ),
        ),
        const SizedBox(height: 14),
        Section(
          title: 'Auto-cleanup',
          subtitle: fp.cleanupAfterDays != null
              ? 'Project copies unused for ${fp.cleanupAfterDays} days are removed.'
              : 'Off: project copies stay until you remove them.',
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (fp.cleanupAfterDays != null)
                DropdownButton<int>(
                  value: fp.cleanupAfterDays,
                  underline: const SizedBox(),
                  borderRadius: BorderRadius.circular(10),
                  items: ({7, 14, 30, 60, 90, fp.cleanupAfterDays!}.toList()..sort())
                      .map((d) => DropdownMenuItem(value: d, child: Text('after $d days')))
                      .toList(),
                  onChanged: (d) => _setCleanup(d),
                ),
              const SizedBox(width: 8),
              Switch(value: fp.cleanupAfterDays != null, onChanged: (on) => _setCleanup(on ? 30 : null)),
            ],
          ),
          children: const [],
        ),
        if (fp.secrets.isNotEmpty) ...[
          const SizedBox(height: 14),
          Section(
            title: 'Secrets',
            subtitle: 'Kept encrypted for jobs that ask for them.',
            children: [
              for (final s in fp.secrets)
                _Row(
                  icon: Icons.key_rounded,
                  title: s.name,
                  mono: true,
                  subtitle: [
                    if (s.note != null) s.note!,
                    s.lastUsedMs != null ? 'last used ${when(s.lastUsedMs)}' : 'never used',
                    if (s.expiresAtMs != null) 'expires ${when(s.expiresAtMs)}',
                  ].join(' · '),
                  onRemove: remove == null
                      ? null
                      : () => remove('secret:${s.name}',
                          title: 'Delete the secret ${s.name}?', message: 'Jobs that use it will stop working.'),
                ),
            ],
          ),
        ],
        if (fp.projects.isNotEmpty) ...[
          const SizedBox(height: 14),
          Section(
            title: 'Project copies',
            subtitle: 'Synced from this device, with their build output.',
            children: [
              for (final p in fp.projects)
                _Row(
                  icon: Icons.folder_outlined,
                  title: p.id,
                  mono: true,
                  subtitle: '${bytes(p.bytes)} in ${p.files} files · synced ${when(p.lastSyncedMs)}',
                  onRemove: remove == null
                      ? null
                      : () => remove('project:${p.id}',
                          title: 'Delete ${p.id}?',
                          message: 'Its copy and build output go from the machine. The next sync sends it all again.'),
                ),
            ],
          ),
        ],
        if (fp.jobs > 0 || fp.shells.isNotEmpty) ...[
          const SizedBox(height: 14),
          Section(
            title: 'History',
            children: [
              if (fp.jobs > 0)
                _Row(
                  icon: Icons.history_rounded,
                  title: '${count(fp.jobs, 'job')}${fp.unfinishedJobs > 0 ? ', ${fp.unfinishedJobs} unfinished' : ''}',
                  subtitle: '${bytes(fp.jobBytes)} of logs',
                  onRemove: remove == null
                      ? null
                      : () => remove('jobs',
                          title: 'Delete your jobs?',
                          message: 'Finished jobs and their logs are deleted; unfinished ones are cancelled.'),
                ),
              if (fp.shells.isNotEmpty)
                _Row(
                  icon: Icons.terminal_rounded,
                  title: count(fp.shells.length, 'shell'),
                  subtitle: 'Running or kept for reattaching',
                  onRemove: remove == null
                      ? null
                      : () => remove('shells', title: 'Close all your shells?', message: 'Anything running in them stops.'),
                ),
            ],
          ),
        ],
        const SizedBox(height: 14),
        Container(
          padding: const EdgeInsets.all(16),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(12),
            border: Border.all(color: scheme.error.withValues(alpha: 0.35)),
          ),
          child: Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text('Leave no trace', style: text.titleSmall),
                    Text('Remove everything above from ${widget.machine.name}, and optionally unpair.',
                        style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                  ],
                ),
              ),
              if (!empty)
                OutlinedButton(
                  style: OutlinedButton.styleFrom(foregroundColor: scheme.error),
                  onPressed: remove == null
                      ? null
                      : () => remove('everything',
                          title: 'Remove everything?',
                          message: "Everything of this device's goes from ${widget.machine.name}. You stay paired."),
                  child: const Text('Remove everything'),
                ),
              const SizedBox(width: 8),
              FilledButton(
                style: FilledButton.styleFrom(backgroundColor: scheme.error, foregroundColor: scheme.onError),
                onPressed: busy ? null : _unpair,
                child: const Text('Remove and unpair'),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A stacked bar of where the bytes go, with a legend.
class _Breakdown extends StatelessWidget {
  const _Breakdown({required this.parts});

  final List<({String label, BigInt value, Color color})> parts;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    final shown = parts.where((p) => p.value > BigInt.zero).toList();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        LayoutBuilder(builder: (context, constraints) {
          final total = shown.fold(0.0, (sum, p) => sum + p.value.toDouble());
          final room = constraints.maxWidth - 2 * (shown.length - 1);
          // Every part gets at least a sliver so small ones still show; the
          // largest gives up the room for them.
          final widths = [for (final p in shown) (room * p.value.toDouble() / total).clamp(4.0, room)];
          final over = widths.fold(0.0, (a, b) => a + b) - room;
          final largest = widths.indexOf(widths.reduce((a, b) => a > b ? a : b));
          widths[largest] -= over;
          return ClipRRect(
            borderRadius: BorderRadius.circular(5),
            child: Container(
              height: 10,
              width: constraints.maxWidth,
              color: scheme.surfaceContainerHighest,
              child: Row(
                children: [
                  for (final (i, p) in shown.indexed) ...[
                    if (i > 0) const SizedBox(width: 2),
                    Container(width: widths[i], color: p.color),
                  ],
                ],
              ),
            ),
          );
        }),
        const SizedBox(height: 8),
        Wrap(
          spacing: 16,
          children: [
            for (final p in shown)
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Container(
                    width: 8,
                    height: 8,
                    decoration: BoxDecoration(color: p.color, borderRadius: BorderRadius.circular(2)),
                  ),
                  const SizedBox(width: 6),
                  Text('${p.label} ', style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                  Text(bytes(p.value), style: text.bodySmall?.copyWith(fontWeight: FontWeight.w600)),
                ],
              ),
          ],
        ),
      ],
    );
  }
}

class _Row extends StatelessWidget {
  const _Row({required this.icon, required this.title, required this.subtitle, this.onRemove, this.mono = false});

  final IconData icon;
  final String title;
  final String subtitle;
  final VoidCallback? onRemove;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(
        children: [
          IconTile(icon, size: 30, color: scheme.onSurfaceVariant),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(title,
                    overflow: TextOverflow.ellipsis,
                    style: mono
                        ? monoStyle(size: 12.5, weight: FontWeight.w600, color: scheme.onSurface)
                        : text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
                Text(subtitle,
                    overflow: TextOverflow.ellipsis,
                    style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
              ],
            ),
          ),
          IconButton(
            tooltip: 'Remove',
            onPressed: onRemove,
            icon: const Icon(Icons.delete_outline_rounded),
            color: scheme.onSurfaceVariant,
          ),
        ],
      ),
    );
  }
}
