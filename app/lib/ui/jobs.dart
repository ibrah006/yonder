import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../src/rust/api/yonder.dart';
import 'ansi.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

enum _Filter { all, active, failed }

/// Jobs on one machine: the list, a run dialog, and the selected job's live log.
class JobsTab extends StatefulWidget {
  const JobsTab({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<JobsTab> createState() => _JobsTabState();
}

class _JobsTabState extends State<JobsTab> {
  List<Job>? jobs;
  String? error;
  String? selected;
  Timer? refresh;
  _Filter filter = _Filter.all;
  final search = TextEditingController();

  @override
  void initState() {
    super.initState();
    _load();
    refresh = Timer.periodic(const Duration(seconds: 5), (_) => _load());
  }

  @override
  void dispose() {
    refresh?.cancel();
    search.dispose();
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

  Future<void> _run({Job? like}) async {
    final started = await showDialog<Job>(
      context: context,
      builder: (_) => _RunDialog(app: widget.app, machine: widget.machine, recent: jobs ?? [], like: like),
    );
    if (started != null) _started(started);
  }

  void _started(Job job) {
    setState(() => selected = job.id);
    _load();
  }

  Future<void> _again(Job job) async {
    try {
      final started = await widget.app.runJob(
        machineId: widget.machine.id,
        command: job.command,
        cwd: job.cwd,
        label: job.label,
      );
      _started(started);
      if (mounted) toast(context, 'Started again', icon: Icons.replay_rounded);
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
  }

  Future<void> _cancel(Job job) async {
    try {
      await widget.app.cancelJob(machineId: widget.machine.id, jobId: job.id);
      _load();
    } catch (e) {
      if (mounted) toast(context, errorText(e), error: true);
    }
  }

  List<Job> get _shown {
    final q = search.text.trim().toLowerCase();
    return (jobs ?? []).where((j) {
      final ok = switch (filter) {
        _Filter.all => true,
        _Filter.active => j.state == 'running' || j.state == 'queued',
        _Filter.failed => j.state == 'failed' || j.state == 'lost',
      };
      return ok && (q.isEmpty || '${j.label ?? ''} ${j.command} ${j.startedBy}'.toLowerCase().contains(q));
    }).toList();
  }

  @override
  Widget build(BuildContext context) {
    final jobs = this.jobs;
    final job = jobs?.where((j) => j.id == selected).firstOrNull;
    final scheme = Theme.of(context).colorScheme;
    final meta = Theme.of(context).platform == TargetPlatform.macOS;
    if (jobs == null) {
      return error != null
          ? EmptyState(
              icon: Icons.cloud_off_rounded,
              title: "Couldn't load jobs",
              message: error,
              action: OutlinedButton(onPressed: _load, child: const Text('Try again')),
            )
          : const Center(child: CircularProgressIndicator());
    }
    if (jobs.isEmpty) {
      return EmptyState(
        icon: Icons.rocket_launch_outlined,
        title: 'No jobs on ${widget.machine.name} yet',
        message: 'Run a command here, or from a terminal on this Mac with '
            'yonder run ${widget.machine.name} -- <command>. Jobs keep running if you close the app.',
        action: FilledButton.icon(
          onPressed: _run,
          icon: const Icon(Icons.play_arrow_rounded, size: 18),
          label: const Text('Run a command'),
        ),
      );
    }
    final shown = _shown;
    final active = jobs.where((j) => j.state == 'running' || j.state == 'queued').length;
    final failed = jobs.where((j) => j.state == 'failed' || j.state == 'lost').length;
    return CallbackShortcuts(
      bindings: {SingleActivator(LogicalKeyboardKey.keyJ, meta: meta, control: !meta): _run},
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(
            width: 360,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 14, 12, 8),
                  child: Row(
                    children: [
                      Expanded(
                        child: SizedBox(
                          height: 36,
                          child: TextField(
                            controller: search,
                            onChanged: (_) => setState(() {}),
                            decoration: InputDecoration(
                              hintText: 'Search jobs',
                              prefixIcon: const Icon(Icons.search_rounded, size: 18),
                              contentPadding: EdgeInsets.zero,
                              suffixIcon: search.text.isEmpty
                                  ? null
                                  : IconButton(
                                      iconSize: 16,
                                      onPressed: () => setState(search.clear),
                                      icon: const Icon(Icons.close_rounded),
                                    ),
                            ),
                          ),
                        ),
                      ),
                      const SizedBox(width: 8),
                      Tooltip(
                        message: 'Run a command (${meta ? '⌘' : 'Ctrl+'}J)',
                        child: FilledButton.icon(
                          onPressed: _run,
                          icon: const Icon(Icons.play_arrow_rounded, size: 18),
                          label: const Text('Run'),
                        ),
                      ),
                    ],
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 12, 6),
                  child: Wrap(
                    spacing: 6,
                    children: [
                      for (final (f, label) in [
                        (_Filter.all, 'All ${jobs.length}'),
                        (_Filter.active, 'Active $active'),
                        (_Filter.failed, 'Failed $failed'),
                      ])
                        ChoiceChip(
                          label: Text(label),
                          selected: filter == f,
                          showCheckmark: false,
                          visualDensity: VisualDensity.compact,
                          onSelected: (_) => setState(() => filter = f),
                        ),
                    ],
                  ),
                ),
                if (error != null)
                  Padding(padding: const EdgeInsets.symmetric(horizontal: 16), child: ErrorText(error!)),
                Expanded(
                  child: shown.isEmpty
                      ? Center(child: Text('No jobs match.', style: TextStyle(color: scheme.onSurfaceVariant)))
                      : ListView.builder(
                          padding: const EdgeInsets.fromLTRB(8, 2, 8, 12),
                          itemCount: shown.length,
                          itemBuilder: (context, i) {
                            final j = shown[i];
                            return _JobTile(
                              job: j,
                              selected: j.id == selected,
                              onTap: () => setState(() => selected = j.id),
                              onAgain: () => _again(j),
                              onEdit: () => _run(like: j),
                              onCancel: () => _cancel(j),
                            );
                          },
                        ),
                ),
              ],
            ),
          ),
          const VerticalDivider(width: 1),
          Expanded(
            child: job == null
                ? const EmptyState(icon: Icons.notes_rounded, title: 'Select a job to see its output')
                : _JobLog(
                    key: ValueKey(job.id),
                    app: widget.app,
                    machine: widget.machine,
                    job: job,
                    onChanged: _load,
                    onAgain: () => _again(job),
                    onEdit: () => _run(like: job),
                  ),
          ),
        ],
      ),
    );
  }
}

