import 'dart:convert';
import 'dart:developer' as developer;
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

/// Debug builds only: `ext.yonder.snapshot` saves a PNG of the window to the
/// `path` it's given, and `ext.yonder.tap` taps the widget showing `text`
/// (tool/snapshot.mjs calls both), for screenshots of the app without
/// screen-recording permission.
final snapshotKey = GlobalKey();

void registerSnapshot() {
  if (!kDebugMode) return;
  developer.registerExtension('ext.yonder.snapshot', (method, params) async {
    final path = params['path'];
    final boundary = snapshotKey.currentContext?.findRenderObject() as RenderRepaintBoundary?;
    if (path == null || boundary == null) {
      return developer.ServiceExtensionResponse.error(
          developer.ServiceExtensionResponse.invalidParams, 'needs a path, and the app on screen');
    }
    final ratio = ui.PlatformDispatcher.instance.views.first.devicePixelRatio;
    final image = await boundary.toImage(pixelRatio: ratio);
    final png = await image.toByteData(format: ui.ImageByteFormat.png);
    await File(path).writeAsBytes(png!.buffer.asUint8List());
    return developer.ServiceExtensionResponse.result(jsonEncode({'path': path}));
  });
  developer.registerExtension('ext.yonder.tap', (method, params) async {
    final want = params['text'];
    Offset? at;
    void visit(Element e) {
      if (at != null) return;
      final w = e.widget;
      final shown = w is Text ? (w.data ?? w.textSpan?.toPlainText()) : w is RichText ? w.text.toPlainText() : null;
      final box = e.renderObject;
      if (shown == want && box is RenderBox && box.attached && box.hasSize) {
        at = box.localToGlobal(box.size.center(Offset.zero));
        return;
      }
      e.visitChildren(visit);
    }

    WidgetsBinding.instance.rootElement?.visitChildren(visit);
    final point = at;
    if (point == null) {
      return developer.ServiceExtensionResponse.error(
          developer.ServiceExtensionResponse.invalidParams, 'nothing shows "$want"');
    }
    GestureBinding.instance.handlePointerEvent(PointerDownEvent(position: point));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    GestureBinding.instance.handlePointerEvent(PointerUpEvent(position: point));
    return developer.ServiceExtensionResponse.result(jsonEncode({'x': point.dx, 'y': point.dy}));
  });
}
