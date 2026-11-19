import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

/// Shells on one machine: start a new one or reattach to one that's still running.
class TerminalTab extends StatefulWidget {
  const TerminalTab({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<TerminalTab> createState() => _TerminalTabState();
}

class _TerminalTabState extends State<TerminalTab> {
  List<ShellInfo> shells = [];
  String? error;

  /// The shell shown: `''` for a new one, an id to attach, `null` for none.
  String? open;
  int generation = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final list = await widget.app.shells(machineId: widget.machine.id);
      if (mounted) setState(() => shells = list);
    } catch (e) {
      if (mounted) setState(() => error = errorText(e));
    }
  }

  void _show(String? session) => setState(() {
        open = session;
        generation++;
      });

  @override
  Widget build(BuildContext context) {
    final open = this.open;
    if (open != null) {
      return _TerminalView(
        key: ValueKey('$open-$generation'),
        app: widget.app,
        machine: widget.machine,
        session: open.isEmpty ? null : open,
        onClose: () {
          _show(null);
          _load();
        },
      );
    }
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Row(
          children: [
            FilledButton.icon(
              onPressed: () => _show(''),
              icon: const Icon(Icons.terminal, size: 18),
              label: const Text('New shell'),
            ),
            const SizedBox(width: 8),
            IconButton(tooltip: 'Refresh', onPressed: _load, icon: const Icon(Icons.refresh)),
          ],
        ),
        const SizedBox(height: 8),
        Text(
          'Shells keep running when you leave them or lose the connection; reattach below.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        if (error != null)
          Text(error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
        for (final s in shells)
          ListTile(
            leading: Icon(s.exited ? Icons.stop_circle_outlined : Icons.terminal),
            title: Text('Shell ${s.id}'),
            subtitle: Text(
              'started ${when(s.createdAtMs)} by ${s.startedBy}'
              '${s.exited ? ' · exited' : s.attached > 0 ? ' · ${s.attached} attached' : ' · detached'}',
            ),
            trailing: s.exited ? null : const Text('Attach'),
            onTap: s.exited ? null : () => _show(s.id),
          ),
      ],
    );
  }
}

class _TerminalView extends StatefulWidget {
  const _TerminalView({
    super.key,
    required this.app,
    required this.machine,
    required this.session,
    required this.onClose,
  });

  final YonderApp app;
  final Machine machine;
  final String? session;
  final VoidCallback onClose;

  @override
  State<_TerminalView> createState() => _TerminalViewState();
}

class _TerminalViewState extends State<_TerminalView> {
  final terminal = Terminal(maxLines: 10000);
  Shell? shell;
  StreamSubscription<ShellEvent>? events;
  late ByteConversionSink decoder;
  BigInt offset = BigInt.zero;
  String? problem;
  bool exited = false;
  bool started = false;

  @override
  void initState() {
    super.initState();
    decoder = const Utf8Decoder(allowMalformed: true)
        .startChunkedConversion(_TerminalSink(terminal));
    terminal.onOutput = (data) => shell?.send(data: utf8.encode(data));
    terminal.onResize = (cols, rows, _, _) {
      final shell = this.shell;
      if (shell != null) {
        shell.resize(cols: cols, rows: rows);
      } else if (!started && cols > 0 && rows > 0) {
        _connect(widget.session, cols, rows, null);
      }
    };
  }

  /// Opens (or reopens after a drop) the shell, replaying from `from`.
  Future<void> _connect(String? session, int cols, int rows, BigInt? from) async {
    started = true;
    try {
      final opened = await widget.app.openShell(
        machineId: widget.machine.id,
        session: session,
        cols: cols,
        rows: rows,
        from: from,
      );
      if (!mounted) {
        opened.detach();
        return;
      }
      setState(() {
        shell = opened;
        problem = null;
      });
      events = opened.events().listen((event) {
        switch (event) {
          case ShellEvent_Output(offset: final at, :final data):
            // Skip anything already shown (a replay after reconnecting).
            final skip = (offset - at).toInt();
            if (skip < data.length) {
              decoder.add(skip > 0 ? data.sublist(skip) : data);
              offset = at + BigInt.from(data.length);
            }
          case ShellEvent_Exited(:final code):
            setState(() => exited = true);
            terminal.write('\r\n[shell exited${code != null ? ' with code $code' : ''}]\r\n');
          case ShellEvent_Disconnected(:final message):
            setState(() {
              problem = 'Connection lost ($message). The shell keeps running.';
              shell = null;
            });
        }
      });
    } catch (e) {
      if (mounted) setState(() => problem = errorText(e));
    }
  }

  void _reconnect() {
    final id = shell?.id() ?? widget.session;
    events?.cancel();
    setState(() => problem = null);
    _connect(id, terminal.viewWidth, terminal.viewHeight, offset);
  }

  @override
  void dispose() {
    events?.cancel();
    shell?.detach();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final shell = this.shell;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
          child: Row(
            children: [
              TextButton.icon(
                onPressed: widget.onClose,
                icon: const Icon(Icons.arrow_back, size: 18),
                label: Text(exited ? 'Back' : 'Leave running'),
              ),
              const SizedBox(width: 8),
              Text(shell != null ? 'Shell ${shell.id()}' : 'Connecting…',
                  style: Theme.of(context).textTheme.bodySmall),
              const Spacer(),
              if (problem != null) ...[
                Flexible(
                  child: Text(problem!,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(color: Theme.of(context).colorScheme.error)),
                ),
                if (started && !exited)
                  TextButton(onPressed: _reconnect, child: const Text('Reconnect')),
              ],
            ],
          ),
        ),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 12),
            child: TerminalView(
              terminal,
              autofocus: true,
              textStyle: const TerminalStyle(fontFamily: 'Menlo', fontSize: 13),
            ),
          ),
        ),
      ],
    );
  }
}

/// Feeds decoded text into the terminal.
class _TerminalSink implements Sink<String> {
  _TerminalSink(this.terminal);

  final Terminal terminal;

  @override
  void add(String data) => terminal.write(data);

  @override
  void close() {}
}