String _outcome(Job job) => switch (job.state) {
      'succeeded' => 'Succeeded',
      'failed' => job.exitCode != null ? 'Failed · exit ${job.exitCode}' : 'Failed',
      'cancelled' => 'Cancelled',
      'lost' => 'Lost',
      'queued' => 'Queued',
      _ => 'Running',
    };

class _JobTile extends StatelessWidget {
  const _JobTile({
    required this.job,
    required this.selected,
    required this.onTap,
    required this.onAgain,
    required this.onEdit,
    required this.onCancel,
  });

  final Job job;
  final bool selected;
  final VoidCallback onTap;
  final VoidCallback onAgain;
  final VoidCallback onEdit;
  final VoidCallback onCancel;

  Future<void> _menu(BuildContext context, Offset at) async {
    final running = job.state == 'running' || job.state == 'queued';
    final overlay = Overlay.of(context).context.findRenderObject() as RenderBox;
    PopupMenuItem<String> item(String value, IconData icon, String label) => PopupMenuItem(
          value: value,
          height: 36,
          child: Row(children: [Icon(icon, size: 18), const SizedBox(width: 10), Text(label)]),
        );
    final choice = await showMenu<String>(
      context: context,
      position: RelativeRect.fromRect(at & const Size(1, 1), Offset.zero & overlay.size),
      items: [
        item('again', Icons.replay_rounded, 'Run again'),
        item('edit', Icons.edit_outlined, 'Edit and run…'),
        item('copy', Icons.copy_rounded, 'Copy command'),
        if (running) item('cancel', Icons.stop_circle_outlined, 'Cancel'),
      ],
    );
    if (!context.mounted) return;
    switch (choice) {
      case 'again':
        onAgain();
      case 'edit':
        onEdit();
      case 'copy':
        copy(context, job.command, what: 'Command copied');
      case 'cancel':
        onCancel();
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: GestureDetector(
        onSecondaryTapDown: (d) => _menu(context, d.globalPosition),
        child: Material(
          color: selected ? scheme.secondaryContainer : Colors.transparent,
          borderRadius: BorderRadius.circular(10),
          child: InkWell(
            borderRadius: BorderRadius.circular(10),
            onTap: onTap,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(10, 9, 10, 9),
              child: Row(
                children: [
                  _StateIcon(state: job.state),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          job.label ?? job.command,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: job.label != null
                              ? text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)
                              : monoStyle(size: 12.5, weight: FontWeight.w500, color: scheme.onSurface),
                        ),
                        const SizedBox(height: 2),
                        Text(
                          [
                            when(job.createdAtMs),
                            job.startedBy,
                            if (job.startedAtMs != null) duration(job.startedAtMs, job.finishedAtMs),
                            if (job.state == 'failed' && job.exitCode != null) 'exit ${job.exitCode}',
                          ].join(' · '),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: text.bodySmall?.copyWith(
                            color: job.state == 'failed' ? scheme.error : scheme.onSurfaceVariant,
                          ),
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
    );
  }
}

class _StateIcon extends StatelessWidget {
  const _StateIcon({required this.state});

  final String state;

  @override
  Widget build(BuildContext context) {
    final color = Tones.of(context).job(state);
    return Container(
      width: 28,
      height: 28,
      decoration: BoxDecoration(color: color.withValues(alpha: 0.13), shape: BoxShape.circle),
      child: Center(
        child: switch (state) {
          'running' => SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2, color: color)),
          'queued' => Icon(Icons.hourglass_empty_rounded, size: 15, color: color),
          'succeeded' => Icon(Icons.check_rounded, size: 17, color: color),
          'cancelled' => Icon(Icons.block_rounded, size: 15, color: color),
          _ => Icon(Icons.close_rounded, size: 17, color: color),
        },
      ),
    );
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
    required this.onAgain,
    required this.onEdit,
  });

  final YonderApp app;
  final Machine machine;
  final Job job;
  final VoidCallback onChanged;
  final VoidCallback onAgain;
  final VoidCallback onEdit;

  @override
  State<_JobLog> createState() => _JobLogState();
}

class _JobLogState extends State<_JobLog> {
  final log = AnsiLog();
  final scroll = ScrollController();
  final decoder = const Utf8Decoder(allowMalformed: true);
  StreamSubscription<LogEvent>? subscription;
  Job? finished;
  String? error;
  final notes = <({String text, bool ok})>[];

