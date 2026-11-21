import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

/// Runs a recipe's machine part with administrator rights ("one-click machine prep").
class PrepareDialog extends StatefulWidget {
  const PrepareDialog({super.key, required this.app, required this.machine});

  final YonderApp app;
  final Machine machine;

  @override
  State<PrepareDialog> createState() => _PrepareDialogState();
}

class _PrepareDialogState extends State<PrepareDialog> {
  late final List<RecipeInfo> fitting = recipes().where((r) => r.os == widget.machine.os).toList();
  late String? chosen = fitting.isEmpty ? null : fitting.first.name;
  bool starting = false;
  String? error;

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
    final scheme = Theme.of(context).colorScheme;
    final tones = Tones.of(context);
    final grant = 'yonder-agent allow "${widget.app.deviceName()}" admin';
    return AlertDialog(
      title: Row(children: [
        const IconTile(Icons.build_outlined),
        const SizedBox(width: 12),
        Expanded(child: Text('Prepare ${widget.machine.name}', overflow: TextOverflow.ellipsis)),
      ]),
      content: SizedBox(
        width: 540,
        child: fitting.isEmpty
            ? Text('No recipe for ${widget.machine.os} machines yet.')
            : Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('Installs the tools builds need, for every account. Safe to run again: it skips '
                      "what's there.", style: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant)),
                  const SizedBox(height: 14),
                  for (final r in fitting)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Material(
                        color: chosen == r.name ? scheme.secondaryContainer : scheme.surfaceContainerHighest.withValues(alpha: 0.4),
                        borderRadius: BorderRadius.circular(12),
                        child: InkWell(
                          borderRadius: BorderRadius.circular(12),
                          onTap: () => setState(() => chosen = r.name),
                          child: Padding(
                            padding: const EdgeInsets.all(14),
                            child: Row(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Icon(
                                  chosen == r.name ? Icons.radio_button_checked_rounded : Icons.radio_button_off_rounded,
                                  size: 20,
                                  color: chosen == r.name ? scheme.primary : scheme.outline,
                                ),
                                const SizedBox(width: 12),
                                Expanded(
                                  child: Column(
                                    crossAxisAlignment: CrossAxisAlignment.start,
                                    children: [
                                      Text(r.name, style: monoStyle(size: 13, weight: FontWeight.w600, color: scheme.onSurface)),
                                      const SizedBox(height: 3),
                                      Text(r.about, style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                                    ],
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ),
                  if (widget.machine.os == 'win')
                    Text(
                      'For Windows: Visual Studio C++ tools, Git, fvm, NuGet, Inno Setup, Developer Mode, '
                      'long paths, no sleep on mains power. The Visual Studio part can take 20 minutes or more.',
                      style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                    ),
                  const SizedBox(height: 12),
                  Notice(
                    icon: Icons.admin_panel_settings_outlined,
                    color: tones.warn,
                    text: 'Runs with administrator rights, so this device needs the admin permission. '
                        "If it doesn't have it, grant it on the machine with: $grant",
                    action: IconButton(
                      tooltip: 'Copy command',
                      onPressed: () => copy(context, grant, what: 'Command copied'),
                      icon: const Icon(Icons.copy_rounded, size: 18),
                    ),
                  ),
                  if (error != null) ErrorText(error!),
                ],
              ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        if (fitting.isNotEmpty)
          FilledButton.icon(
            onPressed: starting ? null : _start,
            icon: starting
                ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.build_rounded, size: 18),
            label: const Text('Prepare'),
          ),
      ],
    );
  }
}
