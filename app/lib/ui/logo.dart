import 'dart:math' as math;

import 'package:flutter/material.dart';

/// Yonder's mark on its tile, drawn from the same geometry as
/// brand/yonder-icon.svg: a dot for the computer you're at, a ring for the one
/// far away, one arc between them. With [drawing], the arc draws itself over
/// and over (for "connecting").
class YonderLogo extends StatefulWidget {
  const YonderLogo({super.key, this.size = 22, this.drawing = false});

  final double size;
  final bool drawing;

  @override
  State<YonderLogo> createState() => _YonderLogoState();
}

class _YonderLogoState extends State<YonderLogo> with SingleTickerProviderStateMixin {
  late final controller = AnimationController(vsync: this, duration: const Duration(milliseconds: 1600));

  @override
  void initState() {
    super.initState();
    if (widget.drawing) controller.repeat();
  }

  @override
  void didUpdateWidget(YonderLogo old) {
    super.didUpdateWidget(old);
    if (widget.drawing && !controller.isAnimating) controller.repeat();
    if (!widget.drawing && controller.isAnimating) controller.stop();
  }

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: 'Yonder',
      image: true,
      child: SizedBox.square(
        dimension: widget.size,
        child: AnimatedBuilder(
          animation: controller,
          builder: (context, _) => CustomPaint(
            painter: _LogoPainter(
              // Arc draws in the first 70%, holds, then starts again.
              progress: widget.drawing ? Curves.easeInOutCubic.transform(math.min(controller.value / 0.7, 1)) : 1,
              bold: widget.size < 30,
            ),
          ),
        ),
      ),
    );
  }
}

class _LogoPainter extends CustomPainter {
  _LogoPainter({required this.progress, required this.bold});

  /// How much of the arc is drawn, 0 to 1.
  final double progress;

  /// The heavier small-size drawing (brand/yonder-icon-small.svg).
  final bool bold;

  static const mint = Color(0xFF8FE3BE);

  @override
  void paint(Canvas canvas, Size size) {
    // Work on the icon's 100-unit grid.
    canvas.scale(size.width / 100, size.height / 100);
    final tile = RRect.fromRectAndRadius(const Rect.fromLTWH(0, 0, 100, 100), const Radius.circular(22.5));
    canvas.drawRRect(
      tile,
      Paint()
        ..shader = const LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment(0.8, 1),
          colors: [Color(0xFF4C8272), Color(0xFF1A362F)],
        ).createShader(const Rect.fromLTWH(0, 0, 100, 100)),
    );
    canvas.drawRRect(
      tile,
      Paint()
        ..shader = const LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          stops: [0, 0.5],
          colors: [Color(0x38FFFFFF), Color(0x00FFFFFF)],
        ).createShader(const Rect.fromLTWH(0, 0, 100, 100)),
    );

    // The mark, on its own 100-unit grid inside the tile.
    canvas.translate(bold ? 11 : 19, bold ? 11 : 19);
    canvas.scale(bold ? 0.78 : 0.62);
    final stroke = Paint()
      ..color = mint
      ..style = PaintingStyle.stroke
      ..strokeCap = StrokeCap.round;
    final arc = Path()
      ..moveTo(25, 75)
      ..arcToPoint(bold ? const Offset(61, 29) : const Offset(63, 28), radius: const Radius.circular(52));
    final shown = progress >= 1
        ? arc
        : () {
            final metric = arc.computeMetrics().first;
            return metric.extractPath(0, metric.length * progress);
          }();
    canvas.drawPath(shown, stroke..strokeWidth = bold ? 12 : 8);
    canvas.drawCircle(const Offset(25, 75), bold ? 14 : 11, Paint()..color = mint);
    // The far end appears as the arc reaches it.
    final reach = ((progress - 0.85) / 0.15).clamp(0.0, 1.0);
    canvas.drawCircle(
      const Offset(75, 27),
      bold ? 11 : 10,
      Paint()
        ..color = mint.withValues(alpha: reach)
        ..style = PaintingStyle.stroke
        ..strokeWidth = bold ? 10 : 7,
    );
  }

  @override
  bool shouldRepaint(_LogoPainter old) => old.progress != progress || old.bold != bold;
}