  /// Scrolls along with new output while the view is at the bottom.
  bool follow = true;
  Timer? ticker;

  @override
  void initState() {
    super.initState();
    // Keeps the running time current.
    ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && (finished ?? widget.job).state == 'running') setState(() {});
    });
    scroll.addListener(() {
      if (!scroll.hasClients) return;
      final atEnd = scroll.position.pixels >= scroll.position.maxScrollExtent - 40;
      if (atEnd != follow) setState(() => follow = atEnd);
    });
    subscription = widget.app
        .followJob(machineId: widget.machine.id, jobId: widget.job.id, from: BigInt.zero)
        .listen(
      (event) {
        switch (event) {
          case LogEvent_Output(:final data):
            log.add(decoder.convert(data));
          case LogEvent_Finished(:final job):
            finished = job;
            widget.onChanged();
          case LogEvent_LentGitLogin(:final host, :final lent):
            notes.add((text: lent ? 'Lent your git login for $host' : 'No git login here for $host', ok: lent));
        }
        if (!mounted) return;
        setState(() {});
        if (follow) WidgetsBinding.instance.addPostFrameCallback((_) => _toEnd());
      },
      onError: (Object e) {
        if (mounted) setState(() => error = errorText(e));
      },
    );
  }

  void _toEnd({bool animate = false}) {
    if (!scroll.hasClients) return;
    final end = scroll.position.maxScrollExtent;
    if (animate) {
      scroll.animateTo(end, duration: const Duration(milliseconds: 300), curve: Curves.easeOutCubic);
    } else {
      scroll.jumpTo(end);
    }
  }

  @override
  void dispose() {
    ticker?.cancel();
    subscription?.cancel();
    scroll.dispose();
    super.dispose();
  }

  Future<void> _cancel() async {
    if (!await confirm(
      context,
      title: 'Cancel this job?',
      message: 'It stops on ${widget.machine.name} straight away.',
      action: 'Cancel job',
    )) {
      return;
    }
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
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    final running = job.state == 'running' || job.state == 'queued';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(20, 14, 16, 10),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (job.label != null) Text(job.label!, style: text.titleMedium),
                    SelectableText(
                      job.command,
                      minLines: 1,
                      maxLines: 3,
                      style: monoStyle(
                        size: job.label != null ? 12 : 13.5,
                        weight: job.label != null ? null : FontWeight.w600,
                        color: job.label != null ? scheme.onSurfaceVariant : scheme.onSurface,
                      ),
                    ),
                    const SizedBox(height: 8),
                    Wrap(
                      spacing: 12,
                      runSpacing: 6,
                      crossAxisAlignment: WrapCrossAlignment.center,
                      children: [
                        Pill(_outcome(job), color: tones.job(job.state), dot: true, pulse: job.state == 'running'),
                        if (job.startedAtMs != null)
                          _Fact(Icons.timer_outlined, duration(job.startedAtMs, job.finishedAtMs)),
                        _Fact(Icons.person_outline_rounded, job.startedBy),
                        if (job.cwd != null) _Fact(Icons.folder_outlined, job.cwd!),
                        CopyableMono(job.id, size: 11, what: 'Job id copied'),
                      ],
                    ),
                    if (job.message != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 6),
                        child: Text(job.message!, style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                      ),
                  ],
                ),
              ),
              const SizedBox(width: 12),
              IconButton(
                tooltip: 'Copy output',
                onPressed: log.isEmpty ? null : () => copy(context, log.plain, what: 'Output copied'),
                icon: const Icon(Icons.copy_all_rounded),
              ),
              IconButton(tooltip: 'Edit and run…', onPressed: widget.onEdit, icon: const Icon(Icons.edit_outlined)),
              const SizedBox(width: 4),
              if (running)
                OutlinedButton.icon(
                  style: OutlinedButton.styleFrom(foregroundColor: scheme.error),
                  onPressed: _cancel,
                  icon: const Icon(Icons.stop_rounded, size: 18),
                  label: const Text('Cancel'),
                )
              else
                FilledButton.tonalIcon(
                  onPressed: widget.onAgain,
                  icon: const Icon(Icons.replay_rounded, size: 18),
                  label: const Text('Run again'),
                ),
            ],
          ),
        ),
        for (final note in notes)
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 16, 6),
            child: Row(children: [
              Icon(note.ok ? Icons.key_rounded : Icons.key_off_rounded, size: 15, color: note.ok ? tones.ok : tones.warn),
              const SizedBox(width: 6),
              Text(note.text, style: text.bodySmall),
            ]),
          ),
        if (error != null) Padding(padding: const EdgeInsets.symmetric(horizontal: 20), child: ErrorText(error!)),
        Expanded(
          child: Container(
            margin: const EdgeInsets.fromLTRB(16, 0, 16, 16),
            decoration: BoxDecoration(
              color: scheme.surfaceContainerLowest,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: scheme.outlineVariant.withValues(alpha: 0.6)),
            ),
            clipBehavior: Clip.antiAlias,
            child: Stack(
              children: [
                Positioned.fill(
                  child: Scrollbar(
                    controller: scroll,
                    child: SingleChildScrollView(
                      controller: scroll,
                      padding: const EdgeInsets.all(14),
                      child: SizedBox(
                        width: double.infinity,
                        child: log.isEmpty
                            ? Text(running ? 'Waiting for output…' : 'No output.',
                                style: monoStyle(color: scheme.onSurfaceVariant))
                            : SelectableText.rich(
                                log.span(monoStyle(size: 12, height: 1.45, color: scheme.onSurface),
                                    Theme.of(context).brightness),
                              ),
                      ),
                    ),
                  ),
                ),
                if (!follow)
                  Positioned(
                    right: 14,
                    bottom: 14,
                    child: FadeIn(
                      child: FloatingActionButton.small(
                        heroTag: null,
                        tooltip: running ? 'Follow new output' : 'Jump to the end',
                        onPressed: () {
                          setState(() => follow = true);
                          _toEnd(animate: true);
                        },
                        child: const Icon(Icons.arrow_downward_rounded),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact(this.icon, this.text);

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 14, color: scheme.onSurfaceVariant),
        const SizedBox(width: 4),
        ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 320),
          child: Text(
            text,
            overflow: TextOverflow.ellipsis,
            style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ),
      ],
    );
  }
}

