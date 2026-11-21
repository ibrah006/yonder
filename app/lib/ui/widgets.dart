import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'theme.dart';

/// A small coloured dot; it pulses while `pulse` is on (something is running).
class StatusDot extends StatefulWidget {
  const StatusDot({super.key, required this.color, this.pulse = false, this.size = 8});

  final Color color;
  final bool pulse;
  final double size;

  @override
  State<StatusDot> createState() => _StatusDotState();
}

class _StatusDotState extends State<StatusDot> with SingleTickerProviderStateMixin {
  late final controller = AnimationController(vsync: this, duration: const Duration(milliseconds: 1400));

  @override
  void initState() {
    super.initState();
    if (widget.pulse) controller.repeat();
  }

  @override
  void didUpdateWidget(StatusDot old) {
    super.didUpdateWidget(old);
    if (widget.pulse && !controller.isAnimating) controller.repeat();
    if (!widget.pulse && controller.isAnimating) controller.reset();
  }

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.size;
    return SizedBox(
      width: s * 2,
      height: s * 2,
      child: AnimatedBuilder(
        animation: controller,
        builder: (context, _) {
          final t = controller.value;
          return Stack(
            alignment: Alignment.center,
            children: [
              if (widget.pulse)
                Container(
                  width: s + s * t,
                  height: s + s * t,
                  decoration: BoxDecoration(
                    shape: BoxShape.circle,
                    color: widget.color.withValues(alpha: 0.45 * (1 - t)),
                  ),
                ),
              AnimatedContainer(
                duration: const Duration(milliseconds: 250),
                width: s,
                height: s,
                decoration: BoxDecoration(color: widget.color, shape: BoxShape.circle),
              ),
            ],
          );
        },
      ),
    );
  }
}

/// A rounded label in a tint of `color`, e.g. "Running" or "admin".
class Pill extends StatelessWidget {
  const Pill(this.text, {super.key, required this.color, this.icon, this.dot = false, this.pulse = false});

  final String text;
  final Color color;
  final IconData? icon;
  final bool dot;
  final bool pulse;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: EdgeInsets.fromLTRB(dot ? 4 : 8, 3, 9, 3),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(20),
        border: Border.all(color: color.withValues(alpha: 0.25)),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (dot) StatusDot(color: color, pulse: pulse, size: 7),
          if (icon != null) ...[Icon(icon, size: 13, color: color), const SizedBox(width: 4)],
          Text(text, style: TextStyle(color: color, fontSize: 12, fontWeight: FontWeight.w600)),
        ],
      ),
    );
  }
}

/// A big friendly message for an empty or not-yet-loaded place.
class EmptyState extends StatelessWidget {
  const EmptyState({super.key, required this.icon, required this.title, this.message, this.action});

  final IconData icon;
  final String title;
  final String? message;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(32),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 380),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 56,
                height: 56,
                decoration: BoxDecoration(
                  color: scheme.primaryContainer.withValues(alpha: 0.5),
                  borderRadius: BorderRadius.circular(16),
                ),
                child: Icon(icon, color: scheme.primary, size: 28),
              ),
              const SizedBox(height: 16),
              Text(title, style: text.titleMedium, textAlign: TextAlign.center),
              if (message != null) ...[
                const SizedBox(height: 6),
                Text(
                  message!,
                  style: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
                  textAlign: TextAlign.center,
                ),
              ],
              if (action != null) ...[const SizedBox(height: 18), action!],
            ],
          ),
        ),
      ),
    );
  }
}

/// A coloured strip with a message and an optional action, for things to know.
class Notice extends StatelessWidget {
  const Notice({super.key, required this.icon, required this.color, required this.text, this.action});

  final IconData icon;
  final Color color;
  final String text;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.fromLTRB(12, 8, 8, 8),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: color.withValues(alpha: 0.28)),
      ),
      child: Row(
        children: [
          Icon(icon, size: 18, color: color),
          const SizedBox(width: 10),
          Expanded(child: SelectableText(text, style: Theme.of(context).textTheme.bodyMedium)),
          if (action != null) ...[const SizedBox(width: 8), action!],
        ],
      ),
    );
  }
}

