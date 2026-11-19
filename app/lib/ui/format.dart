/// Small formatting helpers shared by the screens.
library;

String bytes(BigInt value) {
  final n = value.toDouble();
  if (n < 1e3) return '${value.toInt()} B';
  if (n < 1e6) return '${(n / 1e3).toStringAsFixed(1)} KB';
  if (n < 1e9) return '${(n / 1e6).toStringAsFixed(1)} MB';
  return '${(n / 1e9).toStringAsFixed(2)} GB';
}

DateTime fromMs(BigInt ms) => DateTime.fromMillisecondsSinceEpoch(ms.toInt());

/// "14:05" today, "Mon 14:05" this week, "3 Nov 14:05" otherwise.
String when(BigInt? ms) {
  if (ms == null) return '—';
  final t = fromMs(ms);
  final now = DateTime.now();
  String two(int n) => n.toString().padLeft(2, '0');
  final time = '${two(t.hour)}:${two(t.minute)}';
  if (t.year == now.year && t.month == now.month && t.day == now.day) return time;
  const days = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  if (now.difference(t).inDays < 7) return '${days[t.weekday - 1]} $time';
  return '${t.day} ${months[t.month - 1]} $time';
}

String date(BigInt unixSecs) {
  final t = DateTime.fromMillisecondsSinceEpoch(unixSecs.toInt() * 1000);
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  return '${t.day} ${months[t.month - 1]} ${t.year}';
}

String duration(BigInt? startMs, BigInt? endMs) {
  if (startMs == null) return '';
  final end = endMs ?? BigInt.from(DateTime.now().millisecondsSinceEpoch);
  final s = ((end - startMs).toInt() / 1000).round();
  if (s < 60) return '${s}s';
  if (s < 3600) return '${s ~/ 60}m ${s % 60}s';
  return '${s ~/ 3600}h ${(s % 3600) ~/ 60}m';
}

/// The message of a Rust error, without the "AnyhowException(" wrapping.
String errorText(Object error) {
  final text = error.toString();
  final match = RegExp(r'^AnyhowException\((.*)\)$', dotAll: true).firstMatch(text);
  return (match?.group(1) ?? text).split('\n\nStack backtrace').first.trim();
}

String uptime(BigInt secs) {
  final s = secs.toInt();
  if (s < 3600) return '${s ~/ 60} min';
  if (s < 86400) return '${s ~/ 3600} h';
  return '${s ~/ 86400} days';
}
