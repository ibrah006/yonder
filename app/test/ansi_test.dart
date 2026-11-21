import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yonder_app/ui/ansi.dart';

List<TextSpan> spans(AnsiLog log) =>
    log.span(const TextStyle(), Brightness.dark).children!.cast<TextSpan>();

void main() {
  test('colour codes become styles, not text', () {
    final log = AnsiLog()..add('plain \x1B[31mred\x1B[0m back');
    expect(log.plain, 'plain red back');
    final s = spans(log);
    expect(s.map((e) => e.text), ['plain ', 'red', ' back']);
    expect(s[0].style, isNull);
    expect(s[1].style?.color, isNotNull);
    expect(s[2].style, isNull);
  });

  test('an escape split across chunks still parses', () {
    final log = AnsiLog()
      ..add('a\x1B[1')
      ..add(';32mb');
    expect(log.plain, 'ab');
    expect(spans(log)[1].style?.fontWeight, FontWeight.bold);
  });

  test('cursor moves and carriage returns are dropped', () {
    final log = AnsiLog()..add('one\r\ntwo\x1B[2K\x1B[1Gthree\r');
    expect(log.plain, 'one\ntwothree');
  });

  test('keeps only the newest text past the limit', () {
    final log = AnsiLog(maxChars: 10)
      ..add('0123456789')
      ..add('\x1B[33mabcde');
    expect(log.plain, '56789abcde');
    expect(log.length, 10);
  });

  test('256-colour and true-colour codes', () {
    final log = AnsiLog()..add('\x1B[38;5;196mx\x1B[38;2;1;2;3my');
    final s = spans(log);
    expect(s[0].style?.color, const Color.fromARGB(255, 255, 0, 0));
    expect(s[1].style?.color, const Color(0xFF010203));
  });
}
