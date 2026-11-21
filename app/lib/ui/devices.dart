import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

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

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final list = await widget.app.devices(machineId: widget.machine.id);
      if (mounted) {
        setState(() {
          devices = list;
          error = null;
        });
      }
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  Future<void> _revoke(Device device) async {
    var keep = false;
    final scheme = Theme.of(context).colorScheme;
    final go = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setDialog) => AlertDialog(
          icon: Icon(Icons.gpp_bad_outlined, color: scheme.error),
          title: Text('Revoke ${device.name}?'),
          content: SizedBox(
            width: 420,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text("${device.name} won't be able to reach ${widget.machine.name} any more."),
                const SizedBox(height: 8),
                CheckboxListTile(
                  contentPadding: EdgeInsets.zero,
                  value: !keep,
                  onChanged: (v) => setDialog(() => keep = !(v ?? true)),
                  title: const Text('Also remove its secrets, project copies, jobs and shells'),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
            FilledButton(
              style: FilledButton.styleFrom(backgroundColor: scheme.error, foregroundColor: scheme.onError),
              onPressed: () => Navigator.pop(context, true),
              child: const Text('Revoke'),
            ),
          ],
        ),
      ),
    );
    if (go != true) return;
    try {
      final r = await widget.app.revoke(machineId: widget.machine.id, deviceId: device.id, keep: keep);
      if (mounted) {
        toast(
          context,
          'Revoked ${device.name}'
          '${keep ? '' : '; removed ${r.secrets} secrets, ${r.projects} project copies, ${r.jobs} jobs (${bytes(r.bytes)})'}.',
          icon: Icons.gpp_good_outlined,
        );
      }
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
    _load();
  }

  @override
  Widget build(BuildContext context) {
    final devices = this.devices;
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    if (devices == null) {
      return error != null
          ? EmptyState(
              icon: Icons.cloud_off_rounded,
              title: "Couldn't load devices",
              message: error,
              action: OutlinedButton(onPressed: _load, child: const Text('Try again')),
            )
          : const Center(child: CircularProgressIndicator());
    }
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 18, 24, 24),
      children: [
        Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('Paired devices', style: text.titleMedium),
                  Text(
                    'Lost a laptop? Revoke it here from any device with the "manage machine" permission.',
                    style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                  ),
                ],
              ),
            ),
            IconButton(tooltip: 'Refresh', onPressed: _load, icon: const Icon(Icons.refresh_rounded)),
          ],
        ),
        const SizedBox(height: 14),
        if (error != null) ErrorText(error!),
        for (final (i, d) in devices.indexed)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: FadeIn(
              delay: Duration(milliseconds: 30 * i),
              child: Card(
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(14, 12, 12, 12),
                  child: Row(
                    children: [
                      IconTile(d.isThisDevice ? Icons.laptop_mac_rounded : Icons.devices_other_rounded,
                          color: d.isThisDevice ? scheme.primary : scheme.onSurfaceVariant),
                      const SizedBox(width: 12),
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                Flexible(
                                  child: Text(d.name,
                                      overflow: TextOverflow.ellipsis,
                                      style: text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
                                ),
                                if (d.isThisDevice) ...[
                                  const SizedBox(width: 8),
                                  Pill('this device', color: tones.ok),
                                ],
                              ],
                            ),
                            const SizedBox(height: 2),
                            Row(
                              children: [
                                CopyableMono(d.shortId, size: 11, what: 'Device id copied'),
                                Text(' · paired ${date(d.pairedAt)}',
                                    style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                              ],
                            ),
                            const SizedBox(height: 6),
                            Wrap(
                              spacing: 4,
                              runSpacing: 4,
                              children: [
                                for (final p in d.permissions)
                                  Pill(p.replaceAll('_', ' '), color: p == 'admin' ? tones.warn : scheme.primary),
                              ],
                            ),
                          ],
                        ),
                      ),
                      if (!d.isThisDevice)
                        OutlinedButton(
                          style: OutlinedButton.styleFrom(foregroundColor: scheme.error),
                          onPressed: () => _revoke(d),
                          child: const Text('Revoke'),
                        ),
                    ],
                  ),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
