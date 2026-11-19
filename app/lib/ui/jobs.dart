import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

/// Jobs on one machine: the list, a run dialog, and the selected job's live log.
class JobsTab extends StatefulWidget {
  const JobsTab({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<JobsTab> createState() => _JobsTabState();
}

class _JobsTabState extends State<JobsTab> {
  List<Job> jobs = [];
  String? error;
  String? selected;
  Timer? refresh;

  @override
  void initState() {
    super.initState();
    _load();
    refresh = Timer.periodic(const Duration(seconds: 5), (_) => _load());
  }

  @override
  void dispose() {
    refresh?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final list = await widget.app.jobs(machineId: widget.machine.id);
      if (!mounted) return;
      setState(() {
        jobs = list;
        error = null;
        selected ??= list.isEmpty ? null : list.first.id;
      });
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  Future<void> _run() async {
    final started = await showDialog<Job>(
      context: context,
      builder: (_) => _RunDialog(app: widget.app, machine: widget.machine),
    );
    if (started != null) {
      setState(() => selected = started.id);
      _load();
    }
  }

  @override
  Widget build(BuildContext context) {
    final job = jobs.where((j) => j.id == selected).firstOrNull;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SizedBox(
          width: 340,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.all(12),
                child: Row(
                  children: [
                    FilledButton.icon(
                      onPressed: _run,
                      icon: const Icon(Icons.play_arrow, size: 18),
                      label: const Text('Run'),
                    ),
                    const Spacer(),
                    IconButton(
                      tooltip: 'Refresh',
                      onPressed: _load,
                      icon: const Icon(Icons.refresh),
                    ),
                  ],
                ),
              ),
              if (error != null)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  child: Text(error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
                ),
              Expanded(
                child: jobs.isEmpty
                    ? const Center(child: Text('No jobs yet.'))
                    : ListView(
                        children: [
                          for (final j in jobs)
                            ListTile(
                              dense: true,
                              selected: j.id == selected,
                              leading: _StateIcon(state: j.state),
                              title: Text(
                                j.label ?? j.command,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              subtitle: Text(
                                '${when(j.createdAtMs)} · ${j.startedBy}'
                                '${j.startedAtMs != null ? ' · ${duration(j.startedAtMs, j.finishedAtMs)}' : ''}',
                              ),
                              onTap: () => setState(() => selected = j.id),
                            ),
                        ],
                      ),
              ),
            ],
          ),
        ),
        const VerticalDivider(width: 1),
        Expanded(
          child: job == null
              ? const Center(child: Text('Select a job to see its output.'))
              : _JobLog(
                  key: ValueKey(job.id),
                  app: widget.app,
                  machine: widget.machine,
                  job: job,
                  onChanged: _load,
                ),
        ),
      ],
    );
  }
}

class _StateIcon extends StatelessWidget {
  const _StateIcon({required this.state});

  final String state;

  @override
  Widget build(BuildContext context) {
    return switch (state) {
      'running' => const SizedBox(
          width: 18,
          height: 18,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      'queued' => const Icon(Icons.schedule, size: 18),
      'succeeded' => const Icon(Icons.check_circle, size: 18, color: Colors.green),
      'cancelled' => const Icon(Icons.cancel, size: 18, color: Colors.grey),
      _ => Icon(Icons.error, size: 18, color: Theme.of(context).colorScheme.error),
    };
  }
}

/// A job's output, following it live while it runs.
class _JobLog extends StatefulWidget {
  const _JobLog({
    super.key,
    required this.app,
    required this.machine,
    required this.job,
    required this.onChanged,
  });

  final YonderApp app;
  final Machine machine;
  final Job job;
  final VoidCallback onChanged;

  @override
  State<_JobLog> createState() => _JobLogState();
}

class _JobLogState extends State<_JobLog> {
  /// Keeps the view responsive on huge logs.
  static const maxChars = 400000;
  static final ansi = RegExp(r'\x1B\[[0-9;?]*[ -/]*[@-~]|\r');

  final buffer = StringBuffer();
  final scroll = ScrollController();
  final decoder = const Utf8Decoder(allowMalformed: true);
  StreamSubscription<LogEvent>? subscription;
  Job? finished;
  String? error;
  final notes = <String>[];

