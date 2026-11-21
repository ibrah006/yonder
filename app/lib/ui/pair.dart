import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';
import 'theme.dart';
import 'widgets.dart';

/// Pairs with a machine using the ticket `yonder-agent pair` showed on it.
class PairDialog extends StatefulWidget {
  const PairDialog({super.key, required this.app});

  final YonderApp app;

  @override
  State<PairDialog> createState() => _PairDialogState();
}

class _PairDialogState extends State<PairDialog> {
  final ticket = TextEditingController();
  bool waiting = false;
  String? error;

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim() ?? '';
    if (mounted && text.isNotEmpty) setState(() => ticket.text = text);
  }

  Future<void> _pair() async {
    if (ticket.text.trim().isEmpty || waiting) return;
    setState(() {
      waiting = true;
      error = null;
    });
    try {
      final machine = await widget.app.pair(ticket: ticket.text.trim());
      if (mounted) Navigator.pop(context, machine);
    } catch (e) {
      if (mounted) {
        setState(() {
          waiting = false;
          error = errorText(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    return AlertDialog(
      title: const Row(children: [IconTile(Icons.link_rounded), SizedBox(width: 12), Text('Pair a machine')]),
      content: SizedBox(
        width: 520,
        child: AnimatedSize(
          duration: const Duration(milliseconds: 200),
          alignment: Alignment.topCenter,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(children: [
                Text('On the machine, run ', style: text.bodyMedium),
                const CopyableMono('yonder-agent pair', size: 12.5),
                Text(' and paste the ticket.', style: text.bodyMedium),
              ]),
              const SizedBox(height: 14),
              TextField(
                controller: ticket,
                autofocus: true,
                enabled: !waiting,
                maxLines: 3,
                onChanged: (_) => setState(() {}),
                style: monoStyle(size: 12),
                decoration: InputDecoration(
                  labelText: 'Ticket',
                  alignLabelWithHint: true,
                  suffixIcon: Padding(
                    padding: const EdgeInsets.all(4),
                    child: TextButton.icon(
                      onPressed: waiting ? null : _paste,
                      icon: const Icon(Icons.content_paste_rounded, size: 16),
                      label: const Text('Paste'),
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 16),
              Container(
                width: double.infinity,
                padding: const EdgeInsets.all(14),
                decoration: BoxDecoration(
                  color: scheme.primaryContainer.withValues(alpha: 0.35),
                  borderRadius: BorderRadius.circular(10),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(children: [
                      Icon(Icons.verified_user_outlined, size: 16, color: scheme.primary),
                      const SizedBox(width: 6),
                      Text('The machine asks you to approve this device. Check it shows:', style: text.bodySmall),
                    ]),
                    const SizedBox(height: 8),
                    SelectableText(
                      '${widget.app.deviceName()} · ${widget.app.fingerprint()}',
                      style: monoStyle(size: 14, weight: FontWeight.w600, color: scheme.onSurface),
                    ),
                  ],
                ),
              ),
              if (waiting)
                Padding(
                  padding: const EdgeInsets.only(top: 14),
                  child: FadeIn(
                    child: Row(
                      children: [
                        const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
                        const SizedBox(width: 10),
                        Text('Waiting for approval on the machine…', style: text.bodyMedium),
                      ],
                    ),
                  ),
                ),
              if (error != null) ErrorText(error!),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(
          onPressed: waiting || ticket.text.trim().isEmpty ? null : _pair,
          child: Text(waiting ? 'Waiting…' : 'Pair'),
        ),
      ],
    );
  }
}
