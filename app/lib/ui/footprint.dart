import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

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
  String? done;
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

  Future<void> _clean(String what, {String? confirm}) async {
    if (confirm != null && !await _confirm(confirm)) return;
    setState(() => busy = true);
    try {
      final r = await widget.app.clean(machineId: widget.machine.id, what: what);
      done = _describe(r);
    } catch (e) {
      error = errorText(e);
    }
    setState(() => busy = false);
    _load();
  }

  Future<void> _unpair() async {
    if (!await _confirm(
      'Remove everything of this device\'s from ${widget.machine.name}, then unpair? '
      'You\'d need a new ticket from the machine to pair again.',
    )) {
      return;
    }
    setState(() => busy = true);
    try {
      await widget.app.unpair(machineId: widget.machine.id, keep: false);
      widget.onUnpaired();
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
    if (mounted) setState(() => busy = false);
  }

  Future<void> _setCleanup(int? days) async {
    try {
      await widget.app.setCleanup(machineId: widget.machine.id, afterDays: days);
    } catch (e) {
      error = errorText(e);
    }
    _load();
  }

  Future<bool> _confirm(String question) async {
    return await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            content: Text(question),
            actions: [
              TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
              FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('Remove')),
            ],
          ),
        ) ??
        false;
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
    if (fp == null) {
      return Center(child: error != null ? Text(error!) : const CircularProgressIndicator());
    }
    final total = fp.projects.fold(BigInt.zero, (sum, p) => sum + p.bytes) + fp.jobBytes;
    final empty = fp.secrets.isEmpty && fp.projects.isEmpty && fp.jobs == 0 && fp.shells.isEmpty;
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text(
          empty ? 'Nothing of this device\'s is left on ${widget.machine.name}.' : '${bytes(total)} of this device\'s on ${widget.machine.name}',
          style: text.titleMedium,
        ),
        const SizedBox(height: 8),
        _Fact(
          icon: fp.diskEncryption == 'off' ? Icons.lock_open : Icons.lock,
          color: fp.diskEncryption == 'off' ? scheme.error : null,
          text: switch (fp.diskEncryption) {
            'on' => 'Disk encryption is on.',
            'off' => 'Disk encryption is OFF: secrets and project copies sit unencrypted on its disk.',
            _ => 'Disk encryption: unknown.',
          },
        ),
        Row(
          children: [
            const Icon(Icons.auto_delete_outlined, size: 18),
            const SizedBox(width: 8),
            Expanded(
              child: Text(fp.cleanupAfterDays != null
                  ? 'Auto-cleanup: project copies unused for ${fp.cleanupAfterDays} days are removed.'
                  : 'Auto-cleanup is off.'),
            ),
            if (fp.cleanupAfterDays != null)
              DropdownButton<int>(
                value: fp.cleanupAfterDays,
                items: {7, 14, 30, 60, 90, fp.cleanupAfterDays!}
                    .map((d) => DropdownMenuItem(value: d, child: Text('$d days')))
                    .toList(),
                onChanged: (d) => _setCleanup(d),
              ),
            Switch(
              value: fp.cleanupAfterDays != null,
              onChanged: (on) => _setCleanup(on ? 30 : null),
            ),
          ],
        ),
        if (done != null)
          Padding(padding: const EdgeInsets.only(top: 8), child: Text(done!, style: text.bodySmall)),
        if (error != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(error!, style: TextStyle(color: scheme.error)),
          ),
        if (fp.secrets.isNotEmpty) ...[
          _Heading('Secrets'),
          for (final s in fp.secrets)
            ListTile(
              dense: true,
              leading: const Icon(Icons.key),
              title: Text(s.name),
              subtitle: Text([
                if (s.note != null) s.note!,
                s.lastUsedMs != null ? 'last used ${when(s.lastUsedMs)}' : 'never used',
                if (s.expiresAtMs != null) 'expires ${when(s.expiresAtMs)}',
              ].join(' · ')),
              trailing: _RemoveButton(
                onPressed: busy ? null : () => _clean('secret:${s.name}', confirm: 'Delete the secret ${s.name}? Jobs that use it will stop working.'),
              ),
            ),
        ],
        if (fp.projects.isNotEmpty) ...[
          _Heading('Project copies'),
          for (final p in fp.projects)
            ListTile(
              dense: true,
              leading: const Icon(Icons.folder_outlined),
              title: Text(p.id),
              subtitle: Text('${bytes(p.bytes)} in ${p.files} files · synced ${when(p.lastSyncedMs)}'),
              trailing: _RemoveButton(
                onPressed: busy ? null : () => _clean('project:${p.id}', confirm: 'Delete ${p.id} and its build output from the machine? The next sync sends it all again.'),
              ),
            ),
        ],
        if (fp.jobs > 0) ...[
          _Heading('Jobs'),
          ListTile(
            dense: true,
            leading: const Icon(Icons.history),
            title: Text('${fp.jobs} jobs${fp.unfinishedJobs > 0 ? ', ${fp.unfinishedJobs} unfinished' : ''}'),
            subtitle: Text('${bytes(fp.jobBytes)} of logs'),
            trailing: _RemoveButton(
              onPressed: busy ? null : () => _clean('jobs', confirm: 'Delete your finished jobs and their logs? Unfinished ones are cancelled.'),
            ),
          ),
        ],
        if (fp.shells.isNotEmpty) ...[
          _Heading('Shells'),
          ListTile(
            dense: true,
            leading: const Icon(Icons.terminal),
            title: Text('${fp.shells.length} shells'),
            trailing: _RemoveButton(
              onPressed: busy ? null : () => _clean('shells', confirm: 'Close all your shells on the machine?'),
            ),
          ),
        ],
        const SizedBox(height: 24),
        Wrap(
          spacing: 12,
          children: [
            if (!empty)
              FilledButton.tonal(
                onPressed: busy
                    ? null
                    : () => _clean('everything', confirm: 'Remove everything of this device\'s from ${widget.machine.name}? You stay paired.'),
                child: const Text('Remove everything'),
              ),
            OutlinedButton(onPressed: busy ? null : _unpair, child: const Text('Remove everything and unpair')),
          ],
        ),
      ],
    );
  }
}

class _Heading extends StatelessWidget {
  const _Heading(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(top: 20, bottom: 4),
        child: Text(text, style: Theme.of(context).textTheme.titleSmall),
      );
}

class _Fact extends StatelessWidget {
  const _Fact({required this.icon, required this.text, this.color});

  final IconData icon;
  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Row(
          children: [
            Icon(icon, size: 18, color: color),
            const SizedBox(width: 8),
            Expanded(child: Text(text, style: TextStyle(color: color))),
          ],
        ),
      );
}

class _RemoveButton extends StatelessWidget {
  const _RemoveButton({required this.onPressed});

  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) =>
      IconButton(tooltip: 'Remove', onPressed: onPressed, icon: const Icon(Icons.delete_outline));
}
