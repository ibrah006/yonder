import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

/// Runs a recipe's machine part with administrator rights ("one-click machine prep").
class PrepareDialog extends StatefulWidget {
  const PrepareDialog({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<PrepareDialog> createState() => _PrepareDialogState();
}

class _PrepareDialogState extends State<PrepareDialog> {
  late final List<RecipeInfo> fitting =
      recipes().where((r) => r.os == widget.machine.os).toList();
  String? chosen;
  bool starting = false;
  String? error;

  @override
  void initState() {
    super.initState();
    chosen = fitting.isEmpty ? null : fitting.first.name;
  }

  Future<void> _start() async {
    final recipe = chosen;
    if (recipe == null) return;
    setState(() {
      starting = true;
      error = null;
    });
    try {
      final job = await widget.app.prepareMachine(machineId: widget.machine.id, recipe: recipe);
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
    final text = Theme.of(context).textTheme;
    return AlertDialog(
      title: Text('Prepare ${widget.machine.name}'),
      content: SizedBox(
        width: 520,
        child: fitting.isEmpty
            ? Text('No recipe for ${widget.machine.os} machines yet.')
            : Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  RadioGroup<String>(
                    groupValue: chosen,
                    onChanged: (v) => setState(() => chosen = v),
                    child: Column(
                      children: [
                        for (final r in fitting)
                          RadioListTile<String>(
                            contentPadding: EdgeInsets.zero,
                            value: r.name,
                            title: Text(r.name),
                            subtitle: Text(r.about),
                          ),
                      ],
                    ),
                  ),
                  const SizedBox(height: 8),
                  Text(
                    'Installs the tools every account needs and sets the machine up for builds '
                    '(for Windows: Visual Studio C++ tools, Git, fvm, NuGet, Inno Setup, Developer Mode, '
                    'long paths, no sleep on mains power). Safe to run again; it skips what\'s there. '
                    'The Visual Studio part can take 20 minutes or more.',
                    style: text.bodySmall,
                  ),
                  const SizedBox(height: 8),
                  Text(
                    'It runs with administrator rights, so this device needs the admin permission. '
                    'If it doesn\'t have it, the machine\'s owner grants it there with:\n'
                    'yonder-agent allow "${widget.app.deviceName()}" admin',
                    style: text.bodySmall,
                  ),
                  if (error != null)
                    Padding(
                      padding: const EdgeInsets.only(top: 12),
                      child: SelectableText(error!,
                          style: TextStyle(color: Theme.of(context).colorScheme.error)),
                    ),
                ],
              ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        if (fitting.isNotEmpty)
          FilledButton(onPressed: starting ? null : _start, child: const Text('Prepare')),
      ],
    );
  }
}
