import 'package:flutter/material.dart';

/// Turns terminal output with ANSI colour codes into styled text, a chunk at
/// a time, keeping at most `maxChars` (the oldest text goes first).
class AnsiLog {
  AnsiLog({this.maxChars = 400000});

  final int maxChars;

  /// Runs of text with the same style.
  final List<({String text, _Sgr style})> _runs = [];
  int _length = 0;
  _Sgr _style = const _Sgr();

  /// An escape sequence cut off at the end of the last chunk.
  String _pending = '';

  static final _escape = RegExp(r'\x1B(?:\[([0-9;?]*)([ -/]*)([@-~])|\][^\x07\x1B]*(?:\x07|\x1B\\)|[@-Z\\-_])');

  int get length => _length;
  bool get isEmpty => _length == 0;

  /// The plain text, for copying.
  String get plain => _runs.map((r) => r.text).join();

  void add(String chunk) {
    var text = _pending + chunk;
    _pending = '';
    // Keep an unfinished escape for the next chunk.
    final cut = text.lastIndexOf('\x1B');
    if (cut != -1 && cut > text.length - 32 && !_escape.hasMatch(text.substring(cut))) {
      _pending = text.substring(cut);
      text = text.substring(0, cut);
    }
    var at = 0;
    for (final m in _escape.allMatches(text)) {
      _append(text.substring(at, m.start));
      at = m.end;
      if (m.group(3) == 'm' && (m.group(2) ?? '').isEmpty) _style = _style.apply(m.group(1) ?? '');
    }
    _append(text.substring(at));
    _trim();
  }

  void _append(String text) {
    text = text.replaceAll('\r\n', '\n').replaceAll('\r', '');
    if (text.isEmpty) return;
    _length += text.length;
    if (_runs.isNotEmpty && _runs.last.style == _style) {
      final last = _runs.removeLast();
      _runs.add((text: last.text + text, style: _style));
    } else {
      _runs.add((text: text, style: _style));
    }
  }

  void _trim() {
    while (_length > maxChars && _runs.isNotEmpty) {
      final first = _runs.first;
      final over = _length - maxChars;
      if (first.text.length <= over) {
        _runs.removeAt(0);
        _length -= first.text.length;
      } else {
        _runs[0] = (text: first.text.substring(over), style: first.style);
        _length -= over;
      }
    }
  }

  /// The log as spans for a RichText, coloured for `brightness`.
  TextSpan span(TextStyle base, Brightness brightness) {
    final palette = brightness == Brightness.dark ? _darkPalette : _lightPalette;
    return TextSpan(
      style: base,
      children: [for (final r in _runs) TextSpan(text: r.text, style: r.style.textStyle(palette))],
    );
  }
}

/// The SGR state: colours (palette index or 0xAARRGGBB), bold, dim, italic, underline.
@immutable
class _Sgr {
  const _Sgr({this.fg, this.bg, this.bold = false, this.dim = false, this.italic = false, this.underline = false});

  final int? fg;
  final int? bg;
  final bool bold;
  final bool dim;
  final bool italic;
  final bool underline;

  _Sgr copy({int? fg, int? bg, bool? bold, bool? dim, bool? italic, bool? underline, bool clearFg = false, bool clearBg = false}) => _Sgr(
        fg: clearFg ? null : fg ?? this.fg,
        bg: clearBg ? null : bg ?? this.bg,
        bold: bold ?? this.bold,
        dim: dim ?? this.dim,
        italic: italic ?? this.italic,
        underline: underline ?? this.underline,
      );

  _Sgr apply(String params) {
    final codes = params.isEmpty ? [0] : params.split(';').map((p) => int.tryParse(p) ?? 0).toList();
    var s = this;
    for (var i = 0; i < codes.length; i++) {
      final c = codes[i];
      switch (c) {
        case 0:
          s = const _Sgr();
        case 1:
          s = s.copy(bold: true);
        case 2:
          s = s.copy(dim: true);
        case 3:
          s = s.copy(italic: true);
        case 4:
          s = s.copy(underline: true);
        case 22:
          s = s.copy(bold: false, dim: false);
        case 23:
          s = s.copy(italic: false);
        case 24:
          s = s.copy(underline: false);
        case >= 30 && <= 37:
          s = s.copy(fg: c - 30);
        case >= 90 && <= 97:
          s = s.copy(fg: c - 90 + 8);
        case 39:
          s = s.copy(clearFg: true);
        case >= 40 && <= 47:
          s = s.copy(bg: c - 40);
        case >= 100 && <= 107:
          s = s.copy(bg: c - 100 + 8);
        case 49:
          s = s.copy(clearBg: true);
        case 38 || 48:
          int? colour;
          if (i + 2 < codes.length && codes[i + 1] == 5) {
            colour = codes[i + 2];
            i += 2;
          } else if (i + 4 < codes.length && codes[i + 1] == 2) {
            colour = 0xFF000000 | (codes[i + 2] << 16) | (codes[i + 3] << 8) | codes[i + 4];
            i += 4;
          }
          if (colour != null) s = c == 38 ? s.copy(fg: colour) : s.copy(bg: colour);
      }
    }
    return s;
  }

  TextStyle? textStyle(List<Color> palette) {
    if (this == const _Sgr()) return null;
    Color? colour(int? v) {
      if (v == null) return null;
      if (v > 255) return Color(v);
      if (v < 16) return palette[v];
      if (v >= 232) {
        final g = 8 + (v - 232) * 10;
        return Color.fromARGB(255, g, g, g);
      }
      final n = v - 16;
      int level(int x) => x == 0 ? 0 : 55 + x * 40;
      return Color.fromARGB(255, level(n ~/ 36), level((n ~/ 6) % 6), level(n % 6));
    }

    final fgColour = colour(fg);
    return TextStyle(
      color: dim ? fgColour?.withValues(alpha: 0.6) : fgColour,
      backgroundColor: colour(bg),
      fontWeight: bold ? FontWeight.bold : null,
      fontStyle: italic ? FontStyle.italic : null,
      decoration: underline ? TextDecoration.underline : null,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is _Sgr &&
      other.fg == fg &&
      other.bg == bg &&
      other.bold == bold &&
      other.dim == dim &&
      other.italic == italic &&
      other.underline == underline;

  @override
  int get hashCode => Object.hash(fg, bg, bold, dim, italic, underline);
}

const _darkPalette = [
  Color(0xFF3B4441), Color(0xFFFF7B72), Color(0xFF5BD18B), Color(0xFFF2CC60),
  Color(0xFF74AEFF), Color(0xFFD2A8FF), Color(0xFF56D4DD), Color(0xFFD5DDD9),
  Color(0xFF6E7B76), Color(0xFFFFA198), Color(0xFF7EE2A8), Color(0xFFF8E3A1),
  Color(0xFFA5CBFF), Color(0xFFE2C5FF), Color(0xFFB3F0FF), Color(0xFFFFFFFF),
];

const _lightPalette = [
  Color(0xFF24292F), Color(0xFFCF222E), Color(0xFF1A7F37), Color(0xFF9A6700),
  Color(0xFF0969DA), Color(0xFF8250DF), Color(0xFF1B7C83), Color(0xFF6E7781),
  Color(0xFF57606A), Color(0xFFA40E26), Color(0xFF2DA44E), Color(0xFFBF8700),
  Color(0xFF218BFF), Color(0xFFA475F9), Color(0xFF3192AA), Color(0xFF8C959F),
];
