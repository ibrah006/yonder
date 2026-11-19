import 'package:flutter/material.dart';

import '../src/rust/api/yonder.dart';
import 'format.dart';

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

  Future<void> _pair() async {
    if (ticket.text.trim().isEmpty) return;
    setState(() {
      waiting = true;
      error = null;
    });
    try {
      final machine = await widget.app.pair(ticket: ticket.text);
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
    return AlertDialog(
      title: const Text('Pair a machine'),
      content: SizedBox(
        width: 520,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('On the machine, run `yonder-agent pair` and paste the ticket it shows.'),
            const SizedBox(height: 12),
            TextField(
              controller: ticket,
              autofocus: true,
              maxLines: 3,
              style: const TextStyle(fontFamily: 'Menlo', fontSize: 12),
              decoration: const InputDecoration(labelText: 'Ticket', border: OutlineInputBorder()),
            ),
            const SizedBox(height: 16),
            Text('The machine will ask you to approve this device. Check it shows:', style: text.bodySmall),
            SelectableText(
              '${widget.app.deviceName()} · ${widget.app.fingerprint()}',
              style: text.titleMedium?.copyWith(fontFamily: 'Menlo'),
            ),
            if (waiting)
              const Padding(
                padding: EdgeInsets.only(top: 12),
                child: Row(
                  children: [
                    SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
                    SizedBox(width: 8),
                    Text('Waiting for approval on the machine…'),
                  ],
                ),
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
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(onPressed: waiting ? null : _pair, child: const Text('Pair')),
      ],
    );
  }
}
