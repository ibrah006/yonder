import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

/// Devices paired with a machine, and revoking a lost one.
class DevicesTab extends StatefulWidget {
  const DevicesTab({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<DevicesTab> createState() => _DevicesTabState();
}

class _DevicesTabState extends State<DevicesTab> {
  List<Device>? devices;
  String? error;
  String? done;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final list = await widget.app.devices(machineId: widget.machine.id);
      if (mounted) setState(() => devices = list);
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  Future<void> _revoke(Device device) async {
    var keep = false;
    final go = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setDialog) => AlertDialog(
          title: Text('Revoke ${device.name}?'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text('${device.name} won\'t be able to reach ${widget.machine.name} any more.'),
              CheckboxListTile(
                contentPadding: EdgeInsets.zero,
                value: !keep,
                onChanged: (v) => setDialog(() => keep = !(v ?? true)),
                title: const Text('Also remove its secrets, project copies, jobs and shells'),
              ),
            ],
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
            FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('Revoke')),
          ],
        ),
      ),
    );
    if (go != true) return;
    try {
      final r = await widget.app.revoke(machineId: widget.machine.id, deviceId: device.id, keep: keep);
      done = 'Revoked ${device.name}${keep ? '' : '; removed ${r.secrets} secrets, ${r.projects} project copies, ${r.jobs} jobs (${bytes(r.bytes)})'}.';
    } catch (e) {
      error = errorText(e);
    }
    _load();
  }

  @override
  Widget build(BuildContext context) {
    final devices = this.devices;
    if (devices == null) {
      return Center(child: error != null ? Text(error!) : const CircularProgressIndicator());
    }
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text(
          'Lost a laptop? Revoke it here, from any other device that has the "manage machine" permission.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        if (done != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(done!)),
        if (error != null)
          Text(error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
        for (final d in devices)
          ListTile(
            leading: Icon(d.isThisDevice ? Icons.laptop : Icons.devices_other),
            title: Text(d.isThisDevice ? '${d.name} (this device)' : d.name),
            subtitle: Text('${d.shortId} · paired ${date(d.pairedAt)} · ${d.permissions.join(', ')}'),
            trailing: d.isThisDevice
                ? null
                : OutlinedButton(onPressed: () => _revoke(d), child: const Text('Revoke')),
          ),
      ],
    );
  }
}
