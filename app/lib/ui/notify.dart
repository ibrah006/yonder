import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import '../src/rust/api/yonder.dart';

/// Notifies when a job on any machine finishes, so long builds can run unwatched.
class JobNotifier {
  final _plugin = FlutterLocalNotificationsPlugin();
  bool _ready = false;
  int _nextId = 1;

  /// Job states seen last time, per machine; a machine's first check only records them.
  final Map<String, Map<String, String>> _seen = {};

  Future<void> init() async {
    try {
      _ready = await _plugin.initialize(
            settings: const InitializationSettings(macOS: DarwinInitializationSettings()),
          ) ??
          false;
    } catch (_) {
      _ready = false;
    }
  }

  /// Compares `jobs` with last time and notifies for each that finished since.
  void check(Machine machine, List<Job> jobs) {
    final before = _seen[machine.id];
    _seen[machine.id] = {for (final j in jobs) j.id: j.state};
    if (before == null || !_ready) return;
    for (final job in jobs) {
      final was = before[job.id];
      final unfinished = was == 'running' || was == 'queued';
      if (!unfinished || job.state == 'running' || job.state == 'queued') continue;
      final what = job.label ?? job.command;
      final outcome = switch (job.state) {
        'succeeded' => 'succeeded',
        'failed' => job.exitCode != null ? 'failed (exit ${job.exitCode})' : 'failed',
        'cancelled' => 'was cancelled',
        _ => 'was lost',
      };
      _plugin.show(
        id: _nextId++,
        title: '${machine.name}: $outcome',
        body: what,
      );
    }
  }
}
