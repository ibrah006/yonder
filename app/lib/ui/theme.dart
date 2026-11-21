import 'package:flutter/material.dart';

/// The app's look: one seed colour, quiet surfaces with hairline borders,
/// rounded corners, and a few status colours that read in light and dark.
const brand = Color(0xFF3D6B5E);

/// Monospace for commands, logs and ids.
const mono = 'Menlo';
const monoFallback = ['SF Mono', 'Consolas', 'monospace'];

TextStyle monoStyle({double size = 12.5, Color? color, FontWeight? weight, double? height}) => TextStyle(
      fontFamily: mono,
      fontFamilyFallback: monoFallback,
      fontSize: size,
      color: color,
      fontWeight: weight,
      height: height,
    );

ThemeData buildTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  final scheme = ColorScheme.fromSeed(
    seedColor: brand,
    brightness: brightness,
    surface: dark ? const Color(0xFF111614) : const Color(0xFFFBFCFB),
  );
  final radius = BorderRadius.circular(10);
  final base = ThemeData(
    colorScheme: scheme,
    brightness: brightness,
    visualDensity: VisualDensity.compact,
    scaffoldBackgroundColor: scheme.surface,
    splashFactory: InkSparkle.splashFactory,
  );
  final text = base.textTheme;
  return base.copyWith(
    textTheme: text.copyWith(
      headlineSmall: text.headlineSmall?.copyWith(fontWeight: FontWeight.w600, letterSpacing: -0.4),
      titleLarge: text.titleLarge?.copyWith(fontWeight: FontWeight.w600, letterSpacing: -0.2),
      titleMedium: text.titleMedium?.copyWith(fontWeight: FontWeight.w600),
      titleSmall: text.titleSmall?.copyWith(fontWeight: FontWeight.w600),
      labelSmall: text.labelSmall?.copyWith(letterSpacing: 0.6, fontWeight: FontWeight.w600),
    ),
    dividerTheme: DividerThemeData(color: scheme.outlineVariant.withValues(alpha: 0.6), space: 1, thickness: 1),
    cardTheme: CardThemeData(
      elevation: 0,
      margin: EdgeInsets.zero,
      color: scheme.surfaceContainerLow,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: scheme.outlineVariant.withValues(alpha: 0.6)),
      ),
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: scheme.surfaceContainerLow,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16)),
      titleTextStyle: text.titleLarge?.copyWith(fontWeight: FontWeight.w600, color: scheme.onSurface),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainerHighest.withValues(alpha: dark ? 0.5 : 0.6),
      isDense: true,
      contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 12),
      border: OutlineInputBorder(borderRadius: radius, borderSide: BorderSide.none),
      enabledBorder: OutlineInputBorder(
        borderRadius: radius,
        borderSide: BorderSide(color: scheme.outlineVariant.withValues(alpha: 0.5)),
      ),
      focusedBorder: OutlineInputBorder(borderRadius: radius, borderSide: BorderSide(color: scheme.primary, width: 1.5)),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        shape: RoundedRectangleBorder(borderRadius: radius),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        shape: RoundedRectangleBorder(borderRadius: radius),
        side: BorderSide(color: scheme.outlineVariant),
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(shape: RoundedRectangleBorder(borderRadius: radius)),
    ),
    tooltipTheme: TooltipThemeData(
      waitDuration: const Duration(milliseconds: 400),
      decoration: BoxDecoration(color: scheme.inverseSurface, borderRadius: BorderRadius.circular(6)),
      textStyle: TextStyle(color: scheme.onInverseSurface, fontSize: 12),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      width: 440,
      shape: RoundedRectangleBorder(borderRadius: radius),
    ),
    listTileTheme: ListTileThemeData(shape: RoundedRectangleBorder(borderRadius: radius)),
    chipTheme: ChipThemeData(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
      side: BorderSide(color: scheme.outlineVariant),
    ),
    popupMenuTheme: PopupMenuThemeData(
      shape: RoundedRectangleBorder(borderRadius: radius),
      color: scheme.surfaceContainerHigh,
    ),
    scrollbarTheme: const ScrollbarThemeData(radius: Radius.circular(8), thickness: WidgetStatePropertyAll(6)),
  );
}

/// Status colours that keep their meaning (and contrast) in both modes.
class Tones {
  Tones._(this.dark, this.scheme);

  factory Tones.of(BuildContext context) {
    final theme = Theme.of(context);
    return Tones._(theme.brightness == Brightness.dark, theme.colorScheme);
  }

  final bool dark;
  final ColorScheme scheme;

  Color get ok => dark ? const Color(0xFF5BD18B) : const Color(0xFF1C8A4B);
  Color get busy => dark ? const Color(0xFF74AEFF) : const Color(0xFF2B66CF);
  Color get warn => dark ? const Color(0xFFF2B94B) : const Color(0xFFA86B0C);
  Color get bad => scheme.error;
  Color get idle => scheme.outline;

  /// A job state's colour.
  Color job(String state) => switch (state) {
        'succeeded' => ok,
        'running' || 'queued' => busy,
        'cancelled' => idle,
        _ => bad,
      };
}

/// The terminal's colours: always dark, tinted towards the brand.
const terminalBackground = Color(0xFF0E1412);