class _RunDialog extends StatefulWidget {
  const _RunDialog({required this.app, required this.machine, required this.recent, this.like});

  final YonderApp app;
  final Machine machine;

  /// Jobs to offer recent commands from.
  final List<Job> recent;

  /// A job to start from (edit and run).
  final Job? like;

  @override
  State<_RunDialog> createState() => _RunDialogState();
}

class _RunDialogState extends State<_RunDialog> {
  late final command = TextEditingController(text: widget.like?.command);
  late final cwd = TextEditingController(text: widget.like?.cwd);
  late final label = TextEditingController(text: widget.like?.label);
  late bool more = widget.like?.cwd != null || widget.like?.label != null;
  bool starting = false;
  String? error;

  Future<void> _start() async {
    if (command.text.trim().isEmpty || starting) return;
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
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final meta = Theme.of(context).platform == TargetPlatform.macOS;
    final recent = <String>{for (final j in widget.recent) j.command}.where((c) => c.length < 80).take(6).toList();
    final win = widget.machine.os == 'win';
    return CallbackShortcuts(
      bindings: {SingleActivator(LogicalKeyboardKey.enter, meta: meta, control: !meta): _start},
      child: AlertDialog(
        title: Row(children: [
          const IconTile(Icons.play_arrow_rounded),
          const SizedBox(width: 12),
          Expanded(child: Text('Run on ${widget.machine.name}', overflow: TextOverflow.ellipsis)),
        ]),
        content: SizedBox(
          width: 520,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              TextField(
                controller: command,
                autofocus: true,
                minLines: 2,
                maxLines: 6,
                onChanged: (_) => setState(() {}),
                style: monoStyle(size: 13),
                decoration: InputDecoration(
                  labelText: win ? 'PowerShell command' : 'Shell command',
                  hintText: win ? 'flutter build windows' : 'make release',
                  alignLabelWithHint: true,
                ),
              ),
              if (recent.isNotEmpty) ...[
                const SizedBox(height: 12),
                Text('RECENT', style: text.labelSmall?.copyWith(color: scheme.onSurfaceVariant)),
                const SizedBox(height: 6),
                Wrap(
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    for (final c in recent)
                      ActionChip(
                        label: Text(c, style: monoStyle(size: 11.5), overflow: TextOverflow.ellipsis),
                        onPressed: () => setState(() => command.text = c),
                      ),
                  ],
                ),
              ],
              const SizedBox(height: 8),
              TextButton.icon(
                onPressed: () => setState(() => more = !more),
                icon: AnimatedRotation(
                  turns: more ? 0.25 : 0,
                  duration: const Duration(milliseconds: 180),
                  child: const Icon(Icons.chevron_right_rounded, size: 18),
                ),
                label: const Text('Folder and label'),
              ),
              AnimatedSize(
                duration: const Duration(milliseconds: 200),
                curve: Curves.easeOutCubic,
                alignment: Alignment.topCenter,
                child: !more
                    ? const SizedBox(width: double.infinity)
                    : Column(
                        children: [
                          const SizedBox(height: 4),
                          TextField(
                            controller: cwd,
                            style: monoStyle(size: 12.5),
                            decoration: const InputDecoration(
                              labelText: 'Folder on the machine',
                              hintText: "Default: the build account's home",
                              prefixIcon: Icon(Icons.folder_outlined, size: 18),
                            ),
                          ),
                          const SizedBox(height: 10),
                          TextField(
                            controller: label,
                            decoration: const InputDecoration(
                              labelText: 'Label',
                              hintText: 'Shown in the list instead of the command',
                              prefixIcon: Icon(Icons.label_outline_rounded, size: 18),
                            ),
                          ),
                        ],
                      ),
              ),
              if (error != null) ErrorText(error!),
            ],
          ),
        ),
        actions: [
          Text('${meta ? '⌘' : 'Ctrl+'}Enter to run', style: text.bodySmall?.copyWith(color: scheme.outline)),
          const SizedBox(width: 8),
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
          FilledButton.icon(
            onPressed: starting || command.text.trim().isEmpty ? null : _start,
            icon: starting
                ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.play_arrow_rounded, size: 18),
            label: const Text('Run'),
          ),
        ],
      ),
    );
  }
}