/// A titled card that groups related rows.
class Section extends StatelessWidget {
  const Section({super.key, required this.title, this.subtitle, this.trailing, required this.children});

  final String title;
  final String? subtitle;
  final Widget? trailing;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 14, 12, 10),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(title, style: text.titleSmall),
                      if (subtitle != null)
                        Text(subtitle!, style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                    ],
                  ),
                ),
                ?trailing,
              ],
            ),
            const SizedBox(height: 6),
            ...children,
          ],
        ),
      ),
    );
  }
}

/// An icon in a soft rounded square, as a list row's leading.
class IconTile extends StatelessWidget {
  const IconTile(this.icon, {super.key, this.color, this.size = 34});

  final IconData icon;
  final Color? color;
  final double size;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final c = color ?? scheme.primary;
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        color: c.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(size * 0.28),
      ),
      child: Icon(icon, size: size * 0.53, color: c),
    );
  }
}

/// Asks before doing something; `destructive` paints the button red.
Future<bool> confirm(
  BuildContext context, {
  required String title,
  required String message,
  required String action,
  bool destructive = true,
}) async {
  final scheme = Theme.of(context).colorScheme;
  return await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          icon: destructive ? Icon(Icons.warning_amber_rounded, color: scheme.error) : null,
          title: Text(title),
          content: ConstrainedBox(constraints: const BoxConstraints(maxWidth: 420), child: Text(message)),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
            FilledButton(
              autofocus: true,
              style: destructive
                  ? FilledButton.styleFrom(backgroundColor: scheme.error, foregroundColor: scheme.onError)
                  : null,
              onPressed: () => Navigator.pop(context, true),
              child: Text(action),
            ),
          ],
        ),
      ) ??
      false;
}

/// Copies `text` and says so.
void copy(BuildContext context, String text, {String what = 'Copied'}) {
  Clipboard.setData(ClipboardData(text: text));
  toast(context, what, icon: Icons.check);
}

/// A short floating message at the bottom.
void toast(BuildContext context, String message, {IconData? icon, bool error = false}) {
  final scheme = Theme.of(context).colorScheme;
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(
      duration: Duration(seconds: error ? 6 : 3),
      backgroundColor: error ? scheme.errorContainer : null,
      content: Row(
        children: [
          if (icon != null || error) ...[
            Icon(error ? Icons.error_outline : icon,
                size: 18, color: error ? scheme.onErrorContainer : scheme.onInverseSurface),
            const SizedBox(width: 10),
          ],
          Expanded(
            child: Text(message, style: TextStyle(color: error ? scheme.onErrorContainer : null)),
          ),
        ],
      ),
    ));
}

/// Text in the monospace font, copied on click.
class CopyableMono extends StatelessWidget {
  const CopyableMono(this.text, {super.key, this.size = 12, this.what});

  final String text;
  final double size;
  final String? what;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Tooltip(
      message: 'Copy',
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: () => copy(context, text, what: what ?? 'Copied $text'),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 2),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Flexible(
                child: Text(text,
                    overflow: TextOverflow.ellipsis,
                    style: monoStyle(size: size, color: scheme.onSurfaceVariant)),
              ),
              const SizedBox(width: 4),
              Icon(Icons.copy_rounded, size: size, color: scheme.outline),
            ],
          ),
        ),
      ),
    );
  }
}

/// A small error line in the error colour.
class ErrorText extends StatelessWidget {
  const ErrorText(this.text, {super.key});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(Icons.error_outline, size: 16, color: scheme.error),
          const SizedBox(width: 8),
          Expanded(child: SelectableText(text, style: TextStyle(color: scheme.error))),
        ],
      ),
    );
  }
}

/// Fades and slides its child in when it first appears.
class FadeIn extends StatelessWidget {
  const FadeIn({super.key, required this.child, this.delay = Duration.zero});

  final Widget child;
  final Duration delay;

  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: const Duration(milliseconds: 260) + delay,
      curve: Curves.easeOutCubic,
      builder: (context, t, child) => Opacity(
        opacity: t,
        child: Transform.translate(offset: Offset(0, 6 * (1 - t)), child: child),
      ),
      child: child,
    );
  }
}
