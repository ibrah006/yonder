import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

/// Shells on one machine: start a new one or reattach to one that's still running.
class TerminalTab extends StatefulWidget {
  const TerminalTab({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<TerminalTab> createState() => _TerminalTabState();
}

class _TerminalTabState extends State<TerminalTab> {
  List<ShellInfo>? shells;
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
      if (mounted) {
        setState(() {
          shells = list;
          error = null;
        });
      }
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
    final shells = this.shells;
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    final live = shells?.where((s) => !s.exited).toList() ?? [];
    final win = widget.machine.os == 'win';
    if (shells != null && live.isEmpty) {
      return EmptyState(
        icon: Icons.terminal_rounded,
        title: 'Open a shell on ${widget.machine.name}',
        message: '${win ? 'PowerShell' : 'A shell'} as the build account. It keeps running when you '
            'leave it or lose the connection, so you can come back to it.',
        action: Column(
          children: [
            FilledButton.icon(
              onPressed: () => _show(''),
              icon: const Icon(Icons.add_rounded, size: 18),
              label: const Text('New shell'),
            ),
            if (error != null) ErrorText(error!),
          ],
        ),
      );
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
                  Text('Shells', style: text.titleMedium),
                  Text('They keep running when you leave them; reattach here.',
                      style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                ],
              ),
            ),
            IconButton(tooltip: 'Refresh', onPressed: _load, icon: const Icon(Icons.refresh_rounded)),
            const SizedBox(width: 6),
            FilledButton.icon(
              onPressed: () => _show(''),
              icon: const Icon(Icons.add_rounded, size: 18),
              label: const Text('New shell'),
            ),
          ],
        ),
        const SizedBox(height: 14),
        if (error != null) ErrorText(error!),
        if (shells == null) const Center(child: Padding(padding: EdgeInsets.all(24), child: CircularProgressIndicator())),
        for (final (i, s) in live.indexed)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: FadeIn(
              delay: Duration(milliseconds: 30 * i),
              child: Card(
                clipBehavior: Clip.antiAlias,
                child: InkWell(
                  onTap: () => _show(s.id),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
                    child: Row(
                      children: [
                        const IconTile(Icons.terminal_rounded),
                        const SizedBox(width: 12),
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text('Shell ${s.id}', style: monoStyle(size: 13, weight: FontWeight.w600)),
                              const SizedBox(height: 2),
                              Text('started ${when(s.createdAtMs)} by ${s.startedBy}',
                                  style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                            ],
                          ),
                        ),
                        Pill(
                          s.attached > 0 ? '${s.attached} attached' : 'detached',
                          color: s.attached > 0 ? tones.busy : tones.idle,
                          dot: true,
                        ),
                        const SizedBox(width: 12),
                        const Text('Attach'),
                        const Icon(Icons.chevron_right_rounded),
                      ],
                    ),
                  ),
                ),
              ),
            ),
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
    decoder = const Utf8Decoder(allowMalformed: true).startChunkedConversion(_TerminalSink(terminal));
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
    final tones = Tones.of(context);
    final scheme = Theme.of(context).colorScheme;
    final (label, color, pulse) = exited
        ? ('Exited', tones.idle, false)
        : problem != null
            ? ('Disconnected', tones.bad, false)
            : shell != null
                ? ('Connected', tones.ok, false)
                : ('Connecting…', tones.busy, true);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 16, 8),
          child: Row(
            children: [
              TextButton.icon(
                onPressed: widget.onClose,
                icon: const Icon(Icons.arrow_back_rounded, size: 18),
                label: Text(exited ? 'Back' : 'Leave running'),
              ),
              const SizedBox(width: 8),
              Pill(label, color: color, dot: true, pulse: pulse),
              const SizedBox(width: 10),
              if (shell != null) Text('Shell ${shell.id()}', style: monoStyle(size: 12, color: scheme.onSurfaceVariant)),
              const Spacer(),
              if (problem != null) ...[
                Flexible(
                  child: Text(problem!, overflow: TextOverflow.ellipsis, style: TextStyle(color: scheme.error)),
                ),
                if (started && !exited) ...[
                  const SizedBox(width: 8),
                  FilledButton.tonalIcon(
                    onPressed: _reconnect,
                    icon: const Icon(Icons.refresh_rounded, size: 18),
                    label: const Text('Reconnect'),
                  ),
                ],
              ],
            ],
          ),
        ),
        Expanded(
          child: Container(
            margin: const EdgeInsets.fromLTRB(16, 0, 16, 16),
            padding: const EdgeInsets.fromLTRB(12, 10, 4, 6),
            decoration: BoxDecoration(
              color: terminalBackground,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: scheme.outlineVariant.withValues(alpha: 0.6)),
            ),
            clipBehavior: Clip.antiAlias,
            child: TerminalView(
              terminal,
              autofocus: true,
              theme: _terminalTheme,
              textStyle: const TerminalStyle(fontFamily: mono, fontFamilyFallback: monoFallback, fontSize: 13),
            ),
          ),
        ),
      ],
    );
  }
}

const _terminalTheme = TerminalTheme(
  cursor: Color(0xFF7EE2A8),
  selection: Color(0x5574AEFF),
  foreground: Color(0xFFD5DDD9),
  background: terminalBackground,
  black: Color(0xFF3B4441),
  red: Color(0xFFFF7B72),
  green: Color(0xFF5BD18B),
  yellow: Color(0xFFF2CC60),
  blue: Color(0xFF74AEFF),
  magenta: Color(0xFFD2A8FF),
  cyan: Color(0xFF56D4DD),
  white: Color(0xFFD5DDD9),
  brightBlack: Color(0xFF6E7B76),
  brightRed: Color(0xFFFFA198),
  brightGreen: Color(0xFF7EE2A8),
  brightYellow: Color(0xFFF8E3A1),
  brightBlue: Color(0xFFA5CBFF),
  brightMagenta: Color(0xFFE2C5FF),
  brightCyan: Color(0xFFB3F0FF),
  brightWhite: Color(0xFFFFFFFF),
  searchHitBackground: Color(0xFFF2CC60),
  searchHitBackgroundCurrent: Color(0xFF5BD18B),
  searchHitForeground: Color(0xFF0E1412),
);

/// Feeds decoded text into the terminal.
class _TerminalSink implements Sink<String> {
  _TerminalSink(this.terminal);

  final Terminal terminal;

  @override
  void add(String data) => terminal.write(data);

  @override
  void close() {}
}