  @override
  void initState() {
    super.initState();
    subscription = widget.app
        .followJob(machineId: widget.machine.id, jobId: widget.job.id, from: BigInt.zero)
        .listen(
      (event) {
        switch (event) {
          case LogEvent_Output(:final data):
            buffer.write(decoder.convert(data).replaceAll(ansi, ''));
            if (buffer.length > maxChars) {
              final text = buffer.toString();
              buffer
                ..clear()
                ..write(text.substring(text.length - maxChars));
            }
          case LogEvent_Finished(:final job):
            finished = job;
            widget.onChanged();
          case LogEvent_LentGitLogin(:final host, :final lent):
            notes.add(lent ? 'Lent your git login for $host' : 'No git login here for $host');
        }
        if (!mounted) return;
        setState(() {});
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (scroll.hasClients) scroll.jumpTo(scroll.position.maxScrollExtent);
        });
      },
      onError: (Object e) {
        if (mounted) setState(() => error = errorText(e));
      },
    );
  }

  @override
  void dispose() {
    subscription?.cancel();
    scroll.dispose();
    super.dispose();
  }

  Future<void> _cancel() async {
    try {
      await widget.app.cancelJob(machineId: widget.machine.id, jobId: widget.job.id);
      widget.onChanged();
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  @override
  Widget build(BuildContext context) {
    final job = finished ?? widget.job;
    final text = Theme.of(context).textTheme;
    final running = job.state == 'running' || job.state == 'queued';
    final outcome = switch (job.state) {
      'succeeded' => 'Succeeded',
      'failed' => 'Failed${job.exitCode != null ? ' with exit code ${job.exitCode}' : ''}',
      'cancelled' => 'Cancelled',
      'lost' => 'Lost',
      'queued' => 'Queued',
      _ => 'Running',
    };
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 12, 8),
          child: Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SelectableText(job.command, style: text.titleSmall),
                    Text(
                      [
                        outcome,
                        if (job.startedAtMs != null) duration(job.startedAtMs, job.finishedAtMs),
                        if (job.cwd != null) 'in ${job.cwd}',
                        'job ${job.id}',
                        if (job.message != null) job.message!,
                      ].join(' · '),
                      style: text.bodySmall,
                    ),
                  ],
                ),
              ),
              if (running)
                OutlinedButton(onPressed: _cancel, child: const Text('Cancel')),
            ],
          ),
        ),
        for (final note in notes)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Text(note, style: text.bodySmall),
          ),
        if (error != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Text(error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
          ),
        Expanded(
          child: Container(
            margin: const EdgeInsets.fromLTRB(12, 4, 12, 12),
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              color: Theme.of(context).colorScheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(6),
            ),
            child: SingleChildScrollView(
              controller: scroll,
              child: SelectableText(
                buffer.isEmpty ? (running ? 'Waiting for output…' : '(no output)') : buffer.toString(),
                style: const TextStyle(fontFamily: 'Menlo', fontSize: 12, height: 1.35),
              ),
            ),
          ),
        ),
      ],
    );
  }
}

class _RunDialog extends StatefulWidget {
  const _RunDialog({required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<_RunDialog> createState() => _RunDialogState();
}

class _RunDialogState extends State<_RunDialog> {
  final command = TextEditingController();
  final cwd = TextEditingController();
  final label = TextEditingController();
  bool starting = false;
  String? error;

  Future<void> _start() async {
    if (command.text.trim().isEmpty) return;
    setState(() {
      starting = true;
      error = null;
    });
    try {
      final job = await widget.app.runJob(
        machineId: widget.machine.id,
        command: command.text,
        cwd: cwd.text.trim().isEmpty ? null : cwd.text.trim(),
        label: label.text.trim().isEmpty ? null : label.text.trim(),
      );
      if (mounted) Navigator.pop(context, job);
    } catch (e) {
      if (mounted) {
        setState(() {
          starting = false;
          error = errorText(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text('Run on ${widget.machine.name}'),
      content: SizedBox(
        width: 480,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: command,
              autofocus: true,
              minLines: 1,
              maxLines: 4,
              style: const TextStyle(fontFamily: 'Menlo', fontSize: 13),
              decoration: InputDecoration(
                labelText: widget.machine.os == 'win' ? 'PowerShell command' : 'Shell command',
                hintText: 'flutter build windows',
              ),
              onSubmitted: (_) => _start(),
            ),
            TextField(
              controller: cwd,
              decoration: const InputDecoration(
                labelText: 'Folder on the machine (optional)',
                hintText: 'Default: the build account\'s home',
              ),
            ),
            TextField(
              controller: label,
              decoration: const InputDecoration(labelText: 'Label (optional)'),
            ),
            if (error != null)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
              ),
          ],
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Close')),
        FilledButton(onPressed: starting ? null : _start, child: const Text('Run')),
      ],
    );
  }
}
